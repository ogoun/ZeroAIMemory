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
//!   имени в тексте — предупреждение записи заметки или журнала);
//! - поиск имён и псевдонимов в тексте (M6.6 §6) — одно сравнение [`ProseText::spans`] на
//!   предупреждения `term_alias` и подсказки терминов к запросу: код и идентификаторы — не
//!   проза, аббревиатуры ([`is_acronym`]) — с учётом регистра; слишком общий псевдоним —
//!   [`broad_alias`] (предупреждение `term_alias_broad` при записи термина).
//!
//! Текст чанков термина ([`chunk_source`]) отличается от прочих записей: имя, строка
//! `Псевдонимы: …`, определение, тело; псевдонимы и идентификаторы кода идут и в префикс
//! контекста чанка ([`prefix_terms`]), поэтому FTS находит термин по ним.

use std::ops::Range;

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

/// Знак слова: буква, цифра или `_` (как `\w`, M6.6 §6.2: `ZAM_X` — одно слово).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Соединитель частей идентификатора (M6.6 §6.2): `zam-server`, `hooks.rs`, `crates/zam`,
/// `C:\zam`, `zam::foo`, `user@zam`, `C#`, `$ZAM`; `_` — и так знак слова.
fn is_connector(c: char) -> bool {
    matches!(c, '_' | '-' | '.' | '/' | '\\' | ':' | '@' | '#' | '$')
}

/// За краем вхождения (`beyond` — знаки от края наружу) слово продолжается: знак слова или
/// серия соединителей, за которой знак слова (`zam::foo`). Соединитель без знака слова за ним —
/// граница: точка в конце предложения («настроены hooks.»), «hooks: …».
fn joined(mut beyond: impl Iterator<Item = char>) -> bool {
    match beyond.next() {
        Some(c) if is_word(c) => true,
        Some(c) if is_connector(c) => beyond
            .find(|&c| is_word(c) || !is_connector(c))
            .is_some_and(is_word),
        _ => false,
    }
}

/// Вхождение `hay[r]` — слово прозы, а не часть слова или идентификатора: граница проверяется у
/// края из знака слова (у «C++» правый край — знак: «C++17» находит «C++»).
fn bounded(hay: &str, r: &Range<usize>) -> bool {
    let s = &hay[r.clone()];
    let starts_word = s.chars().next().is_some_and(is_word);
    let ends_word = s.chars().next_back().is_some_and(is_word);
    (!starts_word || !joined(hay[..r.start].chars().rev()))
        && (!ends_word || !joined(hay[r.end..].chars()))
}

/// Нижний регистр без сдвига смещений: знак, чья строчная форма — другой длины в UTF-8 или
/// несколько знаков («İ»), остаётся как есть (одинаково в тексте и в имени).
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) if x.len_utf8() == c.len_utf8() => x,
                _ => c,
            }
        })
        .collect()
}

/// Имя или псевдоним термина — аббревиатура (M6.6 §6.3, решение координатора 10): не меньше
/// двух букв, и все буквы заглавные (`ZAM`, `MCP`, `AWS S3`); цифры и знаки не мешают, одна
/// буква (`S3`, `C++`) — не аббревиатура. Аббревиатура сравнивается с учётом регистра
/// ([`ProseText::spans`]), прочие имена — без.
pub fn is_acronym(name: &str) -> bool {
    let mut letters = 0;
    for c in name.chars().filter(|c| c.is_alphabetic()) {
        if !c.is_uppercase() {
            return false;
        }
        letters += 1;
    }
    letters >= 2
}

/// Почему псевдоним слишком общий ([`broad_alias`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadAlias {
    /// Меньше 3 букв или цифр («ok», «S3»).
    Short,
    /// Служебное слово ([`zam_core::text::is_stop_word`]: «для», «the»).
    StopWord,
}

/// Псевдоним термина слишком общий (M6.6 §6.5): меньше 3 букв или цифр либо служебное слово
/// (без регистра и знаков, [`normalize_title`]). Запись термина не отклоняется — сервер
/// предупреждает `term_alias_broad`.
pub fn broad_alias(alias: &str) -> Option<BroadAlias> {
    if alias.chars().filter(|c| c.is_alphanumeric()).count() < 3 {
        Some(BroadAlias::Short)
    } else if zam_core::text::is_stop_word(&normalize_title(alias)) {
        Some(BroadAlias::StopWord)
    } else {
        None
    }
}

/// Текст для поиска имён и псевдонимов терминов (M6.6 §6): проза без кода
/// ([`zam_markdown::prose_mask`]: код — пробелами) с исходным регистром и в нижнем регистре
/// (по знакам, без сдвига смещений); смещения в байтах у обеих копий — как у исходного текста.
pub struct ProseText {
    cased: String,
    folded: String,
}

