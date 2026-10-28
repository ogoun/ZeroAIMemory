//! Словарь домена (спецификация M4.5 §2): записи `term` — каноническое имя, псевдонимы,
//! определение, «не путать с», идентификаторы кода.
//!
//! - [`TermData`] — разбор и нормализация `data` термина (пределы §2.1): псевдонимы и
//!   идентификаторы без пробелов по краям, без пустых и повторов;
//! - уникальность: нормализованные ([`normalize_title`]) имя и псевдонимы действующих
//!   (`active`) терминов пространства не пересекаются — проверка внутри транзакции записи
//!   ([`ensure_names_free`]), нарушение — [`StoreError::TermConflict`]; замещаемый термин и
//!   сам правимый исключаются;
//! - версия словаря (`meta.glossary_version`) растёт при любой записи `term`
//!   ([`terms_written`]): по ней кэшируется словарь в процессе сервера (поиск, подсказки);
//!   там же перепроверяются неразрешённые вики-ссылки `[[Имя]]` пространства
//!   ([`crate::graph::terms_changed`]);
//! - чтение: [`active_terms`], [`glossary_version`], [`find_alias_uses`] (псевдоним вместо
//!   имени в тексте — предупреждение записи заметки или журнала).
//!
//! Текст чанков термина ([`chunk_source`]) отличается от прочих записей: имя, строка
//! `Псевдонимы: …`, определение, тело; псевдонимы и идентификаторы кода идут и в префикс
//! контекста чанка ([`prefix_terms`]), поэтому FTS находит термин по ним.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use zam_core::Id;

use crate::entries::normalize_title;
use crate::{Db, StoreError};

/// Предел длины имени термина, символов (спецификация M4.5 §2.1).
pub const TERM_TITLE_MAX_CHARS: usize = 200;
/// Псевдонимов у термина и длина псевдонима.
pub const ALIASES_MAX: usize = 32;
pub const ALIAS_MAX_CHARS: usize = 200;
/// Определение: обязательно, не длиннее.
pub const DEFINITION_MAX_CHARS: usize = 300;
/// Записей «не путать с» и длина записи **[решение исполнителя]**: предел длины в спецификации
/// не назван, берётся как у псевдонима.
pub const NOT_TO_CONFUSE_MAX: usize = 16;
pub const NOT_TO_CONFUSE_MAX_CHARS: usize = 200;
/// Идентификаторов кода и длина идентификатора.
pub const CODE_MAX: usize = 32;
pub const CODE_MAX_CHARS: usize = 200;
/// Предупреждений об использовании псевдонима на одну запись.
pub const ALIAS_USES_MAX: usize = 5;

/// Ключ `meta`: версия словаря.
const VERSION_KEY: &str = "glossary_version";

/// Разобранная и нормализованная `data` термина.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TermData {
    pub aliases: Vec<String>,
    pub definition: String,
    pub not_to_confuse: Vec<String>,
    pub code: Vec<String>,
}

/// Строки массива `data.<field>`: без пробелов по краям, без пустых и повторов, не больше
/// `max` по `item_max` символов. Ошибка — текст `data.<field>: …`.
fn strings(
    v: Option<&Value>,
    field: &str,
    max: usize,
    item_max: usize,
) -> Result<Vec<String>, String> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let a = v
        .as_array()
        .ok_or_else(|| format!("data.{field}: ожидается массив строк"))?;
    let mut out: Vec<String> = Vec::with_capacity(a.len());
    for x in a {
        let s = x
            .as_str()
            .ok_or_else(|| format!("data.{field}: ожидается массив строк"))?
            .trim();
        if s.chars().count() > item_max {
            return Err(format!("data.{field}: строки до {item_max} символов"));
        }
        if !s.is_empty() && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }
    if out.len() > max {
        return Err(format!("data.{field}: не больше {max} элементов"));
    }
    Ok(out)
}

