//! Записи журналов решения (§7.2–7.5 ТЗ): Changelog, Releases и Insights в таблице
//! `entries`, их ревизии (`entry_revisions`, только дописываются) и синхронная индексация.
//!
//! Структура модуля:
//! - `mod.rs` — типы и отображение строк `entries`;
//! - [`write`] — создание (с `Idempotency-Key`), правка с ревизией, мягкое удаление и
//!   восстановление, замещение (`supersede`) и отзыв (`retract`) — каждая операция одной
//!   транзакцией писателя;
//! - [`read`] — запись по id, лента с курсором и фильтрами, ревизии;
//! - [`chunks`] — чанки записи (документ `# <title>\n\n<body>` → `chunk_note`), FTS и задания
//!   индексации (`target_kind = 'entry'`) в той же транзакции; данные для воркера и reindex.
//!
//! Хранилище проверяет только целостность: статус есть только у insight; переходы
//! `active ↔ revisit` — правкой, `→ superseded` — замещением, `→ retracted` — отзывом с
//! непустой причиной; из `superseded` и `retracted` обратно не возвращаются. Права на
//! `certainty` (кто и когда может ставить `axiom`/`theorem`) — [`crate::certainty`]: каждая
//! операция записи проверяет их в своей транзакции.

mod chunks;
mod read;
mod triggers;
mod write;

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use zam_core::Id;

use crate::StoreError;
use crate::jobs::IndexState;
use crate::nodes::AuthorKind;
use crate::time::{conv_err, ts_at, ts_opt};

pub use chunks::{
    ReindexEntry, enqueue_entry_upserts, entry_chunks_for_index, finish_entries_reindexed,
    reindex_entries_page,
};
pub(crate) use chunks::{rechunk_entries, rescan_entry_secrets, set_entry_state};
pub use read::{
    CHANGELOG_BETWEEN_MAX, DEDUP_CANDIDATES_MAX, DedupCandidate, changelog_between,
    entry_revisions, find_release_by_version, get_entry, list_entries, title_duplicates,
};
pub use triggers::{
    KEYWORD_MAX_CHARS, NewTrigger, PATTERN_MAX_CHARS, TOOLS_MAX, TRIGGERS_MAX, Trigger,
    TriggerKind, TriggerRow, active_triggers, entry_triggers, set_triggers, validate_trigger,
};
pub(crate) use triggers::{check_triggers, replace_triggers, same_triggers};
pub use write::{
    create_entry, delete_entry, entry_by_idempotency_key, restore_entry, retract_entry,
    supersede_entry, update_entry,
};

/// Строковое перечисление с отображением в SQL (`TEXT`) и serde (нижний регистр).
macro_rules! text_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name {
            $($var),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$var => $s),+
                }
            }

            /// Значение по строке; неизвестная — `None`.
            pub fn parse(s: &str) -> Option<$name> {
                match s {
                    $($s => Some($name::$var),)+
                    _ => None,
                }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }

        impl FromSql for $name {
            fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = v.as_str()?;
                $name::parse(s).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(
                        format!(concat!("неизвестное значение ", stringify!($name), ": {}"), s)
                            .into(),
                    )
                })
            }
        }
    };
}

text_enum!(
    /// Вид записи журнала.
    EntryKind {
        Changelog => "changelog",
        Release => "release",
        Insight => "insight",
    }
);

text_enum!(
    /// Статус insight (§7.5): жизненный цикл записи.
    EntryStatus {
        Active => "active",
        Revisit => "revisit",
        Superseded => "superseded",
        Retracted => "retracted",
    }
);

text_enum!(
    /// Эпистемический статус (§7.5): насколько записи можно доверять. Правила —
    /// [`crate::certainty`].
    Certainty {
        Axiom => "axiom",
        Theorem => "theorem",
        Hypothesis => "hypothesis",
    }
);

/// Предел длины заголовка, символов. [решение исполнителя]
pub const TITLE_MAX_CHARS: usize = 500;
/// Предел длины области (как у заголовка `X-Zam-Area`), символов.
pub const AREA_MAX_CHARS: usize = 128;
/// Предел числа тегов и длины тега, символов. [решение исполнителя]
pub const TAGS_MAX: usize = 50;
pub const TAG_MAX_CHARS: usize = 100;
/// Предел длины `Idempotency-Key` и причины, символов. [решение исполнителя]
pub const KEY_MAX_CHARS: usize = 200;
pub const REASON_MAX_CHARS: usize = 2000;
/// Предел страницы ленты.
pub const LIST_LIMIT_MAX: usize = 200;

