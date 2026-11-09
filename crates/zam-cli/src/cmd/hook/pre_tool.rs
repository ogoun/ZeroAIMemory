//! `zam hook pre-tool` (`PreToolUse`, matcher `Write|Edit|Bash`; ≤ 5 мс сопоставление, ≤ 50 мс
//! процесс; §10.3) — **только локально, без сети**:
//!
//! - `file_glob` — по `tool_input.file_path` (у NotebookEdit — `notebook_path`) относительно
//!   корня (`/` как разделитель, `.` и `..` нормализуются; вне корня — не сопоставляется); на
//!   Windows без учёта регистра; `command_regex` — по `tool_input.command`;
//! - триггер с `tools` — только для этих инструментов (без учёта регистра);
//! - Codex правит файлы инструментом `apply_patch` (`tool_input.command` — текст патча): пути для
//!   `file_glob` — из заголовков `*** Add File:`, `*** Update File:`, `*** Delete File:` (и
//!   `*** Move to:`), `command_regex` к патчу не применяется (fix round 1, I-3; ⚠️ формат
//!   входа на живом Codex не проверен);
//! - знание о файле (M4.5 §6.3, ключ `file_context`) у правки — первыми строками, до
//!   триггеров `file_glob`: те же строки, что у `post-tool` на `Read`, раз за сессию на файл;
//! - запись показывается не больше раза за сессию (`shown-<сессия>.json`); правка
//!   Write/Edit/MultiEdit/NotebookEdit/`apply_patch` — маркер `edits-<сессия>` для `checkpoint`
//!   (R1). Маркер пишется и при выключенном `pre_tool`, если включён `checkpoint`
//!   **[решение исполнителя]**.
//!
//! Подсказка: `ZeroAIMemory: для <маска>: <title> [<вид>, <certainty>] — i:<id>`, для команды —
//! без «для …». Показ `pre_tool` в `usage` сервера не пишется: сети здесь нет.
//!
//! M4.5 §9.4: запуск субагента (инструмент `Agent`/`Task`, matcher `Agent|Task` регистрирует
//! `zam init`) — запись `{at, agent_type, task}` в очередь `spawn-<сессия>.json`; её берёт
//! `subagent-start`. Пишется по ключу `subagent_start`, независимо от `pre_tool`.

use globset::GlobBuilder;
// пути из входа инструментов агента — общие с адаптерами снимка сессии (§8а)
use regex::RegexBuilder;
pub use zam_core::toolpath::{glob_path, patch_files};

use super::cache::{self, Trigger};
use super::input::Input;
use super::output::Output;
use super::{Env, Flag, Kind, Settings};

/// Предел размера скомпилированного regex — как у проверки триггеров на сервере.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Предел команды для сопоставления, байт.
const COMMAND_SCAN_MAX: usize = 64 * 1024;

/// Инструменты правки файлов (факт правок для `checkpoint`).
pub fn is_edit_tool(name: &str) -> bool {
    matches!(
        name,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch"
    )
}

/// Инструмент запуска субагента у Claude Code (`Agent`; прежнее имя — `Task`).
pub fn is_spawn_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

/// Тип субагента, если `subagent_type` не задан (так делает сам Claude Code).
pub const DEFAULT_AGENT_TYPE: &str = "general-purpose";