impl ProseText {
    pub fn new(text: &str) -> ProseText {
        let cased = zam_markdown::prose_mask(text);
        let folded = fold(&cased);
        ProseText { cased, folded }
    }

    /// Проза с исходным регистром: код заменён пробелами.
    pub fn as_str(&self) -> &str {
        &self.cased
    }

    /// Вхождения имени или псевдонима `name` в прозе — байтовые диапазоны по возрастанию
    /// начала (могут перекрываться). Одно сравнение на предупреждения `term_alias`
    /// ([`find_alias_uses`]) и подсказки терминов к запросу (`GlossaryIndex::find_in_head`
    /// сервера), M6.6 §6:
    /// - код не текст — ищется в [`ProseText`];
    /// - идентификатор не слово — у края из знака слова нет знака слова (`_` — тоже), а у
    ///   соединителя (`- . / \ : @ # $`) за ним нет знака слова: `zam-server`, `hooks.rs`,
    ///   `ZAM_X`, `crates/zam`, `zam::foo` не находят `zam`/`hooks`; точка в конце предложения
    ///   — граница;
    /// - `acronym` ([`is_acronym`] имени, вызывающий считает один раз) — с учётом регистра,
    ///   иначе без;
    /// - слова многословного имени разделяет в тексте один пробельный знак или `\r\n` (перенос
    ///   строки), а не несколько: на месте кода в маске — серия пробелов.
    pub fn spans<'a>(
        &'a self,
        name: &str,
        acronym: bool,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        let (hay, needle) = if acronym {
            (self.cased.as_str(), name.to_string())
        } else {
            (self.folded.as_str(), fold(name))
        };
        Spans {
            hay,
            parts: needle.split_whitespace().map(str::to_string).collect(),
            from: 0,
        }
    }
}

/// Итератор [`ProseText::spans`].
struct Spans<'a> {
    hay: &'a str,
    /// Слова имени (по пробельным знакам).
    parts: Vec<String>,
    /// Байт, с которого ищется следующее вхождение первого слова.
    from: usize,
}

impl Spans<'_> {
    /// Конец имени, чьё первое слово кончается в `end`: остальные слова — через один
    /// пробельный знак или `\r\n`.
    fn tail(&self, mut end: usize) -> Option<usize> {
        for p in &self.parts[1..] {
            let rest = &self.hay[end..];
            let gap = if rest.starts_with("\r\n") {
                2
            } else {
                rest.chars()
                    .next()
                    .filter(|c| c.is_whitespace())?
                    .len_utf8()
            };
            if !rest[gap..].starts_with(p.as_str()) {
                return None;
            }
            end += gap + p.len();
        }
        Some(end)
    }
}

impl Iterator for Spans<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let first = self.parts.first()?;
        let step = first.chars().next().map_or(1, char::len_utf8);
        loop {
            let start = self.from + self.hay.get(self.from..)?.find(first.as_str())?;
            // следующий поиск — со следующего знака: вхождения могут перекрываться
            self.from = start + step;
            if let Some(end) = self.tail(start + first.len()) {
                let r = start..end;
                if bounded(self.hay, &r) {
                    return Some(r);
                }
            }
        }
    }
}

