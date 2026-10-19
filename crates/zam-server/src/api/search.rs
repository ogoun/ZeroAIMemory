//! `POST /search` (§9.1–9.3) — гибридный поиск: Qdrant (dense + sparse + сводки) и FTS5
//! (`bm25` + `trigram`), слияние weighted RRF в `zam-search` с весами секции `[search]`.
//!
//! **Тело** (остальные поля §9.1 принимаются и пока игнорируются):
//! - `query` — до 512 символов, синтаксис FTS5 в нём не действует;
//! - `scope`: `solution`, `include_knowledge` (true), `include_rules` (true),
//!   `all_solutions` (false), `path_prefix`, `spaces: [SpaceRef]`, `include_human`;
//! - `mode`: `brief` (по умолчанию) | `chunks` | `notes`;
//! - `expand`: `none` (по умолчанию) | `section` (текст всей секции чанка) | `neighbors`
//!   (чанк с соседними листами) | `graph` (400: будет в M3); действует в `mode=chunks`;
//! - `top_k` (10, 1..=100), `budget_tokens` (800), `format`: `json` (по умолчанию) | `md`.
//!
//! **Область.** `scope.spaces` задаёт пространства явно; иначе — `scope.solution`
//! (или заголовок `X-Zam-Solution`) плюс Knowledge и Rules по флагам, а при
//! `all_solutions` — все решения. Неизвестное решение или пространство из списка
//! просто не добавляет пространства (ответ 200, возможно пустой). Human — только
//! администратору и только с `include_human: true`; токен его не видит никогда.
//! Human ищется только каналом FTS: в Qdrant его нет, и сервер не передаёт его
//! в фильтр векторного запроса.
//!
//! **Алгоритм** (§9.2):
//! 1. Параллельно: каналы FTS и — если векторный канал доступен
//!    ([`IndexRuntime::vector_available`]) — эмбеддинг запроса (`spawn_blocking`, сессия
//!    запросов) и `VectorIndex::search` по пространствам области без Human.
//! 2. Векторные хиты проверяются по SQLite ([`live_chunks`]): чанк текущей ревизии, узел
//!    жив, не в корзине, не исключён (`index: false`), в области. Точка, оставшаяся
//!    в Qdrant после неудачного `delete`, в выдачу не попадает.
//! 3. `fuse` (weighted RRF + затухание + ≤ `max_per_node` с узла) → `note_to_leaf`:
//!    сводка (L1 заметки или секции) заменяется лучшим листом узла — ближайшим к запросу
//!    по косинусу (эмбеддинги из кэша `embeddings`), без вектора — первым листом секции.
//! 4. `top_k` (в `notes` — узлов), `expand`, упаковка в `budget_tokens`.
//!
//! **Деградация.** Нет модели, ORT, Qdrant, чужая модель коллекции или сбой запроса
//! (эмбеддинг, Qdrant) — ответ из одного FTS, `degraded: true` и `degraded_reason`
//! (`no_runtime`, `no_model`, `qdrant_down`, `qdrant_locked`, `broken`, `model_mismatch`,
//! `embed_failed`, `search_failed`), а не ошибка.
//!
//! **Бюджет** — в токенах счётчика чанкера (токенизатор модели, без неё — оценка):
//! карточка (путь, заголовок, сниппет или L0) плюс текст в `chunks` (`token_count`
//! чанка или расширенного текста). Элементы добавляются, пока помещаются; текст
//! последнего урезается до остатка (`text_truncated: true`). Хотя бы один элемент
//! возвращается всегда.
//!
//! **Режимы.** `brief` — карточки со `snippet`; `chunks` — плюс `text`; `notes` — по
//! одному элементу на узел (лучший чанк), `l0` вместо `snippet`.
//!
//! **Ответ JSON:** `items` (у элемента — `node_id`, `space`, `path`, `heading_path`,
//! `level`, `rev`, `chunk_ids`, `score` 0..1, `sources` — каналы `vector`/`fts`/`trigram`,
//! `tokens` — размер полного текста чанка, `updated_at`, `kind`, `snippet` | `l0`,
//! `text`), `degraded`, `degraded_reason` (или `null`), `pending_count` — живые заметки
//! области, ждущие векторной индексации, `timings {fts_ms, embed_ms, vector_ms, fuse_ms}`
//! (`null` — этап не выполнялся). Маркеры совпадений в JSON — U+E000/U+E001.
//!
//! **Формат `md`** — карточки §9.3, по одной на элемент:
//! `<n>. n:<id> <пространство>/<путь> › <секция>  [score] rev<N> · <дата> · ~<N> tok`,
//! под ней с отступом — сниппет (`brief`, маркеры → `**…**`), L0 (`notes`) или текст
//! (`chunks`). При деградации последняя строка — `> degraded: <причина> (только FTS)`.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_index::{IndexError, VectorFilter, VectorHit, VectorIndex, embed_key_of};
use zam_ml::Embedding;
use zam_search::{
    ChunkMeta, Cost, Fit, Level, Ranked, SearchInputs, Weights, fuse, note_to_leaf, pack,
    truncate_to_tokens,
};
use zam_store::time::fmt_ts;
use zam_store::{
    ChunkScope, Db, FtsChannels, FtsHit, FtsQuery, MARK_END, MARK_START, Space, SpaceKind,
    count_tokens, get_embeddings, get_node, get_solution, list_spaces, live_chunks,
    neighbor_leaves, node_content, node_leaves, pending_count, query_needles, search_fts_channels,
    text_snippet,
};

