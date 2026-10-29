//! Полнотекстовый поиск по чанкам (§9.2 ТЗ, канал (b)): FTS5 без моделей.
//!
//! - Канал A — `chunks_fts` (unicode61, `bm25` с весами колонок text 1.0,
//!   heading_path 0.5, context_prefix 0.3), top 50. Сначала строгий запрос
//!   (все слова), при пустой выдаче — ослабленный (любое слово). Префиксный
//!   поиск (`"слово"*`) — только для слов от 3 символов: префикс из 1–2 букв
//!   раскрывается в тысячи термов и стоит секунды.
//! - Канал B — `chunks_fts_tri` (trigram), top 20: подстроки-идентификаторы
//!   запроса (токен ≥ 3 символов с `_ . / - :` или цифрами) через OR; если
//!   идентификаторов нет — вся фраза, и только когда канал A ничего не нашёл.
//! - Слияние weighted RRF (k = 60, веса 1.0 и 0.7), не больше 3 чанков на узел, top_k.
//! - В FTS лежат листы (`chunk`) и L1 заметки (`note`, §8.2; L1 секций — только для
//!   векторов). L1 заметки попадает в выдачу, только если ни один лист узла не найден:
//!   он повторяет предложения листов, но находит заметку по словам из разных листов.
//!
//! - Канал `alias` (M4.5) — тот же FTS по запросу, где слова, совпавшие с именами и псевдонимами
//!   терминов словаря, заменены группами `("слово"* OR "имя"* OR "псевдоним"*)` ([`AliasGroup`],
//!   строит сервер); строгий запрос, затем ослабленный, как у основного канала. Псевдоним короче
//!   3 символов ищется только точным словом, без `*`. Без групп канал не строится.
//!
//! Для гибридного поиска (M2) каналы отдаются раздельно — [`search_fts_channels`]: их
//! сливает `zam-search` вместе с векторным каналом и весами из конфигурации; там же
//! правило «сводка → лучший лист». [`live_chunks`] — метаданные и текст векторных хитов
//! с проверкой по SQLite (живой узел, не исключён, в области); [`live_folders`] — то же для
//! точек папок (канал папок M3, §8.3).
//!
//! С M3 цель чанка — узел или запись журнала (`entries`, [`FtsHit::entry`]). Фильтр видов
//! и статусов ([`TargetFilter`]) действует в обоих каналах и в [`live_chunks`]: виды —
//! `note` (заметки) и виды записей, статусы — только у insight (записи без статуса проходят
//! всегда). У записей нет пути: при `path_prefix` они не выдаются.
//!
//! Выборка двухшаговая в одной читающей транзакции: каналы отбирают только
//! `rowid` и узел (глобальный top с запасом из индекса FTS, затем фильтр области;
//! если в области осталось мало — точный запрос с фильтром), текст, метаданные
//! и `snippet()` читаются лишь для итоговых top_k. Фильтр области (пространства,
//! префикс пути, живые узлы) действует до отсечения top-N канала.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, ToSql, params, params_from_iter};
use zam_core::{Id, TargetKind, TargetRef};

use crate::time::ts_at;
use crate::{Db, StoreError};

/// Маркеры подсветки в `snippet` (Private Use Area: в тексте заметок не встречаются,
/// клиент заменяет их на свою разметку).
pub const MARK_START: char = '\u{E000}';
pub const MARK_END: char = '\u{E001}';

/// Предел длины запроса в символах.
pub const QUERY_MAX_CHARS: usize = 512;

/// Константа RRF.
const RRF_K: f64 = 60.0;
/// Вес канала bm25 (unicode61).
const W_BM25: f64 = 1.0;
/// Вес канала trigram.
const W_TRIGRAM: f64 = 0.7;
/// Кандидатов из канала bm25.
const BM25_LIMIT: i64 = 50;
/// Кандидатов из канала trigram.
const TRIGRAM_LIMIT: i64 = 20;
/// Во сколько раз больше кандидатов канала отбирается до фильтра области.
const OVERSAMPLE: i64 = 8;
/// Не больше чанков с одного узла (§9.2 п.6).
const PER_NODE_CAP: usize = 3;
/// Предел `top_k`.
const TOP_K_MAX: usize = 100;
/// Слов в запросе FTS: остальные отбрасываются, чтобы стоимость запроса была ограничена.
const MAX_TERMS: usize = 32;
/// Минимальная длина (в символах) слова для префиксного поиска и подстроки trigram.
const MIN_PREFIX_CHARS: usize = 3;
/// Токенов в сниппете канала bm25.
const SNIPPET_TOKENS: i64 = 16;
/// Длина сниппета trigram-хита в символах (без многоточий).
const SNIPPET_CHARS: usize = 160;

/// Виды, статусы и область целей поиска. По умолчанию — всё: заметки, записи всех видов,
/// любые статусы и области.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetFilter {
    /// `note` (заметки) и виды записей (`insight`, `changelog`, `release`); пусто — все.
    /// Остальные значения (`folder`, `attachment`) чанков не находят.
    pub kinds: Vec<String>,
    /// Разрешённые статусы записей всех видов; пусто — любые. Записи без статуса и узлы проходят.
    pub statuses: Vec<String>,
    /// Строгий фильтр области (`scope.area`, §9.1): только узлы и записи с `area`, равной
    /// значению; `None` — любые (и без области).
    pub area: Option<String>,
    /// Разрешённые `certainty` (`filters.certainty`, §7.5); пусто — любые (и без статуса).
    /// Непустой список пропускает только заметки и записи с этими статусами.
    pub certainty: Vec<String>,
    /// `human` | `agent` (`filters.decided_by`, M4.5): только решения (`insight` с
    /// `data.kind = decision`) с таким `data.decision.decided_by`; без поля — по автору записи.
    /// `None` — без фильтра.
    pub decided_by: Option<String>,
    /// Нормализованный путь файла (`filters.file`, M4.5 §6.3): только заметки и записи со
    /// ссылкой на него (`file_refs`, точное совпадение). `None` — без фильтра.
    pub file: Option<String>,
    /// Виды insight (`filters.insight_kinds`, §9.1): insight проходят, только если `data.kind`
    /// из списка; записи других видов и заметки фильтр не трогает. Пусто — любые.
    pub insight_kinds: Vec<String>,
}

/// Виды записей журналов (с M4.5 — `term`, `task`, `question`).
const ENTRY_KINDS: [&str; 6] = [
    "changelog",
    "release",
    "insight",
    "term",
    "task",
    "question",
];

/// Параметры SQL фильтра целей.
struct TargetArgs {
    /// Узлы проходят.
    notes: bool,
    /// JSON-массив разрешённых видов записей.
    kinds: String,
    /// JSON-массив разрешённых статусов или `None` (любые).
    statuses: Option<String>,
    /// Область или `None` (любые).
    area: Option<String>,
    /// JSON-массив разрешённых `certainty` или `None` (любые).
    certainty: Option<String>,
    /// `human` / `agent` или `None` (любые).
    decided_by: Option<String>,
    /// Путь ссылки на файл или `None` (любые).
    file: Option<String>,
    /// JSON-массив видов insight или `None` (любые).
    insight_kinds: Option<String>,
}

impl TargetFilter {
    fn args(&self) -> TargetArgs {
        let all = self.kinds.is_empty();
        let kinds: Vec<&str> = ENTRY_KINDS
            .into_iter()
            .filter(|k| all || self.kinds.iter().any(|x| x == k))
            .collect();
        TargetArgs {
            notes: all || self.kinds.iter().any(|k| k == "note"),
            kinds: serde_json::Value::from(kinds).to_string(),
            statuses: (!self.statuses.is_empty())
                .then(|| serde_json::Value::from(self.statuses.clone()).to_string()),
            area: self.area.clone(),
            certainty: (!self.certainty.is_empty())
                .then(|| serde_json::Value::from(self.certainty.clone()).to_string()),
            decided_by: self.decided_by.clone(),
            file: self.file.clone(),
            insight_kinds: (!self.insight_kinds.is_empty())
                .then(|| serde_json::Value::from(self.insight_kinds.clone()).to_string()),
        }
    }
}

