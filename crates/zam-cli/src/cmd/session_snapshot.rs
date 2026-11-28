//! `zam session snapshot --transcript <файл> [--format claude|codex] [--session <id>]
//! [--tail-kb N]` (M6 §8а, задача 17b): снимок сессии из журнала агента вручную — для агентов без
//! хуков и ручного снимка; `zam sessions show <id> [--json]` (M7.5 §8.9 п. 4) — чтение снимка.
//!
//! - **Формат**: `--format`, иначе по первой строке журнала (`"type":"session_meta"` — Codex,
//!   иначе Claude Code).
//! - **Сессия** по порядку: `--session`, `ZAM_SESSION_ID`, id из самого журнала (Claude —
//!   `sessionId` последней записи, Codex — `session_meta.payload.id`;
//!   [`TranscriptAdapter::session_hint`](crate::cmd::hook::snapshot::TranscriptAdapter::session_hint)),
//!   затем — `sessionId` последней строки хвоста любого вида (M7.5 §8.9 п. 4: служебные строки
//!   `bridge-session`, `/model` предфильтр снимка не пропускает, [`any_line_session`]). Ни одного
//!   источника — код 1 и «укажите --session». Записи журнала берутся последней сессии
//!   файла (как у хука без `session_id`): явный id задаёт только сессию на сервере.
//! - Сессии нет на сервере (`GET /sessions/{id}` → 404) — `POST /sessions {id, agent, solution}`,
//!   как в `session-start`. Есть — не трогается (`POST` освежил бы `last_seen_at`, и оборванная
//!   сессия выглядела бы живой).
//! - Снимок — та же сборка, что у хуков ([`hook::snapshot::take`]: хвост `--tail-kb` (диапазон
//!   ключа `hooks.snapshot_tail_kb`, 64..=16384; нет флага — ключ), маскировка, пределы), со сроком
//!   [`CLI_DEADLINE`]; `PUT /sessions/{id}/snapshot` с `X-Zam-Session: <id>` (явная сессия).
//!   Ошибка сервера — ошибка команды, в очередь хуков снимок не кладётся. Пустой снимок (нет ни
//!   запросов, ни файлов, ни команд, ни конспекта) — «в журнале нет запросов — снимать нечего»,
//!   код 0, без запросов к серверу **[решение координатора M7.5]** (`--json` — `{session, bytes:
//!   0, status: "empty"}`). Вывод — «снимок сессии <id>: N байт, <полнота>», `--json` —
//!   `{session, bytes, status, trigger}`.
//! - **Чтение** (`show`) — `GET /sessions/{id}/snapshot` (сервер маскирует поля заново):
//!   строка `снимок сессии <id> · <taken_at> UTC · <повод> · <полнота> · N байт`, с M7.6 (§9.4,
//!   сервер с `snapshot_m76`) — строки `сессия: <начало> — <конец> UTC` (даты из журнала агента,
//!   нет одной — «?») и `задача: k:<id>`, затем конспект, запросы, файлы и команды; время —
//!   `agent_ts`. Снимка нет — код 5; `--json` — ответ как есть.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::snapshot::SnapTrigger;

use super::hook::snapshot::{self, AgentKind, TakeOpts};
use super::hook::snapshot_queue::{self, MAX_BYTES};
use super::hook::{self, Env, Kind, encode_segment};
use super::{Common, emit, emit_json};
use crate::client::{CliError, EXIT_INPUT, EXIT_NOT_FOUND, read_json};

/// Строка пустого снимка (M7.5 §8.9 п. 4): в хвосте журнала нет ни запросов, ни файлов, ни
/// команд, ни конспекта.
pub const NOTHING_TO_TAKE: &str = "в журнале нет запросов — снимать нечего";

/// Срок сборки снимка командой (не хук: бюджета хука нет, но мегабайты хвоста на HDD).
pub const CLI_DEADLINE: Duration = Duration::from_secs(5);
/// Начало журнала для угадывания формата, байт.
const SNIFF_MAX: u64 = 64 * 1024;

/// Формат журнала по первой строке: `session_meta` — Codex, иначе Claude Code.
pub fn sniff_format(path: &Path) -> AgentKind {
    let mut head = Vec::new();
    let _ = std::fs::File::open(path).and_then(|f| f.take(SNIFF_MAX).read_to_end(&mut head));
    let first = head.split(|b| *b == b'\n').next().unwrap_or_default();
    let s = String::from_utf8_lossy(first);
    if s.contains("\"type\":\"session_meta\"") || s.contains("\"type\": \"session_meta\"") {
        AgentKind::Codex
    } else {
        AgentKind::Claude
    }
}

