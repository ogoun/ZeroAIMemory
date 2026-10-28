//! `zam hook post-tool` (бюджет 250 мс; §10.3): ошибка инструмента —
//! `PostToolUseFailure.error` (Claude Code) или непустой `PostToolUse.tool_response.stderr`
//! (Codex, Bash). Сначала локальный `error_regex` по кэшу триггеров (первые
//! [`ERROR_SCAN_MAX`] байт), затем `/recall(post_tool)`. Сервер сам проверяет `error_regex`,
//! поэтому при ответе сервера локальные подсказки добавляются только для записей, которых нет
//! в его `items`; сервер недоступен — остаются локальные. Повторы не гасятся: подсказка об
//! ошибке выдаётся при каждой ошибке (§10.3).
use regex::RegexBuilder;
use serde_json::{Value, json};

use super::cache::{self, Trigger};
use super::input::Input;
use super::output::Output;
use super::{Env, Kind, Settings, head_bytes};
use crate::client::read_json;

/// Сканируемая и отправляемая часть ошибки, байт (как `ERROR_SCAN_MAX` сервера).
pub const ERROR_SCAN_MAX: usize = 64 * 1024;
/// Предел размера скомпилированного regex — как у сервера.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Начало строки подсказки об ошибке (как у сервера).
const LEAD: &str = "эта ошибка уже разбиралась: ";

/// Текст ошибки из stdin: `error` у `PostToolUseFailure`, `tool_response.stderr` у
/// `PostToolUse` (у Codex `tool_response` бывает строкой JSON — она разбирается).
pub fn error_text(inp: &Input) -> Option<String> {
    let s = if inp.event == "PostToolUseFailure" {
        inp.str("error").map(str::to_string)
    } else {
        let tr = inp.v.get("tool_response")?;
        let parsed;
        let obj = match tr {
            Value::String(t) => {
                parsed = serde_json::from_str::<Value>(t).ok()?;
                &parsed
            }
            v => v,
        };
        obj.get("stderr")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    s.filter(|e| !e.trim().is_empty())
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    if !s.on(Kind::PostTool) {
        return Output::Empty;
    }
    let Some(error) = error_text(inp) else {
        return Output::Empty;
    };
    let error = head_bytes(&error, ERROR_SCAN_MAX);
    let tool = inp.str("tool_name").filter(|t| !t.is_empty());
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
    Output::context(&inp.event, &lines.join("\n"))
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
    fn error_from_failure_and_stderr() {
        let f = inp(r#"{"hook_event_name":"PostToolUseFailure","error":"Exit code 1\nbad"}"#);
        assert_eq!(error_text(&f).as_deref(), Some("Exit code 1\nbad"));
        let c = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stdout":"","stderr":"boom"}}"#,
        );
        assert_eq!(error_text(&c).as_deref(), Some("boom"));
        let st = inp(r#"{"hook_event_name":"PostToolUse","tool_response":"{\"stderr\":\"x\"}"}"#);
        assert_eq!(error_text(&st).as_deref(), Some("x"));
        let ok = inp(r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"  "}}"#);
        assert_eq!(error_text(&ok), None);
    }
}