/// Запрос полнотекстового поиска.
#[derive(Debug, Clone)]
pub struct FtsQuery {
    /// Текст запроса как прислан, до 512 символов: синтаксис FTS5 в нём не действует.
    pub text: String,
    /// Разрешённые пространства; пусто → `BadInput`.
    pub space_ids: Vec<Id>,
    /// Префикс пути: сам узел `prefix` и всё под `prefix/`. Регистр учитывается.
    pub path_prefix: Option<String>,
    /// Размер выдачи, 1..=100.
    pub top_k: usize,
    /// Виды и статусы целей.
    pub targets: TargetFilter,
    /// Расширение запроса словарём (канал `alias`, только [`search_fts_channels`]); пусто —
    /// канал не строится.
    pub alias_groups: Vec<AliasGroup>,
}

/// Группа расширения запроса: слова запроса `start..start + len` ([`query_terms`]) заменяются
/// на «слово OR `with`» (M4.5, спецификация §2.2 п. 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasGroup {
    /// Индекс первого заменяемого слова в [`query_terms`].
    pub start: usize,
    /// Сколько подряд идущих слов заменяет группа (многословное имя — несколько).
    pub len: usize,
    /// Имена и псевдонимы термина (в исходной записи; многословные ищутся фразой).
    pub with: Vec<String>,
}

/// Найденный чанк (FTS или — через [`live_chunks`] — векторный хит).
#[derive(Debug, Clone)]
pub struct FtsHit {
    pub chunk_id: String,
    /// Цель чанка: узел или запись журнала.
    pub target: TargetRef,
    pub space_id: Id,
    pub path: String,
    pub heading_path: String,
    /// `chunk` / `section` / `note`.
    pub level: String,
    /// Ревизия, по которой построен чанк.
    pub rev: i64,
    /// Токенов в тексте чанка (счётчик чанкера).
    pub token_count: i64,
    /// [`search_fts`] — слитый RRF-балл, [`search_fts_channels`] — `1/место` в канале,
    /// [`live_chunks`] — 0. Больше — лучше.
    pub score: f64,
    /// [`search_fts_channels`] — сырой балл `bm25()` канала, в списке которого лежит хит
    /// (у чанка, найденного обоими каналами, в каждом списке свой): меньше — лучше. По нему
    /// сервер сливает каналы нескольких баз. [`search_fts`] и [`live_chunks`] — 0.
    pub raw_score: f64,
    pub updated_at: Timestamp,
    /// Фрагмент с совпадениями между [`MARK_START`] и [`MARK_END`].
    pub snippet: String,
    /// Полный текст чанка (обрезку под бюджет делает сервер).
    pub text: String,
    /// `nodes.index_state` узла: `indexed` — его чанки есть в векторном индексе; `pending`,
    /// `failed`, `excluded` — нет (гибрид не понижает такие хиты ослабленного bm25).
    /// У записи — `entries.index_state`.
    pub index_state: String,
    /// Поля записи журнала; `None` — чанк заметки.
    pub entry: Option<EntryHit>,
    /// Область узла или записи (`nodes.area` / `entries.area`).
    pub area: Option<String>,
    /// `certainty` узла или записи (`nodes.certainty` / `entries.certainty`, §7.5).
    pub certainty: Option<String>,
    /// `nodes.verified_at` заметки (M4.5 §7); у записи — `None`.
    pub verified_at: Option<String>,
}

/// Поля записи журнала в хите поиска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryHit {
    /// `insight` / `changelog` / `release`.
    pub kind: String,
    pub status: Option<String>,
    /// `data.kind` записи (вид инсайта, вид изменения changelog).
    pub insight_kind: Option<String>,
    pub certainty: Option<String>,
    pub title: String,
}

/// Цель чанка по `chunks.node_id` / `chunks.entry_id`.
fn chunk_target(node: Option<Id>, entry: Option<Id>) -> rusqlite::Result<TargetRef> {
    match (node, entry) {
        (Some(id), None) => Ok(TargetRef {
            kind: TargetKind::Node,
            id,
        }),
        (None, Some(id)) => Ok(TargetRef {
            kind: TargetKind::Entry,
            id,
        }),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            "у чанка нет ровно одной цели (node_id / entry_id)".into(),
        )),
    }
}

/// Соединение чанка `c` с его узлом `n` или записью `e`. LEFT JOIN, как и CROSS JOIN,
/// фиксирует порядок: сначала чанки, затем цель по первичному ключу.
const TARGET_JOIN: &str =
    "LEFT JOIN nodes n ON n.id = c.node_id LEFT JOIN entries e ON e.id = c.entry_id";

/// Колонки хита для [`hit_row`] (после [`TARGET_JOIN`]).
const HIT_COLS: &str = "c.id, c.node_id, c.entry_id, coalesce(n.space_id, e.space_id),
    coalesce(n.path, ''), c.heading_path, c.rev, coalesce(n.updated_at, e.updated_at), c.text,
    c.level, c.token_count, coalesce(n.index_state, e.index_state), e.kind, e.status,
    CASE WHEN json_type(e.data, '$.kind') = 'text' THEN json_extract(e.data, '$.kind') END,
    e.certainty, e.title, coalesce(n.area, e.area), coalesce(n.certainty, e.certainty),
    n.verified_at";

/// Хит по колонкам [`HIT_COLS`], начиная с колонки `o`.
fn hit_row(r: &rusqlite::Row, o: usize) -> rusqlite::Result<FtsHit> {
    let target = chunk_target(r.get(o + 1)?, r.get(o + 2)?)?;
    let entry = match target.kind {
        TargetKind::Node => None,
        TargetKind::Entry => Some(EntryHit {
            kind: r.get(o + 12)?,
            status: r.get(o + 13)?,
            insight_kind: r.get(o + 14)?,
            certainty: r.get(o + 15)?,
            title: r.get(o + 16)?,
        }),
    };
    Ok(FtsHit {
        chunk_id: r.get(o)?,
        target,
        space_id: r.get(o + 3)?,
        path: r.get(o + 4)?,
        heading_path: r.get(o + 5)?,
        level: r.get(o + 9)?,
        rev: r.get(o + 6)?,
        token_count: r.get(o + 10)?,
        score: 0.0,
        raw_score: 0.0,
        updated_at: ts_at(r, o + 7)?,
        snippet: String::new(),
        text: r.get(o + 8)?,
        index_state: r.get(o + 11)?,
        entry,
        area: r.get(o + 17)?,
        certainty: r.get(o + 18)?,
        verified_at: r.get(o + 19)?,
    })
}

/// Слова запроса: всё, что не буква и не цифра (Unicode), — разделитель.
pub fn query_terms(query: &str) -> Vec<&str> {
    terms(query)
}

fn terms(query: &str) -> Vec<&str> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_TERMS)
        .collect()
}

