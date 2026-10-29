//! Вход хука: один JSON-объект на stdin (§10.3, контракт Claude Code и Codex).
//!
//! - Читается не больше [`STDIN_MAX`] байт, остальное отбрасывается (не дочитывается).
//! - Ввод обрезан (огромный `tool_input.content` у Write) — JSON не разобрать, поэтому из
//!   прочитанного начала достаются нужные строковые поля ([`salvage`]): у Claude Code
//!   `hook_event_name`, `session_id`, `cwd`, `tool_name` и `tool_input.file_path` идут раньше
//!   `content`, и первое вхождение ключа — настоящее поле **[решение исполнителя]**. С M4.5
//!   (§9.4) спасаются и `agent_id`, `agent_type` (`SubagentStart`), `tool_input.subagent_type`
//!   и `tool_input.prompt` (`PreToolUse` инструмента `Agent`/`Task`); `prompt`, оборванный
//!   обрезкой, берётся началом — до [`PROMPT_SALVAGE_CHARS`] символов.
//! - Пустой ввод, не-JSON, не объект — `None` (тишина).
use std::io::Read;
use std::path::PathBuf;

use serde_json::Value;

/// Предел чтения stdin, байт (§10.3).
pub const STDIN_MAX: usize = 1024 * 1024;
/// Предел начала `tool_input.prompt`, спасаемого из обрезанного ввода, символов (задача
/// субагента дальше 2000 символов не нужна, §9.4).
pub const PROMPT_SALVAGE_CHARS: usize = 2000;

/// Прочитанный stdin: байты и признак обрезки.
pub struct Raw {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// Читает stdin не больше [`STDIN_MAX`] байт (+1 — признак обрезки).
pub fn read_stdin() -> Raw {
    let mut bytes = Vec::new();
    let _ = std::io::stdin()
        .lock()
        .take(STDIN_MAX as u64 + 1)
        .read_to_end(&mut bytes);
    from_bytes(bytes)
}

pub fn from_bytes(mut bytes: Vec<u8>) -> Raw {
    let truncated = bytes.len() > STDIN_MAX;
    bytes.truncate(STDIN_MAX);
    Raw { bytes, truncated }
}

/// Разобранный вход.
#[derive(Debug, Clone)]
pub struct Input {
    /// `hook_event_name`.
    pub event: String,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Весь объект (после обрезки — только спасённые поля).
    pub v: Value,
}

impl Input {
    pub fn parse(raw: &Raw) -> Option<Input> {
        let text = String::from_utf8_lossy(&raw.bytes);
        let v: Value = match serde_json::from_str(text.trim()) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => return None,
            Err(_) if raw.truncated => salvage(&text),
            Err(_) => return None,
        };
        let event = v["hook_event_name"].as_str()?.to_string();
        let session_id = v["session_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let cwd = v["cwd"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        Some(Input {
            event,
            session_id,
            cwd,
            v,
        })
    }

    /// Строковое поле верхнего уровня.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.v.get(key).and_then(Value::as_str)
    }

    /// Строковое поле `tool_input`.
    pub fn tool_input(&self, key: &str) -> Option<&str> {
        self.v.get("tool_input")?.get(key)?.as_str()
    }
}

/// Строковые поля из обрезанного JSON: первое вхождение `"ключ": "строка"`.
fn salvage(text: &str) -> Value {
    let mut top = serde_json::Map::new();
    for k in [
        "hook_event_name",
        "session_id",
        "cwd",
        "tool_name",
        "agent_id",
        "agent_type",
    ] {
        if let Some(s) = first_string(text, k) {
            top.insert(k.into(), Value::String(s));
        }
    }
    let mut ti = serde_json::Map::new();
    for k in ["file_path", "notebook_path", "command", "subagent_type"] {
        if let Some(s) = first_string(text, k) {
            ti.insert(k.into(), Value::String(s));
        }
    }
    // задача субагента: целиком или, если её оборвала обрезка, — начало
    if let Some(s) = first_string(text, "prompt").or_else(|| first_string_head(text, "prompt")) {
        let head: String = s.chars().take(PROMPT_SALVAGE_CHARS).collect();
        ti.insert("prompt".into(), Value::String(head));
    }
    top.insert("tool_input".into(), Value::Object(ti));
    Value::Object(top)
}