/// Автор записи или ревизии.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorView {
    pub kind: AuthorKind,
    pub user_id: Option<Id>,
    pub token_id: Option<Id>,
    pub session_id: Option<String>,
    pub machine: Option<String>,
}

/// Новая запись журнала.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub space_id: Id,
    pub kind: EntryKind,
    pub title: String,
    pub body: String,
    /// JSON-объект по виду записи (`kind` changelog/insight, `version` релиза, `evidence`, …).
    pub data: serde_json::Value,
    /// `None` — область из атрибуции (`X-Zam-Area`).
    pub area: Option<String>,
    pub tags: Vec<String>,
    /// Только у insight; `None` у insight — `active`. При создании — `active` или `revisit`.
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    pub idempotency_key: Option<String>,
    /// Триггеры вспоминания (только у insight); пишутся в той же транзакции.
    pub triggers: Vec<NewTrigger>,
    /// Явные связи записи (`links` insight, §8.5): `(тип, цель)`, цель — живая заметка или
    /// запись этой базы; пишутся в той же транзакции (нет цели → `NotFound`, `supersedes` →
    /// `BadInput`). Область видимости цели проверяет сервер.
    pub links: Vec<(crate::graph::EdgeType, zam_core::TargetRef)>,
}

/// Запись журнала.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub id: Id,
    pub space_id: Id,
    pub kind: EntryKind,
    pub title: String,
    pub body: String,
    pub data: serde_json::Value,
    pub tags: Vec<String>,
    pub area: Option<String>,
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    pub supersedes_id: Option<Id>,
    pub superseded_by_id: Option<Id>,
    pub retract_reason: Option<String>,
    /// Текущая ревизия (`entries.current_rev`).
    pub rev: i64,
    /// Автор записи (создатель).
    pub author: AuthorView,
    pub index_state: IndexState,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub updated_at: Timestamp,
    #[serde(serialize_with = "crate::time::ser_ts_opt")]
    pub deleted_at: Option<Timestamp>,
}

/// Ревизия записи: полный снимок полей и автор правки.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntryRevision {
    pub rev: i64,
    pub title: String,
    pub body: String,
    pub data: serde_json::Value,
    pub tags: Vec<String>,
    pub area: Option<String>,
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    /// Причина смены статуса или `certainty`.
    pub reason: Option<String>,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub ts: Timestamp,
    pub author: AuthorView,
}

/// Правка записи: `None` — поле не меняется.
#[derive(Debug, Clone, Default)]
pub struct EntryPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    /// Дописать к телу через пустую строку (вместе с `body` — `BadInput`).
    pub append_body: Option<String>,
    pub data: Option<serde_json::Value>,
    pub tags: Option<Vec<String>>,
    /// `Some(None)` — снять область.
    pub area: Option<Option<String>>,
    /// Только `active` или `revisit` и только у insight.
    pub status: Option<EntryStatus>,
    /// Только у insight; `Some(None)` — снять статус. Смена — по матрице прав
    /// [`crate::check_certainty`] с причиной `reason`.
    pub certainty: Option<Option<Certainty>>,
    /// Причина смены статуса или `certainty` (в ревизию).
    pub reason: Option<String>,
    /// Заменить набор триггеров (только у insight); ревизии не создаёт.
    pub triggers: Option<Vec<NewTrigger>>,
}

/// Фильтр ленты записей пространства.
#[derive(Debug, Clone)]
pub struct EntryFilter {
    pub space_id: Id,
    pub kind: Option<EntryKind>,
    /// Пусто — любые статусы (и записи без статуса).
    pub statuses: Vec<EntryStatus>,
    /// Пусто — без ограничения.
    pub certainty: Vec<Certainty>,
    pub area: Option<String>,
    pub tag: Option<String>,
    /// `created_at >= since`.
    pub since: Option<Timestamp>,
    /// `created_at < until`.
    pub until: Option<Timestamp>,
    /// Курсор следующей страницы из прошлого ответа.
    pub cursor: Option<String>,
    /// 1..=200.
    pub limit: usize,
}