/// Подстроки запроса для подсветки сниппета у хита без FTS-сниппета (векторный канал,
/// замена сводки листом): идентификаторы, затем слова от 3 символов, без повторов.
pub fn query_needles(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let words = terms(query)
        .into_iter()
        .filter(|t| t.chars().count() >= MIN_PREFIX_CHARS);
    for t in identifiers(query).into_iter().chain(words) {
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Строка в кавычках FTS5 (кавычки внутри удваиваются): операторы теряют смысл.
fn quote(t: &str) -> String {
    format!("\"{}\"", t.replace('"', "\"\""))
}

/// Слово как терм FTS5: от [`MIN_PREFIX_CHARS`] символов — префиксный (`"слово"*`),
/// короче — точный.
fn quote_term(t: &str) -> String {
    let q = quote(t);
    if t.chars().count() >= MIN_PREFIX_CHARS {
        q + "*"
    } else {
        q
    }
}

/// Безопасный запрос FTS5: каждое слово в кавычках, от 3 символов — с префиксом
/// (`"слово"*`), слова через пробел (неявный AND). Синтаксис FTS5 во входе
/// (`"`, `*`, `AND`, `-`, `:`, `(`, `NEAR`) не действует. `None` — в запросе нет слов.
pub fn sanitize_fts(query: &str) -> Option<String> {
    join_terms(query, " ")
}

/// Ослабленный вариант [`sanitize_fts`]: слова через `OR`.
fn sanitize_fts_any(query: &str) -> Option<String> {
    join_terms(query, " OR ")
}

fn join_terms(query: &str, sep: &str) -> Option<String> {
    let t = terms(query);
    if t.is_empty() {
        return None;
    }
    Some(t.into_iter().map(quote_term).collect::<Vec<_>>().join(sep))
}

/// Имя или псевдоним термина как терм FTS5: слова фразой; префикс (`*`) — только если последнее
/// слово не короче [`MIN_PREFIX_CHARS`] символов, иначе точная фраза. `None` — в строке нет слов.
fn phrase_term(s: &str) -> Option<String> {
    let w = terms(s);
    if w.is_empty() {
        return None;
    }
    let q = quote(&w.join(" "));
    // префикс - только на последнем слове фразы и только от 3 символов: «Plan B» не должна
    // раскрываться в префикс из одной буквы
    let last = w.last().map_or(0, |l| l.chars().count());
    Some(if last >= MIN_PREFIX_CHARS { q + "*" } else { q })
}

/// Запрос канала `alias`: слова запроса, группы вместо совпавших слов; `sep` — `" AND "` (все
/// слова; неявное «И» FTS5 перед скобкой не разбирает) или `" OR "` (любое). `None` — в запросе
/// нет слов.
fn alias_fts(query: &str, groups: &[AliasGroup], sep: &str) -> Option<String> {
    let t = terms(query);
    if t.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        let group = groups
            .iter()
            .find(|g| g.start == i && g.len >= 1 && i + g.len <= t.len());
        if let Some(g) = group {
            let mut alts: Vec<String> = Vec::new();
            let own = t[i..i + g.len].join(" ");
            for w in std::iter::once(&own).chain(&g.with) {
                if let Some(a) = phrase_term(w)
                    && !alts.iter().any(|x| x.to_lowercase() == a.to_lowercase())
                {
                    alts.push(a);
                }
            }
            parts.push(format!("({})", alts.join(" OR ")));
            i += g.len;
        } else {
            parts.push(quote_term(t[i]));
            i += 1;
        }
    }
    Some(parts.join(sep))
}

/// Идентификаторы и код в запросе: токены через пробелы без обрамляющей
/// пунктуации, от 3 символов, с `_ . / - :` или цифрами.
fn identifiers(query: &str) -> Vec<&str> {
    query
        .split_whitespace()
        .map(|t| t.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_'))
        .filter(|t| {
            t.chars().count() >= MIN_PREFIX_CHARS
                && t.chars()
                    .any(|ch| matches!(ch, '_' | '.' | '/' | '-' | ':') || ch.is_ascii_digit())
        })
        .take(MAX_TERMS)
        .collect()
}

/// Запрос канала trigram и подстроки для сниппета. Идентификаторы — через OR;
/// без идентификаторов — вся фраза, если `bm25_empty` и в ней не меньше 3 символов
/// (короче trigram не ищет). `None` — канал не запускается.
fn trigram_query(query: &str, bm25_empty: bool) -> Option<(String, Vec<&str>)> {
    let ids = identifiers(query);
    if !ids.is_empty() {
        let fts = ids
            .iter()
            .map(|t| quote(t))
            .collect::<Vec<_>>()
            .join(" OR ");
        return Some((fts, ids));
    }
    let phrase = query.trim();
    (bm25_empty && phrase.chars().count() >= MIN_PREFIX_CHARS)
        .then(|| (quote(phrase), vec![phrase]))
}

/// Канал поиска.
#[derive(Clone, Copy)]
enum Channel {
    Bm25,
    Trigram,
}

/// `?start, ?start+1, …` — `n` параметров.
fn placeholders(start: usize, n: usize) -> String {
    (start..start + n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Таблица FTS, ранжирование и размер выдачи канала.
fn channel_spec(ch: Channel) -> (&'static str, &'static str, i64) {
    match ch {
        Channel::Bm25 => ("chunks_fts", "bm25(chunks_fts, 1.0, 0.5, 0.3)", BM25_LIMIT),
        Channel::Trigram => ("chunks_fts_tri", "bm25(chunks_fts_tri)", TRIGRAM_LIMIT),
    }
}

/// Условие области для `chunks c` с [`TARGET_JOIN`]: `?2` — префикс пути или NULL,
/// пространства — с `?3`, за ними [`TargetArgs`]: «узлы проходят», JSON видов записей,
/// JSON статусов или NULL, область или NULL (см. [`scope_args`]). Префикс сравнивается через
/// substr, а не LIKE: LIKE в SQLite нечувствителен к регистру ASCII и требует экранирования `%` и
/// `_`. У записи пути нет: при префиксе она не проходит.
fn scope_sql(n_spaces: usize) -> String {
    let sp = placeholders(3, n_spaces);
    let (notes, kinds, statuses, area) = (3 + n_spaces, 4 + n_spaces, 5 + n_spaces, 6 + n_spaces);
    let (cert, by, file, ik) = (7 + n_spaces, 8 + n_spaces, 9 + n_spaces, 10 + n_spaces);
    format!(
        "((c.node_id IS NOT NULL AND ?{notes} AND ?{by} IS NULL AND n.deleted_at IS NULL
           AND n.space_id IN ({sp})
           AND (?2 IS NULL OR n.path = ?2 OR substr(n.path, 1, length(?2) + 1) = ?2 || '/')
           AND (?{area} IS NULL OR n.area = ?{area})
           AND (?{cert} IS NULL OR n.certainty IN (SELECT value FROM json_each(?{cert})))
           AND (?{file} IS NULL OR EXISTS (SELECT 1 FROM file_refs fr WHERE fr.owner_kind = 'node'
                AND fr.owner_id = n.id AND fr.path = ?{file})))
          OR (c.entry_id IS NOT NULL AND ?2 IS NULL AND e.deleted_at IS NULL
           AND e.space_id IN ({sp})
           AND e.kind IN (SELECT value FROM json_each(?{kinds}))
           AND (e.status IS NULL OR ?{statuses} IS NULL
                OR e.status IN (SELECT value FROM json_each(?{statuses})))
           AND (?{area} IS NULL OR e.area = ?{area})
           AND (?{cert} IS NULL OR e.certainty IN (SELECT value FROM json_each(?{cert})))
           AND (?{file} IS NULL OR EXISTS (SELECT 1 FROM file_refs fr WHERE fr.owner_kind = 'entry'
                AND fr.owner_id = e.id AND fr.path = ?{file}))
           AND (?{ik} IS NULL OR e.kind <> 'insight'
                OR (json_type(e.data, '$.kind') = 'text'
                    AND json_extract(e.data, '$.kind') IN (SELECT value FROM json_each(?{ik}))))
           AND (?{by} IS NULL OR (e.kind = 'insight'
                AND CASE WHEN json_type(e.data, '$.kind') = 'text'
                         THEN json_extract(e.data, '$.kind') END = 'decision'
                AND CASE WHEN json_type(e.data, '$.decision.decided_by') = 'text'
                         THEN json_extract(e.data, '$.decision.decided_by')
                         ELSE CASE WHEN e.author_kind = 'agent' THEN 'agent'
                                  ELSE 'human' END END = ?{by}))))"
    )
}

/// Параметры [`scope_sql`] после `?1`: префикс, пространства, фильтр целей.
fn scope_args<'a>(
    first: &'a dyn ToSql,
    prefix: &'a Option<&'a str>,
    spaces: &'a [Id],
    t: &'a TargetArgs,
) -> Vec<&'a dyn ToSql> {
    let mut args: Vec<&dyn ToSql> = vec![first, prefix];
    args.extend(spaces.iter().map(|s| s as &dyn ToSql));
    args.push(&t.notes);
    args.push(&t.kinds);
    args.push(&t.statuses);
    args.push(&t.area);
    args.push(&t.certainty);
    args.push(&t.decided_by);
    args.push(&t.file);
    args.push(&t.insight_kinds);
    args
}

/// JSON-массив чисел для `json_each`: один текст SQL при любом размере списка.
/// Запросы по такому списку и по MATCH используют `CROSS JOIN` — он фиксирует
/// порядок соединения в SQLite: при одном пространстве планировщик иначе
/// обходит все узлы пространства по индексу (`nodes_parent`), что в десятки раз дольше.
fn json_ids(ids: &[i64]) -> String {
    let items: Vec<String> = ids.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(","))
}

