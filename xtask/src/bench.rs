//! `cargo xtask bench --profile cpu|gpu` — замеры нефункциональных целей §17 ТЗ.
//!
//! Обвязка — та же, что у `cargo xtask eval` ([`crate::eval::run::Session`]): собранный
//! `zam-server` из `target/release`, модели из `target/models`, ONNX Runtime и Qdrant из
//! `target/deps/<платформа>/`, каталог данных `target/bench/<прогон>/data`. Корпус — заметки
//! `eval/corpus` и компактные копии до `--notes` ([`copies`]), запросы — `eval/queries.toml`.
//!
//! Этапы (`--phase`, по умолчанию все, по порядку):
//! - `rss` — RSS сервера в простое без моделей (пустой каталог) и с моделью;
//! - `index` — индексация корпуса eval (чанков/с), запись ≤ 10 чанков до векторного индекса
//!   при пустой очереди, загрузка копий и поиск `chunks` при полной очереди индексации;
//! - `search` — поиск `chunks` без реранка при пустой очереди (после перезапуска: холодный
//!   кэш эмбеддингов запросов) и время старта до готовности Qdrant;
//! - `rerank` — поиск с `rerank=on` (реранкер устройства эмбеддера);
//! - `kill` — убийство процесса Qdrant под потоком запросов: задержка поиска до
//!   деградации, во время неё и после перезапуска супервизором;
//! - `reindex` — пересборка Qdrant из `embeddings` без инференса;
//! - `fts` — сервер без моделей на том же каталоге: поиск FTS-only и RSS.
//!
//! Каждый замеряемый проход идёт после перезапуска сервера с прогревом двумя посторонними
//! запросами: LRU-кэш эмбеддингов запросов не подменяет инференс. Итог —
//! `eval/results/<дата>-bench-<профиль>[-метка].{json,md}`; каталог данных удаляется (кроме
//! `--keep`).

pub mod copies;
pub mod report;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::eval::corpus::{Corpus, Query, load_queries};
use crate::eval::metrics::quantile;
use crate::eval::run::{RunSpec, Session, build_server, default_model, git_rev, parse_sets, today};
use crate::eval::{Device, Paths};
use crate::smoke::{admin_login, agent_with};

/// Предел одного запроса (реранк на CPU, загрузка реранкера).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Предел ожидания индексации копий.
const INDEX_WAIT: Duration = Duration::from_secs(4 * 3600);
/// Предел ожидания готовности векторного канала после старта.
const READY_WAIT: Duration = Duration::from_secs(300);
/// Глубина выдачи.
const TOP_K: usize = 10;
/// Пауза перед замером RSS в простое.
const IDLE: Duration = Duration::from_secs(3);
/// Сколько длится поток запросов до убийства Qdrant и после восстановления.
const KILL_MARGIN: Duration = Duration::from_secs(3);
/// Предел ожидания восстановления после убийства Qdrant.
const KILL_WAIT: Duration = Duration::from_secs(180);
/// Попыток прохода при полной очереди (см. `phase_index`).
const FULL_QUEUE_ATTEMPTS: usize = 3;
/// Заметки для замера записи — не длиннее (≈ 5–8 чанков по ~200 токенов).
const WRITE_MAX_BYTES: usize = 6000;

/// Профиль §17.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Cpu,
    Gpu,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Cpu => "cpu",
            Profile::Gpu => "gpu",
        }
    }

    fn device(self) -> Device {
        match self {
            Profile::Cpu => Device::Cpu,
            Profile::Gpu => Device::Cuda,
        }
    }
}

/// Этап замера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Rss,
    Index,
    Search,
    Rerank,
    Kill,
    Reindex,
    Fts,
}

/// Аргументы `cargo xtask bench`.
#[derive(Debug, clap::Args)]
pub struct BenchArgs {
    /// cpu — bge-m3-int8 и jina-reranker-v2 на CPU; gpu — bge-m3-fp16 и bge-reranker-v2-m3
    /// на CUDA
    #[arg(long, value_enum)]
    pub profile: Profile,
    /// Заметок всего: корпус eval и компактные копии до этого числа
    #[arg(long, default_value_t = 10_000)]
    pub notes: usize,
    /// Модель эмбеддингов вместо умолчания профиля
    #[arg(long)]
    pub model: Option<String>,
    /// Реранкер вместо выбранного сервером по устройству
    #[arg(long)]
    pub reranker: Option<String>,
    /// Этапы (можно повторять; по умолчанию все)
    #[arg(long = "phase", value_enum)]
    pub phases: Vec<Phase>,
    /// Запросов в проходе с реранком (каждый k-й из eval/queries.toml)
    #[arg(long, default_value_t = 100)]
    pub rerank_queries: usize,
    /// Записей в замере «запись до векторного индекса»
    #[arg(long, default_value_t = 100)]
    pub writes: usize,
    /// Этап index заканчивается после поиска при полной очереди: индексация копий не
    /// дожидается (скорость на 10 тыс. заметок не замеряется; на CPU это ~2 ч); остальные
    /// этапы тогда работают на частично проиндексированном каталоге — только с `--phase index`
    #[arg(long)]
    pub no_copies_wait: bool,
    /// Не пересоздавать каталог: этапы на уже проиндексированных данных прошлого прогона
    /// (`--keep`); этапы rss и index при этом недоступны
    #[arg(long)]
    pub reuse: bool,
    /// Не удалять каталог данных после прогона (target/bench/<прогон>)
    #[arg(long)]
    pub keep: bool,
    /// Переопределение конфигурации сервера KEY=VALUE (`ml.query_threads=4`)
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub sets: Vec<String>,
    /// Метка в имени прогона и файла результата
    #[arg(long)]
    pub label: Option<String>,
    /// zam-server (по умолчанию собирается `cargo build --release -p zam-server`)
    #[arg(long)]
    pub server_bin: Option<PathBuf>,
    /// Каталог результатов (по умолчанию eval/results)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Не убирать из PATH сервера каталоги CUDA Toolkit
    #[arg(long)]
    pub keep_cuda_path: bool,
}

/// Сводка времён, мс.
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

fn stats(v: &[f64]) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    Some(Stats {
        n: v.len(),
        p50: quantile(v, 0.5),
        p95: quantile(v, 0.95),
        max: v.iter().copied().fold(f64::MIN, f64::max),
        mean: v.iter().sum::<f64>() / v.len() as f64,
    })
}

