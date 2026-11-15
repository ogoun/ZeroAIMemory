//! Стенд агентного A/B `cargo xtask ab` (спецификация M6 §12, §17 п. 16; задача 12) — офлайн-отчёт
//! по прогонам владельца на живом агенте (≥ 10 задач × 2 плеча × 2 повтора, `eval/README.md`).
//!
//! - **Транскрипты** (`--transcripts <каталог>`, рекурсивно `*.jsonl`): Claude Code — строки с
//!   `message.usage` (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`,
//!   `output_tokens`; сообщение, записанное несколькими строками — по блоку, — считается один раз
//!   на пару (сессия, `message.id`) по всему каталогу), сессия — `sessionId` **каждой строки**
//!   (файлы субагентов той же сессии складываются с ней);
//!   Codex — последний накопленный в файле `token_count.info.total_token_usage` (итоги файлов
//!   одной сессии складываются; `input_tokens` с кэшем,
//!   `cached_input_tokens`, `output_tokens`), сессия — `session_meta.payload.id`. Вызовы
//!   инструментов — по **типу** блоков (`tool_use` у Claude Code; `function_call`,
//!   `custom_tool_call`, `local_shell_call` у Codex). Строки разбираются в типы только с этими
//!   полями: текст сообщений, команды и ответы инструментов не сохраняются, файлы не копируются
//!   и не меняются (тест `xtask/tests/ab.rs`).
//! - **Прогоны без журнала** — ошибка; с `--allow-missing` — в `missing`, в агрегаты и сравнения
//!   не входят. Предупреждения — плечо `holdout` без метки сервера, `main` с меткой, `off` с
//!   выдачей памяти.
//! - **Память** — `GET /sessions/{id}` сервера (`--server`, токен `--token` или `ZAM_TOKEN`):
//!   сумма `tokens` по событиям и `holdout`; 404 (плечо без памяти) — 0; без `--server` — нет.
//! - **Метки** (`--runs`, `eval/ab/runs.toml`): `[[run]] task, arm, rep, session, outcome =
//!   pass|fail`; сравнения — `[[compare]] base, arm`, без них — известные пары §12 (`off` →
//!   `memory`, `nomap` → `map`, `noimpact` → `impact`; контрольная группа реальных сессий —
//!   `holdout` → `main`), иначе единственная пара плеч.
//! - **Отчёт** — Markdown в stdout и JSON в `--out`: по прогонам, по плечам (суммы; средние по
//!   задачам с 95 % интервалом бутстрепа задач, [`BOOTSTRAP_M6`] выборок, зерно [`SEED`]; доля
//!   успешных), по сравнениям (`R = Σ плеча / Σ базы` по общим задачам — парный бутстреп задач;
//!   Δ доли успешных). Повторы задачи в плече усредняются до бутстрепа.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::eval::metrics::{BOOTSTRAP_M6, Estimate, SEED, estimate_with, ratio_estimate_with};

/// Предел одной строки транскрипта, байт: длиннее — пропускается (вложения, огромные выводы).
const LINE_MAX: usize = 64 * 1024 * 1024;
/// Срок запроса к серверу.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Известные пары плеч §12: (база, плечо).
const KNOWN_PAIRS: [(&str, &str); 4] = [
    ("off", "memory"),
    ("nomap", "map"),
    ("noimpact", "impact"),
    ("holdout", "main"),
];

#[derive(clap::Args)]
pub struct AbArgs {
    /// Каталог транскриптов агента (*.jsonl, рекурсивно); читаются только поля usage
    #[arg(long)]
    pub transcripts: PathBuf,
    /// Метки прогонов: задача, плечо, повтор, сессия, pass|fail (eval/ab/runs.toml)
    #[arg(long)]
    pub runs: PathBuf,
    /// Адрес сервера для `GET /sessions/{id}` (токены памяти); без него память не считается
    #[arg(long)]
    pub server: Option<String>,
    /// Токен сервера (по умолчанию — ZAM_TOKEN)
    #[arg(long)]
    pub token: Option<String>,
    /// JSON-отчёт (каталог создаётся)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Прогоны без журнала не ошибка: они перечисляются в `missing` и в агрегаты не входят
    #[arg(long)]
    pub allow_missing: bool,
}