impl TermData {
    /// Разбирает `data` термина (пределы §2.1). Неизвестное поле, `definition` вне 1..=300
    /// символов, псевдоним без букв и цифр — ошибка с текстом `data.<поле>: …`.
    pub fn parse(data: &Value) -> Result<TermData, String> {
        let o = data
            .as_object()
            .ok_or_else(|| "data должно быть JSON-объектом".to_string())?;
        for k in o.keys() {
            if !["aliases", "definition", "not_to_confuse", "code"].contains(&k.as_str()) {
                return Err(format!("data.{k}: неизвестное поле"));
            }
        }
        let aliases = strings(o.get("aliases"), "aliases", ALIASES_MAX, ALIAS_MAX_CHARS)?;
        if aliases.iter().any(|a| normalize_title(a).is_empty()) {
            return Err("data.aliases: псевдоним должен содержать буквы или цифры".into());
        }
        let definition = match o.get("definition") {
            Some(Value::String(s)) => s.trim().to_string(),
            None | Some(Value::Null) => String::new(),
            Some(_) => return Err("data.definition: ожидается строка".into()),
        };
        if definition.is_empty() || definition.chars().count() > DEFINITION_MAX_CHARS {
            return Err(format!(
                "data.definition: обязательна, от 1 до {DEFINITION_MAX_CHARS} символов"
            ));
        }
        Ok(TermData {
            aliases,
            definition,
            not_to_confuse: strings(
                o.get("not_to_confuse"),
                "not_to_confuse",
                NOT_TO_CONFUSE_MAX,
                NOT_TO_CONFUSE_MAX_CHARS,
            )?,
            code: strings(o.get("code"), "code", CODE_MAX, CODE_MAX_CHARS)?,
        })
    }

    /// Нормализованная `data` для записи.
    pub fn to_value(&self) -> Value {
        json!({
            "aliases": self.aliases,
            "definition": self.definition,
            "not_to_confuse": self.not_to_confuse,
            "code": self.code,
        })
    }

    /// Имена термина для проверки уникальности: `(нормализованное, как задано)` — имя, затем
    /// псевдонимы; повторы нормализованного (псевдоним равен имени) пропускаются.
    pub fn names(&self, title: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for raw in std::iter::once(title).chain(self.aliases.iter().map(String::as_str)) {
            let n = normalize_title(raw);
            if !n.is_empty() && !out.iter().any(|(o, _)| *o == n) {
                out.push((n, raw.trim().to_string()));
            }
        }
        out
    }
}

/// Имя и `data` термина: имя 1..=200 символов с буквой или цифрой, `data` — [`TermData::parse`].
/// Возвращает нормализованные имя и данные; ошибка — текст для 422 (`title: …`, `data.<поле>: …`).
pub fn check_term(title: &str, data: &Value) -> Result<(String, TermData), String> {
    let t = title.trim();
    if t.is_empty() || t.chars().count() > TERM_TITLE_MAX_CHARS {
        return Err(format!("title: от 1 до {TERM_TITLE_MAX_CHARS} символов"));
    }
    if normalize_title(t).is_empty() {
        return Err("title: имя должно содержать буквы или цифры".into());
    }
    Ok((t.to_string(), TermData::parse(data)?))
}

/// Пересечение имени или псевдонима нового термина с действующим термином пространства.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TermConflict {
    /// Существующий термин.
    pub id: Id,
    pub title: String,
    /// Имя или псевдоним **нового** термина (как задан), совпавший с именем или псевдонимом
    /// существующего **[решение исполнителя]**.
    pub matched: String,
}

