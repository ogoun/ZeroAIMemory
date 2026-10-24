//! `POST /search` (§9.1–9.3) — гибридный поиск: Qdrant (dense + sparse + сводки) и FTS5
//! (`bm25` + `trigram`), слияние weighted RRF в `zam-search` с весами секции `[search]`.
//!
//! **Тело** (остальные поля §9.1 принимаются и пока игнорируются):
//! - `query` — до 512 символов, синтаксис FTS5 в нём не действует;
//! - `scope`: `solution`, `include_knowledge` (true), `include_rules` (true),
//!   `all_solutions` (false), `path_prefix`, `spaces: [SpaceRef]`, `include_human`,
//!   `area` — строгий фильтр области (только узлы и записи с этой `area`: FTS, Qdrant и
//!   проверка векторных хитов; до 128 печатных символов, пустая — без фильтра);
//! - `mode`: `brief` (по умолчанию) | `chunks` | `notes`;
//! - `expand`: `none` (по умолчанию) | `section` (текст всей секции чанка) | `neighbors`
//!   (чанк с соседними листами) — в `mode=chunks`; `graph` — в любом режиме: соседи первых
//!   `top_k` элементов по графу (§8.5, [`graph`]: до `ceil(top_k / 2)` новых элементов с
//!   баллом `родитель × search.graph_weight`, `sources: ["graph"]`, `via: {type, from}`);
//! - `top_k` (10, 1..=100), `budget_tokens` (800), `format`: `json` (по умолчанию) | `md`;
//! - `rerank`: `off` (по умолчанию) | `on` — серверный реранк (см. ниже);
//! - `filters.kinds` — виды целей: `note`, `insight`, `changelog`, `release` (по умолчанию все
//!   четыре), `folder` — карточки папок (см. «Папки»), `attachment` принимается, но ничего не
//!   находит (вложения — M5); `filters.status` — статусы insight (по умолчанию `active`,
//!   `revisit`); `include_history: true` снимает фильтр статуса. Фильтр действует и в Qdrant
//!   (`kinds`, `must_not` по исключённым статусам), и в FTS, и в проверке векторных хитов
//!   ([`live_chunks`]); у changelog и release статуса нет — они проходят всегда.
//!   `filters.certainty` — эпистемические статусы (`axiom`, `theorem`, `hypothesis`, §7.5)
//!   заметок и записей; пусто — любые, иначе цели без статуса не проходят (Qdrant, FTS и
//!   проверка хитов). Бусты `search.boost_axiom`/`boost_theorem`/`boost_hypothesis`
//!   (1,20 / 1,10 / 1,00) — множители слитого балла по `certainty`.
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
//!    проиндексирован) → бусты ([`zam_search::apply_boosts`], §9.2 п. 4: insight `gotcha` и
//!    `failure` — `search.boost_gotcha`/`boost_failure`, узлы и записи области из
//!    `X-Zam-Area` — `search.boost_area`; балл в ответе ограничен 1) → `note_to_leaf` →
//!    ≤ `max_per_node` с узла:
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
//! 5. `top_k` (в `notes` — узлов), `expand` (`graph` — соседи после `top_k`), упаковка в
//!    `budget_tokens`.
//!
//! **Папки** (§8.3, §9.2). Векторный запрос добавляет prefetch dense top-10 по точкам папок
//! (`VectorFilter::folders`), если включён канал папок (`search.w_folder > 0`, по умолчанию
//! 0 — до сетки задачи 18) или запрошены карточки (`filters.kinds ∋ folder`). Точки папок
//! направляются в базу владельца и проверяются по SQLite ([`live_folders`]: папка жива, не в
//! корзине, не Human, в пространствах цели и под `path_prefix`); удалённая папка, точка
//! которой осталась в Qdrant, не действует. Ранг папки — среди живых, по порядку векторной
//! выдачи. Канал `folder`: чанку узла под путём папки (того же пространства) прибавляется
//! `w_folder / (rrf_k + ранг)` по лучшей папке-предку, один раз (`sources` получает
//! `folder`). Карточки — только при `filters.kinds ∋ folder`, не больше `search.max_folders`
//! (2) лучших папок, сверх `top_k` **[решение исполнителя]**: балл — как у кандидата только
//! векторного канала того же места ([`zam_search::folder_score`]), карточка встаёт перед
//! первым элементом с меньшим баллом (соседей `expand=graph` она не обходит), не
//! реранжируется и не расширяется. JSON карточки: `node_id`, `kind: "folder"`, `level:
//! "folder"`, `space`, `path`, `score`, `sources: ["folder"]`, `child_count`, `l0` (в `notes`)
//! или `snippet` (L0), `chunk_ids: []`; md — `<n>. f:<id> <пространство>/<путь>/  [score] ·
//! <N> детей` и L0 строкой ниже.
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
//! полного текста чанка, `updated_at`, `kind` (`note` или вид записи), `snippet` | `l0`,
//! `text`, `rerank_score` 0..1 — у переставленных реранком, `via {type, from}` — у соседей
//! `expand=graph`), `degraded`, `degraded_reason`
//! и `degraded_text` (или `null`), `warnings` (строки: о реранке, `index rebuilding` — индекс
//! пересобирается, векторная выдача может быть неполной, `solution access …` и
//! `solution unavailable` — решение исключено из области), `pending_count` — живые
//! заметки области всех целей, ждущие векторной индексации, `timings {fts_ms, embed_ms, vector_ms,
//! fuse_ms, rerank_ms, expand_ms, pending_ms, total_ms}` (`null` — этап не выполнялся;
//! `total_ms` — весь обработчик без сериализации ответа). Маркеры совпадений в JSON —
//! U+E000/U+E001.
//!
//! Элемент заметки несёт `certainty` её текущей ревизии (`null` — без статуса).
//! Элемент записи журнала вместо `node_id` несёт `entry_id` и поля записи: `kind`,
//! `status`, `insight_kind` (`data.kind`), `certainty`, `title`; путь у записи пуст,
//! `expand` и L0 к записям не применяются.
//!
//! **Формат `md`** — карточки §9.3, по одной на элемент:
//! `<n>. n:<id> <пространство>/<путь> › <секция>  [score] rev<N>[ · <certainty>] · <дата> · ~<N> tok`;
//! у записи — `<n>. i:<id> insight/<вид> «<заголовок>»  [score] <status> · <certainty> · ~<N>
//! tok` (`c:` — changelog, `r:` — release; отсутствующие части опускаются; у соседа
//! `expand=graph` — ` · via <тип> ← <родитель>`),
//! под ней с отступом — сниппет (`brief`, маркеры → `**…**`), L0 (`notes`) или текст
//! (`chunks`); балл — реранка, если элемент переставлен им. При деградации строка
//! `> degraded: <код> — <текст причины> (только FTS)`, в том числе при пустой выдаче (тогда она
//! единственная), предупреждения — строками `> warning: <текст>` в конце.
//!
//! **Ошибки баз владельцев.** Любой сбой чтения базы чужого решения — FTS, проверка векторных
//! хитов, листы сводок, `expand`, L0, `pending_count` — исключает решение целиком ([`Health`]):
//! WARN в журнал, `warnings: ["solution unavailable"]`, элементы этой цели убираются из
//! выдачи, ответ строится по остальным целям. Сбой личной базы — ошибка запроса, как в M2.
pub mod graph;
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
    Boosts, ChunkMeta, Cost, Fit, FolderHit, LeafRef, Level, Ranked, SearchInputs, Weights,
    apply_boosts, cap_per_node, drop_unresolved_tail, folder_score, fuse_normed, in_section,
    note_to_leaf, pack, summaries_without_leaf, truncate_to_tokens,
};
use zam_store::time::fmt_ts;
use zam_store::{
    ChunkScope, Db, FtsHit, FtsQuery, LeafHit, MARK_END, MARK_START, Space, SpaceKind,
    TargetFilter, count_tokens, get_embeddings, get_node, leaf_term_hits, leaves_of_nodes,
    live_chunks, live_folders, neighbor_leaves, node_content, pending_count, query_needles,
    search_fts_channels, text_snippet,
};

