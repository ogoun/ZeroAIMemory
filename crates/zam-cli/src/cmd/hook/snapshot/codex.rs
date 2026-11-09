//! Адаптер журнала Codex (`~/.codex/sessions/…/rollout-*.jsonl`), §8а. Формат — по документации
//! и исходникам Codex (`RolloutLine {timestamp, type, payload}`); живого журнала на машине
//! владельца нет (задача 17a, шаг 0), фикстура — `codex-doc-session.jsonl`:
//! - конспект — `type: "compacted"`, текст в `payload.message`; `response_item` с
//!   `payload.type: "compaction"` несёт `encrypted_content` — не берётся;
//! - запрос человека — `type: "event_msg"` с `payload.type: "user_message"` (`payload.message`).
//!   `response_item` с `role: "user"` не берутся: в них `<environment_context>` и
//!   `<user_instructions>` (правила проекта не копируются, §2);
//! - команда — `response_item` `function_call` с именем `shell`/`shell_command`/`exec_command`
//!   (`arguments` — JSON-строка: `command` строкой или массивом, `cmd`) или `local_shell_call`
//!   (`action.command`); исход — `function_call_output` с тем же `call_id` (`metadata.exit_code`
//!   или «Exit code: N»);
//! - файлы — пути патча `apply_patch` (`custom_tool_call.input`, `function_call.arguments.input`
//!   или команда `apply_patch …`), относительно `cwd` сессии;
//! - сессия — `session_meta.payload.id`; файл Codex — одна сессия, фильтра по сессии нет.
//!   `session_meta` — первая строка файла: вне окна хвоста её читает `read_head` (мод. снимка);
//! - каталог путей патча — `cwd` последнего `turn_context` в окне, иначе `session_meta.payload.cwd`,
//!   иначе корень решения.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use zam_core::snapshot::{SnapCommand, SnapshotData};
use zam_core::toolpath::{glob_path, patch_files};

use super::{TranscriptAdapter, strip_tags};

/// Признаки строк, которые стоит разбирать: §8а и вызовы инструментов Codex (`"function_call`
/// без закрывающей кавычки покрывает и `function_call_output`).
pub const PREFILTER: [&str; 7] = [
    "\"compacted\"",
    "\"user_message\"",
    "\"session_meta\"",
    "\"turn_context\"",
    "\"function_call",
    "\"custom_tool_call",
    "\"local_shell_call",
];

/// Обёртки окружения и правил проекта — не слова человека.
const INSERT_TAGS: [&str; 2] = ["user_instructions", "environment_context"];

/// Инструменты оболочки Codex.
const SHELL_TOOLS: [&str; 4] = ["shell", "shell_command", "exec_command", "container.exec"];

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<Value>,
    cwd: Option<Value>,
    message: Option<Value>,
    name: Option<String>,
    arguments: Option<Value>,
    input: Option<Value>,
    action: Option<Value>,
    call_id: Option<String>,
    output: Option<Value>,
}

/// Адаптер журнала Codex.
pub struct CodexAdapter {
    root: PathBuf,
    cwd: Option<PathBuf>,
    hint: Option<String>,
    skipped: u32,
    data: SnapshotData,
    pending: HashMap<String, usize>,
}

impl CodexAdapter {
    pub fn new(root: &Path) -> Self {
        CodexAdapter {
            root: root.to_path_buf(),
            cwd: None,
            hint: None,
            skipped: 0,
            data: SnapshotData::default(),
            pending: HashMap::new(),
        }
    }

    fn files(&mut self, patch: &str) {
        let cwd = self.cwd.clone().unwrap_or_else(|| self.root.clone());
        for p in patch_files(patch) {
            if let Some(rel) = glob_path(&p, &cwd, &self.root) {
                self.data.files.push(rel);
            }
        }
    }

    fn command(&mut self, cmd: String, call_id: Option<String>) {
        if cmd.trim_start().starts_with("apply_patch") {
            self.files(&cmd);
            return;
        }
        if let Some(id) = call_id {
            self.pending.insert(id, self.data.commands.len());
        }
        self.data.commands.push(SnapCommand { cmd, failed: None });
    }

