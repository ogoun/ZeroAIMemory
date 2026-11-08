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
//! - `session_id` (или `X-Zam-Session`) — сессия агента: первый поиск с ней создаёт сессию
//!   ([`super::sessions`]); `skip_seen` (по умолчанию `true`) — элемент, уже выданный этой
//!   сессии в той же ревизии, идёт с `seen: true` без фрагмента и текста (в md — `(seen)`, §9.3).
//!   Выданным считается элемент, у которого в той же ревизии отмечены все его чанки
//!   (`chunk_ids`) или цель целиком (`part = ''`, полный текст `/read`) — решение
//!   контроллера R11. Чанки `mode=chunks`, отданные целиком (не усечённые бюджетом),
//!   записываются в `session_seen` базы пользователя запроса (`part = chunk_id`, владелец —
//!   владелец базы цели) одной транзакцией с касанием сессии. Карточки папок не отмечаются;
//!   сбой учёта сессии поиск не валит (WARN, ответ без пометок).
//!   M6 §7.1: учитывается только **явная** сессия — неявную, подставленную MCP без
//!   `X-Zam-Session`, поиск не видит. Запрос из инструмента MCP без явного `skip_seen`
//!   помечает выданное (`seen: true`, в md — `(выдавалось)`), но оставляет фрагмент и текст:
//!   субагенты на общем подключении MCP приходят с сессией родителя. Выданным считается только
//!   отмеченное в текущей эпохе сессии (M6 §7.2): после сжатия контекста — снова фрагмент.
//!   Исключение (решение контролёра 2026-11-06, §7.1): карточка `(выдавалось)`, чья цель —
//!   источник действующей выжимки шага 0 **этого же** ответа (та же цель и ревизия; у
//!   источника-секции — только карточки, чей чанк целиком лежит в тексте секции), идёт без
//!   фрагмента и текста с пометкой `(выдавалось, в выжимке)`, в JSON — `seen: true`,
//!   `in_cache: true` ([`mark_cache_sources`]): текст источника уже в ответе; у `[cache, stale:
//!   …]` фрагмент остаётся;
//! - `filters.kinds` — виды целей: `note`, `insight`, `changelog`, `release`, `attachment`
//!   (фрагменты вложений, M5; см. «Вложения») — по умолчанию все, `folder` — карточки папок
//!   (см. «Папки»); `filters.status` — статусы insight (по умолчанию `active`,
//!   `revisit`); `include_history: true` снимает фильтр статуса. Фильтр действует и в Qdrant
//!   (`kinds`, `must_not` по исключённым статусам), и в FTS, и в проверке векторных хитов
//!   ([`live_chunks`]); у changelog и release статуса нет — они проходят всегда.
//!   Отчёты (M5.5 §7) — только сессиям WebUI: вид `report` у них допустим и входит в умолчания
//!   (карточка `{kind: "report", id: "p:<id>", space, title, snippet, created_at}`); токену вида
//!   нет (400 тем же текстом, что у неизвестного), его умолчания и `scope_hash` прежние.
//!   Идеи (M5.7 §4) — вид `idea`: у токена только явным `kinds`, у сессии WebUI — и по
//!   умолчанию; статусы по умолчанию `new, proposed, deferred, accepted`. Вид и статусы идей в
//!   ключ кэша выжимок не входят ([`scope_key`]).
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
//! **Шаг 0 — кэш выжимок** (§9.3, §9.4; [`cache_step`]): без `no_cache: true` поиск после
//! каналов (нужен dense-вектор запроса) ищет действующую выжимку того же `scope_hash` и
//! похожего запроса; не больше двух выжимок идут первыми элементами `kind: "cache"` (md —
//! `[cache] «запрос» — выжимка <дата>, <N> источников, все актуальны (id: c_…)` и текст;
//! недействительная — `[cache, stale: <источники>]` без текста), их текст — из
//! `budget_tokens`; `timings.cache_ms`.
//!
//! **Словарь домена** (M4.5, спецификация §2.2 п. 1; [`crate::glossary`]): слова запроса
//! сопоставляются (без регистра, многословные имена — по подряд идущим словам, не больше
//! [`crate::glossary::MAX_GROUPS`] мест) с именами и псевдонимами действующих терминов
//! пространств каждой цели; по ним строится дополнительный FTS-запрос — канал `alias` (источник
//! `alias` в `sources`), где совпавшее слово заменено группой `("слово"* OR "имя"* OR …)`;
//! псевдоним короче 3 символов ищется точным словом. Вес — `search.w_fts_alias` (0,5; 0 выключает
//! канал и расширение), ослабленный `alias` при векторном канале — не выше `w_fts_relaxed`.
//! Векторный канал, trigram, ключ кэша выжимок (`scope_hash` + `query_norm`) не меняются;
//! `QUERY_MAX_CHARS` проверяется по исходному тексту. Ответ несёт `expanded: [{term, word,
//! with}]`. Умолчания видов: к `note`, `insight`, `changelog`, `release` добавлены `term` и
//! `question` (статус `answered`); открытые вопросы и задачи — только явным `filters.kinds`
//! ([`default_statuses`]: фильтр статусов общий для FTS и `must_not` Qdrant, поэтому согласован
//! в обоих каналах); `include_history` снимает фильтр статусов у всех видов.
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
//!    `X-Zam-Area` — `search.boost_area`; балл выше 1 после бустов и канала папок — деление
//!    всех баллов на лучший, `zam_search::rescale_overflow`) → `note_to_leaf` →
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
//!    заголовок секции и текст чанка», фрагмент до `ml.rerank_tokens` = 256 токенов) с
//!    бустами п. 4 — вид, область, `certainty` ([`zam_search::apply_boosts_to`], та же
//!    функция, что после слияния; затухание по свежести и канал папок к баллу реранкера не
//!    применяются; решение владельца 2026-10-26), затем то же деление при переполнении
//!    ([`zam_search::overflow_divisor`]); хвост остаётся в порядке слияния; лимит на узел
//!    после перестановки соблюдается (набор тот же). Реранкер — по устройству эмбеддера,
//!    грузится при первом таком запросе (`spawn_blocking`). Нет модели — ответ без реранка и
//!    `warnings: ["reranker not installed"]`; ошибка или паника загрузки и инференса — `["reranker
//!    failed"]`, не 500.
//! 5. `top_k` (в `notes` — узлов), `expand` (`graph` — соседи после `top_k`), упаковка в
//!    `budget_tokens`.
//!
//! **Папки** (§8.3, §9.2). Векторный запрос добавляет prefetch dense top-10 по точкам папок
//! (`VectorFilter::folders`), если включён канал папок (`search.w_folder > 0`, по умолчанию
//! 0 — по сетке задачи 18 M3) или запрошены карточки (`filters.kinds ∋ folder`). Точки папок
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
//! **Вложения** (M5, задача 6). Фрагмент документа-вложения (чанк `level = attachment`) — своя
//! цель: документ (`attachment_id`), а не заметка-владелец; каналы FTS и векторный (листовой
//! prefetch уровня `attachment`, Human — только FTS). Видимость — живой документ живого узла в
//! пространствах цели (участнику общего решения — только пространство решения); путь, `area`,
//! `certainty` и `filters.file` — заметки-владельца на момент поиска (Qdrant их у точек вложений
//! не проверяет — проверяет SQLite). С документа — не больше `search.max_per_attachment` (2)
//! фрагментов, квота заметки (`max_per_node`) — своя. `expand=section|neighbors|graph`, L0 и
//! `seen` сессии к вложениям не применяются (`/read` по `locator` и учёт выданного — задача 7).
//! JSON: `kind: "attachment"`, `id: "a:<chunk>"` (ссылка для `/read`), `node_id` владельца,
//! `attachment_id`, `file`, `blob_sha256`, `locator` (`page:12`, `sheet:Итоги!A1:F40`,
//! `slide:3`, `para:40`), `locator_text` (`стр. 12`, `лист «Итоги» A1:F40`, `слайд 3`, `абз. 40`),
//! `path`, `heading_path`, `level`, `rev` (0), `certainty` владельца, `snippet` (и в `notes`) или
//! `text`; md — `<n>. a:<chunk> <пространство>/<путь заметки> › <файл>[ › <путь заголовков>][ ·
//! <locator_text>]  [score][ · <certainty>] · <дата> · <возраст> · ~<N> tok`.
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
//! полного текста чанка, `updated_at`, `age_days` (M4.5 §7: целые сутки от `updated_at`, у
//! заметки с `verified_at` — от более позднего), `kind` (`note` или вид записи), `snippet` | `l0`,
//! `text`, `rerank_score` 0..1 — сырой балл реранкера у переставленных им (их `score` — он
//! же с бустами), `via {type, from}` — у соседей
//! `expand=graph`), `degraded`, `degraded_reason`
//! и `degraded_text` (или `null`), `warnings` (строки: о реранке, `index rebuilding` — индекс
//! пересобирается, векторная выдача может быть неполной, `solution access …` и
//! `solution unavailable` — решение исключено из области), `pending_count` — живые
//! заметки и записи области, ждущие векторной индексации (сумма по целям, не исключённым к
//! концу ответа). Это снимок **до** векторного запроса: воркер ставит `indexed` после
//! подтверждённой записи точек, поэтому `pending_count == 0` гарантирует, что подтверждённые
//! upsert видны вектору; цель, проиндексированная между снимком и запросом, лишь завышает
//! счётчик. `timings {fts_ms, embed_ms, vector_ms,
//! fuse_ms, rerank_ms, expand_ms, pending_ms, total_ms}` (`null` — этап не выполнялся;
//! `total_ms` — весь обработчик без сериализации ответа). Маркеры совпадений в JSON —
//! U+E000/U+E001.
//!
//! Элемент заметки несёт `certainty` её текущей ревизии (`null` — без статуса).
//! Элемент записи журнала вместо `node_id` несёт `entry_id` и поля записи: `kind`,
//! `status`, `insight_kind` (`data.kind`), `certainty`, `title`; путь у записи пуст,
//! `expand` и L0 к записям не применяются.
//!
//! **Формат `md`** — карточки v2 (§9.3, M6 §4.1), по одной на элемент:
//! `<n>. n:<id> <пространство>/<путь> › <секция>  [.82] r<N>[ · ax|th|hyp] · <дата UTC
//! yyyy-MM-dd HH:mm:ss> · <N>t` ([`card_tail`]; возраста нет, `age_days` — в JSON); у записи —
//! `<n>. i:<id> insight/<вид> «<заголовок>»  [.77][ hyp ·][ <статус, если не active> ·] <дата> ·
//! <N>t` (`c:` — changelog, `r:` — release; ревизии в хвосте записи нет), у папки —
//! `f:<id> <пространство>/<путь>/  [.61] <N> детей`; у соседа `expand=graph` — ` · via <тип> ←
//! <родитель>`. **Бюджет** Markdown — вся карточка счётчиком сервера вместе со строками
//! деградации, предупреждений и строкой MCP `~N tok` ([`pack_items`]); `brief`/`notes` без
//! карточек с баллом ниже `brief_min_rel × лучший` (поле запроса или `search.brief_min_rel`).
//! под ней с отступом — сниппет (`brief`, маркеры → `**…**`), L0 (`notes`) или текст
//! (`chunks`); балл — реранка, если элемент переставлен им. При деградации строка
//! `> degraded: <код> — <текст причины> (только FTS)`, в том числе при пустой выдаче (тогда она
//! единственная), предупреждения — строками `> warning: <текст>` в конце.
//!
//! **Ошибки баз владельцев.** Любой сбой чтения базы чужого решения — FTS, проверка векторных
//! хитов, листы сводок, `expand`, L0, `pending_count` — исключает решение целиком ([`Health`]):
//! WARN в журнал, `warnings: ["solution unavailable"]`, элементы этой цели убираются из
//! выдачи, ответ строится по остальным целям. Сбой личной базы — ошибка запроса, как в M2.
mod cache_step;
pub mod graph;
pub mod merge;
pub mod targets;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use futures_util::future::join_all;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::{Id, SolutionGuid, TargetKind, TargetRef};
use zam_index::{IndexError, VectorFilter, VectorHit, VectorIndex, VectorScope};
use zam_ml::Embedding;
use zam_search::{
    Boosts, ChunkMeta, FolderHit, KindBoostMode, LeafRef, Level, Ranked, SearchInputs, Weights,
    apply_boosts, apply_boosts_to, cap_per_node, drop_unresolved_tail, folder_score, fuse_normed,
    in_section, note_to_leaf, overflow_divisor, rescale_overflow, summaries_without_leaf,
    target_cap, truncate_to_tokens,
};
use zam_store::time::fmt_ts;
use zam_store::{
    AttachmentHit, ChunkScope, Db, EntryKind, FtsHit, FtsQuery, LeafHit, MARK_END, MARK_START,
    SeenItem, SeenKey, SeenTarget, Space, SpaceKind, StoreError, TargetFilter, TokenCount,
    WHOLE_TARGET, chunk_ref, count_tokens, get_embeddings, get_node, leaf_term_hits,
    leaves_of_targets, live_chunks, live_folders, mark_seen_with_tokens, neighbor_leaves,
    node_content, pending_count, query_needles, search_fts_channels, seen_revs, text_snippet,
    touch_session,
};

use self::merge::merge_channels;
use self::targets::{
    ScopeReq, SearchTarget, WARN_SOLUTION_UNAVAILABLE, dedup_keep_order, resolve_targets,
};
use super::doc_locator::{self, Sheets, secret_sheet};
use super::json_body;
use super::scope::skip_foreign;
use super::sessions::{McpOrigin, RequestSession, explicit_only, seen_session};
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, header_value};
use crate::auth::Principal;
use crate::config::SearchSection;
use crate::error::ApiError;
use crate::glossary::{Expansion, alias_groups};
use crate::index_state::{DegradedReason, IndexRuntime};
use crate::rerank;

/// Заголовок текущего решения агента.
pub const SOLUTION_HEADER: &str = "x-zam-solution";
const DEFAULT_TOP_K: usize = 10;
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
pub(crate) struct Filters {
    kinds: Option<Vec<String>>,
    status: Option<Vec<String>>,
    /// `axiom | theorem | hypothesis` (§7.5); пусто — любые, в том числе без статуса.
    certainty: Option<Vec<String>>,
    /// `human | agent` (M4.5, §5.1): только решения с таким `data.decision.decided_by`
    /// (без поля — по автору записи); остальные записи и заметки отсеиваются.
    decided_by: Option<String>,
    /// Путь файла (M4.5 §6.3): только заметки и записи со ссылкой на него (точное совпадение
    /// после нормализации, некорректный путь — 400).
    file: Option<String>,
    /// Виды insight (`data.kind`, §9.1): insight проходят, только если их вид из списка;
    /// заметки и записи других видов фильтр не трогает (их отбирает `kinds`).
    insight_kinds: Option<Vec<String>>,
}