/// Псевдонимы терминов `terms`, встреченные в прозе `text` вместо имени ([`ProseText::spans`]:
/// не в коде и не в идентификаторах, аббревиатуры — с учётом регистра, прочие — без): пары
/// `(псевдоним, имя)`, не больше [`ALIAS_USES_MAX`] (порядок — терминов, затем псевдонимов).
pub fn find_alias_uses<'a>(
    text: &str,
    terms: impl IntoIterator<Item = &'a GlossaryEntry>,
) -> Vec<(String, String)> {
    let prose = ProseText::new(text);
    let mut out: Vec<(String, String)> = Vec::new();
    'terms: for t in terms {
        let own = normalize_title(&t.title);
        for a in &t.aliases {
            if normalize_title(a) == own || prose.spans(a, is_acronym(a)).next().is_none() {
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

    /// M6.6 §6.1–§6.2 (замечание 12): код в обратных кавычках и ограждениях и идентификаторы
    /// (`zam-server`, `hooks.rs`, `ZAM_X`, `crates/zam`, `zam::foo`) — не проза; точка в конце
    /// предложения — граница. Живой случай обкатки 2026-11-17: «ZAM» на `zam-code`,
    /// «code map»/«zam code» на `` `zam code map` ``.
    #[test]
    fn alias_uses_skip_code_and_identifiers() {
        let terms = [
            entry("Сервер памяти", &["zam"]),
            entry("Хуки", &["hooks"]),
            entry("ZeroAIMemory", &["ZAM"]),
            entry("Карта кода", &["code map", "zam code"]),
        ];
        let hit = |t: &str| find_alias_uses(t, &terms);
        for text in [
            "Запусти `zam-server` сейчас",
            "Запусти `zam` сейчас",
            "Пример:\n```\nzam start\nhooks\n```\nвсё",
            "Пример:\n~~~sh\nzam start\n~~~\n",
            "правка в hooks.rs готова",
            "переменная ZAM_X и zam_y",
            "путь crates/zam и C:\\zam",
            "вызов zam::foo и user@zam",
            "команда zam-code обновляет карту",
            "команда ZAM-code обновляет карту",
            "Карта — `zam code map`.",
            "",
        ] {
            assert!(hit(text).is_empty(), "{text:?}: {:?}", hit(text));
        }
        assert_eq!(
            hit("Все настроены hooks."),
            [("hooks".into(), "Хуки".into())]
        );
        assert_eq!(hit("hooks: готовы\n"), [("hooks".into(), "Хуки".into())]);
        assert_eq!(
            hit("Запусти zam сейчас, а `zam` — в коде"),
            [("zam".into(), "Сервер памяти".into())]
        );
        assert_eq!(
            hit("Обнови code map."),
            [("code map".into(), "Карта кода".into())]
        );
    }

    /// M6.6 §6.3 (решение координатора 10): аббревиатура (две и более буквы, все заглавные) —
    /// с учётом регистра, прочие псевдонимы — без, как прежде.
    #[test]
    fn alias_uses_acronym_case() {
        let terms = [
            entry("ZeroAIMemory", &["ZAM"]),
            entry("Выжимка", &["summary"]),
        ];
        let hit = |t: &str| find_alias_uses(t, &terms);
        assert!(hit("zam тут").is_empty(), "{:?}", hit("zam тут"));
        assert!(hit("Zam тут").is_empty(), "{:?}", hit("Zam тут"));
        assert_eq!(hit("ZAM тут"), [("ZAM".into(), "ZeroAIMemory".into())]);
        assert_eq!(
            hit("Summary в начале фразы"),
            [("summary".into(), "Выжимка".into())]
        );
        assert_eq!(hit("SUMMARY"), [("summary".into(), "Выжимка".into())]);
    }

    #[test]
    fn acronym_and_broad_alias_rules() {
        for a in ["ZAM", "MCP", "AWS S3", "ЗАМ", "C2C"] {
            assert!(is_acronym(a), "{a}");
        }
        for a in ["Zam", "zam", "S3", "C++", "Z", "summary", "42", ""] {
            assert!(!is_acronym(a), "{a}");
        }
        assert_eq!(broad_alias("ok"), Some(BroadAlias::Short));
        assert_eq!(broad_alias("S3"), Some(BroadAlias::Short));
        assert_eq!(broad_alias("C++"), Some(BroadAlias::Short));
        assert_eq!(broad_alias("для"), Some(BroadAlias::StopWord));
        assert_eq!(broad_alias("The!"), Some(BroadAlias::StopWord));
        for a in ["MCP", "zam", "summary", "for the"] {
            assert_eq!(broad_alias(a), None, "{a}");
        }
    }

    /// Диапазоны [`ProseText::spans`] — смещения исходного текста; слова имени — через один
    /// пробельный знак или перенос строки, не через вырезанный код.
    #[test]
    fn prose_spans_offsets_and_multiword() {
        let text = "Ёж `код` code\nmap и code\r\nmap, code `x` map, code  map; İzmir";
        let p = ProseText::new(text);
        let got: Vec<&str> = p.spans("Code Map", false).map(|r| &text[r]).collect();
        assert_eq!(got, ["code\nmap", "code\r\nmap"]);
        let got: Vec<&str> = p.spans("ёж", false).map(|r| &text[r]).collect();
        assert_eq!(got, ["Ёж"]);
        assert_eq!(p.spans("İzmir", false).count(), 1);
        assert_eq!(p.spans("код", false).count(), 0);
        assert_eq!(p.spans("", false).count(), 0);
        // перекрывающиеся вхождения: первое («xa a») не на границе, второе — на ней
        let p = ProseText::new("xa a a");
        let got: Vec<(usize, usize)> = p.spans("a a", false).map(|r| (r.start, r.end)).collect();
        assert_eq!(got, [(3, 6)]);
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
