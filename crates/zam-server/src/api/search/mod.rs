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
//! - `top_k` (10, 1..=100), `budget_tokens` (800), `format`: `json` (по умолчанию) | `md`;
//! - `rerank`: `off` (по умолчанию) | `on` — серверный реранк (см. ниже).
//!
//! **Область** ([`targets`]): цели — личная база пользователя (Knowledge, Rules, свои
//! решения, Human — только администратору с `include_human: true`) и базы владельцев
//! доступных чужих решений (у каждой — ровно пространство решения). `scope.spaces` задаёт
//! пространства явно; иначе — `scope.solution` (или заголовок `X-Zam-Solution`) плюс
//! Knowledge и Rules по флагам, а при `all_solutions` — все доступные решения. Неизвестное
//! решение или пространство из списка просто не добавляет пространства (ответ 200, возможно
//! пустой); чужое решение без одобренного доступа исключается с предупреждением
//! `solution access pending|rejected|revoked`. Human ищется только каналом FTS: в Qdrant его
//! нет, и сервер не передаёт его в фильтр векторного запроса.
//!
//! **Алгоритм** (§9.2):
//! 1. Параллельно: каналы FTS по каждой цели в пределах её пространств (базы — параллельно,
//!    затем [`merge::merge_channels`] по сырому баллу FTS5) и — если векторный канал доступен
//!    ([`IndexRuntime::vector_available`]) — эмбеддинг запроса (`spawn_blocking`, сессия
//!    запросов) и **один** `VectorIndex::search` с областями всех целей без Human.
//! 2. Векторные хиты направляются в базу владельца (`VectorHit::user_id`) и проверяются по
//!    SQLite ([`live_chunks`]) в пространствах своей цели: чанк текущей ревизии, узел
//!    жив, не в корзине, не исключён (`index: false`), в области. Точка, оставшаяся
//!    в Qdrant после неудачного `delete`, в выдачу не попадает. Все дальнейшие чтения
//!    (листы сводок, `expand`, L0, `pending_count`) идут в базу цели элемента.
//! 3. `fuse` (weighted RRF + затухание; bm25 ослабленного запроса — с весом
//!    `w_fts_relaxed`, если векторный канал работает и чанк ему виден, т. е. не Human и узел
//!    проиндексирован) → `note_to_leaf` → ≤ `max_per_node` с узла:
//!    сводка (L1 заметки или секции) сливается с найденным листом своего узла (для
//!    секции — листом этой секции) или заменяется лучшим листом: ближайшим к запросу по
//!    косинусу (эмбеддинги из кэша `embeddings`), без вектора — листом с наибольшим числом
//!    слов запроса; ни одного совпадения — сводка остаётся. Пути листов всех сводок
//!    читаются одним запросом, тексты — вторым и только у листов-кандидатов (нужны для
//!    ключа кэша), эмбеддинги — одним. Сводка за окном кандидатов без листа в выдаче
//!    отбрасывается (узел, найденный только ею, не попадает). Лимит на узел — после замены.
//! 4. `rerank=on` ([`crate::rerank`]): первые `ml.rerank_top` (20) кандидатов после
//!    `note_to_leaf` и лимита на узел переставляются по баллу реранкера (пара «запрос —
//!    заголовок секции и текст чанка», фрагмент до `ml.rerank_tokens` = 256 токенов), хвост
//!    остаётся в порядке слияния; лимит на узел после перестановки соблюдается (набор тот
//!    же). Реранкер — по устройству эмбеддера, грузится при первом таком запросе
//!    (`spawn_blocking`). Нет модели — ответ без реранка и `warnings: ["reranker not
//!    installed"]`; ошибка или паника загрузки и инференса — `["reranker failed"]`, не 500.
//! 5. `top_k` (в `notes` — узлов), `expand`, упаковка в `budget_tokens`.
//!
//! **Деградация.** Нет модели, ORT, Qdrant, чужая модель коллекции или сбой запроса
//! (эмбеддинг, Qdrant) — ответ из одного FTS (по всем целям), `degraded: true` и `degraded_reason`
//! (`no_runtime`, `no_model`, `qdrant_down`, `qdrant_locked`, `broken`, `model_mismatch`,
//! `embed_failed`, `search_failed`) с текстом по-русски `degraded_text`, а не ошибка.
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
//! **Ответ JSON:** `items` (у элемента — `node_id`, `space`, `agent_solution_id` — у элемента
//! решения (своего или чужого), `path`, `heading_path`,
//! `level`, `rev`, `chunk_ids`, `score` 0..1, `sources` — каналы `vector`/`fts`/`trigram`,
//! `tokens` — размер отданного текста в `chunks` (после `expand` и усечения), иначе
//! полного текста чанка, `updated_at`, `kind`, `snippet` | `l0`,
//! `text`, `rerank_score` 0..1 — у переставленных реранком), `degraded`, `degraded_reason`
//! и `degraded_text` (или `null`), `warnings` (строки: о реранке, `index rebuilding` — индекс
//! пересобирается, векторная выдача может быть неполной, `solution access …` и
//! `solution unavailable` — решение исключено из области), `pending_count` — живые
//! заметки области всех целей, ждущие векторной индексации, `timings {fts_ms, embed_ms, vector_ms,
//! fuse_ms, rerank_ms, expand_ms, pending_ms, total_ms}` (`null` — этап не выполнялся;
//! `total_ms` — весь обработчик без сериализации ответа). Маркеры совпадений в JSON —
//! U+E000/U+E001.
//!
//! **Формат `md`** — карточки §9.3, по одной на элемент:
//! `<n>. n:<id> <пространство>/<путь> › <секция>  [score] rev<N> · <дата> · ~<N> tok`,
//! под ней с отступом — сниппет (`brief`, маркеры → `**…**`), L0 (`notes`) или текст
//! (`chunks`); балл — реранка, если элемент переставлен им. При деградации строка
//! `> degraded: <код> — <текст причины> (только FTS)`, в том числе при пустой выдаче (тогда она
//! единственная), предупреждения — строками `> warning: <текст>` в конце.
//!
//! **Ошибки баз владельцев.** Сбой FTS, проверки векторных хитов или `pending_count` в базе
//! чужого решения — решение исключается (WARN в журнал, `solution unavailable`), ответ строится
//! по остальным целям; сбой личной базы — ошибка запроса, как в M2.
pub mod merge;
pub mod targets;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_index::{IndexError, VectorFilter, VectorHit, VectorIndex, VectorScope};
use zam_ml::Embedding;
use zam_search::{
    ChunkMeta, Cost, Fit, LeafRef, Level, Ranked, SearchInputs, Weights, cap_per_node,
    drop_unresolved_tail, fuse, in_section, note_to_leaf, pack, summaries_without_leaf,
    truncate_to_tokens,
};
use zam_store::time::fmt_ts;
use zam_store::{
    ChunkScope, Db, FtsHit, FtsQuery, LeafHit, MARK_END, MARK_START, Space, SpaceKind, StoreError,
    count_tokens, get_embeddings, get_node, leaf_term_hits, leaves_of_nodes, live_chunks,
    neighbor_leaves, node_content, pending_count, query_needles, search_fts_channels, text_snippet,
};

