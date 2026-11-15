//! Адаптер журнала Claude Code (`~/.claude/projects/<slug>/<uuid>.jsonl`), §8а. Формат сверен
//! с живым журналом версии 2.1.314 (задача 17a, шаг 0; журнал этапа — «Сверка форматов §8а»):
//! - конспект — запись `type: "user"` с `isCompactSummary: true` сразу после
//!   `{"type":"system","subtype":"compact_boundary"}`; обёртка Claude (первая строка «This session
//!   is being continued…», хвостовые указания со ссылкой на журнал) снимается. Записей
//!   `type: "summary"` в 2.1.314 нет (заголовок — `ai-title`); по документации это заголовок
//!   `/resume`, не конспект — не берутся;
//! - запрос человека — `type: "user"` с `message.content` строкой или блоками `text`, без
//!   `isMeta`, `isSidechain`, `isCompactSummary`, `toolUseResult` и блоков `tool_result`; если
//!   есть `origin.kind`, то только `human` (уведомления фоновых задач — `task-notification`,
//!   сообщения субагентов — `peer`). Вставки (`<bash-…>`, `<command-…>`, `<system-reminder>`,
//!   `<task-notification>`, вывод хуков, «[Request interrupted by user…]») вырезаются;
//! - команда — `tool_use` `Bash` или `PowerShell` (Windows), `input.command`; исход — `is_error`
//!   блока `tool_result` с тем же `tool_use_id`;
//! - файл — `tool_use` `Edit`/`Write`/`MultiEdit` (`input.file_path`), `NotebookEdit`
//!   (`input.notebook_path`, по документации; `file_path` тоже принимается);
//! - сессия — `sessionId` записи (совпадает с именем файла и `session_id` входа хука; поле
//!   `session_id` записи — другой id, не используется). Чужая сессия пропускается; без
//!   известной сессии берётся последняя сессия файла (смена `sessionId` сбрасывает собранное).
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_json::Value;
use zam_core::snapshot::{SnapCommand, SnapshotData};
use zam_core::toolpath::glob_path;

use super::{TranscriptAdapter, strip_tags};

/// Признаки строк, которые стоит разбирать (§8а).
pub const PREFILTER: [&str; 4] = [
    "\"isCompactSummary\"",
    "\"compact_boundary\"",
    "\"type\":\"user\"",
    "\"tool_use\"",
];

/// Вставки клиента и хуков в тексте запроса — не слова человека.
const INSERT_TAGS: [&str; 13] = [
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
    "command-name",
    "command-message",
    "command-args",
    "system-reminder",
    "user-prompt-submit-hook",
    "task-notification",
    "user_instructions",
];

/// Обёртка конспекта сжатия Claude Code (живой журнал 2.1.314).
const WRAP_HEAD: &str = "This session is being continued from a previous conversation";
const WRAP_TAIL: [&str; 3] = [
    "If you need specific details from before compaction",
    "Continue the conversation from where it left off",
    "The messages after this summary are the most recent messages",
];