use super::json_body;
use super::scope::{SpaceRef, canonical_guid, find_space, token_can_read, user_db};
use crate::app::AppState;
use crate::auth::Principal;
use crate::config::SearchSection;
use crate::error::ApiError;
use crate::index_state::{DegradedReason, IndexRuntime};

/// Заголовок текущего решения агента.
pub const SOLUTION_HEADER: &str = "x-zam-solution";
const DEFAULT_TOP_K: usize = 10;
const DEFAULT_BUDGET_TOKENS: usize = 800;
/// Предел top_k в FTS (`zam_store::search`).
const TOP_K_MAX: usize = 100;
/// Сколько кандидатов слияния (× `top_k`) проходит замену сводок листами: после
/// слияния сводки с листом выдача короче, нужен запас.
const OVERSAMPLE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SearchMode {
    Brief,
    Chunks,
    Notes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutFormat {
    Json,
    Md,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Expand {
    None,
    Section,
    Neighbors,
    Graph,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScopeReq {
    solution: Option<String>,
    include_knowledge: Option<bool>,
    include_rules: Option<bool>,
    all_solutions: bool,
    path_prefix: Option<String>,
    spaces: Option<Vec<String>>,
    include_human: bool,
}

#[derive(Deserialize)]
struct SearchReq {
    query: String,
    #[serde(default)]
    scope: ScopeReq,
    mode: Option<SearchMode>,
    top_k: Option<usize>,
    budget_tokens: Option<usize>,
    format: Option<OutFormat>,
    expand: Option<Expand>,
}

/// Пространства запроса с учётом политики доступа (см. модуль).
async fn resolve_spaces(
    db: &Db,
    p: &Principal,
    headers: &HeaderMap,
    sc: &ScopeReq,
) -> Result<Vec<Space>, ApiError> {
    let human_ok = matches!(p, Principal::Admin { .. }) && sc.include_human;
    let mut out: Vec<Space> = Vec::new();
    if let Some(list) = &sc.spaces {
        for raw in list {
            // неизвестная форма или пространство — просто не добавляется
            let Ok(r) = SpaceRef::parse(raw) else {
                continue;
            };
            match find_space(db, &r).await {
                Ok(s) => out.push(s),
                Err(ApiError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
    } else {
        let solution = match &sc.solution {
            Some(g) => Some(
                canonical_guid(g.trim())
                    .ok_or_else(|| ApiError::BadRequest("scope.solution: GUID решения".into()))?,
            ),
            // заголовок агента: некорректное значение игнорируется
            None => headers
                .get(SOLUTION_HEADER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| canonical_guid(v.trim())),
        };
        if let Some(g) = solution
            && let Some(s) = get_solution(db, &g).await?
        {
            out.push(s);
        }
        let all = list_spaces(db).await?;
        for s in all {
            let take = match s.kind {
                SpaceKind::Knowledge => sc.include_knowledge.unwrap_or(true),
                SpaceKind::Rules => sc.include_rules.unwrap_or(true),
                SpaceKind::Solution => sc.all_solutions,
                SpaceKind::Human => human_ok,
            };
            if take {
                out.push(s);
            }
        }
    }
    out.retain(|s| token_can_read(p, s) && (s.kind != SpaceKind::Human || human_ok));
    out.sort_by_key(|s| s.id);
    out.dedup_by_key(|s| s.id);
    Ok(out)
}

impl SearchSection {
    /// Параметры слияния `zam-search`.
    fn weights(&self) -> Weights {
        Weights {
            rrf_k: self.rrf_k,
            w_vector: self.w_vector,
            w_fts: self.w_fts,
            w_trigram: self.w_trigram,
            recency_half_life_days: self.recency_half_life_days,
            max_per_node: self.max_per_node,
        }
    }
}

/// Итог векторного канала одного запроса.
#[derive(Default)]
struct VectorRun {
    /// `None` — канал не запускался или не удался.
    hits: Option<Vec<VectorHit>>,
    /// Причина, если канал не удался во время запроса.
    failed: Option<DegradedReason>,
    /// Плотный вектор запроса (выбор лучшего листа).
    dense: Option<Vec<f32>>,
    embed_ms: Option<f64>,
    vector_ms: Option<f64>,
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}

/// Векторный канал сервера: эмбеддер и индекс из [`IndexRuntime`].
async fn vector_channel(index: &IndexRuntime, filter: VectorFilter, query: String) -> VectorRun {
    let (Some(emb), Some(vector)) = (index.embedder.clone(), index.vector.clone()) else {
        return VectorRun::default();
    };
    let embed = move || emb.embed_query(&query).map_err(|e| e.to_string());
    run_vector_channel(embed, vector.as_ref(), filter).await
}

/// Эмбеддинг запроса (`embed` — в `spawn_blocking`) и поиск в индексе. Ошибка и паника
/// эмбеддинга, сбой индекса — деградация с причиной (в журнал — без текста запроса).
async fn run_vector_channel(
    embed: impl FnOnce() -> Result<Embedding, String> + Send + 'static,
    vector: &dyn VectorIndex,
    filter: VectorFilter,
) -> VectorRun {
    let mut run = VectorRun::default();
    let t = Instant::now();
    let e = match tokio::task::spawn_blocking(embed).await {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "эмбеддинг запроса не удался: поиск без векторного канала");
            run.failed = Some(DegradedReason::EmbedFailed);
            return run;
        }
        Err(j) => {
            tracing::error!(
                panic = j.is_panic(),
                "эмбеддинг запроса аварийно завершился: поиск без векторного канала"
            );
            run.failed = Some(DegradedReason::EmbedFailed);
            return run;
        }
    };
    run.embed_ms = Some(ms(t));
    let t = Instant::now();
    match vector.search(&e.dense, &e.sparse, &filter).await {
        Ok(h) => run.hits = Some(h),
        Err(err) => {
            tracing::warn!(error = %err, "векторный поиск не удался: ответ только из FTS");
            run.failed = Some(match err {
                IndexError::Unavailable(_) | IndexError::NotFound(_) => DegradedReason::QdrantDown,
                _ => DegradedReason::SearchFailed,
            });
        }
    }
    run.vector_ms = Some(ms(t));
    run.dense = Some(e.dense);
    run
}

/// Скалярное произведение (векторы нормированы: косинус).
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Лучший лист для каждой сводки без листа своего узла в `ranked`: `chunk_id сводки →
/// chunk_id листа`. Кандидаты — листы секции сводки (`section`) или всей заметки
/// (`note`); с вектором запроса — ближайший по косинусу среди листов с эмбеддингом
/// в кэше, иначе первый по порядку.
async fn best_leaves(
    db: &Db,
    ranked: &[Ranked],
    hits: &HashMap<String, FtsHit>,
    query: Option<(&[f32], &str)>,
) -> Result<HashMap<String, String>, ApiError> {
    let with_leaf: HashSet<Id> = ranked
        .iter()
        .filter(|r| r.level == Level::Chunk)
        .map(|r| r.node_id)
        .collect();
    let mut out = HashMap::new();
    for r in ranked {
        if !r.level.is_summary() || with_leaf.contains(&r.node_id) {
            continue;
        }
        let leaves = node_leaves(db, r.node_id).await?;
        let hp = hits
            .get(&r.chunk_id)
            .map_or("", |h| h.heading_path.as_str());
        let in_section = |l: &&zam_store::LeafChunk| {
            r.level == Level::Note
                || hp.is_empty()
                || l.heading_path == hp
                || l.heading_path.starts_with(&format!("{hp} > "))
        };
        let mut cands: Vec<&zam_store::LeafChunk> = leaves.iter().filter(in_section).collect();
        if cands.is_empty() {
            cands = leaves.iter().collect();
        }
        let Some(first) = cands.first() else {
            continue;
        };
        let mut best = first.chunk_id.clone();
        if let Some((q, model)) = query {
            let keys: Vec<String> = cands
                .iter()
                .map(|l| embed_key_of(&l.context_prefix, &l.text))
                .collect();
            let embs = get_embeddings(db, model, &keys).await?;
            let scored = cands
                .iter()
                .zip(&keys)
                .filter_map(|(l, k)| embs.get(k).map(|e| (dot(q, &e.dense), l)))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, l)) = scored {
                best = l.chunk_id.clone();
            }
        }
        out.insert(r.chunk_id.clone(), best);
    }
    Ok(out)
}

/// Элемент выдачи до упаковки.
struct Item {
    hit: FtsHit,
    score: f64,
    sources: Vec<&'static str>,
    chunk_ids: Vec<String>,
    l0: Option<String>,
    /// Текст для `chunks` (после `expand`) и его размер в токенах.
    text: Option<(String, usize)>,
}

/// `notes`: по одному элементу на узел (лучший чанк), не больше `top_k`.
fn group_by_node(items: Vec<Item>, top_k: usize) -> Vec<Item> {
    let mut order: Vec<Item> = Vec::new();
    let mut pos: HashMap<Id, usize> = HashMap::new();
    for it in items {
        match pos.get(&it.hit.node_id) {
            Some(&i) => order[i].chunk_ids.push(it.hit.chunk_id),
            None => {
                pos.insert(it.hit.node_id, order.len());
                order.push(it);
            }
        }
    }
    order.truncate(top_k);
    order
}

/// Собственный текст секции `heading_path` (заголовок и текст до первой подсекции);
/// пустой путь — преамбула. `None` — секции нет.
fn own_section<'a>(src: &'a str, heading_path: &str) -> Option<&'a str> {
    let doc = zam_markdown::parse(src);
    let i = doc.sections.iter().position(|s| {
        if heading_path.is_empty() {
            s.level == 0
        } else {
            s.level > 0 && s.path.join(" > ") == heading_path
        }
    })?;
    let s = &doc.sections[i];
    let end = doc
        .sections
        .get(i + 1)
        .map_or(s.range.end, |n| n.range.start.min(s.range.end))
        .max(s.body.start.min(s.range.end));
    src.get(s.range.start..end).map(str::trim_end)
}