/// Вид insight, которого не бывает: векторный фильтр с пустым пересечением видов
/// (`decided_by` + `insight_kinds` без `decision`) не пропускает ни одной точки.
const NO_INSIGHT_KIND: &str = "-";

/// Допустимые виды целей `filters.kinds` (с M4.5 — `term`, `task`, `question`, спецификация
/// M4.5 §1.3; с M5.7 — `idea`, только явным `kinds`, как `task`).
const KINDS: [&str; 10] = [
    "note",
    "insight",
    "changelog",
    "release",
    "term",
    "task",
    "question",
    "folder",
    "attachment",
    IDEA,
];
/// Виды по умолчанию (§9.1; с M4.5 — `term` и `question`, спецификация M4.5 §1.3; с M5 —
/// `attachment`). Умолчания входят в `scope_hash` кэша выжимок: выжимки поиска с прежними
/// умолчаниями таким поиском не находятся и пересоздаются (CHANGELOG).
const DEFAULT_KINDS: [&str; 7] = [
    "note",
    "insight",
    "changelog",
    "release",
    "term",
    "question",
    "attachment",
];
/// Отчёт (M5.5 §7): вид целей только сессий WebUI (поиск Ctrl+K) — в допустимых и по
/// умолчанию; токену (агенту) такого вида нет: `report` → 400 тем же текстом, что у
/// неизвестного вида, умолчания и `scope_hash` токена прежние.
const REPORT: &str = "report";
/// Идея (M5.7 §4): у токена — только явным `kinds` (умолчания и `scope_hash` токена прежние), у
/// сессии WebUI — и по умолчанию (Ctrl+K находит идеи). В ключ кэша выжимок не входит, как
/// [`REPORT`] ([`scope_key`]).
const IDEA: &str = "idea";
/// [`KINDS`] сессий WebUI.
const HUMAN_KINDS: [&str; 11] = [
    "note",
    "insight",
    "changelog",
    "release",
    "term",
    "task",
    "question",
    "folder",
    "attachment",
    REPORT,
    IDEA,
];
/// [`DEFAULT_KINDS`] сессий WebUI.
const HUMAN_DEFAULT_KINDS: [&str; 9] = [
    "note",
    "insight",
    "changelog",
    "release",
    "term",
    "question",
    "attachment",
    REPORT,
    IDEA,
];
/// Статусы записей `filters.status`: insight и term, с M4.5 — task и question (спецификация
/// M4.5 §1.3), с M5.7 — идеи ([`IDEA_STATUSES`]; `done` — общий с task). Умолчания по виду —
/// [`default_statuses`].
const STATUSES: [&str; 16] = [
    "active",
    "revisit",
    "superseded",
    "retracted",
    "open",
    "in_progress",
    "blocked",
    "done",
    "dropped",
    "answered",
    "obsolete",
    "new",
    "proposed",
    "accepted",
    "deferred",
    "declined",
];
/// Статусы, которые бывают только у идей: в ключ кэша выжимок не входят ([`scope_key`]).
const IDEA_STATUSES: [&str; 5] = ["new", "proposed", "accepted", "deferred", "declined"];
/// Статусы идей по умолчанию (M5.7 §4): живые замыслы и начатые, без `declined` и `done`.
const IDEA_DEFAULT_STATUSES: [&str; 4] = ["new", "proposed", "deferred", "accepted"];
/// Статусы по умолчанию (§7.5): действующие.
const DEFAULT_STATUSES: [&str; 2] = ["active", "revisit"];

/// Статусы по умолчанию для видов `kinds` (`explicit` — виды заданы запросом). Фильтр статусов
/// общий для всех видов (FTS и `must_not` Qdrant), поэтому умолчания складываются:
/// - `insight` и `term` — действующие (`active`, `revisit`);
/// - `question`: в умолчании видов — только `answered`, при явном `kinds` — ещё `open`;
/// - `task` (только явным `kinds`): `open`, `in_progress`, `blocked` — выполненные и
///   отброшенные только явным `filters.status`;
/// - `idea` (токену — только явным `kinds`): [`IDEA_DEFAULT_STATUSES`].
///
/// Значения `answered` и `open` в других видах не встречаются, поэтому слияние безопасно.
fn default_statuses(kinds: &[String], explicit: bool) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = DEFAULT_STATUSES.to_vec();
    let has = |k: &str| kinds.iter().any(|x| x == k);
    if has("question") {
        v.push("answered");
        if explicit {
            v.push("open");
        }
    }
    if has("task") {
        for s in ["open", "in_progress", "blocked"] {
            if !v.contains(&s) {
                v.push(s);
            }
        }
    }
    if has(IDEA) {
        v.extend(IDEA_DEFAULT_STATUSES);
    }
    v
}
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
            kind_mode: self.kind_boost_mode,
            kind_exempt: HashSet::new(),
        }
    }
}

/// Фильтр векторного канала. При `decided_by` точки сужаются до решений (`kind = insight`,
/// `insight_kinds = [decision]`): иначе другие записи вытесняют из окна выдачи редкие решения
/// агента. Деление `human`/`agent` внутри Qdrant неполное — его делает SQLite при `live_chunks`.
/// `insight_kinds` сужает точки, только когда виды целей — одни insight: условие Qdrant
/// `insight_kind` отсекло бы заметки и записи без вида; иначе виды insight отсеивает SQLite
/// (`live_chunks`). С `decided_by` — пересечение с `decision` (пустое — ни одной точки).
fn vector_filter(
    scopes: Vec<VectorScope>,
    path_prefix: Option<String>,
    t: &TargetFilter,
    folders: bool,
) -> VectorFilter {
    let by = t.decided_by.is_some();
    let only_insight = by || (t.kinds.len() == 1 && t.kinds[0] == "insight");
    let insight_kinds = match (by, t.insight_kinds.is_empty()) {
        (true, true) => vec!["decision".to_string()],
        (true, false) if t.insight_kinds.iter().any(|k| k == "decision") => {
            vec!["decision".to_string()]
        }
        (true, false) => vec![NO_INSIGHT_KIND.to_string()],
        (false, _) if only_insight => t.insight_kinds.clone(),
        (false, _) => Vec::new(),
    };
    VectorFilter {
        scopes,
        path_prefix,
        kinds: if by {
            vec!["insight".into()]
        } else {
            t.kinds.clone()
        },
        insight_kinds,
        exclude_statuses: excluded_statuses(t),
        area: t.area.clone(),
        certainty: t.certainty.clone(),
        folders,
        ..VectorFilter::default()
    }
}

/// Фильтр целей запроса: виды и статусы (`include_history` — любые статусы); `human` — сессия
/// WebUI (виды с `report`, [`HUMAN_KINDS`]), иначе токен.
fn target_filter(
    f: &Filters,
    include_history: bool,
    human: bool,
) -> Result<TargetFilter, ApiError> {
    let (allowed, default): (&[&str], &[&str]) = if human {
        (&HUMAN_KINDS, &HUMAN_DEFAULT_KINDS)
    } else {
        (&KINDS, &DEFAULT_KINDS)
    };
    let kinds = filter_list("filters.kinds", f.kinds.as_ref(), allowed, default)?;
    let explicit = f.kinds.as_ref().is_some_and(|k| !k.is_empty());
    let statuses = filter_list(
        "filters.status",
        f.status.as_ref(),
        &STATUSES,
        &default_statuses(&kinds, explicit),
    )?;
    let certainty = filter_list("filters.certainty", f.certainty.as_ref(), &CERTAINTIES, &[])?;
    let mut insight_kinds = filter_list(
        "filters.insight_kinds",
        f.insight_kinds.as_ref(),
        &super::insights::INSIGHT_KINDS,
        &[],
    )?;
    insight_kinds.sort();
    insight_kinds.dedup();
    let with_idea = kinds.iter().any(|k| k == IDEA);
    Ok(TargetFilter {
        kinds,
        // include_history снимает фильтр статусов; без явного `kinds` открытые вопросы и задачи
        // (`open`) в историю не входят - они только по явному `kinds`
        statuses: match (include_history, explicit) {
            (false, _) => statuses,
            (true, true) => Vec::new(),
            // статусы идей — только если идеи среди видов (у токена без явного idea их нет):
            // список истории и ключ кэша выжимок токена прежние
            (true, false) => STATUSES
                .iter()
                .filter(|s| **s != "open")
                .filter(|s| with_idea || !IDEA_STATUSES.contains(s))
                .map(|s| s.to_string())
                .collect(),
        },
        area: None,
        certainty,
        decided_by: super::entries::parse_decided_by(
            f.decided_by.as_deref(),
            "filters.decided_by",
        )?,
        file: f
            .file
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                zam_store::normalize_ref_path(p)
                    .map_err(|e| ApiError::BadRequest(format!("filters.file: {e}")))
            })
            .transpose()?,
        insight_kinds,
        entry_ids: Vec::new(),
    })
}

