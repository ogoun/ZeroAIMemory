//! `zam hook post-tool` — два режима.
//!
//! **Ошибка** (бюджет 250 мс; §10.3): `PostToolUseFailure.error` (Claude Code) или непустой
//! `PostToolUse.tool_response.stderr` (Codex, Bash). Сначала локальный `error_regex` по кэшу
//! триггеров (первые [`ERROR_SCAN_MAX`] байт), затем `/recall(post_tool)`. Сервер сам
//! проверяет `error_regex`, поэтому при ответе сервера локальные подсказки добавляются только
//! для записей, которых нет в его `items`; сервер недоступен — остаются локальные. Повторы не
//! гасятся: подсказка об ошибке выдаётся при каждой ошибке (§10.3). Подсказки — по ключу
//! `post_tool`; в журнал сессии (ключ `session_journal`) — `Ev::Error` и, если известна
//! команда, `Ev::Cmd {ok: false, err_fp}`. Петля (§9.3, ключ `session_journal`, и при
//! выключенном `post_tool`): тот же `err_fp` в журнале [`LOOP_AT`]-й раз — строка «эта ошибка
//! повторяется N-й раз в сессии…», раз на отпечаток ([`repeat_hint`]).
//!
//! **Успех** (M4.5 §6.3, §9.1; `PostToolUse` без ошибки; без сети, бюджет 50 мс):
//! - `Read` — `Ev::Read` и знание о файле по `file-refs.json` ([`file_context_lines`], ключ
//!   `file_context`): до 3 записей, раз за сессию на файл (`shown-<сессия>.json`, ключ
//!   `file:<путь>`), ссылка `stale` — «возможно устарело»;
//! - `Write|Edit|MultiEdit|NotebookEdit|apply_patch` — `Ev::Edit` по каждому пути и строка
//!   `{path, hash}` в очередь `changed-<сессия>.jsonl` (при решении; файл больше
//!   [`HASH_MAX`] не хэшируется — его поймает `zam refs check`, удалённый — хэш
//!   [`MISSING_HASH`]);
//! - `Bash` — `Ev::Cmd {exe, ok: true}` (первое слово команды);
//! - MCP [`MCP_MEMORY_LOG`] — `Ev::MemoryWrite {kind}` из `tool_input.kind` (§9.3: запись
//!   insight через MCP гасит напоминание о паре «ошибка → исправление»).
//!
//! Путь — относительно корня проекта по правилам ссылок ([`crate::cmd::refs::ref_path`]);
//! файл вне корня в журнал и очередь не попадает.
use regex::RegexBuilder;
use serde_json::{Value, json};

use super::cache::{self, PathRefs, Shown, Trigger};
use super::input::Input;
use super::journal::{self, Ev};
use super::output::Output;
use super::pre_tool::{is_edit_tool, patch_files};
use super::{Agent, Env, Flag, Kind, Settings, head_bytes};
use crate::client::read_json;
use crate::cmd::refs::{self, MISSING_HASH};

/// Предел файла, который `post-tool` хэширует в бюджете 50 мс, байт.
pub const HASH_MAX: u64 = 8 * 1024 * 1024;
/// Записей в строке знания о файле.
const FILE_ITEMS_MAX: usize = 3;
/// Предел заголовка записи в строке знания о файле, символов.
const TITLE_MAX: usize = 120;
/// Сканируемая и отправляемая часть ошибки, байт (как `ERROR_SCAN_MAX` сервера).
pub const ERROR_SCAN_MAX: usize = 64 * 1024;
/// Предел размера скомпилированного regex — как у сервера.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Начало строки подсказки об ошибке (как у сервера).
const LEAD: &str = "эта ошибка уже разбиралась: ";

