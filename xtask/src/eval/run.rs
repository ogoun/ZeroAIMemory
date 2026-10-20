//! Прогон eval: сервер на временном каталоге, загрузка корпуса, ожидание индексации,
//! запросы и метрики.
//!
//! Сервер — `target/release/zam-server` (или `--server-bin`), запускается обвязкой smoke
//! ([`crate::smoke::Server`]): свободные порты сервера и Qdrant, `ZAM_*` разработчика убраны.
//! Модели берутся прямо из `target/models` (`ml.models_dir`), ONNX Runtime и Qdrant — из
//! `target/deps/<платформа>/` (`ml.ort_dylib`, `qdrant.binary`). Для CUDA из `PATH` сервера
//! убираются каталоги CUDA Toolkit: библиотеки NVIDIA берутся только из `<ORT>/cuda`, как
//! у пользователя после `zam-server cuda fetch`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use super::corpus::{Corpus, Kind, Lang, Query};
use super::metrics::{Aggregate, QueryMetrics, aggregate, quantile, score};
use super::report;
use super::{Device, EvalArgs, Mode, Paths};
use crate::smoke::{Server, SmokeEnv, api, api_timeout, wait_health_for};

/// Ожидание `/health` после старта (загрузка модели с HDD, sha256 файла модели).
const HEALTH_WAIT: Duration = Duration::from_secs(300);
/// Предел ожидания векторной индексации корпуса (fp32 на CPU — минуты).
const INDEX_WAIT: Duration = Duration::from_secs(3 * 3600);
/// Предел одного запроса поиска (реранк на CPU, первая загрузка реранкера).
const SEARCH_TIMEOUT: Duration = Duration::from_secs(300);
/// Глубина выдачи для метрик (MRR@10, nDCG@10).
pub const TOP_K: usize = 10;
/// Бюджет ответа: без усечения выдачи по токенам.
const BUDGET_TOKENS: usize = 1_000_000;

/// Что запускать: модель (None — FTS-only без моделей), устройство, реранкер, конфигурация.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// Каталог прогона `target/eval/<name>`.
    pub name: String,
    pub model: Option<String>,
    pub device: Device,
    pub reranker: Option<String>,
    /// Переопределения конфигурации (`search.w_fts`, `index.chunk_target_max`, …).
    pub sets: Vec<(String, String)>,
    pub keep_cuda_path: bool,
}