/// `expand=section|neighbors` для `chunks`: текст элемента заменяется расширенным;
/// повторная секция того же узла сливается в первый элемент (`chunk_ids`).
async fn expand_items(db: &Db, items: Vec<Item>, how: Expand) -> Result<Vec<Item>, ApiError> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    let mut sections: HashMap<(Id, String), usize> = HashMap::new();
    let mut contents: HashMap<(Id, i64), Arc<String>> = HashMap::new();
    for mut it in items {
        let base = it.hit.text.clone();
        let text = match how {
            Expand::Section => {
                let key = (it.hit.node_id, it.hit.heading_path.clone());
                if let Some(&i) = sections.get(&key) {
                    let ids = std::mem::take(&mut it.chunk_ids);
                    out[i].chunk_ids.extend(ids);
                    continue;
                }
                sections.insert(key, out.len());
                let ck = (it.hit.node_id, it.hit.rev);
                let src = match contents.get(&ck) {
                    Some(s) => s.clone(),
                    None => {
                        let (_, s) = node_content(db, it.hit.node_id, Some(it.hit.rev)).await?;
                        let s = Arc::new(s);
                        contents.insert(ck, s.clone());
                        s
                    }
                };
                // склеенные чанкером короткие секции: лист может быть длиннее секции
                match own_section(&src, &it.hit.heading_path) {
                    Some(sec) if sec.chars().count() >= base.chars().count() => sec.to_string(),
                    _ => base,
                }
            }
            Expand::Neighbors => {
                let (prev, next) = neighbor_leaves(db, it.hit.chunk_id.clone()).await?;
                [prev, Some(base), next]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
            Expand::None | Expand::Graph => base,
        };
        let tokens = count_tokens(&text);
        it.text = Some((text, tokens));
        out.push(it);
    }
    Ok(out)
}

