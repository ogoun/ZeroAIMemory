//! `zam session snapshot --transcript <файл> [--format claude|codex] [--session <id>]` (M6 §8а,
//! задача 17b): снимок сессии из журнала агента вручную — для агентов без хуков и ручного снимка.
//!
//! - **Формат**: `--format`, иначе по первой строке журнала (`"type":"session_meta"` — Codex,
//!   иначе Claude Code).
//! - **Сессия** по порядку: `--session`, `ZAM_SESSION_ID`, id из самого журнала (Claude —
//!   `sessionId` последней записи, Codex — `session_meta.payload.id`; [`Snapshot::session_hint`]).
//!   Ни одного источника — код 1 и «укажите --session». Записи журнала берутся последней сессии
//!   файла (как у хука без `session_id`): явный id задаёт только сессию на сервере.
//! - Сессии нет на сервере (`GET /sessions/{id}` → 404) — `POST /sessions {id, agent, solution}`,
//!   как в `session-start`. Есть — не трогается (`POST` освежил бы `last_seen_at`, и оборванная
//!   сессия выглядела бы живой).
//! - Снимок — та же сборка, что у хуков ([`hook::snapshot::take`]: хвост `hooks.snapshot_tail_kb`,
//!   маскировка, пределы), со сроком [`CLI_DEADLINE`]; `PUT /sessions/{id}/snapshot` с
//!   `X-Zam-Session: <id>` (явная сессия). Ошибка сервера — ошибка команды, в очередь хуков снимок
//!   не кладётся. Пустой снимок (нет ни запросов, ни файлов, ни команд, ни конспекта) — ошибка
//!   ввода без `PUT`. Вывод — «снимок сессии <id>: N байт, <полнота>», `--json` — `{session, bytes,
//!   status, trigger}`.
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use zam_core::snapshot::SnapTrigger;

use super::hook::snapshot::{self, AgentKind, TakeOpts};
use super::hook::snapshot_queue::{self, MAX_BYTES};
use super::hook::{self, Env, Kind, encode_segment};
use super::{Common, emit, emit_json};
use crate::client::{CliError, EXIT_INPUT, EXIT_NOT_FOUND};

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

pub fn run(
    common: &Common,
    transcript: &Path,
    format: Option<&str>,
    session: Option<&str>,
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
        .or_else(|| hook::session_var(std::env::var("ZAM_SESSION_ID").ok()));
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
    let opts = TakeOpts {
        root: env.root.clone(),
        agent,
        // записи журнала — последней сессии файла: явный id задаёт только сессию на сервере
        session: String::new(),
        trigger: SnapTrigger::Manual,
        tail_kb: settings.snapshot_tail_kb(),
        deadline: CLI_DEADLINE,
        max_bytes: MAX_BYTES,
        summary: None,
        min_len: None,
        min_idle: None,
    };
    let snap = snapshot::take(transcript, &opts).ok_or_else(|| {
        CliError::new(
            EXIT_INPUT,
            format!("журнал агента не прочитан: {}", transcript.display()),
        )
    })?;
    let Some(id) = explicit.or_else(|| hook::session_var(snap.session_hint.clone())) else {
        return Err(CliError::config(
            "в журнале нет id сессии: укажите --session",
        ));
    };
    let d = &snap.data;
    if d.compact_summary.is_none()
        && d.prompts.is_empty()
        && d.files.is_empty()
        && d.commands.is_empty()
    {
        return Err(CliError::input(format!(
            "снимок пуст: в хвосте журнала нет ни запросов, ни файлов, ни команд, ни конспекта \
             (формат {}?)",
            agent.as_str()
        )));
    }
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