use self::merge::merge_channels;
use self::targets::{
    ScopeReq, SearchTarget, WARN_SOLUTION_UNAVAILABLE, dedup_keep_order, resolve_targets,
};
use super::json_body;
use super::scope::skip_foreign;
use crate::app::AppState;
use crate::auth::Principal;
use crate::config::SearchSection;
use crate::error::ApiError;
use crate::index_state::{DegradedReason, IndexRuntime};
use crate::rerank;

/// Заголовок текущего решения агента.
pub const SOLUTION_HEADER: &str = "x-zam-solution";
const DEFAULT_TOP_K: usize = 10;
const DEFAULT_BUDGET_TOKENS: usize = 800;
/// Предел top_k в FTS (`zam_store::search`).
const TOP_K_MAX: usize = 100;
/// Кандидатов слитого канала FTS нескольких баз (как `bm25` одной базы в `zam_store`).
const FTS_CHANNEL_TOP: usize = 50;
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
enum RerankMode {
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Expand {
    None,
    Section,
    Neighbors,
    Graph,
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
    rerank: Option<RerankMode>,
}

impl SearchSection {
    /// Параметры слияния `zam-search`.
    fn weights(&self) -> Weights {
        Weights {
            rrf_k: self.rrf_k,
            w_vector: self.w_vector,
            w_fts: self.w_fts,
            w_fts_relaxed: self.w_fts_relaxed,
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
            run.embed_ms = Some(ms(t));
            return run;
        }
        Err(j) => {
            tracing::error!(
                panic = j.is_panic(),
                "эмбеддинг запроса аварийно завершился: поиск без векторного канала"
            );
            run.failed = Some(DegradedReason::EmbedFailed);
            run.embed_ms = Some(ms(t));
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

/// Лист для каждой сводки из `summaries` (у них нет подходящего листа в выдаче):
/// `chunk_id сводки → лист`. Кандидаты — листы секции сводки (`section`) или всей
/// заметки (`note`):
/// - с вектором запроса — ближайший по косинусу среди листов с эмбеддингом в кэше;
/// - иначе (или без эмбеддингов) — лист, где FTS находит больше всего слов запроса
///   ([`query_needles`]), при равенстве — первый; ни одного — сводка остаётся как есть
///   (как в M1).
///
/// Всё пакетно, без запроса на каждую сводку и без чтения текстов листов: совпадения
/// слов — по индексу FTS; при векторе — пути и ключи кэша (`chunks.embed_key`) листов
/// всех узлов одним чтением, эмбеддинги листов-кандидатов — одним `get_embeddings`.
async fn best_leaves(
    db: &Db,
    summaries: &[&Ranked],
    needles: &[String],
    query: Option<(&[f32], &str)>,
) -> Result<HashMap<String, LeafRef>, ApiError> {
    // сводки есть только у узлов
    let summaries: Vec<&Ranked> = summaries
        .iter()
        .copied()
        .filter(|r| r.target.kind == TargetKind::Node)
        .collect();
    let mut nodes: Vec<Id> = summaries.iter().map(|r| r.target.id).collect();
    nodes.sort();
    nodes.dedup();
    if nodes.is_empty() {
        return Ok(HashMap::new());
    }
    let fits = |r: &Ranked, hp: &str| r.level == Level::Note || in_section(hp, &r.heading_path);
    let mut out: HashMap<String, LeafRef> = HashMap::new();
    if let Some((q, model)) = query {
        // пути и ключи кэша листов без текстов; эмбеддинги — только листов-кандидатов
        let leaves = leaves_of_nodes(db, nodes.clone(), false).await?;
        let mut need: Vec<String> = summaries
            .iter()
            .flat_map(|r| {
                leaves
                    .get(&r.target.id)
                    .into_iter()
                    .flatten()
                    .filter(|l| fits(r, &l.heading_path) && !l.embed_key.is_empty())
                    .map(|l| l.embed_key.clone())
            })
            .collect();
        need.sort();
        need.dedup();
        let embs = get_embeddings(db, model, &need).await?;
        for r in &summaries {
            let best = leaves
                .get(&r.target.id)
                .into_iter()
                .flatten()
                .filter(|l| fits(r, &l.heading_path))
                .filter_map(|l| {
                    let e = embs.get(&l.embed_key)?;
                    Some((dot(q, &e.dense), l))
                })
                .max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, l)) = best {
                out.insert(
                    r.chunk_id.clone(),
                    LeafRef {
                        chunk_id: l.chunk_id.clone(),
                        heading_path: l.heading_path.clone(),
                    },
                );
            }
        }
    }
    // без вектора или без эмбеддингов в кэше — по словам запроса
    if summaries.iter().any(|r| !out.contains_key(&r.chunk_id)) {
        let hits = leaf_term_hits(db, nodes, needles.to_vec()).await?;
        for r in &summaries {
            if out.contains_key(&r.chunk_id) {
                continue;
            }
            // по узлу и ordinal: при равном числе совпадений остаётся первый лист
            let mut best: Option<&LeafHit> = None;
            for h in hits
                .iter()
                .filter(|h| h.node_id == r.target.id && fits(r, &h.heading_path))
            {
                if best.is_none_or(|b| h.hits > b.hits) {
                    best = Some(h);
                }
            }
            if let Some(h) = best {
                out.insert(
                    r.chunk_id.clone(),
                    LeafRef {
                        chunk_id: h.chunk_id.clone(),
                        heading_path: h.heading_path.clone(),
                    },
                );
            }
        }
    }
    Ok(out)
}

/// Элемент выдачи до упаковки.
struct Item {
    /// Индекс цели (базы) элемента.
    target: usize,
    hit: FtsHit,
    score: f64,
    sources: Vec<&'static str>,
    chunk_ids: Vec<String>,
    l0: Option<String>,
    /// Текст для `chunks` (после `expand`) и его размер в токенах.
    text: Option<(String, usize)>,
    /// Балл реранкера (0..1), если элемент переставлен реранком.
    rerank_score: Option<f32>,
}

/// Текст пары реранкера: заголовок секции и текст чанка.
fn rerank_text(h: &FtsHit) -> String {
    if h.heading_path.is_empty() {
        h.text.clone()
    } else {
        format!("{}\n{}", h.heading_path, h.text)
    }
}

/// Не больше `max` элементов с одной цели (узла), порядок сохраняется.
fn cap_items(items: Vec<Item>, max: usize) -> Vec<Item> {
    let mut seen: HashMap<TargetRef, usize> = HashMap::new();
    items
        .into_iter()
        .filter(|it| {
            let n = seen.entry(it.hit.target).or_insert(0);
            *n += 1;
            *n <= max
        })
        .collect()
}

/// `rerank=on`: первые `ml.rerank_top` элементов — по баллу реранкера (см. модуль).
/// Сбой — элементы как были и предупреждение.
async fn rerank_items(
    st: &AppState,
    query: &str,
    items: Vec<Item>,
) -> (Vec<Item>, Option<&'static str>) {
    let device = st.index.device.as_ref().map(|d| d.device);
    let r = match st.rerank.get(device).await {
        Ok(r) => r,
        Err(w) => return (items, Some(w)),
    };
    let n = st.cfg.ml.rerank_top.min(items.len());
    let texts: Vec<String> = items[..n].iter().map(|it| rerank_text(&it.hit)).collect();
    match rerank::score(r, query.to_string(), texts, st.cfg.ml.rerank_tokens).await {
        Ok(scores) => {
            let out: Vec<Item> = rerank::reorder(items, &scores)
                .into_iter()
                .map(|(mut it, s)| {
                    it.rerank_score = s;
                    it
                })
                .collect();
            // лимит на узел — и после перестановки
            (cap_items(out, st.cfg.search.max_per_node), None)
        }
        Err(w) => (items, Some(w)),
    }
}

/// `notes`: по одному элементу на узел (лучший чанк), не больше `top_k`.
fn group_by_node(items: Vec<Item>, top_k: usize) -> Vec<Item> {
    let mut order: Vec<Item> = Vec::new();
    let mut pos: HashMap<TargetRef, usize> = HashMap::new();
    for it in items {
        match pos.get(&it.hit.target) {
            Some(&i) => order[i].chunk_ids.push(it.hit.chunk_id),
            None => {
                pos.insert(it.hit.target, order.len());
                order.push(it);
            }
        }
    }
    order.truncate(top_k);
    order
}

/// Собственный текст секции `heading_path` (заголовок и текст до первой подсекции);
/// пустой путь — преамбула. Если секций с таким путём несколько, берётся та, где лежит
/// лист `leaf`: секции, содержащие самую длинную (наиболее специфичную) строку листа, а
/// среди них — с наибольшей долей строк листа. Короткие однотипные строки (```` ``` ````,
/// `---`, `</details>`) так секцию не решают. Ни одна не подошла — первая. `None` —
/// секции нет.
fn own_section<'a>(src: &'a str, heading_path: &str, leaf: &str) -> Option<&'a str> {
    let doc = zam_markdown::parse(src);
    let own = |i: usize| {
        let s = &doc.sections[i];
        let end = doc
            .sections
            .get(i + 1)
            .map_or(s.range.end, |n| n.range.start.min(s.range.end))
            .max(s.body.start.min(s.range.end));
        src.get(s.range.start..end).map(str::trim_end)
    };
    let found: Vec<usize> = (0..doc.sections.len())
        .filter(|&i| {
            let s = &doc.sections[i];
            if heading_path.is_empty() {
                s.level == 0
            } else {
                s.level > 0 && s.path.join(" > ") == heading_path
            }
        })
        .collect();
    let lines: Vec<&str> = leaf
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // первая из самых длинных строк
    let longest = lines.iter().copied().fold(None::<&str>, |b, l| match b {
        Some(b) if b.chars().count() >= l.chars().count() => Some(b),
        _ => Some(l),
    });
    let by_leaf = longest.and_then(|key| {
        let mut best: Option<(usize, usize)> = None;
        for &i in &found {
            let Some(t) = own(i) else { continue };
            if !t.contains(key) {
                continue;
            }
            let n = lines.iter().filter(|l| t.contains(*l)).count();
            if best.is_none_or(|(_, m)| n > m) {
                best = Some((i, n));
            }
        }
        best.map(|(i, _)| i)
    });
    own(by_leaf.or(found.first().copied())?)
}

