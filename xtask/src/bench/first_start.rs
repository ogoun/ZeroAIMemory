//! `cargo xtask bench --scenario first-start` — первый старт Qdrant после массовой
//! индексации (задача 19 M3, §17 ТЗ).
//!
//! Один запуск — один вариант настроек Qdrant (`--set qdrant.flush_interval_sec=…`,
//! `qdrant.wal_capacity_mb`, `qdrant.shutdown_flush_wait_ms`) и метка `--label`. Каталог
//! данных — `<--work>/<профиль>-first-start/data`, он переживает запуски: корпус eval и
//! копии до `--notes` индексируются один раз (`build`, первый запуск или `--rebuild`), а
//! каждый следующий вариант начинается с `zam-server qdrant reset` и пересборки коллекции
//! из `embeddings` без инференса — коллекция создаётся заново с параметрами варианта
//! (размер блока WAL Qdrant 1.19 меняется только так).
//!
//! Циклы (`--reps` каждого):
//! - `graceful` — пачка новых заметок (`--burst`), ожидание пустой очереди (скорость
//!   индексации пачки), сразу штатная остановка (Ctrl+Break / SIGTERM; время остановки),
//!   замеряемый старт;
//! - `kill` — та же пачка, через `--kill-after` с после начала индексации, пока очередь не
//!   пуста, — жёсткое завершение сервера (TerminateProcess / SIGKILL; Qdrant умирает вместе
//!   с ним), замеряемый старт.
//!
//! Замеряемый старт: от запуска `zam-server` до строки «Qdrant готов», готовность самого
//! процесса Qdrant, число повторённых операций WAL (строка Qdrant «Recovered collection
//! zam_chunks: N/N»), время от запуска Qdrant до конца повтора, загрузка модели. Затем —
//! ожидание пустой очереди и проверка сохранности: число точек
//! в Qdrant до и после `reindex` из `embeddings` (источник истины — SQLite) одно и то же.
//! После проверки — пауза без записи, пока подтверждённая часть WAL не перестанет расти
//! (`first-index`), и остановка: следующий цикл начинается с чистого WAL.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use super::copies::{self, NoteSpec};
use super::{
    Ctx, Http, INDEX_WAIT, Profile, Stats, WorkerStat, chunk_stats, log_line, ms_since, stats,
    wait_idle, wait_ready,
};
use crate::eval::corpus::Corpus;
use crate::eval::run::Session;

/// Предел паузы перед остановкой без замера (см. `settle_and_stop`).
const SETTLE_LIMIT: Duration = Duration::from_secs(180);
/// Предел штатной остановки сервера.
const STOP_LIMIT: Duration = Duration::from_secs(120);
/// Умолчание `qdrant.flush_interval_sec` сервера (если вариант его не задаёт): 3 с с M4
/// (`zam_index::supervisor::DEFAULT_FLUSH_INTERVAL_SEC`, решение владельца 2026-10-26).
const DEFAULT_FLUSH_SEC: u64 = 3;

/// Параметры сценария (из `BenchArgs`).
#[derive(Debug, Clone)]
pub struct Args {
    pub notes: usize,
    pub reps: usize,
    pub burst: usize,
    pub kill_after: Duration,
    pub rebuild: bool,
    /// Не пересоздавать коллекцию (`qdrant reset`) перед циклами: варианты без смены
    /// `wal_capacity_mb` идут на той же коллекции.
    pub no_prepare: bool,
    /// Только циклы `graceful` (варианты, различающиеся лишь ожиданием при остановке).
    pub graceful_only: bool,
    /// Варианты: имя и переопределения поверх `--set`. Циклы чередуют варианты внутри
    /// каждого повтора, нечётные повторы — в обратном порядке (`v1 v2 v3 v3 v2 v1 …`):
    /// дрейф HDD и коллекции делится между ними поровну. Пусто — один вариант `--label`.
    pub variants: Vec<(String, Vec<(String, String)>)>,
}

/// Индексация каталога с нуля (корпус eval и копии).
#[derive(Debug, Clone, Serialize)]
pub struct BuildRun {
    pub notes: usize,
    pub chunks: u64,
    pub secs: f64,
    pub chunks_per_sec: f64,
}

/// Замеряемый старт.
#[derive(Debug, Clone, Serialize)]
pub struct StartRun {
    /// От запуска `zam-server` до строки «Qdrant готов».
    pub qdrant_from_start_ms: Option<f64>,
    /// Готовность процесса Qdrant от его запуска.
    pub qdrant_ready_ms: Option<f64>,
    /// Повторённых операций WAL (нет строки «Recovered collection» — 0).
    pub wal_ops: u64,
    /// От запуска процесса Qdrant до конца повтора WAL.
    pub replay_done_ms: Option<f64>,
    pub model_load_ms: Option<f64>,
    /// От запуска `zam-server` до готовности векторного канала.
    pub vector_ready_ms: f64,
}