use self::merge::merge_channels;
use self::targets::{
    ScopeReq, SearchTarget, WARN_SOLUTION_UNAVAILABLE, dedup_keep_order, resolve_targets,
};
use super::json_body;
use super::scope::skip_foreign;
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, header_value};
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
/// Кандидатов слитого канала `bm25` нескольких баз (как в одной базе, `zam_store`).
const FTS_BM25_TOP: usize = 50;
/// Кандидатов слитого канала `trigram` нескольких баз (как в одной базе, `zam_store`).
const FTS_TRIGRAM_TOP: usize = 20;
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

/// `filters` запроса (§9.1); остальные поля пока игнорируются.
#[derive(Deserialize, Default)]
struct Filters {
    kinds: Option<Vec<String>>,
    status: Option<Vec<String>>,
    /// `axiom | theorem | hypothesis` (§7.5); пусто — любые, в том числе без статуса.
    certainty: Option<Vec<String>>,
}

/// Допустимые виды целей `filters.kinds`.
const KINDS: [&str; 6] = [
    "note",
    "insight",
    "changelog",
    "release",
    "folder",
    "attachment",
];
/// Виды по умолчанию (§9.1).
const DEFAULT_KINDS: [&str; 4] = ["note", "insight", "changelog", "release"];
/// Статусы insight.
const STATUSES: [&str; 4] = ["active", "revisit", "superseded", "retracted"];
/// Статусы по умолчанию (§7.5): действующие.
const DEFAULT_STATUSES: [&str; 2] = ["active", "revisit"];
/// Эпистемические статусы `filters.certainty` (§7.5).
const CERTAINTIES: [&str; 3] = ["axiom", "theorem", "hypothesis"];

/// Список значений фильтра: пустой или не задан — `default`; неизвестное значение — 400.
fn filter_list(
    name: &str,
    v: Option<&Vec<String>>,
    allowed: &[&str],
    default: &[&str],
) -> Result<Vec<String>, ApiError> {
    let v: Vec<String> = match v.filter(|v| !v.is_empty()) {
        Some(v) => v.clone(),
        None => default.iter().map(|s| s.to_string()).collect(),
    };
    if let Some(bad) = v.iter().find(|s| !allowed.contains(&s.as_str())) {
        return Err(ApiError::BadRequest(format!(
            "{name}: неизвестное значение «{bad}»; допустимы {}",
            allowed.join(", ")
        )));
    }
    Ok(v)
}

/// Строгий фильтр области `scope.area`: без пробелов по краям, пустая — `None`; длиннее
/// [`zam_store::AREA_MAX_CHARS`] или с управляющими символами → 400.
fn area_filter(a: Option<&str>) -> Result<Option<String>, ApiError> {
    let a = zam_store::normalize_area(a);
    if a.as_ref().is_some_and(|a| {
        a.chars().count() > zam_store::AREA_MAX_CHARS || a.chars().any(char::is_control)
    }) {
        return Err(ApiError::BadRequest(format!(
            "scope.area: до {} печатных символов",
            zam_store::AREA_MAX_CHARS
        )));
    }
    Ok(a)
}