/// Имена `title`/`data` свободны среди действующих терминов пространства `space` (кроме
/// `except`) — иначе [`StoreError::TermConflict`] со всеми пересечениями. Вызывается внутри
/// транзакции писателя (`BEGIN IMMEDIATE`): проверка и запись атомарны.
pub(crate) fn ensure_names_free(
    c: &Connection,
    space: Id,
    title: &str,
    data: &TermData,
    except: Option<Id>,
) -> Result<(), StoreError> {
    let mine = data.names(title);
    let mut st = c.prepare_cached(
        "SELECT id, title, data FROM entries
         WHERE space_id = ?1 AND kind = 'term' AND status = 'active' AND deleted_at IS NULL
           AND (?2 IS NULL OR id <> ?2)",
    )?;
    let rows = st.query_map(params![space, except], |r| {
        Ok((
            r.get::<_, Id>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut conflicts = Vec::new();
    for row in rows {
        let (id, t, d) = row?;
        let other = lossy_names(&t, &d);
        for (norm, raw) in &mine {
            if other.contains(norm) {
                conflicts.push(TermConflict {
                    id,
                    title: t.clone(),
                    matched: raw.clone(),
                });
            }
        }
    }
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(StoreError::TermConflict(conflicts))
    }
}

/// Нормализованные имя и псевдонимы сохранённого термина (устойчиво к неполной `data`).
fn lossy_names(title: &str, data: &str) -> Vec<String> {
    let mut out = vec![normalize_title(title)];
    if let Ok(d) = serde_json::from_str::<Value>(data) {
        out.extend(
            str_items(d.get("aliases"))
                .iter()
                .map(|a| normalize_title(a)),
        );
    }
    out.retain(|n| !n.is_empty());
    out
}

fn str_items(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

/// Запись термина (создание, правка, замещение, отзыв, удаление, восстановление) в
/// транзакции писателя: версия словаря растёт, неразрешённые вики-ссылки пространства
/// перепроверяются.
pub(crate) fn terms_written(c: &Connection, space: Id) -> Result<(), StoreError> {
    c.prepare_cached(
        "INSERT INTO meta (key, value) VALUES (?1, '1')
         ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
    )?
    .execute([VERSION_KEY])?;
    crate::graph::terms_changed(c, space)
}

/// Версия словаря: растёт при любой записи `term`; 0 — терминов не писали.
pub async fn glossary_version(db: &Db) -> Result<i64, StoreError> {
    db.read(|c| {
        let v: Option<String> = c
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [VERSION_KEY],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|v| v.parse().ok()).unwrap_or(0))
    })
    .await
}

/// Действующий термин для подсказок, расширения запроса и предупреждений.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GlossaryEntry {
    pub id: Id,
    pub space_id: Id,
    pub title: String,
    pub aliases: Vec<String>,
    pub definition: String,
    /// Ревизия записи (учёт показа подсказки в `session_seen`).
    pub rev: i64,
}

/// Действующие (`active`, не удалённые) термины пространств `spaces` по имени.
pub async fn active_terms(db: &Db, spaces: &[Id]) -> Result<Vec<GlossaryEntry>, StoreError> {
    let spaces = serde_json::to_string(spaces).expect("сериализация списка id");
    read_terms(db, Some(spaces)).await
}

/// Действующие термины всех пространств базы по имени: источник кэша словаря сервера (область
/// запроса отбирается при чтении кэша по `space_id`).
pub async fn all_active_terms(db: &Db) -> Result<Vec<GlossaryEntry>, StoreError> {
    read_terms(db, None).await
}

async fn read_terms(db: &Db, spaces: Option<String>) -> Result<Vec<GlossaryEntry>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT id, space_id, title, data, current_rev FROM entries
             WHERE kind = 'term' AND status = 'active' AND deleted_at IS NULL
               AND (?1 IS NULL OR space_id IN (SELECT value FROM json_each(?1)))
             ORDER BY title_norm, id",
        )?;
        let rows = st.query_map([spaces], |r| {
            Ok((
                r.get::<_, Id>(0)?,
                r.get::<_, Id>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, space_id, title, data, rev) = row?;
            let d: Value = serde_json::from_str(&data)
                .map_err(|e| StoreError::Corrupt(format!("data термина {id}: {e}")))?;
            out.push(GlossaryEntry {
                id,
                space_id,
                title,
                aliases: str_items(d.get("aliases")),
                definition: d
                    .get("definition")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rev,
            });
        }
        Ok(out)
    })
    .await
}

/// Термин по имени или псевдониму (без регистра и знаков препинания, [`normalize_title`]).
pub fn find_term<'a>(terms: &'a [GlossaryEntry], name: &str) -> Option<&'a GlossaryEntry> {
    let n = normalize_title(name);
    if n.is_empty() {
        return None;
    }
    terms.iter().find(|t| {
        normalize_title(&t.title) == n || t.aliases.iter().any(|a| normalize_title(a) == n)
    })
}