/// Упакованный элемент: элемент и (для `chunks`) текст с признаком обрезки.
struct Packed {
    item: Item,
    text: Option<(String, bool)>,
}

/// Строка карточки без текста: по ней считается стоимость в токенах.
fn card_summary(it: &Item, mode: SearchMode) -> &str {
    match mode {
        SearchMode::Notes => it.l0.as_deref().unwrap_or(""),
        _ => &it.hit.snippet,
    }
}

/// Упаковка в бюджет (см. модуль).
fn pack_items(items: Vec<Item>, mode: SearchMode, budget_tokens: usize) -> Vec<Packed> {
    let costs: Vec<Cost> = items
        .iter()
        .map(|it| Cost {
            base: count_tokens(&format!(
                "{} {} {}",
                it.hit.path,
                it.hit.heading_path,
                card_summary(it, mode)
            )),
            text: it.text.as_ref().map_or(0, |t| t.1),
        })
        .collect();
    let fits = pack(&costs, budget_tokens);
    items
        .into_iter()
        .zip(fits)
        .map(|(mut it, fit)| {
            let text = it.text.take().map(|(t, _)| match fit {
                Fit::Whole => (t, false),
                Fit::Truncated(n) => (truncate_to_tokens(&t, n, &count_tokens).to_string(), true),
            });
            Packed { item: it, text }
        })
        .collect()
}