/// Кандидат канала: `(seq, цель, это L1 заметки или записи, bm25 канала)`.
type Cand = (i64, TargetRef, bool, f64);

/// Выполняет канал: кандидаты по убыванию релевантности канала.
///
/// Ранжирование с соединением `chunks`/`nodes` для каждого совпадения дорого
/// (на 30 тыс. чанков частое слово — ~30 мс), поэтому сначала берётся
/// глобальный top `limit × OVERSAMPLE` только из индекса FTS, затем к нему
/// применяется фильтр области. Порядок фильтр сохраняет, поэтому результат
/// точный, если после фильтра осталось не меньше `limit`; иначе (большая часть
/// совпадений вне области) выполняется полный запрос с фильтром.
fn run_channel(
    c: &Connection,
    ch: Channel,
    fts: &str,
    prefix: Option<&str>,
    spaces: &[Id],
    targets: &TargetArgs,
) -> Result<Vec<Cand>, StoreError> {
    let (table, order, limit) = channel_spec(ch);
    let over = limit * OVERSAMPLE;
    let mut st = c.prepare_cached(&format!(
        "SELECT rowid, {order} FROM {table} WHERE {table} MATCH ?1 ORDER BY {order}, rowid LIMIT {over}"
    ))?;
    let top_raw: Vec<(i64, f64)> = st
        .query_map([fts], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let top: Vec<i64> = top_raw.iter().map(|t| t.0).collect();
    if top.is_empty() {
        return Ok(Vec::new());
    }

    let ids = json_ids(&top);
    let args = scope_args(&ids, &prefix, spaces, targets);
    let mut st = c.prepare_cached(&format!(
        "SELECT c.seq, c.node_id, c.entry_id, c.level = 'note'
         FROM json_each(?1) j
         CROSS JOIN chunks c ON c.seq = j.value
         {TARGET_JOIN}
         WHERE {}",
        scope_sql(spaces.len())
    ))?;
    let allowed: HashMap<i64, (TargetRef, bool)> = st
        .query_map(params_from_iter(args), |r| {
            Ok((r.get(0)?, (chunk_target(r.get(1)?, r.get(2)?)?, r.get(3)?)))
        })?
        .collect::<Result<_, _>>()?;
    let out: Vec<Cand> = top_raw
        .iter()
        .filter_map(|(seq, raw)| {
            allowed
                .get(seq)
                .map(|(node, note)| (*seq, *node, *note, *raw))
        })
        .take(limit as usize)
        .collect();
    if out.len() as i64 == limit || (top.len() as i64) < over {
        return Ok(out);
    }

    let args = scope_args(&fts, &prefix, spaces, targets);
    let mut st = c.prepare_cached(&format!(
        "SELECT f.rowid, c.node_id, c.entry_id, c.level = 'note', {order}
         FROM {table} f
         CROSS JOIN chunks c ON c.seq = f.rowid
         {TARGET_JOIN}
         WHERE {table} MATCH ?1 AND {}
         ORDER BY {order}, f.rowid
         LIMIT {limit}",
        scope_sql(spaces.len())
    ))?;
    let rows = st.query_map(params_from_iter(args), |r| {
        Ok((
            r.get(0)?,
            chunk_target(r.get(1)?, r.get(2)?)?,
            r.get(3)?,
            r.get(4)?,
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Итоговый кандидат после слияния.
struct Ranked {
    seq: i64,
    score: f64,
    /// Найден каналом bm25 (сниппет — через FTS5 `snippet()`).
    in_bm25: bool,
    /// Найден каналом `alias` (сниппет — через `snippet()`, если не найден `bm25`).
    in_alias: bool,
}

/// Полнотекстовый поиск по чанкам живых узлов и записей в пределах `space_ids`,
/// `path_prefix` и фильтра целей.
/// Запрос без слов (и без фразы для trigram) — пустая выдача.
/// `top_k` вне 1..=100, пустой `space_ids` или запрос длиннее 512 символов → `BadInput`.
pub async fn search_fts(db: &Db, q: FtsQuery) -> Result<Vec<FtsHit>, StoreError> {
    let (spaces, prefix) = check_query(&q)?;
    db.read(move |c| {
        // один снимок данных для обоих шагов; транзакция только читает и
        // откатывается при выходе
        let tx = c.unchecked_transaction()?;
        // канал alias — только у гибридного поиска ([`search_fts_channels`])
        let ch = run_channels(
            &tx,
            &q.text,
            prefix.as_deref(),
            &spaces,
            &q.targets.args(),
            &[],
        )?;
        let ranked = merge(ch.a, ch.b, q.top_k);
        fetch_hits(&tx, &ranked, ch.a_fts.as_deref(), None, &ch.needles)
    })
    .await
}

/// Каналы FTS по отдельности: кандидаты в порядке канала (лучший первый).
#[derive(Debug, Clone, Default)]
pub struct FtsChannels {
    /// `bm25` (unicode61), до 50.
    pub bm25: Vec<FtsHit>,
    /// `trigram`, до 20: только если в запросе есть идентификаторы (или bm25 пуст).
    pub trigram: Vec<FtsHit>,
    /// Канал `alias` (словарь, M4.5), до 50; пуст без групп расширения. Чанк, найденный и
    /// основным каналом, есть в обоих списках.
    pub alias: Vec<FtsHit>,
    /// `alias` получен ослабленным запросом (любое слово).
    pub alias_relaxed: bool,
    /// `bm25` получен ослабленным запросом (любое слово): строгий (все слова) ничего не
    /// нашёл. Такие хиты слабее — гибрид даёт им отдельный вес (`search.w_fts_relaxed`).
    pub relaxed: bool,
}

/// Кандидаты каналов `bm25` и `trigram` без слияния (гибридный поиск): та же область и
/// те же правила запроса, что у [`search_fts`], но без лимита на узел, без `top_k` и без
/// правила для L1 заметки — их применяет `zam-search`. Чанк, найденный обоими каналами,
/// есть в обоих списках. `top_k` запроса не используется, но проверяется, как и
/// остальные поля.
pub async fn search_fts_channels(db: &Db, q: FtsQuery) -> Result<FtsChannels, StoreError> {
    let (spaces, prefix) = check_query(&q)?;
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let ch = run_channels(
            &tx,
            &q.text,
            prefix.as_deref(),
            &spaces,
            &q.targets.args(),
            &q.alias_groups,
        )?;
        let in_a: HashSet<i64> = ch.a.iter().map(|c| c.0).collect();
        let in_alias: HashSet<i64> = ch.alias.iter().map(|c| c.0).collect();
        // текст и сниппеты — одним шагом для объединения каналов; сниппет чанка,
        // найденного bm25, — из FTS5 `snippet()`
        let mut all: Vec<Ranked> = Vec::with_capacity(ch.a.len() + ch.b.len() + ch.alias.len());
        for (seq, _, _, _) in ch.a.iter().chain(&ch.alias).chain(&ch.b) {
            if !all.iter().any(|r| r.seq == *seq) {
                all.push(Ranked {
                    seq: *seq,
                    score: 0.0,
                    in_bm25: in_a.contains(seq),
                    in_alias: in_alias.contains(seq),
                });
            }
        }
        let hits: HashMap<String, FtsHit> = fetch_hits(
            &tx,
            &all,
            ch.a_fts.as_deref(),
            ch.alias_fts.as_deref(),
            &ch.needles,
        )?
        .into_iter()
        .map(|h| (h.chunk_id.clone(), h))
        .collect();
        let seq_ids: HashMap<i64, String> = chunk_ids_of(&tx, &all)?;
        let take = |l: &[Cand]| -> Vec<FtsHit> {
            l.iter()
                .enumerate()
                .filter_map(|(i, (seq, _, _, raw))| {
                    let mut h = hits.get(seq_ids.get(seq)?)?.clone();
                    h.score = 1.0 / (i + 1) as f64;
                    h.raw_score = *raw;
                    Some(h)
                })
                .collect()
        };
        Ok(FtsChannels {
            bm25: take(&ch.a),
            trigram: take(&ch.b),
            alias: take(&ch.alias),
            alias_relaxed: ch.alias_relaxed && !ch.alias.is_empty(),
            relaxed: ch.relaxed && !ch.a.is_empty(),
        })
    })
    .await
}

/// `seq → chunks.id` для кандидатов.
fn chunk_ids_of(c: &Connection, ranked: &[Ranked]) -> Result<HashMap<i64, String>, StoreError> {
    if ranked.is_empty() {
        return Ok(HashMap::new());
    }
    let seqs: Vec<i64> = ranked.iter().map(|r| r.seq).collect();
    let mut st = c.prepare_cached(
        "SELECT c.seq, c.id FROM json_each(?1) j CROSS JOIN chunks c ON c.seq = j.value",
    )?;
    let rows = st.query_map([json_ids(&seqs)], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Кандидаты каналов, запрос bm25 (для `snippet()`) и подстроки trigram.
struct Channels<'q> {
    a: Vec<Cand>,
    b: Vec<Cand>,
    /// Канал `alias`; пуст без групп расширения.
    alias: Vec<Cand>,
    a_fts: Option<String>,
    /// Запрос канала `alias` (для `snippet()`).
    alias_fts: Option<String>,
    /// `a` — по ослабленному запросу.
    relaxed: bool,
    /// `alias` — по ослабленному запросу.
    alias_relaxed: bool,
    needles: Vec<&'q str>,
}

/// Канал A (строгий запрос, при пустой выдаче — ослабленный) и канал B.
fn run_channels<'q>(
    tx: &Connection,
    text: &'q str,
    prefix: Option<&str>,
    spaces: &[Id],
    t: &TargetArgs,
    groups: &[AliasGroup],
) -> Result<Channels<'q>, StoreError> {
    let mut a_fts = sanitize_fts(text);
    let mut relaxed = false;
    let mut a = match &a_fts {
        Some(fts) => run_channel(tx, Channel::Bm25, fts, prefix, spaces, t)?,
        None => Vec::new(),
    };
    if a.is_empty() && terms(text).len() > 1 {
        a_fts = sanitize_fts_any(text);
        relaxed = true;
        if let Some(fts) = &a_fts {
            a = run_channel(tx, Channel::Bm25, fts, prefix, spaces, t)?;
        }
    }
    let mut alias: Vec<Cand> = Vec::new();
    let mut alias_fts_q: Option<String> = None;
    let mut alias_relaxed = false;
    if !groups.is_empty() {
        alias_fts_q = alias_fts(text, groups, " AND ");
        if let Some(fts) = &alias_fts_q {
            alias = run_channel(tx, Channel::Bm25, fts, prefix, spaces, t)?;
        }
        if alias.is_empty() && terms(text).len() > 1 {
            alias_fts_q = alias_fts(text, groups, " OR ");
            alias_relaxed = true;
            if let Some(fts) = &alias_fts_q {
                alias = run_channel(tx, Channel::Bm25, fts, prefix, spaces, t)?;
            }
        }
    }
    let (b, needles) = match trigram_query(text, a.is_empty()) {
        Some((fts, needles)) => (
            run_channel(tx, Channel::Trigram, &fts, prefix, spaces, t)?,
            needles,
        ),
        None => (Vec::new(), Vec::new()),
    };
    Ok(Channels {
        a,
        b,
        alias,
        a_fts,
        alias_fts: alias_fts_q,
        relaxed,
        alias_relaxed,
        needles,
    })
}

/// Проверка запроса: `(пространства без повторов, нормализованный префикс)`.
fn check_query(q: &FtsQuery) -> Result<(Vec<Id>, Option<String>), StoreError> {
    if !(1..=TOP_K_MAX).contains(&q.top_k) {
        return Err(StoreError::BadInput(format!(
            "top_k должен быть в 1..={TOP_K_MAX}, получено {}",
            q.top_k
        )));
    }
    if q.space_ids.is_empty() {
        return Err(StoreError::BadInput(
            "не задано ни одного пространства".into(),
        ));
    }
    if q.text.chars().count() > QUERY_MAX_CHARS {
        return Err(StoreError::BadInput(format!(
            "запрос длиннее {QUERY_MAX_CHARS} символов"
        )));
    }
    let mut spaces = q.space_ids.clone();
    spaces.sort();
    spaces.dedup();
    Ok((spaces, norm_prefix(q.path_prefix.as_deref())))
}

/// Префикс пути без завершающих `/`; пустой — `None`.
fn norm_prefix(p: Option<&str>) -> Option<String> {
    p.map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// Область для [`live_chunks`].
#[derive(Debug, Clone)]
pub struct ChunkScope {
    /// Разрешённые пространства; пусто — пустая выдача.
    pub space_ids: Vec<Id>,
    /// Префикс пути (как в [`FtsQuery`]).
    pub path_prefix: Option<String>,
    /// Только цели, которые векторизуются (`index_state <> 'excluded'`): для векторных
    /// хитов — точка исключённого узла могла остаться в Qdrant.
    pub indexable_only: bool,
    /// Виды и статусы целей (как в [`FtsQuery`]).
    pub targets: TargetFilter,
}

/// Чанки `ids` живых узлов и записей области (с текстом, без сниппета, `score = 0`) в
/// порядке `ids`; отсутствующие (старая ревизия, цель удалена или в корзине, вне области
/// или фильтра целей, исключена при `indexable_only`) пропускаются. Источник истины для
/// векторных хитов: точки в Qdrant могут пережить неудачное задание `delete`.
pub async fn live_chunks(
    db: &Db,
    ids: Vec<String>,
    scope: ChunkScope,
) -> Result<Vec<FtsHit>, StoreError> {
    if ids.is_empty() || scope.space_ids.is_empty() {
        return Ok(Vec::new());
    }
    let prefix = norm_prefix(scope.path_prefix.as_deref());
    let list = serde_json::Value::from(ids.clone()).to_string();
    let t = scope.targets.args();
    db.read(move |c| {
        let prefix = prefix.as_deref();
        let args = scope_args(&list, &prefix, &scope.space_ids, &t);
        let mut st = c.prepare_cached(&format!(
            "SELECT {HIT_COLS}
             FROM json_each(?1) j
             CROSS JOIN chunks c ON c.id = j.value
             {TARGET_JOIN}
             WHERE {} {}",
            scope_sql(scope.space_ids.len()),
            if scope.indexable_only {
                "AND coalesce(n.index_state, e.index_state) <> 'excluded'"
            } else {
                ""
            }
        ))?;
        let rows = st.query_map(params_from_iter(args), |r| hit_row(r, 0))?;
        let mut by_id: HashMap<String, FtsHit> = rows
            .map(|h| h.map(|h| (h.chunk_id.clone(), h)))
            .collect::<Result<_, _>>()?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    })
    .await
}

/// Папка векторного канала папок, прошедшая проверку по SQLite ([`live_folders`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRow {
    pub id: Id,
    pub space_id: Id,
    pub path: String,
    /// Свёртка L0 папки (§8.3), замаскированная сканером секретов.
    pub l0: Option<String>,
    /// Живых (не удалённых и не в корзине) детей первого уровня.
    pub child_count: i64,
}

/// Папки `ids` (точки уровня `folder` векторной выдачи) в порядке `ids`: только живые
/// (не удалённые, не в корзине) папки пространств `space_ids`, кроме Human, под префиксом
/// `path_prefix` (по целым сегментам; сама папка с этим путём проходит). Отсутствующие и
/// не прошедшие проверку пропускаются: точка в Qdrant может пережить удаление папки.
/// Пустой `space_ids` — пустая выдача.
pub async fn live_folders(
    db: &Db,
    ids: Vec<Id>,
    space_ids: &[Id],
    path_prefix: Option<&str>,
) -> Result<Vec<FolderRow>, StoreError> {
    if ids.is_empty() || space_ids.is_empty() {
        return Ok(Vec::new());
    }
    let prefix = norm_prefix(path_prefix);
    let spaces = space_ids.to_vec();
    let list =
        serde_json::Value::from(ids.iter().map(Id::to_string).collect::<Vec<_>>()).to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT n.id, n.space_id, n.path, n.l0,
                    (SELECT count(*) FROM nodes ch
                     WHERE ch.space_id = n.space_id AND ch.parent_id = n.id
                       AND ch.deleted_at IS NULL)
             FROM json_each(?1) j
             CROSS JOIN nodes n ON n.id = j.value
             JOIN spaces s ON s.id = n.space_id
             WHERE n.kind = 'folder' AND n.deleted_at IS NULL AND s.kind <> 'human'
               AND n.space_id IN ({})
               AND (?2 IS NULL OR n.path = ?2 OR substr(n.path, 1, length(?2) + 1) = ?2 || '/')",
            placeholders(3, spaces.len())
        ))?;
        let mut args: Vec<&dyn ToSql> = vec![&list, &prefix];
        args.extend(spaces.iter().map(|s| s as &dyn ToSql));
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok(FolderRow {
                id: r.get(0)?,
                space_id: r.get(1)?,
                path: r.get(2)?,
                l0: r.get(3)?,
                child_count: r.get(4)?,
            })
        })?;
        let mut by_id: HashMap<Id, FolderRow> = rows
            .map(|f| f.map(|f| (f.id, f)))
            .collect::<Result<_, _>>()?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    })
    .await
}

