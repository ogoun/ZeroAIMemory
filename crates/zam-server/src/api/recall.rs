//! Проактивное вспоминание `POST /recall` (§10.3, §12; задача 8 M4) — подсказки хукам агента
//! (задача 10) и инструменту MCP `memory_recall` (задача 9).
//!
//! ```text
//! POST /recall {event, session_id?, solution?, area?, prompt?, error?, tool?, budget_tokens?,
//!               agent_block_version?}
//!   → 200 {text, items: [{id, kind, title, certainty, reason}], tokens, timings, warnings,
//!          hooks?}
//! ```
//!
//! - `text` — готовый Markdown для `additionalContext`; пусто — тишина. Каждая подсказка — строка
//!   `ZeroAIMemory: <заголовок> [<вид>, <certainty>] — i:<id>` (у заметки — `n:<id>`, у
//!   changelog — `c:<id>`), под ней с отступом может идти пояснение (L1 правила, фрагмент).
//!   `items` — показанные подсказки по порядку: `id` — та же ссылка, `kind` — `insight`,
//!   `changelog`, `note` или `folder`, `reason` — откуда подсказка (`rules`, `solution`,
//!   `gotcha`, `failure`, `revisit`, `changelog`, `search`, `error_regex`, `error_fts`);
//!   `tokens` — размер `text` (счётчик чанкера), `timings {collect_ms, record_ms, total_ms}`;
//! - **`session_start`** — дайджест не больше `budget_tokens` (по умолчанию
//!   `recall.digest_tokens` = 1500) по приоритету: напоминание об устаревшем блоке инструкции
//!   (`agent_block_version` < [`AGENT_BLOCK_VERSION`]), ожидающие запросы доступа к решениям
//!   пользователя (`access_requests_pending`), Rules (L1 каждой заметки, до
//!   [`RULES_MAX`] заметок, L1 — до [`RULE_L1_TOKENS`] токенов), «L1 решения» — корневые узлы
//!   пространства решения с L0 (до [`SOLUTION_ROOT_MAX`]), до 5 действующих (`active`)
//!   `gotcha`/`failure` решения и Knowledge (новые первыми), до 5 записей `revisit`, последние
//!   5 записей changelog решения **[решение исполнителя]**. Ответ несёт `hooks: {session_start,
//!   prompt, pre_tool, post_tool, checkpoint}` — настройки типов подсказок пользователя (R5;
//!   хранения пользовательских настроек на сервере пока нет — все `true`, задача 13);
//! - **`prompt`** — поиск `mode=brief` (без реранка, без кэша выжимок, `skip_seen: false`) по
//!   первым 512 символам `prompt` в Knowledge, Rules и решении; из выдачи — только insight и
//!   заметки Rules с баллом ≥ `recall.prompt_min_score` (0,6), не больше [`PROMPT_MAX`];
//! - **`post_tool`** — ошибка инструмента: совпадения `error_regex` действующих триггеров
//!   insight решения и Knowledge (по первым [`ERROR_SCAN_MAX`] байтам `error`; триггер с
//!   `tools` — только для этих инструментов, если `tool` задан), затем FTS по insight по
//!   нормализованной ошибке ([`normalize_error`]): строгий запрос (все слова) — как есть,
//!   ослабленный (любое слово) — если запись покрывает достаточно слов ошибки
//!   ([`coverage_enough`]). Не больше [`POST_TOOL_MAX`]; строка — «ZeroAIMemory: эта ошибка уже
//!   разбиралась: …»;
//! - **без повторов**: подсказка `session_start` и `prompt` показывается не больше раза за
//!   сессию — по `session_seen` с частью [`RECALL_PART`] (любая ревизия); `post_tool` выдаёт
//!   подсказку при каждой ошибке. Показанное отмечается в `session_seen` базы пользователя
//!   запроса (касание сессии и отметка — одна транзакция), каждый показ — `usage(recall_shown,
//!   <событие>)` в базе цели (у общего решения — владельца). Сессия — `session_id` или
//!   `X-Zam-Session`; без неё подсказки выдаются без учёта повторов и без `session_seen`
//!   **[решение исполнителя]** (MCP без сессии); некорректный id — 400. Сбой учёта — WARN, ответ
//!   не портится;
//! - **область** — как у поиска ([`resolve_targets`]): личная база (Knowledge, Rules, своё
//!   решение) и база владельца доступного чужого решения (только пространство решения). Human
//!   и чужой Knowledge не подмешиваются никогда; решение без доступа исключается с
//!   `warnings: ["solution access pending|rejected|revoked"]`, недоступная база владельца —
//!   `solution unavailable`. Решение — `solution` тела (не GUID — 400) или `X-Zam-Solution`;
//! - **бюджет сервера** `recall.timeout_ms` (200): шаги сбора идут до срока, по превышении
//!   отдаётся собранное и `warnings: ["recall timeout"]`; учёт показов — после сбора;
//! - `budget_tokens` — 1..=200000 (иначе 400); `event` — `session_start | prompt | post_tool`
//!   (иначе 400; `pre_tool` — только локально в хуке, §10.3).
//!
//! Маршрут — служебный, рядом с `/search` и `/read`: `/recall` пишет только `usage` и
//! `session_seen`, а не память, поэтому `memory_writes` не растёт (решение контроллера R12).
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;
use zam_core::{AGENT_BLOCK_VERSION, Id, TargetKind, TargetRef};
use zam_search::recall::{
    Section, coverage_enough, error_regex_matches, error_terms, hint_line, normalize_error,
    one_line, pack_sections, term_coverage,
};
use zam_search::truncate_to_tokens;
use zam_store::{
    Certainty, Entry, EntryFilter, EntryKind, EntryStatus, FtsQuery, NodeKind, RECALL_PART,
    SeenItem, SeenKey, SpaceKind, StoreError, TargetFilter, TriggerKind, UsageEvent, UsageKind,
    UsageSource, active_triggers, count_tokens, get_entry, list_children, list_entries, mark_seen,
    node_content, record_usages, search_fts_channels, seen_revs, touch_session,
};