/// Балл в ответе: 4 знака.
fn round_score(s: f64) -> f64 {
    (s * 10_000.0).round() / 10_000.0
}

fn item_json(p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> Value {
    let h = &p.item.hit;
    let mut o = Map::new();
    o.insert("node_id".into(), json!(h.node_id));
    o.insert("space".into(), json!(spaces.get(&h.space_id)));
    o.insert("path".into(), json!(h.path));
    o.insert("heading_path".into(), json!(h.heading_path));
    o.insert("level".into(), json!(h.level));
    o.insert("rev".into(), json!(h.rev));
    o.insert("chunk_ids".into(), json!(p.item.chunk_ids));
    o.insert("score".into(), json!(round_score(p.item.score)));
    o.insert("sources".into(), json!(p.item.sources));
    o.insert("tokens".into(), json!(h.token_count));
    o.insert("updated_at".into(), json!(fmt_ts(h.updated_at)));
    o.insert("kind".into(), json!("note"));
    if mode == SearchMode::Notes {
        o.insert("l0".into(), json!(p.item.l0));
    } else {
        o.insert("snippet".into(), json!(h.snippet));
    }
    if let Some((t, truncated)) = &p.text {
        o.insert("text".into(), json!(t));
        if *truncated {
            o.insert("text_truncated".into(), json!(true));
        }
    }
    Value::Object(o)
}

/// Одна строка: маркеры → `**`, переводы строк → пробелы.
fn md_inline(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\r' | '\n' => ' ',
            c => c,
        })
        .collect::<String>()
        .replace([MARK_START, MARK_END], "**")
}