/// Лист заметки или записи журнала для замены сводки (§9.2 п. 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafChunk {
    pub chunk_id: String,
    pub heading_path: String,
    pub ordinal: i64,
    pub text: String,
    pub context_prefix: String,
    /// Ключ кэша `embeddings` (`chunks.embed_key`): эмбеддинг листа без чтения текста.
    pub embed_key: String,
}

/// Листы (`level = 'chunk'`) узла по порядку.
pub async fn node_leaves(db: &Db, node: Id) -> Result<Vec<LeafChunk>, StoreError> {
    let t = TargetRef {
        kind: TargetKind::Node,
        id: node,
    };
    Ok(leaves_of_targets(db, vec![t], true)
        .await?
        .remove(&t)
        .unwrap_or_default())
}

/// Листы нескольких целей (узлов и записей журналов) одним чтением: `цель → листы по
/// порядку`; цели без листов в ответе нет (замена сводок листами в поиске — без запроса на
/// каждую сводку). `with_text = false` — без `text` и `context_prefix` (пустые строки):
/// тексты сотен листов не читаются, если нужны только пути, порядок и ключи кэша эмбеддингов.
pub async fn leaves_of_targets(
    db: &Db,
    targets: Vec<TargetRef>,
    with_text: bool,
) -> Result<HashMap<TargetRef, Vec<LeafChunk>>, StoreError> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    db.read(move |c| {
        // один снимок и один подготовленный запрос по индексу `chunks_node` (`chunks_entry`)
        // на цель: соединение с json_each здесь медленнее (сортировка всех листов)
        let tx = c.unchecked_transaction()?;
        let sql = |col: &str| {
            format!(
                "SELECT id, heading_path, ordinal,
                        CASE WHEN ?2 THEN text ELSE '' END,
                        CASE WHEN ?2 THEN context_prefix ELSE '' END,
                        embed_key
                 FROM chunks WHERE {col} = ?1 AND {col} IS NOT NULL AND level = 'chunk'
                 ORDER BY ordinal"
            )
        };
        let mut by_node = tx.prepare_cached(&sql("node_id"))?;
        let mut by_entry = tx.prepare_cached(&sql("entry_id"))?;
        let mut out: HashMap<TargetRef, Vec<LeafChunk>> = HashMap::new();
        for t in targets {
            let st = match t.kind {
                TargetKind::Node => &mut by_node,
                TargetKind::Entry => &mut by_entry,
            };
            let rows = st.query_map(params![t.id, with_text], |r| {
                Ok(LeafChunk {
                    chunk_id: r.get(0)?,
                    heading_path: r.get(1)?,
                    ordinal: r.get(2)?,
                    text: r.get(3)?,
                    context_prefix: r.get(4)?,
                    // NULL только у строк в обход записи чанков (база дозаполняется при открытии)
                    embed_key: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                })
            })?;
            let leaves: Vec<LeafChunk> = rows.collect::<Result<_, _>>()?;
            if !leaves.is_empty() {
                out.insert(t, leaves);
            }
        }
        Ok(out)
    })
    .await
}

