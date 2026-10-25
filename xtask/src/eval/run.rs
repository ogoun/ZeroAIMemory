//! Прогон eval: сервер на временном каталоге, загрузка корпуса, ожидание индексации,
//! запросы и метрики.
//!
//! Сервер — `target/release/zam-server` (или `--server-bin`), запускается обвязкой smoke
//! ([`crate::smoke::Server`]): свободные порты сервера и Qdrant, `ZAM_*` разработчика убраны.
//! Модели берутся прямо из `target/models` (`ml.models_dir`), ONNX Runtime и Qdrant — из
//! `target/deps/<платформа>/` (`ml.ort_dylib`, `qdrant.binary`). Для CUDA из `PATH` сервера
//! убираются каталоги CUDA Toolkit: библиотеки NVIDIA берутся только из `<ORT>/cuda`, как
//! у пользователя после `zam-server cuda fetch`.
//!
//! **Набор M3.** Загрузчик регистрирует решение набора ([`EVAL_SOLUTION`]): заметки группы
//! `e` (`<язык>/zam/…`) пишутся в его пространство, остальные — в Knowledge; insight
//! `eval/insights.toml` создаются `POST /spaces/{решение}/entries` (`kind: insight`,
//! `force: true` — дедупликация набора не нужна), замещаются `POST /entries/{id}/supersede` и
//! отзываются `POST /entries/{id}/retract`. `axiom` создаёт человеческий токен, выпущенный
//! администратором прогона (`POST /admin/users/{id}/tokens {human: true}`), остальное —
//! токен прогона (агент; `theorem` — с `data.evidence`). Соответствие id записей и ключей
//! набора сохраняется в каталоге прогона (`entries.json`) — для перезапусков на том же
//! индексе. Поиск — `scope.solution` набора с Knowledge, без Rules.
//!
//! Метрики M3 по видам запросов:
//! - `superseded` — `cancelled@5` в обычном поиске и контроль с `include_history: true`
//!   (там замещённые видны — иначе набор устаревания ничего не проверяет);
//! - `certainty` — `reliable_first` (с бустами `certainty` сервера; без бустов — второй
//!   проход в [`run_modes`] на том же индексе с `search.boost_* = 1`);
//! - `chunk` — второй запрос `mode = chunks` (`expand = none`): листы выдачи сравниваются с
//!   `evidence` по **собственному** тексту листа — перекрытие с предыдущим листом
//!   (`chunk_overlap_sentences`) отрезается ([`leaf_cores`]): лист, получивший доказательство
//!   только благодаря перекрытию, не засчитывается **[решение исполнителя]** (исследование
//!   нарезки §5: «без учёта перекрытия»).

mod load;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use super::compare::{Comparison, compare};
use super::corpus::{
    Corpus, EVAL_SOLUTION, INSIGHT_REF, Kind, Lang, Note, Query, in_solution, is_long_path,
};
use super::leaves::{LeafHit, SplitStats, chunk_leaves, leaf_cores, split_stats};
use super::metrics::{
    Aggregate, Estimate, QueryMetrics, aggregate, cancelled_at_5, chunk_scores, estimate, quantile,
    reliable_first, score,
};
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

