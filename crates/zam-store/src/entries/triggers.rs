//! Триггеры проактивного вспоминания (§10.3 ТЗ) у insight: маска файла (`file_glob`),
//! regex команды (`command_regex`) или ошибки (`error_regex`), ключевое слово (`keyword`).
//!
//! - Проверка ([`validate_trigger`]): маска — `globset::Glob`; regex — `regex::RegexBuilder` с
//!   пределом размера 1 МиБ (диалект без обратных ссылок и lookaround: сопоставление за
//!   линейное время, ReDoS невозможен); ключевое слово — 1..=200 символов; шаблон — до 1000
//!   символов; инструментов — до 16. Сопоставление выполняет CLI (M4).
//! - Набор триггеров записи заменяется целиком ([`set_triggers`]; не больше 32): триггеры —
//!   настройка показа, а не история, поэтому прежние строки удаляются
//!   **[решение исполнителя]**.
//! - Выдача для кэша клиента ([`active_triggers`]) — только включённые триггеры живых insight
//!   со статусом `active` или `revisit`.

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use zam_core::Id;

use super::{Certainty, EntryKind, load_entry};
use crate::time::{fmt_ts, ts_at};
use crate::{Db, StoreError};

/// Предел триггеров одной записи.
pub const TRIGGERS_MAX: usize = 32;
/// Предел длины шаблона, символов.
pub const PATTERN_MAX_CHARS: usize = 1000;
/// Предел длины ключевого слова, символов.
pub const KEYWORD_MAX_CHARS: usize = 200;
/// Предел числа инструментов триггера.
pub const TOOLS_MAX: usize = 16;
/// Предел длины имени инструмента, символов. [решение исполнителя]
pub const TOOL_MAX_CHARS: usize = 64;
/// Предел размера скомпилированного regex (`RegexBuilder::size_limit`), байт.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Вид триггера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    FileGlob,
    CommandRegex,
    ErrorRegex,
    Keyword,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::FileGlob => "file_glob",
            TriggerKind::CommandRegex => "command_regex",
            TriggerKind::ErrorRegex => "error_regex",
            TriggerKind::Keyword => "keyword",
        }
    }

    /// Вид по строке; неизвестный — `None`.
    pub fn parse(s: &str) -> Option<TriggerKind> {
        match s {
            "file_glob" => Some(TriggerKind::FileGlob),
            "command_regex" => Some(TriggerKind::CommandRegex),
            "error_regex" => Some(TriggerKind::ErrorRegex),
            "keyword" => Some(TriggerKind::Keyword),
            _ => None,
        }
    }
}

impl ToSql for TriggerKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for TriggerKind {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = v.as_str()?;
        TriggerKind::parse(s).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(format!("неизвестный вид триггера: {s}").into())
        })
    }
}

/// Новый триггер (JSON: `{type, pattern, tools?}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTrigger {
    #[serde(rename = "type")]
    pub kind: TriggerKind,
    pub pattern: String,
    /// Инструменты агента, на которые действует триггер (`Write`, `Edit`, `Bash`); `None` —
    /// любые.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

/// Триггер записи.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trigger {
    pub id: Id,
    #[serde(rename = "type")]
    pub kind: TriggerKind,
    pub pattern: String,
    pub tools: Option<Vec<String>>,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
}

/// Действующий триггер для кэша клиента: триггер и поля его insight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRow {
    pub id: Id,
    pub entry_id: Id,
    pub kind: TriggerKind,
    pub pattern: String,
    pub tools: Option<Vec<String>>,
    pub title: String,
    /// `data.kind` записи (пусто, если не задан).
    pub insight_kind: String,
    pub certainty: Option<Certainty>,
    pub space_id: Id,
}

/// Строка печатных символов длиной `1..=max` (после обрезки пробелов по краям).
fn printable(s: &str, max: usize) -> bool {
    let n = s.chars().count();
    (1..=max).contains(&n) && !s.chars().any(char::is_control)
}