#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
    #[serde(rename = "sessionId")]
    session: Option<String>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(rename = "isSidechain")]
    sidechain: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    compact: Option<bool>,
    #[serde(rename = "toolUseResult")]
    tool_result: Option<IgnoredAny>,
    origin: Option<Origin>,
    cwd: Option<String>,
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Origin {
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Msg {
    content: Option<Content>,
}

/// `message.content`: строка или блоки. Своя десериализация — без буфера `untagged`
/// (содержимое `tool_result` пропускается, не копируясь).
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Content;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("строка или массив блоков")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Content, E> {
                Ok(Content::Text(v.to_string()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Content, E> {
                Ok(Content::Text(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Content, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = a.next_element::<Block>()? {
                    out.push(b);
                }
                Ok(Content::Blocks(out))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<Value>,
    id: Option<String>,
    name: Option<String>,
    input: Option<ToolInput>,
    tool_use_id: Option<String>,
    is_error: Option<bool>,
}

#[derive(Deserialize)]
struct ToolInput {
    command: Option<Value>,
    file_path: Option<Value>,
    notebook_path: Option<Value>,
}

/// Адаптер журнала Claude Code.
pub struct ClaudeAdapter {
    root: PathBuf,
    session: String,
    current: Option<String>,
    skipped: u32,
    after_boundary: bool,
    data: SnapshotData,
    /// `tool_use.id` → индекс команды (исход приходит позже, в `tool_result`).
    pending: HashMap<String, usize>,
}

impl ClaudeAdapter {
    pub fn new(root: &Path, session: &str) -> Self {
        ClaudeAdapter {
            root: root.to_path_buf(),
            session: session.to_string(),
            current: None,
            skipped: 0,
            after_boundary: false,
            data: SnapshotData::default(),
            pending: HashMap::new(),
        }
    }

    /// Запись своей сессии? Без известной сессии смена `sessionId` сбрасывает собранное.
    fn own(&mut self, sid: Option<&str>) -> bool {
        let Some(sid) = sid else { return true };
        if !self.session.is_empty() {
            return sid == self.session;
        }
        if self.current.as_deref() != Some(sid) {
            if self.current.is_some() {
                self.data = SnapshotData::default();
                self.pending.clear();
                self.after_boundary = false;
            }
            self.current = Some(sid.to_string());
        }
        true
    }

    fn user(&mut self, r: Rec) {
        let content = r.message.and_then(|m| m.content);
        if r.compact == Some(true) {
            if self.after_boundary
                && let Some(t) = content.as_ref().map(text_of)
            {
                let t = unwrap_summary(&t);
                if !t.is_empty() {
                    self.data.compact_summary = Some(t);
                }
            }
            self.after_boundary = false;
            return;
        }
        self.after_boundary = false;
        if let Some(Content::Blocks(blocks)) = &content {
            let mut results = false;
            for b in blocks {
                if b.kind.as_deref() == Some("tool_result") {
                    results = true;
                    if let Some(i) = b
                        .tool_use_id
                        .as_ref()
                        .and_then(|id| self.pending.remove(id))
                    {
                        self.data.commands[i].failed = Some(b.is_error.unwrap_or(false));
                    }
                }
            }
            if results {
                return;
            }
        }
        if r.tool_result.is_some() || r.is_meta == Some(true) {
            return;
        }
        if r.origin
            .as_ref()
            .and_then(|o| o.kind.as_deref())
            .is_some_and(|k| k != "human")
        {
            return;
        }
        let Some(content) = content else { return };
        let mut text = strip_tags(&text_of(&content), &INSERT_TAGS);
        text = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("[Request interrupted by user"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !text.is_empty() {
            self.data.prompts.push(text);
        }
    }

    fn assistant(&mut self, r: Rec) {
        self.after_boundary = false;
        let Some(Content::Blocks(blocks)) = r.message.and_then(|m| m.content) else {
            return;
        };
        let cwd = r
            .cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.clone());
        for b in blocks {
            if b.kind.as_deref() != Some("tool_use") {
                continue;
            }
            let (Some(name), Some(input)) = (b.name.as_deref(), b.input) else {
                continue;
            };
            match name {
                "Bash" | "PowerShell" => {
                    if let Some(cmd) = input.command.as_ref().and_then(Value::as_str) {
                        if let Some(id) = b.id {
                            self.pending.insert(id, self.data.commands.len());
                        }
                        self.data.commands.push(SnapCommand {
                            cmd: cmd.to_string(),
                            failed: None,
                        });
                    }
                }
                "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                    let p = input
                        .file_path
                        .as_ref()
                        .or(input.notebook_path.as_ref())
                        .and_then(Value::as_str);
                    if let Some(rel) = p.and_then(|p| glob_path(p, &cwd, &self.root)) {
                        self.data.files.push(rel);
                    }
                }
                _ => {}
            }
        }
    }
}

impl TranscriptAdapter for ClaudeAdapter {
    fn accept_line(&mut self, line: &str) {
        let r: Rec = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => {
                self.skipped += 1;
                return;
            }
        };
        if r.sidechain == Some(true) || !self.own(r.session.as_deref()) {
            return;
        }
        match r.kind.as_deref() {
            Some("system") if r.subtype.as_deref() == Some("compact_boundary") => {
                self.after_boundary = true;
            }
            Some("user") => self.user(r),
            Some("assistant") => self.assistant(r),
            _ => {}
        }
    }

    fn skipped(&self) -> u32 {
        self.skipped
    }

    fn session_hint(&self) -> Option<String> {
        if self.session.is_empty() {
            self.current.clone()
        } else {
            Some(self.session.clone())
        }
    }

    fn finish(self: Box<Self>) -> SnapshotData {
        self.data
    }
}

/// Текст содержимого: строка или блоки `text` через перевод строки (картинки и прочее — мимо).
fn text_of(c: &Content) -> String {
    match c {
        Content::Text(s) => s.clone(),
        Content::Blocks(bs) => bs
            .iter()
            .filter(|b| b.kind.as_deref() == Some("text"))
            .filter_map(|b| b.text.as_ref().and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Конспект без обёртки Claude Code: первая строка «This session is being continued…»,
/// строка «Summary:» и хвостовые указания (ссылка на полный журнал, «Continue…»).
fn unwrap_summary(t: &str) -> String {
    t.lines()
        .filter(|l| {
            let l = l.trim();
            !(l.starts_with(WRAP_HEAD)
                || l == "Summary:"
                || WRAP_TAIL.iter().any(|w| l.starts_with(w)))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}
