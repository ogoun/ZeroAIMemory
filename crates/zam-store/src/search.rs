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
//! Для гибридного поиска (M2) каналы отдаются раздельно — [`search_fts_channels`]: их
//! сливает `zam-search` вместе с векторным каналом и весами из конфигурации; там же
//! правило «сводка → лучший лист». [`live_chunks`] — метаданные и текст векторных хитов
//! с проверкой по SQLite (живой узел, не исключён, в области).
//!
//! Выборка двухшаговая в одной читающей транзакции: каналы отбирают только
//! `rowid` и узел (глобальный top с запасом из индекса FTS, затем фильтр области;
//! если в области осталось мало — точный запрос с фильтром), текст, метаданные
//! и `snippet()` читаются лишь для итоговых top_k. Фильтр области (пространства,
//! префикс пути, живые узлы) действует до отсечения top-N канала.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::{Connection, ToSql, params, params_from_iter};
use zam_core::Id;

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
}

/// Найденный чанк (FTS или — через [`live_chunks`] — векторный хит).
#[derive(Debug, Clone)]
pub struct FtsHit {
    pub chunk_id: String,
    pub node_id: Id,
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
    pub updated_at: Timestamp,
    /// Фрагмент с совпадениями между [`MARK_START`] и [`MARK_END`].
    pub snippet: String,
    /// Полный текст чанка (обрезку под бюджет делает сервер).
    pub text: String,
}

/// Слова запроса: всё, что не буква и не цифра (Unicode), — разделитель.
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

/// Условие области для `chunks c JOIN nodes n`: `?2` — префикс пути или NULL,
/// пространства — с `?3`. Префикс сравнивается через substr, а не LIKE: LIKE
/// в SQLite нечувствителен к регистру ASCII и требует экранирования `%` и `_`.
fn scope_sql(n_spaces: usize) -> String {
    format!(
        "n.deleted_at IS NULL
         AND n.space_id IN ({})
         AND (?2 IS NULL OR n.path = ?2 OR substr(n.path, 1, length(?2) + 1) = ?2 || '/')",
        placeholders(3, n_spaces)
    )
}

/// JSON-массив чисел для `json_each`: один текст SQL при любом размере списка.
/// Запросы по такому списку и по MATCH используют `CROSS JOIN` — он фиксирует
/// порядок соединения в SQLite: при одном пространстве планировщик иначе
/// обходит все узлы пространства по индексу (`nodes_parent`), что в десятки раз дольше.
fn json_ids(ids: &[i64]) -> String {
    let items: Vec<String> = ids.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(","))
}