/// Проверка триггера (см. модуль); текст ошибки — для ответа 422.
pub fn validate_trigger(t: &NewTrigger) -> Result<(), String> {
    let p = t.pattern.as_str();
    if p.trim().is_empty() {
        return Err("pattern: пустой шаблон".into());
    }
    if p.chars().count() > PATTERN_MAX_CHARS {
        return Err(format!("pattern: длиннее {PATTERN_MAX_CHARS} символов"));
    }
    match t.kind {
        TriggerKind::FileGlob => {
            globset::Glob::new(p).map_err(|e| format!("pattern: неверная маска file_glob: {e}"))?;
        }
        TriggerKind::CommandRegex | TriggerKind::ErrorRegex => {
            regex::RegexBuilder::new(p)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map_err(|e| {
                    format!(
                        "pattern: неверный {} (диалект regex без обратных ссылок и \
                         lookaround, до 1 МиБ): {e}",
                        t.kind.as_str()
                    )
                })?;
        }
        TriggerKind::Keyword => {
            if !printable(p.trim(), KEYWORD_MAX_CHARS) {
                return Err(format!(
                    "pattern: ключевое слово — от 1 до {KEYWORD_MAX_CHARS} печатных символов"
                ));
            }
        }
    }
    if let Some(tools) = &t.tools {
        if tools.len() > TOOLS_MAX {
            return Err(format!("tools: не больше {TOOLS_MAX}"));
        }
        if let Some(bad) = tools.iter().find(|x| !printable(x.trim(), TOOL_MAX_CHARS)) {
            return Err(format!(
                "tools: «{bad}» — имя инструмента от 1 до {TOOL_MAX_CHARS} печатных символов"
            ));
        }
    }
    Ok(())
}

/// Набор триггеров: каждый проверен (`triggers[i]: …`), не больше [`TRIGGERS_MAX`];
/// ключевое слово и имена инструментов — без пробелов по краям, пустой список инструментов
/// — `None`.
pub(crate) fn check_triggers(ts: Vec<NewTrigger>) -> Result<Vec<NewTrigger>, StoreError> {
    if ts.len() > TRIGGERS_MAX {
        return Err(StoreError::BadInput(format!(
            "triggers: не больше {TRIGGERS_MAX} на запись"
        )));
    }
    ts.into_iter()
        .enumerate()
        .map(|(i, mut t)| {
            validate_trigger(&t)
                .map_err(|e| StoreError::BadInput(format!("triggers[{i}]: {e}")))?;
            if t.kind == TriggerKind::Keyword {
                t.pattern = t.pattern.trim().to_string();
            }
            t.tools = t
                .tools
                .map(|v| {
                    v.into_iter()
                        .map(|x| x.trim().to_string())
                        .collect::<Vec<_>>()
                })
                .filter(|v| !v.is_empty());
            Ok(t)
        })
        .collect()
}

/// Заменяет триггеры записи `entry` набором `ts` (уже проверенным [`check_triggers`]).
pub(crate) fn replace_triggers(
    c: &Connection,
    entry: Id,
    ts: &[NewTrigger],
    now: &str,
) -> Result<(), StoreError> {
    c.prepare_cached("DELETE FROM triggers WHERE entry_id = ?1")?
        .execute([entry])?;
    let mut ins = c.prepare_cached(
        "INSERT INTO triggers (id, entry_id, type, pattern, tools, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
    )?;
    for t in ts {
        let tools = t
            .tools
            .as_ref()
            .map(|v| serde_json::to_string(v).expect("сериализация списка строк"));
        ins.execute(params![Id::new(), entry, t.kind, t.pattern, tools, now])?;
    }
    Ok(())
}