/// Текст ошибки из stdin (fix round 1, I-1): у Claude Code — только `PostToolUseFailure.error`
/// (успешный Bash со stderr — успех: предупреждения компиляторов и прогресс идут в stderr); у
/// Codex — непустой `PostToolUse.tool_response.stderr` (`tool_response` бывает строкой JSON — она
/// разбирается), а если в ответе есть код выхода (`exit_code`, `exitCode`, `metadata.exit_code`),
/// — только при ненулевом.
pub fn error_text(inp: &Input, agent: Agent) -> Option<String> {
    let s = if inp.event == "PostToolUseFailure" {
        inp.str("error").map(str::to_string)
    } else if agent == Agent::Codex {
        let tr = inp.v.get("tool_response")?;
        let parsed;
        let obj = match tr {
            Value::String(t) => {
                parsed = serde_json::from_str::<Value>(t).ok()?;
                &parsed
            }
            v => v,
        };
        let code = ["/exit_code", "/exitCode", "/metadata/exit_code"]
            .iter()
            .find_map(|p| obj.pointer(p).and_then(Value::as_i64));
        if code == Some(0) {
            return None;
        }
        obj.get("stderr")
            .and_then(Value::as_str)
            .map(str::to_string)
    } else {
        None
    };
    s.filter(|e| !e.trim().is_empty())
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    match error_text(inp, env.agent) {
        Some(error) => failure(env, inp, s, &error),
        None if inp.event == "PostToolUse" => success(env, inp, s),
        None => Output::Empty,
    }
}