impl SearchSection {
    /// Бусты §9.2 п. 4: виды инсайтов, `certainty` (§7.5) и текущая область (`X-Zam-Area`,
    /// если задана).
    fn boosts(&self, area: Option<String>) -> Boosts {
        Boosts {
            insight_kinds: [
                ("gotcha".to_string(), self.boost_gotcha),
                ("failure".to_string(), self.boost_failure),
            ]
            .into_iter()
            .collect(),
            certainty: [
                ("axiom".to_string(), self.boost_axiom),
                ("theorem".to_string(), self.boost_theorem),
                ("hypothesis".to_string(), self.boost_hypothesis),
            ]
            .into_iter()
            .collect(),
            area: area.map(|a| (a, self.boost_area)),
        }
    }
}

/// Фильтр целей запроса: виды и статусы (`include_history` — любые статусы).
fn target_filter(f: &Filters, include_history: bool) -> Result<TargetFilter, ApiError> {
    let kinds = filter_list("filters.kinds", f.kinds.as_ref(), &KINDS, &DEFAULT_KINDS)?;
    let statuses = filter_list(
        "filters.status",
        f.status.as_ref(),
        &STATUSES,
        &DEFAULT_STATUSES,
    )?;
    let certainty = filter_list("filters.certainty", f.certainty.as_ref(), &CERTAINTIES, &[])?;
    Ok(TargetFilter {
        kinds,
        statuses: if include_history {
            Vec::new()
        } else {
            statuses
        },
        area: None,
        certainty,
    })
}

/// Статусы, которые векторный фильтр исключает (`must_not`): точки без статуса (узлы,
/// changelog, release, точки M2) проходят.
fn excluded_statuses(t: &TargetFilter) -> Vec<String> {
    if t.statuses.is_empty() {
        return Vec::new();
    }
    STATUSES
        .iter()
        .filter(|s| !t.statuses.iter().any(|x| x == *s))
        .map(|s| s.to_string())
        .collect()
}

#[derive(Deserialize)]
struct SearchReq {
    query: String,
    #[serde(default)]
    scope: ScopeReq,
    #[serde(default)]
    filters: Filters,
    #[serde(default)]
    include_history: bool,
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
            w_folder: self.w_folder,
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
    /// Сосед `expand=graph`: тип ребра и родитель.
    via: Option<(zam_store::EdgeType, TargetRef)>,
    /// Карточка папки: число живых детей (см. модуль, «Папки»).
    folder: Option<i64>,
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

/// Секция листа для `expand=section`: начало секции H1–H3 (или преамбулы) в байтах
/// исходника — ключ слияния — и её собственный текст (заголовок и текст до первой
/// подсекции).
///
/// С задачи 17 путь листа включает H4–H6 (`A > B > Глубокий`), а секции `Doc` — только
/// H1–H3, поэтому кандидаты — секции, чей путь является префиксом `heading_path` по
/// компонентам (` > `); преамбула — префикс любого пути. Секция определяется по месту
/// листа: среди кандидатов, содержащих самую длинную (наиболее специфичную) строку листа,
/// — с наибольшей долей строк листа, при равенстве — самая глубокая. Собственные тексты
/// секций не пересекаются, поэтому так находится секция, где лист действительно лежит, —
/// и при повторяющихся путях, и при « > » в тексте заголовка. Короткие однотипные строки
/// (```` ``` ````, `---`, `</details>`) секцию не решают. Лист не найден в тексте (например,
/// исходник другой ревизии) — запасной вариант: самый длинный префикс пути, первый из
/// равных. `None` — подходящей секции нет.
fn leaf_section<'a>(src: &'a str, heading_path: &str, leaf: &str) -> Option<(usize, &'a str)> {
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
    // (индекс, глубина) кандидатов
    let found: Vec<(usize, usize)> = (0..doc.sections.len())
        .filter_map(|i| {
            let s = &doc.sections[i];
            if s.level == 0 {
                return Some((i, 0));
            }
            let p = s.path.join(" > ");
            let prefix = heading_path == p
                || heading_path
                    .strip_prefix(p.as_str())
                    .is_some_and(|rest| rest.starts_with(" > "));
            prefix.then_some((i, s.path.len()))
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
        let mut best: Option<(usize, usize, usize)> = None;
        for &(i, depth) in &found {
            let Some(t) = own(i) else { continue };
            if !t.contains(key) {
                continue;
            }
            let n = lines.iter().filter(|l| t.contains(*l)).count();
            if best.is_none_or(|(_, m, d)| (n, depth) > (m, d)) {
                best = Some((i, n, depth));
            }
        }
        best.map(|(i, _, _)| i)
    });
    // запасной вариант: самый длинный префикс пути (преамбула — только для пустого пути)
    let by_path = || {
        let depth = if heading_path.is_empty() { 0 } else { 1 };
        found
            .iter()
            .filter(|&&(_, d)| d >= depth)
            .fold(None::<(usize, usize)>, |b, &(i, d)| match b {
                Some((_, bd)) if bd >= d => b,
                _ => Some((i, d)),
            })
            .map(|(i, _)| i)
    };
    let i = by_leaf.or_else(by_path)?;
    Some((doc.sections[i].range.start, own(i)?))
}

/// Ключ слияния секций в `expand=section`: секция H1–H3 листа (по её началу в исходнике),
/// если найдена, иначе — путь заголовков листа.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SectionKey {
    At(usize),
    Path(String),
}