/// Один цикл.
#[derive(Debug, Clone, Serialize)]
pub struct Cycle {
    /// Вариант настроек (`--variant`).
    pub variant: String,
    /// `build` (после индексации каталога с нуля), `graceful` или `kill`.
    pub kind: String,
    pub rep: usize,
    /// `graceful`: от пустой очереди до покоя воркера (нет пачки и отложенных свёрток L0).
    pub quiet_secs: Option<f64>,
    /// Штатная остановка: от сигнала до строки «воркер индексации остановлен».
    pub worker_stop_ms: Option<f64>,
    pub burst_notes: usize,
    /// Пачка: листовых чанков и скорость (только `graceful` и `build` — до пустой очереди).
    pub burst_chunks: Option<u64>,
    pub index_secs: Option<f64>,
    pub chunks_per_sec: Option<f64>,
    /// Заданий воркера, закрытых до остановки, и очередь в момент остановки.
    pub jobs_before_stop: u64,
    pub queue_at_stop: u64,
    /// Записей точек, не подтверждённых в WAL, в момент остановки (`wal_pending_ops`).
    pub wal_pending_at_stop: Option<u64>,
    /// Ожидание подтверждения WAL при штатной остановке: мс и итог (`acked`/`timeout`/`time`).
    pub shutdown_wait_ms: Option<f64>,
    pub shutdown_wait_result: Option<String>,
    /// Штатная остановка: от сигнала до выхода процесса.
    pub stop_ms: Option<f64>,
    pub start: StartRun,
    /// Точек в Qdrant после восстановления и пустой очереди / после `reindex` из
    /// `embeddings` (SQLite).
    pub points_qdrant: Option<u64>,
    pub points_expected: Option<u64>,
    pub data_ok: bool,
}

/// Итог запуска сценария.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FirstStartResult {
    pub date: String,
    pub git: String,
    pub profile: Option<Profile>,
    pub label: Option<String>,
    pub host: Value,
    pub model_id: Option<String>,
    pub device: Value,
    pub sets: BTreeMap<String, String>,
    pub notes: usize,
    pub build: Option<BuildRun>,
    /// Пересборка коллекции варианта из `embeddings` после `qdrant reset`, с.
    pub prepare_secs: Option<f64>,
    pub cycles: Vec<Cycle>,
}

/// Число повторённых операций WAL по журналу сервера: строка Qdrant
/// «Recovered collection zam_chunks: N/N (100%)». Нет строки — повторять было нечего.
pub fn wal_ops(log: &str) -> u64 {
    log.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let m = v["fields"]["message"].as_str()?.to_string();
            let rest = m.strip_prefix("Recovered collection ")?;
            let (_, frac) = rest.rsplit_once(": ")?;
            frac.split('/').next()?.trim().parse::<u64>().ok()
        })
        .sum()
}

/// Метка времени первой строки журнала с сообщением, начинающимся с `prefix`.
fn first_ts(log: &str, prefix: &str) -> Option<jiff::Timestamp> {
    log.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| {
            v["fields"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with(prefix))
        })
        .and_then(|v| v["timestamp"].as_str()?.parse().ok())
}

/// Окно без роста подтверждённой части WAL, после которого сброс закончился:
/// `2 · flush_interval_sec + 8` с (проход фонового сброса на HDD — до ~5 с, задача 19).
pub fn settle(sets: &[(String, String)]) -> Duration {
    let f = sets
        .iter()
        .rfind(|(k, _)| k == "qdrant.flush_interval_sec")
        .and_then(|(_, v)| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_FLUSH_SEC);
    Duration::from_secs(2 * f + 8)
}

/// Пачка из `n` новых заметок с путями `burst/<метка>/<цикл>/…` (тексты — копии корпуса).
pub fn burst(corpus: &Corpus, n: usize, label: &str, cycle: &str) -> Vec<NoteSpec> {
    copies::make(corpus, corpus.notes.len() + n)
        .into_iter()
        .enumerate()
        .map(|(i, mut s)| {
            let name = s.path.rsplit('/').next().unwrap_or("n").to_string();
            s.path = format!("burst/{label}/{cycle}/{i:04}-{name}");
            s
        })
        .collect()
}