/// Команда `tool_input.command` (строка или массив аргументов).
fn command_of(inp: &Input) -> Option<String> {
    let v = inp.v.get("tool_input")?.get("command")?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = v.as_array()?.iter().filter_map(|x| x.as_str()).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Режим ошибки (M4) и события журнала.
fn failure(env: &Env, inp: &Input, s: &Settings, error: &str) -> Output {
    let error = head_bytes(error, ERROR_SCAN_MAX);
    let tool = inp.str("tool_name").filter(|t| !t.is_empty());
    if let (Some(t), Some(id)) = (tool, inp.session_id.as_deref())
        && is_edit_tool(t)
    {
        record_edits(env, s, inp, t, id, false);
    }
    let mut loop_line = None;
    if let Some(session) = inp.session_id.as_deref()
        && s.flag_on(Flag::SessionJournal)
    {
        let norm = journal::mask_secrets(&journal::normalize_error(error));
        let fp = journal::err_fingerprint(&norm);
        journal::note(
            env,
            session,
            &Ev::Error {
                tool: tool.unwrap_or("").to_string(),
                err_fp: fp.clone(),
                text: journal::error_text(&norm),
            },
        );
        loop_line = repeat_hint(env, session, &fp);
        if tool != Some("apply_patch")
            && let Some(exe) = command_of(inp).as_deref().and_then(journal::exe_of)
        {
            journal::note(
                env,
                session,
                &Ev::Cmd {
                    exe,
                    ok: false,
                    err_fp: Some(fp),
                },
            );
        }
    }
    if !s.on(Kind::PostTool) {
        return Output::context(&inp.event, loop_line.as_deref().unwrap_or_default());
    }
    let server = env.server_url();
    let cached = cache::usable_triggers(env, server.as_deref());
    let local = cached
        .as_ref()
        .map(|c| local_matches(&c.triggers, tool, error, env))
        .unwrap_or_default();
    let remote = env.client().and_then(|client| {
        let body = json!({
            "event": "post_tool",
            "session_id": inp.session_id,
            "solution": env.solution.as_ref().map(|x| x.id.clone()),
            "error": error,
            "tool": tool,
        });
        match client.post_json("/recall", &body).and_then(read_json) {
            Ok(v) => Some(v),
            Err(e) => {
                env.log(&format!("recall(post_tool): {e}"));
                None
            }
        }
    });
    let mut lines: Vec<String> = Vec::new();
    let mut known: Vec<String> = Vec::new();
    if let Some(v) = &remote {
        for w in cache::strings(&v["warnings"]) {
            env.log(&format!("recall(post_tool): предупреждение сервера: {w}"));
        }
        if let Some(t) = v["text"].as_str().filter(|t| !t.trim().is_empty()) {
            lines.push(t.trim_end().to_string());
        }
        if let Some(items) = v["items"].as_array() {
            known.extend(
                items
                    .iter()
                    .filter_map(|i| i["id"].as_str())
                    .map(str::to_string),
            );
        }
    }
    for t in local {
        let r = format!("i:{}", t.entry_id);
        if !known.contains(&r) {
            known.push(r);
            lines.push(t.hint(LEAD));
        }
    }
    lines.extend(loop_line);
    Output::context(&inp.event, &lines.join("\n"))
}

/// Повтор ошибки, с которого `post-tool` советует остановиться (§9.3).
pub const LOOP_AT: usize = 3;

/// Петля (§9.3): ошибка с отпечатком `fp` в журнале сессии [`LOOP_AT`]-й раз и больше — строка
/// «эта ошибка повторяется N-й раз…», раз на отпечаток (`shown`, ключ `loop:<fp>`).
fn repeat_hint(env: &Env, session: &str, fp: &str) -> Option<String> {
    let n = journal::read(&env.root, session)
        .iter()
        .filter(|(_, e)| matches!(e, Ev::Error { err_fp, .. } if err_fp == fp))
        .count();
    if n < LOOP_AT {
        return None;
    }
    let key = format!("loop:{fp}");
    let mut sh = cache::read_shown(env, session);
    if sh.shown.contains(&key) {
        return None;
    }
    sh.shown.push(key);
    cache::write_shown(env, session, &sh);
    Some(format!(
        "ZeroAIMemory: эта ошибка повторяется {n}-й раз в сессии — проверь память \
         (memory_search) или запиши разбор"
    ))
}

/// Имя MCP-инструмента записи в память (Claude Code: `mcp__<сервер>__<инструмент>`).
pub const MCP_MEMORY_LOG: &str = "mcp__zeroaimemory__memory_log";

/// Успешный ответ MCP `memory_log` (§9.1): `Ev::MemoryWrite {kind}` из `tool_input.kind`
/// (до 32 символов; нет — `entry`). Ответ с `isError: true` — не запись.
fn mcp_memory_write(env: &Env, inp: &Input, session: &str) {
    let failed = inp
        .v
        .get("tool_response")
        .and_then(|r| r.get("isError"))
        .and_then(Value::as_bool)
        == Some(true);
    if failed {
        return;
    }
    let kind: String = inp
        .tool_input("kind")
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or("entry")
        .chars()
        .take(32)
        .collect();
    journal::note(env, session, &Ev::MemoryWrite { kind });
}

/// Пути файлов инструмента: `file_path`/`notebook_path`, у `apply_patch` — заголовки патча.
pub fn tool_files(inp: &Input, tool: &str) -> Vec<String> {
    if tool == "apply_patch" {
        return patch_files(&command_of(inp).unwrap_or_default());
    }
    inp.tool_input("file_path")
        .or_else(|| inp.tool_input("notebook_path"))
        .map(str::to_string)
        .into_iter()
        .collect()
}

/// Режим успеха: журнал, знание о файле, очередь изменённых (без сети).
fn success(env: &Env, inp: &Input, s: &Settings) -> Output {
    let tool = inp.str("tool_name").unwrap_or_default();
    let session = inp.session_id.as_deref();
    let journal_on = s.flag_on(Flag::SessionJournal);
    let rels = || -> Vec<String> {
        tool_files(inp, tool)
            .iter()
            .filter_map(|f| refs::ref_path(f, &env.cwd, &env.root).ok())
            .collect()
    };
    match tool {
        "Read" => {
            let rels = rels();
            if let (Some(id), true) = (session, journal_on) {
                for p in &rels {
                    journal::note(env, id, &Ev::Read { path: p.clone() });
                }
            }
            if !s.flag_on(Flag::FileContext) {
                return Output::Empty;
            }
            let mut shown = session.map(|id| cache::read_shown(env, id));
            let lines = file_context_lines(env, shown.as_mut(), &rels);
            if let (Some(id), Some(sh), false) = (session, shown.as_ref(), lines.is_empty()) {
                cache::write_shown(env, id, sh);
            }
            Output::context(&inp.event, &lines.join("\n"))
        }
        t if is_edit_tool(t) => {
            if let Some(id) = session {
                record_edits(env, s, inp, tool, id, journal_on);
            }
            Output::Empty
        }
        "Bash" => {
            if let (Some(id), true) = (session, journal_on)
                && let Some(exe) = command_of(inp).as_deref().and_then(journal::exe_of)
            {
                journal::note(
                    env,
                    id,
                    &Ev::Cmd {
                        exe,
                        ok: true,
                        err_fp: None,
                    },
                );
            }
            Output::Empty
        }
        t if t.eq_ignore_ascii_case(MCP_MEMORY_LOG) => {
            if let (Some(id), true) = (session, journal_on) {
                mcp_memory_write(env, inp, id);
            }
            Output::Empty
        }
        _ => Output::Empty,
    }
}

/// Правка файлов инструментом `tool`: маркер правок, `Ev::Edit` по каждому пути (`journal_on`)
/// и очередь `{path, hash}` (при решении). Вызывается и на отказ правки (fix round 1, M-8):
/// патч мог примениться частично.
fn record_edits(env: &Env, s: &Settings, inp: &Input, tool: &str, id: &str, journal_on: bool) {
    if s.on(Kind::Checkpoint) {
        cache::mark_edits(env, id);
    }
    let rels: Vec<String> = tool_files(inp, tool)
        .iter()
        .filter_map(|f| refs::ref_path(f, &env.cwd, &env.root).ok())
        .collect();
    let mut queue = Vec::new();
    for p in &rels {
        if journal_on {
            journal::note(env, id, &Ev::Edit { path: p.clone() });
        }
        if env.solution.is_some()
            && let Some(hash) = edited_hash(env, p)
        {
            queue.push(cache::Changed {
                path: p.clone(),
                hash,
            });
        }
    }
    cache::queue_changed(env, id, &queue);
}

/// Хэш правленого файла для очереди: нет файла — [`MISSING_HASH`], больше [`HASH_MAX`] или
/// не читается — `None` (строка в журнал хуков).
fn edited_hash(env: &Env, rel: &str) -> Option<String> {
    let abs = env.root.join(rel);
    match std::fs::metadata(&abs) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(MISSING_HASH.to_string()),
        Err(e) => {
            env.log(&format!("хэш правленого файла: {e}"));
            None
        }
        Ok(m) if m.len() > HASH_MAX => {
            env.log("правленый файл больше 8 МБ — не хэшируется (zam refs check)");
            None
        }
        Ok(_) => refs::hash_file(&abs)
            .inspect_err(|e| env.log(&format!("хэш правленого файла: {e}")))
            .ok(),
    }
}