/// `mode=chunks`: текст элементов (с `expand=section|neighbors` — расширенный), не
/// больше `top_k` элементов. Повторная секция того же узла сливается в первый элемент
/// (`chunk_ids`), а выдача добирается следующими кандидатами окна. Текст секции и соседи
/// читаются из базы цели элемента.
async fn expand_items(
    health: &mut Health<'_>,
    items: Vec<Item>,
    how: Expand,
    top_k: usize,
) -> Result<Vec<Item>, ApiError> {
    let targets = health.targets;
    let mut out: Vec<Item> = Vec::with_capacity(top_k);
    let mut sections: HashMap<(TargetRef, SectionKey), usize> = HashMap::new();
    let mut contents: HashMap<(Id, i64), Arc<String>> = HashMap::new();
    for mut it in items {
        if out.len() >= top_k {
            break;
        }
        if !health.alive[it.target] {
            continue;
        }
        let ti = it.target;
        let db = &targets[ti].db;
        let base = it.hit.text.clone();
        // секции и соседние листы — только у узлов
        let how = if it.hit.target.kind == TargetKind::Node {
            how
        } else {
            Expand::None
        };
        let text = match how {
            Expand::Section => {
                let ck = (it.hit.target.id, it.hit.rev);
                let src = match contents.get(&ck) {
                    Some(s) => s.clone(),
                    None => {
                        let r = node_content(db, it.hit.target.id, Some(it.hit.rev)).await;
                        let Some((_, s)) = health.soft(ti, r)? else {
                            drop_target(&mut out, &mut sections, ti);
                            continue;
                        };
                        let s = Arc::new(s);
                        contents.insert(ck, s.clone());
                        s
                    }
                };
                let sec = leaf_section(&src, &it.hit.heading_path, &base);
                let key = (
                    it.hit.target,
                    sec.map_or_else(
                        || SectionKey::Path(it.hit.heading_path.clone()),
                        |(at, _)| SectionKey::At(at),
                    ),
                );
                if let Some(&i) = sections.get(&key) {
                    let ids = std::mem::take(&mut it.chunk_ids);
                    out[i].chunk_ids.extend(ids);
                    continue;
                }
                sections.insert(key, out.len());
                // склеенные чанкером короткие секции: лист может быть длиннее секции
                match sec {
                    Some((_, sec)) if sec.chars().count() >= base.chars().count() => {
                        sec.to_string()
                    }
                    _ => base,
                }
            }
            Expand::Neighbors => {
                let r = neighbor_leaves(db, it.hit.chunk_id.clone()).await;
                let Some((prev, next)) = health.soft(ti, r)? else {
                    drop_target(&mut out, &mut sections, ti);
                    continue;
                };
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

/// Убирает из `out` элементы исключённой цели `t` и переиндексирует секции оставшихся.
fn drop_target(
    out: &mut Vec<Item>,
    sections: &mut HashMap<(TargetRef, SectionKey), usize>,
    t: usize,
) {
    let mut kept = 0;
    let pos: Vec<Option<usize>> = out
        .iter()
        .map(|it| {
            (it.target != t).then(|| {
                kept += 1;
                kept - 1
            })
        })
        .collect();
    out.retain(|it| it.target != t);
    sections.retain(|_, i| match pos.get(*i).copied().flatten() {
        Some(j) => {
            *i = j;
            true
        }
        None => false,
    });
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

/// Балл ответа: 0..1 с точностью 1e-4 (после бустов слитый балл может превысить 1 — порядок
/// при этом сохраняется, шкала ответа — нет).
fn round_score(s: f64) -> f64 {
    (s.min(1.0) * 10_000.0).round() / 10_000.0
}

/// JSON карточки папки (см. модуль, «Папки»).
fn folder_json(p: &Packed, children: i64, mode: SearchMode, spaces: &HashMap<Id, Space>) -> Value {
    let h = &p.item.hit;
    let space = spaces.get(&h.space_id);
    let mut o = Map::new();
    o.insert("node_id".into(), json!(h.target.id));
    o.insert("space".into(), json!(space));
    if let Some(g) = space.and_then(|s| s.solution_guid.as_ref()) {
        o.insert("agent_solution_id".into(), json!(g));
    }
    o.insert("path".into(), json!(h.path));
    o.insert("heading_path".into(), json!(""));
    o.insert("level".into(), json!(h.level));
    o.insert("kind".into(), json!("folder"));
    o.insert("chunk_ids".into(), json!(p.item.chunk_ids));
    o.insert("score".into(), json!(round_score(p.item.score)));
    o.insert("sources".into(), json!(p.item.sources));
    o.insert("tokens".into(), json!(p.tokens));
    o.insert("child_count".into(), json!(children));
    if mode == SearchMode::Notes {
        o.insert("l0".into(), json!(p.item.l0));
    } else {
        o.insert("snippet".into(), json!(h.snippet));
    }
    Value::Object(o)
}

fn item_json(p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> Value {
    if let Some(children) = p.item.folder {
        return folder_json(p, children, mode, spaces);
    }
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
    if let Some((t, from)) = &p.item.via {
        o.insert("via".into(), json!({"type": t, "from": from}));
    }
    o.insert("sources".into(), json!(p.item.sources));
    o.insert("tokens".into(), json!(p.tokens));
    o.insert("updated_at".into(), json!(fmt_ts(h.updated_at)));
    match &h.entry {
        Some(e) => {
            o.insert("kind".into(), json!(e.kind));
            o.insert("status".into(), json!(e.status));
            o.insert("insight_kind".into(), json!(e.insight_kind));
            o.insert("certainty".into(), json!(e.certainty));
            o.insert("title".into(), json!(e.title));
        }
        None => {
            o.insert("kind".into(), json!("note"));
            o.insert("certainty".into(), json!(h.certainty));
        }
    }
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

/// Заголовок карточки записи журнала (§9.3):
/// `i:<id> insight/<вид> «<заголовок>»  [0.77] active · hypothesis · ~120 tok`.
fn entry_card(e: &zam_store::EntryHit, id: Id, score: f64, tokens: usize) -> String {
    let letter = match e.kind.as_str() {
        "insight" => "i",
        "changelog" => "c",
        "release" => "r",
        _ => "e",
    };
    let mut line = format!("{letter}:{id} {}", e.kind);
    if let Some(k) = &e.insight_kind {
        line.push('/');
        line.push_str(&md_inline(k));
    }
    line.push_str(&format!(" «{}»", md_inline(&e.title)));
    let mut parts: Vec<String> = [&e.status, &e.certainty]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    parts.push(format!("~{tokens} tok"));
    line.push_str(&format!("  [{score:.2}] {}\n", parts.join(" · ")));
    line
}

/// Карточка §9.3: `1. n:<id> <space>/<path> › <секция>  [0.82] rev7 · 2026-10-10 · ~340 tok`
/// (у записи журнала — [`entry_card`]).
fn item_md(n: usize, p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> String {
    let h = &p.item.hit;
    let score = p.item.rerank_score.map_or(p.item.score, f64::from).min(1.0);
    let mut line = match (&h.entry, p.item.folder) {
        // карточка папки: `f:<id> <пространство>/<путь>/  [0.61] · 4 детей`
        (_, Some(children)) => {
            let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
            format!(
                "{n}. f:{} {space}/{}/  [{score:.2}] · {children} детей\n",
                h.target.id, h.path
            )
        }
        (Some(e), None) => format!("{n}. {}", entry_card(e, h.target.id, score, p.tokens)),
        (None, None) => {
            let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
            let mut line = format!("{n}. {} {space}/{}", h.target, h.path);
            if !h.heading_path.is_empty() {
                line.push_str(" › ");
                line.push_str(&h.heading_path);
            }
            let certainty = h
                .certainty
                .as_ref()
                .map(|c| format!(" · {c}"))
                .unwrap_or_default();
            line.push_str(&format!(
                "  [{score:.2}] rev{}{certainty} · {} · ~{} tok\n",
                h.rev,
                h.updated_at.strftime("%Y-%m-%d"),
                p.tokens
            ));
            line
        }
    };
    if let Some((t, from)) = &p.item.via {
        if line.ends_with('\n') {
            line.pop();
        }
        line.push_str(&format!(" · via {} ← {from}\n", t.as_str()));
    }
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

/// Состояние целей запроса: какие ещё в выдаче, и предупреждения ответа.
struct Health<'a> {
    targets: &'a [SearchTarget],
    /// Цель не исключена сбоем чтения своей базы.
    alive: Vec<bool>,
    warnings: Vec<String>,
}

impl<'a> Health<'a> {
    fn new(targets: &'a [SearchTarget], warnings: Vec<String>) -> Health<'a> {
        Health {
            targets,
            alive: vec![true; targets.len()],
            warnings,
        }
    }

    /// Результат чтения базы цели `i`: сбой в базе чужого решения исключает решение (WARN
    /// в журнал, `solution unavailable`, цель больше не жива) и даёт `None`; сбой личной
    /// базы — ошибка запроса.
    fn soft<T, E>(&mut self, i: usize, r: Result<T, E>) -> Result<Option<T>, ApiError>
    where
        E: std::fmt::Display + Into<ApiError>,
    {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(e) => match &self.targets[i].solution {
                Some(info) => {
                    skip_foreign(info, &e);
                    self.alive[i] = false;
                    self.warnings.push(WARN_SOLUTION_UNAVAILABLE.into());
                    Ok(None)
                }
                None => Err(e.into()),
            },
        }
    }
}

/// Точка папки векторной выдачи.
fn is_folder(h: &VectorHit) -> bool {
    h.level == zam_index::LEVEL_FOLDER
}

/// Точки папок `vh` — по базам владельцев: живые папки пространств цели без Human
/// ([`live_folders`]) как `(цель, папка, ранг)`, ранг — с 1 среди живых по порядку векторной
/// выдачи. Папка засчитывается только из базы своего владельца; сбой чтения базы чужого
/// решения исключает решение ([`Health::soft`]).
async fn check_folders(
    health: &mut Health<'_>,
    vh: &[VectorHit],
    prefix: Option<&str>,
) -> Result<Vec<(usize, zam_store::FolderRow, usize)>, ApiError> {
    let targets = health.targets;
    let mut live: HashMap<(Id, Id), (usize, zam_store::FolderRow)> = HashMap::new();
    for (i, t) in targets.iter().enumerate() {
        if !health.alive[i] {
            continue;
        }
        let ids: Vec<Id> = vh
            .iter()
            .filter(|h| is_folder(h) && h.user_id == t.owner && h.target.kind == TargetKind::Node)
            .map(|h| h.target.id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        let r = live_folders(&t.db, ids, &t.vector_space_ids(), prefix).await;
        if let Some(found) = health.soft(i, r)? {
            for f in found {
                live.entry((t.owner, f.id)).or_insert((i, f));
            }
        }
    }
    let mut out = Vec::new();
    for h in vh.iter().filter(|h| is_folder(h)) {
        if let Some((i, f)) = live.remove(&(h.user_id, h.target.id)) {
            let rank = out.len() + 1;
            out.push((i, f, rank));
        }
    }
    Ok(out)
}

/// Карточка папки цели `target` с баллом `score` (см. модуль, «Папки»).
fn folder_card(target: usize, f: &zam_store::FolderRow, score: f64) -> Item {
    let l0 = f.l0.clone().unwrap_or_default();
    Item {
        target,
        hit: FtsHit {
            chunk_id: String::new(),
            target: TargetRef {
                kind: TargetKind::Node,
                id: f.id,
            },
            space_id: f.space_id,
            path: f.path.clone(),
            heading_path: String::new(),
            level: zam_index::LEVEL_FOLDER.into(),
            rev: 0,
            token_count: 0,
            score,
            raw_score: 0.0,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            snippet: l0,
            text: String::new(),
            index_state: "indexed".into(),
            entry: None,
            area: None,
            certainty: None,
        },
        score,
        sources: vec![zam_search::Channel::Folder.as_str()],
        chunk_ids: Vec::new(),
        l0: f.l0.clone(),
        text: None,
        rerank_score: None,
        via: None,
        folder: Some(f.child_count),
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
    if req.query.chars().count() > zam_store::search::QUERY_MAX_CHARS {
        return Err(ApiError::BadRequest(format!(
            "запрос длиннее {} символов",
            zam_store::search::QUERY_MAX_CHARS
        )));
    }
    let budget = req.budget_tokens.unwrap_or(DEFAULT_BUDGET_TOKENS);
    let mut kinds = target_filter(&req.filters, req.include_history)?;
    kinds.area = area_filter(req.scope.area.as_deref())?;
    let boosts = st.cfg.search.boosts(header_value(&headers, AREA_HEADER)?);
    let (targets, warnings) = resolve_targets(&st, &p, &headers, &req.scope).await?;
    let mut health = Health::new(&targets, warnings);
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
    // точки папок: карточки по `filters.kinds ∋ folder`, канал подъёма — при `w_folder > 0`
    let want_folders = kinds.kinds.iter().any(|k| k == "folder");
    let folder_channel = st.cfg.search.w_folder > 0.0;

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
                    targets: kinds.clone(),
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
            kinds: kinds.kinds.clone(),
            exclude_statuses: excluded_statuses(&kinds),
            area: kinds.area.clone(),
            certainty: kinds.certainty.clone(),
            folders: want_folders || folder_channel,
            ..VectorFilter::default()
        };
        vector_channel(&st.index, filter, req.query.clone()).await
    };
    let ((runs, fts_ms), vrun) = tokio::join!(fts, vector);
    // цель, чья база не прочиталась (чужое решение), исключается из выдачи целиком
    let mut per_db = Vec::with_capacity(targets.len());
    for (i, r) in runs.into_iter().enumerate() {
        if let Some(ch) = health.soft(i, r)? {
            per_db.push((i, ch));
        }
    }
    let fts = merge_channels(per_db, FTS_BM25_TOP, FTS_TRIGRAM_TOP);

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
                if !health.alive[i] {
                    continue;
                }
                let ids: Vec<String> = vh
                    .iter()
                    .filter(|h| h.user_id == t.owner && !is_folder(h))
                    .map(|h| h.chunk_id.clone())
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                let scope = ChunkScope {
                    space_ids: t.vector_space_ids(),
                    path_prefix: prefix.clone(),
                    indexable_only: true,
                    targets: kinds.clone(),
                };
                if let Some(found) = health.soft(i, live_chunks(&t.db, ids, scope).await)? {
                    for h in found {
                        live.entry(h.chunk_id.clone()).or_insert((i, h));
                    }
                }
            }
            // порядок векторного канала; хит засчитывается только из базы своего владельца
            let ids: Vec<String> = vh
                .iter()
                .filter(|h| !is_folder(h))
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
    // точки папок — в базу своего владельца, только живые папки пространств цели
    let mut folders = match &vrun.hits {
        Some(vh) => check_folders(&mut health, vh, prefix.as_deref()).await?,
        None => Vec::new(),
    };
    // база, не прочитавшаяся при проверке векторных хитов, — вне выдачи целиком (кандидаты
    // без метаданных `fuse` отбрасывает)
    hits.retain(|_, (i, _)| health.alive[*i]);
    folders.retain(|(i, _, _)| health.alive[*i]);

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
                    // вид инсайта — только у insight (у changelog `data.kind` — вид изменения)
                    insight_kind: h
                        .entry
                        .as_ref()
                        .filter(|e| e.kind == "insight")
                        .and_then(|e| e.insight_kind.clone()),
                    // статус заметки или записи (бусты надёжности, §7.5)
                    certainty: h.certainty.clone(),
                    area: h.area.clone(),
                    space_id: h.space_id,
                    path: h.path.clone(),
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
        folders: folders
            .iter()
            .map(|(_, f, rank)| FolderHit {
                folder: f.id,
                space_id: f.space_id,
                path_prefix: f.path.clone(),
                rank: *rank,
            })
            .collect(),
    };
    let weights = st.cfg.search.weights();
    let (mut ranked, norm_max) = fuse_normed(&inputs, &weights, jiff::Timestamp::now());
    apply_boosts(&mut ranked, &inputs.chunks, &boosts);
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
            let r = best_leaves(&t.db, &group, &needles, query_vec).await;
            let Some(found) = health.soft(i, r)? else {
                continue;
            };
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
                    targets: kinds.clone(),
                };
                let Some(leaves) = health.soft(i, live_chunks(&t.db, missing, scope).await)? else {
                    continue;
                };
                for h in leaves {
                    hits.insert(h.chunk_id.clone(), (i, h));
                }
            }
            best.extend(found);
        }
    }
    // база, не прочитавшаяся при выборе листов, — вне выдачи целиком
    hits.retain(|_, (i, _)| health.alive[*i]);
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
                via: None,
                folder: None,
            })
        })
        .collect();
    // карточки папок (только по `filters.kinds ∋ folder`): лучшие живые папки канала
    let cards: Vec<Item> = if want_folders {
        folders
            .iter()
            .take(st.cfg.search.max_folders)
            .map(|(i, f, rank)| folder_card(*i, f, folder_score(*rank, &weights, norm_max)))
            .collect()
    } else {
        Vec::new()
    };
    let fuse_ms = ms(fuse_t);

    // 4. серверный реранк; пересборка индекса — предупреждение о неполной векторной выдаче
    if st.index.reindexing() {
        health
            .warnings
            .push(crate::index_state::WARN_REINDEXING.into());
    }
    let mut rerank_ms = None;
    let items = if rerank_on && !items.is_empty() {
        let t = Instant::now();
        let (items, warning) = rerank_items(&st, &req.query, items).await;
        rerank_ms = Some(ms(t));
        health.warnings.extend(warning.map(String::from));
        items
    } else {
        items
    };

    // 5. top_k, режим, expand, упаковка
    let expand_t = Instant::now();
    let mut items = match mode {
        SearchMode::Notes => group_by_node(items, top_k),
        SearchMode::Chunks => expand_items(&mut health, items, expand, top_k).await?,
        SearchMode::Brief => items.into_iter().take(top_k).collect(),
    };
    if expand == Expand::Graph {
        let extra = graph::expand_graph(
            &mut health,
            &items,
            top_k,
            st.cfg.search.graph_weight,
            &kinds,
            prefix.clone(),
            &needles,
            mode == SearchMode::Chunks,
        )
        .await?;
        items.extend(extra);
    }
    // карточки папок — по баллу среди найденных, до соседей по графу
    for card in cards {
        let pos = items
            .iter()
            .position(|it| it.via.is_some() || it.score < card.score)
            .unwrap_or(items.len());
        items.insert(pos, card);
    }
    if mode == SearchMode::Notes {
        for it in &mut items {
            if it.hit.target.kind == TargetKind::Node
                && it.folder.is_none()
                && health.alive[it.target]
            {
                let r = get_node(&targets[it.target].db, it.hit.target.id).await;
                if let Some(n) = health.soft(it.target, r)? {
                    it.l0 = n.l0;
                }
            }
        }
    }
    // элементы целей, исключённых на этих шагах, — прочь
    items.retain(|it| health.alive[it.target]);
    let expand_ms = ms(expand_t);
    // заметки области всех целей, ещё не прошедшие векторную индексацию (без `index: false`
    // и Human)
    let pending_t = Instant::now();
    let mut pending = 0;
    for (i, t) in targets.iter().enumerate() {
        if !health.alive[i] {
            continue;
        }
        let r = pending_count(&t.db, t.space_ids(), prefix.clone()).await;
        pending += health.soft(i, r)?.unwrap_or(0);
    }
    let pending_ms = ms(pending_t);
    // pending_count цели, исключённой на этом шаге, уже не прибавлен; её элементы — прочь
    items.retain(|it| health.alive[it.target]);
    let mut warnings = health.warnings;
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
    use zam_store::StoreError;

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
            entry: None,
            area: None,
            certainty: None,
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
                via: None,
                folder: None,
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

    /// Текст секции листа (без ключа).
    fn own_section<'a>(src: &'a str, heading_path: &str, leaf: &str) -> Option<&'a str> {
        leaf_section(src, heading_path, leaf).map(|(_, t)| t)
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

    /// Путь листа с H4–H6 (задача 17): секция — H1–H3, в которой лежит лист; листы одной
    /// секции под разными H4 дают один ключ слияния, одноимённые секции — разные.
    #[test]
    fn leaf_section_for_h4_paths() {
        let src = "# A\n\n## B\n\nначало B\n\n#### Глубокий\n\nтекст под H4\n\n\
                   ##### Ещё глубже\n\nтекст под H5\n\n## C\n\nтекст C\n";
        let b = "## B\n\nначало B\n\n#### Глубокий\n\nтекст под H4\n\n##### Ещё глубже\n\n\
                 текст под H5";
        let k4 = leaf_section(src, "A > B > Глубокий", "#### Глубокий\n\nтекст под H4");
        assert_eq!(k4.map(|(_, t)| t), Some(b));
        let k5 = leaf_section(
            src,
            "A > B > Глубокий > Ещё глубже",
            "##### Ещё глубже\n\nтекст под H5",
        );
        assert_eq!(k5, k4);
        // лист не найден в тексте — самый длинный префикс пути
        assert_eq!(own_section(src, "A > B > Глубокий", "нет такого"), Some(b));
        assert_eq!(own_section(src, "A > Q", "нет такого"), Some("# A"));
        assert_eq!(own_section(src, "Q > B", "нет такого"), None);
        // « > » в тексте заголовка не путает: «B > X» — не подсекция B
        let gt = "# A\n\n## B\n\nпро B\n\n## B > X\n\nпро B > X\n\n#### H\n\nлист\n";
        assert_eq!(
            own_section(gt, "A > B > X > H", "#### H\n\nлист"),
            Some("## B > X\n\nпро B > X\n\n#### H\n\nлист")
        );
        // H4 в преамбуле (до первого H1–H3) — преамбула
        let pre = "#### Сначала\n\nтекст до заголовков\n\n# A\n\nтекст A\n";
        assert_eq!(
            own_section(pre, "Сначала", "#### Сначала\nтекст до заголовков"),
            Some("#### Сначала\n\nтекст до заголовков")
        );
        // одноимённые секции с H4 внутри — разные ключи
        let dup = "# A\n\n#### X\n\nпервый лист\n\n# A\n\n#### X\n\nвторой лист\n";
        let k1 = leaf_section(dup, "A > X", "#### X\nпервый лист").unwrap();
        let k2 = leaf_section(dup, "A > X", "#### X\nвторой лист").unwrap();
        assert_ne!(k1.0, k2.0);
        assert_eq!(k2.1, "# A\n\n#### X\n\nвторой лист");
    }

    /// Сбой чтения базы чужого решения исключает цель с предупреждением, сбой личной базы —
    /// ошибка запроса (инъекция ошибки в `Health::soft`).
    #[tokio::test]
    async fn health_excludes_foreign_target_personal_fails() {
        let dir = crate::test_util::dir_in_target("search-health");
        let db = Db::open(&dir.join("u.db"), zam_store::USER_MIGRATIONS).unwrap();
        let sp = Space {
            id: Id::new(),
            kind: SpaceKind::Solution,
            solution_guid: None,
            name: "s".into(),
        };
        let mk = |solution| SearchTarget {
            owner: Id::new(),
            db: db.clone(),
            spaces: vec![sp.clone()],
            role: super::super::scope::Role::Personal,
            solution,
        };
        let info = zam_store::SolutionInfo {
            guid: zam_core::SolutionGuid::new_v7(),
            owner_id: Id::new(),
            space_id: sp.id,
            name: "x".into(),
            created_at: jiff::Timestamp::now(),
        };
        let targets = vec![mk(None), mk(Some(info))];
        let mut h = Health::new(&targets, vec!["solution access pending".into()]);
        let fail = || Err::<i64, StoreError>(StoreError::NotFound);
        assert_eq!(h.soft(1, Ok::<i64, StoreError>(5)).unwrap(), Some(5));
        assert_eq!(h.soft(1, fail()).unwrap(), None);
        assert_eq!(h.alive, vec![true, false]);
        assert_eq!(
            h.warnings,
            vec!["solution access pending", "solution unavailable"]
        );
        // ошибка сервера (best_leaves) — так же
        assert!(
            h.soft(1, Err::<(), ApiError>(ApiError::NotFound))
                .unwrap()
                .is_none()
        );
        // личная база — ошибка запроса, цель остаётся
        assert!(h.soft(0, fail()).is_err());
        assert!(h.alive[0]);

        // drop_target: элементы цели убраны, индекс секций пересобран
        let mut out = items(
            vec![
                hit(Id::new(), "1"),
                hit(Id::new(), "22"),
                hit(Id::new(), "333"),
            ],
            false,
        );
        out[1].target = 1;
        let mut sections: HashMap<(TargetRef, SectionKey), usize> = out
            .iter()
            .enumerate()
            .map(|(i, it)| ((it.hit.target, SectionKey::At(i)), i))
            .collect();
        drop_target(&mut out, &mut sections, 1);
        assert_eq!(out.len(), 2);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[&(out[0].hit.target, SectionKey::At(0))], 0);
        assert_eq!(sections[&(out[1].hit.target, SectionKey::At(2))], 1);
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

    #[test]
    fn md_entry_card_format() {
        let id = Id::new();
        let mut it = items(vec![hit(id, "текст")], false).remove(0);
        it.hit.target = TargetRef {
            kind: TargetKind::Entry,
            id,
        };
        it.hit.entry = Some(zam_store::EntryHit {
            kind: "insight".into(),
            status: Some("active".into()),
            insight_kind: Some("gotcha".into()),
            certainty: Some("hypothesis".into()),
            title: "Bash-скрипты должны иметь LF".into(),
        });
        let mut p = Packed {
            item: it,
            text: None,
            tokens: 120,
        };
        let s = item_md(2, &p, SearchMode::Brief, &HashMap::new());
        assert_eq!(
            s,
            format!(
                "2. i:{id} insight/gotcha «Bash-скрипты должны иметь LF»  [0.50] active · \
                 hypothesis · ~120 tok\n   **s**\n"
            )
        );
        // changelog без статуса и certainty
        p.item.hit.entry = Some(zam_store::EntryHit {
            kind: "changelog".into(),
            status: None,
            insight_kind: Some("fix".into()),
            certainty: None,
            title: "Исправлен CRLF".into(),
        });
        p.tokens = 9;
        let s = item_md(1, &p, SearchMode::Brief, &HashMap::new());
        assert!(
            s.starts_with(&format!(
                "1. c:{id} changelog/fix «Исправлен CRLF»  [0.50] ~9 tok\n"
            )),
            "{s}"
        );
        let j = item_json(&p, SearchMode::Brief, &HashMap::new());
        assert_eq!(j["entry_id"], json!(id));
        assert!(j.get("node_id").is_none());
        assert_eq!(j["kind"], "changelog");
        assert_eq!(j["insight_kind"], "fix");
        assert_eq!(j["title"], "Исправлен CRLF");
        assert!(j["status"].is_null());
    }

    #[test]
    fn filters_defaults_and_validation() {
        let f = target_filter(&Filters::default(), false).unwrap();
        assert_eq!(f.kinds, DEFAULT_KINDS.map(String::from).to_vec());
        assert_eq!(f.statuses, DEFAULT_STATUSES.map(String::from).to_vec());
        assert_eq!(excluded_statuses(&f), vec!["superseded", "retracted"]);
        // include_history снимает фильтр статуса
        let f = target_filter(&Filters::default(), true).unwrap();
        assert!(f.statuses.is_empty() && excluded_statuses(&f).is_empty());
        let f = target_filter(
            &Filters {
                kinds: Some(vec!["insight".into()]),
                status: Some(vec!["retracted".into()]),
                certainty: Some(vec!["axiom".into(), "theorem".into()]),
            },
            false,
        )
        .unwrap();
        assert_eq!(f.kinds, vec!["insight"]);
        assert_eq!(f.certainty, vec!["axiom", "theorem"]);
        // по умолчанию certainty не ограничен
        assert!(
            target_filter(&Filters::default(), false)
                .unwrap()
                .certainty
                .is_empty()
        );
        assert_eq!(
            excluded_statuses(&f),
            vec!["active", "revisit", "superseded"]
        );
        for bad in [
            Filters {
                kinds: Some(vec!["nope".into()]),
                ..Filters::default()
            },
            Filters {
                status: Some(vec!["Active".into()]),
                ..Filters::default()
            },
            Filters {
                certainty: Some(vec!["certain".into()]),
                ..Filters::default()
            },
        ] {
            assert!(target_filter(&bad, false).is_err());
        }
    }
}