/// `mode=chunks`: текст элементов (с `expand=section|neighbors` — расширенный), не
/// больше `top_k` элементов. Повторная секция того же узла сливается в первый элемент
/// (`chunk_ids`), а выдача добирается следующими кандидатами окна. Текст секции и соседи
/// читаются из базы цели элемента.
async fn expand_items(
    targets: &[SearchTarget],
    items: Vec<Item>,
    how: Expand,
    top_k: usize,
) -> Result<Vec<Item>, ApiError> {
    let mut out: Vec<Item> = Vec::with_capacity(top_k);
    let mut sections: HashMap<(TargetRef, String), usize> = HashMap::new();
    let mut contents: HashMap<(Id, i64), Arc<String>> = HashMap::new();
    for mut it in items {
        if out.len() >= top_k {
            break;
        }
        let db = &targets[it.target].db;
        let base = it.hit.text.clone();
        // секции и соседние листы — только у узлов
        let how = if it.hit.target.kind == TargetKind::Node {
            how
        } else {
            Expand::None
        };
        let text = match how {
            Expand::Section => {
                let key = (it.hit.target, it.hit.heading_path.clone());
                if let Some(&i) = sections.get(&key) {
                    let ids = std::mem::take(&mut it.chunk_ids);
                    out[i].chunk_ids.extend(ids);
                    continue;
                }
                sections.insert(key, out.len());
                let ck = (it.hit.target.id, it.hit.rev);
                let src = match contents.get(&ck) {
                    Some(s) => s.clone(),
                    None => {
                        let (_, s) = node_content(db, it.hit.target.id, Some(it.hit.rev)).await?;
                        let s = Arc::new(s);
                        contents.insert(ck, s.clone());
                        s
                    }
                };
                // склеенные чанкером короткие секции: лист может быть длиннее секции
                match own_section(&src, &it.hit.heading_path, &base) {
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
    /// Размер в токенах: отданного текста (`chunks`, после `expand` и усечения), иначе —
    /// полного текста чанка (сколько стоит его прочитать).
    tokens: usize,
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
            let text = it.text.take().map(|(t, n_full)| match fit {
                Fit::Whole => (t, false, n_full),
                Fit::Truncated(n) => {
                    let t = truncate_to_tokens(&t, n, &count_tokens).to_string();
                    let n = count_tokens(&t);
                    (t, true, n)
                }
            });
            let tokens = text
                .as_ref()
                .map_or(it.hit.token_count.max(0) as usize, |t| t.2);
            Packed {
                item: it,
                text: text.map(|(t, tr, _)| (t, tr)),
                tokens,
            }
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
    let id_key = match h.target.kind {
        TargetKind::Node => "node_id",
        TargetKind::Entry => "entry_id",
    };
    o.insert(id_key.into(), json!(h.target.id));
    let space = spaces.get(&h.space_id);
    o.insert("space".into(), json!(space));
    if let Some(g) = space.and_then(|s| s.solution_guid.as_ref()) {
        o.insert("agent_solution_id".into(), json!(g));
    }
    o.insert("path".into(), json!(h.path));
    o.insert("heading_path".into(), json!(h.heading_path));
    o.insert("level".into(), json!(h.level));
    o.insert("rev".into(), json!(h.rev));
    o.insert("chunk_ids".into(), json!(p.item.chunk_ids));
    o.insert("score".into(), json!(round_score(p.item.score)));
    if let Some(r) = p.item.rerank_score {
        o.insert("rerank_score".into(), json!(round_score(f64::from(r))));
    }
    o.insert("sources".into(), json!(p.item.sources));
    o.insert("tokens".into(), json!(p.tokens));
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
    let mut line = format!("{n}. {} {space}/{}", h.target, h.path);
    if !h.heading_path.is_empty() {
        line.push_str(" › ");
        line.push_str(&h.heading_path);
    }
    line.push_str(&format!(
        "  [{:.2}] rev{} · {} · ~{} tok\n",
        p.item.rerank_score.map_or(p.item.score, f64::from),
        h.rev,
        h.updated_at.strftime("%Y-%m-%d"),
        p.tokens
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

/// Результат чтения базы цели: сбой в базе чужого решения исключает решение (WARN в журнал,
/// `solution unavailable`) и даёт `None`; сбой личной базы — ошибка запроса.
fn soft<T>(
    t: &SearchTarget,
    r: Result<T, StoreError>,
    warnings: &mut Vec<String>,
) -> Result<Option<T>, ApiError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) => match &t.solution {
            Some(info) => {
                skip_foreign(info, &e);
                warnings.push(WARN_SOLUTION_UNAVAILABLE.into());
                Ok(None)
            }
            None => Err(e.into()),
        },
    }
}

/// `POST /search`.
pub async fn search(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let total_t = Instant::now();
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
    let (targets, mut warnings) = resolve_targets(&st, &p, &headers, &req.scope).await?;
    let prefix = req
        .scope
        .path_prefix
        .as_deref()
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    // Human в Qdrant нет никогда: векторный запрос — по областям целей без него; цель без
    // других пространств в фильтр не входит (область без пространств ничего не пропускает)
    let scopes: Vec<VectorScope> = targets
        .iter()
        .filter_map(|t| {
            let space_ids = t.vector_space_ids();
            (!space_ids.is_empty()).then_some(VectorScope {
                user_id: t.owner,
                space_ids,
            })
        })
        .collect();
    let available = st.index.vector_available();
    let run_vector = available.is_ok() && !scopes.is_empty() && !req.query.trim().is_empty();

    // 1. каналы FTS (по базе на цель, параллельно) и векторный (один запрос) — параллельно
    let fts = async {
        let t = Instant::now();
        let runs = join_all(targets.iter().map(|tg| {
            search_fts_channels(
                &tg.db,
                FtsQuery {
                    text: req.query.clone(),
                    space_ids: tg.space_ids(),
                    path_prefix: prefix.clone(),
                    top_k: TOP_K_MAX,
                },
            )
        }))
        .await;
        (runs, ms(t))
    };
    let vector = async {
        if !run_vector {
            return VectorRun::default();
        }
        let filter = VectorFilter {
            scopes: scopes.clone(),
            path_prefix: prefix.clone(),
            ..VectorFilter::default()
        };
        vector_channel(&st.index, filter, req.query.clone()).await
    };
    let ((runs, fts_ms), vrun) = tokio::join!(fts, vector);
    // цель, чья база не прочиталась (чужое решение), исключается из выдачи целиком
    let mut alive = vec![true; targets.len()];
    let mut per_db = Vec::with_capacity(targets.len());
    for (i, r) in runs.into_iter().enumerate() {
        match soft(&targets[i], r, &mut warnings)? {
            Some(ch) => per_db.push((i, ch)),
            None => alive[i] = false,
        }
    }
    let fts = merge_channels(per_db, FTS_CHANNEL_TOP);

    // 2. векторные хиты — в базу своего владельца, только живые чанки пространств цели
    let fuse_t = Instant::now();
    let mut hits: HashMap<String, (usize, FtsHit)> = HashMap::new();
    for (t, h) in fts.bm25.iter().chain(&fts.trigram) {
        hits.entry(h.chunk_id.clone())
            .or_insert_with(|| (*t, h.clone()));
    }
    let vector_ids: Option<Vec<String>> = match &vrun.hits {
        Some(vh) => {
            let mut live: HashMap<String, (usize, FtsHit)> = HashMap::new();
            for (i, t) in targets.iter().enumerate() {
                if !alive[i] {
                    continue;
                }
                let ids: Vec<String> = vh
                    .iter()
                    .filter(|h| h.user_id == t.owner)
                    .map(|h| h.chunk_id.clone())
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                let scope = ChunkScope {
                    space_ids: t.vector_space_ids(),
                    path_prefix: prefix.clone(),
                    indexable_only: true,
                };
                match soft(t, live_chunks(&t.db, ids, scope).await, &mut warnings)? {
                    Some(found) => {
                        for h in found {
                            live.entry(h.chunk_id.clone()).or_insert((i, h));
                        }
                    }
                    None => alive[i] = false,
                }
            }
            // порядок векторного канала; хит засчитывается только из базы своего владельца
            let ids: Vec<String> = vh
                .iter()
                .filter(|h| {
                    live.get(&h.chunk_id)
                        .is_some_and(|(i, _)| targets[*i].owner == h.user_id)
                })
                .map(|h| h.chunk_id.clone())
                .collect();
            for (id, v) in live {
                hits.entry(id).or_insert(v);
            }
            Some(ids)
        }
        None => None,
    };
    // база, не прочитавшаяся при проверке векторных хитов, — вне выдачи целиком (кандидаты
    // без метаданных `fuse` отбрасывает)
    hits.retain(|_, (i, _)| alive[*i]);

    // 3. слияние, сводки → листы
    let chunks: HashMap<String, ChunkMeta> = hits
        .iter()
        .filter_map(|(id, (_, h))| {
            Some((
                id.clone(),
                ChunkMeta {
                    target: h.target,
                    level: Level::parse(&h.level)?,
                    heading_path: h.heading_path.clone(),
                    updated_at: h.updated_at,
                },
            ))
        })
        .collect();
    // чанки, которых векторный канал не видит (Human, узел не проиндексирован): ослабленный
    // bm25 для них не понижается — иначе такая находка проигрывает всем векторным кандидатам
    let human: HashSet<Id> = targets
        .iter()
        .flat_map(|t| &t.spaces)
        .filter(|s| s.kind == SpaceKind::Human)
        .map(|s| s.id)
        .collect();
    let vector_blind = hits
        .values()
        .filter(|(_, h)| human.contains(&h.space_id) || h.index_state != "indexed")
        .map(|(_, h)| h.chunk_id.clone())
        .collect();
    let inputs = SearchInputs {
        fts: fts.bm25.iter().map(|(_, h)| h.chunk_id.clone()).collect(),
        trigram: fts
            .trigram
            .iter()
            .map(|(_, h)| h.chunk_id.clone())
            .collect(),
        vector: vector_ids,
        chunks,
        // ослабленный bm25 (ни один чанк не содержит всех слов запроса) — свой вес
        // `w_fts_relaxed`: его случайные совпадения по одному слову вытесняли векторные
        // находки (eval); действует только при векторном канале
        fts_relaxed: fts.relaxed,
        vector_blind,
    };
    let weights = st.cfg.search.weights();
    let ranked = fuse(&inputs, &weights, jiff::Timestamp::now());
    let window = top_k * OVERSAMPLE;
    let query_vec = vrun
        .dense
        .as_deref()
        .zip(st.index.model_id())
        .filter(|_| vrun.hits.is_some());
    let needles = query_needles(&req.query);
    // сводки за окном без своего листа — прочь (лист им не выбирается); листы ищутся
    // для сводок окна, а наличие листа проверяется по всему списку
    let ranked = drop_unresolved_tail(ranked, window);
    let mut best: HashMap<String, LeafRef> = HashMap::new();
    {
        // листы сводок — в базе цели сводки
        let mut by_target: HashMap<usize, Vec<&Ranked>> = HashMap::new();
        for r in summaries_without_leaf(&ranked, window) {
            if let Some((i, _)) = hits.get(&r.chunk_id) {
                by_target.entry(*i).or_default().push(r);
            }
        }
        for (i, group) in by_target {
            let t = &targets[i];
            let found = best_leaves(&t.db, &group, &needles, query_vec).await?;
            // листы-замены — живые чанки области цели; не нашёлся (узел перезаписан между
            // чтениями) — сводка остаётся как есть
            let missing: Vec<String> = found
                .values()
                .filter(|l| !hits.contains_key(&l.chunk_id))
                .map(|l| l.chunk_id.clone())
                .collect();
            if !missing.is_empty() {
                let scope = ChunkScope {
                    space_ids: t.space_ids(),
                    path_prefix: prefix.clone(),
                    indexable_only: false,
                };
                for h in live_chunks(&t.db, missing, scope).await? {
                    hits.insert(h.chunk_id.clone(), (i, h));
                }
            }
            best.extend(found);
        }
    }
    best.retain(|summary, l| {
        let ok = hits.contains_key(&l.chunk_id);
        if !ok {
            tracing::debug!(summary, leaf = %l.chunk_id, "лист-замена сводки не найден: сводка остаётся");
        }
        ok
    });
    let ranked = note_to_leaf(ranked, |r| best.get(&r.chunk_id).cloned());
    // лимит на цель — после замены сводок листами
    let mut ranked = cap_per_node(ranked, weights.max_per_node);
    let rerank_on = req.rerank == Some(RerankMode::On);
    // реранку — не меньше `rerank_top` кандидатов, даже при малом top_k
    ranked.truncate(if rerank_on {
        window.max(st.cfg.ml.rerank_top)
    } else {
        window
    });
    let needles: Vec<&str> = needles.iter().map(String::as_str).collect();
    let items: Vec<Item> = ranked
        .into_iter()
        .filter_map(|r| {
            let (target, hit) = hits.get(&r.chunk_id)?;
            let mut hit = hit.clone();
            if hit.snippet.is_empty() {
                hit.snippet = text_snippet(&hit.text, &needles);
            }
            Some(Item {
                target: *target,
                chunk_ids: vec![hit.chunk_id.clone()],
                hit,
                score: r.score,
                sources: r.sources,
                l0: None,
                text: None,
                rerank_score: None,
            })
        })
        .collect();
    let fuse_ms = ms(fuse_t);

    // 4. серверный реранк; пересборка индекса — предупреждение о неполной векторной выдаче
    if st.index.reindexing() {
        warnings.push(crate::index_state::WARN_REINDEXING.into());
    }
    let mut rerank_ms = None;
    let items = if rerank_on && !items.is_empty() {
        let t = Instant::now();
        let (items, warning) = rerank_items(&st, &req.query, items).await;
        rerank_ms = Some(ms(t));
        warnings.extend(warning.map(String::from));
        items
    } else {
        items
    };

    // 5. top_k, режим, expand, упаковка
    let expand_t = Instant::now();
    let mut items = match mode {
        SearchMode::Notes => group_by_node(items, top_k),
        SearchMode::Chunks => expand_items(&targets, items, expand, top_k).await?,
        SearchMode::Brief => items.into_iter().take(top_k).collect(),
    };
    if mode == SearchMode::Notes {
        for it in &mut items {
            if it.hit.target.kind == TargetKind::Node {
                it.l0 = get_node(&targets[it.target].db, it.hit.target.id).await?.l0;
            }
        }
    }
    let expand_ms = ms(expand_t);
    // заметки области всех целей, ещё не прошедшие векторную индексацию (без `index: false`
    // и Human)
    let pending_t = Instant::now();
    let mut pending = 0;
    for (i, t) in targets.iter().enumerate() {
        if !alive[i] {
            continue;
        }
        let r = pending_count(&t.db, t.space_ids(), prefix.clone()).await;
        pending += soft(t, r, &mut warnings)?.unwrap_or(0);
    }
    let pending_ms = ms(pending_t);
    dedup_keep_order(&mut warnings);
    let packed = pack_items(items, mode, budget);
    let degraded = available.err().or(vrun.failed);
    let by_id: HashMap<Id, Space> = targets
        .into_iter()
        .flat_map(|t| t.spaces)
        .map(|s| (s.id, s))
        .collect();

    Ok(match format {
        OutFormat::Json => {
            let items: Vec<Value> = packed.iter().map(|p| item_json(p, mode, &by_id)).collect();
            Json(json!({
                "items": items,
                "degraded": degraded.is_some(),
                "degraded_reason": degraded.map(DegradedReason::as_str),
                "degraded_text": degraded.map(DegradedReason::text),
                "warnings": warnings,
                "pending_count": pending,
                "timings": {
                    "fts_ms": fts_ms,
                    "embed_ms": vrun.embed_ms,
                    "vector_ms": vrun.vector_ms,
                    "fuse_ms": fuse_ms,
                    "rerank_ms": rerank_ms,
                    "expand_ms": expand_ms,
                    "pending_ms": pending_ms,
                    // от разбора запроса до ответа (без сериализации JSON)
                    "total_ms": ms(total_t),
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
            if let Some(r) = degraded {
                body.push_str(&format!(
                    "> degraded: {} — {} (только FTS)\n",
                    r.as_str(),
                    r.text()
                ));
            }
            for w in &warnings {
                body.push_str(&format!("> warning: {w}\n"));
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
            target: TargetRef {
                kind: TargetKind::Node,
                id: node,
            },
            space_id: Id::new(),
            path: "p".into(),
            heading_path: String::new(),
            level: "chunk".into(),
            rev: 1,
            token_count: count_tokens(text) as i64,
            score: 1.0,
            raw_score: 0.0,
            updated_at: jiff::Timestamp::now(),
            snippet: format!("{MARK_START}s{MARK_END}"),
            text: text.into(),
            index_state: "indexed".into(),
        }
    }

    fn items(hits: Vec<FtsHit>, with_text: bool) -> Vec<Item> {
        hits.into_iter()
            .map(|h| Item {
                target: 0,
                chunk_ids: vec![h.chunk_id.clone()],
                text: with_text.then(|| (h.text.clone(), h.token_count as usize)),
                hit: h,
                score: 0.5,
                sources: vec!["fts"],
                l0: None,
                rerank_score: None,
            })
            .collect()
    }

    #[test]
    fn rerank_text_and_cap_items() {
        let a = Id::new();
        let mut h = hit(a, "текст");
        assert_eq!(rerank_text(&h), "текст");
        h.heading_path = "A > B".into();
        assert_eq!(rerank_text(&h), "A > B\nтекст");
        let b = Id::new();
        let its = items(
            vec![hit(a, "1"), hit(a, "22"), hit(b, "333"), hit(a, "4444")],
            false,
        );
        let capped = cap_items(its, 2);
        let ids: Vec<&str> = capped.iter().map(|i| i.hit.chunk_id.as_str()).collect();
        assert_eq!(ids, ["c1", "c2", "c3"]);
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
        fn delete_scope<'a>(&'a self, _: &'a VectorFilter) -> zam_index::IndexFuture<'a, ()> {
            unreachable!()
        }
        fn delete_targets<'a>(
            &'a self,
            _: Id,
            _: &'a [zam_core::TargetRef],
        ) -> zam_index::IndexFuture<'a, ()> {
            unreachable!()
        }
        fn nearest_dense<'a>(
            &'a self,
            _: &'a [f32],
            _: &'a VectorFilter,
            _: usize,
        ) -> zam_index::IndexFuture<'a, Vec<VectorHit>> {
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
        VectorFilter::scope(Id::new(), vec![Id::new()])
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
        // время до сбоя, а не null
        assert!(r.hits.is_none() && r.embed_ms.is_some() && r.vector_ms.is_none());
        let r = run_vector_channel(|| panic!("инференс упал"), &idx, filter()).await;
        assert_eq!(r.failed, Some(DegradedReason::EmbedFailed));
        assert!(r.hits.is_none() && r.embed_ms.is_some());
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
        assert_eq!(own_section(src, "A", ""), Some("# A\n\nтекст A"));
        assert_eq!(own_section(src, "A > B", ""), Some("## B\n\nтекст B"));
        assert_eq!(own_section(src, "", ""), Some("Преамбула."));
        assert_eq!(own_section(src, "C", ""), Some("# C\n\nтекст C"));
        assert_eq!(own_section(src, "Q", ""), None);
        // повторяющийся путь: секция — та, где лежит лист
        let dup = "# A\n\nпервая\n\n# A\n\nвторая строка\n";
        assert_eq!(
            own_section(dup, "A", "# A\nвторая строка"),
            Some("# A\n\nвторая строка")
        );
        assert_eq!(own_section(dup, "A", "первая"), Some("# A\n\nпервая"));
        assert_eq!(own_section(dup, "A", "нет такого"), Some("# A\n\nпервая"));
        // однотипная концовка (```) не решает: по самой длинной строке листа
        let code = "# A\n\n```sh\necho первый\n```\n\n# A\n\n```sh\necho второй вариант\n```\n";
        assert_eq!(
            own_section(code, "A", "```sh\necho второй вариант\n```"),
            Some("# A\n\n```sh\necho второй вариант\n```")
        );
        assert_eq!(
            own_section(code, "A", "```sh\necho первый\n```"),
            Some("# A\n\n```sh\necho первый\n```")
        );
        // самая длинная строка есть в обеих — решает доля совпавших строк
        let tie = "# A\n\nобщая длинная строка\nx1\n\n# A\n\nобщая длинная строка\ny2\n";
        assert_eq!(
            own_section(tie, "A", "общая длинная строка\ny2"),
            Some("# A\n\nобщая длинная строка\ny2")
        );
    }

    #[test]
    fn md_card_format() {
        let mut it = items(vec![hit(Id::new(), "текст")], false).remove(0);
        it.hit.heading_path = "A > B".into();
        it.hit.updated_at = "2026-10-10T08:00:00Z".parse().unwrap();
        let p = Packed {
            item: it,
            text: None,
            tokens: 7,
        };
        let s = item_md(1, &p, SearchMode::Brief, &HashMap::new());
        let want = format!(
            "1. n:{} ?/p › A > B  [0.50] rev1 · 2026-10-10 · ~7 tok\n   **s**\n",
            p.item.hit.target.id
        );
        assert_eq!(s, want);
    }
}