/// Запуск субагента — в очередь `spawn-<сессия>.json` (§9.4): тип и первые 2000 символов
/// `tool_input.prompt`. Ключ `subagent_start` выключен или нет сессии — ничего.
fn queue_spawn(env: &Env, inp: &Input, s: &Settings, session: Option<&str>) {
    let Some(id) = session else { return };
    if !s.on(Kind::SubagentStart) {
        return;
    }
    let agent_type = inp
        .tool_input("subagent_type")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(DEFAULT_AGENT_TYPE);
    let task: String = inp
        .tool_input("prompt")
        .unwrap_or_default()
        .chars()
        .take(super::input::PROMPT_SALVAGE_CHARS)
        .collect();
    cache::push_spawn(
        env,
        id,
        cache::Spawn {
            at: super::journal::Timestamp::now(),
            agent_type: agent_type.chars().take(128).collect(),
            task,
        },
    );
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    let tool = inp.str("tool_name").unwrap_or_default();
    let hints_on = s.on(Kind::PreTool);
    let session = inp.session_id.as_deref();
    if is_spawn_tool(tool) {
        queue_spawn(env, inp, s, session);
    }
    if is_edit_tool(tool)
        && s.on(Kind::Checkpoint)
        && let Some(id) = session
    {
        cache::mark_edits(env, id);
    }
    if !hints_on {
        return Output::Empty;
    }
    let command = command_of(inp);
    let (files, command) = if tool == "apply_patch" {
        (patch_files(command.as_deref().unwrap_or_default()), None)
    } else {
        let f = inp
            .tool_input("file_path")
            .or_else(|| inp.tool_input("notebook_path"));
        (f.map(str::to_string).into_iter().collect(), command)
    };
    let mut shown = session.map(|id| cache::read_shown(env, id));
    let mut dirty = false;
    let mut lines: Vec<String> = Vec::new();
    // знание о файле (M4.5 §6.3) — перед триггерами `file_glob`, в общем пределе текста
    if is_edit_tool(tool) && s.flag_on(Flag::FileContext) {
        let rels: Vec<String> = files
            .iter()
            .filter_map(|f| crate::cmd::refs::ref_path(f, &env.cwd, &env.root).ok())
            .collect();
        let before = shown.as_ref().map_or(0, |sh| sh.shown.len());
        lines.extend(super::post_tool::file_context_lines(
            env,
            shown.as_mut(),
            &rels,
        ));
        dirty |= shown.as_ref().map_or(0, |sh| sh.shown.len()) != before;
    }
    let server = env.server_url();
    if let Some(c) = cache::usable_triggers(env, server.as_deref()) {
        let found = matches(&c.triggers, tool, &files, command.as_deref(), env);
        let mut taken: Vec<&str> = Vec::new();
        for (t, lead) in found {
            if taken.contains(&t.entry_id.as_str()) {
                continue;
            }
            taken.push(&t.entry_id);
            if let Some(sh) = shown.as_mut() {
                if sh.shown.contains(&t.entry_id) {
                    continue;
                }
                sh.shown.push(t.entry_id.clone());
                dirty = true;
            }
            lines.push(t.hint(&lead));
        }
    }
    if dirty && let (Some(id), Some(sh)) = (session, shown.as_ref()) {
        cache::write_shown(env, id, sh);
    }
    let text = lines.join("\n");
    // выданное без сети — в очередь учёта, уйдёт со следующим сетевым хуком (M4.5 §9.5)
    if let Some(id) = session {
        cache::note_tokens(env, id, cache::TOKENS_PRE_TOOL, &text);
    }
    Output::context(&inp.event, &text)
}

/// `tool_input.command`: строка или массив аргументов (через пробел).
fn command_of(inp: &Input) -> Option<String> {
    let v = inp.v.get("tool_input")?.get("command")?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = v.as_array()?.iter().filter_map(|x| x.as_str()).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Совпавшие триггеры и начало строки подсказки (`для <маска>: ` у `file_glob`).
fn matches<'a>(
    triggers: &'a [Trigger],
    tool: &str,
    files: &[String],
    command: Option<&str>,
    env: &Env,
) -> Vec<(&'a Trigger, String)> {
    let tool = (!tool.is_empty()).then_some(tool);
    let rels: Vec<String> = files
        .iter()
        .filter_map(|f| glob_path(f, &env.cwd, &env.root))
        .collect();
    let command = command.map(|c| super::head_bytes(c, COMMAND_SCAN_MAX));
    let mut out = Vec::new();
    for t in triggers.iter().filter(|t| t.for_tool(tool)) {
        match (t.kind.as_str(), command) {
            ("file_glob", _) if !rels.is_empty() => {
                let g = GlobBuilder::new(&t.pattern)
                    .case_insensitive(cfg!(windows))
                    .build();
                match g {
                    Ok(g) => {
                        let m = g.compile_matcher();
                        if rels.iter().any(|p| m.is_match(p)) {
                            out.push((t, format!("для {}: ", t.pattern)));
                        }
                    }
                    Err(_) => env.log(&format!("триггер {}: неверная маска", t.id)),
                }
            }
            ("command_regex", Some(cmd)) => {
                let r = RegexBuilder::new(&t.pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .dfa_size_limit(REGEX_SIZE_LIMIT)
                    .build();
                match r {
                    Ok(r) if r.is_match(cmd) => out.push((t, String::new())),
                    Ok(_) => {}
                    Err(_) => env.log(&format!("триггер {}: неверный regex", t.id)),
                }
            }
            _ => {}
        }
    }
    out
}