/// JSON-массив инструментов из колонки `tools`.
fn tools_col(r: &rusqlite::Row, i: usize) -> rusqlite::Result<Option<Vec<String>>> {
    let s: Option<String> = r.get(i)?;
    s.map(|s| serde_json::from_str(&s).map_err(|e| crate::time::conv_err(i, e)))
        .transpose()
}

/// Заменяет все триггеры insight `entry` набором `ts` одной транзакцией. Неверный триггер
/// (`triggers[i]: …`), больше [`TRIGGERS_MAX`] или запись не insight → `BadInput`; нет
/// записи или удалена → `NotFound`.
pub async fn set_triggers(db: &Db, entry: Id, ts: Vec<NewTrigger>) -> Result<(), StoreError> {
    let ts = check_triggers(ts)?;
    db.write(move |tx| {
        let e = load_entry(tx, entry)?;
        if e.deleted_at.is_some() {
            return Err(StoreError::NotFound);
        }
        if e.kind != EntryKind::Insight {
            return Err(StoreError::BadInput(
                "триггеры есть только у insight".into(),
            ));
        }
        replace_triggers(tx, entry, &ts, &fmt_ts(Timestamp::now()))
    })
    .await
}

/// Триггеры записи в порядке добавления (включая выключенные — для карточки записи).
pub async fn entry_triggers(db: &Db, entry: Id) -> Result<Vec<Trigger>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT id, type, pattern, tools, created_at FROM triggers
             WHERE entry_id = ?1 ORDER BY rowid",
        )?;
        let rows = st.query_map([entry], |r| {
            Ok(Trigger {
                id: r.get(0)?,
                kind: r.get(1)?,
                pattern: r.get(2)?,
                tools: tools_col(r, 3)?,
                created_at: ts_at(r, 4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Действующие триггеры пространств `space_ids`: включённые, у живых insight со статусом
/// `active` или `revisit`. Порядок — пространство, запись, порядок добавления.
pub async fn active_triggers(db: &Db, space_ids: &[Id]) -> Result<Vec<TriggerRow>, StoreError> {
    if space_ids.is_empty() {
        return Ok(Vec::new());
    }
    let spaces =
        serde_json::Value::from(space_ids.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT t.id, t.entry_id, t.type, t.pattern, t.tools, e.title,
                    CASE WHEN json_type(e.data, '$.kind') = 'text'
                         THEN json_extract(e.data, '$.kind') ELSE '' END,
                    e.certainty, e.space_id
             FROM triggers t JOIN entries e ON e.id = t.entry_id
             WHERE t.enabled = 1 AND e.deleted_at IS NULL AND e.kind = 'insight'
               AND e.status IN ('active', 'revisit')
               AND e.space_id IN (SELECT value FROM json_each(?1))
             ORDER BY e.space_id, e.id, t.rowid",
        )?;
        let rows = st.query_map([spaces], |r| {
            Ok(TriggerRow {
                id: r.get(0)?,
                entry_id: r.get(1)?,
                kind: r.get(2)?,
                pattern: r.get(3)?,
                tools: tools_col(r, 4)?,
                title: r.get(5)?,
                insight_kind: r.get(6)?,
                certainty: r.get(7)?,
                space_id: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(kind: TriggerKind, pattern: &str) -> NewTrigger {
        NewTrigger {
            kind,
            pattern: pattern.into(),
            tools: None,
        }
    }

    #[test]
    fn validate_trigger_rejects_bad_glob_regex_and_lengths() {
        for ok in [
            t(TriggerKind::FileGlob, "**/*.sh"),
            t(TriggerKind::FileGlob, "scripts/{a,b}/*.ps1"),
            t(TriggerKind::CommandRegex, r"^git\s+push\b"),
            t(TriggerKind::ErrorRegex, r"\^M|bad interpreter"),
            t(TriggerKind::Keyword, "  CRLF  "),
            NewTrigger {
                tools: Some(vec!["Write".into(), "Edit".into()]),
                ..t(TriggerKind::FileGlob, "*.sh")
            },
        ] {
            assert_eq!(validate_trigger(&ok), Ok(()), "{ok:?}");
        }
        let long = "a".repeat(PATTERN_MAX_CHARS + 1);
        for bad in [
            t(TriggerKind::FileGlob, "a[b"),
            t(TriggerKind::FileGlob, "{a,b"),
            t(TriggerKind::CommandRegex, "(unclosed"),
            t(TriggerKind::ErrorRegex, "a{2,1}"),
            t(TriggerKind::FileGlob, ""),
            t(TriggerKind::Keyword, "   "),
            t(TriggerKind::Keyword, &"k".repeat(KEYWORD_MAX_CHARS + 1)),
            t(TriggerKind::Keyword, "a\nb"),
            t(TriggerKind::FileGlob, &long),
            t(TriggerKind::CommandRegex, &long),
            NewTrigger {
                tools: Some(vec!["x".into(); TOOLS_MAX + 1]),
                ..t(TriggerKind::FileGlob, "*.sh")
            },
            NewTrigger {
                tools: Some(vec![" ".into()]),
                ..t(TriggerKind::FileGlob, "*.sh")
            },
        ] {
            assert!(validate_trigger(&bad).is_err(), "{bad:?}");
        }
        // regex, который больше 1 МиБ в скомпилированном виде, отклоняется
        let huge = t(TriggerKind::CommandRegex, r"\w{1000}\w{1000}\w{1000}");
        assert!(validate_trigger(&huge).is_err());
        // набор: индекс неверного триггера в тексте, предел числа
        let e = check_triggers(vec![
            t(TriggerKind::FileGlob, "*.sh"),
            t(TriggerKind::CommandRegex, "("),
        ])
        .unwrap_err();
        assert!(e.to_string().contains("triggers[1]"), "{e}");
        assert!(check_triggers(vec![t(TriggerKind::Keyword, "x"); TRIGGERS_MAX + 1]).is_err());
        let norm = check_triggers(vec![NewTrigger {
            tools: Some(Vec::new()),
            ..t(TriggerKind::Keyword, " lf ")
        }])
        .unwrap();
        assert_eq!((norm[0].pattern.as_str(), &norm[0].tools), ("lf", &None));
    }

    #[test]
    fn regex_without_backrefs_is_only_dialect() {
        // обратные ссылки и lookaround — не поддерживаются: катастрофического перебора нет
        for bad in [r"(a)\1", r"(?=x)y", r"(?!x)y", r"(?<=x)y", r"(?<!x)y"] {
            assert!(
                validate_trigger(&t(TriggerKind::ErrorRegex, bad)).is_err(),
                "{bad}"
            );
        }
        // классический ReDoS-шаблон в этом диалекте безопасен: линейное время
        let evil = t(TriggerKind::ErrorRegex, "(a+)+$");
        assert_eq!(validate_trigger(&evil), Ok(()));
        let re = regex::Regex::new("(a+)+$").unwrap();
        let input = format!("{}!", "a".repeat(100_000));
        let started = std::time::Instant::now();
        assert!(!re.is_match(&input));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn trigger_json_shape() {
        let v: NewTrigger =
            serde_json::from_str(r#"{"type":"file_glob","pattern":"**/*.sh","tools":["Write"]}"#)
                .unwrap();
        assert_eq!(v.kind, TriggerKind::FileGlob);
        assert!(serde_json::from_str::<NewTrigger>(r#"{"type":"glob","pattern":"x"}"#).is_err());
        for k in [
            TriggerKind::FileGlob,
            TriggerKind::CommandRegex,
            TriggerKind::ErrorRegex,
            TriggerKind::Keyword,
        ] {
            assert_eq!(TriggerKind::parse(k.as_str()), Some(k));
            assert_eq!(
                serde_json::to_string(&k).unwrap(),
                format!("\"{}\"", k.as_str())
            );
        }
    }
}
