//! Вход хука: один JSON-объект на stdin (§10.3, контракт Claude Code и Codex).
//!
//! - Читается не больше [`STDIN_MAX`] байт, остальное отбрасывается (не дочитывается).
//! - Ввод обрезан (огромный `tool_input.content` у Write) — JSON не разобрать, поэтому из
//!   прочитанного начала достаются нужные строковые поля ([`salvage`]): у Claude Code
//!   `hook_event_name`, `session_id`, `cwd`, `tool_name` и `tool_input.file_path` идут раньше
//!   `content`, и первое вхождение ключа — настоящее поле **[решение исполнителя]**.
//! - Пустой ввод, не-JSON, не объект — `None` (тишина).
use std::io::Read;
use std::path::PathBuf;

use serde_json::Value;

/// Предел чтения stdin, байт (§10.3).
pub const STDIN_MAX: usize = 1024 * 1024;

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
    for k in ["hook_event_name", "session_id", "cwd", "tool_name"] {
        if let Some(s) = first_string(text, k) {
            top.insert(k.into(), Value::String(s));
        }
    }
    let mut ti = serde_json::Map::new();
    for k in ["file_path", "notebook_path", "command"] {
        if let Some(s) = first_string(text, k) {
            ti.insert(k.into(), Value::String(s));
        }
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
}