    fn response_item(&mut self, p: Payload) {
        match p.kind.as_deref() {
            Some("function_call") | Some("custom_tool_call") => {
                let name = p.name.as_deref().unwrap_or_default();
                // `arguments` — JSON-строка; `input` у `custom_tool_call` — сырой текст
                let args: Value = match &p.arguments {
                    Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
                    Some(v) => v.clone(),
                    None => Value::Null,
                };
                if name == "apply_patch" {
                    let patch = p
                        .input
                        .as_ref()
                        .and_then(Value::as_str)
                        .or_else(|| args.get("input").and_then(Value::as_str));
                    if let Some(patch) = patch {
                        self.files(patch);
                    }
                } else if SHELL_TOOLS.contains(&name) {
                    let cmd = args.get("command").or_else(|| args.get("cmd"));
                    if let Some(c) = cmd.and_then(command_text) {
                        self.command(c, p.call_id);
                    }
                }
            }
            Some("local_shell_call") => {
                let cmd = p.action.as_ref().and_then(|a| a.get("command"));
                if let Some(c) = cmd.and_then(command_text) {
                    self.command(c, p.call_id);
                }
            }
            Some("function_call_output") | Some("custom_tool_call_output") => {
                let i = p.call_id.as_ref().and_then(|id| self.pending.remove(id));
                if let (Some(i), Some(code)) = (i, p.output.as_ref().and_then(exit_failed)) {
                    self.data.commands[i].failed = Some(code);
                }
            }
            _ => {}
        }
    }
}

impl TranscriptAdapter for CodexAdapter {
    fn accept_line(&mut self, line: &str) {
        let l: Line = match serde_json::from_str(line) {
            Ok(l) => l,
            Err(_) => {
                self.skipped += 1;
                return;
            }
        };
        let Some(p) = l.payload else { return };
        match l.kind.as_deref() {
            Some("session_meta") => {
                self.hint = p.id.as_ref().and_then(Value::as_str).map(str::to_string);
                if self.cwd.is_none() {
                    self.cwd = p.cwd.as_ref().and_then(Value::as_str).map(PathBuf::from);
                }
            }
            // `cwd` хода: последний в окне — для путей патчей (сессия могла сменить каталог)
            Some("turn_context") => {
                if let Some(c) = p.cwd.as_ref().and_then(Value::as_str) {
                    self.cwd = Some(PathBuf::from(c));
                }
            }
            Some("compacted") => {
                if let Some(m) = p.message.as_ref().and_then(Value::as_str) {
                    let m = m.trim();
                    if !m.is_empty() {
                        self.data.compact_summary = Some(m.to_string());
                    }
                }
            }
            Some("event_msg") if p.kind.as_deref() == Some("user_message") => {
                if let Some(m) = p.message.as_ref().and_then(Value::as_str) {
                    let m = strip_tags(m, &INSERT_TAGS);
                    if !m.is_empty() {
                        self.data.prompts.push(m);
                    }
                }
            }
            Some("response_item") => self.response_item(p),
            _ => {}
        }
    }

    fn skipped(&self) -> u32 {
        self.skipped
    }

    fn session_hint(&self) -> Option<String> {
        self.hint.clone()
    }

    fn finish(self: Box<Self>) -> SnapshotData {
        self.data
    }
}

/// Первая строка файла для адаптера (она вне окна хвоста): только `session_meta`. Целая строка
/// проверяется разбором; начало длинной строки (`cut`) — по признаку типа в начале, а `id` и
/// `cwd` достаёт [`meta_from_prefix`] и собирает из них короткую запись `session_meta`.
pub fn head_line(head: &str, cut: bool) -> Option<String> {
    if !cut {
        let l: Line = serde_json::from_str(head).ok()?;
        return (l.kind.as_deref() == Some("session_meta")).then(|| head.to_string());
    }
    let (id, cwd) = meta_from_prefix(head)?;
    let mut payload = serde_json::Map::new();
    payload.insert("id".into(), Value::String(id));
    if let Some(c) = cwd {
        payload.insert("cwd".into(), Value::String(c));
    }
    Some(serde_json::json!({"type": "session_meta", "payload": payload}).to_string())
}