/// Проход запросов поиска.
#[derive(Debug, Clone, Serialize)]
pub struct Pass {
    pub rerank: bool,
    /// Время ответа на клиенте.
    pub client: Option<Stats>,
    /// `timings` сервера по этапам.
    pub server: BTreeMap<String, Stats>,
    pub degraded: usize,
    pub warnings: usize,
    /// Заданий индексации в очереди (queued + running) до и после прохода.
    pub queue_before: u64,
    pub queue_after: u64,
    /// Работа воркера за проход: закрытых заданий и время инференса индексации, мс
    /// (`/index/status` → `worker`, сессия администратора).
    pub jobs_done: u64,
    pub index_embed_ms: f64,
    /// Проход «при полной очереди» действителен, только если воркер за него реально
    /// индексировал (`jobs_done > 0` и инференс шёл); иначе — `false` и пометка в отчёте.
    pub under_load: Option<bool>,
    /// Попытка, давшая этот проход (полная очередь: повтор после перезапуска, если воркер
    /// за проход не индексировал).
    pub attempts: usize,
}

/// Индексация части корпуса.
#[derive(Debug, Clone, Serialize)]
pub struct IndexRun {
    pub notes: usize,
    /// Листовых чанков (`level = chunk`) и средний размер, токенов.
    pub chunks: u64,
    pub avg_tokens: f64,
    /// От начала загрузки до пустой очереди.
    pub secs: f64,
    pub load_secs: f64,
    pub chunks_per_sec: f64,
    /// `POST /nodes` во время загрузки (запись до FTS при работающей индексации).
    pub post: Option<Stats>,
    /// Время воркера за прогон (`/index/status` → `worker`): инференс и запись в Qdrant;
    /// остальное — SQLite, свёртки L0 и ожидание.
    pub embed_secs: f64,
    pub qdrant_secs: f64,
}

/// Счётчики воркера с запуска сервера (`/index/status` → `worker`, только администратору).
#[derive(Debug, Clone, Copy, Default)]
struct WorkerStat {
    jobs_done: u64,
    embed_ms: f64,
    qdrant_ms: f64,
}

impl WorkerStat {
    fn of(status: &Value) -> Result<WorkerStat> {
        let w = &status["worker"];
        if w.is_null() {
            bail!("в /index/status нет worker: статус запрошен не администратором");
        }
        Ok(WorkerStat {
            jobs_done: w["jobs_done"].as_u64().unwrap_or(0),
            embed_ms: w["embed_ms"].as_f64().unwrap_or(0.0),
            qdrant_ms: w["qdrant_ms"].as_f64().unwrap_or(0.0),
        })
    }

    /// Разность `self - before`.
    fn since(self, before: WorkerStat) -> WorkerStat {
        WorkerStat {
            jobs_done: self.jobs_done.saturating_sub(before.jobs_done),
            embed_ms: self.embed_ms - before.embed_ms,
            qdrant_ms: self.qdrant_ms - before.qdrant_ms,
        }
    }
}

/// `(инференс, Qdrant)` воркера с запуска сервера, с.
fn worker_busy(h: &Http) -> Result<(f64, f64)> {
    let w = WorkerStat::of(&h.status()?)?;
    Ok((w.embed_ms / 1000.0, w.qdrant_ms / 1000.0))
}

/// Ждёт, пока воркер закроет хотя бы одно задание (индексация идёт), не дольше `limit`.
fn wait_worker_progress(h: &Http, limit: Duration) -> Result<bool> {
    let w0 = WorkerStat::of(&h.status()?)?;
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        thread::sleep(Duration::from_millis(100));
        let w = WorkerStat::of(&h.status()?)?.since(w0);
        if w.jobs_done > 0 && w.embed_ms > 0.0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Старт сервера на готовом каталоге.
#[derive(Debug, Clone, Serialize)]
pub struct Startup {
    /// От запуска процесса до `/health` (сервер слушает после загрузки модели).
    pub health_ms: f64,
    /// От запуска процесса до готовности векторного канала (`/index/status`: Qdrant готов,
    /// модель загружена).
    pub ready_ms: f64,
    /// От запуска процесса до строки «Qdrant готов» журнала (метка времени строки) — цель
    /// §17 «старт до готовности Qdrant».
    pub qdrant_from_start_ms: Option<f64>,
    /// Готовность самого процесса Qdrant от его запуска (`ready_ms` той же строки).
    pub qdrant_ready_ms: Option<f64>,
    /// Загрузка модели (`ms` строки «модель загружена»).
    pub model_load_ms: Option<f64>,
}

/// Убийство Qdrant под потоком запросов.
#[derive(Debug, Clone, Serialize)]
pub struct KillRun {
    pub before: Option<Stats>,
    /// Ответы от убийства до первого недеградированного.
    pub down: Option<Stats>,
    pub after: Option<Stats>,
    /// Первый ответ после убийства.
    pub first_after_kill_ms: Option<f64>,
    /// От убийства до первого ответа с `degraded: true`.
    pub to_degraded_ms: Option<f64>,
    /// От убийства до первого ответа без деградации после перезапуска.
    pub to_restored_ms: Option<f64>,
    /// Время этого ответа (первый запрос к новому процессу Qdrant, холодный).
    pub first_restored_ms: Option<f64>,
    pub reasons: BTreeMap<String, usize>,
    pub restarts: u64,
    /// Переходы состояния ответа: (мс от убийства, время ответа, причина деградации или
    /// `ok`) — первый ответ каждой серии.
    pub timeline: Vec<(f64, f64, String)>,
}

/// Пересборка из `embeddings`.
#[derive(Debug, Clone, Serialize)]
pub struct ReindexRun {
    pub points: u64,
    pub misses: u64,
    pub elapsed_ms: f64,
    pub wall_ms: f64,
    pub points_per_sec: f64,
}

/// Запись до векторного индекса.
#[derive(Debug, Clone, Serialize)]
pub struct WriteRun {
    /// `POST /nodes` (до FTS).
    pub post: Option<Stats>,
    /// От `POST` до пустой очереди: задание закрыто после подтверждённой записи точек
    /// (`wait=true`).
    pub to_vector: Option<Stats>,
    /// Разбивка одной записи по счётчикам воркера: инференс, запись в Qdrant (удаление и
    /// upsert) и остальное (ожидание задания, SQLite, планирование свёрток) — `to_vector`
    /// минус `post`, инференс и Qdrant.
    pub embed: Option<Stats>,
    pub qdrant: Option<Stats>,
    pub other: Option<Stats>,
    pub chunks_avg: f64,
    pub chunks_max: u64,
    /// Записей больше чем на 10 чанков — не в замере.
    pub excluded: usize,
}

/// Результат прогона.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BenchResult {
    pub date: String,
    pub git: String,
    pub profile: Option<Profile>,
    pub label: Option<String>,
    pub host: Value,
    pub model_id: Option<String>,
    pub device: Value,
    pub reranker: Option<String>,
    pub sets: BTreeMap<String, String>,
    pub notes: usize,
    pub queries: usize,
    pub rss_idle_no_models: Option<u64>,
    pub rss_idle_model: Option<u64>,
    /// Частная резидентная память с моделью в простое: рабочий набор Windows включает
    /// страницы образов DLL (CUDA, cuDNN, ORT) и отображённых файлов, частный — нет.
    pub private_idle_model: Option<u64>,
    pub rss_after_search: Option<u64>,
    pub private_after_search: Option<u64>,
    pub rss_after_rerank: Option<u64>,
    pub rss_fts_loaded: Option<u64>,
    pub qdrant_rss: Option<u64>,
    pub qdrant_points: Option<u64>,
    pub index_base: Option<IndexRun>,
    pub index_copies: Option<IndexRun>,
    pub write: Option<WriteRun>,
    pub search_full_queue: Option<Pass>,
    pub search_empty_queue: Option<Pass>,
    pub search_cached: Option<Pass>,
    pub rerank: Option<Pass>,
    pub fts_only: Option<Pass>,
    pub startup: Vec<Startup>,
    pub kill: Option<KillRun>,
    pub reindex: Option<ReindexRun>,
}