/// Кандидат канала: `(seq, node_id, это L1 заметки)`.
type Cand = (i64, Id, bool);

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
) -> Result<Vec<Cand>, StoreError> {
    let (table, order, limit) = channel_spec(ch);
    let over = limit * OVERSAMPLE;
    let mut st = c.prepare_cached(&format!(
        "SELECT rowid FROM {table} WHERE {table} MATCH ?1 ORDER BY {order}, rowid LIMIT {over}"
    ))?;
    let top: Vec<i64> = st
        .query_map([fts], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if top.is_empty() {
        return Ok(Vec::new());
    }

    let ids = json_ids(&top);
    let mut args: Vec<&dyn ToSql> = vec![&ids, &prefix];
    args.extend(spaces.iter().map(|s| s as &dyn ToSql));
    let mut st = c.prepare_cached(&format!(
        "SELECT c.seq, c.node_id, c.level = 'note'
         FROM json_each(?1) j
         CROSS JOIN chunks c ON c.seq = j.value
         CROSS JOIN nodes n ON n.id = c.node_id
         WHERE {}",
        scope_sql(spaces.len())
    ))?;
    let allowed: HashMap<i64, (Id, bool)> = st
        .query_map(params_from_iter(args), |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?)))
        })?
        .collect::<Result<_, _>>()?;
    let out: Vec<Cand> = top
        .iter()
        .filter_map(|seq| allowed.get(seq).map(|(node, note)| (*seq, *node, *note)))
        .take(limit as usize)
        .collect();
    if out.len() as i64 == limit || (top.len() as i64) < over {
        return Ok(out);
    }

    let mut args: Vec<&dyn ToSql> = vec![&fts, &prefix];
    args.extend(spaces.iter().map(|s| s as &dyn ToSql));
    let mut st = c.prepare_cached(&format!(
        "SELECT f.rowid, c.node_id, c.level = 'note'
         FROM {table} f
         CROSS JOIN chunks c ON c.seq = f.rowid
         CROSS JOIN nodes n ON n.id = c.node_id
         WHERE {table} MATCH ?1 AND {}
         ORDER BY {order}, f.rowid
         LIMIT {limit}",
        scope_sql(spaces.len())
    ))?;
    let rows = st.query_map(params_from_iter(args), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Итоговый кандидат после слияния.
struct Ranked {
    seq: i64,
    score: f64,
    /// Найден каналом bm25 (сниппет — через FTS5 `snippet()`).
    in_bm25: bool,
}

/// Полнотекстовый поиск по чанкам живых узлов в пределах `space_ids` и `path_prefix`.
/// Запрос без слов (и без фразы для trigram) — пустая выдача.
/// `top_k` вне 1..=100, пустой `space_ids` или запрос длиннее 512 символов → `BadInput`.
pub async fn search_fts(db: &Db, q: FtsQuery) -> Result<Vec<FtsHit>, StoreError> {
    let (spaces, prefix) = check_query(&q)?;
    db.read(move |c| {
        // один снимок данных для обоих шагов; транзакция только читает и
        // откатывается при выходе
        let tx = c.unchecked_transaction()?;
        let ch = run_channels(&tx, &q.text, prefix.as_deref(), &spaces)?;
        let ranked = merge(ch.a, ch.b, q.top_k);
        fetch_hits(&tx, &ranked, ch.a_fts.as_deref(), &ch.needles)
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
        let ch = run_channels(&tx, &q.text, prefix.as_deref(), &spaces)?;
        let in_a: HashSet<i64> = ch.a.iter().map(|c| c.0).collect();
        // текст и сниппеты — одним шагом для объединения каналов; сниппет чанка,
        // найденного bm25, — из FTS5 `snippet()`
        let mut all: Vec<Ranked> = Vec::with_capacity(ch.a.len() + ch.b.len());
        for (seq, _, _) in ch.a.iter().chain(&ch.b) {
            if !all.iter().any(|r| r.seq == *seq) {
                all.push(Ranked {
                    seq: *seq,
                    score: 0.0,
                    in_bm25: in_a.contains(seq),
                });
            }
        }
        let hits: HashMap<String, FtsHit> =
            fetch_hits(&tx, &all, ch.a_fts.as_deref(), &ch.needles)?
                .into_iter()
                .map(|h| (h.chunk_id.clone(), h))
                .collect();
        let seq_ids: HashMap<i64, String> = chunk_ids_of(&tx, &all)?;
        let take = |l: &[Cand]| -> Vec<FtsHit> {
            l.iter()
                .enumerate()
                .filter_map(|(i, (seq, _, _))| {
                    let mut h = hits.get(seq_ids.get(seq)?)?.clone();
                    h.score = 1.0 / (i + 1) as f64;
                    Some(h)
                })
                .collect()
        };
        Ok(FtsChannels {
            bm25: take(&ch.a),
            trigram: take(&ch.b),
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
    a_fts: Option<String>,
    /// `a` — по ослабленному запросу.
    relaxed: bool,
    needles: Vec<&'q str>,
}

/// Канал A (строгий запрос, при пустой выдаче — ослабленный) и канал B.
fn run_channels<'q>(
    tx: &Connection,
    text: &'q str,
    prefix: Option<&str>,
    spaces: &[Id],
) -> Result<Channels<'q>, StoreError> {
    let mut a_fts = sanitize_fts(text);
    let mut relaxed = false;
    let mut a = match &a_fts {
        Some(fts) => run_channel(tx, Channel::Bm25, fts, prefix, spaces)?,
        None => Vec::new(),
    };
    if a.is_empty() && terms(text).len() > 1 {
        a_fts = sanitize_fts_any(text);
        relaxed = true;
        if let Some(fts) = &a_fts {
            a = run_channel(tx, Channel::Bm25, fts, prefix, spaces)?;
        }
    }
    let (b, needles) = match trigram_query(text, a.is_empty()) {
        Some((fts, needles)) => (
            run_channel(tx, Channel::Trigram, &fts, prefix, spaces)?,
            needles,
        ),
        None => (Vec::new(), Vec::new()),
    };
    Ok(Channels {
        a,
        b,
        a_fts,
        relaxed,
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
    /// Только узлы, которые векторизуются (`index_state <> 'excluded'`): для векторных
    /// хитов — точка исключённого узла могла остаться в Qdrant.
    pub indexable_only: bool,
}

/// Чанки `ids` живых узлов области (с текстом, без сниппета, `score = 0`) в порядке
/// `ids`; отсутствующие (старая ревизия, узел удалён или в корзине, вне области,
/// исключён при `indexable_only`) пропускаются. Источник истины для векторных хитов:
/// точки в Qdrant могут пережить неудачное задание `delete`.
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
    db.read(move |c| {
        let mut args: Vec<&dyn ToSql> = vec![&list, &prefix];
        args.extend(scope.space_ids.iter().map(|s| s as &dyn ToSql));
        let mut st = c.prepare_cached(&format!(
            "SELECT c.id, c.node_id, n.space_id, n.path, c.heading_path, c.rev, n.updated_at,
                    c.text, c.level, c.token_count
             FROM json_each(?1) j
             CROSS JOIN chunks c ON c.id = j.value
             CROSS JOIN nodes n ON n.id = c.node_id
             WHERE {} {}",
            scope_sql(scope.space_ids.len()),
            if scope.indexable_only {
                "AND n.index_state <> 'excluded'"
            } else {
                ""
            }
        ))?;
        let rows = st.query_map(params_from_iter(args), |r| {
            Ok(FtsHit {
                chunk_id: r.get(0)?,
                node_id: r.get(1)?,
                space_id: r.get(2)?,
                path: r.get(3)?,
                heading_path: r.get(4)?,
                level: r.get(8)?,
                rev: r.get(5)?,
                token_count: r.get(9)?,
                score: 0.0,
                updated_at: ts_at(r, 6)?,
                snippet: String::new(),
                text: r.get(7)?,
            })
        })?;
        let mut by_id: HashMap<String, FtsHit> = rows
            .map(|h| h.map(|h| (h.chunk_id.clone(), h)))
            .collect::<Result<_, _>>()?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    })
    .await
}

/// Лист заметки для замены сводки (§9.2 п. 6).
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
    Ok(leaves_of_nodes(db, vec![node], true)
        .await?
        .remove(&node)
        .unwrap_or_default())
}

/// Листы нескольких узлов одним запросом: `узел → листы по порядку`; узла без листов
/// в ответе нет (замена сводок листами в поиске — без запроса на каждую сводку).
/// `with_text = false` — без `text` и `context_prefix` (пустые строки): тексты сотен
/// листов не читаются, если нужны только пути, порядок и ключи кэша эмбеддингов.
pub async fn leaves_of_nodes(
    db: &Db,
    nodes: Vec<Id>,
    with_text: bool,
) -> Result<HashMap<Id, Vec<LeafChunk>>, StoreError> {
    if nodes.is_empty() {
        return Ok(HashMap::new());
    }
    db.read(move |c| {
        // один снимок и один подготовленный запрос по индексу `chunks_node` на узел:
        // соединение с json_each здесь медленнее (сортировка всех листов)
        let tx = c.unchecked_transaction()?;
        let mut st = tx.prepare_cached(
            "SELECT id, heading_path, ordinal,
                    CASE WHEN ?2 THEN text ELSE '' END,
                    CASE WHEN ?2 THEN context_prefix ELSE '' END,
                    embed_key
             FROM chunks WHERE node_id = ?1 AND level = 'chunk' ORDER BY ordinal",
        )?;
        let mut out: HashMap<Id, Vec<LeafChunk>> = HashMap::new();
        for node in nodes {
            let rows = st.query_map(params![node, with_text], |r| {
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
                out.insert(node, leaves);
            }
        }
        Ok(out)
    })
    .await
}

/// Лист узла с числом найденных в нём слов запроса ([`leaf_term_hits`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafHit {
    pub node_id: Id,
    pub chunk_id: String,
    pub heading_path: String,
    pub ordinal: i64,
    /// Сколько слов запроса найдено в листе (≥ 1).
    pub hits: usize,
}

/// Листы узлов `nodes`, в которых FTS5 находит слова запроса (`needles`, каждое — как
/// запрос [`sanitize_fts`]), с числом найденных слов; по узлу и `ordinal`. Листы без
/// совпадений в ответ не входят. Тексты не читаются: по одному запросу к индексу на
/// слово, в одной читающей транзакции.
pub async fn leaf_term_hits(
    db: &Db,
    nodes: Vec<Id>,
    needles: Vec<String>,
) -> Result<Vec<LeafHit>, StoreError> {
    if nodes.is_empty() || needles.is_empty() {
        return Ok(Vec::new());
    }
    let list =
        serde_json::Value::from(nodes.iter().map(Id::to_string).collect::<Vec<_>>()).to_string();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let mut st = tx.prepare_cached(
            "SELECT DISTINCT c.id, c.node_id, c.heading_path, c.ordinal FROM chunks_fts f
             CROSS JOIN chunks c ON c.seq = f.rowid
             WHERE chunks_fts MATCH ?1 AND c.level = 'chunk'
               AND c.node_id IN (SELECT value FROM json_each(?2))",
        )?;
        let mut out: HashMap<String, LeafHit> = HashMap::new();
        for n in needles.iter().take(MAX_TERMS) {
            let Some(fts) = sanitize_fts(n) else {
                continue;
            };
            let rows = st.query_map(params![fts, list], |r| {
                Ok(LeafHit {
                    chunk_id: r.get(0)?,
                    node_id: r.get(1)?,
                    heading_path: r.get(2)?,
                    ordinal: r.get(3)?,
                    hits: 1,
                })
            })?;
            for h in rows {
                let h = h?;
                out.entry(h.chunk_id.clone())
                    .and_modify(|e| e.hits += 1)
                    .or_insert(h);
            }
        }
        let mut v: Vec<LeafHit> = out.into_values().collect();
        v.sort_by(|a, b| a.node_id.cmp(&b.node_id).then(a.ordinal.cmp(&b.ordinal)));
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

/// Weighted RRF по чанкам, L1 заметки — только у узлов без найденных листов,
/// лимит на узел, top_k.
fn merge(a: Vec<Cand>, b: Vec<Cand>, top_k: usize) -> Vec<Ranked> {
    let with_leaf: HashSet<Id> = a
        .iter()
        .chain(&b)
        .filter(|(_, _, note)| !note)
        .map(|(_, node, _)| *node)
        .collect();
    let keep = |c: &Cand| !c.2 || !with_leaf.contains(&c.1);
    let strip = |l: Vec<Cand>| -> Vec<(i64, Id)> {
        l.into_iter()
            .filter(keep)
            .map(|(seq, node, _)| (seq, node))
            .collect()
    };
    rrf(strip(a), strip(b), top_k)
}

/// Weighted RRF по чанкам, лимит на узел, top_k.
fn rrf(a: Vec<(i64, Id)>, b: Vec<(i64, Id)>, top_k: usize) -> Vec<Ranked> {
    let mut by_seq: HashMap<i64, (Id, f64, bool)> = HashMap::new();
    for (w, list, is_a) in [(W_BM25, a, true), (W_TRIGRAM, b, false)] {
        for (i, (seq, node)) in list.into_iter().enumerate() {
            let e = by_seq.entry(seq).or_insert((node, 0.0, false));
            e.1 += w / (RRF_K + (i + 1) as f64);
            e.2 |= is_a;
        }
    }
    let mut all: Vec<(i64, (Id, f64, bool))> = by_seq.into_iter().collect();
    all.sort_by(|(x, (_, sx, _)), (y, (_, sy, _))| sy.total_cmp(sx).then(x.cmp(y)));

    let mut per_node: HashMap<Id, usize> = HashMap::new();
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
    needles: &[&str],
) -> Result<Vec<FtsHit>, StoreError> {
    if ranked.is_empty() {
        return Ok(Vec::new());
    }
    let seqs: Vec<i64> = ranked.iter().map(|r| r.seq).collect();
    let mut st = c.prepare_cached(
        "SELECT c.seq, c.id, c.node_id, n.space_id, n.path, c.heading_path, c.rev,
                n.updated_at, c.text, c.level, c.token_count
         FROM json_each(?1) j
         CROSS JOIN chunks c ON c.seq = j.value
         CROSS JOIN nodes n ON n.id = c.node_id",
    )?;
    let rows = st.query_map([json_ids(&seqs)], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            FtsHit {
                chunk_id: r.get(1)?,
                node_id: r.get(2)?,
                space_id: r.get(3)?,
                path: r.get(4)?,
                heading_path: r.get(5)?,
                level: r.get(9)?,
                rev: r.get(6)?,
                token_count: r.get(10)?,
                score: 0.0,
                updated_at: ts_at(r, 7)?,
                snippet: String::new(),
                text: r.get(8)?,
            },
        ))
    })?;
    let mut by_seq: HashMap<i64, FtsHit> = rows.collect::<Result<_, _>>()?;

    let mut snippets: HashMap<i64, String> = HashMap::new();
    let a_seqs: Vec<i64> = ranked.iter().filter(|r| r.in_bm25).map(|r| r.seq).collect();
    if let Some(fts) = bm25_fts
        && !a_seqs.is_empty()
    {
        // snippet() требует курсора MATCH, поэтому запрос повторяется одним
        // проходом по совпадениям; `+rowid` не отдаётся FTS5 как ограничение:
        // иначе для каждого rowid из списка MATCH (с раскрытием префиксов)
        // вычисляется заново — на 30 тыс. чанков в 10 раз дольше
        let (ms, me) = (MARK_START.to_string(), MARK_END.to_string());
        let mut st = c.prepare_cached(&format!(
            "SELECT rowid, snippet(chunks_fts, 0, ?1, ?2, '…', {SNIPPET_TOKENS})
             FROM chunks_fts
             WHERE chunks_fts MATCH ?3 AND +rowid IN (SELECT value FROM json_each(?4))"
        ))?;
        let rows = st.query_map(params![ms, me, fts, json_ids(&a_seqs)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        snippets = rows.collect::<Result<_, _>>()?;
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

/// Сколько живых заметок в `space_ids` (и под `path_prefix`, если задан) ждут
/// векторной индексации (`index_state = 'pending'`) — `pending_count` выдачи поиска.
/// Заметки с `index: false` (`excluded`) не считаются.
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
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM nodes n
             WHERE n.kind = 'note' AND n.index_state = 'pending' AND n.deleted_at IS NULL
               AND n.space_id IN (SELECT value FROM json_each(?1))
               AND (?2 IS NULL OR n.path = ?2 OR substr(n.path, 1, length(?2) + 1) = ?2 || '/')",
            params![ids, prefix],
            |r| r.get(0),
        )?)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