/// Карточка §9.3: `1. n:<id> <space>/<path> › <секция>  [0.82] rev7 · 2026-10-10 · ~340 tok`.
fn item_md(n: usize, p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> String {
    let h = &p.item.hit;
    let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
    let mut line = format!("{n}. n:{} {space}/{}", h.node_id, h.path);
    if !h.heading_path.is_empty() {
        line.push_str(" › ");
        line.push_str(&h.heading_path);
    }
    line.push_str(&format!(
        "  [{:.2}] rev{} · {} · ~{} tok\n",
        p.item.score,
        h.rev,
        h.updated_at.strftime("%Y-%m-%d"),
        h.token_count
    ));
    match &p.text {
        Some((t, _)) => {
            for l in t.lines() {
                line.push_str("   ");
                line.push_str(l);
                line.push('\n');
            }
        }
        None => {
            let summary = md_inline(card_summary(&p.item, mode));
            if !summary.trim().is_empty() {
                line.push_str("   ");
                line.push_str(summary.trim());
                line.push('\n');
            }
        }
    }
    line
}

/// `POST /search`.
pub async fn search(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let req: SearchReq = json_body(body)?;
    let mode = req.mode.unwrap_or(SearchMode::Brief);
    let format = req.format.unwrap_or(OutFormat::Json);
    let expand = req.expand.unwrap_or(Expand::None);
    let top_k = req.top_k.unwrap_or(DEFAULT_TOP_K);
    if !(1..=TOP_K_MAX).contains(&top_k) {
        return Err(ApiError::BadRequest(format!("top_k: от 1 до {TOP_K_MAX}")));
    }
    if expand == Expand::Graph {
        return Err(ApiError::BadRequest(
            "expand=graph будет в M3 (граф знаний)".into(),
        ));
    }
    if req.query.chars().count() > zam_store::search::QUERY_MAX_CHARS {
        return Err(ApiError::BadRequest(format!(
            "запрос длиннее {} символов",
            zam_store::search::QUERY_MAX_CHARS
        )));
    }
    let budget = req.budget_tokens.unwrap_or(DEFAULT_BUDGET_TOKENS);
    let (user, db) = user_db(&st, &p, &headers).await?;
    let spaces = resolve_spaces(&db, &p, &headers, &req.scope).await?;
    let space_ids: Vec<Id> = spaces.iter().map(|s| s.id).collect();
    // Human в Qdrant нет никогда: векторный запрос — только по остальным пространствам
    let vec_spaces: Vec<Id> = spaces
        .iter()
        .filter(|s| s.kind != SpaceKind::Human)
        .map(|s| s.id)
        .collect();
    let prefix = req
        .scope
        .path_prefix
        .as_deref()
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let available = st.index.vector_available();
    let run_vector = available.is_ok() && !vec_spaces.is_empty() && !req.query.trim().is_empty();

    // 1. каналы FTS и векторный — параллельно
    let fts = async {
        let t = Instant::now();
        let r = if space_ids.is_empty() {
            Ok(FtsChannels::default())
        } else {
            search_fts_channels(
                &db,
                FtsQuery {
                    text: req.query.clone(),
                    space_ids: space_ids.clone(),
                    path_prefix: prefix.clone(),
                    top_k: TOP_K_MAX,
                },
            )
            .await
        };
        (r, ms(t))
    };
    let vector = async {
        if !run_vector {
            return VectorRun::default();
        }
        let filter = VectorFilter {
            user_id: user.id,
            space_ids: vec_spaces.clone(),
            path_prefix: prefix.clone(),
            levels: Vec::new(),
        };
        vector_channel(&st.index, filter, req.query.clone()).await
    };
    let ((fts, fts_ms), vrun) = tokio::join!(fts, vector);
    let fts = fts?;

    // 2. векторные хиты — только живые чанки области по SQLite
    let fuse_t = Instant::now();
    let mut hits: HashMap<String, FtsHit> = HashMap::new();
    for h in fts.bm25.iter().chain(&fts.trigram) {
        hits.entry(h.chunk_id.clone()).or_insert_with(|| h.clone());
    }
    let vector_ids: Option<Vec<String>> = match &vrun.hits {
        Some(vh) => {
            let ids: Vec<String> = vh.iter().map(|h| h.chunk_id.clone()).collect();
            let live = live_chunks(
                &db,
                ids,
                ChunkScope {
                    space_ids: vec_spaces.clone(),
                    path_prefix: prefix.clone(),
                    indexable_only: true,
                },
            )
            .await?;
            let ids = live.iter().map(|h| h.chunk_id.clone()).collect();
            for h in live {
                hits.entry(h.chunk_id.clone()).or_insert(h);
            }
            Some(ids)
        }
        None => None,
    };

    // 3. слияние, сводки → листы
    let chunks: HashMap<String, ChunkMeta> = hits
        .iter()
        .filter_map(|(id, h)| {
            Some((
                id.clone(),
                ChunkMeta {
                    node_id: h.node_id,
                    level: Level::parse(&h.level)?,
                    updated_at: h.updated_at,
                },
            ))
        })
        .collect();
    let inputs = SearchInputs {
        fts: fts.bm25.iter().map(|h| h.chunk_id.clone()).collect(),
        trigram: fts.trigram.iter().map(|h| h.chunk_id.clone()).collect(),
        vector: vector_ids,
        chunks,
    };
    let mut ranked = fuse(&inputs, &st.cfg.search.weights(), jiff::Timestamp::now());
    ranked.truncate(top_k * OVERSAMPLE);
    let query_vec = vrun
        .dense
        .as_deref()
        .zip(st.index.model_id())
        .filter(|_| vrun.hits.is_some());
    let best = best_leaves(&db, &ranked, &hits, query_vec).await?;
    let ranked = note_to_leaf(ranked, |r| best.get(&r.chunk_id).cloned());
    let missing: Vec<String> = ranked
        .iter()
        .filter(|r| !hits.contains_key(&r.chunk_id))
        .map(|r| r.chunk_id.clone())
        .collect();
    if !missing.is_empty() {
        let scope = ChunkScope {
            space_ids: space_ids.clone(),
            path_prefix: prefix.clone(),
            indexable_only: false,
        };
        for h in live_chunks(&db, missing, scope).await? {
            hits.insert(h.chunk_id.clone(), h);
        }
    }
    let needles = query_needles(&req.query);
    let needles: Vec<&str> = needles.iter().map(String::as_str).collect();
    let items: Vec<Item> = ranked
        .into_iter()
        .filter_map(|r| {
            let mut hit = hits.get(&r.chunk_id)?.clone();
            if hit.snippet.is_empty() {
                hit.snippet = text_snippet(&hit.text, &needles);
            }
            Some(Item {
                chunk_ids: vec![hit.chunk_id.clone()],
                hit,
                score: r.score,
                sources: r.sources,
                l0: None,
                text: None,
            })
        })
        .collect();
    let fuse_ms = ms(fuse_t);

    // 4. top_k, режим, expand, упаковка
    let mut items = match mode {
        SearchMode::Notes => group_by_node(items, top_k),
        _ => items.into_iter().take(top_k).collect(),
    };
    match mode {
        SearchMode::Notes => {
            for it in &mut items {
                it.l0 = get_node(&db, it.hit.node_id).await?.l0;
            }
        }
        SearchMode::Chunks => items = expand_items(&db, items, expand).await?,
        SearchMode::Brief => {}
    }
    // заметки области, ещё не прошедшие векторную индексацию (без `index: false` и Human)
    let pending = pending_count(&db, space_ids.clone(), prefix.clone()).await?;
    let packed = pack_items(items, mode, budget);
    let degraded = available.err().or(vrun.failed);
    let by_id: HashMap<Id, Space> = spaces.into_iter().map(|s| (s.id, s)).collect();

    Ok(match format {
        OutFormat::Json => {
            let items: Vec<Value> = packed.iter().map(|p| item_json(p, mode, &by_id)).collect();
            Json(json!({
                "items": items,
                "degraded": degraded.is_some(),
                "degraded_reason": degraded.map(DegradedReason::as_str),
                "pending_count": pending,
                "timings": {
                    "fts_ms": fts_ms,
                    "embed_ms": vrun.embed_ms,
                    "vector_ms": vrun.vector_ms,
                    "fuse_ms": fuse_ms,
                },
            }))
            .into_response()
        }
        OutFormat::Md => {
            let mut body: String = packed
                .iter()
                .enumerate()
                .map(|(i, p)| item_md(i + 1, p, mode, &by_id))
                .collect();
            if let Some(r) = degraded
                && !packed.is_empty()
            {
                body.push_str(&format!("> degraded: {} (только FTS)\n", r.as_str()));
            }
            let mut r = body.into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/markdown; charset=utf-8"),
            );
            r
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(node: Id, text: &str) -> FtsHit {
        FtsHit {
            chunk_id: format!("c{}", text.len()),
            node_id: node,
            space_id: Id::new(),
            path: "p".into(),
            heading_path: String::new(),
            level: "chunk".into(),
            rev: 1,
            token_count: count_tokens(text) as i64,
            score: 1.0,
            updated_at: jiff::Timestamp::now(),
            snippet: format!("{MARK_START}s{MARK_END}"),
            text: text.into(),
        }
    }

    fn items(hits: Vec<FtsHit>, with_text: bool) -> Vec<Item> {
        hits.into_iter()
            .map(|h| Item {
                chunk_ids: vec![h.chunk_id.clone()],
                text: with_text.then(|| (h.text.clone(), h.token_count as usize)),
                hit: h,
                score: 0.5,
                sources: vec!["fts"],
                l0: None,
            })
            .collect()
    }

    #[test]
    fn budget_trims_text_and_keeps_one() {
        let long = "слово ".repeat(100);
        let hs = vec![hit(Id::new(), &long), hit(Id::new(), "бб")];
        let p = pack_items(items(hs, true), SearchMode::Chunks, 10);
        assert_eq!(p.len(), 1);
        let (t, tr) = p[0].text.clone().unwrap();
        assert!(tr && count_tokens(&t) <= 10 && !t.is_empty(), "{t}");
        let p = pack_items(
            items(vec![hit(Id::new(), "x")], true),
            SearchMode::Chunks,
            0,
        );
        assert_eq!(p.len(), 1);
        let p = pack_items(
            items(vec![hit(Id::new(), "x"), hit(Id::new(), "y")], false),
            SearchMode::Brief,
            0,
        );
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn notes_group_by_node() {
        let a = Id::new();
        let b = Id::new();
        let g = group_by_node(
            items(vec![hit(a, "1"), hit(b, "22"), hit(a, "333")], false),
            10,
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].chunk_ids, vec!["c1".to_string(), "c3".to_string()]);
        assert_eq!(
            group_by_node(items(vec![hit(a, "1"), hit(b, "2")], false), 1).len(),
            1
        );
    }

    /// Индекс, отвечающий на поиск заданной ошибкой (остальное не вызывается).
    struct FailingIndex(fn() -> IndexError);

    impl VectorIndex for FailingIndex {
        fn ensure<'a>(
            &'a self,
            _: &'a str,
            _: usize,
        ) -> zam_index::IndexFuture<'a, zam_index::IndexMeta> {
            unreachable!()
        }
        fn upsert(&self, _: Vec<zam_index::IndexPoint>) -> zam_index::IndexFuture<'_, ()> {
            unreachable!()
        }
        fn delete_node(&self, _: Id, _: Id) -> zam_index::IndexFuture<'_, ()> {
            unreachable!()
        }
        fn search<'a>(
            &'a self,
            _: &'a [f32],
            _: &'a [(u32, f32)],
            _: &'a VectorFilter,
        ) -> zam_index::IndexFuture<'a, Vec<VectorHit>> {
            let e = (self.0)();
            Box::pin(async move { Err(e) })
        }
        fn count<'a>(&'a self, _: &'a VectorFilter) -> zam_index::IndexFuture<'a, u64> {
            unreachable!()
        }
        fn drop_all(&self) -> zam_index::IndexFuture<'_, ()> {
            unreachable!()
        }
        fn health(&self) -> zam_index::IndexFuture<'_, ()> {
            unreachable!()
        }
    }

    fn filter() -> VectorFilter {
        VectorFilter {
            user_id: Id::new(),
            space_ids: vec![Id::new()],
            path_prefix: None,
            levels: Vec::new(),
        }
    }

    fn emb() -> Result<Embedding, String> {
        Ok(Embedding {
            dense: vec![1.0, 0.0],
            sparse: Vec::new(),
        })
    }

    #[tokio::test]
    async fn embed_error_or_panic_degrades_not_fails() {
        let idx = FailingIndex(|| unreachable!("до индекса не доходит"));
        let r = run_vector_channel(|| Err("сбой".into()), &idx, filter()).await;
        assert_eq!(r.failed, Some(DegradedReason::EmbedFailed));
        assert!(r.hits.is_none() && r.embed_ms.is_none());
        let r = run_vector_channel(|| panic!("инференс упал"), &idx, filter()).await;
        assert_eq!(r.failed, Some(DegradedReason::EmbedFailed));
        assert!(r.hits.is_none());
    }

    #[tokio::test]
    async fn index_errors_map_to_reasons() {
        for (mk, want) in [
            (
                (|| IndexError::Unavailable("x".into())) as fn() -> IndexError,
                DegradedReason::QdrantDown,
            ),
            (
                || IndexError::NotFound("x".into()),
                DegradedReason::QdrantDown,
            ),
            (
                || IndexError::Corrupt("x".into()),
                DegradedReason::SearchFailed,
            ),
            (|| IndexError::Auth, DegradedReason::SearchFailed),
        ] {
            let r = run_vector_channel(emb, &FailingIndex(mk), filter()).await;
            assert_eq!(r.failed, Some(want));
            assert!(r.hits.is_none() && r.embed_ms.is_some() && r.vector_ms.is_some());
        }
    }

    #[test]
    fn md_markers() {
        assert_eq!(md_inline("a\u{E000}b\u{E001}\nc"), "a**b** c");
    }

    #[test]
    fn own_section_text() {
        let src = "Преамбула.\n\n# A\n\nтекст A\n\n## B\n\nтекст B\n\n# C\n\nтекст C\n";
        assert_eq!(own_section(src, "A"), Some("# A\n\nтекст A"));
        assert_eq!(own_section(src, "A > B"), Some("## B\n\nтекст B"));
        assert_eq!(own_section(src, ""), Some("Преамбула."));
        assert_eq!(own_section(src, "C"), Some("# C\n\nтекст C"));
        assert_eq!(own_section(src, "Q"), None);
    }

    #[test]
    fn md_card_format() {
        let mut it = items(vec![hit(Id::new(), "текст")], false).remove(0);
        it.hit.heading_path = "A > B".into();
        it.hit.updated_at = "2026-10-10T08:00:00Z".parse().unwrap();
        let p = Packed {
            item: it,
            text: None,
        };
        let s = item_md(1, &p, SearchMode::Brief, &HashMap::new());
        let want = format!(
            "1. n:{} ?/p › A > B  [0.50] rev1 · 2026-10-10 · ~{} tok\n   **s**\n",
            p.item.hit.node_id, p.item.hit.token_count
        );
        assert_eq!(s, want);
    }
}