/// Первый лист (`level = 'chunk'`, наименьший `ordinal`) каждой цели — узла или записи:
/// `цель → id чанка`; целей без листов в ответе нет. Карточка соседа по графу в
/// `expand=graph` поиска (область и фильтры проверяет [`live_chunks`]).
pub async fn first_leaves(
    db: &Db,
    targets: Vec<TargetRef>,
) -> Result<HashMap<TargetRef, String>, StoreError> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let mut by_node = tx.prepare_cached(
            "SELECT id FROM chunks WHERE node_id = ?1 AND level = 'chunk' ORDER BY ordinal LIMIT 1",
        )?;
        let mut by_entry = tx.prepare_cached(
            "SELECT id FROM chunks WHERE entry_id = ?1 AND level = 'chunk' ORDER BY ordinal LIMIT 1",
        )?;
        let mut out = HashMap::new();
        for t in targets {
            let st = match t.kind {
                TargetKind::Node => &mut by_node,
                TargetKind::Entry => &mut by_entry,
            };
            if let Some(id) = st
                .query_row([t.id], |r| r.get::<_, String>(0))
                .optional()?
            {
                out.insert(t, id);
            }
        }
        Ok(out)
    })
    .await
}

/// Лист цели (узла или записи журнала) с числом найденных в нём слов запроса
/// ([`leaf_term_hits`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafHit {
    pub target: TargetRef,
    pub chunk_id: String,
    pub heading_path: String,
    pub ordinal: i64,
    /// Сколько слов запроса найдено в листе (≥ 1).
    pub hits: usize,
}

/// Листы целей `targets` (узлов и записей журналов), в которых FTS5 находит слова запроса
/// (`needles`, каждое — как запрос [`sanitize_fts`]), с числом найденных слов; по цели и
/// `ordinal`. Листы без совпадений в ответ не входят. Тексты не читаются; в одной читающей
/// транзакции: листы целей (по индексам `chunks_node` и `chunks_entry`) и по одному проходу
/// индекса FTS на слово — только `rowid` совпадений, пересечение с листами — в памяти.
///
/// Соединение совпадений с `chunks` и фильтр по узлам в SQL (как было) читали строку
/// `chunks` на каждое совпадение: на 10 тыс. заметок частое слово («и», «the») — 16 тыс.
/// строк и 25 мс; проверка `MATCH … AND rowid = c.seq` по каждому листу — 0,05–0,1 мс на
/// лист и слово, сотни листов у окна сводок. Проход одних `rowid` — 2–5 мс на самое
/// частое слово (замеры `cargo xtask bench`, задача 15 M2).
pub async fn leaf_term_hits(
    db: &Db,
    targets: Vec<TargetRef>,
    needles: Vec<String>,
) -> Result<Vec<LeafHit>, StoreError> {
    if targets.is_empty() || needles.is_empty() {
        return Ok(Vec::new());
    }
    let list = |kind: TargetKind| {
        serde_json::Value::from(
            targets
                .iter()
                .filter(|t| t.kind == kind)
                .map(|t| t.id.to_string())
                .collect::<Vec<_>>(),
        )
        .to_string()
    };
    let lists = [
        (TargetKind::Node, list(TargetKind::Node)),
        (TargetKind::Entry, list(TargetKind::Entry)),
    ];
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let mut leaves: HashMap<i64, LeafHit> = HashMap::new();
        for (kind, list) in &lists {
            let col = match kind {
                TargetKind::Node => "node_id",
                TargetKind::Entry => "entry_id",
            };
            let mut st = tx.prepare_cached(&format!(
                "SELECT c.seq, c.id, c.{col}, c.heading_path, c.ordinal FROM chunks c
                 WHERE c.level = 'chunk' AND c.{col} IS NOT NULL
                   AND c.{col} IN (SELECT value FROM json_each(?1))"
            ))?;
            let rows = st.query_map([list], |r| {
                Ok((
                    r.get(0)?,
                    LeafHit {
                        chunk_id: r.get(1)?,
                        target: TargetRef {
                            kind: *kind,
                            id: r.get(2)?,
                        },
                        heading_path: r.get(3)?,
                        ordinal: r.get(4)?,
                        hits: 0,
                    },
                ))
            })?;
            for row in rows {
                let (seq, hit) = row?;
                leaves.insert(seq, hit);
            }
        }
        if leaves.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = tx.prepare_cached("SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1")?;
        for n in needles.iter().take(MAX_TERMS) {
            let Some(fts) = sanitize_fts(n) else {
                continue;
            };
            for seq in st.query_map([fts], |r| r.get::<_, i64>(0))? {
                if let Some(l) = leaves.get_mut(&seq?) {
                    l.hits += 1;
                }
            }
        }
        let mut v: Vec<LeafHit> = leaves.into_values().filter(|l| l.hits > 0).collect();
        v.sort_by(|a, b| a.target.cmp(&b.target).then(a.ordinal.cmp(&b.ordinal)));
        Ok(v)
    })
    .await
}