use super::json_body;
use super::nodes::read::l1;
use super::search::targets::{ScopeReq, SearchTarget, dedup_keep_order, resolve_targets};
use super::search::{Health, SOLUTION_HEADER};
use super::sessions::{RequestSession, request_session};
use crate::app::AppState;
use crate::attrib::{AREA_HEADER, SESSION_HEADER};
use crate::auth::{Principal, target_user_id};
use crate::error::ApiError;

/// Предупреждение: сбор подсказок не уложился в `recall.timeout_ms`.
pub const WARN_RECALL_TIMEOUT: &str = "recall timeout";
/// Заметок Rules в дайджесте.
pub const RULES_MAX: usize = 20;
/// Предел L1 одной заметки Rules в дайджесте, токенов.
pub const RULE_L1_TOKENS: usize = 200;
/// Корневых узлов решения («L1 решения») в дайджесте.
pub const SOLUTION_ROOT_MAX: usize = 10;
/// Действующих `gotcha`/`failure`, записей `revisit` и changelog в дайджесте — каждого.
pub const DIGEST_ENTRIES_MAX: usize = 5;
/// Подсказок к запросу пользователя.
pub const PROMPT_MAX: usize = 3;
/// Подсказок к ошибке инструмента.
pub const POST_TOOL_MAX: usize = 3;
/// Байт ошибки, по которым сопоставляется `error_regex`.
pub const ERROR_SCAN_MAX: usize = 64 * 1024;
/// Кандидатов записей журнала одного вида с пространства (запас на уже показанные).
const ENTRY_FETCH: usize = 30;
/// Предел `budget_tokens` запроса (как у `/read`).
const BUDGET_MAX: usize = 200_000;
/// Кандидатов поиска `prompt` до фильтра видов, балла и показанного.
const PROMPT_SEARCH_TOP_K: usize = 20;
/// Бюджет карточек внутреннего поиска `prompt` (не текста подсказок), токенов.
const PROMPT_SEARCH_BUDGET: usize = 4000;
/// Предел фрагмента под подсказкой `prompt`, токенов.
const SNIPPET_TOKENS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    SessionStart,
    Prompt,
    PostTool,
}

