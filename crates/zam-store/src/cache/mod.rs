//! Кэш выжимок агентов (§9.3 шаг 0 и шаг 3, §9.4; задача 7 M4).
//!
//! - `answer_cache` — выжимка агента (`summary`, Markdown) на запрос `query` в пространстве
//!   кэша `space_id`: по решению — пространство решения в базе владельца (общая на решение),
//!   иначе — Knowledge пользователя. Ключ — `(scope_hash, query_norm)` ([`scope_hash`],
//!   [`normalize_query`]) и dense-вектор запроса `query_vec` для семантического попадания;
//! - `cache_deps` — источники выжимки: узлы и записи журналов **этой же базы** с ревизией и
//!   хэшем текста цели (или секции, если она указана), закреплёнными при сохранении
//!   ([`cache_put`]). Human — [`CacheError::ForbiddenSource`] (Vault проверяет сервер: у
//!   записей Vault нет цели `TargetRef`);
//! - **проверка при чтении** ([`cache_lookup`], [`cache_get`], [`cache_list`]): ревизии и
//!   хэши всех источников выбранных выжимок сверяются одним SQL-запросом ([`deps`]); вместе
//!   с причинами, записанными при инвалидации (`stale_reason`), это список изменившихся
//!   источников [`CacheHit::stale`];
//! - **инвалидация при записи** ([`invalidate`]) — в той же транзакции писателя, что и запись
//!   источника: правка (`changed`; секция, текст которой не изменился, лишь получает новую
//!   ревизию), удаление и перенос в корзину (`deleted`), перенос узла (`changed`), замещение
//!   и отзыв insight (`superseded`, `retracted`); новые релевантные источники — воркер
//!   индексации ([`mark_new_sources`], `new_sources`);
//! - **срок жизни** ([`gc_cache`]): выжимки без обращений дольше срока и сверх предела на
//!   базу (LRU по `last_used_at`) удаляются, превращённые в записи (`promoted_to`) — нет;
//! - **превращение** ([`cache_promote`]) — заметка или insight с текстом выжимки и связями
//!   `derived_from` на источники, одной транзакцией с отметкой `promoted_to`.
//!
//! Текст выжимок и запросов в журнал не пишется.
mod deps;
mod invalidate;

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;
use zam_core::{Id, TargetKind, TargetRef};

pub use self::deps::section_text;
pub(crate) use self::invalidate::invalidate_revised;
pub use self::invalidate::{invalidate_target, mark_new_sources};
use crate::embeddings::{decode_dense, encode_dense};
use crate::entries::{EntryKind, NewEntry};
use crate::graph::{EdgeType, NewEdge};
use crate::nodes::{Attribution, AuthorKind, NewNote};
use crate::paths::NodePath;
use crate::time::{fmt_ts, ser_ts, ser_ts_opt, ts_at, ts_opt};
use crate::{Db, StoreError};

/// Не больше выжимок в ответе поиска (§9.4).
pub const CACHE_LOOKUP_MAX: usize = 2;
/// Предел длины запроса выжимки, символов (как у запроса поиска).
pub const CACHE_QUERY_MAX_CHARS: usize = crate::search::QUERY_MAX_CHARS;
/// Не больше источников у одной выжимки.
pub const CACHE_SOURCES_MAX: usize = 50;

/// Причина недействительности источника (§9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleKind {
    Changed,
    Deleted,
    Superseded,
    Retracted,
    NewSources,
}

impl StaleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StaleKind::Changed => "changed",
            StaleKind::Deleted => "deleted",
            StaleKind::Superseded => "superseded",
            StaleKind::Retracted => "retracted",
            StaleKind::NewSources => "new_sources",
        }
    }
}

/// Изменившийся источник выжимки: цель (у `new_sources` — новая цель), секция источника и
/// причина.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StaleSource {
    pub target: TargetRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub reason: StaleKind,
}

/// Источник выжимки, закреплённый при сохранении.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheDep {
    pub target: TargetRef,
    pub rev: i64,
    pub text_hash: String,
    /// Путь секции; `None` — цель целиком.
    pub section: Option<String>,
}