/// Исход прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunLabel {
    task: String,
    arm: String,
    rep: u32,
    session: String,
    outcome: Outcome,
    /// Заметка владельца (в отчёт не идёт).
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComparePair {
    base: String,
    arm: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunsFile {
    #[serde(default)]
    run: Vec<RunLabel>,
    #[serde(default)]
    compare: Vec<ComparePair>,
}

/// Токены и вызовы инструментов одной сессии по транскриптам.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub output: u64,
    pub tool_calls: u64,
}

impl Usage {
    fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
        self.output += o.output;
        self.tool_calls += o.tool_calls;
    }

    fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_creation + self.output
    }
}

// ---- разбор строк: только нужные поля (остальное serde пропускает, не сохраняя) ----

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    message: Option<Message>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    usage: Option<ClaudeUsage>,
    #[serde(default)]
    content: Content,
}

/// `content` сообщения: массив блоков (читается только `type`) или строка (не читается).
#[derive(Deserialize, Default)]
#[serde(untagged)]
enum Content {
    Blocks(Vec<Block>),
    #[default]
    #[serde(skip)]
    None,
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize, Clone, Copy, Default)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize, Clone, Copy, Default)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// Вызовы инструментов Codex — типы `response_item.payload.type`.
const CODEX_TOOL_ITEMS: [&str; 3] = ["function_call", "custom_tool_call", "local_shell_call"];

/// Учёт транскриптов всего каталога (ревью задачи 12, M1): каждая строка относится к **своей**
/// сессии (`sessionId` строки; без него — сессия файла: `session_meta` Codex, последняя
/// встреченная `sessionId`, иначе имя файла), сообщение Claude Code — один раз на пару (сессия,
/// `message.id`) по всему каталогу (одно сообщение в двух файлах — один раз).
#[derive(Default)]
struct Scan {
    /// (сессия, `message.id`) → usage (повтор строки сообщения — одна запись, большая).
    claude: HashMap<(String, String), ClaudeUsage>,
    /// Сессия → сообщения Claude Code без id и итоги Codex (сумма по файлам сессии).
    plain: HashMap<String, Usage>,
    /// Сессия → вызовы инструментов.
    tools: HashMap<String, u64>,
    /// Сессия → файлы с её строками.
    files: HashMap<String, BTreeSet<PathBuf>>,
}

/// Состояние разбора одного файла.
struct FileState {
    path: PathBuf,
    /// Сессия файла для строк без `sessionId`.
    current: Option<String>,
    /// Codex: сессия → последний накопленный итог в этом файле.
    codex: HashMap<String, CodexUsage>,
}

impl FileState {
    fn new(path: &Path) -> FileState {
        FileState {
            path: path.to_path_buf(),
            current: None,
            codex: HashMap::new(),
        }
    }