/// Строки знания о файле (§6.3) по `file-refs.json` для путей `rels` (относительно корня):
/// `ZeroAIMemory: о файле <path> в памяти: <title> [<kind>] — <ref>; …` (до 3 записей).
/// `shown` задан — каждый файл раз за сессию (ключ `file:<путь>` добавляется в `shown`).
pub fn file_context_lines(
    env: &Env,
    mut shown: Option<&mut Shown>,
    rels: &[String],
) -> Vec<String> {
    if rels.is_empty() {
        return Vec::new();
    }
    let server = env.server_url();
    let Some(c) = cache::usable_file_refs(env, server.as_deref()) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for rel in rels {
        let Some(pr) = c.find(rel).filter(|p| !p.items.is_empty()) else {
            continue;
        };
        let key = format!("file:{}", pr.path);
        if let Some(sh) = shown.as_deref_mut() {
            if sh.shown.contains(&key) {
                continue;
            }
            sh.shown.push(key);
        }
        lines.push(file_hint(pr));
    }
    lines
}

/// Строка знания о файле: до [`FILE_ITEMS_MAX`] записей, `stale` — «возможно устарело».
pub fn file_hint(pr: &PathRefs) -> String {
    let parts: Vec<String> = pr
        .items
        .iter()
        .take(FILE_ITEMS_MAX)
        .map(|i| {
            let title: String = i
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(TITLE_MAX)
                .collect();
            let mut tag = i.kind.clone();
            if i.state == "stale" {
                tag.push_str(", возможно устарело");
            }
            format!("{title} [{tag}] — {}", i.id)
        })
        .collect();
    format!(
        "ZeroAIMemory: о файле {} в памяти: {}",
        pr.path,
        parts.join("; ")
    )
}