/// Путь-префикс области: без `/` в конце, пустой — `None`.
fn path_prefix(sc: &ScopeReq) -> Option<String> {
    sc.path_prefix
        .as_deref()
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// `scope_hash` кэша выжимок (§9.4) для области запроса: решение, флаги Knowledge и Rules (по
/// умолчанию — да), префикс пути, область и фильтры после умолчаний (списки упорядочены) —
/// одинаковая для `POST /search` и `POST /cache`; `human` — сессия WebUI ([`target_filter`]).
pub(crate) fn cache_scope_hash(
    sc: &ScopeReq,
    f: &Filters,
    include_history: bool,
    solution: Option<&SolutionGuid>,
    human: bool,
) -> Result<String, ApiError> {
    let mut t = target_filter(f, include_history, human)?;
    t.area = area_filter(sc.area.as_deref())?;
    Ok(scope_key(sc, &t, solution))
}

/// Ключ области ([`cache_scope_hash`]). Вид `report` в ключ не входит: отчёт источником выжимки
/// не бывает (M5.5 §7), и поиск WebUI с умолчаниями (там есть `report`) находит выжимки той же
/// области агента. Так же исключены вид `idea` и статусы идей (M5.7 §4, I9): иначе новое
/// умолчание WebUI изменило бы ключ, и поиск WebUI перестал бы находить прежние выжимки.
fn scope_key(sc: &ScopeReq, t: &TargetFilter, solution: Option<&SolutionGuid>) -> String {
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    let kinds: Vec<String> = t
        .kinds
        .iter()
        .filter(|k| *k != REPORT && *k != IDEA)
        .cloned()
        .collect();
    // статусы идей не входят в ключ, только когда в видах есть idea (умолчания WebUI или явный
    // вид): без idea они — обычный фильтр других видов и меняют выдачу
    let with_idea = t.kinds.iter().any(|k| k == IDEA);
    let statuses: Vec<String> = t
        .statuses
        .iter()
        .filter(|s| !(with_idea && IDEA_STATUSES.contains(&s.as_str())))
        .cloned()
        .collect();
    let mut filters = json!({
        "kinds": sorted(&kinds),
        "status": sorted(&statuses),
        "certainty": sorted(&t.certainty),
    });
    // ключ только при заданном фильтре: хэши прежних кэшей не меняются
    if let Some(by) = &t.decided_by {
        filters["decided_by"] = json!(by);
    }
    if let Some(f) = &t.file {
        filters["file"] = json!(f);
    }
    if !t.insight_kinds.is_empty() {
        filters["insight_kinds"] = json!(sorted(&t.insight_kinds));
    }
    zam_store::scope_hash(&json!({
        "solution": solution.map(SolutionGuid::as_str),
        "include_knowledge": sc.include_knowledge.unwrap_or(true),
        "include_rules": sc.include_rules.unwrap_or(true),
        "path_prefix": path_prefix(sc),
        "area": t.area,
        "filters": filters,
    }))
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
    /// Отсечка слабых карточек запроса (0..=1); нет — `search.brief_min_rel` (M6 §4.2).
    /// Внутренний поиск подсказок `/recall` передаёт 0: у подсказок свой порог.
    brief_min_rel: Option<f64>,
    format: Option<OutFormat>,
    expand: Option<Expand>,
    rerank: Option<RerankMode>,
    /// Не повторять выданное этой сессии (по умолчанию — да, §9.1).
    skip_seen: Option<bool>,
    /// Сессия агента; иначе — `X-Zam-Session`.
    session_id: Option<String>,
    /// Пропустить шаг 0 — кэш выжимок (§9.3).
    #[serde(default)]
    no_cache: bool,
}

impl SearchSection {
    /// Параметры слияния `zam-search`.
    fn weights(&self) -> Weights {
        Weights {
            rrf_k: self.rrf_k,
            w_vector: self.w_vector,
            w_fts: self.w_fts,
            w_fts_relaxed: self.w_fts_relaxed,
            w_fts_alias: self.w_fts_alias,
            w_trigram: self.w_trigram,
            w_folder: self.w_folder,
            recency_half_life_days: self.recency_half_life_days,
            max_per_node: self.max_per_node,
            max_per_attachment: self.max_per_attachment,
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
/// цели (`note` заметки или записи журнала):
/// - с вектором запроса — ближайший по косинусу среди листов с эмбеддингом в кэше;
/// - иначе (или без эмбеддингов) — лист, где FTS находит больше всего слов запроса
///   ([`query_needles`]), при равенстве — первый; ни одного — сводка остаётся как есть
///   (как в M1).
///
/// Всё пакетно, без запроса на каждую сводку и без чтения текстов листов: совпадения
/// слов — по индексу FTS; при векторе — пути и ключи кэша (`chunks.embed_key`) листов
/// всех целей одним чтением, эмбеддинги листов-кандидатов — одним `get_embeddings`.
async fn best_leaves(
    db: &Db,
    summaries: &[&Ranked],
    needles: &[String],
    query: Option<(&[f32], &str)>,
) -> Result<HashMap<String, LeafRef>, ApiError> {
    let mut targets: Vec<TargetRef> = summaries.iter().map(|r| r.target).collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let fits = |r: &Ranked, hp: &str| r.level == Level::Note || in_section(hp, &r.heading_path);
    let mut out: HashMap<String, LeafRef> = HashMap::new();
    if let Some((q, model)) = query {
        // пути и ключи кэша листов без текстов; эмбеддинги — только листов-кандидатов
        let leaves = leaves_of_targets(db, targets.clone(), false).await?;
        let mut need: Vec<String> = summaries
            .iter()
            .flat_map(|r| {
                leaves
                    .get(&r.target)
                    .into_iter()
                    .flatten()
                    .filter(|l| fits(r, &l.heading_path) && !l.embed_key.is_empty())
                    .map(|l| l.embed_key.clone())
            })
            .collect();
        need.sort();
        need.dedup();
        let embs = get_embeddings(db, model, &need).await?;
        for r in summaries {
            let best = leaves
                .get(&r.target)
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
        let hits = leaf_term_hits(db, targets, needles.to_vec()).await?;
        for r in summaries {
            if out.contains_key(&r.chunk_id) {
                continue;
            }
            // по цели и ordinal: при равном числе совпадений остаётся первый лист
            let mut best: Option<&LeafHit> = None;
            for h in hits
                .iter()
                .filter(|h| h.target == r.target && fits(r, &h.heading_path))
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
#[derive(Clone)]
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
    /// Сырой балл реранкера (0..1), если элемент переставлен реранком (`score` — он же с
    /// бустами, §9.2 п. 5).
    rerank_score: Option<f32>,
    /// Сосед `expand=graph`: тип ребра и родитель.
    via: Option<(zam_store::EdgeType, TargetRef)>,
    /// Карточка папки: число живых детей (см. модуль, «Папки»).
    folder: Option<i64>,
    /// Уже выдан этой сессии в той же ревизии (`skip_seen`, §9.3): без фрагмента и текста.
    seen: bool,
    /// Уже выдан, но фрагмент и текст оставлены (запрос MCP без явного `skip_seen: true`,
    /// M6 §7.1: субагенты на общем подключении): пометка «(выдавалось)» вместо `(seen)`.
    seen_kept: bool,
    /// Уже выдан, и текст цели — в действующей выжимке этого же ответа (M6 §7.1, решение
    /// контролёра 2026-11-06): без фрагмента, пометка «(выдавалось, в выжимке)».
    in_cache: bool,
    /// Пути устаревших ссылок цели на файлы (M4.5 §6.2); пусто — поля в ответе нет.
    stale_refs: Vec<String>,
}

impl Item {
    /// Выданное прячется: без фрагмента и текста (`seen` без `seen_kept`).
    fn hidden(&self) -> bool {
        self.seen && !self.seen_kept
    }
}

/// Текст пары реранкера: заголовок секции и текст чанка.
fn rerank_text(h: &FtsHit) -> String {
    if h.heading_path.is_empty() {
        h.text.clone()
    } else {
        format!("{}\n{}", h.heading_path, h.text)
    }
}

/// Не больше `max` элементов с одной цели (узла, записи) и `max_attachment` с документа
/// вложения ([`target_cap`]), порядок сохраняется.
fn cap_items(items: Vec<Item>, max: usize, max_attachment: usize) -> Vec<Item> {
    let mut seen: HashMap<TargetRef, usize> = HashMap::new();
    items
        .into_iter()
        .filter(|it| {
            let n = seen.entry(it.hit.target).or_insert(0);
            *n += 1;
            *n <= target_cap(it.hit.target.kind, max, max_attachment)
        })
        .collect()
}

/// `rerank=on`: первые `ml.rerank_top` элементов — по баллу реранкера с бустами `boosts`
/// (см. модуль). Сбой — элементы как были и предупреждение.
async fn rerank_items(
    st: &AppState,
    query: &str,
    items: Vec<Item>,
    boosts: &Boosts,
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
            let out = boost_reranked(items, &scores, boosts);
            // лимит на узел и документ вложения — и после перестановки
            let s = &st.cfg.search;
            (cap_items(out, s.max_per_node, s.max_per_attachment), None)
        }
        Err(w) => (items, Some(w)),
    }
}

/// Баллы реранкера `raw` первых элементов `items` с бустами (§9.2 п. 5): `score = rerank_score
/// × boost_kind × boost_area × boost_certainty`, порядок первых — по нему, затем деление при
/// переполнении (лучший — 1). `rerank_score` — сырой балл. Хвост (без балла реранкера) остаётся
/// после них со своими баллами слияния.
fn boost_reranked(items: Vec<Item>, raw: &[f32], boosts: &Boosts) -> Vec<Item> {
    let boosted: Vec<f64> = items
        .iter()
        .zip(raw)
        .map(|(it, &s)| match chunk_meta(&it.hit) {
            Some(m) => apply_boosts_to(f64::from(s), &m, boosts, None),
            None => f64::from(s),
        })
        .collect();
    let with_raw: Vec<(Item, Option<f32>)> = items
        .into_iter()
        .enumerate()
        .map(|(i, it)| (it, raw.get(i).copied()))
        .collect();
    let mut out: Vec<Item> = rerank::reorder(with_raw, &boosted)
        .into_iter()
        .map(|((mut it, raw), boosted)| {
            if let (Some(r), Some(b)) = (raw, boosted) {
                it.rerank_score = Some(r);
                it.score = b;
            }
            it
        })
        .collect();
    let top = overflow_divisor(out.first().map_or(0.0, |it| it.score));
    for it in out.iter_mut().filter(|it| it.rerank_score.is_some()) {
        it.score /= top;
    }
    out
}

/// Метаданные бустов чанка выдачи (§9.2 п. 4): цель, уровень, вид инсайта, `certainty`,
/// область. Неизвестный уровень — `None` (кандидат вне слияния).
fn chunk_meta(h: &FtsHit) -> Option<ChunkMeta> {
    Some(ChunkMeta {
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
    })
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

/// Секции одной ревизии заметки для `expand=section`: собственные диапазоны секций H1–H3 и
/// преамбулы (заголовок и текст до первой подсекции) с путями. Строится одним разбором
/// исходника ([`SectionTable::build`]) на `(узел, ревизия)` за запрос — см. [`SectionCache`].
struct SectionTable {
    src: String,
    secs: Vec<SecRow>,
}

/// Секция в [`SectionTable`].
struct SecRow {
    /// Начало секции в исходнике — ключ слияния.
    start: usize,
    /// Собственный текст секции (без хвостовых пробелов).
    own: Range<usize>,
    /// Путь через ` > `; у преамбулы — пустой.
    path: String,
    /// Число заголовков в пути; 0 — преамбула.
    depth: usize,
}

/// Тестовая точка отказа: разбор исходника с этой строкой паникует (проверка деградации
/// `expand=section` при панике в `spawn_blocking`).
#[cfg(test)]
const PANIC_BUILD_MARK: &str = "\u{1}zam-test-panic-section-build\u{1}";

impl SectionTable {
    /// Разбор исходника (CPU, до `note_max_bytes`): вызывать вне async-потока.
    fn build(src: String) -> Self {
        #[cfg(test)]
        assert!(
            !src.contains(PANIC_BUILD_MARK),
            "тестовая паника разбора: {}",
            &src[..src.len().min(256)]
        );
        let doc = zam_markdown::parse(&src);
        let secs = (0..doc.sections.len())
            .filter_map(|i| {
                let s = &doc.sections[i];
                let end = doc
                    .sections
                    .get(i + 1)
                    .map_or(s.range.end, |n| n.range.start.min(s.range.end))
                    .max(s.body.start.min(s.range.end));
                let t = src.get(s.range.start..end)?;
                let preamble = s.level == 0;
                Some(SecRow {
                    start: s.range.start,
                    own: s.range.start..s.range.start + t.trim_end().len(),
                    path: if preamble {
                        String::new()
                    } else {
                        s.path.join(" > ")
                    },
                    depth: if preamble { 0 } else { s.path.len() },
                })
            })
            .collect();
        Self { src, secs }
    }

    fn own(&self, r: &SecRow) -> &str {
        &self.src[r.own.clone()]
    }

    /// Секция листа: начало секции H1–H3 (или преамбулы) — ключ слияния — и её собственный
    /// диапазон.
    ///
    /// С задачи 17 путь листа включает H4–H6 (`A > B > Глубокий`), а секции `Doc` — только
    /// H1–H3, поэтому кандидаты — секции, чей путь является префиксом `heading_path` по
    /// компонентам (` > `); преамбула — префикс любого пути. Секция определяется по месту
    /// листа: кандидат, содержащий самую длинную (наиболее специфичную) строку листа; если
    /// таких несколько — с наибольшей долей строк листа, при равенстве — самый глубокий, затем
    /// первый. Собственные тексты секций не пересекаются, поэтому так находится секция, где
    /// лист действительно лежит, — и при повторяющихся путях, и при « > » в тексте заголовка.
    /// Короткие однотипные строки (```` ``` ````, `---`, `</details>`) секцию не решают.
    /// Дословно повторяющийся текст в одноимённой более глубокой секции (лист под
    /// `# A > #### X` и такой же текст в `## X` под `# A`) отдаёт более глубокую — известное
    /// ограничение. Лист не найден в тексте (исходник другой ревизии; строка листа с маской
    /// секрета — листы режутся по замаскированному исходнику, а здесь исходник ревизии) —
    /// запасной вариант: самый длинный префикс пути, первый из равных. `None` — подходящей
    /// секции нет.
    ///
    /// Стоимость: один проход по собственным текстам кандидатов (вместе — не больше
    /// исходника); доля строк — только при нескольких кандидатах со строкой. Вызывать вне
    /// async-потока: на заметке в 5 МБ проход — миллисекунды.
    fn locate(&self, heading_path: &str, leaf: &str) -> Option<(usize, Range<usize>)> {
        let cands: Vec<&SecRow> = self
            .secs
            .iter()
            .filter(|r| {
                r.depth == 0
                    || heading_path == r.path
                    || heading_path
                        .strip_prefix(r.path.as_str())
                        .is_some_and(|rest| rest.starts_with(" > "))
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
            let holders: Vec<&SecRow> = cands
                .iter()
                .copied()
                .filter(|r| self.own(r).contains(key))
                .collect();
            if holders.len() <= 1 {
                return holders.first().copied();
            }
            let mut best: Option<(&SecRow, usize)> = None;
            for r in holders {
                let t = self.own(r);
                let n = lines.iter().filter(|l| t.contains(*l)).count();
                if best.is_none_or(|(b, m)| (n, r.depth) > (m, b.depth)) {
                    best = Some((r, n));
                }
            }
            best.map(|(r, _)| r)
        });
        // запасной вариант: самый длинный префикс пути (преамбула — только для пустого пути)
        let by_path = || {
            let depth = usize::from(!heading_path.is_empty());
            cands
                .iter()
                .copied()
                .filter(|r| r.depth >= depth)
                .fold(None::<&SecRow>, |b, r| match b {
                    Some(b) if b.depth >= r.depth => Some(b),
                    _ => Some(r),
                })
        };
        let r = by_leaf.or_else(by_path)?;
        Some((r.start, r.own.clone()))
    }
}

/// Таблицы секций за один запрос `expand=section`: исходник ревизии читается и разбирается
/// один раз на `(цель поиска, узел, ревизия)`, сколько бы листов этой заметки ни было в окне.
/// Индекс цели в ключе: один и тот же `(узел, ревизия)` может оказаться в двух базах (копия
/// базы под другим пользователем). `None` — разбор прерван паникой, элементы узла — без секции.
#[derive(Default)]
struct SectionCache {
    tables: HashMap<(usize, Id, i64), Option<Arc<SectionTable>>>,
    /// Сколько раз разобран исходник.
    #[cfg(test)]
    builds: usize,
}

/// Разбор или поиск секции прерван (паника в `spawn_blocking`): WARN в журнал и одно
/// предупреждение ответа; элемент отдаётся с текстом листа. В журнал — только признаки
/// `panic`/`cancelled`, без `Display` у `JoinError`: сообщение паники (например, среза строки)
/// может содержать фрагмент исходника заметки, в том числе незамаскированный секрет.
fn section_failed(health: &mut Health<'_>, what: &str, e: &tokio::task::JoinError) {
    tracing::warn!(
        panic = e.is_panic(),
        cancelled = e.is_cancelled(),
        "expand=section: {what} прерван, элемент без секции"
    );
    if !health
        .warnings
        .iter()
        .any(|w| w == WARN_SECTION_UNAVAILABLE)
    {
        health.warnings.push(WARN_SECTION_UNAVAILABLE.into());
    }
}

/// Секция листа по исходнику `src` (разбор на каждый вызов — для тестов).
#[cfg(test)]
fn leaf_section<'a>(src: &'a str, heading_path: &str, leaf: &str) -> Option<(usize, &'a str)> {
    let t = SectionTable::build(src.to_string());
    let (at, own) = t.locate(heading_path, leaf)?;
    Some((at, &src[own]))
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
///
/// Исходник ревизии читается и разбирается один раз на `(цель, узел, ревизия)` (`cache`),
/// разбор и поиск секции листа — в `spawn_blocking`; паника там — элемент без секции и
/// предупреждение `section unavailable`, а не 500 на весь запрос.
async fn expand_items(
    health: &mut Health<'_>,
    items: Vec<Item>,
    how: Expand,
    top_k: usize,
    cache: &mut SectionCache,
) -> Result<Vec<Item>, ApiError> {
    let targets = health.targets;
    let mut out: Vec<Item> = Vec::with_capacity(top_k);
    let mut sections: HashMap<(usize, TargetRef, SectionKey), usize> = HashMap::new();
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
                let ck = (ti, it.hit.target.id, it.hit.rev);
                let table = match cache.tables.get(&ck) {
                    Some(t) => t.clone(),
                    None => {
                        let r = node_content(db, it.hit.target.id, Some(it.hit.rev)).await;
                        let Some((_, s)) = health.soft(ti, r)? else {
                            drop_target(&mut out, &mut sections, ti);
                            continue;
                        };
                        let t = match tokio::task::spawn_blocking(move || SectionTable::build(s))
                            .await
                        {
                            Ok(t) => Some(Arc::new(t)),
                            Err(e) => {
                                section_failed(health, "разбор заметки", &e);
                                None
                            }
                        };
                        #[cfg(test)]
                        {
                            cache.builds += 1;
                        }
                        cache.tables.insert(ck, t.clone());
                        t
                    }
                };
                let sec = match &table {
                    Some(t) => {
                        let (t, hp, leaf) = (t.clone(), it.hit.heading_path.clone(), base.clone());
                        match tokio::task::spawn_blocking(move || t.locate(&hp, &leaf)).await {
                            Ok(sec) => sec,
                            Err(e) => {
                                section_failed(health, "поиск секции", &e);
                                None
                            }
                        }
                    }
                    None => None,
                };
                let key = (
                    ti,
                    it.hit.target,
                    sec.as_ref().map_or_else(
                        || SectionKey::Path(it.hit.heading_path.clone()),
                        |(at, _)| SectionKey::At(*at),
                    ),
                );
                if let Some(&i) = sections.get(&key) {
                    let ids = std::mem::take(&mut it.chunk_ids);
                    out[i].chunk_ids.extend(ids);
                    continue;
                }
                sections.insert(key, out.len());
                // склеенные чанкером короткие секции: лист может быть длиннее секции
                match sec.zip(table.as_deref()).map(|((_, own), t)| &t.src[own]) {
                    Some(sec) if sec.chars().count() >= base.chars().count() => sec.to_string(),
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
    sections: &mut HashMap<(usize, TargetRef, SectionKey), usize>,
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

/// Строка карточки без текста: по ней считается стоимость в токенах. У вложения L0 нет — в
/// `notes` тоже сниппет.
fn card_summary(it: &Item, mode: SearchMode) -> &str {
    if it.hidden() {
        return "";
    }
    match mode {
        SearchMode::Notes if it.hit.attachment.is_none() => it.l0.as_deref().unwrap_or(""),
        _ => &it.hit.snippet,
    }
}

/// Строки деградации и предупреждений в конце Markdown.
fn md_tail(degraded: Option<DegradedReason>, warnings: &[String]) -> String {
    let mut s = String::new();
    if let Some(r) = degraded {
        s.push_str(&format!(
            "> degraded: {} — {} (только FTS)\n",
            r.as_str(),
            r.text()
        ));
    }
    for w in warnings {
        s.push_str(&format!("> warning: {w}\n"));
    }
    s
}

/// Markdown ответа: выжимки шага 0, карточки по порядку и строки [`md_tail`].
fn md_body(
    cache_md: &str,
    packed: &[Packed],
    mode: SearchMode,
    spaces: &HashMap<Id, Space>,
    tail: &str,
) -> String {
    let mut body = cache_md.to_string();
    body.extend(
        packed
            .iter()
            .enumerate()
            .map(|(i, p)| item_md(i + 1, p, mode, spaces)),
    );
    body.push_str(tail);
    body
}

/// Фрагмент или L0 карточки, урезанный до `room` токенов карточки целиком (по границе символа,
/// незакрытый маркер совпадения закрывается, в конце — `…`). Стоимость считается по префиксу
/// с закрытием, но `truncate_to_tokens` ещё отступает к границе слова; на стыке счёт токенизатора
/// неаддитивен, так что первая карточка может выйти на 1 токен дороже `room` — только она:
/// следующие берутся целиком или не берутся, а итог проверяет цикл по Markdown целиком.
fn cut_summary(full: &str, cost: &dyn Fn(&str) -> usize, room: usize) -> String {
    let close = |t: &str| -> String {
        let opened = t.matches(MARK_START).count() > t.matches(MARK_END).count();
        let mut t = t.to_string();
        if opened {
            t.push(MARK_END);
        }
        if !t.is_empty() {
            t.push('…');
        }
        t
    };
    let cut = truncate_to_tokens(full, room, &|pre: &str| cost(&close(pre)));
    close(cut)
}

/// Упаковка в бюджет (M6 §4.2): стоимость элемента — **вся** его карточка Markdown
/// ([`item_md`]: номер, id, путь, секция, хвост, пометки, фрагмент или текст) счётчиком
/// `count`; элементы берутся по порядку, пока помещаются. Первая карточка отдаётся всегда:
/// не помещается — урезается её фрагмент (L0) или текст (`chunks`), вплоть до пустого. У
/// следующей в `chunks` урезается текст до остатка, в `brief`/`notes` — стоп.
fn pack_items(
    items: Vec<Item>,
    mode: SearchMode,
    budget_tokens: usize,
    count: &dyn Fn(&str) -> usize,
    spaces: &HashMap<Id, Space>,
) -> Vec<Packed> {
    let mut out: Vec<Packed> = Vec::new();
    let mut used = 0usize;
    for mut it in items {
        let n = out.len() + 1;
        let text = it.text.take();
        let mut p = Packed {
            tokens: text
                .as_ref()
                .map_or(it.hit.token_count.max(0) as usize, |t| t.1),
            text: text.map(|(t, _)| (t, false)),
            item: it,
        };
        let cost = count(&item_md(n, &p, mode, spaces));
        if used + cost <= budget_tokens {
            used += cost;
            out.push(p);
            continue;
        }
        let room = budget_tokens.saturating_sub(used);
        let first = out.is_empty();
        match p.text.take() {
            // текст `chunks`: урезается до остатка (у не первой — если остаток есть)
            Some((full, _)) if first || room > 0 => {
                let render = |t: &str| -> usize {
                    let q = Packed {
                        item: p.item.clone(),
                        text: Some((t.to_string(), true)),
                        tokens: count(t),
                    };
                    count(&item_md(n, &q, mode, spaces))
                };
                let cut = truncate_to_tokens(&full, room, &render).to_string();
                if !first && cut.is_empty() {
                    break;
                }
                p.tokens = count(&cut);
                p.text = Some((cut, true));
                out.push(p);
            }
            Some(_) => {}
            // `brief`/`notes`: первая карточка — с урезанным фрагментом
            None if first => {
                let notes_l0 = mode == SearchMode::Notes && p.item.hit.attachment.is_none();
                let full = card_summary(&p.item, mode).to_string();
                let render = |sum: &str| -> usize {
                    let mut q = Packed {
                        item: p.item.clone(),
                        text: None,
                        tokens: p.tokens,
                    };
                    if notes_l0 {
                        q.item.l0 = Some(sum.to_string());
                    } else {
                        q.item.hit.snippet = sum.to_string();
                    }
                    count(&item_md(n, &q, mode, spaces))
                };
                let cut = cut_summary(&full, &render, room);
                if notes_l0 {
                    p.item.l0 = Some(cut);
                } else {
                    p.item.hit.snippet = cut;
                }
                out.push(p);
            }
            None => {}
        }
        break;
    }
    out
}

/// Отсечка слабых карточек `brief`/`notes` (M6 §4.2, `search.brief_min_rel`): балл ниже
/// `min_rel × лучший` — вон; соседи `expand=graph` (явный запрос) не отсекаются.
fn drop_weak(items: &mut Vec<Item>, mode: SearchMode, min_rel: f64) {
    if min_rel <= 0.0 || !matches!(mode, SearchMode::Brief | SearchMode::Notes) {
        return;
    }
    let best = items.iter().map(|it| it.score).fold(f64::MIN, f64::max);
    if !best.is_finite() {
        return;
    }
    items.retain(|it| it.via.is_some() || it.score >= min_rel * best);
}

/// Балл ответа: 0..1 с точностью 1e-4. Переполнение после бустов и канала папок уже снято
/// [`rescale_overflow`] (деление на лучший); `min(1.0)` — страховка для баллов вне слияния
/// (реранк, соседи графа).
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

/// Подпись `locator` фрагмента вложения (`стр. 12`, `лист «Итоги» A1:F40`, …) со сканером
/// секретов (имя листа — текст документа, как путь заголовков); `None` — без `locator` или он не
/// разбирается.
fn locator_text(a: &AttachmentHit) -> Option<String> {
    a.locator
        .as_deref()
        .and_then(|l| zam_docs::Locator::parse(l).ok())
        .map(|l| doc_locator::shown(&l, a.sheet_no).1)
}

/// `locator` фрагмента для карточки — со сканером секретов (имя листа); лист с секретом в имени —
/// `sheet:#N!…` ([`doc_locator::shown`]): такой `locator` читается `/read`. Сырой `locator` наружу
/// не нужен: `/read a:<chunk>` (задача 7) берёт его из чанка по `chunk_id`.
fn shown_locator(a: &AttachmentHit) -> Option<String> {
    a.locator
        .as_deref()
        .map(|raw| match zam_docs::Locator::parse(raw) {
            Ok(l) => doc_locator::shown(&l, a.sheet_no).0,
            Err(_) => zam_markdown::mask_text(raw),
        })
}

fn item_json(p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>, now: Timestamp) -> Value {
    if let Some(children) = p.item.folder {
        return folder_json(p, children, mode, spaces);
    }
    let h = &p.item.hit;
    let mut o = Map::new();
    match (h.target.kind, &h.attachment) {
        // фрагмент вложения: ссылка `a:<chunk>`, документ, файл, `locator`; путь и `node_id` —
        // заметки-владельца
        (TargetKind::Attachment, Some(a)) => {
            o.insert("id".into(), json!(chunk_ref(&h.chunk_id)));
            o.insert("node_id".into(), json!(a.node_id));
            o.insert("attachment_id".into(), json!(a.id));
            o.insert("file".into(), json!(a.filename));
            o.insert("blob_sha256".into(), json!(a.blob_sha256));
            o.insert("locator".into(), json!(shown_locator(a)));
            o.insert("locator_text".into(), json!(locator_text(a)));
        }
        (TargetKind::Node, _) => {
            o.insert("node_id".into(), json!(h.target.id));
        }
        (TargetKind::Entry, _) => {
            o.insert("entry_id".into(), json!(h.target.id));
        }
        // хит вложения всегда несёт поля документа (`zam_store::search`)
        (TargetKind::Attachment, None) => {
            o.insert("attachment_id".into(), json!(h.target.id));
        }
    }
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
    o.insert("age_days".into(), json!(hit_age(h, now)));
    match &h.entry {
        Some(e) => {
            o.insert("kind".into(), json!(e.kind));
            o.insert("status".into(), json!(e.status));
            o.insert("insight_kind".into(), json!(e.insight_kind));
            o.insert("certainty".into(), json!(e.certainty));
            o.insert("title".into(), json!(e.title));
            // карточка отчёта (только WebUI, M5.5 §7): ссылка `p:<id>` и дата создания
            if e.kind == REPORT {
                o.insert("id".into(), json!(format!("p:{}", h.target.id)));
                o.insert("created_at".into(), json!(fmt_ts(e.created_at)));
            }
        }
        None => {
            let kind = match h.target.kind {
                TargetKind::Attachment => "attachment",
                TargetKind::Node | TargetKind::Entry => "note",
            };
            o.insert("kind".into(), json!(kind));
            o.insert("certainty".into(), json!(h.certainty));
        }
    }
    o.insert("seen".into(), json!(p.item.seen));
    if p.item.in_cache {
        o.insert("in_cache".into(), json!(true));
    }
    if !p.item.stale_refs.is_empty() {
        o.insert("stale_refs".into(), json!(p.item.stale_refs));
    }
    // у вложения L0 нет: и в `notes` — сниппет
    if mode == SearchMode::Notes && h.attachment.is_none() {
        o.insert("l0".into(), json!(p.item.l0));
    } else if !p.item.hidden() {
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

/// Возраст элемента в днях (M4.5 §7): от `updated_at`, у заметки с `verified_at` — от более
/// позднего из двух.
fn hit_age(h: &FtsHit, now: Timestamp) -> u32 {
    zam_store::time::age_days(now, h.updated_at, h.verified_at.as_deref())
}

/// Балл карточки v2 (M6 §4.1): два знака без ведущего нуля — `.82`; единица — `1.0`.
pub(crate) fn score_text(score: f64) -> String {
    let t = format!("{:.2}", score.clamp(0.0, 1.0));
    match t.strip_prefix('0') {
        Some(rest) => rest.to_string(),
        None => "1.0".into(),
    }
}

/// Хвост карточки v2 (M6 §4.1): `[.82] r7 · th · 2026-10-10 14:03:22 · 340t` — балл, ревизия
/// (`rev = 0` — без неё: записи журнала, вложения), надёжность, статус записи (только не
/// `active`), дата обновления UTC `yyyy-MM-dd HH:mm:ss`, размер в токенах. Возраста «N дн.
/// назад» нет: строка детерминирована, `age_days` остаётся в JSON.
pub(crate) fn card_tail(
    score: f64,
    rev: i64,
    updated: Timestamp,
    tokens: usize,
    certainty: Option<&str>,
    status: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if rev > 0 {
        parts.push(format!("r{rev}"));
    }
    if let Some(c) = certainty.filter(|c| !c.is_empty()) {
        parts.push(zam_core::certainty_short(c).to_string());
    }
    if let Some(st) = status.filter(|st| !st.is_empty() && *st != "active") {
        parts.push(st.to_string());
    }
    parts.push(zam_core::time::agent_ts(&fmt_ts(updated)).unwrap_or_default());
    parts.push(format!("{tokens}t"));
    format!("[{}] {}", score_text(score), parts.join(" · "))
}

/// Пометка Markdown об устаревших ссылках (M4.5 §6.2):
/// `возможно устарело: изменился <путь>[, <путь>…]`; пусто — `None`.
pub(crate) fn stale_line(paths: &[String]) -> Option<String> {
    (!paths.is_empty()).then(|| {
        format!(
            "возможно устарело: изменился {}",
            paths
                .iter()
                .map(|p| md_inline(p))
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Одна строка: маркеры → `**`, переводы строк → пробелы.
pub(crate) fn md_inline(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\r' | '\n' => ' ',
            c => c,
        })
        .collect::<String>()
        .replace([MARK_START, MARK_END], "**")
}

/// Заголовок карточки записи журнала (§9.3, M6 §4.1):
/// `i:<id> insight/<вид> «<заголовок>»  [.77] hyp · 2026-09-28 08:00:01 · 120t`.
fn entry_card(
    e: &zam_store::EntryHit,
    id: Id,
    score: f64,
    updated: Timestamp,
    tokens: usize,
) -> String {
    let letter = EntryKind::parse(&e.kind).map_or("e", EntryKind::ref_letter);
    let mut line = format!("{letter}:{id} {}", e.kind);
    if let Some(k) = &e.insight_kind {
        line.push('/');
        line.push_str(&md_inline(k));
    }
    line.push_str(&format!(" «{}»", md_inline(&e.title)));
    let tail = card_tail(
        score,
        0,
        updated,
        tokens,
        e.certainty.as_deref(),
        e.status.as_deref(),
    );
    line.push_str(&format!("  {tail}\n"));
    line
}

/// Заголовок карточки фрагмента вложения (без номера): `a:<chunk> <space>/<путь заметки> › <файл>
/// › <путь заголовков> · стр. 12  [.82] th · 2026-10-10 14:03:22 · 120t`.
fn attachment_card(
    h: &FtsHit,
    a: &AttachmentHit,
    space: &str,
    score: f64,
    tokens: usize,
) -> String {
    let mut line = format!(
        "{} {space}/{} › {}",
        chunk_ref(&h.chunk_id),
        h.path,
        md_inline(&a.filename)
    );
    if !h.heading_path.is_empty() {
        line.push_str(" › ");
        line.push_str(&md_inline(&h.heading_path));
    }
    if let Some(l) = locator_text(a) {
        line.push_str(" · ");
        line.push_str(&md_inline(&l));
    }
    let tail = card_tail(score, 0, h.updated_at, tokens, h.certainty.as_deref(), None);
    line.push_str(&format!("  {tail}\n"));
    line
}

/// Карточка §9.3, M6 §4.1: `1. n:<id> <space>/<path> › <секция>  [.82] r7 · 2026-10-10 14:03:22
/// · 340t` (у записи журнала — [`entry_card`], у вложения — [`attachment_card`]).
fn item_md(n: usize, p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> String {
    let h = &p.item.hit;
    // у переставленных реранкером `score` — его балл с бустами (§9.2 п. 5)
    let score = p.item.score.min(1.0);
    let mut line = match (&h.entry, p.item.folder) {
        // карточка папки: `f:<id> <пространство>/<путь>/  [.61] 4 детей`
        (_, Some(children)) => {
            let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
            format!(
                "{n}. f:{} {space}/{}/  [{}] {children} детей\n",
                h.target.id,
                h.path,
                score_text(score)
            )
        }
        (Some(e), None) => format!(
            "{n}. {}",
            entry_card(e, h.target.id, score, h.updated_at, p.tokens)
        ),
        (None, None) if h.attachment.is_some() => {
            let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
            let a = h.attachment.as_ref().expect("проверено выше");
            format!("{n}. {}", attachment_card(h, a, space, score, p.tokens))
        }
        (None, None) => {
            let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
            let mut line = format!("{n}. {} {space}/{}", h.target, h.path);
            if !h.heading_path.is_empty() {
                line.push_str(" › ");
                line.push_str(&h.heading_path);
            }
            let tail = card_tail(
                score,
                h.rev,
                h.updated_at,
                p.tokens,
                h.certainty.as_deref(),
                None,
            );
            line.push_str(&format!("  {tail}\n"));
            line
        }
    };
    if let Some((t, from)) = &p.item.via {
        if line.ends_with('\n') {
            line.pop();
        }
        line.push_str(&format!(" · via {} ← {from}\n", t.as_str()));
    }
    // выдан этой сессии в той же ревизии: пометка вместо фрагмента (§9.3); у MCP — пометка
    // при фрагменте (M6 §7.1)
    if p.item.seen {
        if line.ends_with('\n') {
            line.pop();
        }
        line.push_str(if p.item.in_cache {
            "   (выдавалось, в выжимке)\n"
        } else if p.item.seen_kept {
            "   (выдавалось)\n"
        } else {
            "   (seen)\n"
        });
    }
    // устаревшие ссылки на файлы — строкой под заголовком карточки (M4.5 §6.2)
    if let Some(s) = stale_line(&p.item.stale_refs) {
        line.push_str("   ");
        line.push_str(&s);
        line.push('\n');
    }
    if p.item.hidden() {
        return line;
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

/// `stale_refs` элементов выдачи: один запрос на базу цели. Сбой чтения — WARN без текста,
/// элементы остаются без пометки.
async fn attach_stale_refs(targets: &[SearchTarget], items: &mut [Item]) {
    for (i, t) in targets.iter().enumerate() {
        // ссылок на файлы у документа вложения нет
        let owners: Vec<TargetRef> = items
            .iter()
            .filter(|it| {
                it.target == i
                    && it.folder.is_none()
                    && it.hit.target.kind != TargetKind::Attachment
            })
            .map(|it| it.hit.target)
            .collect();
        if owners.is_empty() {
            continue;
        }
        match zam_store::stale_refs_of(&t.db, &owners).await {
            Ok(by) => {
                for it in items.iter_mut().filter(|it| it.target == i) {
                    if let Some(p) = by.get(&it.hit.target) {
                        it.stale_refs = p.clone();
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "чтение устаревших ссылок на файлы"),
        }
    }
}

/// Номер листа ([`zam_store::AttachmentHit::sheet_no`]) у фрагментов листов, имя которых меняет
/// сканер секретов: у двух таких листов одна маска, и карточка без номера не позволила бы ни
/// различить их, ни прочитать (`/read` ищет лист по сырому имени или номеру). Номер считается
/// так же, как в `/read` ([`Sheets`]), по одному запросу `doc_sheet_locators` на документ и только
/// для таких фрагментов. Сбой чтения — WARN, карточка остаётся с маской.
async fn number_secret_sheets(targets: &[SearchTarget], items: &mut [Item]) {
    let mut cache: HashMap<(usize, Id), Option<Sheets>> = HashMap::new();
    for it in items.iter_mut() {
        let Some(a) = it.hit.attachment.as_mut() else {
            continue;
        };
        let Some(Ok(l)) = a.locator.as_deref().map(zam_docs::Locator::parse) else {
            continue;
        };
        if !secret_sheet(&l) {
            continue;
        }
        let key = (it.target, a.id);
        let sheets = match cache.entry(key) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let db = &targets[it.target].db;
                v.insert(match zam_store::doc_sheet_locators(db, a.id).await {
                    Ok(list) => Some(Sheets::of(list.iter().map(String::as_str))),
                    Err(e) => {
                        tracing::warn!(error = %e, "поиск: номера листов вложения не прочитаны");
                        None
                    }
                })
            }
        };
        if let (Some(sheets), zam_docs::Locator::Sheet { name, .. }) = (sheets.as_ref(), &l) {
            a.sheet_no = sheets.no(name);
        }
    }
}

/// Предупреждение ответа: текст секции (`expand=section`) не получен — разбор или поиск
/// секции прерван паникой; элемент отдан с текстом листа.
const WARN_SECTION_UNAVAILABLE: &str = "section unavailable";

/// Состояние целей запроса: какие ещё в выдаче, и предупреждения ответа.
pub(crate) struct Health<'a> {
    targets: &'a [SearchTarget],
    /// Цель не исключена сбоем чтения своей базы.
    pub(crate) alive: Vec<bool>,
    pub(crate) warnings: Vec<String>,
}

impl<'a> Health<'a> {
    pub(crate) fn new(targets: &'a [SearchTarget], warnings: Vec<String>) -> Health<'a> {
        Health {
            targets,
            alive: vec![true; targets.len()],
            warnings,
        }
    }

    /// Цели запроса.
    pub(crate) fn targets(&self) -> &'a [SearchTarget] {
        self.targets
    }

    /// Результат чтения базы цели `i`: сбой в базе чужого решения исключает решение (WARN
    /// в журнал, `solution unavailable`, цель больше не жива) и даёт `None`; сбой личной
    /// базы — ошибка запроса.
    pub(crate) fn soft<T, E>(&mut self, i: usize, r: Result<T, E>) -> Result<Option<T>, ApiError>
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
            verified_at: None,
            attachment: None,
        },
        score,
        sources: vec![zam_search::Channel::Folder.as_str()],
        chunk_ids: Vec::new(),
        l0: f.l0.clone(),
        text: None,
        rerank_score: None,
        via: None,
        folder: Some(f.child_count),
        seen: false,
        seen_kept: false,
        in_cache: false,
        stale_refs: Vec::new(),
    }
}

/// Ключ выданного: владелец базы цели элемента, цель и часть — `chunk_id` или
/// [`WHOLE_TARGET`] (решение контроллера R11). Фрагмент вложения — цель-блоб по sha256 (M5,
/// задача 7: один файл в двух заметках — одна цель), часть — `chunk_id` или `locator`,
/// прочитанный через `/read` целиком.
fn seen_key(targets: &[SearchTarget], it: &Item, part: &str) -> SeenKey {
    let target = match &it.hit.attachment {
        Some(a) => SeenTarget::Blob(a.blob_sha256.clone()),
        None => it.hit.target.into(),
    };
    SeenKey {
        owner_id: targets[it.target].owner,
        target,
        part: part.to_string(),
    }
}

/// Элемент учитывается в `session_seen`: не карточка папки.
fn seen_tracked(it: &Item) -> bool {
    it.folder.is_none()
}

/// Помечает элементы, уже выданные сессии в той же ревизии (`seen`), и снимает их текст:
/// выдана цель целиком, все чанки элемента (`chunk_ids`) или — у вложения — его `locator`
/// (`/read` отдал единицу документа целиком). `keep` — пометить, но оставить фрагмент и текст
/// (`seen_kept`, MCP без явного `skip_seen`, M6 §7.1).
async fn mark_seen_items(
    s: &RequestSession,
    targets: &[SearchTarget],
    items: &mut [Item],
    keep: bool,
) -> Result<(), StoreError> {
    let locator = |it: &Item| it.hit.attachment.as_ref().and_then(|a| a.locator.clone());
    let parts = |it: &Item| -> Vec<SeenKey> {
        std::iter::once(WHOLE_TARGET.to_string())
            .chain(locator(it))
            .chain(it.chunk_ids.iter().cloned())
            .map(|part| seen_key(targets, it, &part))
            .collect()
    };
    let keys: Vec<SeenKey> = items
        .iter()
        .filter(|it| seen_tracked(it))
        .flat_map(parts)
        .collect();
    let revs = seen_revs(&s.db, &s.touch.id, s.touch.epoch, &keys).await?;
    let same = |k: &SeenKey, rev: i64| revs.get(k) == Some(&rev);
    for it in items.iter_mut().filter(|it| seen_tracked(it)) {
        let rev = it.hit.rev;
        let whole = same(&seen_key(targets, it, WHOLE_TARGET), rev)
            || locator(it).is_some_and(|l| same(&seen_key(targets, it, &l), rev));
        let chunks = !it.chunk_ids.is_empty()
            && it
                .chunk_ids
                .iter()
                .all(|c| same(&seen_key(targets, it, c), rev));
        if whole || chunks {
            it.seen = true;
            it.seen_kept = keep;
            if !keep {
                it.text = None;
            }
        }
    }
    Ok(())
}

/// Карточки `(выдавалось)` (MCP, `seen_kept`), чья цель — источник действующей выжимки шага 0
/// этого ответа (`cache`), — без фрагмента и текста, `in_cache` (M6 §7.1, решение контролёра
/// 2026-11-06). Цель и ревизия совпадают с источником точно. Источник-секция покрывает карточку,
/// только если текст её чанка **целиком** лежит в тексте секции ([`zam_store::section_text`]
/// ревизии карточки): чанкер склеивает короткие секции со следующей и даёт склейке путь первой
/// (а лист H4 — путь первой H4), поэтому `heading_path` не доказывает, что в чанке нет текста
/// соседней секции (повторное ревью задачи 7a, I-A). Сбой чтения текста или любой промах —
/// фрагмент остаётся. Ожидаемо с фрагментом: заметка с секретами (чанки режутся по маскированному
/// тексту) и чанк с повторённым заголовком блока — текста чанка нет в тексте секции дословно.
async fn mark_cache_sources(
    targets: &[SearchTarget],
    cache: &[cache_step::CacheItem],
    items: &mut [Item],
) {
    let deps: Vec<&zam_store::CacheDep> = cache.iter().flat_map(|c| c.valid_sources()).collect();
    if deps.is_empty() {
        return;
    }
    // текст ревизии узла — один раз на (база, узел, ревизия)
    let mut texts: HashMap<(usize, Id, i64), Option<String>> = HashMap::new();
    for it in items.iter_mut().filter(|it| it.seen && it.seen_kept) {
        let mut covered = false;
        for d in deps
            .iter()
            .filter(|d| d.target == it.hit.target && d.rev == it.hit.rev)
        {
            let Some(sec) = d.section.as_deref() else {
                covered = true;
                break;
            };
            if it.hit.target.kind != TargetKind::Node {
                continue;
            }
            let key = (it.target, it.hit.target.id, it.hit.rev);
            if let std::collections::hash_map::Entry::Vacant(v) = texts.entry(key) {
                let db = &targets[it.target].db;
                let r = node_content(db, it.hit.target.id, Some(it.hit.rev)).await;
                v.insert(r.ok().map(|(_, t)| t));
            }
            let inside = texts[&key]
                .as_deref()
                .and_then(|t| zam_store::section_text(t, sec))
                .is_some_and(|st| !it.hit.text.is_empty() && st.contains(it.hit.text.as_str()));
            if inside {
                covered = true;
                break;
            }
        }
        if covered {
            it.seen_kept = false;
            it.in_cache = true;
            it.text = None;
        }
    }
}

/// Событие `session_tokens` поиска REST и CLI (M6 §8).
pub const SEARCH_TOKEN_EVENT: &str = "search";

/// Сбой учёта сессии не валит поиск: WARN (без id сессии и текста), ответ — без пометок.
fn session_failed(what: &str, e: &StoreError) {
    tracing::warn!(error = %e, "поиск: {what} сессии не выполнено, ответ без пометок seen");
}

/// Есть ли в векторных пространствах (без Human) целей извлечённый документ вложения. Сбой
/// чтения — `true` (prefetch вложений остаётся; WARN без текста).
async fn any_attachments(targets: &[SearchTarget]) -> bool {
    for t in targets {
        match zam_store::has_attachment_chunks(&t.db, t.vector_space_ids()).await {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "поиск: проверка вложений не удалась, prefetch остаётся");
                return true;
            }
        }
    }
    false
}

/// Расширения запроса словарём для цели `tg`; сбой чтения словаря поиск не валит (WARN без
/// текста запроса, расширения нет).
async fn alias_expansion(st: &AppState, tg: &SearchTarget, qterms: &[&str]) -> Vec<Expansion> {
    match st.glossary.index(&tg.db).await {
        Ok(idx) => idx.expand(qterms, &tg.space_ids()),
        Err(e) => {
            tracing::warn!(error = %e, "поиск: словарь не прочитан, запрос без расширения");
            Vec::new()
        }
    }
}

/// `POST /search`.
pub async fn search(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    origin: Option<Extension<McpOrigin>>,
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
    let budget = req.budget_tokens.unwrap_or(st.cfg.search.budget_tokens);
    let min_rel = req.brief_min_rel.unwrap_or(st.cfg.search.brief_min_rel);
    if !(min_rel.is_finite() && (0.0..=1.0).contains(&min_rel)) {
        return Err(ApiError::BadRequest("brief_min_rel: от 0 до 1".into()));
    }
    let mut kinds = target_filter(&req.filters, req.include_history, p.is_human())?;
    kinds.area = area_filter(req.scope.area.as_deref())?;
    let mut boosts = st.cfg.search.boosts(header_value(&headers, AREA_HEADER)?);
    let (targets, warnings) = resolve_targets(&st, &p, &headers, &req.scope).await?;
    // сессия: некорректный id — 400; сбой её базы поиск не валит (WARN, без пометок)
    let session = match seen_session(
        &st,
        &p,
        &headers,
        origin.as_deref(),
        req.session_id.as_deref(),
    )
    .await
    {
        // неявная сессия MCP (M6 §7.1) — как без сессии: без отметок и пометок
        Ok(s) => explicit_only(s),
        // некорректный id — 400, несуществующий пользователь (`X-Zam-User`) — 404
        Err(e @ (ApiError::BadRequest(_) | ApiError::NotFound)) => return Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "поиск: база сессии недоступна, ответ без пометок seen");
            None
        }
    };
    // `mode=chunks` касается сессии вместе с отметкой выданного (одна транзакция)
    if let Some(s) = &session
        && mode != SearchMode::Chunks
        && let Err(e) = touch_session(&s.db, &s.touch).await
    {
        session_failed("касание", &e);
    }
    let mut health = Health::new(&targets, warnings);
    let prefix = path_prefix(&req.scope);
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
    // расширение запроса словарём (канал `alias`, M4.5): слова запроса — по именам и
    // псевдонимам терминов пространств каждой цели; `w_fts_alias = 0` выключает канал
    let qterms = zam_store::query_terms(&req.query);
    let alias_on = st.cfg.search.w_fts_alias > 0.0 && !qterms.is_empty();
    let fts = async {
        let t = Instant::now();
        let runs = join_all(targets.iter().map(|tg| async {
            let exp = if alias_on {
                alias_expansion(&st, tg, &qterms).await
            } else {
                Vec::new()
            };
            let r = search_fts_channels(
                &tg.db,
                FtsQuery {
                    text: req.query.clone(),
                    space_ids: tg.space_ids(),
                    path_prefix: prefix.clone(),
                    top_k: TOP_K_MAX,
                    targets: kinds.clone(),
                    alias_groups: alias_groups(&exp),
                },
            )
            .await;
            (r, exp)
        }))
        .await;
        (runs, ms(t))
    };
    let vector = async {
        if !run_vector {
            return VectorRun::default();
        }
        let mut filter = vector_filter(
            scopes.clone(),
            prefix.clone(),
            &kinds,
            want_folders || folder_channel,
        );
        filter.attach_prefetch = Some(st.cfg.search.attach_prefetch);
        // prefetch вложений — только если в области есть извлечённые документы (одна проверка
        // EXISTS по базе цели: +1,8 мс к p95 без пользы, раунд правок 2 задачи 6)
        if zam_index::attachment_window(&filter) > 0 {
            filter.no_attachments = !any_attachments(&targets).await;
        }
        vector_channel(&st.index, filter, req.query.clone()).await
    };
    // заметки области всех целей, ещё не прошедшие векторную индексацию (без `index: false`
    // и Human), — снимок ДО векторного запроса: воркер отмечает `indexed` только после
    // подтверждённой записи точек в Qdrant, поэтому `pending_count == 0` гарантирует, что
    // вектор их видит (флейк шага 17 smoke: счёт после запроса давал 0 при FTS-only выдаче).
    // Сумма — по целям, живым к концу обработчика
    let pending_t = Instant::now();
    let pending_runs = join_all(
        targets
            .iter()
            .map(|t| pending_count(&t.db, t.space_ids(), prefix.clone())),
    )
    .await;
    let mut pending_by = vec![0; targets.len()];
    for (i, r) in pending_runs.into_iter().enumerate() {
        if health.alive[i] {
            pending_by[i] = health.soft(i, r)?.unwrap_or(0);
        }
    }
    let pending_ms = ms(pending_t);
    let ((runs, fts_ms), vrun) = tokio::join!(fts, vector);
    // тест задаёт порядок «вектор прочитан → воркер проиндексировал → ответ»
    #[cfg(feature = "test-hooks")]
    st.search_hold.park().await;
    // 0. кэш выжимок (§9.3): действующие выжимки — первыми элементами
    let cache_t = Instant::now();
    let cache_items = if req.no_cache {
        Vec::new()
    } else {
        cache_step::lookup(
            &st,
            &p,
            &headers,
            &req.scope,
            &kinds,
            &req.query,
            session.as_ref().map(|s| s.touch.id.as_str()),
            vrun.dense.as_deref(),
        )
        .await
    };
    let cache_ms = (!req.no_cache).then(|| ms(cache_t));
    // цель, чья база не прочиталась (чужое решение), исключается из выдачи целиком
    let mut per_db = Vec::with_capacity(targets.len());
    let mut expanded: Vec<Expansion> = Vec::new();
    for (i, (r, exp)) in runs.into_iter().enumerate() {
        // цель уже исключена сбоем подсчёта `pending_count`
        if !health.alive[i] {
            continue;
        }
        if let Some(ch) = health.soft(i, r)? {
            per_db.push((i, ch));
            for e in exp {
                if !expanded
                    .iter()
                    .any(|x| x.term == e.term && x.word == e.word)
                {
                    expanded.push(e);
                }
            }
        }
    }
    let fts = merge_channels(per_db, FTS_BM25_TOP, FTS_TRIGRAM_TOP);

    // 2. векторные хиты — в базу своего владельца, только живые чанки пространств цели
    let fuse_t = Instant::now();
    let mut hits: HashMap<String, (usize, FtsHit)> = HashMap::new();
    for (t, h) in fts.bm25.iter().chain(&fts.alias).chain(&fts.trigram) {
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
        .filter_map(|(id, (_, h))| Some((id.clone(), chunk_meta(h)?)))
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
        alias: fts.alias.iter().map(|(_, h)| h.chunk_id.clone()).collect(),
        alias_relaxed: fts.alias_relaxed,
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
    let (mut ranked, norm_max) = fuse_normed(&inputs, &weights, zam_store::time::now());
    // M6 §10: без буста вида — записи, которым противоречит более надёжная запись среди всех
    // кандидатов слияния (до `top_k`); тот же набор действует и на баллы реранкера
    if boosts.kind_mode == KindBoostMode::UnlessContradicted {
        let cands: Vec<graph::KindCandidate> = ranked
            .iter()
            .filter_map(|r| {
                let (i, h) = hits.get(&r.chunk_id)?;
                let kind = inputs.chunks.get(&r.chunk_id)?.insight_kind.as_deref();
                Some(graph::KindCandidate {
                    target: *i,
                    r: h.target,
                    boosted: boosts.kind_boosted(kind),
                })
            })
            .collect();
        if cands.iter().any(|c| c.boosted) {
            boosts.kind_exempt = graph::kind_boost_exempt(&mut health, &cands).await?;
        }
    }
    apply_boosts(&mut ranked, &inputs.chunks, &boosts);
    // балл выше 1 после бустов и канала папок: все баллы делятся на лучший (порядок прежний)
    let overflow = rescale_overflow(&mut ranked);
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
    let mut ranked = cap_per_node(ranked, weights.max_per_node, weights.max_per_attachment);
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
                seen: false,
                seen_kept: false,
                in_cache: false,
                stale_refs: Vec::new(),
            })
        })
        .collect();
    // карточки папок (только по `filters.kinds ∋ folder`): лучшие живые папки канала
    let cards: Vec<Item> = if want_folders {
        folders
            .iter()
            .take(st.cfg.search.max_folders)
            .map(|(i, f, rank)| {
                folder_card(*i, f, folder_score(*rank, &weights, norm_max) / overflow)
            })
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
        let (items, warning) = rerank_items(&st, &req.query, items, &boosts).await;
        rerank_ms = Some(ms(t));
        health.warnings.extend(warning.map(String::from));
        items
    } else {
        items
    };
    // порядок по `contradicts` — после реранка и бустов (M4.5, §5.2)
    let mut items = items;
    // только первые `top_k` элементов («обе в выдаче»): аксиома из хвоста окна в выдачу не поднимается
    if st.cfg.search.contradicts_order && top_k > 1 && items.len() > 1 {
        let tail = items.split_off(top_k.min(items.len()));
        let r = graph::order_by_contradicts(&mut health, &mut items).await;
        items.extend(tail);
        r?;
    }

    // 5. top_k, режим, expand, упаковка
    let expand_t = Instant::now();
    let mut items = match mode {
        SearchMode::Notes => group_by_node(items, top_k),
        SearchMode::Chunks => {
            let mut cache = SectionCache::default();
            expand_items(&mut health, items, expand, top_k, &mut cache).await?
        }
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
    // снимок `pending_count` (до векторного запроса) — только живых к этому моменту целей
    let pending: i64 = pending_by
        .iter()
        .enumerate()
        .filter(|(i, _)| health.alive[*i])
        .map(|(_, n)| n)
        .sum();
    let mut warnings = health.warnings;
    dedup_keep_order(&mut warnings);
    // выданное этой сессии в той же ревизии — без фрагмента и текста (§9.3)
    // MCP (M6 §7.1): субагенты идут с сессией родителя — без явного `skip_seen` фрагмент
    // остаётся, с пометкой «(выдавалось)»
    if let Some(s) = &session
        && req.skip_seen.unwrap_or(true)
        && let Err(e) = mark_seen_items(
            s,
            &targets,
            &mut items,
            s.via_mcp && req.skip_seen.is_none(),
        )
        .await
    {
        session_failed("чтение выданного", &e);
    }
    // источник выжимки этого ответа — без фрагмента (M6 §7.1, решение контролёра 2026-11-06)
    mark_cache_sources(&targets, &cache_items, &mut items).await;
    // устаревшие ссылки на файлы — пометкой в карточке (M4.5 §6.2), ранжирование не меняется
    attach_stale_refs(&targets, &mut items).await;
    // лист с секретом в имени — номером, как в `/read` (задача 7 M5, раунд правок 2)
    number_secret_sheets(&targets, &mut items).await;
    // слабые карточки `brief`/`notes` — вон (M6 §4.2, `search.brief_min_rel`)
    drop_weak(&mut items, mode, min_rel);
    let degraded = available.err().or(vrun.failed);
    let now = zam_store::time::now();
    let by_id: HashMap<Id, Space> = targets
        .iter()
        .flat_map(|t| t.spaces.iter().cloned())
        .map(|s| (s.id, s))
        .collect();
    // честный бюджет (M6 §4.2): Markdown целиком — выжимки шага 0, карточки, строки
    // деградации и предупреждений и строка MCP `~N tok` — счётчиком сервера; JSON — тот же отбор
    let tail_md = md_tail(degraded, &warnings);
    let cache_md: String = cache_items.iter().map(cache_step::item_md).collect();
    let reserve = count_tokens(&cache_md)
        + count_tokens(&tail_md)
        + count_tokens(&format!("~{budget} tok"))
        + 1;
    // упаковка суммами частей — против `budget_cap` (без модели — с запасом оценки), итог — `fits`
    let mut packed = pack_items(
        items,
        mode,
        zam_store::budget_cap(budget).saturating_sub(reserve),
        &count_tokens,
        &by_id,
    );
    // суммы частей и счёт целого могут разойтись на стыках: с конца снимаются карточки
    // Markdown ответа (он же — размер выдачи для учёта) считается один раз на итог отбора
    let mut md_text = md_body(&cache_md, &packed, mode, &by_id, &tail_md);
    while packed.len() > 1 {
        let n = count_tokens(&md_text);
        if zam_store::fits_budget(&format!("{md_text}~{n} tok"), budget) {
            break;
        }
        packed.pop();
        md_text = md_body(&cache_md, &packed, mode, &by_id, &tail_md);
    }
    if let Some(s) = &session {
        // чанки `mode=chunks`, отданные целиком, — выданы сессии (R11: по `chunk_id`)
        let given: Vec<SeenItem> = if mode == SearchMode::Chunks {
            packed
                .iter()
                .filter(|p| matches!(p.text, Some((_, false))) && seen_tracked(&p.item))
                .flat_map(|p| {
                    p.item.chunk_ids.iter().map(|c| SeenItem {
                        key: seen_key(&targets, &p.item, c),
                        rev: p.item.hit.rev,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        // учёт выданного (M6 §8): REST и CLI с явной сессией — событие `search`, токены —
        // Markdown того же отбора (у `format=json` — тоже он: размер выдачи, а не обёртки JSON);
        // из MCP вызов считает инструмент событием `mcp`
        let counts: Vec<TokenCount> = (!s.via_mcp)
            .then(|| TokenCount {
                event: SEARCH_TOKEN_EVENT.into(),
                calls: 1,
                tokens: count_tokens(&md_text) as u64,
                reread_calls: 0,
            })
            .into_iter()
            .collect();
        // отметки и учёт — одной транзакцией базы сессии, как у `/recall` и `/read`
        if (mode == SearchMode::Chunks || !counts.is_empty())
            && let Err(e) = mark_seen_with_tokens(&s.db, &s.touch, &given, &counts).await
        {
            session_failed("отметка и учёт выданного", &e);
        }
    }

    Ok(match format {
        OutFormat::Json => {
            let items: Vec<Value> = cache_items
                .iter()
                .map(cache_step::item_json)
                .chain(packed.iter().map(|p| item_json(p, mode, &by_id, now)))
                .collect();
            Json(json!({
                "items": items,
                "degraded": degraded.is_some(),
                "degraded_reason": degraded.map(DegradedReason::as_str),
                "degraded_text": degraded.map(DegradedReason::text),
                "warnings": warnings,
                "expanded": expanded,
                "pending_count": pending,
                "timings": {
                    "cache_ms": cache_ms,
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
            let mut r = md_text.into_response();
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
            updated_at: zam_store::time::now(),
            snippet: format!("{MARK_START}s{MARK_END}"),
            text: text.into(),
            index_state: "indexed".into(),
            entry: None,
            area: None,
            certainty: None,
            verified_at: None,
            attachment: None,
        }
    }

    /// Упаковка счётчиком сервера без пространств (карточки — `?/<путь>`).
    fn pk(items: Vec<Item>, mode: SearchMode, budget: usize) -> Vec<Packed> {
        pack_items(items, mode, budget, &count_tokens, &HashMap::new())
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
                seen: false,
                seen_kept: false,
                in_cache: false,
                stale_refs: Vec::new(),
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
        let capped = cap_items(its, 2, 1);
        let ids: Vec<&str> = capped.iter().map(|i| i.hit.chunk_id.as_str()).collect();
        assert_eq!(ids, ["c1", "c2", "c3"]);
        // у документа вложения — своя квота (`max_per_attachment`), заметку она не трогает
        let mut its = items(
            vec![
                hit(a, "1"),
                hit(a, "22"),
                att_hit(b, "333"),
                att_hit(b, "4444"),
            ],
            false,
        );
        its.push(items(vec![hit(a, "55555")], false).remove(0));
        let ids: Vec<String> = cap_items(its, 3, 1)
            .into_iter()
            .map(|i| i.hit.chunk_id)
            .collect();
        assert_eq!(ids, ["c1", "c2", "c3", "c5"]);
    }

    /// Чанк документа вложения `doc` (узел-владелец — новый).
    fn att_hit(doc: Id, text: &str) -> FtsHit {
        let mut h = hit(doc, text);
        h.target.kind = TargetKind::Attachment;
        h.level = "attachment".into();
        h.rev = 0;
        h.attachment = Some(zam_store::AttachmentHit {
            id: doc,
            node_id: Id::new(),
            filename: "книга.pdf".into(),
            blob_sha256: "f".repeat(64),
            locator: Some("page:12".into()),
            sheet_no: None,
        });
        h
    }

    /// Карточка вложения (задача 6 M5): `a:<chunk>`, путь заметки-владельца, файл, путь заголовков
    /// и подпись `locator`; JSON — `kind: attachment`, `id`, `node_id` владельца, `attachment_id`,
    /// `file`, `blob_sha256`, `locator`, `locator_text`.
    #[test]
    fn md_card_locator_text() {
        let now = zam_store::time::now();
        let doc = Id::new();
        let mut h = att_hit(doc, "Текст страницы");
        h.chunk_id = "ab".repeat(16);
        h.path = "docs/отчёт".into();
        h.heading_path = "Глава 1".into();
        let owner = h.attachment.as_ref().unwrap().node_id;
        let card = |h: FtsHit| {
            let mut p = pk(items(vec![h], false), SearchMode::Brief, 1000).remove(0);
            p.tokens = 7;
            p
        };
        let p = card(h.clone());
        let s = item_md(1, &p, SearchMode::Brief, &HashMap::new());
        let chunk = "ab".repeat(16);
        assert!(
            s.starts_with(&format!(
                "1. a:{chunk} ?/docs/отчёт › книга.pdf › Глава 1 · стр. 12  [.50] {} · 7t\n",
                p.item.hit.updated_at.strftime("%Y-%m-%d %H:%M:%S")
            )),
            "{s}"
        );
        let j = item_json(&p, SearchMode::Brief, &HashMap::new(), now);
        assert_eq!(j["kind"], "attachment");
        assert_eq!(j["id"], format!("a:{chunk}"));
        assert_eq!(j["node_id"], json!(owner));
        assert_eq!(j["attachment_id"], json!(doc));
        assert_eq!(j["file"], "книга.pdf");
        assert_eq!(j["blob_sha256"], "f".repeat(64));
        assert_eq!(j["locator"], "page:12");
        assert_eq!(j["locator_text"], "стр. 12");
        assert_eq!(j["level"], "attachment");
        assert!(j.get("entry_id").is_none());
        // секрет в имени листа маскируется и в `locator`, и в подписи (имя файла — как есть)
        let mut s = h.clone();
        s.attachment.as_mut().unwrap().locator = Some("sheet:AKIAIOSFODNN7EXAMPLE!A1:B2".into());
        let p = card(s);
        let j = item_json(&p, SearchMode::Brief, &HashMap::new(), now);
        let md = item_md(1, &p, SearchMode::Brief, &HashMap::new());
        for t in [j["locator"].to_string(), j["locator_text"].to_string(), md] {
            assert!(!t.contains("AKIAIOSFODNN7EXAMPLE"), "{t}");
        }
        assert!(j["locator"].as_str().unwrap().ends_with("!A1:B2"), "{j}");
        // лист с диапазоном, слайд, абзац; без locator — без подписи
        for (loc, text) in [
            ("sheet:Итоги!A1:F40", "лист «Итоги» A1:F40"),
            ("slide:3", "слайд 3"),
            ("para:40", "абз. 40"),
        ] {
            let mut h = h.clone();
            h.attachment.as_mut().unwrap().locator = Some(loc.into());
            let p = card(h);
            assert!(
                item_md(1, &p, SearchMode::Brief, &HashMap::new())
                    .contains(&format!("› Глава 1 · {text}  [")),
                "{loc}"
            );
            assert_eq!(
                item_json(&p, SearchMode::Brief, &HashMap::new(), now)["locator_text"],
                text
            );
        }
        let mut h = h.clone();
        h.heading_path.clear();
        h.attachment.as_mut().unwrap().locator = None;
        let p = card(h);
        assert!(
            item_md(1, &p, SearchMode::Brief, &HashMap::new())
                .starts_with(&format!("1. a:{chunk} ?/docs/отчёт › книга.pdf  [.50]")),
        );
        assert!(item_json(&p, SearchMode::Brief, &HashMap::new(), now)["locator_text"].is_null());
        // в `notes` у вложения нет L0 — карточка несёт сниппет
        let p = card(att_hit(doc, "x"));
        let j = item_json(&p, SearchMode::Notes, &HashMap::new(), now);
        assert_eq!(j["snippet"], json!(p.item.hit.snippet));
    }

    #[test]
    fn budget_trims_text_and_keeps_one() {
        let long = "слово ".repeat(100);
        let hs = vec![hit(Id::new(), &long), hit(Id::new(), "бб")];
        // бюджет — вся карточка (M6 §4.2): заголовок и урезанный текст
        let p = pk(items(hs, true), SearchMode::Chunks, 80);
        assert_eq!(p.len(), 1);
        let (t, tr) = p[0].text.clone().unwrap();
        let md = item_md(1, &p[0], SearchMode::Chunks, &HashMap::new());
        assert!(tr && count_tokens(&md) <= 80 && !t.is_empty(), "{md}");
        let p = pk(
            items(vec![hit(Id::new(), "x")], true),
            SearchMode::Chunks,
            0,
        );
        assert_eq!(p.len(), 1);
        let p = pk(
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

    /// Заметка `content` в личной базе: цель поиска и id узла.
    async fn note_target(name: &str, content: String) -> (SearchTarget, Id) {
        let dir = crate::test_util::dir_in_target(name);
        let db = zam_store::UserDbs::new(dir).get(Id::new()).await.unwrap();
        let sp = zam_store::list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.kind == SpaceKind::Knowledge)
            .unwrap();
        let n = zam_store::create_note(
            &db,
            zam_store::NewNote {
                space_id: sp.id,
                path: zam_store::NodePath::parse("длинная").unwrap(),
                content,
                l0: None,
                attrib: zam_store::Attribution::new(zam_store::AuthorKind::Agent),
            },
        )
        .await
        .unwrap();
        let t = SearchTarget {
            owner: Id::new(),
            db,
            spaces: vec![sp],
            role: super::super::scope::Role::Personal,
            solution: None,
        };
        (t, n.id)
    }

    /// Хиты-листы `paras` одной секции `heading_path` узла `node` (ревизия 1).
    fn section_hits(node: Id, heading_path: &str, paras: &[String]) -> Vec<Item> {
        let hits = paras
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut h = hit(node, p);
                h.chunk_id = format!("c{i}");
                h.heading_path = heading_path.into();
                h
            })
            .collect();
        items(hits, false)
    }

    /// `expand=section`: 60 листов одной секции (путь с H4) — исходник разобран один раз,
    /// листы слиты в один элемент с текстом секции H2 (ревью fix round 1 задачи 17).
    #[tokio::test]
    async fn expand_section_parses_note_once() {
        let paras: Vec<String> = (0..60)
            .map(|i| format!("Абзац номер {i} про перенос данных пакетами."))
            .collect();
        let content = format!(
            "# Док\n\n## Детали\n\n#### Глубже\n\n{}\n\n## Другое\n\nтекст\n",
            paras.join("\n\n")
        );
        let (t, node) = note_target("search-expand-once", content).await;
        let targets = vec![t];
        let mut h = Health::new(&targets, Vec::new());
        let mut cache = SectionCache::default();
        let its = section_hits(node, "Док > Детали > Глубже", &paras);
        let out = expand_items(&mut h, its, Expand::Section, 100, &mut cache)
            .await
            .unwrap();
        assert_eq!(cache.builds, 1, "исходник разобран один раз");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chunk_ids.len(), 60);
        let text = &out[0].text.as_ref().unwrap().0;
        assert!(text.starts_with("## Детали") && text.contains("Абзац номер 59 "));
        assert!(!text.contains("## Другое"));
    }

    /// Паника разбора или поиска секции в `spawn_blocking` не роняет запрос 500: элемент
    /// отдаётся без секции (текст листа), в ответе — предупреждение (финальная волна M3,
    /// ревью fix round 2 задачи 17).
    #[tokio::test]
    async fn expand_section_panic_degrades_to_leaf() {
        // журнал теста: текст паники (в нём фрагмент исходника заметки) туда не попадает
        let buf = LogBuf::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(sub);
        let leaf = "Абзац про перенос данных пакетами.".to_string();
        let secret = "zam-test-note-secret-7f3a";
        let content = format!("# Док\n\n## Детали\n\n{leaf} {secret}\n\n{PANIC_BUILD_MARK}\n");
        let (t, node) = note_target("search-expand-panic", content).await;
        let targets = vec![t];
        let mut h = Health::new(&targets, Vec::new());
        let mut cache = SectionCache::default();
        let its = section_hits(node, "Док > Детали", std::slice::from_ref(&leaf));
        let out = expand_items(&mut h, its, Expand::Section, 10, &mut cache)
            .await
            .expect("без 500");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text.as_ref().unwrap().0, leaf, "текст листа");
        assert_eq!(h.warnings, [WARN_SECTION_UNAVAILABLE]);
        assert!(h.alive[0], "цель не исключена");
        let log = buf.text();
        assert!(
            log.contains("элемент без секции") && log.contains("panic=true"),
            "{log}"
        );
        assert!(!log.contains(secret), "текст заметки в журнале: {log}");
        assert!(
            !log.contains("тестовая паника"),
            "сообщение паники в журнале: {log}"
        );
    }

    /// Буфер журнала теста (`tracing_subscriber::fmt().with_writer`).
    #[derive(Clone, Default)]
    struct LogBuf(Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogBuf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for LogBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = LogBuf;
        fn make_writer(&'a self) -> LogBuf {
            self.clone()
        }
    }

    /// Ключ кэша секций и слияния — с индексом цели: один и тот же `(узел, ревизия)` в двух
    /// базах (копия базы под другим пользователем) не получает таблицу секций чужого
    /// исходника и не сливается с чужим элементом (ревью fix round 2 задачи 17, Minor 1).
    #[tokio::test]
    async fn expand_section_cache_keyed_by_target() {
        let a_text = "Текст секции базы А.".to_string();
        let b_text = "Текст секции базы Б, другой.".to_string();
        let (ta, node) = note_target(
            "search-expand-two-a",
            format!("# Док\n\n## Детали\n\n{a_text}\n"),
        )
        .await;
        let (tb, _) = note_target("search-expand-two-b", "# Другая\n".into()).await;
        let b_src = format!("# Док\n\n## Детали\n\n{b_text}\n");
        let space = tb.spaces[0].id;
        tb.db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, current_rev,
                                        index_state, created_at, updated_at)
                     VALUES (?1, ?2, NULL, 'note', 'копия', 'копия', 1, 'excluded', ?3, ?3)",
                    (node, space, "2026-10-26T00:00:00Z"),
                )?;
                tx.execute(
                    "INSERT INTO revisions (node_id, rev, content, content_hash, size,
                                            author_kind, ts)
                     VALUES (?1, 1, ?2, 'h', ?3, 'agent', ?4)",
                    (node, &b_src, b_src.len() as i64, "2026-10-26T00:00:00Z"),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let targets = vec![ta, tb];
        let mut h = Health::new(&targets, Vec::new());
        let mut cache = SectionCache::default();
        let mut its = section_hits(node, "Док > Детали", &[a_text.clone(), b_text.clone()]);
        its[1].target = 1;
        let out = expand_items(&mut h, its, Expand::Section, 10, &mut cache)
            .await
            .unwrap();
        assert_eq!(out.len(), 2, "разные базы не сливаются");
        assert!(out[0].text.as_ref().unwrap().0.contains(&a_text));
        assert!(
            out[1].text.as_ref().unwrap().0.contains(&b_text),
            "своя таблица секций"
        );
        assert_eq!(cache.builds, 2);
    }

    /// Замер `expand=section` (ревью fix round 1 задачи 17): заметка ~1 МБ без H1–H3,
    /// 300 кандидатов окна из её конца. Запуск:
    /// `cargo test -p zam-server --release --lib bench_expand_section_big_note`
    /// `-- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "замер"]
    async fn bench_expand_section_big_note() {
        let paras: Vec<String> = (0..12_000)
            .map(|i| format!("Абзац номер {i} про перенос данных пакетами, проверка {i}."))
            .collect();
        let content = paras.join("\n\n");
        eprintln!("заметка: {} байт", content.len());
        let (t, node) = note_target("search-expand-bench", content).await;
        let targets = vec![t];
        let mut h = Health::new(&targets, Vec::new());
        let mut cache = SectionCache::default();
        let its = section_hits(node, "", &paras[paras.len() - 300..]);
        let t0 = Instant::now();
        let out = expand_items(&mut h, its, Expand::Section, 100, &mut cache)
            .await
            .unwrap();
        eprintln!(
            "300 кандидатов: {:?}, разборов {}, элементов {}",
            t0.elapsed(),
            cache.builds,
            out.len()
        );
        assert_eq!(cache.builds, 1);
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
            created_at: zam_store::time::now(),
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
        let mut sections: HashMap<(usize, TargetRef, SectionKey), usize> = out
            .iter()
            .enumerate()
            .map(|(i, it)| ((it.target, it.hit.target, SectionKey::At(i)), i))
            .collect();
        drop_target(&mut out, &mut sections, 1);
        assert_eq!(out.len(), 2);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[&(0, out[0].hit.target, SectionKey::At(0))], 0);
        assert_eq!(sections[&(0, out[1].hit.target, SectionKey::At(2))], 1);
    }

    /// Хвост карточки §4.1 дословно (примеры спецификации).
    #[test]
    fn card_tail_v2_strings() {
        let t = |s: &str| -> Timestamp { s.parse().unwrap() };
        assert_eq!(
            card_tail(0.82, 7, t("2026-10-10T14:03:22Z"), 340, None, None),
            "[.82] r7 · 2026-10-10 14:03:22 · 340t"
        );
        assert_eq!(
            card_tail(
                0.77,
                0,
                t("2026-09-28T08:00:01Z"),
                120,
                Some("hypothesis"),
                Some("active")
            ),
            "[.77] hyp · 2026-09-28 08:00:01 · 120t"
        );
        assert_eq!(
            card_tail(1.0, 3, t("2026-01-02T03:04:05Z"), 5, Some("axiom"), None),
            "[1.0] r3 · ax · 2026-01-02 03:04:05 · 5t"
        );
        assert_eq!(
            card_tail(
                0.051,
                0,
                t("2026-01-02T03:04:05Z"),
                0,
                Some("theorem"),
                Some("superseded")
            ),
            "[.05] th · superseded · 2026-01-02 03:04:05 · 0t"
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
            "1. n:{} ?/p › A > B  [.50] r1 · 2026-10-10 08:00:00 · 7t\n   **s**\n",
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
            created_at: jiff::Timestamp::UNIX_EPOCH,
        });
        let mut p = Packed {
            item: it,
            text: None,
            tokens: 120,
        };
        let now = p.item.hit.updated_at;
        let at = now.strftime("%Y-%m-%d %H:%M:%S");
        let s = item_md(2, &p, SearchMode::Brief, &HashMap::new());
        // §4.1 дословно: статус `active` не выводится, надёжность — `hyp`
        assert_eq!(
            s,
            format!(
                "2. i:{id} insight/gotcha «Bash-скрипты должны иметь LF»  [.50] hyp · {at} · \
                 120t\n   **s**\n"
            )
        );
        // changelog без статуса и certainty
        p.item.hit.entry = Some(zam_store::EntryHit {
            kind: "changelog".into(),
            status: None,
            insight_kind: Some("fix".into()),
            certainty: None,
            title: "Исправлен CRLF".into(),
            created_at: jiff::Timestamp::UNIX_EPOCH,
        });
        p.tokens = 9;
        let s = item_md(1, &p, SearchMode::Brief, &HashMap::new());
        assert!(
            s.starts_with(&format!(
                "1. c:{id} changelog/fix «Исправлен CRLF»  [.50] {at} · 9t\n"
            )),
            "{s}"
        );
        let j = item_json(&p, SearchMode::Brief, &HashMap::new(), now);
        assert_eq!(j["entry_id"], json!(id));
        assert_eq!(j["age_days"], 0);
        assert!(j.get("node_id").is_none());
        assert_eq!(j["kind"], "changelog");
        assert_eq!(j["insight_kind"], "fix");
        assert_eq!(j["title"], "Исправлен CRLF");
        assert!(j["status"].is_null());
    }

    #[test]
    fn filters_defaults_and_validation() {
        let f = target_filter(&Filters::default(), false, false).unwrap();
        assert_eq!(f.kinds, DEFAULT_KINDS.map(String::from).to_vec());
        assert_eq!(f.file, None);
        // filters.file нормализуется; путь выше корня — 400
        let file = |p: &str| {
            target_filter(
                &Filters {
                    file: Some(p.into()),
                    ..Filters::default()
                },
                false,
                false,
            )
        };
        assert_eq!(
            file(".\\src\\a.rs").unwrap().file.as_deref(),
            Some("src/a.rs")
        );
        assert!(matches!(file("../x"), Err(ApiError::BadRequest(_))));
        // умолчание видов включает term и question: статусы — действующие и `answered`
        assert_eq!(f.statuses, ["active", "revisit", "answered"]);
        let def = |kinds: &[&str]| {
            target_filter(
                &Filters {
                    kinds: Some(kinds.iter().map(|s| s.to_string()).collect()),
                    ..Filters::default()
                },
                false,
                false,
            )
            .unwrap()
            .statuses
        };
        // явный `kinds`: открытые вопросы и текущие задачи; выполненные и отброшенные — по статусу
        assert_eq!(
            def(&["question"]),
            ["active", "revisit", "answered", "open"]
        );
        assert_eq!(
            def(&["task"]),
            ["active", "revisit", "open", "in_progress", "blocked"]
        );
        assert_eq!(def(&["insight", "term"]), ["active", "revisit"]);
        // исключаются все статусы, кроме выбранных (с M4.5 — и статусы task/question)
        let rest = |keep: &[&str]| -> Vec<String> {
            STATUSES
                .iter()
                .filter(|s| !keep.contains(s))
                .map(|s| s.to_string())
                .collect()
        };
        assert_eq!(
            excluded_statuses(&f),
            rest(&["active", "revisit", "answered"])
        );
        assert!(excluded_statuses(&f).starts_with(&["superseded".into(), "retracted".into()]));
        // include_history без `kinds`: все статусы, кроме `open` (открытые вопросы и задачи - только
        // по явному `kinds`); с явным `kinds` фильтр статусов снят
        let f = target_filter(&Filters::default(), true, false).unwrap();
        assert!(!f.statuses.contains(&"open".to_string()));
        assert!(f.statuses.contains(&"superseded".to_string()));
        // статусы идей — только при идеях среди видов (M5.7): у токена их нет, у WebUI — есть
        let mut want = vec!["open"];
        want.extend(IDEA_STATUSES);
        assert_eq!(excluded_statuses(&f), want);
        let web = target_filter(&Filters::default(), true, true).unwrap();
        assert_eq!(excluded_statuses(&web), ["open"]);
        let f = target_filter(
            &Filters {
                kinds: Some(vec!["question".into()]),
                ..Filters::default()
            },
            true,
            false,
        )
        .unwrap();
        assert!(f.statuses.is_empty() && excluded_statuses(&f).is_empty());
        // decided_by сужает векторный фильтр до решений
        let by = target_filter(
            &Filters {
                decided_by: Some("agent".into()),
                ..Filters::default()
            },
            false,
            false,
        )
        .unwrap();
        let vf = vector_filter(Vec::new(), None, &by, false);
        assert_eq!(vf.kinds, ["insight"]);
        assert_eq!(vf.insight_kinds, ["decision"]);
        let plain = target_filter(&Filters::default(), false, false).unwrap();
        assert!(
            vector_filter(Vec::new(), None, &plain, false)
                .insight_kinds
                .is_empty()
        );
        let f = target_filter(
            &Filters {
                kinds: Some(vec!["insight".into()]),
                status: Some(vec!["retracted".into()]),
                certainty: Some(vec!["axiom".into(), "theorem".into()]),
                decided_by: None,
                file: None,
                insight_kinds: None,
            },
            false,
            false,
        )
        .unwrap();
        assert_eq!(f.kinds, vec!["insight"]);
        assert_eq!(f.certainty, vec!["axiom", "theorem"]);
        // по умолчанию certainty не ограничен
        assert!(
            target_filter(&Filters::default(), false, false)
                .unwrap()
                .certainty
                .is_empty()
        );
        assert_eq!(excluded_statuses(&f), rest(&["retracted"]));
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
            Filters {
                insight_kinds: Some(vec!["Gotcha".into()]),
                ..Filters::default()
            },
        ] {
            assert!(target_filter(&bad, false, false).is_err());
        }
    }

    /// `filters.insight_kinds` (§9.1): векторный канал сужается только при одних insight,
    /// с `decided_by` — пересечение с `decision`; ключ кэша — только при заданном фильтре.
    #[test]
    fn insight_kinds_vector_filter_and_cache_key() {
        let tf = |f: Filters| target_filter(&f, false, false).unwrap();
        let ik = || Some(vec!["failure".to_string(), "experiment".to_string()]);
        // виды по умолчанию (заметки и записи) — векторный фильтр не сужается
        let any = tf(Filters {
            insight_kinds: ik(),
            ..Filters::default()
        });
        assert_eq!(any.insight_kinds, ["experiment", "failure"]);
        assert!(
            vector_filter(Vec::new(), None, &any, false)
                .insight_kinds
                .is_empty()
        );
        // одни insight — сужается
        let only = tf(Filters {
            kinds: Some(vec!["insight".into()]),
            insight_kinds: ik(),
            ..Filters::default()
        });
        assert_eq!(
            vector_filter(Vec::new(), None, &only, false).insight_kinds,
            ["experiment", "failure"]
        );
        // decided_by: пересечение с decision; пустое — ни одной точки
        let by = |kinds: Vec<String>| {
            vector_filter(
                Vec::new(),
                None,
                &tf(Filters {
                    decided_by: Some("human".into()),
                    insight_kinds: Some(kinds),
                    ..Filters::default()
                }),
                false,
            )
            .insight_kinds
        };
        assert_eq!(by(vec!["decision".into(), "gotcha".into()]), ["decision"]);
        assert_eq!(by(vec!["gotcha".into()]), [NO_INSIGHT_KIND]);
        // ключ кэша выжимок меняется только при заданном фильтре
        let sc = ScopeReq::default();
        let key = |f: &Filters| cache_scope_hash(&sc, f, false, None, false).unwrap();
        let base = key(&Filters::default());
        assert_eq!(
            base,
            key(&Filters {
                insight_kinds: Some(Vec::new()),
                ..Filters::default()
            })
        );
        let a = key(&Filters {
            insight_kinds: ik(),
            ..Filters::default()
        });
        let b = key(&Filters {
            insight_kinds: Some(vec![
                "experiment".into(),
                "failure".into(),
                "failure".into(),
            ]),
            ..Filters::default()
        });
        assert_ne!(base, a);
        assert_eq!(a, b, "порядок и повторы не меняют ключ");
    }

    /// Ключ кэша выжимок M5.6 запроса по умолчанию (без решения) и с `include_history`.
    const M56_DEFAULT_KEY: &str =
        "c31dfa31c9c09b6586b0cdda868871345d33c71cd0d52cc8014eab4257b9158f";
    const M56_HISTORY_KEY: &str =
        "1ce9365890aceb9666a59596f7216cb168b0d9d20986ec89a529db2f4dd8c469";

    /// M5.7 (I9): вид `idea` и статусы идей не входят в ключ кэша выжимок — ключи M5.6
    /// (фикстура) сохраняются у токена и у WebUI, с историей и без.
    #[test]
    fn scope_key_ignores_idea() {
        let sc = ScopeReq::default();
        let key = |f: &Filters, history: bool, human: bool| {
            cache_scope_hash(&sc, f, history, None, human).unwrap()
        };
        let d = Filters::default();
        let fixture = |status: &[&str]| {
            zam_store::scope_hash(&json!({
                "solution": null,
                "include_knowledge": true,
                "include_rules": true,
                "path_prefix": null,
                "area": null,
                "filters": {
                    "kinds": ["attachment", "changelog", "insight", "note", "question", "release",
                              "term"],
                    "status": status,
                    "certainty": [],
                },
            }))
        };
        let current = fixture(&["active", "answered", "revisit"]);
        let history = fixture(&[
            "active",
            "answered",
            "blocked",
            "done",
            "dropped",
            "in_progress",
            "obsolete",
            "retracted",
            "revisit",
            "superseded",
        ]);
        // замороженные ключи M5.6 (`M56_*_KEY`): hex — `zam_store::scope_hash` структур
        // `current` и `history`, посчитан 2026-11-04 на коде после 4361d39 для запроса по
        // умолчанию; структуры сверены вручную со `scope_key` на 1ceeec0 (DEFAULT_KINDS — 7
        // видов, DEFAULT_STATUSES + answered; история — STATUSES без open), `scope_hash`
        // (`zam-store/src/cache`) с 1ceeec0 не менялся
        assert_eq!(current, M56_DEFAULT_KEY);
        assert_eq!(history, M56_HISTORY_KEY);
        for human in [false, true] {
            assert_eq!(key(&d, false, human), current, "human = {human}");
            assert_eq!(key(&d, true, human), history, "human = {human}");
        }
        let kinds = |k: &[&str]| Filters {
            kinds: Some(k.iter().map(|s| s.to_string()).collect()),
            ..Filters::default()
        };
        assert_eq!(
            key(&kinds(&["note", "idea"]), false, false),
            key(&kinds(&["note"]), false, false)
        );
        // без idea в kinds статусы идей — обычный фильтр и входят в ключ
        let with_status = |k: &[&str], s: &[&str]| Filters {
            kinds: Some(k.iter().map(|x| x.to_string()).collect()),
            status: Some(s.iter().map(|x| x.to_string()).collect()),
            ..Filters::default()
        };
        assert_ne!(
            key(&with_status(&["insight"], &["active", "new"]), false, false),
            key(&with_status(&["insight"], &["active"]), false, false)
        );
        assert_ne!(
            key(&with_status(&["insight"], &["new"]), false, false),
            key(&kinds(&["insight"]), false, false)
        );
        // у WebUI idea в умолчаниях, у токена — только явно
        assert!(
            target_filter(&d, false, true)
                .unwrap()
                .kinds
                .contains(&IDEA.to_string())
        );
        assert!(
            !target_filter(&d, false, false)
                .unwrap()
                .kinds
                .contains(&IDEA.to_string())
        );
        let t = target_filter(&kinds(&["idea"]), false, false).unwrap();
        for s in IDEA_DEFAULT_STATUSES {
            assert!(t.statuses.contains(&s.to_string()), "{s}");
        }
        assert!(!t.statuses.contains(&"declined".to_string()));
    }
}