    fn fallback(&self) -> String {
        self.current.clone().unwrap_or_else(|| {
            self.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }
}

fn claude_usage(u: &ClaudeUsage) -> Usage {
    Usage {
        input: u.input_tokens,
        cache_read: u.cache_read_input_tokens,
        cache_creation: u.cache_creation_input_tokens,
        output: u.output_tokens,
        tool_calls: 0,
    }
}

impl Scan {
    fn touch(&mut self, session: &str, f: &FileState) {
        self.files
            .entry(session.to_string())
            .or_default()
            .insert(f.path.clone());
    }

    fn line(&mut self, f: &mut FileState, l: Line) {
        if let Some(s) = &l.session_id {
            f.current = Some(s.clone());
        }
        if let (Some("session_meta"), Some(id)) = (
            l.kind.as_deref(),
            l.payload.as_ref().and_then(|p| p.id.clone()),
        ) {
            f.current = Some(id);
        }
        let session = l.session_id.clone().unwrap_or_else(|| f.fallback());
        if let Some(m) = l.message {
            let mut counted = false;
            if let Content::Blocks(b) = &m.content {
                let n = b
                    .iter()
                    .filter(|x| x.kind.as_deref() == Some("tool_use"))
                    .count() as u64;
                if n > 0 {
                    *self.tools.entry(session.clone()).or_default() += n;
                    counted = true;
                }
            }
            if let Some(u) = m.usage {
                match m.id {
                    Some(id) => {
                        let e = self.claude.entry((session.clone(), id)).or_default();
                        if u.output_tokens >= e.output_tokens {
                            *e = u;
                        }
                    }
                    None => self
                        .plain
                        .entry(session.clone())
                        .or_default()
                        .add(&claude_usage(&u)),
                }
                counted = true;
            }
            if counted {
                self.touch(&session, f);
            }
        }
        if let Some(p) = l.payload {
            match (l.kind.as_deref(), p.kind.as_deref()) {
                (Some("event_msg"), Some("token_count")) => {
                    if let Some(t) = p.info.and_then(|i| i.total_token_usage) {
                        f.codex.insert(session.clone(), t);
                        self.touch(&session, f);
                    }
                }
                (Some("response_item"), Some(k)) if CODEX_TOOL_ITEMS.contains(&k) => {
                    *self.tools.entry(session.clone()).or_default() += 1;
                    self.touch(&session, f);
                }
                _ => {}
            }
        }
    }

    /// Конец файла: итоги Codex файла — в сумму сессии.
    fn end(&mut self, f: FileState) {
        for (s, c) in f.codex {
            self.plain.entry(s).or_default().add(&Usage {
                input: c.input_tokens.saturating_sub(c.cached_input_tokens),
                cache_read: c.cached_input_tokens,
                cache_creation: 0,
                output: c.output_tokens,
                tool_calls: 0,
            });
        }
    }

    /// Сессия → (usage, число файлов).
    fn finish(self) -> BTreeMap<String, (Usage, usize)> {
        let mut out: BTreeMap<String, (Usage, usize)> = BTreeMap::new();
        for ((s, _), u) in &self.claude {
            out.entry(s.clone()).or_default().0.add(&claude_usage(u));
        }
        for (s, u) in &self.plain {
            out.entry(s.clone()).or_default().0.add(u);
        }
        for (s, n) in &self.tools {
            out.entry(s.clone()).or_default().0.tool_calls += n;
        }
        for (s, f) in &self.files {
            out.entry(s.clone()).or_default().1 = f.len();
        }
        out
    }

    /// Файл: строки читаются потоком; битая или слишком длинная строка пропускается.
    fn file(&mut self, p: &Path) -> Result<()> {
        let fh = fs::File::open(p).with_context(|| format!("транскрипт {}", p.display()))?;
        let mut r = BufReader::new(fh);
        let mut st = FileState::new(p);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if r.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            if buf.len() > LINE_MAX {
                continue;
            }
            if let Ok(l) = serde_json::from_slice::<Line>(&buf) {
                self.line(&mut st, l);
            }
        }
        self.end(st);
        Ok(())
    }
}

/// Файлы `*.jsonl` каталога (рекурсивно), по пути.
fn jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).with_context(|| format!("каталог {}", d.display()))? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Учёт по сессиям: (usage, число файлов).
fn transcripts(dir: &Path) -> Result<BTreeMap<String, (Usage, usize)>> {
    let mut scan = Scan::default();
    for p in jsonl_files(dir)? {
        scan.file(&p)?;
    }
    Ok(scan.finish())
}