fn first_string(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let at = text.find(&pat)? + pat.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    serde_json::Deserializer::from_str(rest)
        .into_iter::<String>()
        .next()?
        .ok()
}

/// Начало строкового значения `"ключ": "…` без закрывающей кавычки (ввод оборван внутри
/// строки): экранирование JSON разбирается, неполная последовательность в конце отбрасывается.
fn first_string_head(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let at = text.find(&pat)? + pat.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
    let mut out = String::new();
    let mut n = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => {
                let Some(e) = chars.next() else { break };
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        let Ok(code) = u32::from_str_radix(&hex, 16) else {
                            break;
                        };
                        // половина суррогатной пары — замена: важен смысл начала задачи
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
        n += 1;
        if n >= PROMPT_SALVAGE_CHARS {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> Raw {
        from_bytes(s.as_bytes().to_vec())
    }

    #[test]
    fn parses_object_and_rejects_garbage() {
        let i = Input::parse(&raw(
            r#"{"hook_event_name":"PreToolUse","session_id":"s1","cwd":"/x","tool_input":{"file_path":"/x/a.sh"}}"#,
        ))
        .unwrap();
        assert_eq!(i.event, "PreToolUse");
        assert_eq!(i.session_id.as_deref(), Some("s1"));
        assert_eq!(i.tool_input("file_path"), Some("/x/a.sh"));
        for bad in ["", "   ", "не json", "[1,2]", "{\"session_id\":\"s\"}"] {
            assert!(Input::parse(&raw(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn salvages_fields_from_truncated_input() {
        let mut s = String::from(
            r#"{"session_id":"s\"1","cwd":"C:\\p","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"C:\\p\\deploy.sh","content":""#,
        );
        s.push_str(&"x".repeat(STDIN_MAX + 10));
        let r = from_bytes(s.into_bytes());
        assert!(r.truncated);
        let i = Input::parse(&r).unwrap();
        assert_eq!(i.event, "PreToolUse");
        assert_eq!(i.session_id.as_deref(), Some("s\"1"));
        assert_eq!(i.cwd, Some(PathBuf::from("C:\\p")));
        assert_eq!(i.tool_input("file_path"), Some("C:\\p\\deploy.sh"));
        assert_eq!(i.str("tool_name"), Some("Write"));
    }

    /// Задача 10 M4.5: поля субагента и начало оборванного `prompt`.
    #[test]
    fn salvages_subagent_fields() {
        let mut s = String::from(
            r#"{"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"Task","tool_input":{"subagent_type":"Explore","description":"d","prompt":"Найди \"кэш\"\nв коде "#,
        );
        s.push_str(&"я".repeat(STDIN_MAX));
        let r = from_bytes(s.into_bytes());
        assert!(r.truncated);
        let i = Input::parse(&r).unwrap();
        assert_eq!(i.tool_input("subagent_type"), Some("Explore"));
        let p = i.tool_input("prompt").unwrap();
        assert!(p.starts_with("Найди \"кэш\"\nв коде я"), "{p}");
        assert_eq!(p.chars().count(), PROMPT_SALVAGE_CHARS);
        let mut s = String::from(
            r#"{"session_id":"s1","hook_event_name":"SubagentStart","agent_id":"a7","agent_type":"Plan","x":""#,
        );
        s.push_str(&"x".repeat(STDIN_MAX));
        let i = Input::parse(&from_bytes(s.into_bytes())).unwrap();
        assert_eq!(i.str("agent_id"), Some("a7"));
        assert_eq!(i.str("agent_type"), Some("Plan"));
        let head = |t: &str| first_string_head(t, "prompt");
        assert_eq!(head(r#"{"prompt": "abд"#).as_deref(), Some("abд"));
        assert_eq!(head(r#"{"prompt": "ab\"#).as_deref(), Some("ab"));
        assert_eq!(head(r#"{"prompt": 1"#), None);
    }
}