/// Источник в запросе сохранения: цель, ревизия, на которой основана выжимка (`None` —
/// текущая), и секция.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub target: TargetRef,
    pub rev: Option<i64>,
    pub section: Option<String>,
}

/// Новая выжимка.
#[derive(Debug, Clone, PartialEq)]
pub struct CachePut {
    pub space_id: Id,
    pub area: Option<String>,
    pub author_user_id: Id,
    /// Агент (токен) или человек (WebUI): вместе с `author_user_id` и сессией — кто заменяет
    /// свою прежнюю выжимку того же ключа.
    pub author_kind: AuthorKind,
    pub session_id: Option<String>,
    pub machine: Option<String>,
    pub scope_hash: String,
    pub query: String,
    /// Dense-вектор запроса (нормированный); `None` — без модели: только точное попадание.
    pub query_vec: Option<Vec<f32>>,
    /// Модель `query_vec` (`model_id` эмбеддера); вектор другой модели не сравнивается.
    pub model_id: Option<String>,
    pub summary: String,
    pub tokens: i64,
    pub sources: Vec<SourceSpec>,
}

/// Выжимка кэша.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CacheEntry {
    pub id: Id,
    pub space_id: Id,
    pub area: Option<String>,
    pub author_user_id: Id,
    pub session_id: Option<String>,
    pub machine: Option<String>,
    pub scope_hash: String,
    pub query: String,
    pub summary: String,
    pub tokens: i64,
    /// `valid` | `stale` — состояние, записанное инвалидацией (проверку при чтении см.
    /// [`CacheHit::stale`]).
    pub state: String,
    /// Причины, записанные инвалидацией при записи источников.
    pub stale_reason: Vec<StaleSource>,
    #[serde(serialize_with = "ser_ts")]
    pub created_at: Timestamp,
    #[serde(serialize_with = "ser_ts")]
    pub last_used_at: Timestamp,
    pub hits: i64,
    /// `n:<id>` | `e:<id>` — заметка или insight, в которые превращена выжимка.
    pub promoted_to: Option<String>,
    #[serde(serialize_with = "ser_ts_opt")]
    pub deleted_at: Option<Timestamp>,
    pub sources: Vec<CacheDep>,
}

/// Выжимка с итогом проверки источников: пустой `stale` — все источники актуальны.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CacheHit {
    pub entry: CacheEntry,
    pub stale: Vec<StaleSource>,
    /// Пространства источников (видимость участнику решения).
    #[serde(skip)]
    pub source_spaces: Vec<Id>,
}

impl CacheHit {
    /// Все источники актуальны.
    pub fn is_valid(&self) -> bool {
        self.stale.is_empty()
    }
}

/// Попадание в кэш при поиске (§9.4).
#[derive(Debug, Clone, PartialEq)]
pub struct CacheQuery {
    pub space_id: Id,
    pub scope_hash: String,
    pub query_norm: String,
    pub query_vec: Option<Vec<f32>>,
    /// Модель `query_vec`: семантическое попадание — только по выжимкам этой модели.
    pub model_id: Option<String>,
    /// Текущая сессия: её выжимки — первыми.
    pub session_id: Option<String>,
    /// Выжимки других сессий (`cache.cross_session`).
    pub cross_session: bool,
    /// Порог косинуса семантического попадания (`cache.semantic_threshold`).
    pub semantic_threshold: f32,
    /// Не больше выжимок ([`CACHE_LOOKUP_MAX`]).
    pub limit: usize,
    /// Пространства, видимые запросу; выжимка с источником вне их не выдаётся (участник
    /// решения не видит Knowledge владельца). `None` — без ограничения.
    pub visible_spaces: Option<Vec<Id>>,
}

/// Фильтр списка выжимок.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheFilter {
    /// Пространства кэша; `None` — все пространства базы.
    pub space_ids: Option<Vec<Id>>,
    /// Подстрока нормализованного запроса.
    pub q: Option<String>,
    pub session_id: Option<String>,
    /// `Some(true)` — только действующие (после проверки источников), `Some(false)` — только
    /// недействительные.
    pub valid: Option<bool>,
    pub visible_spaces: Option<Vec<Id>>,
    pub limit: usize,
}