/// `payload.id` и `payload.cwd` из начала строки `session_meta`, обрезанной до разбора
/// (`instructions`/`base_instructions` — AGENTS.md — длиннее предела чтения). Разбор
/// ограниченный, без полного JSON: тип — `"type":"session_meta"` до `"payload"`, поля ищутся
/// после `"payload":{` и до первого `"instructions"`/`"base_instructions"` (в `SessionMeta` Codex
/// они идут раньше); значение — JSON-строка до закрывающей неэкранированной кавычки.
pub fn meta_from_prefix(prefix: &str) -> Option<(String, Option<String>)> {
    let p = prefix.find("\"payload\"")?;
    if !prefix[..p].contains("\"type\":\"session_meta\"") {
        return None;
    }
    let body = &prefix[p..];
    let stop = ["\"instructions\"", "\"base_instructions\""]
        .iter()
        .filter_map(|k| body.find(k))
        .min()
        .unwrap_or(body.len());
    let body = &body[..stop];
    let field = |name: &str| -> Option<String> {
        let key = format!("\"{name}\":");
        let mut from = 0;
        while let Some(i) = body[from..].find(&key) {
            let at = from + i;
            from = at + key.len();
            // ключ — не внутри строки: перед ним `{` или `,` (без пробелов, как у serde_json)
            if !matches!(body[..at].chars().next_back(), Some('{' | ',')) {
                continue;
            }
            let rest = body[from..].trim_start();
            if !rest.starts_with('"') {
                return None;
            }
            let bytes = rest.as_bytes();
            let mut j = 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 2,
                    b'"' => return serde_json::from_str::<String>(&rest[..=j]).ok(),
                    _ => j += 1,
                }
            }
            return None;
        }
        None
    };
    Some((field("id")?, field("cwd")))
}

/// Текст команды: строка или массив аргументов; `bash -lc "<скрипт>"` → скрипт.
fn command_text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let argv: Vec<&str> = v.as_array()?.iter().filter_map(Value::as_str).collect();
    let shell = |s: &str| {
        let base = s.rsplit(['/', '\\']).next().unwrap_or(s);
        matches!(
            base.trim_end_matches(".exe"),
            "bash" | "sh" | "zsh" | "pwsh" | "powershell" | "cmd"
        )
    };
    match argv.as_slice() {
        [sh, flag, script, ..] if shell(sh) && flag.starts_with(['-', '/']) => {
            Some(script.to_string())
        }
        [] => None,
        _ => Some(argv.join(" ")),
    }
}

/// Исход по выводу инструмента: `metadata.exit_code` (JSON — строкой или объектом),
/// `success`, либо строка «Exit code: N» / «Process exited with code N». `None` — неизвестно.
fn exit_failed(out: &Value) -> Option<bool> {
    let obj = match out {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) if v.is_object() => v,
            _ => return exit_from_text(s),
        },
        v => v.clone(),
    };
    if let Some(code) = obj.pointer("/metadata/exit_code").and_then(Value::as_i64) {
        return Some(code != 0);
    }
    if let Some(ok) = obj.get("success").and_then(Value::as_bool) {
        return Some(!ok);
    }
    obj.get("output")
        .or_else(|| obj.get("content"))
        .and_then(Value::as_str)
        .and_then(exit_from_text)
}

fn exit_from_text(s: &str) -> Option<bool> {
    s.lines().take(4).find_map(|l| {
        let l = l.trim();
        let n = l
            .strip_prefix("Exit code:")
            .or_else(|| l.strip_prefix("Process exited with code"))?;
        n.trim().parse::<i64>().ok().map(|c| c != 0)
    })
}
