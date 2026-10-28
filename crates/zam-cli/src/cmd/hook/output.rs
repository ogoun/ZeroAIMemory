//! Вывод хука (§10.3, контракт Claude Code и Codex): `{"hookSpecificOutput": {"hookEventName":
//! "<событие из stdin>", "additionalContext": "<текст>"}}`, у `checkpoint` на `Stop` —
//! `{"decision": "block", "reason": "<текст>"}`, или пусто. Имена полей у обоих агентов одни.
//!
//! Предел текста: Claude Code — [`CLAUDE_MAX_CHARS`] символов (запас к 10 000); Codex —
//! около 2500 токенов (`additionalContextLimit`), поэтому [`CODEX_MAX_BYTES`] байт UTF-8: это
//! ≈ 1900 токенов при оценке 4 байта на токен и ≈ 2500 при 3 байтах (код, пути, кириллица)
//! **[решение исполнителя]**. Длиннее — обрезка по последней целой строке и пометка.
use serde_json::json;

use super::Agent;

/// Предел текста для Claude Code, символов (§10.3).
pub const CLAUDE_MAX_CHARS: usize = 9000;
/// Предел текста для Codex, байт UTF-8 (см. модуль).
pub const CODEX_MAX_BYTES: usize = 7500;
/// Пометка обрезки.
const CUT_MARK: &str = "\n… (подсказки ZeroAIMemory обрезаны)";

/// Результат хука.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Empty,
    /// Контекст агенту: событие из stdin и текст.
    Context {
        event: String,
        text: String,
    },
    /// `Stop`: продолжить с инструкцией (один раз за сессию).
    Block {
        reason: String,
    },
}

impl Output {
    /// Контекст или тишина, если текст пуст.
    pub fn context(event: &str, text: &str) -> Output {
        if text.trim().is_empty() {
            return Output::Empty;
        }
        Output::Context {
            event: event.to_string(),
            text: text.trim_end().to_string(),
        }
    }

    /// JSON для stdout (с переводом строки) или пустая строка.
    pub fn render(&self, agent: Agent) -> String {
        let v = match self {
            Output::Empty => return String::new(),
            Output::Context { event, text } => json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": limit(text, agent),
                }
            }),
            Output::Block { reason } => json!({
                "decision": "block",
                "reason": limit(reason, agent),
            }),
        };
        let mut s = v.to_string();
        s.push('\n');
        s
    }
}

/// Длина текста в единицах предела агента.
fn size(s: &str, agent: Agent) -> usize {
    match agent {
        Agent::Claude => s.chars().count(),
        Agent::Codex => s.len(),
    }
}

/// Текст в пределе агента: целые строки и пометка обрезки.
pub fn limit(text: &str, agent: Agent) -> String {
    let max = match agent {
        Agent::Claude => CLAUDE_MAX_CHARS,
        Agent::Codex => CODEX_MAX_BYTES,
    };
    if size(text, agent) <= max {
        return text.to_string();
    }
    let room = max - size(CUT_MARK, agent);
    // префикс в `room` единиц по границе символа
    let mut end = 0;
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = match agent {
            Agent::Claude => 1,
            Agent::Codex => c.len_utf8(),
        };
        if used + w > room {
            break;
        }
        used += w;
        end = i + c.len_utf8();
    }
    let head = &text[..end];
    let head = head.rfind('\n').map_or(head, |n| &head[..n]);
    format!("{head}{CUT_MARK}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_shapes() {
        assert_eq!(Output::context("X", "  \n").render(Agent::Claude), "");
        let s = Output::context("PreToolUse", "ZeroAIMemory: a").render(Agent::Codex);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            "ZeroAIMemory: a"
        );
        let b = Output::Block { reason: "r".into() }.render(Agent::Claude);
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(
            (v["decision"].as_str(), v["reason"].as_str()),
            (Some("block"), Some("r"))
        );
    }

    #[test]
    fn limits_per_agent() {
        let line = "ZeroAIMemory: строка подсказки — i:01J\n";
        let long = line.repeat(1000);
        let c = limit(&long, Agent::Claude);
        assert!(c.chars().count() <= CLAUDE_MAX_CHARS);
        assert!(c.ends_with(CUT_MARK));
        assert!(c.trim_end_matches(CUT_MARK).ends_with("i:01J"));
        let x = limit(&long, Agent::Codex);
        assert!(x.len() <= CODEX_MAX_BYTES);
        assert!(x.len() > CODEX_MAX_BYTES - line.len() * 2);
        assert_eq!(limit("коротко", Agent::Codex), "коротко");
    }
}