/// Во что превратить выжимку (§9.3).
#[derive(Debug, Clone, PartialEq)]
pub enum PromoteAs {
    /// Заметка `path` в пространстве кэша.
    Note { path: NodePath },
    /// Insight вида `kind` (`data.kind`) в пространстве кэша, заголовок — `title`.
    Insight { kind: String, title: String },
}

/// Ошибка сохранения или превращения выжимки.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Источник из Human (или Vault — проверяет сервер): 422 `cache-forbidden-source`.
    #[error("источник {0} не может быть источником выжимки: Human и Vault в кэш не попадают")]
    ForbiddenSource(String),
    /// Источника нет, он удалён, это папка или нет секции.
    #[error("источник {target}: {detail}")]
    SourceNotFound { target: String, detail: String },
    /// Источник — insight `superseded` или `retracted`: 422 `cache-source-inactive`.
    #[error("источник {target}: запись {status} — выжимка на ней сразу недействительна")]
    SourceInactive { target: String, status: String },
    /// Ревизия источника в запросе не совпала с текущей: выжимка уже устарела.
    #[error("источник {target}: ревизия {current_rev}, а не {expected}")]
    SourceChanged {
        target: String,
        expected: i64,
        current_rev: i64,
    },
}

/// Нормализованный запрос (ключ кэша): NFKC, нижний регистр, пробелы схлопнуты.
pub fn normalize_query(q: &str) -> String {
    let n: String = q.nfkc().flat_map(char::to_lowercase).collect();
    n.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Канонический JSON: ключи объектов по порядку, без пробелов.
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                canonical(&m[k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `scope_hash` (§9.4): blake3 канонического JSON области `{solution, include_knowledge,
/// include_rules, path_prefix, area, filters}` (собирает сервер).
pub fn scope_hash(scope: &Value) -> String {
    let mut s = String::new();
    canonical(scope, &mut s);
    blake3::hash(s.as_bytes()).to_hex().to_string()
}

pub(crate) fn kind_str(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Node => "node",
        TargetKind::Entry => "entry",
    }
}

pub(crate) fn kind_of(s: &str) -> Option<TargetKind> {
    match s {
        "node" => Some(TargetKind::Node),
        "entry" => Some(TargetKind::Entry),
        _ => None,
    }
}

/// `section` колонки: `''` — цель целиком.
pub(crate) fn section_col(s: Option<&str>) -> &str {
    s.unwrap_or("")
}

fn section_opt(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

const CACHE_COLS: &str = "a.id, a.space_id, a.area, a.author_user_id, a.session_id, a.machine, \
    a.scope_hash, a.query, a.summary, a.tokens, a.state, a.stale_reason, a.created_at, \
    a.last_used_at, a.hits, a.promoted_to, a.deleted_at";

fn cache_row(r: &Row) -> rusqlite::Result<CacheEntry> {
    let stale: Option<String> = r.get(11)?;
    let stale_reason = match stale {
        Some(s) => serde_json::from_str(&s).map_err(|e| crate::time::conv_err(11, e))?,
        None => Vec::new(),
    };
    Ok(CacheEntry {
        id: r.get(0)?,
        space_id: r.get(1)?,
        area: r.get(2)?,
        author_user_id: r.get(3)?,
        session_id: r.get(4)?,
        machine: r.get(5)?,
        scope_hash: r.get(6)?,
        query: r.get(7)?,
        summary: r.get(8)?,
        tokens: r.get(9)?,
        state: r.get(10)?,
        stale_reason,
        created_at: ts_at(r, 12)?,
        last_used_at: ts_at(r, 13)?,
        hits: r.get(14)?,
        promoted_to: r.get(15)?,
        deleted_at: ts_opt(r, 16)?,
        sources: Vec::new(),
    })
}

/// Выжимка по id (в том числе удалённая) с источниками.
fn load_cache(c: &Connection, id: Id) -> Result<Option<CacheEntry>, StoreError> {
    let e = c
        .prepare_cached(&format!(
            "SELECT {CACHE_COLS} FROM answer_cache a WHERE a.id = ?1"
        ))?
        .query_row([id], cache_row)
        .optional()?;
    let Some(mut e) = e else {
        return Ok(None);
    };
    e.sources = load_deps(c, id)?;
    Ok(Some(e))
}

fn load_deps(c: &Connection, id: Id) -> Result<Vec<CacheDep>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT target_kind, target_id, rev, text_hash, section FROM cache_deps
         WHERE cache_id = ?1 ORDER BY rowid",
    )?;
    let rows = st.query_map([id], |r| {
        let k: String = r.get(0)?;
        Ok((k, r.get::<_, Id>(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (k, id, rev, text_hash, section): (String, Id, i64, String, String) = row?;
        let kind = kind_of(&k)
            .ok_or_else(|| StoreError::Corrupt(format!("cache_deps: вид цели «{k}»")))?;
        out.push(CacheDep {
            target: TargetRef { kind, id },
            rev,
            text_hash,
            section: section_opt(section),
        });
    }
    Ok(out)
}

/// Сохраняет выжимку: источники проверяются и закрепляются (ревизия и хэш текста или секции)
/// в той же транзакции. Прежняя выжимка того же ключа `(scope_hash, query_norm)` в этом
/// пространстве **того же автора** (`author_kind`, `author_user_id`) **и той же сессии**, не
/// превращённая в запись, мягко удаляется — действует новая; выжимки других авторов и
/// сессий остаются **[решение исполнителя, fix round 1]**. Нет пространства →
/// `Store(NotFound)`; пустой список или больше [`CACHE_SOURCES_MAX`] источников →
/// `Store(BadInput)`.
pub async fn cache_put(db: &Db, p: &CachePut) -> Result<CacheEntry, CacheError> {
    Ok(cache_put_with(db, p, None).await?.0)
}

/// [`cache_put`] и сразу [`cache_promote`] — одной транзакцией писателя: если превращение не
/// удалось (путь занят — `Conflict`), выжимка не сохраняется.
pub async fn cache_put_with(
    db: &Db,
    p: &CachePut,
    promote: Option<(PromoteAs, Attribution)>,
) -> Result<(CacheEntry, Option<TargetRef>), CacheError> {
    if p.sources.is_empty() {
        return Err(StoreError::BadInput("sources: хотя бы один источник".into()).into());
    }
    if p.sources.len() > CACHE_SOURCES_MAX {
        return Err(StoreError::BadInput(format!(
            "sources: не больше {CACHE_SOURCES_MAX} источников"
        ))
        .into());
    }
    let p = p.clone();
    db.write(move |tx| {
        let e = match put_tx(tx, &p) {
            Ok(e) => e,
            Err(CacheError::Store(e)) => return Err(e),
            // ошибки источников — до любой записи: фиксация ничего не меняет
            Err(e) => return Ok(Err(e)),
        };
        match promote {
            None => Ok(Ok((e, None))),
            // сбой превращения откатывает и сохранение
            Some((to, a)) => promote_tx(tx, e.id, to, &a).map(|(e, t)| Ok((e, Some(t)))),
        }
    })
    .await?
}

fn put_tx(tx: &Connection, p: &CachePut) -> Result<CacheEntry, CacheError> {
    let space: Option<String> = tx
        .query_row("SELECT kind FROM spaces WHERE id = ?1", [p.space_id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(StoreError::from)?;
    if space.is_none() {
        return Err(StoreError::NotFound.into());
    }
    let mut deps: Vec<CacheDep> = Vec::with_capacity(p.sources.len());
    let mut seen = HashSet::new();
    for s in &p.sources {
        let section = s
            .section
            .as_deref()
            .map(str::trim)
            .filter(|x| !x.is_empty());
        if !seen.insert((s.target, section.map(str::to_string))) {
            continue;
        }
        deps.push(deps::pin(tx, s.target, s.rev, section)?);
    }
    let now = fmt_ts(Timestamp::now());
    let query_norm = normalize_query(&p.query);
    tx.execute(
        "UPDATE answer_cache SET deleted_at = ?4
         WHERE space_id = ?1 AND scope_hash = ?2 AND query_norm = ?3
           AND author_user_id = ?5 AND author_kind = ?6 AND session_id IS ?7
           AND deleted_at IS NULL AND promoted_to IS NULL",
        params![
            p.space_id,
            p.scope_hash,
            query_norm,
            now,
            p.author_user_id,
            p.author_kind.as_str(),
            p.session_id
        ],
    )
    .map_err(StoreError::from)?;
    let id = Id::new();
    tx.execute(
        "INSERT INTO answer_cache (id, space_id, area, author_user_id, session_id, machine,
                                   scope_hash, query, query_norm, query_vec, summary, tokens,
                                   state, created_at, last_used_at, author_kind, model_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'valid', ?13, ?13, ?14,
                 ?15)",
        params![
            id,
            p.space_id,
            p.area,
            p.author_user_id,
            p.session_id,
            p.machine,
            p.scope_hash,
            p.query,
            query_norm,
            p.query_vec.as_deref().map(encode_dense),
            p.summary,
            p.tokens,
            now,
            p.author_kind.as_str(),
            p.query_vec.as_ref().and(p.model_id.as_deref()),
        ],
    )
    .map_err(StoreError::from)?;
    let mut ins = tx
        .prepare_cached(
            "INSERT INTO cache_deps (cache_id, target_kind, target_id, rev, text_hash, section)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(StoreError::from)?;
    for d in &deps {
        ins.execute(params![
            id,
            kind_str(d.target.kind),
            d.target.id,
            d.rev,
            d.text_hash,
            section_col(d.section.as_deref()),
        ])
        .map_err(StoreError::from)?;
    }
    Ok(load_cache(tx, id)?.ok_or(StoreError::NotFound)?)
}

/// Кандидат попадания: выжимка, точное ли совпадение, косинус.
struct Candidate {
    entry: CacheEntry,
    exact: bool,
    sim: f32,
}

fn cos(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut ab, mut aa, mut bb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    (aa > 0.0 && bb > 0.0).then(|| ab / (aa.sqrt() * bb.sqrt()))
}

/// Проверка источников выжимок `entries` (одним SQL) и сбор итогов: записанные причины и
/// найденные проверкой расхождения, без повторов.
fn verify(c: &Connection, entries: Vec<CacheEntry>) -> Result<Vec<CacheHit>, StoreError> {
    let ids: Vec<Id> = entries.iter().map(|e| e.id).collect();
    let mut checked = deps::check(c, &ids)?;
    let mut out = Vec::with_capacity(entries.len());
    for mut entry in entries {
        let (found, spaces) = checked.remove(&entry.id).unwrap_or_default();
        let mut stale: Vec<StaleSource> = Vec::new();
        for s in entry.stale_reason.iter().chain(&found) {
            if !stale.contains(s) {
                stale.push(s.clone());
            }
        }
        entry.sources = load_deps(c, entry.id)?;
        out.push(CacheHit {
            entry,
            stale,
            source_spaces: spaces,
        });
    }
    Ok(out)
}

/// Выжимка видна запросу: все её источники — в видимых пространствах.
fn visible(h: &CacheHit, spaces: Option<&[Id]>) -> bool {
    spaces.is_none_or(|v| h.source_spaces.iter().all(|s| v.contains(s)))
}

/// Попадание в кэш (§9.4): выжимки пространства `space_id` с тем же `scope_hash` — точное
/// совпадение `query_norm` или косинус `query_vec` не ниже порога; сначала текущей сессии
/// (выжимки других — только при `cross_session`), затем точные, затем по косинусу; не больше
/// `limit`. Источники выбранных сверяются одним SQL ([`CacheHit::stale`]); у выданных
/// обновляются `last_used_at` и `hits` (LRU).
pub async fn cache_lookup(db: &Db, q: &CacheQuery) -> Result<Vec<CacheHit>, StoreError> {
    let q = q.clone();
    let hits = db
        .read(move |c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {CACHE_COLS}, a.query_norm, a.query_vec, a.model_id FROM answer_cache a
                 WHERE a.space_id = ?1 AND a.scope_hash = ?2 AND a.deleted_at IS NULL
                   AND (?3 OR a.session_id IS ?4)"
            ))?;
            let rows = st.query_map(
                params![q.space_id, q.scope_hash, q.cross_session, q.session_id],
                |r| {
                    let e = cache_row(r)?;
                    let norm: String = r.get(17)?;
                    let vec: Option<Vec<u8>> = r.get(18)?;
                    let model: Option<String> = r.get(19)?;
                    Ok((e, norm, vec, model))
                },
            )?;
            let mut cands: Vec<Candidate> = Vec::new();
            for row in rows {
                let (entry, norm, vec, model) = row?;
                let exact = norm == q.query_norm;
                // вектор другой модели (или без модели) не сравнивается
                let same_model = model.is_some() && model == q.model_id;
                let sim = match (&q.query_vec, vec) {
                    (Some(qv), Some(b)) if same_model => cos(qv, &decode_dense(&b)?).unwrap_or(0.0),
                    _ => 0.0,
                };
                if exact || sim >= q.semantic_threshold {
                    cands.push(Candidate { entry, exact, sim });
                }
            }
            let mine = |c: &Candidate| {
                q.session_id.is_some() && c.entry.session_id.as_deref() == q.session_id.as_deref()
            };
            // §9.4: сначала текущая сессия, затем остальные; внутри — точные, затем свежие
            cands.sort_by(|a, b| {
                mine(b)
                    .cmp(&mine(a))
                    .then(b.exact.cmp(&a.exact))
                    .then(b.entry.created_at.cmp(&a.entry.created_at))
                    .then(b.sim.total_cmp(&a.sim))
            });
            let mut out = Vec::new();
            // проверка — по кандидатам в порядке выдачи, пока не набрано `limit` видимых
            for chunk in cands.chunks(q.limit.max(1)) {
                let entries = chunk.iter().map(|c| c.entry.clone()).collect();
                for h in verify(c, entries)? {
                    if out.len() < q.limit && visible(&h, q.visible_spaces.as_deref()) {
                        out.push(h);
                    }
                }
                if out.len() >= q.limit {
                    break;
                }
            }
            Ok(out)
        })
        .await?;
    if !hits.is_empty() {
        let ids: Vec<Id> = hits.iter().map(|h| h.entry.id).collect();
        // учёт обращения (LRU) — не повод терять найденное
        if let Err(e) = touch_used(db, ids).await {
            tracing::warn!(error = %e, "кэш выжимок: обращение не отмечено");
        }
    }
    Ok(hits)
}

/// Обращение к выжимкам: `last_used_at` — сейчас, `hits + 1`.
async fn touch_used(db: &Db, ids: Vec<Id>) -> Result<(), StoreError> {
    db.write(move |t| {
        let now = fmt_ts(Timestamp::now());
        let mut st = t.prepare_cached(
            "UPDATE answer_cache SET last_used_at = ?2, hits = hits + 1 WHERE id = ?1",
        )?;
        for id in ids {
            st.execute(params![id, now])?;
        }
        Ok(())
    })
    .await
}

/// Выжимка по id с проверкой источников; удалённая — `None`.
pub async fn cache_get(db: &Db, id: Id) -> Result<Option<CacheHit>, StoreError> {
    db.read(move |c| {
        let Some(e) = load_cache(c, id)? else {
            return Ok(None);
        };
        if e.deleted_at.is_some() {
            return Ok(None);
        }
        Ok(verify(c, vec![e])?.pop())
    })
    .await
}

/// Список живых выжимок базы по фильтру, свежие обращения первыми (с проверкой источников).
pub async fn cache_list(db: &Db, f: &CacheFilter) -> Result<Vec<CacheHit>, StoreError> {
    let f = f.clone();
    db.read(move |c| {
        let spaces = f
            .space_ids
            .as_ref()
            .map(|v| Value::from(v.iter().map(|i| i.to_string()).collect::<Vec<_>>()).to_string());
        let q =
            f.q.as_deref()
                .map(normalize_query)
                .filter(|s| !s.is_empty());
        let mut st = c.prepare_cached(&format!(
            "SELECT {CACHE_COLS} FROM answer_cache a
             WHERE a.deleted_at IS NULL
               AND (?1 IS NULL OR a.space_id IN (SELECT value FROM json_each(?1)))
               AND (?2 IS NULL OR instr(a.query_norm, ?2) > 0)
               AND (?3 IS NULL OR a.session_id = ?3)
             ORDER BY a.last_used_at DESC, a.id"
        ))?;
        let rows = st.query_map(params![spaces, q, f.session_id], cache_row)?;
        let entries: Vec<CacheEntry> = rows.collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        for chunk in entries.chunks(100) {
            for h in verify(c, chunk.to_vec())? {
                if !visible(&h, f.visible_spaces.as_deref()) {
                    continue;
                }
                if f.valid.is_some_and(|v| v != h.is_valid()) {
                    continue;
                }
                out.push(h);
                if out.len() >= f.limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    })
    .await
}

/// Мягко удаляет выжимку; `false` — нет или уже удалена.
pub async fn cache_delete(db: &Db, id: Id) -> Result<bool, StoreError> {
    db.write(move |t| {
        let n = t.execute(
            "UPDATE answer_cache SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, fmt_ts(Timestamp::now())],
        )?;
        Ok(n > 0)
    })
    .await
}

/// Превращает выжимку в заметку или insight пространства кэша одной транзакцией: текст —
/// выжимка, связи `derived_from` — на живые источники, у выжимки — `promoted_to`. Нет или
/// удалена → `NotFound`; уже превращена → `Conflict`; путь занят → `Conflict`.
pub async fn cache_promote(
    db: &Db,
    id: Id,
    to: PromoteAs,
    a: &Attribution,
) -> Result<(CacheEntry, TargetRef), StoreError> {
    let a = a.clone();
    db.write(move |tx| promote_tx(tx, id, to, &a)).await
}

/// [`cache_promote`] внутри транзакции писателя.
fn promote_tx(
    tx: &Connection,
    id: Id,
    to: PromoteAs,
    a: &Attribution,
) -> Result<(CacheEntry, TargetRef), StoreError> {
    let e = load_cache(tx, id)?.ok_or(StoreError::NotFound)?;
    if e.deleted_at.is_some() {
        return Err(StoreError::NotFound);
    }
    if let Some(p) = &e.promoted_to {
        return Err(StoreError::Conflict(format!(
            "выжимка уже превращена в {p}"
        )));
    }
    let now = fmt_ts(Timestamp::now());
    let mut targets: Vec<TargetRef> = Vec::new();
    for d in &e.sources {
        if !targets.contains(&d.target) && deps::is_live(tx, d.target)? {
            targets.push(d.target);
        }
    }
    let mut attrib = a.clone();
    attrib.area = attrib.area.or_else(|| e.area.clone());
    let created = match to {
        PromoteAs::Note { path } => {
            let n = crate::nodes::insert_note(
                tx,
                &NewNote {
                    space_id: e.space_id,
                    path,
                    content: e.summary.clone(),
                    l0: None,
                    attrib: attrib.clone(),
                },
            )?;
            let src = TargetRef {
                kind: TargetKind::Node,
                id: n.id,
            };
            for &dst in &targets {
                let edge = NewEdge {
                    src,
                    dst,
                    kind: EdgeType::DerivedFrom,
                };
                crate::graph::add_explicit(tx, edge, &attrib, &now)?;
            }
            src
        }
        PromoteAs::Insight { kind, title } => {
            let entry = NewEntry {
                space_id: e.space_id,
                kind: EntryKind::Insight,
                title,
                body: e.summary.clone(),
                data: serde_json::json!({ "kind": kind }),
                area: e.area.clone(),
                tags: Vec::new(),
                status: None,
                certainty: None,
                idempotency_key: None,
                triggers: Vec::new(),
                links: targets
                    .iter()
                    .map(|t| (EdgeType::DerivedFrom, *t))
                    .collect(),
            };
            let eid = crate::entries::insert_entry(tx, entry, None, &attrib, None, &now)?;
            TargetRef {
                kind: TargetKind::Entry,
                id: eid,
            }
        }
    };
    tx.execute(
        "UPDATE answer_cache SET promoted_to = ?2 WHERE id = ?1",
        params![id, created.to_string()],
    )?;
    let e = load_cache(tx, id)?.ok_or(StoreError::NotFound)?;
    Ok((e, created))
}

/// Итог чистки кэша базы.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheGc {
    /// Удалены по сроку без обращений (в том числе удалённые мягко).
    pub expired: u64,
    /// Вытеснены сверх предела (LRU).
    pub evicted: u64,
}

/// Срок жизни кэша базы (§9.4): выжимки без обращений с `cutoff` (`last_used_at < cutoff`)
/// и живые сверх `max_entries` (самые давние по `last_used_at`) удаляются вместе с
/// источниками; превращённые в записи (`promoted_to`) не удаляются и в предел не входят.
pub async fn gc_cache(
    db: &Db,
    cutoff: Timestamp,
    max_entries: usize,
) -> Result<CacheGc, StoreError> {
    let cutoff = fmt_ts(cutoff);
    let max = i64::try_from(max_entries).unwrap_or(i64::MAX);
    db.write(move |t| {
        let drop =
            |t: &Connection, sql: &str, p: &dyn rusqlite::ToSql| -> Result<u64, StoreError> {
                let ids: Vec<Id> = t
                    .prepare(sql)?
                    .query_map([p], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut del_deps =
                    t.prepare_cached("DELETE FROM cache_deps WHERE cache_id = ?1")?;
                let mut del = t.prepare_cached("DELETE FROM answer_cache WHERE id = ?1")?;
                for id in &ids {
                    del_deps.execute([id])?;
                    del.execute([id])?;
                }
                Ok(ids.len() as u64)
            };
        let expired = drop(
            t,
            "SELECT id FROM answer_cache WHERE promoted_to IS NULL AND last_used_at < ?1",
            &cutoff,
        )?;
        let evicted = drop(
            t,
            "SELECT id FROM answer_cache
             WHERE promoted_to IS NULL AND deleted_at IS NULL
             ORDER BY last_used_at DESC, id DESC LIMIT -1 OFFSET ?1",
            &max,
        )?;
        Ok(CacheGc { expired, evicted })
    })
    .await
}

/// Число живых выжимок базы (для тестов и статуса).
pub async fn cache_count(db: &Db) -> Result<u64, StoreError> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM answer_cache WHERE deleted_at IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    })
    .await
}

/// JSON колонки `stale_reason`.
pub(crate) fn stale_json(list: &[StaleSource]) -> String {
    serde_json::to_string(list).expect("сериализация причин")
}

/// Записанные причины выжимки `id`.
pub(crate) fn stored_stale(c: &Connection, id: Id) -> Result<Vec<StaleSource>, StoreError> {
    let s: Option<String> = c
        .prepare_cached("SELECT stale_reason FROM answer_cache WHERE id = ?1")?
        .query_row([id], |r| r.get(0))?;
    Ok(match s {
        Some(s) => serde_json::from_str(&s)
            .map_err(|e| StoreError::Corrupt(format!("stale_reason выжимки {id}: {e}")))?,
        None => Vec::new(),
    })
}

/// Помечает выжимки недействительными с причинами (`by_cache`: выжимка → новые причины);
/// причины дописываются без повторов. Число выжимок, получивших новую причину.
pub(crate) fn mark_stale(
    c: &Connection,
    by_cache: HashMap<Id, Vec<StaleSource>>,
) -> Result<u32, StoreError> {
    let mut n = 0;
    let mut upd = c.prepare_cached(
        "UPDATE answer_cache SET state = 'stale', stale_reason = ?2 WHERE id = ?1",
    )?;
    for (id, add) in by_cache {
        let mut cur = stored_stale(c, id)?;
        let before = cur.len();
        for s in add {
            if !cur.contains(&s) {
                cur.push(s);
            }
        }
        if cur.len() > before {
            upd.execute(params![id, stale_json(&cur)])?;
            n += 1;
        }
    }
    Ok(n)
}