/// Действующий термин пространства по имени или псевдониму — для вики-ссылки `[[Имя]]`.
pub(crate) fn term_by_name(
    c: &Connection,
    space: Id,
    name: &str,
) -> Result<Option<Id>, rusqlite::Error> {
    let n = normalize_title(name);
    if n.is_empty() {
        return Ok(None);
    }
    let by_title: Option<Id> = c
        .prepare_cached(
            "SELECT id FROM entries
             WHERE space_id = ?1 AND kind = 'term' AND status = 'active' AND deleted_at IS NULL
               AND title_norm = ?2
             ORDER BY id LIMIT 1",
        )?
        .query_row(params![space, n], |r| r.get(0))
        .optional()?;
    if by_title.is_some() {
        return Ok(by_title);
    }
    let mut st = c.prepare_cached(
        "SELECT id, data FROM entries
         WHERE space_id = ?1 AND kind = 'term' AND status = 'active' AND deleted_at IS NULL
           AND data LIKE '%\"aliases\"%'
         ORDER BY id",
    )?;
    let rows = st.query_map([space], |r| {
        Ok((r.get::<_, Id>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, data) = row?;
        if let Ok(d) = serde_json::from_str::<Value>(&data)
            && str_items(d.get("aliases"))
                .iter()
                .any(|a| normalize_title(a) == n)
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Буква слова: буква или цифра (как в [`normalize_title`]).
fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// `alias` встречается в `lower_text` (оба в нижнем регистре) по границе слова.
fn has_word(lower_text: &str, alias: &str) -> bool {
    if alias.is_empty() {
        return false;
    }
    let (first, last) = (
        alias.chars().next().expect("не пусто"),
        alias.chars().next_back().expect("не пусто"),
    );
    lower_text.match_indices(alias).any(|(i, m)| {
        let before = lower_text[..i].chars().next_back();
        let after = lower_text[i + m.len()..].chars().next();
        (!is_word(first) || !before.is_some_and(is_word))
            && (!is_word(last) || !after.is_some_and(is_word))
    })
}

/// Псевдонимы терминов `terms`, встреченные в `text` вместо имени: пары `(псевдоним, имя)`,
/// по границе слова, без учёта регистра, не больше [`ALIAS_USES_MAX`] (порядок — терминов, затем
/// псевдонимов).
pub fn find_alias_uses<'a>(
    text: &str,
    terms: impl IntoIterator<Item = &'a GlossaryEntry>,
) -> Vec<(String, String)> {
    let lower = text.to_lowercase();
    let mut out: Vec<(String, String)> = Vec::new();
    'terms: for t in terms {
        let own = normalize_title(&t.title);
        for a in &t.aliases {
            if normalize_title(a) == own || !has_word(&lower, &a.to_lowercase()) {
                continue;
            }
            out.push((a.clone(), t.title.clone()));
            if out.len() >= ALIAS_USES_MAX {
                break 'terms;
            }
        }
    }
    out
}

/// Источник нарезки термина: `# имя`, `Псевдонимы: a, b`, определение, тело.
pub(crate) fn chunk_source(title: &str, data: &Value, body: &str) -> String {
    let mut s = format!("# {title}\n");
    let aliases = str_items(data.get("aliases"));
    if !aliases.is_empty() {
        s.push_str(&format!("\nПсевдонимы: {}\n", aliases.join(", ")));
    }
    let def = data
        .get("definition")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !def.is_empty() {
        s.push_str(&format!("\n{def}\n"));
    }
    if !body.trim().is_empty() {
        s.push_str(&format!("\n{body}"));
    }
    s
}

/// Псевдонимы и идентификаторы кода — к тегам префикса контекста чанка.
pub(crate) fn prefix_terms(data: &Value) -> Vec<String> {
    let mut v = str_items(data.get("aliases"));
    v.extend(str_items(data.get("code")));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, aliases: &[&str]) -> GlossaryEntry {
        GlossaryEntry {
            id: Id::new(),
            space_id: Id::new(),
            title: title.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            definition: "определение".into(),
            rev: 1,
        }
    }

    #[test]
    fn parse_normalizes_and_limits() {
        let d = TermData::parse(&json!({
            "aliases": [" summary ", "", "summary", "Digest"],
            "definition": "  Короткая сводка  ",
            "code": ["Summary"],
        }))
        .unwrap();
        assert_eq!(d.aliases, ["summary", "Digest"]);
        assert_eq!(d.definition, "Короткая сводка");
        assert!(d.not_to_confuse.is_empty());
        for (bad, field) in [
            (json!({}), "data.definition"),
            (json!({"definition": ""}), "data.definition"),
            (json!({"definition": "x".repeat(301)}), "data.definition"),
            (json!({"definition": 1}), "data.definition"),
            (json!({"definition": "d", "aliases": "a"}), "data.aliases"),
            (json!({"definition": "d", "aliases": [1]}), "data.aliases"),
            (
                json!({"definition": "d", "aliases": vec!["x".repeat(201)]}),
                "data.aliases",
            ),
            (json!({"definition": "d", "aliases": ["—"]}), "data.aliases"),
            (
                json!({"definition": "d", "aliases": (0..33).map(|i| format!("a{i}")).collect::<Vec<_>>()}),
                "data.aliases",
            ),
            (
                json!({"definition": "d", "not_to_confuse": (0..17).map(|i| format!("a{i}")).collect::<Vec<_>>()}),
                "data.not_to_confuse",
            ),
            (
                json!({"definition": "d", "code": (0..33).map(|i| format!("a{i}")).collect::<Vec<_>>()}),
                "data.code",
            ),
            (json!({"definition": "d", "extra": 1}), "data.extra"),
        ] {
            let e = TermData::parse(&bad).unwrap_err();
            assert!(e.starts_with(field), "{bad}: {e}");
        }
        assert!(TermData::parse(&json!([])).is_err());
    }

    #[test]
    fn title_rules() {
        let ok = json!({"definition": "d"});
        assert!(check_term(" Выжимка ", &ok).is_ok());
        assert!(check_term("", &ok).unwrap_err().starts_with("title"));
        assert!(check_term("—", &ok).unwrap_err().starts_with("title"));
        assert!(check_term(&"x".repeat(201), &ok).is_err());
        assert!(check_term(&"x".repeat(200), &ok).is_ok());
    }

    #[test]
    fn names_skip_alias_equal_to_title() {
        let d =
            TermData::parse(&json!({"definition": "d", "aliases": ["SUMMARY", "digest"]})).unwrap();
        let n = d.names("Summary");
        assert_eq!(
            n,
            [
                ("summary".to_string(), "Summary".to_string()),
                ("digest".to_string(), "digest".to_string())
            ]
        );
    }

    #[test]
    fn alias_uses_by_word_boundary() {
        let terms = [entry("Выжимка", &["summary", "C++"])];
        let hit = |t: &str| find_alias_uses(t, &terms);
        assert_eq!(
            hit("Нужна Summary заметки"),
            [("summary".into(), "Выжимка".into())]
        );
        assert_eq!(hit("(summary)"), [("summary".into(), "Выжимка".into())]);
        assert!(hit("summaryX").is_empty());
        assert!(hit("mysummary").is_empty());
        // граница только у буквенного края: «C++» оканчивается знаком
        assert_eq!(hit("на C++17"), [("C++".into(), "Выжимка".into())]);
        assert!(hit("").is_empty());
    }

    #[test]
    fn alias_uses_limit_and_own_title() {
        let many: Vec<GlossaryEntry> = (0..8)
            .map(|i| entry(&format!("Термин{i}"), &[&format!("alias{i}")]))
            .collect();
        let text = (0..8)
            .map(|i| format!("alias{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(find_alias_uses(&text, &many).len(), ALIAS_USES_MAX);
        // псевдоним, равный имени, не предупреждает
        let own = [entry("Summary", &["summary"])];
        assert!(find_alias_uses("summary", &own).is_empty());
    }

    #[test]
    fn find_term_by_alias_case_insensitive() {
        let terms = [entry("Выжимка", &["summary"]), entry("Чанк", &[])];
        assert_eq!(find_term(&terms, "SUMMARY").unwrap().title, "Выжимка");
        assert_eq!(find_term(&terms, " чанк ").unwrap().title, "Чанк");
        assert!(find_term(&terms, "нет").is_none());
        assert!(find_term(&terms, "—").is_none());
    }

    #[test]
    fn chunk_source_layout() {
        let data = json!({"aliases": ["a", "b"], "definition": "Опр."});
        assert_eq!(
            chunk_source("Имя", &data, "Тело"),
            "# Имя\n\nПсевдонимы: a, b\n\nОпр.\n\nТело"
        );
        assert_eq!(chunk_source("Имя", &json!({}), ""), "# Имя\n");
        assert_eq!(
            prefix_terms(&json!({"aliases": ["a"], "code": ["x_y"]})),
            ["a", "x_y"]
        );
    }
}