/// Имя агента сессии для `POST /sessions` (как у хуков).
fn agent_name(a: AgentKind) -> &'static str {
    match a {
        AgentKind::Claude => hook::Agent::Claude.name(),
        AgentKind::Codex => hook::Agent::Codex.name(),
    }
}

/// `sessionId` последней строки хвоста журнала `path` (последние `tail_bytes` байт), в которой
/// он есть, — любой строки, не только прошедших предфильтр снимка (M7.5 §8.9 п. 4); нет или файл
/// не читается — `None`.
pub fn any_line_session(path: &Path, tail_bytes: u64) -> Option<String> {
    #[derive(Deserialize)]
    struct Sid {
        #[serde(rename = "sessionId")]
        session: Option<String>,
    }
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(tail_bytes)))
        .ok()?;
    let mut buf = Vec::new();
    f.take(tail_bytes).read_to_end(&mut buf).ok()?;
    buf.split(|b| *b == b'\n').rev().find_map(|raw| {
        let line = std::str::from_utf8(raw).ok()?;
        if !line.contains("\"sessionId\"") {
            return None;
        }
        let s = serde_json::from_str::<Sid>(line.trim_end_matches('\r')).ok()?;
        hook::session_var(s.session)
    })
}

pub fn run(
    common: &Common,
    transcript: &Path,
    format: Option<&str>,
    session: Option<&str>,
    tail_kb: Option<u32>,
) -> Result<(), CliError> {
    let agent = match format {
        Some(f) => AgentKind::parse(f)
            .ok_or_else(|| CliError::input(format!("--format: claude или codex, не «{f}»")))?,
        None => sniff_format(transcript),
    };
    if !transcript.is_file() {
        return Err(CliError::input(format!(
            "нет журнала агента: {}",
            transcript.display()
        )));
    }
    let explicit = hook::session_var(session.map(str::to_string))
        .or_else(|| hook::session_var(std::env::var(zam_core::MCP_SESSION_ENV).ok()));
    let cwd = std::env::current_dir().unwrap_or_default();
    let hook_agent = match agent {
        AgentKind::Claude => hook::Agent::Claude,
        AgentKind::Codex => hook::Agent::Codex,
    };
    let env = Env::new(
        Kind::Checkpoint,
        hook_agent,
        Instant::now(),
        cwd,
        super::home_dir(),
    );
    let settings = env.settings();
    // `--tail-kb` (диапазон ключа проверил clap) сильнее `hooks.snapshot_tail_kb`
    let tail_kb = tail_kb.unwrap_or_else(|| settings.snapshot_tail_kb());
    let opts = TakeOpts {
        root: env.root.clone(),
        agent,
        // записи журнала — последней сессии файла: явный id задаёт только сессию на сервере
        session: String::new(),
        trigger: SnapTrigger::Manual,
        tail_kb,
        deadline: CLI_DEADLINE,
        max_bytes: MAX_BYTES,
        summary: None,
        min_len: None,
    };
    let snap = snapshot::take(transcript, &opts).ok_or_else(|| {
        CliError::new(
            EXIT_INPUT,
            format!("журнал агента не прочитан: {}", transcript.display()),
        )
    })?;
    let id = explicit
        .or_else(|| hook::session_var(snap.session_hint.clone()))
        .or_else(|| any_line_session(transcript, u64::from(tail_kb) * 1024));
    let d = &snap.data;
    if d.compact_summary.is_none()
        && d.prompts.is_empty()
        && d.files.is_empty()
        && d.commands.is_empty()
    {
        // пустой журнал (`/resume` без запроса, `/model`, служебные строки) — не ошибка (M7.5
        // §8.9 п. 4): сервер не трогается
        if common.json {
            return emit_json(&json!({"session": id, "bytes": 0, "status": "empty"}));
        }
        let at = id.map(|s| format!(" (сессия {s})")).unwrap_or_default();
        return emit(format!("{NOTHING_TO_TAKE}{at}\n").as_bytes());
    }
    let Some(id) = id else {
        return Err(CliError::config(
            "в журнале нет id сессии: укажите --session",
        ));
    };
    if snap.scan_depth_capped {
        eprintln!("предупреждение: secrets_scan_depth");
    }
    let mut ctx = super::connect(common)?;
    ctx.client.set_session(Some(id.clone()));
    let seg = encode_segment(&id);
    match ctx.client.get(&format!("/sessions/{seg}"), &[]) {
        Ok(_) => {}
        Err(e) if e.code == EXIT_NOT_FOUND => {
            let body = json!({
                "id": id,
                "agent": agent_name(agent),
                "solution": ctx.solution.as_ref().map(|s| s.id.clone()),
            });
            ctx.client
                .post_json("/sessions", &body)
                .map_err(|e| e.context("регистрация сессии"))?;
        }
        Err(e) => return Err(e.context("сессия")),
    }
    let body = snapshot_queue::body_bytes(&snap, &hook::cache::now_rfc3339());
    let bytes = body.len();
    ctx.client
        .put_json_body(&format!("/sessions/{seg}/snapshot"), body)
        .map_err(|e| e.context("снимок сессии"))?;
    let status = snap.status.as_str();
    if ctx.json {
        return emit_json(&json!({
            "session": id,
            "bytes": bytes,
            "status": status,
            "trigger": SnapTrigger::Manual.as_str(),
        }));
    }
    emit(format!("снимок сессии {id}: {bytes} байт, {status}\n").as_bytes())
}