/// HTTP-клиент с постоянным соединением (без накладных расходов TCP на каждый запрос).
struct Http {
    agent: ureq::Agent,
    base: String,
    token: String,
    /// Cookie сессии администратора: статус (`worker` виден только ему) и reindex.
    admin: crate::smoke::AdminSession,
}

impl Http {
    fn new(s: &Session) -> Result<Http> {
        Ok(Http {
            agent: agent_with(REQUEST_TIMEOUT),
            base: s.base.clone(),
            token: s.token().to_string(),
            admin: admin_login(&s.base)?,
        })
    }

    fn call(&self, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", self.token);
        let mut r = match body {
            Some(b) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .content_type("application/json")
                .send(b.to_string())?,
            None => self.agent.get(&url).header("Authorization", &auth).call()?,
        };
        let st = r.status().as_u16();
        let text = r.body_mut().read_to_string()?;
        Ok((st, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// `/index/status` сессией администратора (у прогона один пользователь — он же).
    fn status(&self) -> Result<Value> {
        let mut r = self
            .agent
            .get(&format!("{}/api/v1/index/status", self.base))
            .header("Cookie", &self.admin.cookie)
            .call()?;
        let st = r.status().as_u16();
        let text = r.body_mut().read_to_string()?;
        if st != 200 {
            bail!("GET /index/status: HTTP {st}");
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Заданий в очереди (queued + running).
    fn queue(&self) -> Result<u64> {
        let s = self.status()?;
        Ok(
            s["queue"]["queued"].as_u64().unwrap_or(0)
                + s["queue"]["running"].as_u64().unwrap_or(0),
        )
    }

    fn post_note(&self, path: &str, text: &str) -> Result<()> {
        let body = json!({ "space": "knowledge", "path": path, "kind": "note", "content": text });
        let (st, v) = self.call("/api/v1/nodes", Some(&body))?;
        if st != 201 {
            bail!("POST /nodes {path}: HTTP {st}: {v}");
        }
        Ok(())
    }

    fn search(&self, q: &str, rerank: bool) -> Result<(f64, Value)> {
        let body = json!({
            "query": q,
            "mode": "chunks",
            "top_k": TOP_K,
            "scope": { "spaces": ["knowledge"] },
            "rerank": if rerank { "on" } else { "off" },
        });
        let t0 = Instant::now();
        let (st, v) = self.call("/api/v1/search", Some(&body))?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if st != 200 {
            bail!("POST /search: HTTP {st}: {v}");
        }
        Ok((ms, v))
    }
}

/// Проход запросов `queries` (`chunks`, top 10, Knowledge) по одному, по порядку.
fn pass(h: &Http, queries: &[String], rerank: bool) -> Result<Pass> {
    let queue_before = h.queue()?;
    let w0 = WorkerStat::of(&h.status()?)?;
    let mut client = Vec::with_capacity(queries.len());
    let mut server: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let (mut degraded, mut warnings) = (0, 0);
    for q in queries {
        let (ms, v) = h.search(q, rerank)?;
        client.push(ms);
        if v["degraded"] == true {
            degraded += 1;
        }
        if v["warnings"].as_array().is_some_and(|w| !w.is_empty()) {
            warnings += 1;
        }
        if let Some(t) = v["timings"].as_object() {
            for (k, x) in t {
                if let Some(x) = x.as_f64() {
                    server.entry(k.clone()).or_default().push(x);
                }
            }
        }
    }
    Ok(Pass {
        rerank,
        client: stats(&client),
        server: server
            .into_iter()
            .filter_map(|(k, v)| stats(&v).map(|s| (k, s)))
            .collect(),
        degraded,
        warnings,
        queue_before,
        queue_after: h.queue()?,
        jobs_done: 0,
        index_embed_ms: 0.0,
        under_load: None,
        attempts: 1,
    })
    .and_then(|mut p| {
        let w = WorkerStat::of(&h.status()?)?.since(w0);
        p.jobs_done = w.jobs_done;
        p.index_embed_ms = w.embed_ms;
        Ok(p)
    })
}

fn print_pass(name: &str, p: &Pass) {
    if let Some(c) = &p.client {
        println!(
            "  {name}: p50 {:.1} мс, p95 {:.1} мс, max {:.1} мс (n = {}, degraded {}, warnings {}, очередь {} → {}, воркер: заданий {}, инференс {:.0} мс)",
            c.p50,
            c.p95,
            c.max,
            c.n,
            p.degraded,
            p.warnings,
            p.queue_before,
            p.queue_after,
            p.jobs_done,
            p.index_embed_ms
        );
    }
}

/// Прогрев после старта: посторонний запрос (не из набора: кэш запросов не заполняется
/// запросами прохода), с реранком — и загрузка реранкера.
fn warm_up(h: &Http, rerank: bool) -> Result<()> {
    for q in [
        "прогрев сервера перед замером",
        "warm-up query before measurement",
    ] {
        h.search(q, rerank)?;
    }
    Ok(())
}

fn rss(h: &Http) -> Result<Option<u64>> {
    Ok(h.status()?["server"]["rss_bytes"].as_u64())
}

/// Частная резидентная память процесса: Windows — частный рабочий набор
/// (`WorkingSetPrivate`, без страниц образов DLL и файлов), Linux — `RssAnon`.
fn private_bytes(pid: u32) -> Option<u64> {
    if cfg!(windows) {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "(Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter \"IDProcess={pid}\").WorkingSetPrivate"
                ),
            ])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    } else {
        let s = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let kb: u64 = s
            .lines()
            .find(|l| l.starts_with("RssAnon:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        Some(kb * 1024)
    }
}

/// Листовые чанки заметок с путями по шаблону `LIKE` и средний размер в токенах.
fn chunk_stats(data: &Path, like: &str) -> Result<(u64, f64, u64)> {
    let c = rusqlite::Connection::open_with_flags(
        crate::eval::gate::user_db(data)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let (n, avg): (i64, f64) = c.query_row(
        "SELECT count(*), coalesce(avg(c.token_count), 0.0) FROM chunks c
         JOIN nodes n ON n.id = c.node_id
         WHERE c.level = 'chunk' AND n.deleted_at IS NULL AND n.path LIKE ?1",
        [like],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let max: i64 = c.query_row(
        "SELECT coalesce(max(k), 0) FROM (SELECT count(*) AS k FROM chunks c
         JOIN nodes n ON n.id = c.node_id
         WHERE c.level = 'chunk' AND n.deleted_at IS NULL AND n.path LIKE ?1
         GROUP BY c.node_id)",
        [like],
        |r| r.get(0),
    )?;
    Ok((n as u64, avg, max as u64))
}

/// Листовых чанков по путям заметок (шаблон `LIKE`).
fn chunks_per_path(data: &Path, like: &str) -> Result<BTreeMap<String, u64>> {
    let c = rusqlite::Connection::open_with_flags(
        crate::eval::gate::user_db(data)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut st = c.prepare(
        "SELECT n.path, count(*) FROM chunks c JOIN nodes n ON n.id = c.node_id
         WHERE c.level = 'chunk' AND n.deleted_at IS NULL AND n.path LIKE ?1
         GROUP BY n.path",
    )?;
    let rows = st.query_map([like], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Ждёт пустой очереди и доступного векторного канала; печатает прогресс.
fn wait_idle(s: &mut Session, h: &Http, t0: Instant, limit: Duration) -> Result<()> {
    let mut last = Instant::now();
    loop {
        s.ensure_alive()?;
        let st = h.status()?;
        let q = &st["queue"];
        if q["failed"].as_u64().unwrap_or(0) > 0 {
            bail!(
                "задания индексации завершились ошибкой: {}",
                st["failed_items"]
            );
        }
        if q["queued"] == 0 && q["running"] == 0 && st["vector"]["available"] == true {
            return Ok(());
        }
        if t0.elapsed() > limit {
            bail!("индексация не завершилась за {} с: {q}", limit.as_secs());
        }
        if last.elapsed() > Duration::from_secs(60) {
            println!(
                "  индексация: в очереди {}, выполняется {} ({:.0} с)",
                q["queued"],
                q["running"],
                t0.elapsed().as_secs_f64()
            );
            last = Instant::now();
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Последняя строка журнала сервера с сообщением `message`.
fn log_line(log: &Path, message: &str) -> Option<Value> {
    let text = fs::read_to_string(log).ok()?;
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .rfind(|v| v["fields"]["message"] == message)
}

/// Миллисекунды от `t0` до метки времени строки журнала.
fn ms_since(t0: jiff::Timestamp, line: &Value) -> Option<f64> {
    let ts: jiff::Timestamp = line["timestamp"].as_str()?.parse().ok()?;
    Some(ts.duration_since(t0).as_secs_f64() * 1000.0)
}

/// Ждёт готовности Qdrant и векторного канала после старта.
fn wait_ready(s: &mut Session, h: &Http) -> Result<()> {
    let t0 = Instant::now();
    loop {
        s.ensure_alive()?;
        let st = h.status()?;
        if st["qdrant"]["state"] == "ready" && st["vector"]["available"] == true {
            return Ok(());
        }
        if t0.elapsed() > READY_WAIT {
            bail!(
                "векторный канал не готов за {} с: {} / {}",
                READY_WAIT.as_secs(),
                st["qdrant"],
                st["vector"]
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Контекст прогона.
struct Ctx {
    paths: Paths,
    server: PathBuf,
    spec: RunSpec,
    queries: Vec<String>,
}

impl Ctx {
    fn log(&self) -> PathBuf {
        self.paths
            .work
            .join(&self.spec.name)
            .join("zam-server.stderr.log")
    }

    fn fts_spec(&self) -> RunSpec {
        RunSpec {
            model: None,
            ..self.spec.clone()
        }
    }

    /// Старт с моделью (каталог не пересоздаётся); замер времени до готовности. Модель и
    /// устройство из `/index/status` — в результат.
    fn start(&self, r: &mut BenchResult) -> Result<(Session, Http, Startup)> {
        let wall0 = jiff::Timestamp::now();
        let t0 = Instant::now();
        let mut s = Session::start(&self.paths, &self.spec, &self.server, false)?;
        let health_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let h = Http::new(&s)?;
        wait_ready(&mut s, &h)?;
        let ready_ms = t0.elapsed().as_secs_f64() * 1000.0;
        s.check_model()?;
        r.model_id = s.status["model"]["id"].as_str().map(String::from);
        r.device = s.status["device"].clone();
        let q = log_line(&self.log(), "Qdrant готов");
        let st = Startup {
            health_ms,
            ready_ms,
            qdrant_from_start_ms: q.as_ref().and_then(|l| ms_since(wall0, l)),
            qdrant_ready_ms: q.as_ref().and_then(|l| l["fields"]["ready_ms"].as_f64()),
            model_load_ms: log_line(&self.log(), "модель загружена")
                .and_then(|l| l["fields"]["ms"].as_f64()),
        };
        Ok((s, h, st))
    }

    /// Останавливает сервер и сохраняет его журнал как `zam-server-<этап>.log` (следующий
    /// старт перезаписывает `zam-server.stderr.log`).
    fn stop(&self, s: Session, tag: &str) -> Result<()> {
        s.stop()?;
        let log = self.log();
        let to = log.with_file_name(format!("zam-server-{tag}.log"));
        let _ = fs::rename(&log, &to);
        Ok(())
    }

    fn start_fts(&self) -> Result<(Session, Http)> {
        let s = Session::start(&self.paths, &self.fts_spec(), &self.server, false)?;
        let h = Http::new(&s)?;
        Ok((s, h))
    }
}

/// Этап `rss` (каталог пересоздаётся) — RSS без моделей и с моделью в простое.
fn phase_rss(c: &Ctx, r: &mut BenchResult) -> Result<()> {
    println!("[rss] сервер без моделей на пустом каталоге");
    let (s, h) = c.start_fts()?;
    warm_up(&h, false)?;
    thread::sleep(IDLE);
    r.rss_idle_no_models = rss(&h)?;
    c.stop(s, "rss-fts")?;
    println!("  RSS без моделей: {}", report::mb(r.rss_idle_no_models));
    let (s, h, st) = c.start(r)?;
    thread::sleep(IDLE);
    r.rss_idle_model = rss(&h)?;
    r.private_idle_model = private_bytes(s.pid());
    println!(
        "  RSS с моделью: {} (старт {:.0} мс)",
        report::mb(r.rss_idle_model),
        st.ready_ms
    );
    c.stop(s, "rss")
}

/// Этап `index`.
fn phase_index(c: &Ctx, corpus: &Corpus, a: &BenchArgs, r: &mut BenchResult) -> Result<()> {
    let (notes, writes) = (a.notes, a.writes);
    let (mut s, mut h, _) = c.start(r)?;
    // 1. корпус eval: чанков/с
    println!("[index] корпус eval: {} заметок", corpus.notes.len());
    let w0 = worker_busy(&h)?;
    let t0 = Instant::now();
    s.load_corpus(corpus)?;
    let load_secs = t0.elapsed().as_secs_f64();
    wait_idle(&mut s, &h, t0, INDEX_WAIT)?;
    let secs = t0.elapsed().as_secs_f64();
    let w1 = worker_busy(&h)?;
    let mut chunks = 0;
    let mut tokens = 0.0;
    for like in ["ru/%", "en/%"] {
        let (n, avg, _) = chunk_stats(&s.data, like)?;
        tokens += avg * n as f64;
        chunks += n;
    }
    let base = IndexRun {
        notes: corpus.notes.len(),
        chunks,
        avg_tokens: if chunks > 0 {
            tokens / chunks as f64
        } else {
            0.0
        },
        secs,
        load_secs,
        chunks_per_sec: chunks as f64 / secs,
        post: None,
        embed_secs: w1.0 - w0.0,
        qdrant_secs: w1.1 - w0.1,
    };
    println!(
        "  {} чанков (~{:.0} ток.) за {:.1} с: {:.2} чанка/с (инференс {:.0} с, Qdrant {:.0} с)",
        base.chunks,
        base.avg_tokens,
        base.secs,
        base.chunks_per_sec,
        base.embed_secs,
        base.qdrant_secs
    );
    r.index_base = Some(base);

    // 2. запись ≤ 10 чанков при пустой очереди
    println!("[index] запись до векторного индекса: {writes} заметок");
    let (mut post, mut total) = (Vec::new(), Vec::new());
    let (mut embed, mut qdrant, mut other) = (Vec::new(), Vec::new(), Vec::new());
    // заметки корпуса до ~6 КБ: не больше 10 чанков (цель §17 — запись ≤ 10 чанков); по
    // кругу, если их меньше `writes` (путь копии другой — кэш эмбеддингов не срабатывает)
    let small: Vec<_> = corpus
        .notes
        .values()
        .filter(|n| n.text.len() <= WRITE_MAX_BYTES)
        .collect();
    if small.is_empty() {
        bail!("в корпусе нет заметок до {WRITE_MAX_BYTES} байт");
    }
    for i in 0..writes {
        let n = small[i % small.len()];
        let path = format!("bench-write/w{i:03}");
        let text = format!("# Запись {i}\n\n{}", n.text);
        let w0 = WorkerStat::of(&h.status()?)?;
        let t0 = Instant::now();
        h.post_note(&path, &text)?;
        let p_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let w = loop {
            let st = h.status()?;
            let q = &st["queue"];
            if q["queued"] == 0 && q["running"] == 0 {
                break WorkerStat::of(&st)?.since(w0);
            }
            if t0.elapsed() > Duration::from_secs(120) {
                bail!("запись {path}: очередь не опустела за 120 с");
            }
            thread::sleep(Duration::from_millis(10));
        };
        let t_ms = t0.elapsed().as_secs_f64() * 1000.0;
        post.push(p_ms);
        total.push(t_ms);
        embed.push(w.embed_ms);
        qdrant.push(w.qdrant_ms);
        other.push((t_ms - p_ms - w.embed_ms - w.qdrant_ms).max(0.0));
    }
    // цель §17 — запись не больше 10 чанков: записи, нарезанные на большее число, в замер
    // не входят (заметка до 6 КБ иногда даёт 11 листов)
    let counts = chunks_per_path(&s.data, "bench-write/%")?;
    let keep: Vec<usize> = (0..writes)
        .filter(|i| {
            counts
                .get(&format!("bench-write/w{i:03}"))
                .is_some_and(|n| *n <= 10)
        })
        .collect();
    let pick = |v: &[f64]| -> Vec<f64> { keep.iter().map(|&i| v[i]).collect() };
    let kept: Vec<u64> = (0..writes)
        .filter_map(|i| counts.get(&format!("bench-write/w{i:03}")).copied())
        .filter(|n| *n <= 10)
        .collect();
    let w = WriteRun {
        post: stats(&pick(&post)),
        to_vector: stats(&pick(&total)),
        embed: stats(&pick(&embed)),
        qdrant: stats(&pick(&qdrant)),
        other: stats(&pick(&other)),
        chunks_avg: if kept.is_empty() {
            0.0
        } else {
            kept.iter().sum::<u64>() as f64 / kept.len() as f64
        },
        chunks_max: kept.iter().copied().max().unwrap_or(0),
        excluded: writes - keep.len(),
    };
    if w.excluded > 0 {
        println!("  записей больше 10 чанков, не в замере: {}", w.excluded);
    }
    if let (Some(t), Some(e), Some(q), Some(o)) = (&w.to_vector, &w.embed, &w.qdrant, &w.other) {
        println!(
            "  до векторного индекса: p50 {:.0} мс, p95 {:.0} мс, max {:.0} мс; чанков в среднем {:.1}, max {}",
            t.p50, t.p95, t.max, w.chunks_avg, w.chunks_max
        );
        println!(
            "  из них (p50 / p95): инференс {:.0} / {:.0} мс, Qdrant {:.0} / {:.0} мс, остальное {:.0} / {:.0} мс",
            e.p50, e.p95, q.p50, q.p95, o.p50, o.p95
        );
    }
    r.write = Some(w);

    // 3. копии до `notes`, поиск при полной очереди
    let list = copies::make(corpus, notes);
    if list.is_empty() {
        return c.stop(s, "index");
    }
    println!("[index] копии: {} заметок", list.len());
    s.begin_load();
    let w0 = worker_busy(&h)?;
    let t0 = Instant::now();
    let mut posts = Vec::with_capacity(list.len());
    for n in &list {
        let t = Instant::now();
        h.post_note(&n.path, &n.text)?;
        posts.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let load_secs = t0.elapsed().as_secs_f64();
    println!("  загружено за {load_secs:.1} с, поиск при полной очереди…");
    // воркер должен реально индексировать: иначе «полная очередь» — просто длинный список
    // заданий без нагрузки (так было в первом прогоне GPU: очередь 9279 → 9279)
    // Воркер на HDD бывает занят секундами записи (Qdrant `wait=true`, checkpoint SQLite) и
    // за проход в ~6 с не закрывает ни одного задания: тогда проход повторяется после
    // перезапуска сервера (холодный кэш запросов, очередь — в SQLite, не теряется).
    let mut p = None;
    for attempt in 1..=FULL_QUEUE_ATTEMPTS {
        if attempt > 1 {
            println!("  повтор {attempt}: перезапуск сервера (холодный кэш запросов)");
            c.stop(s, &format!("index-try{}", attempt - 1))?;
            (s, h, _) = c.start(r)?;
        }
        if !wait_worker_progress(&h, Duration::from_secs(120))? {
            println!("  ВНИМАНИЕ: воркер не закрыл ни одного задания за 120 с");
        }
        let mut q = pass(&h, &c.queries, false)?;
        q.attempts = attempt;
        q.under_load = Some(q.jobs_done > 0 && q.index_embed_ms > 0.0);
        let ok = q.under_load == Some(true);
        print_pass("chunks, полная очередь", &q);
        p = Some(q);
        if ok {
            break;
        }
        println!("  за проход воркер не индексировал — замер недействителен");
    }
    let p = p.expect("хотя бы одна попытка");
    r.search_full_queue = Some(p);
    if a.no_copies_wait {
        println!("  --no-copies-wait: индексация копий не дожидается");
        return c.stop(s, "index");
    }
    wait_idle(&mut s, &h, t0, INDEX_WAIT)?;
    let secs = t0.elapsed().as_secs_f64();
    let w1 = worker_busy(&h)?;
    let (n, avg, _) = chunk_stats(&s.data, "bench/%")?;
    let run = IndexRun {
        notes: list.len(),
        chunks: n,
        avg_tokens: avg,
        secs,
        load_secs,
        chunks_per_sec: n as f64 / secs,
        post: stats(&posts),
        embed_secs: w1.0 - w0.0,
        qdrant_secs: w1.1 - w0.1,
    };
    println!(
        "  {} чанков (~{:.0} ток.) за {:.1} с: {:.2} чанка/с (инференс {:.0} с, Qdrant {:.0} с)",
        run.chunks, run.avg_tokens, run.secs, run.chunks_per_sec, run.embed_secs, run.qdrant_secs
    );
    r.index_copies = Some(run);
    let st = h.status()?;
    r.qdrant_points = st["qdrant"]["points"].as_u64();
    r.qdrant_rss = st["qdrant"]["rss_bytes"].as_u64();
    c.stop(s, "index")
}

/// Этап `search`: старт (замер), поиск при пустой очереди, повтор из кэша.
fn phase_search(c: &Ctx, r: &mut BenchResult) -> Result<()> {
    println!("[search] перезапуск и поиск при пустой очереди");
    let (s, h, st) = c.start(r)?;
    println!("  старт до готовности: {:.0} мс", st.ready_ms);
    r.startup.push(st);
    warm_up(&h, false)?;
    let p = pass(&h, &c.queries, false)?;
    print_pass("chunks, пустая очередь", &p);
    r.search_empty_queue = Some(p);
    let p = pass(&h, &c.queries, false)?;
    print_pass("chunks, повтор (кэш запросов)", &p);
    r.search_cached = Some(p);
    thread::sleep(IDLE);
    let st = h.status()?;
    r.rss_after_search = st["server"]["rss_bytes"].as_u64();
    r.private_after_search = private_bytes(s.pid());
    r.qdrant_rss = st["qdrant"]["rss_bytes"].as_u64().or(r.qdrant_rss);
    r.qdrant_points = st["qdrant"]["points"].as_u64().or(r.qdrant_points);
    println!("  RSS после поиска: {}", report::mb(r.rss_after_search));
    c.stop(s, "search")
}

/// Каждый k-й запрос набора, всего `n`.
fn subset(queries: &[String], n: usize) -> Vec<String> {
    if n >= queries.len() || n == 0 {
        return queries.to_vec();
    }
    let step = queries.len() as f64 / n as f64;
    (0..n)
        .map(|i| queries[((i as f64 * step) as usize).min(queries.len() - 1)].clone())
        .collect()
}

/// Этап `rerank`.
fn phase_rerank(c: &Ctx, n: usize, r: &mut BenchResult) -> Result<()> {
    println!("[rerank] перезапуск и поиск с rerank=on");
    let (s, h, st) = c.start(r)?;
    r.startup.push(st);
    warm_up(&h, true)?;
    r.reranker = Some(match (&c.spec.reranker, r.device["kind"].as_str()) {
        (Some(id), _) => id.clone(),
        (None, Some("cuda")) => "bge-reranker-v2-m3".into(),
        (None, _) => "jina-reranker-v2".into(),
    });
    let p = pass(&h, &subset(&c.queries, n), true)?;
    print_pass("chunks, rerank=on", &p);
    if p.warnings > 0 {
        println!("  ВНИМАНИЕ: реранк не выполнен в {} запросах", p.warnings);
    }
    r.rerank = Some(p);
    thread::sleep(IDLE);
    r.rss_after_rerank = rss(&h)?;
    println!("  RSS с реранкером: {}", report::mb(r.rss_after_rerank));
    c.stop(s, "rerank")
}

fn qdrant_pid(data: &Path) -> Result<u32> {
    let f = data.join("qdrant").join("qdrant.pid");
    fs::read_to_string(&f)
        .with_context(|| format!("чтение {}", f.display()))?
        .trim()
        .parse()
        .with_context(|| format!("pid в {}", f.display()))
}

fn kill_pid(pid: u32) -> Result<()> {
    let st = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .status()?
    } else {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()?
    };
    if !st.success() {
        bail!("не удалось убить процесс {pid}: {st}");
    }
    Ok(())
}

/// Этап `kill`: поток запросов, убийство Qdrant, восстановление супервизором.
fn phase_kill(c: &Ctx, r: &mut BenchResult) -> Result<()> {
    println!("[kill] убийство Qdrant под потоком запросов");
    let (mut s, h, st) = c.start(r)?;
    r.startup.push(st);
    warm_up(&h, false)?;
    let t0 = Instant::now();
    let mut killed: Option<Instant> = None;
    let mut degraded_at: Option<f64> = None;
    let mut restored_at: Option<f64> = None;
    let mut first_after: Option<f64> = None;
    let mut first_restored: Option<f64> = None;
    let (mut before, mut down, mut after) = (Vec::new(), Vec::new(), Vec::new());
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut timeline: Vec<(f64, f64, String)> = Vec::new();
    let restarts0 = h.status()?["qdrant"]["restarts"].as_u64().unwrap_or(0);
    // Qdrant снова готов (перезапуск засчитан, канал включён): с этого момента первый
    // недеградированный ответ — восстановление. Ответ без деградации ещё до перезапуска
    // возможен: клиент Qdrant переподключается и дожидается нового процесса.
    let mut back: Option<Instant> = None;
    let mut polled = Instant::now();
    let mut i = 0usize;
    loop {
        s.ensure_alive()?;
        if killed.is_none() && t0.elapsed() > KILL_MARGIN {
            kill_pid(qdrant_pid(&s.data)?)?;
            killed = Some(Instant::now());
        }
        let q = &c.queries[i % c.queries.len()];
        i += 1;
        let sent = Instant::now();
        let (ms, v) = h.search(q, false)?;
        let deg = v["degraded"] == true;
        let Some(k) = killed else {
            before.push(ms);
            continue;
        };
        let since = |t: Instant| t.saturating_duration_since(k).as_secs_f64() * 1000.0;
        if first_after.is_none() {
            first_after = Some(ms);
        }
        let label = if deg {
            v["degraded_reason"].as_str().unwrap_or("?").to_string()
        } else {
            "ok".to_string()
        };
        if timeline.last().is_none_or(|(_, _, l)| *l != label) {
            timeline.push((since(sent), ms, label));
        }
        if deg {
            let reason = v["degraded_reason"].as_str().unwrap_or("?").to_string();
            *reasons.entry(reason).or_default() += 1;
            degraded_at.get_or_insert(since(Instant::now()));
        }
        if back.is_none() && polled.elapsed() > Duration::from_millis(200) {
            polled = Instant::now();
            let st = h.status()?;
            if st["qdrant"]["restarts"].as_u64().unwrap_or(0) > restarts0
                && st["qdrant"]["state"] == "ready"
                && st["vector"]["available"] == true
            {
                back = Some(Instant::now());
            }
        }
        if restored_at.is_none() {
            if !deg && (degraded_at.is_some() || back.is_some_and(|b| sent >= b)) {
                restored_at = Some(since(sent));
                first_restored = Some(ms);
                after.push(ms);
            } else {
                down.push(ms);
            }
        } else {
            after.push(ms);
            if k.elapsed().as_secs_f64() * 1000.0
                > restored_at.unwrap_or(0.0) + KILL_MARGIN.as_secs_f64() * 1000.0
            {
                break;
            }
        }
        if k.elapsed() > KILL_WAIT {
            println!("  Qdrant не восстановился за {} с", KILL_WAIT.as_secs());
            break;
        }
    }
    let restarts = h.status()?["qdrant"]["restarts"].as_u64().unwrap_or(0);
    let run = KillRun {
        before: stats(&before),
        down: stats(&down),
        after: stats(&after),
        first_after_kill_ms: first_after,
        to_degraded_ms: degraded_at,
        to_restored_ms: restored_at,
        first_restored_ms: first_restored,
        reasons,
        restarts,
        timeline,
    };
    if let Some(d) = &run.down {
        println!(
            "  во время простоя Qdrant: p50 {:.1} мс, p95 {:.1} мс, max {:.1} мс (n = {}); первый ответ {:.0} мс; восстановление {:.0} мс",
            d.p50,
            d.p95,
            d.max,
            d.n,
            run.first_after_kill_ms.unwrap_or(0.0),
            run.to_restored_ms.unwrap_or(f64::NAN)
        );
    }
    for (t, ms, l) in &run.timeline {
        println!("    +{t:.0} мс: {l} (ответ {ms:.0} мс)");
    }
    r.kill = Some(run);
    c.stop(s, "kill")
}

/// Этап `reindex`: `POST /admin/reindex` (из кэша) и ожидание итога.
fn phase_reindex(c: &Ctx, r: &mut BenchResult) -> Result<()> {
    println!("[reindex] пересборка Qdrant из embeddings");
    let (mut s, h, st) = c.start(r)?;
    r.startup.push(st);
    let prev = h.status()?["reindex"]["last"]["finished_at"].clone();
    let adm = &h.admin;
    // пересборка базы пользователя прогона (администратора): итог `reindex.last` виден
    // его токену, а итог reindex «всех пользователей» — только сессии администратора
    let mut me = h
        .agent
        .get(&format!("{}/api/v1/auth/me", s.base))
        .header("Cookie", &adm.cookie)
        .call()?;
    let me: Value = serde_json::from_str(&me.body_mut().read_to_string()?)?;
    let user = me["user_id"].as_str().context("в /auth/me нет user_id")?;
    let t0 = Instant::now();
    let mut resp = h
        .agent
        .post(&format!("{}/api/v1/admin/reindex", s.base))
        .header("Cookie", &adm.cookie)
        .header("X-CSRF-Token", &adm.csrf)
        .content_type("application/json")
        .send(json!({ "user": user }).to_string())?;
    if resp.status().as_u16() != 202 {
        bail!(
            "POST /admin/reindex: HTTP {}: {}",
            resp.status(),
            resp.body_mut().read_to_string()?
        );
    }
    let last = loop {
        s.ensure_alive()?;
        let st = h.status()?;
        let rx = &st["reindex"];
        if rx["running"] == false && rx["queued"] == 0 && rx["last"]["finished_at"] != prev {
            break rx["last"].clone();
        }
        if t0.elapsed() > INDEX_WAIT {
            bail!("reindex не завершился: {rx}");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let res = &last["result"];
    if res.is_null() {
        bail!("reindex завершился ошибкой: {last}");
    }
    let points = res["points"].as_u64().unwrap_or(0);
    let elapsed_ms = res["elapsed_ms"].as_f64().unwrap_or(wall_ms);
    let run = ReindexRun {
        points,
        misses: res["misses"].as_u64().unwrap_or(0),
        elapsed_ms,
        wall_ms,
        points_per_sec: points as f64 / (elapsed_ms / 1000.0).max(1e-9),
    };
    println!(
        "  {} точек за {:.0} мс: {:.0} точек/с (промахов кэша {})",
        run.points, run.elapsed_ms, run.points_per_sec, run.misses
    );
    r.reindex = Some(run);
    wait_idle(&mut s, &h, Instant::now(), INDEX_WAIT)?;
    c.stop(s, "reindex")
}

/// Этап `fts`: сервер без моделей на том же каталоге.
fn phase_fts(c: &Ctx, r: &mut BenchResult) -> Result<()> {
    println!("[fts] сервер без моделей на проиндексированном каталоге");
    let (s, h) = c.start_fts()?;
    warm_up(&h, false)?;
    let p = pass(&h, &c.queries, false)?;
    print_pass("FTS-only", &p);
    if p.degraded != p.client.as_ref().map_or(0, |s| s.n) {
        bail!("сервер без моделей ответил без деградации: векторный канал включён");
    }
    r.fts_only = Some(p);
    thread::sleep(IDLE);
    r.rss_fts_loaded = rss(&h)?;
    println!(
        "  RSS без моделей с данными: {}",
        report::mb(r.rss_fts_loaded)
    );
    c.stop(s, "fts")
}

/// Сведения о машине: ядра, ОЗУ, GPU (для таблицы, железо влияет на всё).
fn host() -> Value {
    let cores = thread::available_parallelism().map_or(0, |n| n.get());
    let gpu = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.used,memory.total,utilization.gpu",
            "--format=csv,noheader",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    json!({ "os": std::env::consts::OS, "logical_cpus": cores, "gpu": gpu })
}

/// Точка входа `cargo xtask bench`.
pub fn main(a: BenchArgs, repo: &Path) -> Result<()> {
    let mut phases = a.phases.clone();
    if phases.is_empty() {
        phases = vec![
            Phase::Rss,
            Phase::Index,
            Phase::Search,
            Phase::Rerank,
            Phase::Kill,
            Phase::Reindex,
            Phase::Fts,
        ];
    }
    phases.sort();
    phases.dedup();
    if a.no_copies_wait && phases != [Phase::Index] {
        bail!("--no-copies-wait — только с --phase index: каталог остаётся недоиндексированным");
    }
    if a.reuse && (phases.contains(&Phase::Rss) || phases.contains(&Phase::Index)) {
        bail!("--reuse: этапы rss и index пересоздают или дописывают каталог — уберите их");
    }
    let eval = repo.join("eval");
    let paths = Paths {
        repo: repo.to_path_buf(),
        corpus: eval.join("corpus"),
        queries: eval.join("queries.toml"),
        results: a.out.clone().unwrap_or_else(|| eval.join("results")),
        work: repo.join("target").join("bench"),
    };
    let corpus = Corpus::load(&paths.corpus)?;
    let queries: Vec<Query> = load_queries(&paths.queries)?;
    let device = a.profile.device();
    let model = a
        .model
        .clone()
        .unwrap_or_else(|| default_model(device).to_string());
    let name = match &a.label {
        Some(l) if !l.is_empty() => format!("{}-{l}", a.profile.as_str()),
        _ => a.profile.as_str().to_string(),
    };
    let spec = RunSpec {
        name,
        model: Some(model),
        device,
        reranker: a.reranker.clone(),
        sets: parse_sets(&a.sets)?,
        keep_cuda_path: a.keep_cuda_path,
    };
    let dir = paths.work.join(&spec.name);
    if a.reuse && !dir.join("data").is_dir() {
        bail!(
            "--reuse: нет {} (прогон с --keep)",
            dir.join("data").display()
        );
    }
    let server = build_server(repo, a.server_bin.as_deref())?;
    let c = Ctx {
        paths,
        server,
        spec,
        queries: queries.iter().map(|q| q.text.clone()).collect(),
    };
    let mut r = BenchResult {
        date: today(),
        git: git_rev(repo),
        profile: Some(a.profile),
        label: a.label.clone(),
        host: host(),
        sets: c.spec.sets.iter().cloned().collect(),
        notes: a.notes,
        queries: c.queries.len(),
        ..BenchResult::default()
    };
    if !a.reuse && dir.exists() {
        // новый прогон — с чистого каталога
        fs::remove_dir_all(&dir).with_context(|| format!("очистка {}", dir.display()))?;
    }
    let mut run = || -> Result<()> {
        for ph in &phases {
            match ph {
                Phase::Rss => phase_rss(&c, &mut r)?,
                Phase::Index => phase_index(&c, &corpus, &a, &mut r)?,
                Phase::Search => phase_search(&c, &mut r)?,
                Phase::Rerank => phase_rerank(&c, a.rerank_queries, &mut r)?,
                Phase::Kill => phase_kill(&c, &mut r)?,
                Phase::Reindex => phase_reindex(&c, &mut r)?,
                Phase::Fts => phase_fts(&c, &mut r)?,
            }
        }
        Ok(())
    };
    let res = run();
    // частичный итог пишется и при ошибке этапа (каталог данных тогда остаётся для разбора)
    let (json_file, md_file) = report::write(&c.paths.results, &r)?;
    println!("{}", report::markdown(&r));
    println!("  {}\n  {}", json_file.display(), md_file.display());
    res?;
    if !a.keep {
        fs::remove_dir_all(&dir).with_context(|| format!("удаление {}", dir.display()))?;
        println!(
            "каталог данных {} удалён (--keep — оставить)",
            dir.display()
        );
    }
    Ok(())
}