/// Бинарник Qdrant из `target/deps/<платформа>/qdrant/` (также `bench --edge`).
pub fn qdrant_bin(repo: &Path) -> PathBuf {
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
    /// id записи журнала → ключ insight набора.
    entry_keys: BTreeMap<String, String>,
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
/// Соответствие id записей и ключей insight набора (для перезапусков на том же каталоге).
const ENTRIES_FILE: &str = "entries.json";
/// Бусты `certainty` выключены — второй проход `reliable_first`.
pub const NO_CERTAINTY_BOOSTS: [(&str, &str); 3] = [
    ("search.boost_axiom", "1.0"),
    ("search.boost_theorem", "1.0"),
    ("search.boost_hypothesis", "1.0"),
];

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
        let entry_keys = if fresh {
            BTreeMap::new()
        } else {
            fs::read_to_string(dir.join(ENTRIES_FILE))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
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
            entry_keys,
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

    /// Регистрирует решение набора, пишет заметки (`POST /nodes`: группа `e` — в решение,
    /// остальные — в Knowledge) и insight набора (см. модуль).
    pub fn load_corpus(&mut self, corpus: &Corpus) -> Result<()> {
        self.begin_load();
        let reg = json!({ "agent_solution_id": EVAL_SOLUTION, "name": "eval" });
        let (st, v) = self.api("/api/v1/solutions", Some(&reg))?;
        if st != 201 && st != 200 {
            bail!("POST /solutions: HTTP {st}: {v}");
        }
        for n in corpus.notes.values() {
            let body = json!({
                "space": note_space(&n.path),
                "path": n.path,
                "kind": "note",
                "content": n.text,
            });
            let (st, v) = self.api("/api/v1/nodes", Some(&body))?;
            if st != 201 {
                bail!("POST /nodes {}: HTTP {st}: {v}", n.path);
            }
        }
        self.load_insights(&corpus.insights)
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

    /// Тело `POST /search` запроса набора: `out` — `notes` | `chunks`, `history` —
    /// `include_history`.
    fn search_body(q: &Query, mode: Mode, out: &str, history: bool) -> Value {
        json!({
            "query": q.text,
            "mode": out,
            "top_k": TOP_K,
            "budget_tokens": BUDGET_TOKENS,
            "scope": { "solution": EVAL_SOLUTION, "include_rules": false },
            "include_history": history,
            "rerank": if mode == Mode::HybridRerank { "on" } else { "off" },
        })
    }

    /// `POST /search` с проверками режима (деградация, реранк); ответ и время на клиенте, мс.
    fn search(&self, q: &Query, mode: Mode, body: &Value) -> Result<(Value, f64)> {
        let t0 = Instant::now();
        let (st, v) = api_timeout(
            &self.base,
            &self.token,
            "/api/v1/search",
            Some(body),
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
        Ok((v, ms))
    }

    /// Элементы выдачи: путь заметки или `insight:<key>` записи набора (чужая запись —
    /// `entry:<id>`).
    fn hits(&self, v: &Value) -> Vec<Hit> {
        v["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| Hit {
                        path: match i["entry_id"].as_str() {
                            Some(id) => match self.entry_keys.get(id) {
                                Some(k) => format!("{INSIGHT_REF}{k}"),
                                None => format!("entry:{id}"),
                            },
                            None => i["path"].as_str().unwrap_or_default().to_string(),
                        },
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
            .unwrap_or_default()
    }

    /// Выполняет запросы в режиме `mode`: основной поиск `mode = notes` и, по виду запроса,
    /// контроль `include_history` (`superseded`) или поиск листов `mode = chunks` (`chunk`).
    pub fn run_queries(&self, queries: &[Query], mode: Mode) -> Result<Vec<QueryRun>> {
        let leaves = if queries.iter().any(|q| q.kind == Kind::Chunk) {
            leaf_cores(&self.data)?
        } else {
            HashMap::new()
        };
        let mut out = Vec::with_capacity(queries.len());
        for q in queries {
            let (v, ms) = self.search(q, mode, &Self::search_body(q, mode, "notes", false))?;
            let items = self.hits(&v);
            let ranked: Vec<String> = items.iter().map(|h| h.path.clone()).collect();
            let mut metrics = score(&ranked, &q.relevant());
            let mut cancelled5_history = None;
            let mut leaf_hits = Vec::new();
            match q.kind {
                Kind::Superseded => {
                    metrics.cancelled5 = Some(cancelled_at_5(&ranked, &q.forbid));
                    let body = Self::search_body(q, mode, "notes", true);
                    let (h, _) = self.search(q, mode, &body)?;
                    let hr: Vec<String> = self.hits(&h).into_iter().map(|h| h.path).collect();
                    cancelled5_history = Some(cancelled_at_5(&hr, &q.forbid));
                }
                Kind::Certainty => {
                    metrics.reliable_first = Some(reliable_first(&ranked, &q.expect, &q.weaker));
                }
                Kind::Chunk => {
                    let ev = q.evidence.as_deref().unwrap_or_default();
                    let body = Self::search_body(q, mode, "chunks", false);
                    let (c, _) = self.search(q, mode, &body)?;
                    let (hits, cores) = chunk_leaves(&c, &leaves, ev);
                    let (r5, n10) = chunk_scores(&cores, ev);
                    metrics.chunk_recall5 = Some(r5);
                    metrics.chunk_ndcg10 = Some(n10);
                    leaf_hits = hits;
                }
                _ => {}
            }
            out.push(QueryRun {
                id: q.id.clone(),
                text: q.text.clone(),
                kind: q.kind,
                lang: q.lang,
                disjoint: q.disjoint,
                expect: q.expect.clone(),
                also: q.also.clone(),
                forbid: q.forbid.clone(),
                weaker: q.weaker.clone(),
                long: false,
                fts_only: false,
                metrics,
                cancelled5_history,
                reliable_first_no_boost: None,
                leaves: leaf_hits,
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

    /// Штатная остановка (Ctrl+Break / SIGTERM) не дольше `limit`; время до выхода.
    pub fn stop_graceful(mut self, limit: Duration) -> Result<Duration> {
        self.srv.stop_graceful(limit)
    }

    /// Жёсткое завершение сервера и вместе с ним Qdrant.
    pub fn kill(mut self) -> Result<()> {
        self.srv.kill()
    }
}

/// Пространство заметки при загрузке: группа `e` — решение набора, остальное — Knowledge.
pub fn note_space(path: &str) -> &'static str {
    if in_solution(path) {
        EVAL_SOLUTION
    } else {
        "knowledge"
    }
}

/// Элемент выдачи: путь заметки или `insight:<key>` записи набора.
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
    /// Текст запроса (для `--compare` будущих прогонов).
    #[serde(default)]
    pub text: String,
    pub kind: Kind,
    pub lang: Lang,
    pub disjoint: bool,
    pub expect: Vec<String>,
    /// Дополнительно релевантные цели (`also`): в метриках прогона, не в `--compare`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forbid: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub weaker: Vec<String>,
    /// Ожидается длинная заметка (подмножество «длинные заметки»).
    pub long: bool,
    /// Ожидается заметка с `index: false` (находит только FTS).
    pub fts_only: bool,
    pub metrics: QueryMetrics,
    /// `superseded`: `cancelled@5` в поиске с `include_history: true` (контроль).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled5_history: Option<bool>,
    /// `certainty`: `reliable_first` без бустов `certainty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reliable_first_no_boost: Option<bool>,
    /// `chunk`: первые 10 листов `mode = chunks`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub leaves: Vec<LeafHit>,
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
    /// Запросы к длинным заметкам.
    pub long: Aggregate,
    /// Запросы к заметкам с `index: false` (FTS-only находки).
    pub fts_only: Aggregate,
    /// Контроль устаревания: `cancelled@5` с `include_history: true`.
    pub cancelled5_history: Option<Estimate>,
    /// `reliable_first` без бустов `certainty`.
    pub reliable_first_no_boost: Option<Estimate>,
    /// `chunk-recall@5`, если засчитывать и перекрытие (справочно).
    pub chunk_recall5_with_overlap: Option<Estimate>,
    /// Доля найденных в top-10 ожидаемых заметок по каналам.
    pub channels: BTreeMap<String, f64>,
}

/// Среднее с интервалом по флагам запросов (`None` — флага нет ни у одного).
fn flag_estimate(runs: &[QueryRun], f: fn(&QueryRun) -> Option<bool>) -> Option<Estimate> {
    let v: Vec<f64> = runs
        .iter()
        .filter_map(f)
        .map(|b| if b { 1.0 } else { 0.0 })
        .collect();
    (!v.is_empty()).then(|| estimate(&v))
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
        long: agg(&|r| r.long),
        fts_only: agg(&|r| r.fts_only),
        cancelled5_history: flag_estimate(runs, |r| r.cancelled5_history),
        reliable_first_no_boost: flag_estimate(runs, |r| r.reliable_first_no_boost),
        chunk_recall5_with_overlap: flag_estimate(runs, |r| {
            (r.kind == Kind::Chunk)
                .then(|| r.leaves.iter().take(5).any(|l| l.evidence_with_overlap))
        }),
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
    /// Сравнение с прежним прогоном (`--compare`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare: Option<Comparison>,
    /// Разрезанные границами листов доказательства запросов `chunk` (по данным прогона).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitStats>,
    pub queries: Vec<QueryRun>,
}

/// Пути, от которых зависят замеры eval и bench: код, манифесты сборки и поставки, корпус
/// и запросы eval. Правки документации и результатов (`eval/results/`, `docs/`) прогон не
/// делают «грязным».
const MEASURED_PATHS: &[&str] = &[
    "crates",
    "xtask",
    "Cargo.toml",
    "Cargo.lock",
    "deploy",
    "eval/corpus",
    "eval/queries.toml",
    "eval/insights.toml",
    "eval/grid.toml",
    "eval/grid-overlap.toml",
    "eval/grid-chunk-size.toml",
    "eval/grid-confirm.toml",
    "eval/grid-relaxed.toml",
    "eval/grid-m3.toml",
    "eval/grid-m3-kind.toml",
];

/// `git <args> -- <MEASURED_PATHS>`.
fn with_paths<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut v = args.to_vec();
    v.push("--");
    v.extend_from_slice(MEASURED_PATHS);
    v
}

/// Короткий хэш HEAD и признак незакоммиченных изменений в [`MEASURED_PATHS`].
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
    let dirty = run(&with_paths(&["status", "--porcelain"])).is_some_and(|s| !s.is_empty());
    if !dirty {
        return sha;
    }
    match dirty_hash(repo) {
        Some(h) => format!("{sha}+dirty.{h}"),
        None => format!("{sha}+dirty"),
    }
}

/// Отпечаток незакоммиченных изменений в [`MEASURED_PATHS`]: `git hash-object` от
/// `git diff HEAD --binary` и хэшей неотслеживаемых файлов (не из `.gitignore`) — одинаковое
/// дерево даёт одинаковый отпечаток, по нему прогоны одного «грязного» кода сопоставимы.
/// 8 hex-символов.
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
    let mut buf = out(&with_paths(&["diff", "HEAD", "--binary"]))?;
    let untracked = String::from_utf8(out(&with_paths(&[
        "ls-files",
        "--others",
        "--exclude-standard",
    ]))?)
    .ok()?;
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
    mut runs: Vec<QueryRun>,
) -> RunResult {
    for r in &mut runs {
        r.long = r.expect.iter().any(|p| is_long_path(p));
        r.fts_only = r
            .expect
            .iter()
            .any(|p| corpus.get(p).is_some_and(Note::index_disabled));
    }
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
        compare: None,
        split: None,
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
            "корпус загружен: {} заметок и {} insight за {:.1} с",
            corpus.notes.len(),
            corpus.insights.len(),
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
    let split = chunk_split(&s, queries)?;
    let mut results = Vec::new();
    for &mode in &a.modes {
        let runs = s.run_queries(queries, mode)?;
        let mut r = result(&s, p, corpus, queries, mode, label.as_deref(), runs);
        r.split = split.clone();
        results.push(r);
    }
    s.stop()?;
    // reliable_first без бустов certainty: тот же индекс, перезапуск с search.boost_* = 1
    let certainty: Vec<Query> = queries
        .iter()
        .filter(|q| q.kind == Kind::Certainty)
        .cloned()
        .collect();
    if !certainty.is_empty() {
        let mut nb = spec.clone();
        nb.sets.extend(
            NO_CERTAINTY_BOOSTS
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let mut s = Session::start(p, &nb, &server, false)?;
        s.check_model()?;
        if nb.model.is_some() {
            s.wait_indexed()?;
        }
        for r in &mut results {
            let runs = s.run_queries(&certainty, r.info.mode)?;
            apply_no_boost(r, &runs);
        }
        s.stop()?;
    }
    let mut failed = Vec::new();
    for mut r in results {
        if let Some(prev) = &a.compare {
            r.compare = Some(compare(&p.repo, prev, &r)?);
        }
        let (json, md) = report::write_run(&p.results, &r)?;
        report::print_run(&r);
        println!("  {}\n  {}", json.display(), md.display());
        failed.extend(history_control(&r));
    }
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    Ok(())
}

/// Разрезанные доказательства запросов `chunk` по листам прогона (`None` — таких запросов нет).
pub fn chunk_split(s: &Session, queries: &[Query]) -> Result<Option<SplitStats>> {
    if !queries.iter().any(|q| q.kind == Kind::Chunk) {
        return Ok(None);
    }
    Ok(Some(split_stats(&leaf_cores(&s.data)?, queries)))
}

/// `reliable_first` прохода без бустов → `reliable_first_no_boost` запросов результата.
pub fn apply_no_boost(r: &mut RunResult, runs: &[QueryRun]) {
    for x in runs {
        if let Some(q) = r.queries.iter_mut().find(|q| q.id == x.id) {
            q.reliable_first_no_boost = x.metrics.reliable_first;
        }
    }
    r.metrics = breakdown(&r.queries);
}

/// Контроль устаревания: с `include_history: true` замещённые и отозванные insight должны
/// попадать в top-5 хотя бы иногда (`cancelled@5 > 0`); ноль — набор не проверяет фильтр
/// статусов (запросы не находят старые записи и с историей), прогон завершается ошибкой.
pub fn history_control(r: &RunResult) -> Option<String> {
    match r.metrics.cancelled5_history {
        Some(e) if e.mean <= 0.0 => Some(format!(
            "{}: контроль include_history — cancelled@5 = 0, запросы superseded не находят \
             замещённые записи даже с историей (набор устаревания нечувствителен)",
            r.info.mode.as_str()
        )),
        _ => None,
    }
}

/// Отпечаток набора запросов: FNV-1a по id, тексту, ожидаемым заметкам и виду.
pub fn queries_fingerprint(queries: &[Query]) -> String {
    let text: String = queries
        .iter()
        .map(|q| {
            let mut s = format!("{}\0{}\0{}\0{}\0", q.id, q.text, q.expect.join(","), q.kind);
            // поля M3 — только если заданы: отпечаток запросов M2 не меняется
            if !q.forbid.is_empty()
                || !q.weaker.is_empty()
                || q.evidence.is_some()
                || !q.also.is_empty()
            {
                s.push_str(&format!(
                    "{}\0{}\0{}\0{}\0",
                    q.forbid.join(","),
                    q.weaker.join(","),
                    q.evidence.as_deref().unwrap_or(""),
                    q.also.join(",")
                ));
            }
            s
        })
        .collect();
    super::corpus::fnv1a(text.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_control_fails_on_zero() {
        let est = |m: f64| Estimate {
            mean: m,
            lo: m,
            hi: m,
        };
        let mut r = RunResult {
            info: RunInfo {
                date: String::new(),
                git: String::new(),
                label: None,
                mode: Mode::Hybrid,
                model: None,
                model_id: None,
                device: None,
                device_reason: None,
                reranker: None,
                sets: BTreeMap::new(),
                server_config: Value::Null,
                notes: 0,
                corpus_fingerprint: String::new(),
                queries: 0,
                queries_fingerprint: String::new(),
                index_secs: 0.0,
                query_ms_p50: 0.0,
                query_ms_p95: 0.0,
            },
            metrics: breakdown(&[]),
            compare: None,
            split: None,
            queries: Vec::new(),
        };
        // нет запросов superseded — контроля нет
        assert!(history_control(&r).is_none());
        r.metrics.cancelled5_history = Some(est(0.0));
        assert!(history_control(&r).is_some());
        r.metrics.cancelled5_history = Some(est(0.2));
        assert!(history_control(&r).is_none());
    }

    #[test]
    fn group_e_notes_go_to_the_solution() {
        assert_eq!(note_space("ru/zam/vault-secrets"), EVAL_SOLUTION);
        assert_eq!(note_space("en/long/oncall-handbook"), "knowledge");
        assert!(EVAL_SOLUTION.parse::<String>().is_ok() && EVAL_SOLUTION.len() == 36);
    }
}