/// `zam sessions show <id> [--json]` (M7.5 §8.9 п. 4): снимок сессии `GET /sessions/{id}/snapshot`
/// — текстом ([`show_text`]) или JSON как есть. Снимка или сессии нет — код 5.
pub fn show(common: &Common, id: &str) -> Result<(), CliError> {
    let ctx = super::connect(common)?;
    let id = id.trim();
    let v = ctx
        .client
        .get(&format!("/sessions/{}/snapshot", encode_segment(id)), &[])
        .and_then(read_json)
        .map_err(|e| {
            if e.code == EXIT_NOT_FOUND {
                CliError::new(EXIT_NOT_FOUND, format!("снимка сессии {id} нет на сервере"))
            } else {
                e.context("снимок сессии")
            }
        })?;
    if ctx.json {
        return emit_json(&v);
    }
    emit(show_text(id, &v).as_bytes())
}

/// Снимок текстом: строка «снимок сессии <id> · <время> UTC · <повод> · <полнота> · N байт»
/// (время — `agent_ts`), даты сессии и задача снимка (если есть), затем непустые части —
/// конспект (с отступом), запросы, файлы и команды (у неудачной — пометка «ошибка»).
pub fn show_text(id: &str, v: &Value) -> String {
    let s = |k: &str| v[k].as_str().unwrap_or("?").to_string();
    let taken = v["taken_at"].as_str().map_or_else(
        || "?".to_string(),
        |t| zam_core::time::agent_ts(t).map_or_else(|| t.to_string(), |a| format!("{a} UTC")),
    );
    let mut out = format!(
        "снимок сессии {id} · {taken} · {} · {} · {} байт",
        s("trigger"),
        s("status"),
        v["bytes"].as_u64().unwrap_or(0)
    );
    if let Some(n) = v["skipped"].as_u64().filter(|n| *n > 0) {
        out.push_str(&format!(" · пропущено строк {n}"));
    }
    out.push('\n');
    let d = &v["data"];
    // даты сессии и задача снимка (M7.6 §9.4, сервер с `snapshot_m76`): у старого сервера их нет
    let date = |k: &str| d[k].as_str().and_then(zam_core::time::agent_ts);
    let (start, end) = (date("started_at"), date("ended_at"));
    if start.is_some() || end.is_some() {
        let or_q = |t: Option<String>| t.unwrap_or_else(|| "?".into());
        out.push_str(&format!("сессия: {} — {} UTC\n", or_q(start), or_q(end)));
    }
    if let Some(t) = d["task_id"].as_str() {
        out.push_str(&format!("задача: k:{t}\n"));
    }
    let indent = |t: &str| -> String {
        t.lines()
            .map(|l| format!("  {l}\n"))
            .collect::<Vec<_>>()
            .concat()
    };
    if let Some(sum) = d["compact_summary"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
    {
        out.push_str("конспект:\n");
        out.push_str(&indent(sum));
    }
    let list = |key: &str| d[key].as_array().cloned().unwrap_or_default();
    let prompts = list("prompts");
    if !prompts.is_empty() {
        out.push_str(&format!("запросы ({}):\n", prompts.len()));
        for p in &prompts {
            let p = p.as_str().unwrap_or_default();
            let mut lines = p.lines();
            out.push_str(&format!("  - {}\n", lines.next().unwrap_or_default()));
            for l in lines {
                out.push_str(&format!("    {l}\n"));
            }
        }
    }
    let files = list("files");
    if !files.is_empty() {
        out.push_str(&format!("файлы ({}):\n", files.len()));
        for f in &files {
            out.push_str(&format!("  - {}\n", f.as_str().unwrap_or_default()));
        }
    }
    let commands = list("commands");
    if !commands.is_empty() {
        out.push_str(&format!("команды ({}):\n", commands.len()));
        for c in &commands {
            let mark = if c["failed"] == true {
                "[ошибка] "
            } else {
                ""
            };
            out.push_str(&format!(
                "  - {mark}{}\n",
                c["cmd"].as_str().unwrap_or_default()
            ));
        }
    }
    out
}