/// Память сессии с сервера: (токены по всем событиям, holdout); 404 — `(0, false)`.
fn memory_of(agent: &ureq::Agent, base: &str, token: &str, id: &str) -> Result<(u64, bool)> {
    let url = format!(
        "{}/api/v1/sessions/{}",
        base.trim_end_matches('/'),
        encode_segment(id)
    );
    let mut r = agent
        .get(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .with_context(|| format!("GET {url}"))?;
    let st = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    match st {
        200 => {}
        404 => return Ok((0, false)),
        _ => bail!("GET {url}: HTTP {st}: {text}"),
    }
    let v: Value = serde_json::from_str(&text).with_context(|| format!("GET {url}: не JSON"))?;
    let tokens = v["tokens"]
        .as_object()
        .map(|m| m.values().filter_map(|e| e["tokens"].as_u64()).sum())
        .unwrap_or(0);
    Ok((tokens, v["holdout"].as_bool() == Some(true)))
}

/// Процентное кодирование сегмента пути (id сессии).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Прогон отчёта.
#[derive(Debug, Clone, Serialize)]
struct RunRow {
    task: String,
    arm: String,
    rep: u32,
    session: String,
    outcome: Outcome,
    transcripts: usize,
    input: u64,
    cache_read: u64,
    cache_creation: u64,
    output: u64,
    total: u64,
    tool_calls: u64,
    /// Выдано памятью (`GET /sessions/{id}`); `null` — без `--server`.
    memory: Option<u64>,
    holdout: Option<bool>,
}

/// Метрики прогона по имени (`input`, …, `pass`).
fn metric(r: &RunRow, name: &str) -> f64 {
    match name {
        "input" => r.input as f64,
        "cache_read" => r.cache_read as f64,
        "cache_creation" => r.cache_creation as f64,
        "output" => r.output as f64,
        "total" => r.total as f64,
        "tool_calls" => r.tool_calls as f64,
        "memory" => r.memory.unwrap_or(0) as f64,
        "pass" => f64::from(u8::from(r.outcome == Outcome::Pass)),
        _ => unreachable!("метрика {name}"),
    }
}

const METRICS: [&str; 7] = [
    "input",
    "cache_read",
    "cache_creation",
    "output",
    "total",
    "tool_calls",
    "memory",
];

/// Среднее метрики по повторам задачи в плече: задача → значение.
fn per_task(rows: &[&RunRow], name: &str) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for r in rows {
        let e = acc.entry(r.task.clone()).or_default();
        e.0 += metric(r, name);
        e.1 += 1;
    }
    acc.into_iter()
        .map(|(k, (s, n))| (k, s / n as f64))
        .collect()
}

fn arm_json(rows: &[&RunRow]) -> Value {
    let tasks: BTreeSet<&str> = rows.iter().map(|r| r.task.as_str()).collect();
    let mut sum = serde_json::Map::new();
    let mut avg = serde_json::Map::new();
    for m in METRICS {
        let s: f64 = rows.iter().map(|r| metric(r, m)).sum();
        sum.insert(m.into(), json!(s as u64));
        let v: Vec<f64> = per_task(rows, m).into_values().collect();
        avg.insert(m.into(), json!(estimate_with(&v, BOOTSTRAP_M6)));
    }
    let pass: Vec<f64> = per_task(rows, "pass").into_values().collect();
    json!({
        "runs": rows.len(),
        "tasks": tasks.len(),
        "sum": sum,
        "per_task": avg,
        "pass_rate": estimate_with(&pass, BOOTSTRAP_M6),
    })
}