/// Совпадения `error_regex` кэша (триггер с `tools` — только для них).
fn local_matches<'a>(
    triggers: &'a [Trigger],
    tool: Option<&str>,
    error: &str,
    env: &Env,
) -> Vec<&'a Trigger> {
    let mut out: Vec<&Trigger> = Vec::new();
    for t in triggers
        .iter()
        .filter(|t| t.kind == "error_regex" && t.for_tool(tool))
    {
        if out.iter().any(|o| o.entry_id == t.entry_id) {
            continue;
        }
        match RegexBuilder::new(&t.pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
        {
            Ok(r) if r.is_match(error) => out.push(t),
            Ok(_) => {}
            Err(_) => env.log(&format!("триггер {}: неверный regex", t.id)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hook::input::{Input, from_bytes};

    fn inp(s: &str) -> Input {
        Input::parse(&from_bytes(s.as_bytes().to_vec())).unwrap()
    }

    #[test]
    fn file_hint_format() {
        let item = |id: &str, kind: &str, title: &str, state: &str| cache::RefItem {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            state: state.into(),
        };
        let pr = PathRefs {
            path: "src/pool.rs".into(),
            hash: None,
            items: vec![
                item("i:01A", "gotcha", "Пул  соединений\nтечёт", "ok"),
                item("n:01B", "note", "Архитектура", "stale"),
                item("c:01C", "changelog", "Пул v2", "ok"),
                item("c:01D", "changelog", "лишняя", "ok"),
            ],
        };
        assert_eq!(
            file_hint(&pr),
            "ZeroAIMemory: о файле src/pool.rs в памяти: Пул соединений течёт [gotcha] — i:01A; \
             Архитектура [note, возможно устарело] — n:01B; Пул v2 [changelog] — c:01C"
        );
    }

    #[test]
    fn error_from_failure_and_stderr() {
        let (cl, cx) = (Agent::Claude, Agent::Codex);
        let f = inp(r#"{"hook_event_name":"PostToolUseFailure","error":"Exit code 1\nbad"}"#);
        assert_eq!(error_text(&f, cl).as_deref(), Some("Exit code 1\nbad"));
        let c = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stdout":"","stderr":"boom"}}"#,
        );
        assert_eq!(error_text(&c, cx).as_deref(), Some("boom"));
        // у Claude Code stderr успешной команды — не ошибка
        assert_eq!(error_text(&c, cl), None);
        let st = inp(r#"{"hook_event_name":"PostToolUse","tool_response":"{\"stderr\":\"x\"}"}"#);
        assert_eq!(error_text(&st, cx).as_deref(), Some("x"));
        let ok = inp(r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"  "}}"#);
        assert_eq!(error_text(&ok, cx), None);
        // код выхода 0 — успех и со stderr, ненулевой — ошибка
        let z = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"warn","exit_code":0}}"#,
        );
        assert_eq!(error_text(&z, cx), None);
        let n = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"e","metadata":{"exit_code":2}}}"#,
        );
        assert_eq!(error_text(&n, cx).as_deref(), Some("e"));
    }
}
