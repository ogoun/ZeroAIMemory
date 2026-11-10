//! Нейтральный снимок сессии (спецификация M6 §8а): единственное место его типов. Клиент `zam`
//! собирает снимок из журнала агента адаптерами, хранилище пишет его в `session_snapshots`,
//! сервер показывает. Форматы агентов сюда не попадают.
use serde::{Deserialize, Serialize};

/// Полнота снимка: `partial` — журнал прочитан не весь (срок, предел размера, пропуски).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapStatus {
    Complete,
    Partial,
}

impl SnapStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SnapStatus::Complete => "complete",
            SnapStatus::Partial => "partial",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [SnapStatus::Complete, SnapStatus::Partial]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// Повод снимка: событие хука агента, догоняющая отправка из очереди или явная команда.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapTrigger {
    PostCompact,
    PreCompact,
    SessionStartCompact,
    Stop,
    SessionEnd,
    CatchUp,
    Manual,
}

impl SnapTrigger {
    /// Все поводы.
    pub const ALL: [SnapTrigger; 7] = [
        SnapTrigger::PostCompact,
        SnapTrigger::PreCompact,
        SnapTrigger::SessionStartCompact,
        SnapTrigger::Stop,
        SnapTrigger::SessionEnd,
        SnapTrigger::CatchUp,
        SnapTrigger::Manual,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SnapTrigger::PostCompact => "post_compact",
            SnapTrigger::PreCompact => "pre_compact",
            SnapTrigger::SessionStartCompact => "session_start_compact",
            SnapTrigger::Stop => "stop",
            SnapTrigger::SessionEnd => "session_end",
            SnapTrigger::CatchUp => "catch_up",
            SnapTrigger::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Пределы полей снимка (§8а, «Пределы»): клиент собирает снимок в них, сервер усекает
/// присланное сверх них (остаются последние элементы и хвост конспекта).
pub mod limits {
    /// Запросов человека.
    pub const PROMPTS_MAX: usize = 5;
    /// Символов запроса.
    pub const PROMPT_CHARS: usize = 500;
    /// Изменённых файлов.
    pub const FILES_MAX: usize = 20;
    /// Символов пути файла.
    pub const FILE_CHARS: usize = 512;
    /// Команд.
    pub const COMMANDS_MAX: usize = 10;
    /// Символов команды.
    pub const COMMAND_CHARS: usize = 200;
    /// Байт конспекта (хвост).
    pub const SUMMARY_BYTES: usize = 8 * 1024;
}

/// Форматы журналов агентов (поле `agent` снимка): адаптеры клиента `zam`.
pub const AGENT_FORMATS: [&str; 2] = ["claude", "codex"];

/// Команда из журнала агента; `failed` — исход, если журнал его знает.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapCommand {
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
}

/// Содержимое снимка (JSON `{compact_summary?, prompts[], files[], commands[{cmd, failed?}]}`):
/// хвост конспекта сжатия, последние запросы человека, изменённые файлы, команды.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_summary: Option<String>,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub commands: Vec<SnapCommand>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Имена в JSON совпадают с `as_str` и читаются обратно `parse`.
    #[test]
    fn names_round_trip() {
        for t in SnapTrigger::ALL {
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
            assert_eq!(SnapTrigger::parse(t.as_str()), Some(t));
        }
        assert_eq!(SnapTrigger::parse("compact"), None);
        for s in [SnapStatus::Complete, SnapStatus::Partial] {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            assert_eq!(SnapStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(SnapStatus::parse("done"), None);
    }

    /// Необязательное опускается, отсутствующие массивы читаются пустыми.
    #[test]
    fn data_json_shape() {
        let d = SnapshotData {
            compact_summary: None,
            prompts: vec!["p".into()],
            files: Vec::new(),
            commands: vec![
                SnapCommand {
                    cmd: "a".into(),
                    failed: Some(false),
                },
                SnapCommand {
                    cmd: "b".into(),
                    failed: None,
                },
            ],
        };
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(
            json,
            r#"{"prompts":["p"],"files":[],"commands":[{"cmd":"a","failed":false},{"cmd":"b"}]}"#
        );
        assert_eq!(serde_json::from_str::<SnapshotData>(&json).unwrap(), d);
        let empty: SnapshotData = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, SnapshotData::default());
    }
}