/// Старт с замером (каталог не пересоздаётся).
fn start(c: &Ctx, r: &mut FirstStartResult) -> Result<(Session, Http, StartRun)> {
    let wall0 = jiff::Timestamp::now();
    let t0 = Instant::now();
    let mut s = Session::start(&c.paths, &c.spec, &c.server, false)?;
    let h = Http::new(&s)?;
    wait_ready(&mut s, &h)?;
    let vector_ready_ms = t0.elapsed().as_secs_f64() * 1000.0;
    s.check_model()?;
    r.model_id = s.status["model"]["id"].as_str().map(String::from);
    r.device = s.status["device"].clone();
    let log = fs::read_to_string(c.log()).unwrap_or_default();
    let q = log_line(&c.log(), "Qdrant готов");
    let spawned = first_ts(&log, "Qdrant запущен");
    let recovered = first_ts(&log, "Recovered collection");
    let st = StartRun {
        qdrant_from_start_ms: q.as_ref().and_then(|l| ms_since(wall0, l)),
        qdrant_ready_ms: q.as_ref().and_then(|l| l["fields"]["ready_ms"].as_f64()),
        wal_ops: wal_ops(&log),
        replay_done_ms: match (spawned, recovered) {
            (Some(a), Some(b)) => Some(b.duration_since(a).as_secs_f64() * 1000.0),
            _ => None,
        },
        model_load_ms: log_line(&c.log(), "модель загружена")
            .and_then(|l| l["fields"]["ms"].as_f64()),
        vector_ready_ms,
    };
    Ok((s, h, st))
}

/// Ожидание подтверждения WAL из журнала остановленного сервера.
fn shutdown_wait(c: &Ctx, tag: &str) -> (Option<f64>, Option<String>) {
    let f = c.log().with_file_name(format!("zam-server-{tag}.log"));
    match log_line(&f, "ожидание подтверждения WAL Qdrant перед остановкой")
    {
        Some(l) => (
            l["fields"]["wait_ms"].as_f64(),
            l["fields"]["result"].as_str().map(String::from),
        ),
        None => (None, None),
    }
}