/// Сравнение `arm` с `base` по общим задачам: `R` по метрикам (парный бутстреп задач) и Δ доли
/// успешных (бутстреп разностей по задачам).
fn compare_json(base: &str, arm: &str, rows: &[RunRow]) -> Option<Value> {
    let of = |a: &str| -> Vec<&RunRow> { rows.iter().filter(|r| r.arm == a).collect() };
    let (b, a) = (of(base), of(arm));
    let bt: BTreeMap<String, f64> = per_task(&b, "total");
    let common: Vec<String> = per_task(&a, "total")
        .into_keys()
        .filter(|t| bt.contains_key(t))
        .collect();
    if common.is_empty() {
        return None;
    }
    let ratio = |m: &str| -> Option<Estimate> {
        let (pa, pb) = (per_task(&a, m), per_task(&b, m));
        let pairs: Vec<(f64, f64)> = common.iter().map(|t| (pa[t], pb[t])).collect();
        ratio_estimate_with(&pairs, BOOTSTRAP_M6, SEED)
    };
    let (pa, pb) = (per_task(&a, "pass"), per_task(&b, "pass"));
    let d: Vec<f64> = common.iter().map(|t| pa[t] - pb[t]).collect();
    Some(json!({
        "base": base,
        "arm": arm,
        "tasks": common.len(),
        "r_total": ratio("total"),
        "r_input": ratio("input"),
        "r_cache_read": ratio("cache_read"),
        "r_output": ratio("output"),
        "r_tool_calls": ratio("tool_calls"),
        "d_pass_rate": estimate_with(&d, BOOTSTRAP_M6),
    }))
}

/// Пары сравнения: заданные в `runs.toml`, иначе известные пары §12, иначе единственная пара.
fn pairs(file: &RunsFile, arms: &BTreeSet<String>) -> Vec<(String, String)> {
    if !file.compare.is_empty() {
        return file
            .compare
            .iter()
            .map(|c| (c.base.clone(), c.arm.clone()))
            .collect();
    }
    let known: Vec<(String, String)> = KNOWN_PAIRS
        .iter()
        .filter(|(b, a)| arms.contains(*b) && arms.contains(*a))
        .map(|(b, a)| (b.to_string(), a.to_string()))
        .collect();
    if !known.is_empty() || arms.len() != 2 {
        return known;
    }
    let mut it = arms.iter();
    let (x, y) = (it.next().unwrap().clone(), it.next().unwrap().clone());
    vec![(x, y)]
}

fn load_runs(p: &Path) -> Result<RunsFile> {
    let text = fs::read_to_string(p).with_context(|| format!("метки {}", p.display()))?;
    let f: RunsFile = toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text))
        .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
    let mut seen = BTreeSet::new();
    for r in &f.run {
        if !seen.insert((r.task.clone(), r.arm.clone(), r.rep)) {
            bail!(
                "{}: повтор прогона task={} arm={} rep={}",
                p.display(),
                r.task,
                r.arm,
                r.rep
            );
        }
    }
    Ok(f)
}

/// Оценка с интервалом, `prec` знаков; не посчитана (`null`: пустой знаменатель) — «—».
fn est_p(e: &Value, prec: usize) -> String {
    match (e["mean"].as_f64(), e["lo"].as_f64(), e["hi"].as_f64()) {
        (Some(m), Some(lo), Some(hi)) => format!("{m:.prec$} [{lo:.prec$}–{hi:.prec$}]"),
        _ => "—".to_string(),
    }
}

fn est(e: &Value) -> String {
    est_p(e, 3)
}

fn est0(e: &Value) -> String {
    est_p(e, 0)
}

/// Несогласованность меток и сервера (ревью задачи 12, M3): плечо `holdout` без метки
/// контрольной группы на сервере, `main` — с ней, у плеча `off` выдача памяти больше 0.
fn consistency_warnings(rows: &[RunRow]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rows {
        let who = format!("{} {} rep {} ({})", r.task, r.arm, r.rep, r.session);
        match (r.arm.as_str(), r.holdout, r.memory) {
            ("holdout", Some(false), _) => {
                out.push(format!("{who}: плечо holdout, а у сессии holdout = false"))
            }
            ("main", Some(true), _) => {
                out.push(format!("{who}: плечо main, а у сессии holdout = true"))
            }
            ("off", _, Some(m)) if m > 0 => {
                out.push(format!("{who}: плечо off, а память выдала {m} токенов"))
            }
            _ => {}
        }
    }
    out
}