/// `KEY=VALUE` → (`search.w_fts`, `0.8`).
pub fn parse_sets(raw: &[String]) -> Result<Vec<(String, String)>> {
    raw.iter()
        .map(|s| {
            let (k, v) = s
                .split_once('=')
                .with_context(|| format!("--set {s}: ожидается KEY=VALUE"))?;
            let k = k.trim();
            if k.is_empty() || !k.contains('.') {
                bail!("--set {s}: ключ вида <раздел>.<ключ>");
            }
            Ok((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// `search.w_fts` → `ZAM_SEARCH__W_FTS`.
fn env_key(k: &str) -> String {
    format!("ZAM_{}", k.replace('.', "__").to_ascii_uppercase())
}

/// Платформа сборки xtask.
fn platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

/// Библиотека ONNX Runtime из `target/deps`: CPU-сборка или с CUDA EP.
fn ort_dylib(repo: &Path, device: Device) -> PathBuf {
    let flavor = match device {
        Device::Cpu => "onnxruntime",
        Device::Cuda => "onnxruntime-gpu",
    };
    let name = if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };
    repo.join("target")
        .join("deps")
        .join(platform())
        .join(flavor)
        .join(name)
}

fn qdrant_bin(repo: &Path) -> PathBuf {
    repo.join("target")
        .join("deps")
        .join(platform())
        .join("qdrant")
        .join(crate::package::exe(platform(), "qdrant"))
}

/// `PATH` без каталогов CUDA Toolkit (в них есть cudart/cublas/cudnn).
fn path_without_cuda() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|p| {
            let s = p.to_string_lossy().to_ascii_lowercase();
            !(s.contains("cuda") || s.contains("cudnn") || s.contains("nvidia gpu computing"))
        })
        .collect();
    std::env::join_paths(kept)
        .ok()
        .map(|s| s.to_string_lossy().into_owned())
}

/// Собирает `zam-server` в release (или берёт `--server-bin`); возвращает его каталог.
pub fn server_dir(a: &EvalArgs, p: &Paths) -> Result<PathBuf> {
    build_server(&p.repo, a.server_bin.as_deref())
}

/// `server_bin` или собранный `cargo build --release -p zam-server`; возвращает каталог
/// исполняемого файла (общий для `eval` и `bench`).
pub fn build_server(repo: &Path, server_bin: Option<&Path>) -> Result<PathBuf> {
    let exe = match server_bin {
        Some(b) => b.to_path_buf(),
        None => {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            println!("сборка zam-server (cargo build --release -p zam-server)…");
            let st = Command::new(cargo)
                .args(["build", "--release", "-p", "zam-server"])
                .current_dir(repo)
                .status()
                .context("запуск cargo build")?;
            if !st.success() {
                bail!("cargo build --release -p zam-server: {st}");
            }
            repo.join("target")
                .join("release")
                .join(crate::package::exe(platform(), "zam-server"))
        }
    };
    if !exe.is_file() {
        bail!("нет {}", exe.display());
    }
    // Server::start берёт zam-server из каталога комплекта: имя файла — стандартное
    if exe.file_name().and_then(|n| n.to_str())
        != Some(&crate::package::exe(platform(), "zam-server"))
    {
        bail!("--server-bin: ожидается файл zam-server[.exe]");
    }
    Ok(exe.parent().context("каталог zam-server")?.to_path_buf())
}

/// Запущенный сервер прогона.
pub struct Session {
    srv: Server,
    pub base: String,
    token: String,
    pub data: PathBuf,
    pub spec: RunSpec,
    /// `/index/status` после индексации.
    pub status: Value,
    pub index_secs: f64,
    /// Действующие `[search]` и `[index]` сервера (из его журнала при старте).
    pub server_config: Value,
    /// Начало загрузки корпуса: от него считается время индексации.
    loaded_at: Option<Instant>,
}

/// Сообщение журнала сервера с действующими `[search]` и `[index]`.
const CONFIG_LOG_MESSAGE: &str = "параметры поиска и индексации";

/// Действующие `[search]` и `[index]` из журнала сервера (JSON-строки поля `fields`).
fn server_config(log: &Path) -> Result<Value> {
    let t0 = Instant::now();
    loop {
        let text = fs::read_to_string(log).unwrap_or_default();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let f = &v["fields"];
            if f["message"] == CONFIG_LOG_MESSAGE {
                let parse = |k: &str| {
                    f[k].as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(Value::Null)
                };
                return Ok(json!({ "search": parse("search"), "index": parse("index") }));
            }
        }
        if t0.elapsed() > Duration::from_secs(10) {
            bail!(
                "в журнале {} нет строки «{CONFIG_LOG_MESSAGE}»",
                log.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Окружение сервера прогона.
fn server_env(p: &Paths, spec: &RunSpec, dir: &Path) -> Result<SmokeEnv> {
    let mut env = SmokeEnv::for_model(spec.model.as_deref().unwrap_or("none"))?;
    let models = match &spec.model {
        Some(_) => p.repo.join("target").join("models"),
        None => {
            // FTS-only: пустой каталог моделей — как свежая установка без models fetch
            let d = dir.join("no-models");
            fs::create_dir_all(&d)?;
            d
        }
    };
    let ort = ort_dylib(&p.repo, spec.device);
    if spec.model.is_some() && !ort.is_file() {
        bail!(
            "нет {} (cargo xtask fetch-deps --platform {})",
            ort.display(),
            platform()
        );
    }
    let qdrant = qdrant_bin(&p.repo);
    if !qdrant.is_file() {
        bail!(
            "нет {} (cargo xtask fetch-deps --platform {})",
            qdrant.display(),
            platform()
        );
    }
    let mut vars: Vec<(String, String)> = vec![
        ("ZAM_ML__MODELS_DIR".into(), models.to_string_lossy().into()),
        ("ZAM_ML__DEVICE".into(), spec.device.as_str().into()),
        ("ZAM_ML__ORT_DYLIB".into(), ort.to_string_lossy().into()),
        ("ZAM_QDRANT__BINARY".into(), qdrant.to_string_lossy().into()),
    ];
    if let Some(r) = &spec.reranker {
        vars.push(("ZAM_ML__RERANK_MODEL_CPU".into(), r.clone()));
        vars.push(("ZAM_ML__RERANK_MODEL_GPU".into(), r.clone()));
    }
    for (k, v) in &spec.sets {
        vars.push((env_key(k), v.clone()));
    }
    if spec.device == Device::Cuda
        && !spec.keep_cuda_path
        && let Some(path) = path_without_cuda()
    {
        vars.push(("PATH".into(), path));
    }
    // поздние значения переопределяют ранние (модель из SmokeEnv)
    for (k, v) in vars {
        env.vars.retain(|(ek, _)| *ek != k);
        env.vars.push((k, v));
    }
    Ok(env)
}

/// Файл токена прогона: первый токен печатается только при создании администратора,
/// повторные запуски на том же каталоге (сетка весов) берут его отсюда.
const TOKEN_FILE: &str = "token.txt";

impl Session {
    /// Запускает сервер на `target/eval/<name>/data`; `fresh` — каталог очищается.
    pub fn start(p: &Paths, spec: &RunSpec, server: &Path, fresh: bool) -> Result<Session> {
        let dir = p.work.join(&spec.name);
        if fresh && dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("очистка {}", dir.display()))?;
        }
        fs::create_dir_all(&dir)?;
        let data = dir.join("data");
        let env = server_env(p, spec, &dir)?;
        let base = format!("http://{}", env.addr);
        let log = dir.join("zam-server.stderr.log");
        let mut srv = Server::start(server, &data, &env, &log)?;
        wait_health_for(&mut srv, &base, HEALTH_WAIT)?;
        let server_config = server_config(&log)?;
        let token_file = dir.join(TOKEN_FILE);
        // повторный старт на том же каталоге: токен уже выдан, stderr его не напечатает —
        // без ожидания (иначе 5 с к каждому перезапуску; bench меряет время старта)
        let saved = (!fresh)
            .then(|| fs::read_to_string(&token_file).ok())
            .flatten()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let token = match saved {
            Some(t) => t,
            None => match srv.token_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(t) => {
                    fs::write(&token_file, &t)?;
                    t
                }
                Err(_) => fs::read_to_string(&token_file)
                    .map(|t| t.trim().to_string())
                    .map_err(|_| {
                        anyhow!(
                            "нет токена: ни в stderr сервера, ни в {}; stderr:\n{}",
                            token_file.display(),
                            srv.log_tail()
                        )
                    })?,
            },
        };
        Ok(Session {
            srv,
            base,
            token,
            data,
            spec: spec.clone(),
            status: Value::Null,
            index_secs: 0.0,
            server_config,
            loaded_at: None,
        })
    }

    fn api(&self, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        api(&self.base, &self.token, path, body)
    }

    /// Токен прогона (для своих клиентов `bench`).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// pid процесса `zam-server`.
    pub fn pid(&self) -> u32 {
        self.srv.child.id()
    }

    /// Сервер ещё работает; завершился — ошибка с хвостом журнала.
    pub fn ensure_alive(&mut self) -> Result<()> {
        if let Some(st) = self.srv.child.try_wait()? {
            bail!(
                "сервер завершился ({st}); stderr:
{}",
                self.srv.log_tail()
            );
        }
        Ok(())
    }

    /// Отметка начала загрузки: от неё [`Session::wait_indexed`] считает время индексации
    /// (для загрузки своими средствами, без [`Session::load_corpus`]).
    pub fn begin_load(&mut self) {
        self.loaded_at = Some(Instant::now());
    }

    /// Пишет заметки корпуса в Knowledge (`POST /nodes`).
    pub fn load_corpus(&mut self, corpus: &Corpus) -> Result<()> {
        self.begin_load();
        for n in corpus.notes.values() {
            let body = json!({
                "space": "knowledge",
                "path": n.path,
                "kind": "note",
                "content": n.text,
            });
            let (st, v) = self.api("/api/v1/nodes", Some(&body))?;
            if st != 201 {
                bail!("POST /nodes {}: HTTP {st}: {v}", n.path);
            }
        }
        Ok(())
    }

    /// `/index/status`.
    pub fn index_status(&self) -> Result<Value> {
        let (st, v) = self.api("/api/v1/index/status", None)?;
        if st != 200 {
            bail!("GET /index/status: HTTP {st}");
        }
        Ok(v)
    }

    /// Ждёт векторной индексации: очередь пуста, векторный канал доступен, проверочный
    /// поиск без деградации и `pending_count = 0`. Неудавшиеся задания — ошибка.
    pub fn wait_indexed(&mut self) -> Result<()> {
        // время индексации — от начала загрузки корпуса (запись и очередь идут параллельно)
        let t0 = self.loaded_at.unwrap_or_else(Instant::now);
        let probe = json!({ "query": "eval", "mode": "notes", "top_k": 1 });
        let mut last_print = Instant::now();
        loop {
            if let Some(st) = self.srv.child.try_wait()? {
                bail!("сервер завершился ({st}); stderr:\n{}", self.srv.log_tail());
            }
            let s = self.index_status()?;
            let q = &s["queue"];
            if q["failed"].as_u64().unwrap_or(0) > 0 {
                bail!(
                    "задания индексации завершились ошибкой: {}",
                    s["failed_items"]
                );
            }
            let idle = q["queued"] == 0 && q["running"] == 0;
            if idle && s["vector"]["available"] == true {
                let (code, v) = self.api("/api/v1/search", Some(&probe))?;
                if code == 200 && v["degraded"] == false && v["pending_count"] == 0 {
                    self.index_secs = t0.elapsed().as_secs_f64();
                    self.status = s;
                    return Ok(());
                }
            }
            if t0.elapsed() > INDEX_WAIT {
                bail!(
                    "индексация не завершилась за {} с: vector = {}, queue = {}",
                    INDEX_WAIT.as_secs(),
                    s["vector"],
                    q
                );
            }
            if last_print.elapsed() > Duration::from_secs(30) {
                println!(
                    "  индексация: в очереди {}, выполняется {} ({:.0} с)",
                    q["queued"],
                    q["running"],
                    t0.elapsed().as_secs_f64()
                );
                last_print = Instant::now();
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    /// Проверяет модель и устройство по `/index/status` (только с моделью).
    pub fn check_model(&mut self) -> Result<()> {
        let s = self.index_status()?;
        if let Some(m) = &self.spec.model {
            let id = s["model"]["id"].as_str().unwrap_or_default();
            if !id.starts_with(&format!("{m}@")) {
                bail!("/index/status: модель {id}, ожидалась {m} ({})", s["model"]);
            }
            let kind = s["device"]["kind"].as_str().unwrap_or_default();
            if kind != self.spec.device.as_str() {
                bail!(
                    "устройство {kind}, ожидалось {} (причина: {})",
                    self.spec.device.as_str(),
                    s["device"]["reason"]
                );
            }
        }
        self.status = s;
        Ok(())
    }

    /// Выполняет запросы в режиме `mode`.
    pub fn run_queries(&self, queries: &[Query], mode: Mode) -> Result<Vec<QueryRun>> {
        let mut out = Vec::with_capacity(queries.len());
        for q in queries {
            let body = json!({
                "query": q.text,
                "mode": "notes",
                "top_k": TOP_K,
                "budget_tokens": BUDGET_TOKENS,
                "scope": { "spaces": ["knowledge"] },
                "rerank": if mode == Mode::HybridRerank { "on" } else { "off" },
            });
            let t0 = Instant::now();
            let (st, v) = api_timeout(
                &self.base,
                &self.token,
                "/api/v1/search",
                Some(&body),
                SEARCH_TIMEOUT,
            )?;
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if st != 200 {
                bail!("запрос {}: HTTP {st}: {v}", q.id);
            }
            let degraded = v["degraded"] == true;
            if mode != Mode::Fts && degraded {
                bail!(
                    "запрос {}: поиск деградирован ({}) в режиме {}",
                    q.id,
                    v["degraded_reason"],
                    mode.as_str()
                );
            }
            if mode == Mode::Fts && !degraded {
                bail!("режим fts, но векторный канал включён (degraded = false)");
            }
            let warnings: Vec<String> = v["warnings"]
                .as_array()
                .map(|w| {
                    w.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if mode == Mode::HybridRerank && !warnings.is_empty() {
                bail!("запрос {}: реранк не выполнен: {warnings:?}", q.id);
            }
            let items: Vec<Hit> = v["items"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|i| Hit {
                            path: i["path"].as_str().unwrap_or_default().to_string(),
                            score: i["score"].as_f64().unwrap_or(0.0),
                            sources: i["sources"]
                                .as_array()
                                .map(|s| {
                                    s.iter()
                                        .filter_map(|x| x.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            reranked: !i["rerank_score"].is_null(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let ranked: Vec<String> = items.iter().map(|h| h.path.clone()).collect();
            out.push(QueryRun {
                id: q.id.clone(),
                kind: q.kind,
                lang: q.lang,
                disjoint: q.disjoint,
                expect: q.expect.clone(),
                metrics: score(&ranked, &q.expect),
                hits: items,
                ms,
                server_ms: v["timings"].clone(),
            });
        }
        Ok(out)
    }

    /// Останавливает сервер (Qdrant завершается вместе с ним).
    pub fn stop(mut self) -> Result<()> {
        self.srv.stop()
    }
}

/// Элемент выдачи.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub path: String,
    pub score: f64,
    pub sources: Vec<String>,
    pub reranked: bool,
}

/// Итог одного запроса.
#[derive(Debug, Clone, Serialize)]
pub struct QueryRun {
    pub id: String,
    pub kind: Kind,
    pub lang: Lang,
    pub disjoint: bool,
    pub expect: Vec<String>,
    pub metrics: QueryMetrics,
    pub hits: Vec<Hit>,
    /// Время ответа на клиенте, мс.
    pub ms: f64,
    /// `timings` ответа сервера.
    pub server_ms: Value,
}

/// Метрики по группам запросов.
#[derive(Debug, Clone, Serialize)]
pub struct Breakdown {
    pub overall: Aggregate,
    pub by_kind: BTreeMap<String, Aggregate>,
    pub by_lang: BTreeMap<String, Aggregate>,
    /// Перефразы без общих слов.
    pub disjoint: Aggregate,
    /// Доля найденных в top-10 ожидаемых заметок по каналам.
    pub channels: BTreeMap<String, f64>,
}

pub fn breakdown(runs: &[QueryRun]) -> Breakdown {
    let agg = |f: &dyn Fn(&QueryRun) -> bool| {
        aggregate(
            &runs
                .iter()
                .filter(|r| f(r))
                .map(|r| r.metrics)
                .collect::<Vec<_>>(),
        )
    };
    let mut by_kind = BTreeMap::new();
    for k in Kind::ALL {
        if runs.iter().any(|r| r.kind == k) {
            by_kind.insert(k.as_str().to_string(), agg(&|r| r.kind == k));
        }
    }
    let mut by_lang = BTreeMap::new();
    for (l, name) in [(Lang::Ru, "ru"), (Lang::En, "en")] {
        if runs.iter().any(|r| r.lang == l) {
            by_lang.insert(name.to_string(), agg(&|r| r.lang == l));
        }
    }
    // каналы, которыми найдены ожидаемые заметки в top-10
    let mut found = 0usize;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in runs {
        for h in r
            .hits
            .iter()
            .take(TOP_K)
            .filter(|h| r.expect.contains(&h.path))
        {
            found += 1;
            for s in &h.sources {
                *counts.entry(s.clone()).or_default() += 1;
            }
            if h.sources == ["vector"] {
                *counts.entry("vector_only".into()).or_default() += 1;
            }
        }
    }
    let channels = counts
        .into_iter()
        .map(|(k, n)| {
            (
                k,
                if found == 0 {
                    0.0
                } else {
                    n as f64 / found as f64
                },
            )
        })
        .collect();
    Breakdown {
        overall: agg(&|_| true),
        by_kind,
        by_lang,
        disjoint: agg(&|r| r.disjoint),
        channels,
    }
}

/// Сведения о прогоне для отчёта.
#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub date: String,
    pub git: String,
    pub label: Option<String>,
    pub mode: Mode,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub device: Option<String>,
    pub device_reason: Option<String>,
    pub reranker: Option<String>,
    pub sets: BTreeMap<String, String>,
    /// Действующие `[search]` и `[index]` сервера (итог слоёв конфигурации).
    pub server_config: Value,
    pub notes: usize,
    pub corpus_fingerprint: String,
    pub queries: usize,
    /// Отпечаток запросов (FNV-1a по TOML запросов): результаты на разных наборах не сравнимы.
    pub queries_fingerprint: String,
    pub index_secs: f64,
    pub query_ms_p50: f64,
    pub query_ms_p95: f64,
}

/// Результат прогона режима.
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub info: RunInfo,
    pub metrics: Breakdown,
    pub queries: Vec<QueryRun>,
}

/// Короткий хэш HEAD и признак незакоммиченных изменений.
pub fn git_rev(repo: &Path) -> String {
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let sha = run(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "?".into());
    let dirty = run(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    if !dirty {
        return sha;
    }
    match dirty_hash(repo) {
        Some(h) => format!("{sha}+dirty.{h}"),
        None => format!("{sha}+dirty"),
    }
}

/// Отпечаток незакоммиченных изменений: `git hash-object` от `git diff HEAD --binary` и
/// хэшей неотслеживаемых файлов (не из `.gitignore`) — одинаковое дерево даёт одинаковый
/// отпечаток, по нему прогоны одного «грязного» кода сопоставимы. 8 hex-символов.
fn dirty_hash(repo: &Path) -> Option<String> {
    use std::io::Write as _;
    let out = |args: &[&str]| -> Option<Vec<u8>> {
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let mut buf = out(&["diff", "HEAD", "--binary"])?;
    let untracked =
        String::from_utf8(out(&["ls-files", "--others", "--exclude-standard"])?).ok()?;
    let files: Vec<&str> = untracked.lines().filter(|l| !l.is_empty()).collect();
    if !files.is_empty() {
        let mut args = vec!["hash-object", "--"];
        args.extend(files.iter().copied());
        let hashes = out(&args)?;
        for (f, h) in files.iter().zip(String::from_utf8_lossy(&hashes).lines()) {
            buf.extend_from_slice(
                format!(
                    "
{f} {h}"
                )
                .as_bytes(),
            );
        }
    }
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&buf).ok()?;
    let o = child.wait_with_output().ok()?;
    let h = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (h.len() >= 8).then(|| h[..8].to_string())
}

/// Сегодняшняя дата `YYYY-MM-DD` (местное время).
pub fn today() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d").to_string()
}

/// Модель и устройство по умолчанию для `--device`.
pub fn default_model(device: Device) -> &'static str {
    match device {
        Device::Cpu => "bge-m3-int8",
        Device::Cuda => "bge-m3-fp16",
    }
}

/// Реранкер, который выберет сервер (`ml.rerank_model_cpu|gpu` по умолчанию).
fn effective_reranker(spec: &RunSpec, status: &Value) -> String {
    if let Some(r) = &spec.reranker {
        return r.clone();
    }
    match status["device"]["kind"].as_str() {
        Some("cuda") => "bge-reranker-v2-m3".into(),
        _ => "jina-reranker-v2".into(),
    }
}

/// Собирает результат режима.
pub fn result(
    s: &Session,
    p: &Paths,
    corpus: &Corpus,
    queries: &[Query],
    mode: Mode,
    label: Option<&str>,
    runs: Vec<QueryRun>,
) -> RunResult {
    let ms: Vec<f64> = runs.iter().map(|r| r.ms).collect();
    let st = &s.status;
    let with_model = s.spec.model.is_some();
    RunResult {
        info: RunInfo {
            date: today(),
            git: git_rev(&p.repo),
            label: label.map(String::from),
            mode,
            model: s.spec.model.clone(),
            model_id: with_model.then(|| st["model"]["id"].as_str().unwrap_or_default().into()),
            device: with_model.then(|| st["device"]["kind"].as_str().unwrap_or_default().into()),
            device_reason: with_model
                .then(|| st["device"]["reason"].as_str().unwrap_or_default().into()),
            reranker: (mode == Mode::HybridRerank).then(|| effective_reranker(&s.spec, st)),
            sets: s.spec.sets.iter().cloned().collect(),
            server_config: s.server_config.clone(),
            notes: corpus.notes.len(),
            corpus_fingerprint: corpus.fingerprint(),
            queries: queries.len(),
            queries_fingerprint: queries_fingerprint(queries),
            index_secs: s.index_secs,
            query_ms_p50: quantile(&ms, 0.5),
            query_ms_p95: quantile(&ms, 0.95),
        },
        metrics: breakdown(&runs),
        queries: runs,
    }
}

/// Имя прогона/файла: `<модель|fts>[-<метка>]`.
pub fn run_name(model: Option<&str>, label: Option<&str>) -> String {
    let base = model.unwrap_or("fts").to_string();
    match label {
        Some(l) if !l.is_empty() => format!("{base}-{l}"),
        _ => base,
    }
}

/// Один сервер и один индекс; запросы — в каждом режиме из `--mode`.
pub fn run_modes(a: &EvalArgs, p: &Paths, corpus: &Corpus, queries: &[Query]) -> Result<()> {
    let server = server_dir(a, p)?;
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    if a.modes.contains(&Mode::Fts) && !only_fts {
        bail!("fts запускается отдельно: сервер без моделей (--mode fts)");
    }
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| default_model(a.device).to_string())
    });
    let mut label = a.label.clone();
    if let Some(r) = &a.reranker {
        label = Some(match label {
            Some(l) => format!("{l}-{r}"),
            None => r.clone(),
        });
    }
    let spec = RunSpec {
        name: run_name(model.as_deref(), label.as_deref()),
        model,
        device: a.device,
        reranker: a.reranker.clone(),
        sets: parse_sets(&a.sets)?,
        keep_cuda_path: a.keep_cuda_path,
    };
    let mut s = Session::start(p, &spec, &server, !a.reuse)?;
    s.check_model()?;
    println!(
        "сервер {}: модель {}, устройство {}",
        s.base, s.status["model"]["id"], s.status["device"]
    );
    if !a.reuse {
        let t0 = Instant::now();
        s.load_corpus(corpus)?;
        println!(
            "корпус загружен: {} заметок за {:.1} с",
            corpus.notes.len(),
            t0.elapsed().as_secs_f64()
        );
    }
    if spec.model.is_some() {
        s.wait_indexed()?;
        println!(
            "индексация: {:.1} с, точек в Qdrant {}",
            s.index_secs, s.status["qdrant"]["points"]
        );
    }
    for &mode in &a.modes {
        let runs = s.run_queries(queries, mode)?;
        let r = result(&s, p, corpus, queries, mode, label.as_deref(), runs);
        let (json, md) = report::write_run(&p.results, &r)?;
        report::print_run(&r);
        println!("  {}\n  {}", json.display(), md.display());
    }
    s.stop()
}

/// Отпечаток набора запросов: FNV-1a по id, тексту, ожидаемым заметкам и виду.
pub fn queries_fingerprint(queries: &[Query]) -> String {
    let text: String = queries
        .iter()
        .map(|q| format!("{}\0{}\0{}\0{}\0", q.id, q.text, q.expect.join(","), q.kind))
        .collect();
    super::corpus::fnv1a(text.into_bytes())
}