/// Нормализованный заголовок (дедупликация, §7.5): нижний регистр, всё, что не буква и не
/// цифра, — пробел, пробелы схлопнуты, по краям обрезаны.
pub fn normalize_title(s: &str) -> String {
    let mapped: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Область для сравнения: без пробелов по краям, пустая — `None` (как при записи).
pub fn normalize_area(a: Option<&str>) -> Option<String> {
    a.map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

/// Колонки записи для [`entry_row`]; таблица `entries` под псевдонимом `e`.
pub(crate) const ENTRY_COLS: &str = "e.id, e.space_id, e.kind, e.title, e.body, e.data, e.tags,
    e.area, e.status, e.certainty, e.supersedes_id, e.superseded_by_id, e.retract_reason,
    e.current_rev, e.author_kind, e.user_id, e.token_id, e.session_id, e.machine,
    e.index_state, e.created_at, e.updated_at, e.deleted_at";

/// JSON-колонка `i`.
pub(crate) fn json_col<T: serde::de::DeserializeOwned>(r: &Row, i: usize) -> rusqlite::Result<T> {
    let s: String = r.get(i)?;
    serde_json::from_str(&s).map_err(|e| conv_err(i, e))
}

pub(crate) fn entry_row(r: &Row) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: r.get(0)?,
        space_id: r.get(1)?,
        kind: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        data: json_col(r, 5)?,
        tags: json_col(r, 6)?,
        area: r.get(7)?,
        status: r.get(8)?,
        certainty: r.get(9)?,
        supersedes_id: r.get(10)?,
        superseded_by_id: r.get(11)?,
        retract_reason: r.get(12)?,
        rev: r.get(13)?,
        author: AuthorView {
            kind: r.get(14)?,
            user_id: r.get(15)?,
            token_id: r.get(16)?,
            session_id: r.get(17)?,
            machine: r.get(18)?,
        },
        index_state: r.get(19)?,
        created_at: ts_at(r, 20)?,
        updated_at: ts_at(r, 21)?,
        deleted_at: ts_opt(r, 22)?,
    })
}

/// Запись по id (в том числе удалённая); нет → `NotFound`.
pub(crate) fn load_entry(c: &Connection, id: Id) -> Result<Entry, StoreError> {
    c.prepare_cached(&format!(
        "SELECT {ENTRY_COLS} FROM entries e WHERE e.id = ?1"
    ))?
    .query_row([id], entry_row)
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// Вид инсайта (`data.kind`, строка), если есть.
pub(crate) fn data_kind(data: &serde_json::Value) -> Option<String> {
    data.get("kind")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_title_rules() {
        assert_eq!(
            normalize_title("  Bash-скрипты   должны иметь LF!  "),
            "bash скрипты должны иметь lf"
        );
        assert_eq!(normalize_title("A.B,C;(d)"), "a b c d");
        assert_eq!(normalize_title("—"), "");
        assert_eq!(normalize_title("Ёж\tИ\nЛис"), "ёж и лис");
    }

    #[test]
    fn normalize_title_cases() {
        // регистр (в том числе кириллица и немецкая ß → ss нет: to_lowercase посимвольно)
        assert_eq!(
            normalize_title("CRLF в Bash-Скриптах"),
            normalize_title("crlf в bash скриптах")
        );
        // пунктуация, кавычки и тире — разделители; повторы пробелов схлопываются
        assert_eq!(
            normalize_title("«Bash» — скрипты:  LF!!!"),
            normalize_title("bash скрипты lf")
        );
        // цифры и подчёркивание: цифры значимы, `_` — разделитель
        assert_eq!(normalize_title("Python_3.12 venv"), "python 3 12 venv");
        assert_ne!(normalize_title("Ошибка 1"), normalize_title("Ошибка 2"));
        // заголовок без букв и цифр — пусто (дублей у него нет)
        assert_eq!(normalize_title("?!… —"), "");
    }

    #[test]
    fn enums_round_trip() {
        for k in [EntryKind::Changelog, EntryKind::Release, EntryKind::Insight] {
            assert_eq!(EntryKind::parse(k.as_str()), Some(k));
            assert_eq!(
                serde_json::to_string(&k).unwrap(),
                format!("\"{}\"", k.as_str())
            );
        }
        assert_eq!(Certainty::parse("axiom"), Some(Certainty::Axiom));
        assert_eq!(EntryStatus::parse("Active"), None);
        assert_eq!(
            serde_json::from_str::<Certainty>("\"hypothesis\"").unwrap(),
            Certainty::Hypothesis
        );
    }
}