impl Event {
    fn source(self) -> UsageSource {
        match self {
            Event::SessionStart => UsageSource::SessionStart,
            Event::Prompt => UsageSource::Prompt,
            Event::PostTool => UsageSource::PostTool,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallReq {
    event: Event,
    session_id: Option<String>,
    solution: Option<String>,
    area: Option<String>,
    prompt: Option<String>,
    error: Option<String>,
    tool: Option<String>,
    budget_tokens: Option<usize>,
    agent_block_version: Option<u32>,
}

/// Кандидат в подсказки.
struct Cand {
    /// Индекс цели (базы) в области запроса.
    target: usize,
    tref: TargetRef,
    rev: i64,
    /// Ссылка `i:|n:|c:<id>`.
    reference: String,
    kind: &'static str,
    title: String,
    certainty: Option<String>,
    reason: &'static str,
    /// Markdown подсказки (строка и, возможно, строки с отступом).
    block: String,
}

/// Срок сбора подсказок (`recall.timeout_ms`).
struct Deadline {
    at: Instant,
    expired: bool,
}

impl Deadline {
    /// Шаг сбора до срока: `None` — срок вышел (до шага или во время него).
    async fn step<T>(&mut self, f: impl std::future::Future<Output = T>) -> Option<T> {
        if self.expired || Instant::now() >= self.at {
            self.expired = true;
            return None;
        }
        match tokio::time::timeout_at(self.at, f).await {
            Ok(v) => Some(v),
            Err(_) => {
                self.expired = true;
                None
            }
        }
    }
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}

fn certainty_str(c: Option<Certainty>) -> Option<String> {
    c.map(|c| c.as_str().to_string())
}

/// `data.kind` записи (вид инсайта или изменения), если задан строкой.
fn data_kind(e: &Entry) -> String {
    e.data
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn entry_ref(e: &Entry) -> String {
    let letter = match e.kind {
        EntryKind::Insight => "i",
        EntryKind::Changelog => "c",
        EntryKind::Release => "r",
    };
    format!("{letter}:{}", e.id)
}

/// Подсказка-запись журнала.
fn entry_cand(target: usize, e: &Entry, reason: &'static str, lead: &str) -> Cand {
    let kind = data_kind(e);
    let certainty = certainty_str(e.certainty);
    let reference = entry_ref(e);
    let block = hint_line(
        lead,
        &e.title,
        &[kind.as_str(), certainty.as_deref().unwrap_or_default()],
        &reference,
    );
    Cand {
        target,
        tref: TargetRef {
            kind: TargetKind::Entry,
            id: e.id,
        },
        rev: e.rev,
        reference,
        kind: e.kind.as_str(),
        title: e.title.clone(),
        certainty,
        reason,
        block,
    }
}

/// Пространства цели для записей журналов (insight, changelog): решение и Knowledge.
fn entry_spaces(t: &SearchTarget) -> Vec<Id> {
    t.spaces
        .iter()
        .filter(|s| matches!(s.kind, SpaceKind::Knowledge | SpaceKind::Solution))
        .map(|s| s.id)
        .collect()
}

/// Пространство решения в области: `(цель, пространство)`.
fn solution_space(targets: &[SearchTarget]) -> Option<(usize, Id)> {
    targets.iter().enumerate().find_map(|(i, t)| {
        t.spaces
            .iter()
            .find(|s| s.kind == SpaceKind::Solution)
            .map(|s| (i, s.id))
    })
}

/// Пространство вида `kind` личной базы (цель без чужого решения).
fn personal_space(targets: &[SearchTarget], kind: SpaceKind) -> Option<(usize, Id)> {
    targets.iter().enumerate().find_map(|(i, t)| {
        if t.solution.is_some() {
            return None;
        }
        t.spaces.iter().find(|s| s.kind == kind).map(|s| (i, s.id))
    })
}

/// Записи журнала пространств `spaces` по фильтру `f` (без пространства) — новые первыми.
async fn entries_of(
    health: &mut Health<'_>,
    deadline: &mut Deadline,
    spaces: &[(usize, Id)],
    f: impl Fn(Id) -> EntryFilter,
) -> Result<Vec<(usize, Entry)>, ApiError> {
    let mut out: Vec<(usize, Entry)> = Vec::new();
    for &(i, space) in spaces {
        if !health.alive[i] {
            continue;
        }
        let db = health.targets()[i].db.clone();
        let Some(r) = deadline.step(list_entries(&db, f(space))).await else {
            break;
        };
        if let Some((rows, _)) = health.soft(i, r)? {
            out.extend(rows.into_iter().map(|e| (i, e)));
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse((e.1.created_at, e.1.id)));
    Ok(out)
}

/// Фильтр ленты записей вида `kind` пространства `space` (без статусов и `data.kind`).
fn entry_filter(space: Id, kind: EntryKind) -> EntryFilter {
    EntryFilter {
        space_id: space,
        kind: Some(kind),
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: None,
        tag: None,
        data_kind: None,
        since: None,
        until: None,
        cursor: None,
        limit: ENTRY_FETCH,
    }
}

/// Разделы дайджеста `session_start` (см. модуль); каждый — кандидаты по приоритету.
async fn digest(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<(Vec<String>, Vec<(Option<&'static str>, Vec<Cand>)>), ApiError> {
    let targets = health.targets();
    let mut notices: Vec<String> = Vec::new();
    if let Some(v) = req.agent_block_version
        && v < AGENT_BLOCK_VERSION
    {
        notices.push(format!(
            "ZeroAIMemory: Блок ZeroAIMemory устарел (v{v} → v{AGENT_BLOCK_VERSION}): обнови \
             через `zam init` или WebUI"
        ));
    }
    let user = target_user_id(st, p, headers)?;
    if let Some(r) = deadline
        .step(st.system.pending_requests_for_owner(user))
        .await
    {
        let n = r?;
        if n > 0 {
            notices.push(format!(
                "ZeroAIMemory: запросы доступа к вашим решениям ждут ответа: {n} — WebUI \
                 «Администрирование → Решения» или `zam solution access requests`"
            ));
        }
    }
    let mut sections: Vec<(Option<&'static str>, Vec<Cand>)> = Vec::new();

    // Rules — L1 каждой заметки
    let mut rules: Vec<Cand> = Vec::new();
    if let Some((i, space)) = personal_space(targets, SpaceKind::Rules) {
        let db = targets[i].db.clone();
        if let Some(r) = deadline.step(list_children(&db, space, None, 16)).await {
            let notes: Vec<zam_store::Node> = r?
                .into_iter()
                .filter(|n| n.kind == NodeKind::Note)
                .take(RULES_MAX)
                .collect();
            for n in notes {
                let Some(r) = deadline.step(node_content(&db, n.id, None)).await else {
                    break;
                };
                let (rev, text) = match r {
                    Ok(v) => v,
                    // узел удалён между чтениями — без него
                    Err(StoreError::NotFound) => continue,
                    Err(e) => return Err(e.into()),
                };
                let certainty = certainty_str(n.certainty);
                let reference = format!("n:{}", n.id);
                let mut block = hint_line(
                    "правило ",
                    &n.path,
                    &[certainty.as_deref().unwrap_or_default()],
                    &reference,
                );
                let summary = l1(&text);
                let summary = truncate_to_tokens(&summary, RULE_L1_TOKENS, &count_tokens);
                for line in summary.lines().filter(|l| !l.trim().is_empty()) {
                    block.push_str("\n  ");
                    block.push_str(line.trim_end());
                }
                rules.push(Cand {
                    target: i,
                    tref: TargetRef {
                        kind: TargetKind::Node,
                        id: n.id,
                    },
                    rev,
                    reference,
                    kind: "note",
                    title: n.path.clone(),
                    certainty,
                    reason: "rules",
                    block,
                });
            }
        }
    }
    sections.push((Some("### Правила"), rules));

    // L1 решения — корневые узлы пространства решения с L0
    let sol = solution_space(targets);
    let mut overview: Vec<Cand> = Vec::new();
    if let Some((i, space)) = sol
        && health.alive[i]
    {
        let db = targets[i].db.clone();
        if let Some(r) = deadline.step(list_children(&db, space, None, 1)).await
            && let Some(nodes) = health.soft(i, r)?
        {
            for n in nodes.into_iter().take(SOLUTION_ROOT_MAX) {
                let folder = n.kind == NodeKind::Folder;
                let mut title = n.path.clone();
                if folder {
                    title.push('/');
                }
                if let Some(l0) = n.l0.as_deref().map(one_line).filter(|s| !s.is_empty()) {
                    title.push_str(" — ");
                    title.push_str(&l0);
                }
                let certainty = certainty_str(n.certainty);
                let reference = format!("n:{}", n.id);
                let block = hint_line(
                    "",
                    &title,
                    &[certainty.as_deref().unwrap_or_default()],
                    &reference,
                );
                overview.push(Cand {
                    target: i,
                    tref: TargetRef {
                        kind: TargetKind::Node,
                        id: n.id,
                    },
                    rev: n.current_rev,
                    reference,
                    kind: if folder { "folder" } else { "note" },
                    title: n.path.clone(),
                    certainty,
                    reason: "solution",
                    block,
                });
            }
        }
    }
    sections.push((Some("### Решение"), overview));

    // действующие gotcha/failure, revisit — решение и Knowledge; changelog — решение
    let mut insight_spaces: Vec<(usize, Id)> = sol.into_iter().collect();
    insight_spaces.extend(personal_space(targets, SpaceKind::Knowledge));
    let mut traps: Vec<(usize, Entry)> = Vec::new();
    for kind in ["gotcha", "failure"] {
        traps.extend(
            entries_of(health, deadline, &insight_spaces, |s| EntryFilter {
                statuses: vec![EntryStatus::Active],
                data_kind: Some(kind.to_string()),
                ..entry_filter(s, EntryKind::Insight)
            })
            .await?,
        );
    }
    traps.sort_by_key(|e| std::cmp::Reverse((e.1.created_at, e.1.id)));
    let traps = traps
        .iter()
        .map(|(i, e)| {
            let reason = if data_kind(e) == "failure" {
                "failure"
            } else {
                "gotcha"
            };
            entry_cand(*i, e, reason, "")
        })
        .collect();
    sections.push((Some("### Ловушки и сбои"), traps));
    let revisit = entries_of(health, deadline, &insight_spaces, |s| EntryFilter {
        statuses: vec![EntryStatus::Revisit],
        ..entry_filter(s, EntryKind::Insight)
    })
    .await?
    .iter()
    .map(|(i, e)| entry_cand(*i, e, "revisit", ""))
    .collect();
    sections.push((Some("### Пересмотреть (revisit)"), revisit));
    let changes = entries_of(
        health,
        deadline,
        &sol.into_iter().collect::<Vec<_>>(),
        |s| entry_filter(s, EntryKind::Changelog),
    )
    .await?
    .iter()
    .map(|(i, e)| entry_cand(*i, e, "changelog", ""))
    .collect();
    sections.push((Some("### Последние изменения"), changes));
    Ok((notices, sections))
}

/// `prompt`: внутренний `POST /search` (см. модуль) и отбор insight и заметок Rules.
async fn prompt_hints(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    req: &RecallReq,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
    let query: String = req
        .prompt
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(zam_store::search::QUERY_MAX_CHARS)
        .collect();
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let body = json!({
        "query": query,
        "scope": {"solution": req.solution, "include_knowledge": true, "include_rules": true},
        "filters": {"kinds": ["insight", "note"]},
        "mode": "brief",
        "top_k": PROMPT_SEARCH_TOP_K,
        "budget_tokens": PROMPT_SEARCH_BUDGET,
        "rerank": "off",
        "skip_seen": false,
        "no_cache": true,
        "format": "json",
    });
    // показанное учитывает сам `/recall`: поиск идёт без сессии
    let mut h = headers.clone();
    h.remove(SESSION_HEADER);
    if let Some(a) = zam_store::normalize_area(req.area.as_deref())
        && !h.contains_key(AREA_HEADER)
    {
        h.insert(
            AREA_HEADER,
            HeaderValue::from_str(&a)
                .map_err(|_| ApiError::BadRequest("area: печатные символы".into()))?,
        );
    }
    let search = super::search::search(
        State(st.clone()),
        p.clone(),
        h,
        Ok(Bytes::from(body.to_string())),
    );
    let Some(r) = deadline.step(search).await else {
        return Ok(Vec::new());
    };
    let resp = r?;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .map_err(|e| ApiError::Internal(format!("ответ поиска: {e}")))?;
    if status != StatusCode::OK {
        return Err(ApiError::Internal(format!("поиск подсказок: {status}")));
    }
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::Internal(format!("ответ поиска: {e}")))?;
    if let Some(ws) = v["warnings"].as_array() {
        health
            .warnings
            .extend(ws.iter().filter_map(Value::as_str).map(str::to_string));
    }
    let min = st.cfg.recall.prompt_min_score;
    let targets = health.targets();
    let mut out: Vec<Cand> = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        if it["score"].as_f64().unwrap_or(0.0) < min {
            continue;
        }
        let space_kind = it["space"]["kind"].as_str().unwrap_or_default();
        let kind = it["kind"].as_str().unwrap_or_default();
        let (tref, raw_id) = match (kind, it["entry_id"].as_str(), it["node_id"].as_str()) {
            ("insight", Some(id), _) => (TargetKind::Entry, id),
            ("note", _, Some(id)) if space_kind == "rules" => (TargetKind::Node, id),
            _ => continue,
        };
        let Ok(id) = raw_id.parse::<Id>() else {
            continue;
        };
        let Some(space) = it["space"]["id"]
            .as_str()
            .and_then(|s| s.parse::<Id>().ok())
        else {
            continue;
        };
        let Some(target) = targets
            .iter()
            .position(|t| t.spaces.iter().any(|s| s.id == space))
        else {
            continue;
        };
        let certainty = it["certainty"].as_str().map(str::to_string);
        let snippet = one_line(
            &it["snippet"]
                .as_str()
                .unwrap_or_default()
                .replace([zam_store::MARK_START, zam_store::MARK_END], ""),
        );
        let snippet = truncate_to_tokens(&snippet, SNIPPET_TOKENS, &count_tokens).to_string();
        let (reference, title, lead, tags, out_kind, reason) = match tref {
            TargetKind::Entry => (
                format!("i:{id}"),
                it["title"].as_str().unwrap_or_default().to_string(),
                "",
                it["insight_kind"].as_str().unwrap_or_default().to_string(),
                "insight",
                "search",
            ),
            TargetKind::Node => {
                let path = it["path"].as_str().unwrap_or_default();
                let hp = it["heading_path"].as_str().unwrap_or_default();
                let title = if hp.is_empty() {
                    path.to_string()
                } else {
                    format!("{path} › {hp}")
                };
                (
                    format!("n:{id}"),
                    title,
                    "правило ",
                    String::new(),
                    "note",
                    "rules",
                )
            }
        };
        let mut block = hint_line(
            lead,
            &title,
            &[tags.as_str(), certainty.as_deref().unwrap_or_default()],
            &reference,
        );
        if !snippet.is_empty() {
            block.push_str("\n  ");
            block.push_str(&snippet);
        }
        out.push(Cand {
            target,
            tref: TargetRef { kind: tref, id },
            rev: it["rev"].as_i64().unwrap_or(0),
            reference,
            kind: out_kind,
            title,
            certainty,
            reason,
            block,
        });
    }
    Ok(out)
}

/// `post_tool`: `error_regex`, затем FTS по insight (см. модуль).
async fn error_hints(
    req: &RecallReq,
    health: &mut Health<'_>,
    deadline: &mut Deadline,
) -> Result<Vec<Cand>, ApiError> {
    let raw = req.error.as_deref().unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut cut = raw.len().min(ERROR_SCAN_MAX);
    while !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    let scan = &raw[..cut];
    let targets = health.targets();
    let lead = "эта ошибка уже разбиралась: ";
    let mut out: Vec<Cand> = Vec::new();
    let mut taken: HashSet<TargetRef> = HashSet::new();

    // 1. error_regex действующих триггеров
    for (i, t) in targets.iter().enumerate() {
        let spaces = entry_spaces(t);
        if spaces.is_empty() || !health.alive[i] {
            continue;
        }
        let Some(r) = deadline.step(active_triggers(&t.db, &spaces)).await else {
            break;
        };
        let Some(rows) = health.soft(i, r)? else {
            continue;
        };
        for row in rows {
            if row.kind != TriggerKind::ErrorRegex
                || out.len() >= POST_TOOL_MAX
                || taken.contains(&TargetRef {
                    kind: TargetKind::Entry,
                    id: row.entry_id,
                })
            {
                continue;
            }
            if let (Some(tools), Some(tool)) = (&row.tools, req.tool.as_deref())
                && !tools.iter().any(|x| x == tool)
            {
                continue;
            }
            if !error_regex_matches(&row.pattern, scan) {
                continue;
            }
            let Some(r) = deadline.step(get_entry(&t.db, row.entry_id)).await else {
                break;
            };
            let e = match r {
                Ok(e) if e.deleted_at.is_none() => e,
                Ok(_) | Err(StoreError::NotFound) => continue,
                // сбой базы чужого решения исключает его, личной — ошибка запроса
                Err(e) => {
                    health.soft(i, Err::<(), _>(e))?;
                    break;
                }
            };
            taken.insert(TargetRef {
                kind: TargetKind::Entry,
                id: e.id,
            });
            out.push(entry_cand(i, &e, "error_regex", lead));
        }
    }

    // 2. FTS по insight по нормализованной ошибке
    let normalized = normalize_error(raw);
    let terms = error_terms(&normalized);
    if out.len() < POST_TOOL_MAX && !terms.is_empty() {
        let mut hits: Vec<(f64, usize, zam_store::FtsHit, bool)> = Vec::new();
        for (i, t) in targets.iter().enumerate() {
            let spaces = entry_spaces(t);
            if spaces.is_empty() || !health.alive[i] {
                continue;
            }
            let q = FtsQuery {
                text: normalized.clone(),
                space_ids: spaces,
                path_prefix: None,
                top_k: 100,
                targets: TargetFilter {
                    kinds: vec!["insight".into()],
                    statuses: vec!["active".into(), "revisit".into()],
                    area: None,
                    certainty: Vec::new(),
                },
            };
            let Some(r) = deadline.step(search_fts_channels(&t.db, q)).await else {
                break;
            };
            if let Some(ch) = health.soft(i, r)? {
                hits.extend(ch.bm25.into_iter().map(|h| (h.raw_score, i, h, ch.relaxed)));
            }
        }
        // bm25: меньше — лучше (слияние баз — по сырому баллу, как в поиске)
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, i, h, relaxed) in hits {
            if out.len() >= POST_TOOL_MAX {
                break;
            }
            let Some(e) = &h.entry else {
                continue;
            };
            if taken.contains(&h.target) {
                continue;
            }
            if relaxed
                && !coverage_enough(
                    term_coverage(&terms, &format!("{}\n{}", e.title, h.text)),
                    terms.len(),
                )
            {
                continue;
            }
            taken.insert(h.target);
            let reference = format!("i:{}", h.target.id);
            let kind = e.insight_kind.clone().unwrap_or_default();
            let block = hint_line(
                lead,
                &e.title,
                &[kind.as_str(), e.certainty.as_deref().unwrap_or_default()],
                &reference,
            );
            out.push(Cand {
                target: i,
                tref: h.target,
                rev: h.rev,
                reference,
                kind: "insight",
                title: e.title.clone(),
                certainty: e.certainty.clone(),
                reason: "error_fts",
                block,
            });
        }
    }
    Ok(out)
}

/// Ключ показа подсказки в `session_seen`.
fn seen_key(targets: &[SearchTarget], c: &Cand) -> SeenKey {
    SeenKey {
        owner_id: targets[c.target].owner,
        target: c.tref,
        part: RECALL_PART.to_string(),
    }
}

/// Уже показанные этой сессии подсказки (любая ревизия). Сбой чтения — WARN, без фильтра.
async fn shown(
    s: Option<&RequestSession>,
    targets: &[SearchTarget],
    cands: &[&Cand],
) -> HashSet<SeenKey> {
    let Some(s) = s else {
        return HashSet::new();
    };
    let keys: Vec<SeenKey> = cands.iter().map(|c| seen_key(targets, c)).collect();
    match seen_revs(&s.db, &s.touch.id, &keys).await {
        Ok(m) => m.into_keys().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "recall: показанное сессии не прочитано, без фильтра повторов");
            HashSet::new()
        }
    }
}

/// Учёт показа: `session_seen` (с касанием сессии) и `usage` в базах целей. Сбой — WARN.
async fn record(
    s: Option<&RequestSession>,
    targets: &[SearchTarget],
    user: Id,
    event: Event,
    shown: &[&Cand],
) {
    if let Some(s) = s {
        let items: Vec<SeenItem> = shown
            .iter()
            .map(|c| SeenItem {
                key: seen_key(targets, c),
                rev: c.rev,
            })
            .collect();
        let r = if items.is_empty() {
            touch_session(&s.db, &s.touch).await
        } else {
            mark_seen(&s.db, &s.touch, &items).await
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "recall: учёт сессии не выполнен");
        }
    }
    let mut by_target: HashMap<usize, Vec<UsageEvent>> = HashMap::new();
    for c in shown {
        by_target.entry(c.target).or_default().push(UsageEvent {
            user_id: user,
            session_id: s.map(|s| s.touch.id.clone()),
            target: c.tref,
            event: UsageKind::RecallShown,
            source: Some(event.source()),
        });
    }
    for (i, events) in by_target {
        if let Err(e) = record_usages(&targets[i].db, &events).await {
            tracing::warn!(error = %e, "recall: показы не записаны в usage");
        }
    }
}

/// `POST /recall`.
pub async fn recall(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let t0 = Instant::now();
    let req: RecallReq = json_body(body)?;
    let budget = req.budget_tokens.unwrap_or(st.cfg.recall.digest_tokens);
    if !(1..=BUDGET_MAX).contains(&budget) {
        return Err(ApiError::BadRequest(format!(
            "budget_tokens: от 1 до {BUDGET_MAX}"
        )));
    }
    let area = zam_store::normalize_area(req.area.as_deref());
    if area.as_ref().is_some_and(|a| {
        a.chars().count() > zam_store::AREA_MAX_CHARS || a.chars().any(char::is_control)
    }) {
        return Err(ApiError::BadRequest(format!(
            "area: до {} печатных символов",
            zam_store::AREA_MAX_CHARS
        )));
    }
    if let Some(g) = &req.solution
        && g.trim().parse::<zam_core::SolutionGuid>().is_err()
    {
        return Err(ApiError::BadRequest("solution: GUID решения".into()));
    }
    // сессия: некорректный id — 400; сбой её базы подсказки не валит (без учёта повторов)
    let mut session = match request_session(&st, &p, &headers, req.session_id.as_deref()).await {
        Ok(s) => s,
        Err(e @ ApiError::BadRequest(_)) => return Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "recall: база сессии недоступна, без учёта повторов");
            None
        }
    };
    if let Some(s) = &mut session {
        // известное из тела дописывается в сессию
        if let Some(g) = &req.solution {
            s.touch.solution_guid = g.trim().parse().ok();
        }
        if area.is_some() {
            s.touch.area = area.clone();
        }
    }
    let user = target_user_id(&st, &p, &headers)?;
    let mut sh = headers.clone();
    if req.solution.is_some() {
        // решение тела сильнее заголовка
        sh.remove(SOLUTION_HEADER);
    }
    let scope = ScopeReq {
        solution: req.solution.clone(),
        include_knowledge: Some(true),
        include_rules: Some(true),
        ..ScopeReq::default()
    };
    let (targets, warnings) = resolve_targets(&st, &p, &sh, &scope).await?;
    let mut health = Health::new(&targets, warnings);
    let mut deadline = Deadline {
        at: t0 + Duration::from_millis(st.cfg.recall.timeout_ms),
        expired: false,
    };