/// Ждёт покоя воркера после пустой очереди: нет пачки в работе и отложенных свёрток L0
/// папок (`/index/status` → `worker`), не дольше 5 мин; секунды ожидания.
fn wait_quiet(h: &Http) -> Result<f64> {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(300) {
        let st = h.status()?;
        let w = &st["worker"];
        if w["batch_running"] == false && w["rollups_pending"].as_u64() == Some(0) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(t0.elapsed().as_secs_f64())
}

/// От сигнала остановки до строки «воркер индексации остановлен» журнала `tag`, мс.
fn worker_stop(c: &Ctx, tag: &str, signal: jiff::Timestamp) -> Option<f64> {
    let f = c.log().with_file_name(format!("zam-server-{tag}.log"));
    log_line(&f, "воркер индексации остановлен").and_then(|l| ms_since(signal, &l))
}

/// Сохраняет журнал сервера под именем `zam-server-<tag>.log` (следующий старт его
/// перезаписывает).
fn keep_log(c: &Ctx, tag: &str) {
    let log = c.log();
    let _ = fs::rename(&log, log.with_file_name(format!("zam-server-{tag}.log")));
}

/// `ack_index` WAL коллекции (`first-index`, Qdrant 1.19).
fn ack_index(data: &Path) -> Option<u64> {
    let f = data
        .join("qdrant")
        .join("storage")
        .join("collections")
        .join("zam_chunks")
        .join("0")
        .join("wal")
        .join("first-index");
    serde_json::from_str::<Value>(&fs::read_to_string(f).ok()?)
        .ok()?
        .get("ack_index")?
        .as_u64()
}

/// Пауза без записи и остановка: ждём, пока Qdrant подтвердит в WAL все записи точек
/// (`/index/status` → `qdrant.wal_pending_ops` = 0), не дольше [`SETTLE_LIMIT`]; если
/// счётчика нет — пока `ack_index` не перестанет расти дольше окна [`settle`]. Следующий
/// старт тогда не повторяет WAL (иначе — пометка в выводе).
fn settle_and_stop(c: &Ctx, s: Session, h: &Http, tag: &str) -> Result<()> {
    let window = settle(&c.spec.sets);
    let t0 = Instant::now();
    let mut last = ack_index(&s.data);
    let mut since = Instant::now();
    let mut pending = None;
    while t0.elapsed() < SETTLE_LIMIT {
        thread::sleep(Duration::from_millis(500));
        pending = h.status()?["qdrant"]["wal_pending_ops"].as_u64();
        if pending == Some(0) {
            break;
        }
        let a = ack_index(&s.data);
        if a != last {
            last = a;
            since = Instant::now();
        } else if pending.is_none() && since.elapsed() >= window {
            break;
        }
    }
    if pending.is_some_and(|p| p > 0) {
        println!(
            "  (пауза {:.0} с: в WAL не подтверждено {} операций)",
            t0.elapsed().as_secs_f64(),
            pending.unwrap_or(0)
        );
    }
    s.stop()?;
    keep_log(c, tag);
    Ok(())
}

/// `POST /admin/reindex` (из кэша) базы пользователя прогона; число записанных точек.
pub(super) fn reindex_points(s: &mut Session, h: &Http) -> Result<u64> {
    let last = super::run_reindex(s, h)?;
    let res = &last["result"];
    if res.is_null() {
        bail!("reindex завершился ошибкой: {last}");
    }
    Ok(res["points"].as_u64().unwrap_or(0))
}

/// Проверка сохранности: точек в Qdrant после восстановления столько же, сколько после
/// `reindex` из `embeddings` (SQLite — источник истины).
fn verify(s: &mut Session, h: &Http) -> Result<(Option<u64>, Option<u64>, bool)> {
    wait_idle(s, h, Instant::now(), INDEX_WAIT)?;
    let before = h.status()?["qdrant"]["points"].as_u64();
    let expected = reindex_points(s, h)?;
    wait_idle(s, h, Instant::now(), INDEX_WAIT)?;
    let after = h.status()?["qdrant"]["points"].as_u64();
    // reindex удаляет точки области и пишет их заново из SQLite: число до и после совпадает,
    // если после восстановления ничего не потеряно и не задвоено (точки папок reindex в
    // `points` отчёта не считает — сравнение «до/после», а не с отчётом)
    let ok = before.is_some() && before == after && after >= Some(expected);
    if !ok {
        println!(
            "  ВНИМАНИЕ: точек до reindex {before:?}, reindex записал {expected}, после {after:?}"
        );
    }
    Ok((before, after, ok))
}

/// Мягко удаляет каталог пачки (поддерево) — коллекция не растёт от цикла к циклу.
fn drop_burst(s: &mut Session, h: &Http, path: &str) -> Result<()> {
    let auth = format!("Bearer {}", h.token);
    let mut r = h
        .agent
        .get(&format!("{}/api/v1/nodes/by-path", h.base))
        .query("space", "knowledge")
        .query("path", path)
        .header("Authorization", &auth)
        .call()?;
    let v: Value = serde_json::from_str(&r.body_mut().read_to_string()?)?;
    let id = v["node"]["id"]
        .as_str()
        .with_context(|| format!("нет узла {path}: {v}"))?;
    let r = h
        .agent
        .delete(&format!("{}/api/v1/nodes/{id}", h.base))
        .header("Authorization", &auth)
        .call()?;
    if r.status().as_u16() != 204 {
        bail!("DELETE {path}: HTTP {}", r.status());
    }
    wait_idle(s, h, Instant::now(), INDEX_WAIT)
}

/// Пишет заметки пачки; сбой записи (HTTP 5xx, например `database is locked` при
/// конкуренции за HDD) повторяется до трёх раз с паузой 1 с и печатается.
fn post_all(h: &Http, notes: &[NoteSpec]) -> Result<()> {
    for n in notes {
        let mut attempt = 0;
        loop {
            match h.post_note(&n.path, &n.text) {
                Ok(()) => break,
                Err(e) if attempt < 3 => {
                    attempt += 1;
                    println!(
                        "  ВНИМАНИЕ: запись {} не удалась (попытка {attempt}): {e}",
                        n.path
                    );
                    thread::sleep(Duration::from_secs(1));
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Старт без замера; повтор WAL на нём — признак, что пауза перед прошлой остановкой не
/// дождалась подтверждения (в журнал прогона).
fn start_plain(c: &Ctx, r: &mut FirstStartResult) -> Result<(Session, Http)> {
    let (s, h, st) = start(c, r)?;
    if st.wal_ops > 1 {
        println!(
            "  (старт без замера: повтор {} операций WAL, {} мс)",
            st.wal_ops,
            st.qdrant_from_start_ms
                .map_or("—".into(), |v| format!("{v:.0}"))
        );
    }
    Ok((s, h))
}

fn print_start(st: &StartRun) {
    println!(
        "  старт: до «Qdrant готов» {} мс (процесс Qdrant {} мс), WAL {} операций (повтор до {} мс), модель {} мс, векторный канал {:.0} мс",
        st.qdrant_from_start_ms
            .map_or("—".into(), |v| format!("{v:.0}")),
        st.qdrant_ready_ms.map_or("—".into(), |v| format!("{v:.0}")),
        st.wal_ops,
        st.replay_done_ms.map_or("—".into(), |v| format!("{v:.0}")),
        st.model_load_ms.map_or("—".into(), |v| format!("{v:.0}")),
        st.vector_ready_ms
    );
}

/// Индексация каталога с нуля: корпус eval и копии до `notes`; затем сразу штатная
/// остановка и замеряемый старт (сценарий M2).
fn build(c: &Ctx, corpus: &Corpus, a: &Args, r: &mut FirstStartResult) -> Result<()> {
    println!("[build] индексация с нуля: {} заметок", a.notes);
    let (mut s, h, _) = start(c, r)?;
    let t0 = Instant::now();
    s.load_corpus(corpus)?;
    let list = copies::make(corpus, a.notes);
    post_all(&h, &list)?;
    wait_idle(&mut s, &h, t0, INDEX_WAIT)?;
    let secs = t0.elapsed().as_secs_f64();
    let mut chunks = 0;
    for like in ["ru/%", "en/%", "bench/%"] {
        chunks += chunk_stats(&s.data, like)?.0;
    }
    let b = BuildRun {
        notes: a.notes,
        chunks,
        secs,
        chunks_per_sec: chunks as f64 / secs,
    };
    println!(
        "  {} чанков за {:.0} с: {:.2} чанка/с",
        b.chunks, b.secs, b.chunks_per_sec
    );
    r.build = Some(b.clone());
    let jobs = WorkerStat::of(&h.status()?)?.jobs_done;
    let pending = h.status()?["qdrant"]["wal_pending_ops"].as_u64();
    let signal = jiff::Timestamp::now();
    let stop = s.stop_graceful(STOP_LIMIT)?;
    keep_log(c, "build");
    let (wait_ms, wait_res) = shutdown_wait(c, "build");
    let worker_stop_ms = worker_stop(c, "build", signal);
    println!("  штатная остановка: {:.0} мс", stop.as_secs_f64() * 1000.0);
    let (mut s, h, st) = start(c, r)?;
    print_start(&st);
    let (pq, pe, ok) = verify(&mut s, &h)?;
    r.cycles.push(Cycle {
        variant: r.label.clone().unwrap_or_default(),
        kind: "build".into(),
        rep: 0,
        quiet_secs: None,
        worker_stop_ms,
        burst_notes: a.notes,
        burst_chunks: Some(b.chunks),
        index_secs: Some(b.secs),
        chunks_per_sec: Some(b.chunks_per_sec),
        jobs_before_stop: jobs,
        wal_pending_at_stop: pending,
        shutdown_wait_ms: wait_ms,
        shutdown_wait_result: wait_res.clone(),
        queue_at_stop: 0,
        stop_ms: Some(stop.as_secs_f64() * 1000.0),
        start: st,
        points_qdrant: pq,
        points_expected: pe,
        data_ok: ok,
    });
    settle_and_stop(c, s, &h, "build-verify")
}

/// Коллекция варианта: `qdrant reset` и пересборка из `embeddings` при старте.
fn prepare(c: &Ctx, r: &mut FirstStartResult) -> Result<()> {
    println!("[prepare] qdrant reset и пересборка коллекции варианта из embeddings");
    let data = c.paths.work.join(&c.spec.name).join("data");
    let exe = c.server.join(if cfg!(windows) {
        "zam-server.exe"
    } else {
        "zam-server"
    });
    let out = Command::new(&exe)
        .args(["qdrant", "reset", "--yes", "--data-dir"])
        .arg(&data)
        .env(
            "ZAM_QDRANT__BINARY",
            crate::eval::run::qdrant_bin(&c.paths.repo),
        )
        .output()
        .context("zam-server qdrant reset")?;
    if !out.status.success() {
        bail!(
            "qdrant reset: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let t0 = Instant::now();
    let (mut s, h, _) = start(c, r)?;
    wait_idle(&mut s, &h, t0, INDEX_WAIT)?;
    // пересборка идёт воркером: пустая очередь и отметка снята
    loop {
        let st = h.status()?;
        if st["reindex"]["running"] == false && st["reindex"]["queued"] == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    let secs = t0.elapsed().as_secs_f64();
    r.prepare_secs = Some(secs);
    println!(
        "  готово за {secs:.1} с, точек {}",
        h.status()?["qdrant"]["points"]
    );
    settle_and_stop(c, s, &h, "prepare")
}

/// Цикл `graceful`: пачка до пустой очереди и покоя воркера, сразу штатная остановка,
/// замеряемый старт.
fn cycle_graceful(
    c: &Ctx,
    corpus: &Corpus,
    a: &Args,
    v: &str,
    rep: usize,
    r: &mut FirstStartResult,
) -> Result<()> {
    let tag = format!("{v}-g{rep}");
    // каталог пачки уникален для запуска (`--label`) и варианта: прерванный запуск не
    // оставляет конфликтующих путей следующему
    let root = format!("{}/{v}", r.label.as_deref().unwrap_or("run"));
    let notes = burst(corpus, a.burst, &root, &format!("g{rep}"));
    println!("[{v} graceful {rep}] пачка {} заметок", notes.len());
    let (mut s, h) = start_plain(c, r)?;
    let w0 = WorkerStat::of(&h.status()?)?;
    let t0 = Instant::now();
    post_all(&h, &notes)?;
    wait_idle(&mut s, &h, t0, INDEX_WAIT)?;
    let secs = t0.elapsed().as_secs_f64();
    let quiet = wait_quiet(&h)?;
    let like = format!("burst/{root}/g{rep}/%");
    let chunks = chunk_stats(&s.data, &like)?.0;
    let jobs = WorkerStat::of(&h.status()?)?.since(w0).jobs_done;
    let pending = h.status()?["qdrant"]["wal_pending_ops"].as_u64();
    let signal = jiff::Timestamp::now();
    let stop = s.stop_graceful(STOP_LIMIT)?;
    keep_log(c, &format!("{tag}-before"));
    let (wait_ms, wait_res) = shutdown_wait(c, &format!("{tag}-before"));
    let worker_stop_ms = worker_stop(c, &format!("{tag}-before"), signal);
    let stop_ms = stop.as_secs_f64() * 1000.0;
    println!(
        "  {chunks} чанков за {secs:.1} с ({:.2} чанка/с), покой воркера через {quiet:.1} с; WAL не подтверждено {pending:?}; штатная остановка {stop_ms:.0} мс (воркер {worker_stop_ms:?} мс, ожидание WAL {wait_ms:?} мс, {wait_res:?})",
        chunks as f64 / secs
    );
    let (mut s, h, st) = start(c, r)?;
    print_start(&st);
    let (pq, pe, ok) = verify(&mut s, &h)?;
    r.cycles.push(Cycle {
        variant: v.to_string(),
        kind: "graceful".into(),
        rep,
        quiet_secs: Some(quiet),
        worker_stop_ms,
        burst_notes: notes.len(),
        burst_chunks: Some(chunks),
        index_secs: Some(secs),
        chunks_per_sec: Some(chunks as f64 / secs),
        jobs_before_stop: jobs,
        wal_pending_at_stop: pending,
        shutdown_wait_ms: wait_ms,
        shutdown_wait_result: wait_res.clone(),
        queue_at_stop: 0,
        stop_ms: Some(stop_ms),
        start: st,
        points_qdrant: pq,
        points_expected: pe,
        data_ok: ok,
    });
    drop_burst(&mut s, &h, &format!("burst/{root}/g{rep}"))?;
    settle_and_stop(c, s, &h, &format!("{tag}-after"))
}

/// Цикл `kill`: пачка, жёсткое завершение во время индексации, замеряемый старт.
fn cycle_kill(
    c: &Ctx,
    corpus: &Corpus,
    a: &Args,
    v: &str,
    rep: usize,
    r: &mut FirstStartResult,
) -> Result<()> {
    let tag = format!("{v}-k{rep}");
    let root = format!("{}/{v}", r.label.as_deref().unwrap_or("run"));
    let notes = burst(corpus, a.burst, &root, &format!("k{rep}"));
    println!("[{v} kill {rep}] пачка {} заметок", notes.len());
    let (mut s, h) = start_plain(c, r)?;
    let w0 = WorkerStat::of(&h.status()?)?;
    post_all(&h, &notes)?;
    let t0 = Instant::now();
    // индексация идёт: воркер закрывает задания; убиваем через kill_after, пока очередь не пуста
    loop {
        s.ensure_alive()?;
        thread::sleep(Duration::from_millis(100));
        if t0.elapsed() >= a.kill_after {
            break;
        }
    }
    let queue = h.queue()?;
    let jobs = WorkerStat::of(&h.status()?)?.since(w0).jobs_done;
    let pending = h.status()?["qdrant"]["wal_pending_ops"].as_u64();
    let (wait_ms, wait_res): (Option<f64>, Option<String>) = (None, None);
    if queue == 0 {
        println!("  ВНИМАНИЕ: очередь пуста к моменту остановки — увеличьте --burst");
    }
    s.kill()?;
    keep_log(c, &format!("{tag}-before"));
    println!("  убит: заданий закрыто {jobs}, в очереди {queue}, WAL не подтверждено {pending:?}");
    let (mut s, h, st) = start(c, r)?;
    print_start(&st);
    let (pq, pe, ok) = verify(&mut s, &h)?;
    r.cycles.push(Cycle {
        variant: v.to_string(),
        kind: "kill".into(),
        rep,
        quiet_secs: None,
        worker_stop_ms: None,
        burst_notes: notes.len(),
        burst_chunks: None,
        index_secs: None,
        chunks_per_sec: None,
        jobs_before_stop: jobs,
        wal_pending_at_stop: pending,
        shutdown_wait_ms: wait_ms,
        shutdown_wait_result: wait_res.clone(),
        queue_at_stop: queue,
        stop_ms: None,
        start: st,
        points_qdrant: pq,
        points_expected: pe,
        data_ok: ok,
    });
    drop_burst(&mut s, &h, &format!("burst/{root}/k{rep}"))?;
    settle_and_stop(c, s, &h, &format!("{tag}-after"))
}

/// Сводка по виду циклов: (старт до «Qdrant готов», операций WAL, остановка, скорость).
fn summary(
    r: &FirstStartResult,
    variant: &str,
    kind: &str,
) -> Option<(Stats, Stats, Option<Stats>, Option<Stats>)> {
    let cs: Vec<&Cycle> = r
        .cycles
        .iter()
        .filter(|c| c.kind == kind && c.variant == variant)
        .collect();
    let starts: Vec<f64> = cs
        .iter()
        .filter_map(|c| c.start.qdrant_from_start_ms)
        .collect();
    let wal: Vec<f64> = cs.iter().map(|c| c.start.wal_ops as f64).collect();
    let stop: Vec<f64> = cs.iter().filter_map(|c| c.stop_ms).collect();
    let speed: Vec<f64> = cs.iter().filter_map(|c| c.chunks_per_sec).collect();
    Some((stats(&starts)?, stats(&wal)?, stats(&stop), stats(&speed)))
}

/// Markdown итога запуска.
pub fn markdown(r: &FirstStartResult) -> String {
    let mut s = format!(
        "# bench first-start — {} {}\n\nДата {}, код `{}`, модель {}, заметок {}, параметры: {}\n\n",
        r.profile.map_or("?", |p| p.as_str()),
        r.label.as_deref().unwrap_or(""),
        r.date,
        r.git,
        r.model_id.as_deref().unwrap_or("?"),
        r.notes,
        if r.sets.is_empty() {
            "умолчания".to_string()
        } else {
            r.sets
                .iter()
                .map(|(k, v)| format!("`{k}={v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    if let Some(b) = &r.build {
        s.push_str(&format!(
            "Индексация с нуля: {} чанков за {:.0} с ({:.2} чанка/с)\n\n",
            b.chunks, b.secs, b.chunks_per_sec
        ));
    }
    if let Some(p) = r.prepare_secs {
        s.push_str(&format!("Пересборка коллекции варианта: {p:.1} с\n\n"));
    }
    s.push_str("| Цикл | Старт до «Qdrant готов», мс | Процесс Qdrant, мс | WAL, операций | Повтор до, мс | Модель, мс | Остановка, мс | Скорость пачки, чанка/с | Заданий до остановки / очередь | WAL не подтв. при остановке | Ожидание WAL, мс | Точек до / после reindex | Данные |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let f = |v: Option<f64>| v.map_or("—".into(), |v| format!("{v:.0}"));
    for c in &r.cycles {
        s.push_str(&format!(
            "| {} {} {} | {} | {} | {} | {} | {} | {} | {} | {} / {} | {} | {} | {} / {} | {} |\n",
            c.variant,
            c.kind,
            c.rep,
            f(c.start.qdrant_from_start_ms),
            f(c.start.qdrant_ready_ms),
            c.start.wal_ops,
            f(c.start.replay_done_ms),
            f(c.start.model_load_ms),
            f(c.stop_ms),
            c.chunks_per_sec.map_or("—".into(), |v| format!("{v:.2}")),
            c.jobs_before_stop,
            c.queue_at_stop,
            c.wal_pending_at_stop.map_or("—".into(), |v| v.to_string()),
            match (&c.shutdown_wait_ms, &c.shutdown_wait_result) {
                (Some(ms), Some(r)) => format!("{ms:.0} ({r})"),
                _ => "—".into(),
            },
            c.points_qdrant.map_or("—".into(), |v| v.to_string()),
            c.points_expected.map_or("—".into(), |v| v.to_string()),
            if c.data_ok {
                "целы"
            } else {
                "**расхождение**"
            }
        ));
    }
    let mut variants: Vec<&str> = Vec::new();
    for c in &r.cycles {
        if !variants.contains(&c.variant.as_str()) {
            variants.push(&c.variant);
        }
    }
    for (v, kind) in variants
        .iter()
        .flat_map(|v| ["graceful", "kill"].map(|k| (*v, k)))
    {
        if let Some((st, wal, stop, speed)) = summary(r, v, kind) {
            s.push_str(&format!(
                "\n{v} {kind}: старт медиана {:.0} мс, max {:.0} мс (n = {}); WAL медиана {:.0}, max {:.0}",
                st.p50, st.max, st.n, wal.p50, wal.max
            ));
            if let Some(x) = stop {
                s.push_str(&format!(
                    "; остановка медиана {:.0} мс, max {:.0} мс",
                    x.p50, x.max
                ));
            }
            if let Some(x) = speed {
                s.push_str(&format!("; скорость медиана {:.2} чанка/с", x.p50));
            }
            s.push('\n');
        }
    }
    s
}

/// Контекст варианта: `--set` и переопределения варианта (последние побеждают).
fn variant_ctx(c: &Ctx, sets: &[(String, String)]) -> Ctx {
    let mut v = c.clone();
    v.spec.sets.extend(sets.iter().cloned());
    v
}

/// Точка входа сценария.
pub(super) fn run(c: &Ctx, corpus: &Corpus, a: &Args, r: &mut FirstStartResult) -> Result<()> {
    let data = c.paths.work.join(&c.spec.name).join("data");
    if a.rebuild || !data.is_dir() {
        let dir = c.paths.work.join(&c.spec.name);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("очистка {}", dir.display()))?;
        }
        build(c, corpus, a, r)?;
    } else if !a.no_prepare {
        prepare(c, r)?;
    }
    let variants = if a.variants.is_empty() {
        vec![(r.label.clone().unwrap_or_else(|| "v".into()), Vec::new())]
    } else {
        a.variants.clone()
    };
    for rep in 0..a.reps {
        // нечётные повторы — в обратном порядке (ABBA): линейный дрейф скорости и состояния
        // коллекции внутри повтора не достаётся одному варианту
        let order: Vec<&(String, Vec<(String, String)>)> = if rep % 2 == 0 {
            variants.iter().collect()
        } else {
            variants.iter().rev().collect()
        };
        for (name, sets) in order {
            let cv = variant_ctx(c, sets);
            cycle_graceful(&cv, corpus, a, name, rep, r)?;
            if !a.graceful_only {
                cycle_kill(&cv, corpus, a, name, rep, r)?;
            }
        }
    }
    Ok(())
}

/// Пишет `<out>/<дата>-bench-first-start-<профиль>[-метка].{json,md}`.
pub fn write(out: &Path, r: &FirstStartResult) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    fs::create_dir_all(out)?;
    let mut stem = format!(
        "{}-bench-first-start-{}",
        r.date,
        r.profile.map_or("?", |p| p.as_str())
    );
    if let Some(l) = r.label.as_deref().filter(|l| !l.is_empty()) {
        stem.push('-');
        stem.push_str(l);
    }
    let json_f = out.join(format!("{stem}.json"));
    let md_f = out.join(format!("{stem}.md"));
    fs::write(&json_f, serde_json::to_string_pretty(&json!(r))? + "\n")?;
    fs::write(&md_f, markdown(r))?;
    Ok((json_f, md_f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_ops_from_qdrant_recovered_line() {
        let log = [
            r#"{"timestamp":"2026-10-25T10:00:00Z","fields":{"message":"Qdrant запущен"}}"#,
            r#"{"timestamp":"2026-10-25T10:00:01Z","fields":{"message":"Recovered collection zam_chunks: 39/39 (100%)","component":"qdrant"}}"#,
            "не JSON",
        ]
        .join("\n");
        assert_eq!(wal_ops(&log), 39);
        assert_eq!(wal_ops(""), 0);
        let a = first_ts(&log, "Qdrant запущен").unwrap();
        let b = first_ts(&log, "Recovered collection").unwrap();
        assert_eq!(b.duration_since(a).as_secs_f64(), 1.0);
    }

    #[test]
    fn settle_from_flush_interval() {
        // умолчание M4 — 3 с: 2 × 3 + 8
        assert_eq!(settle(&[]), Duration::from_secs(14));
        let sets = vec![("qdrant.flush_interval_sec".to_string(), "1".to_string())];
        assert_eq!(settle(&sets), Duration::from_secs(10));
    }
}