fn markdown(j: &Value) -> String {
    let mut s = format!(
        "# Агентный A/B (M6 §12)\n\nпрогонов {}, задач {}; интервалы — 95 % бутстреп задач \
         ({BOOTSTRAP_M6} выборок)\n\n## Плечи (среднее на задачу)\n\n\
         | плечо | прогонов | задач | вход | кэш чтение | кэш запись | выход | всего | память | \
         инструменты | доля успешных |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
        j["runs"], j["tasks"]
    );
    if let Some(arms) = j["arms"].as_object() {
        for (name, a) in arms {
            let pt = &a["per_task"];
            s.push_str(&format!(
                "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                a["runs"],
                a["tasks"],
                est0(&pt["input"]),
                est0(&pt["cache_read"]),
                est0(&pt["cache_creation"]),
                est0(&pt["output"]),
                est0(&pt["total"]),
                est0(&pt["memory"]),
                est(&pt["tool_calls"]),
                est(&a["pass_rate"]),
            ));
        }
    }
    let cmp = j["compare"].as_array().cloned().unwrap_or_default();
    if !cmp.is_empty() {
        s.push_str("\n## Сравнения\n\n");
        for c in &cmp {
            let (a, b) = (
                c["arm"].as_str().unwrap_or(""),
                c["base"].as_str().unwrap_or(""),
            );
            s.push_str(&format!(
                "- R {a} / {b} (задач {}): всего {}, вход {}, кэш чтение {}, выход {}, \
                 инструменты {}; Δ доли успешных {}\n",
                c["tasks"],
                est(&c["r_total"]),
                est(&c["r_input"]),
                est(&c["r_cache_read"]),
                est(&c["r_output"]),
                est(&c["r_tool_calls"]),
                est(&c["d_pass_rate"]),
            ));
        }
    }
    let missing = j["missing"].as_array().cloned().unwrap_or_default();
    if !missing.is_empty() {
        s.push_str("\n## Без журнала (в агрегаты не вошли)\n\n");
        for m in &missing {
            s.push_str(&format!(
                "- {} {} rep {}: сессия {}\n",
                m["task"].as_str().unwrap_or(""),
                m["arm"].as_str().unwrap_or(""),
                m["rep"],
                m["session"].as_str().unwrap_or("")
            ));
        }
    }
    let warnings = j["warnings"].as_array().cloned().unwrap_or_default();
    if !warnings.is_empty() {
        s.push_str("\n## Предупреждения\n\n");
        for w in warnings.iter().filter_map(Value::as_str) {
            s.push_str(&format!("- {w}\n"));
        }
    }
    s
}