    // сбор кандидатов
    let (notices, sections, repeatable) = match req.event {
        Event::SessionStart => {
            let (n, s) = digest(&st, &p, &sh, &req, &mut health, &mut deadline).await?;
            (n, s, false)
        }
        Event::Prompt => {
            let c = prompt_hints(&st, &p, &sh, &req, &mut health, &mut deadline).await?;
            (Vec::new(), vec![(None, c)], false)
        }
        Event::PostTool => {
            let c = error_hints(&req, &mut health, &mut deadline).await?;
            (Vec::new(), vec![(None, c)], true)
        }
    };
    // без повторов: показанное сессии, повторы между разделами, исключённые цели
    let all: Vec<&Cand> = sections.iter().flat_map(|(_, c)| c).collect();
    let seen = if repeatable {
        HashSet::new()
    } else {
        shown(session.as_ref(), &targets, &all).await
    };
    let limit = match req.event {
        Event::SessionStart => usize::MAX,
        Event::Prompt => PROMPT_MAX,
        Event::PostTool => POST_TOOL_MAX,
    };
    let mut dup: HashSet<TargetRef> = HashSet::new();
    let mut packed: Vec<Section> = vec![Section {
        title: None,
        blocks: notices,
    }];
    let mut picked: Vec<Vec<&Cand>> = vec![Vec::new()];
    for (title, cands) in &sections {
        let per = match title {
            Some("### Правила") => RULES_MAX,
            Some("### Решение") => SOLUTION_ROOT_MAX,
            Some(_) => DIGEST_ENTRIES_MAX,
            None => limit,
        };
        let chosen: Vec<&Cand> = cands
            .iter()
            .filter(|c| health.alive[c.target])
            .filter(|c| !seen.contains(&seen_key(&targets, c)))
            .filter(|c| dup.insert(c.tref))
            .take(per)
            .collect();
        packed.push(Section {
            title: title.map(str::to_string),
            blocks: chosen.iter().map(|c| c.block.clone()).collect(),
        });
        picked.push(chosen);
    }
    let (text, taken) = pack_sections(&packed, budget, &count_tokens);
    let shown_now: Vec<&Cand> = taken
        .iter()
        .filter(|(si, _)| *si > 0)
        .map(|&(si, bi)| picked[si][bi])
        .collect();
    let collect_ms = ms(t0);

    // учёт показа
    let rt = Instant::now();
    record(session.as_ref(), &targets, user, req.event, &shown_now).await;
    let record_ms = ms(rt);

    let mut warnings = std::mem::take(&mut health.warnings);
    if deadline.expired {
        warnings.push(WARN_RECALL_TIMEOUT.into());
    }
    dedup_keep_order(&mut warnings);
    let items: Vec<Value> = shown_now
        .iter()
        .map(|c| {
            json!({
                "id": c.reference,
                "kind": c.kind,
                "title": c.title,
                "certainty": c.certainty,
                "reason": c.reason,
            })
        })
        .collect();
    let mut out = json!({
        "text": text,
        "items": items,
        "tokens": count_tokens(&text),
        "timings": {
            "collect_ms": collect_ms,
            "record_ms": record_ms,
            "total_ms": ms(t0),
        },
        "warnings": warnings,
    });
    if req.event == Event::SessionStart {
        // R5: настройки типов подсказок пользователя; хранения на сервере пока нет (задача 13)
        out["hooks"] = json!({
            "session_start": true,
            "prompt": true,
            "pre_tool": true,
            "post_tool": true,
            "checkpoint": true,
        });
    }
    Ok(Json(out))
}