/// Тексты соседних листов того же узла: `(предыдущий, следующий)` по `ordinal`
/// (`expand=neighbors`). Нет чанка или он не лист — `(None, None)`.
pub async fn neighbor_leaves(
    db: &Db,
    chunk_id: String,
) -> Result<(Option<String>, Option<String>), StoreError> {
    db.read(move |c| {
        let one = |sql: &str| -> Result<Option<String>, StoreError> {
            let mut st = c.prepare_cached(sql)?;
            let mut rows = st.query([&chunk_id])?;
            Ok(match rows.next()? {
                Some(r) => Some(r.get(0)?),
                None => None,
            })
        };
        let prev = one("SELECT n.text FROM chunks c JOIN chunks n
               ON n.node_id = c.node_id AND n.level = 'chunk' AND n.ordinal < c.ordinal
             WHERE c.id = ?1 AND c.level = 'chunk' ORDER BY n.ordinal DESC LIMIT 1")?;
        let next = one("SELECT n.text FROM chunks c JOIN chunks n
               ON n.node_id = c.node_id AND n.level = 'chunk' AND n.ordinal > c.ordinal
             WHERE c.id = ?1 AND c.level = 'chunk' ORDER BY n.ordinal LIMIT 1")?;
        Ok((prev, next))
    })
    .await
}

/// Weighted RRF по чанкам, L1 заметки — только у целей без найденных листов,
/// лимит на цель, top_k.
fn merge(a: Vec<Cand>, b: Vec<Cand>, top_k: usize) -> Vec<Ranked> {
    let with_leaf: HashSet<TargetRef> = a
        .iter()
        .chain(&b)
        .filter(|(_, _, note, _)| !note)
        .map(|(_, node, _, _)| *node)
        .collect();
    let keep = |c: &Cand| !c.2 || !with_leaf.contains(&c.1);
    let strip = |l: Vec<Cand>| -> Vec<(i64, TargetRef)> {
        l.into_iter()
            .filter(keep)
            .map(|(seq, node, _, _)| (seq, node))
            .collect()
    };
    rrf(strip(a), strip(b), top_k)
}

/// Weighted RRF по чанкам, лимит на цель, top_k.
fn rrf(a: Vec<(i64, TargetRef)>, b: Vec<(i64, TargetRef)>, top_k: usize) -> Vec<Ranked> {
    let mut by_seq: HashMap<i64, (TargetRef, f64, bool)> = HashMap::new();
    for (w, list, is_a) in [(W_BM25, a, true), (W_TRIGRAM, b, false)] {
        for (i, (seq, node)) in list.into_iter().enumerate() {
            let e = by_seq.entry(seq).or_insert((node, 0.0, false));
            e.1 += w / (RRF_K + (i + 1) as f64);
            e.2 |= is_a;
        }
    }
    let mut all: Vec<(i64, (TargetRef, f64, bool))> = by_seq.into_iter().collect();
    all.sort_by(|(x, (_, sx, _)), (y, (_, sy, _))| sy.total_cmp(sx).then(x.cmp(y)));

    let mut per_node: HashMap<TargetRef, usize> = HashMap::new();
    all.into_iter()
        .filter(|(_, (node, _, _))| {
            let n = per_node.entry(*node).or_default();
            *n += 1;
            *n <= PER_NODE_CAP
        })
        .take(top_k)
        .map(|(seq, (_, score, in_bm25))| Ranked {
            seq,
            score,
            in_bm25,
            in_alias: false,
        })
        .collect()
}

/// Шаг 2: текст, метаданные и сниппеты итоговых чанков в порядке `ranked`.
/// `bm25_fts` — запрос канала A, по которому строится `snippet()`;
/// `needles` — подстроки для сниппета trigram-хитов.
fn fetch_hits(
    c: &Connection,
    ranked: &[Ranked],
    bm25_fts: Option<&str>,
    alias_fts: Option<&str>,
    needles: &[&str],
) -> Result<Vec<FtsHit>, StoreError> {
    if ranked.is_empty() {
        return Ok(Vec::new());
    }
    let seqs: Vec<i64> = ranked.iter().map(|r| r.seq).collect();
    let mut st = c.prepare_cached(&format!(
        "SELECT c.seq, {HIT_COLS}
         FROM json_each(?1) j
         CROSS JOIN chunks c ON c.seq = j.value
         {TARGET_JOIN}"
    ))?;
    let rows = st.query_map([json_ids(&seqs)], |r| {
        Ok((r.get::<_, i64>(0)?, hit_row(r, 1)?))
    })?;
    let mut by_seq: HashMap<i64, FtsHit> = rows.collect::<Result<_, _>>()?;

    let mut snippets: HashMap<i64, String> = HashMap::new();
    // snippet() требует курсора MATCH, поэтому запрос повторяется одним
    // проходом по совпадениям; `+rowid` не отдаётся FTS5 как ограничение:
    // иначе для каждого rowid из списка MATCH (с раскрытием префиксов)
    // вычисляется заново — на 30 тыс. чанков в 10 раз дольше.
    // Чанк, найденный `bm25`, — сниппет по его запросу; найденный только `alias` — по запросу
    // канала `alias`.
    let a_seqs: Vec<i64> = ranked.iter().filter(|r| r.in_bm25).map(|r| r.seq).collect();
    let al_seqs: Vec<i64> = ranked
        .iter()
        .filter(|r| !r.in_bm25 && r.in_alias)
        .map(|r| r.seq)
        .collect();
    let (ms, me) = (MARK_START.to_string(), MARK_END.to_string());
    for (fts, seqs) in [(bm25_fts, a_seqs), (alias_fts, al_seqs)] {
        let Some(fts) = fts else { continue };
        if seqs.is_empty() {
            continue;
        }
        let mut st = c.prepare_cached(&format!(
            "SELECT rowid, snippet(chunks_fts, 0, ?1, ?2, '…', {SNIPPET_TOKENS})
             FROM chunks_fts
             WHERE chunks_fts MATCH ?3 AND +rowid IN (SELECT value FROM json_each(?4))"
        ))?;
        let rows = st.query_map(params![ms, me, fts, json_ids(&seqs)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (seq, snip) = row?;
            snippets.insert(seq, snip);
        }
    }

    Ok(ranked
        .iter()
        .filter_map(|r| {
            let mut h = by_seq.remove(&r.seq)?;
            h.score = r.score;
            h.snippet = snippets
                .remove(&r.seq)
                .unwrap_or_else(|| text_snippet(&h.text, needles));
            Some(h)
        })
        .collect())
}

/// Символ в нижнем регистре (первый символ `to_lowercase`): индексы символов сохраняются.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Первое вхождение `pat` в `folded` (текст после [`fold`]) без учёта регистра: индекс
/// символа. Текст сворачивается один раз на все подстроки, а не на каждое сравнение.
fn find_ci(folded: &[char], pat: &str) -> Option<usize> {
    let pat: Vec<char> = pat.chars().map(fold).collect();
    if pat.is_empty() || pat.len() > folded.len() {
        return None;
    }
    folded.windows(pat.len()).position(|w| w == pat.as_slice())
}

/// Сниппет из текста: до [`SNIPPET_CHARS`] символов вокруг первого вхождения
/// первой найденной из `needles` подстроки без учёта регистра, вхождение — между
/// маркерами. Ничего не нашлось — начало текста. Режется только по границам символов.
pub fn text_snippet(text: &str, needles: &[&str]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let folded: Vec<char> = chars.iter().copied().map(fold).collect();
    let (start, len) = needles
        .iter()
        .find_map(|n| find_ci(&folded, n).map(|i| (i, n.chars().count())))
        .unwrap_or((0, 0));
    let budget = SNIPPET_CHARS.saturating_sub(len);
    let mut from = start.saturating_sub(budget / 2);
    let to = (start + len + (budget - (start - from))).min(chars.len());
    // остаток бюджета у конца текста отдаётся началу
    from = from.min(start.saturating_sub(budget.saturating_sub(to - (start + len))));

    let mut s = String::new();
    if from > 0 {
        s.push('…');
    }
    s.extend(&chars[from..start]);
    if len > 0 {
        s.push(MARK_START);
        s.extend(&chars[start..start + len]);
        s.push(MARK_END);
    }
    s.extend(&chars[start + len..to]);
    if to < chars.len() {
        s.push('…');
    }
    s
}

/// Запрос [`pending_count`]: условия совпадают с частичными индексами `nodes_pending`
/// (миграция 0006) и `entries_pending` (0008), поэтому считаются только цели в очереди.
/// `INDEXED BY` — с миграцией 0012 (задача 19b M3) планировщик без статистики выбирал
/// `nodes_live_children (space_id=?)`, то есть обход всех живых узлов области.
const PENDING_COUNT_SQL: &str = "SELECT (SELECT count(*) FROM nodes n INDEXED BY nodes_pending
     WHERE n.kind = 'note' AND n.index_state = 'pending' AND n.deleted_at IS NULL
       AND n.space_id IN (SELECT value FROM json_each(?1))
       AND (?2 IS NULL OR n.path = ?2 OR substr(n.path, 1, length(?2) + 1) = ?2 || '/'))
     + (SELECT count(*) FROM entries e
        WHERE e.index_state = 'pending' AND e.deleted_at IS NULL AND ?2 IS NULL
          AND e.space_id IN (SELECT value FROM json_each(?1)))";

/// Сколько живых заметок и записей журналов в `space_ids` (и под `path_prefix`, если задан;
/// у записей пути нет — при префиксе они не считаются) ждут векторной индексации
/// (`index_state = 'pending'`) — `pending_count` выдачи поиска. Заметки с `index: false`
/// (`excluded`) не считаются.
pub async fn pending_count(
    db: &Db,
    space_ids: Vec<Id>,
    path_prefix: Option<String>,
) -> Result<i64, StoreError> {
    if space_ids.is_empty() {
        return Ok(0);
    }
    let ids = serde_json::Value::from(space_ids.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .to_string();
    let prefix = path_prefix
        .as_deref()
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    db.read(move |c| Ok(c.query_row(PENDING_COUNT_SQL, params![ids, prefix], |r| r.get(0))?))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pending_count` идёт по частичному индексу `nodes_pending`, а не обходом узлов области.
    #[test]
    fn pending_count_uses_partial_index() {
        let c = Connection::open_in_memory().unwrap();
        for m in crate::USER_MIGRATIONS {
            c.execute_batch(m.sql).unwrap();
        }
        let mut st = c
            .prepare(&format!("EXPLAIN QUERY PLAN {PENDING_COUNT_SQL}"))
            .unwrap();
        let plan: Vec<String> = st
            .query_map(params!["[\"x\"]", "a"], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let plan = plan.join("; ");
        assert!(plan.contains("nodes_pending"), "{plan}");
        assert!(plan.contains("entries_pending"), "{plan}");
    }

    #[test]
    fn identifier_detection() {
        assert_eq!(
            identifiers("настройка (core.autocrlf), a_b src/main x-y a:b v42 abc 123 ab. 12"),
            vec![
                "core.autocrlf",
                "a_b",
                "src/main",
                "x-y",
                "a:b",
                "v42",
                "123"
            ]
        );
        for s in ["скрипт bash", "a.", "12", "просто слова", "...", "«ab.»"] {
            assert!(identifiers(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn short_terms_are_exact() {
        assert_eq!(sanitize_fts("в с a").as_deref(), Some(r#""в" "с" "a""#));
        assert_eq!(
            sanitize_fts("файл в git").as_deref(),
            Some(r#""файл"* "в" "git"*"#)
        );
        // символы, а не байты: «ёж» — 2 символа (4 байта)
        assert_eq!(sanitize_fts("ёж").as_deref(), Some(r#""ёж""#));
        assert_eq!(
            sanitize_fts_any("ab cde").as_deref(),
            Some(r#""ab" OR "cde"*"#)
        );
    }

    #[test]
    fn trigram_query_by_identifiers_or_phrase() {
        assert_eq!(
            trigram_query("настройка mygit.core и a\"b.c", false),
            Some((
                r#""mygit.core" OR "a""b.c""#.to_string(),
                vec!["mygit.core", "a\"b.c"]
            ))
        );
        // без идентификаторов — фраза, только при пустом bm25
        assert_eq!(trigram_query(" просто слова ", false), None);
        assert_eq!(
            trigram_query(" просто слова ", true),
            Some((r#""просто слова""#.to_string(), vec!["просто слова"]))
        );
        assert_eq!(trigram_query("ab", true), None);
    }

    #[test]
    fn snippet_window() {
        let m = |s: &str| format!("{MARK_START}{s}{MARK_END}");
        assert_eq!(
            text_snippet("Hello World", &["WORLD"]),
            format!("Hello {}", m("World"))
        );
        assert_eq!(text_snippet("abc", &["zzz"]), "abc");
        // первая найденная подстрока из списка
        assert_eq!(
            text_snippet("a.b c.d", &["x.y", "C.D"]),
            format!("a.b {}", m("c.d"))
        );
        let long = "я".repeat(500);
        let s = text_snippet(&format!("{long}КЛЮЧ{long}"), &["ключ"]);
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.contains(&m("КЛЮЧ")));
        assert_eq!(s.chars().count(), SNIPPET_CHARS + 4);
        // у конца текста окно сдвигается влево
        let s = text_snippet(&format!("{long}ключ"), &["ключ"]);
        assert_eq!(s.chars().count(), 1 + SNIPPET_CHARS + 2);
        assert!(s.ends_with(MARK_END));
    }

    #[test]
    fn alias_query_replaces_words_with_groups() {
        let g = |start, len, with: &[&str]| AliasGroup {
            start,
            len,
            with: with.iter().map(|s| s.to_string()).collect(),
        };
        // слово → группа «слово OR имя OR псевдоним»; короткий псевдоним — точным словом
        let groups = [g(1, 1, &["Выжимка", "db", "кэш выжимок"])];
        assert_eq!(
            alias_fts("про summary тут", &groups, " AND ").as_deref(),
            Some(r#""про"* AND ("summary"* OR "Выжимка"* OR "db" OR "кэш выжимок"*) AND "тут"*"#)
        );
        assert_eq!(
            alias_fts("про summary", &groups, " OR ").as_deref(),
            Some(r#""про"* OR ("summary"* OR "Выжимка"* OR "db" OR "кэш выжимок"*)"#)
        );
        // многословное совпадение заменяет несколько слов одной группой
        let groups = [g(0, 2, &["Кэш"])];
        assert_eq!(
            alias_fts("summary cache тут", &groups, " AND ").as_deref(),
            Some(r#"("summary cache"* OR "Кэш"*) AND "тут"*"#)
        );
        // группа за пределами запроса и повтор альтернативы
        assert_eq!(
            alias_fts("a", &[g(5, 1, &["x"])], " AND ").as_deref(),
            Some(r#""a""#)
        );
        assert_eq!(
            alias_fts("слово", &[g(0, 1, &["Слово"])], " AND ").as_deref(),
            Some(r#"("слово"*)"#)
        );
        assert_eq!(alias_fts("—", &groups, " AND "), None);
        // префикс - по последнему слову фразы, а не по длине всей фразы
        let groups = [g(0, 1, &["Plan B", "Q&A", "db", "Plan Bar"])];
        assert_eq!(
            alias_fts("x", &groups, " AND ").as_deref(),
            Some(r#"("x" OR "Plan B" OR "Q A" OR "db" OR "Plan Bar"*)"#)
        );
    }
}