pub fn main(a: AbArgs) -> Result<()> {
    let file = load_runs(&a.runs)?;
    if file.run.is_empty() {
        bail!("{}: нет ни одного [[run]]", a.runs.display());
    }
    let usage = transcripts(&a.transcripts)?;
    let server = match &a.server {
        Some(url) => {
            let token = a
                .token
                .clone()
                .or_else(|| std::env::var("ZAM_TOKEN").ok())
                .filter(|t| !t.trim().is_empty())
                .context("--server: нужен токен (--token или ZAM_TOKEN)")?;
            Some((crate::smoke::agent_with(HTTP_TIMEOUT), url.clone(), token))
        }
        None => None,
    };
    let mut rows: Vec<RunRow> = Vec::with_capacity(file.run.len());
    // прогоны без журнала (ревью задачи 12, I3): в агрегаты не входят, перечисляются отдельно
    let mut missing: Vec<Value> = Vec::new();
    for r in &file.run {
        let (u, n) = usage.get(&r.session).copied().unwrap_or_default();
        if n == 0 {
            missing.push(json!({"task": r.task, "arm": r.arm, "rep": r.rep,
                                "session": r.session, "outcome": r.outcome}));
            continue;
        }
        let mem = match &server {
            Some((ag, url, tok)) => Some(memory_of(ag, url, tok, &r.session)?),
            None => None,
        };
        rows.push(RunRow {
            task: r.task.clone(),
            arm: r.arm.clone(),
            rep: r.rep,
            session: r.session.clone(),
            outcome: r.outcome,
            transcripts: n,
            input: u.input,
            cache_read: u.cache_read,
            cache_creation: u.cache_creation,
            output: u.output,
            total: u.total(),
            tool_calls: u.tool_calls,
            memory: mem.map(|m| m.0),
            holdout: mem.map(|m| m.1),
        });
    }
    if !missing.is_empty() && !a.allow_missing {
        let ids: Vec<&str> = missing
            .iter()
            .filter_map(|m| m["session"].as_str())
            .collect();
        bail!(
            "нет журнала у {} прогонов: {} — проверьте id сессий и --transcripts \
             (--allow-missing — отчёт без них)",
            ids.len(),
            ids.join(", ")
        );
    }
    let arms: BTreeSet<String> = rows.iter().map(|r| r.arm.clone()).collect();
    let tasks: BTreeSet<&str> = rows.iter().map(|r| r.task.as_str()).collect();
    let mut arms_j = serde_json::Map::new();
    for arm in &arms {
        let rs: Vec<&RunRow> = rows.iter().filter(|r| &r.arm == arm).collect();
        arms_j.insert(arm.clone(), arm_json(&rs));
    }
    let mut cmp = Vec::new();
    let mut warnings: Vec<String> = consistency_warnings(&rows);
    for (b, x) in pairs(&file, &arms) {
        match compare_json(&b, &x, &rows) {
            Some(c) => cmp.push(c),
            None => warnings.push(format!("сравнение {x} / {b}: нет общих задач")),
        }
    }
    let j = json!({
        "runs": rows.len(),
        "tasks": tasks.len(),
        "bootstrap": BOOTSTRAP_M6,
        "seed": SEED,
        "run": rows,
        "arms": arms_j,
        "compare": cmp,
        "missing": missing,
        "warnings": warnings,
    });
    print!("{}", markdown(&j));
    for w in &warnings {
        eprintln!("предупреждение: {w}");
    }
    if let Some(out) = &a.out {
        if let Some(d) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(d)?;
        }
        fs::write(out, serde_json::to_string_pretty(&j)? + "\n")
            .with_context(|| format!("отчёт {}", out.display()))?;
        println!("\nотчёт: {}", out.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Line {
        serde_json::from_str(s).unwrap()
    }

    /// Строки одного «файла» `name`.
    fn scan_file(scan: &mut Scan, name: &str, lines: &[String]) {
        let mut f = FileState::new(Path::new(name));
        for l in lines {
            scan.line(&mut f, line(l));
        }
        scan.end(f);
    }

    fn claude(session: &str, msg: &str, out: u64, block: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"{session}","message":{{"id":"{msg}","usage":{{"input_tokens":3,"cache_read_input_tokens":4,"cache_creation_input_tokens":5,"output_tokens":{out}}},"content":[{{"type":"{block}","text":"x"}}]}}}}"#
        )
    }

    /// Сообщение — один раз на (сессию, id) по всему каталогу; строка относится к своей
    /// `sessionId`, а не к сессии файла.
    #[test]
    fn claude_dedup_by_session_and_message_id() {
        let mut sc = Scan::default();
        scan_file(
            &mut sc,
            "a.jsonl",
            &[
                claude("s", "m", 6, "text"),
                claude("s", "m", 6, "tool_use"),
                r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"строка"}}"#
                    .to_string(),
                // чужая сессия в том же файле
                claude("t", "m", 1, "text"),
            ],
        );
        // то же сообщение в другом файле (копия журнала, resume) — не повторяется
        scan_file(&mut sc, "b.jsonl", &[claude("s", "m", 6, "text")]);
        let got = sc.finish();
        assert_eq!(
            got["s"],
            (
                Usage {
                    input: 3,
                    cache_read: 4,
                    cache_creation: 5,
                    output: 6,
                    tool_calls: 1
                },
                2
            )
        );
        assert_eq!(got["s"].0.total(), 18);
        assert_eq!(got["t"].0.output, 1);
        assert_eq!(got["t"].1, 1);
    }

    /// Codex: последний итог файла, вход без кэша; `info: null` итог не сбрасывает; итоги двух
    /// файлов одной сессии складываются.
    #[test]
    fn codex_last_total_per_file_minus_cache() {
        let tc = |i: u64, c: u64, o: u64| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{i},"cached_input_tokens":{c},"output_tokens":{o}}}}}}}}}"#
            )
        };
        let mut sc = Scan::default();
        scan_file(
            &mut sc,
            "rollout-1.jsonl",
            &[
                r#"{"type":"session_meta","payload":{"id":"c1"}}"#.to_string(),
                tc(5, 1, 1),
                tc(10, 4, 2),
                r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_string(),
                r#"{"type":"response_item","payload":{"type":"local_shell_call"}}"#.to_string(),
            ],
        );
        scan_file(
            &mut sc,
            "rollout-2.jsonl",
            &[
                r#"{"type":"session_meta","payload":{"id":"c1"}}"#.to_string(),
                tc(3, 0, 1),
            ],
        );
        let u = sc.finish()["c1"];
        assert_eq!(
            (u.0.input, u.0.cache_read, u.0.output, u.0.tool_calls, u.1),
            (9, 4, 3, 1, 2)
        );
    }

    #[test]
    fn default_pairs() {
        let arms = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let f = RunsFile::default();
        assert_eq!(
            pairs(&f, &arms(&["memory", "off", "map", "nomap"])),
            [
                ("off".to_string(), "memory".to_string()),
                ("nomap".to_string(), "map".to_string())
            ]
        );
        assert_eq!(
            pairs(&f, &arms(&["a", "b"])),
            [("a".to_string(), "b".to_string())]
        );
        assert!(pairs(&f, &arms(&["a", "b", "c"])).is_empty());
    }

    /// Не посчитанная оценка (`null`) — «—», а не 0.
    #[test]
    fn est_null_is_dash() {
        assert_eq!(est(&Value::Null), "—");
        assert_eq!(est0(&json!({"mean": 1.4, "lo": 1.0, "hi": 2.0})), "1 [1–2]");
    }

    /// Предупреждения: `holdout` без метки, `main` с меткой, `off` с памятью.
    #[test]
    fn consistency_warned() {
        let row = |arm: &str, holdout: Option<bool>, memory: Option<u64>| RunRow {
            task: "t1".into(),
            arm: arm.into(),
            rep: 1,
            session: format!("s-{arm}"),
            outcome: Outcome::Pass,
            transcripts: 1,
            input: 0,
            cache_read: 0,
            cache_creation: 0,
            output: 0,
            total: 0,
            tool_calls: 0,
            memory,
            holdout,
        };
        let w = consistency_warnings(&[
            row("holdout", Some(false), Some(0)),
            row("main", Some(true), Some(5)),
            row("off", Some(false), Some(7)),
            row("off", Some(false), Some(0)),
            row("holdout", Some(true), Some(0)),
            row("memory", None, None),
        ]);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].contains("плечо holdout"), "{w:?}");
        assert!(w[1].contains("плечо main"), "{w:?}");
        assert!(w[2].contains("плечо off") && w[2].contains("7"), "{w:?}");
    }
}
