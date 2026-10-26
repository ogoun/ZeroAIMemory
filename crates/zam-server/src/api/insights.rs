//! Insights (§7.5, §10.3, §12): создание с дедупликацией, замещение, отзыв, пересмотр,
//! триггеры. Правила `certainty` ([`crate::certainty`]) проверяет хранилище в транзакции
//! каждой операции: без `certainty` insight агента — `hypothesis`, человека — `axiom`;
//! `axiom` создаёт, замещает и отзывает только человек (403), `theorem` агента — только с
//! `data.evidence {checked, method, result}` (422).
//!
//! **Создание** — `POST /spaces/{space}/entries` с `kind: "insight"` ([`create`]):
//! - поля §7.5: `title`, `body`, `data {kind, experiment?, evidence?, …}` (`data.kind` — один
//!   из [`INSIGHT_KINDS`]), `scope: solution|global`, `status: active|revisit`, `certainty`,
//!   `triggers`, `tags`, `area`, `message`, `links: [{type, target}]` (явные связи графа,
//!   §8.5: `target` — `node:<id>` | `entry:<id>` | `[[Имя]]`, в той же транзакции;
//!   [`super::graph::insight_links`]); плюс `force` и `supersedes_id`;
//! - `scope: global` — запись в Knowledge пользователя запроса (пространство пути проверяется
//!   только на чтение: отзыв доступа к решению — 403 и здесь); без `scope` — по пространству
//!   пути: решение — `solution`, Knowledge — `global`; Rules и Human — 422, если запись туда
//!   разрешена правами (администратор, `rules.agent_write`), иначе раньше — 403/404 доступа;
//! - дедупликация (без `force` и `supersedes_id`, [`dedup`]): тот же нормализованный
//!   заголовок у `active`/`revisit` той же области и — при доступном векторном канале —
//!   dense-косинус `embed_query(title + "\n" + body)` с insight той же области не ниже
//!   `insights.dedup_threshold` (`nearest_dense` по области базы владельца и пространству
//!   записи, без `superseded`/`retracted`; кандидаты перепроверяются по SQLite). Кандидаты
//!   есть → 409 `/problems/possible-duplicate` с `candidates` (до 5) и `dedup: "full" |
//!   "degraded"`; без модели — только заголовок и `dedup: "degraded"` и в успешном ответе;
//! - повтор с тем же `Idempotency-Key` дедупликацию не проходит (иначе запись нашла бы
//!   саму себя) — хранилище вернёт ту же запись (200) или 422 при другом содержимом;
//! - `supersedes_id` — замещение этой записи ([`zam_store::supersede_entry`]).
//!
//! Ответ — запись, её `triggers` и `dedup` (`full` | `degraded` | `skipped` при `force`,
//! `supersedes_id` и повторе по ключу) **[решение исполнителя]**; при замещении — ещё
//! `superseded` (старая запись).
//!
//! **Операции:** `POST /entries/{id}/supersede {новая запись}` → 201 `{entry, superseded}`;
//! `POST /entries/{id}/retract {reason}` (пустая причина → 422; отзыв `superseded` разрешён:
//! замещённый вывод тоже может оказаться ошибочным, а отзыв ничего не удаляет); правка
//! статуса (`active ↔ revisit`) и триггеров — `PATCH /entries/{id}` ([`super::entries`]).
use std::collections::HashMap;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_index::{VectorFilter, VectorScope};
use zam_store::{
    Certainty, DEDUP_CANDIDATES_MAX, Db, DedupCandidate, EdgeType, Entry, EntryKind, EntryStatus,
    NewEntry, NewTrigger, REASON_MAX_CHARS, SpaceKind, TRIGGERS_MAX, create_entry,
    entry_by_idempotency_key, entry_triggers, get_entry, normalize_area, retract_entry,
    supersede_entry, title_duplicates, validate_trigger,
};

use super::entries::{
    audit_details, check_size, check_title, empty_object, idempotency_key, unprocessable,
};
use super::graph::insight_links;
use super::json_body;
use super::nodes::audit_node;
use super::scope::{EntryScope, MemScope, Mode, entry_scope, live_entry, space_scope};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Виды инсайтов (`data.kind`, §7.5).
pub const INSIGHT_KINDS: [&str; 6] = [
    "gotcha",
    "failure",
    "discovery",
    "decision",
    "experiment",
    "pattern",
];
/// Итоги эксперимента (`data.experiment.result`).
const EXPERIMENT_RESULTS: [&str; 3] = ["confirmed", "refuted", "inconclusive"];
/// Ближайших точек на запрос дедупликации: у записи несколько чанков, часть кандидатов
/// отсеивается проверкой по SQLite. [решение исполнителя]
const DEDUP_NEAREST: usize = 20;

/// Тело создания или замещения insight (поля §7.5).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightReq {
    /// В `POST /spaces/{space}/entries` — `insight`; в `/supersede` необязателен.
    #[serde(default)]
    kind: Option<EntryKind>,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default = "empty_object")]
    data: Value,
    #[serde(default)]
    tags: Vec<String>,
    area: Option<String>,
    message: Option<String>,
    /// `solution` | `global`.
    scope: Option<String>,
    status: Option<EntryStatus>,
    certainty: Option<Certainty>,
    /// Сырые триггеры: разбираются по одному, чтобы 422 называл индекс.
    triggers: Option<Vec<Value>>,
    #[serde(default)]
    force: bool,
    supersedes_id: Option<String>,
    /// Явные связи графа `[{type, target}]`.
    links: Option<Vec<Value>>,
}

/// `data` insight: `kind` из [`INSIGHT_KINDS`]; `experiment` и `evidence`, если заданы, —
/// объекты; `experiment.result` — `confirmed | refuted | inconclusive`.
pub fn check_insight_data(data: &Value) -> Result<(), ApiError> {
    let o = data
        .as_object()
        .ok_or_else(|| unprocessable("data должно быть JSON-объектом"))?;
    let kind = o.get("kind").and_then(Value::as_str);
    if !kind.is_some_and(|k| INSIGHT_KINDS.contains(&k)) {
        return Err(unprocessable(format!(
            "data.kind: одно из {}",
            INSIGHT_KINDS.join(", ")
        )));
    }
    for k in ["experiment", "evidence"] {
        if o.get(k).is_some_and(|v| !v.is_null() && !v.is_object()) {
            return Err(unprocessable(format!("data.{k}: ожидается объект")));
        }
    }
    if let Some(r) = o
        .get("experiment")
        .and_then(|e| e.get("result"))
        .filter(|r| !r.is_null())
        && !r.as_str().is_some_and(|r| EXPERIMENT_RESULTS.contains(&r))
    {
        return Err(unprocessable(format!(
            "data.experiment.result: одно из {}",
            EXPERIMENT_RESULTS.join(", ")
        )));
    }
    Ok(())
}

/// Сырые триггеры → проверенные: не больше [`TRIGGERS_MAX`]; ошибка — 422 `triggers[i]: …`.
pub fn parse_triggers(raw: Vec<Value>) -> Result<Vec<NewTrigger>, ApiError> {
    if raw.len() > TRIGGERS_MAX {
        return Err(unprocessable(format!(
            "triggers: не больше {TRIGGERS_MAX} на запись"
        )));
    }
    raw.into_iter()
        .enumerate()
        .map(|(i, v)| {
            let t: NewTrigger = serde_json::from_value(v).map_err(|e| {
                unprocessable(format!(
                    "triggers[{i}]: {e} (ожидается {{type: file_glob|command_regex|error_regex|keyword, pattern, tools?}})"
                ))
            })?;
            validate_trigger(&t).map_err(|e| unprocessable(format!("triggers[{i}]: {e}")))?;
            Ok(t)
        })
        .collect()
}

/// Статус новой записи: только `active` или `revisit`.
fn check_new_status(s: Option<EntryStatus>) -> Result<(), ApiError> {
    match s {
        None | Some(EntryStatus::Active | EntryStatus::Revisit) => Ok(()),
        Some(s) => Err(unprocessable(format!(
            "status: новая запись — active или revisit, не {} (замещение и отзыв — отдельные операции)",
            s.as_str()
        ))),
    }
}

/// Режим дедупликации в ответе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupMode {
    /// Заголовок и векторный канал.
    Full,
    /// Только заголовок: векторный канал недоступен или не удался.
    Degraded,
    /// Не выполнялась: `force`, `supersedes_id`, повтор по ключу.
    Skipped,
}

impl DedupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DedupMode::Full => "full",
            DedupMode::Degraded => "degraded",
            DedupMode::Skipped => "skipped",
        }
    }
}

/// Кандидаты в дубли нового insight в пространстве `space` базы `db` (владелец `owner`)
/// области `area` и режим проверки (см. модуль). Сбой эмбеддинга или индекса — WARN
/// (без текста записи) и `Degraded`, а не ошибка.
pub async fn dedup(
    st: &AppState,
    db: &Db,
    owner: Id,
    space: Id,
    title: &str,
    body: &str,
    area: Option<&str>,
) -> Result<(Vec<DedupCandidate>, DedupMode), ApiError> {
    let area = normalize_area(area);
    let mut out = title_duplicates(db, space, title, area.as_deref()).await?;
    let (Ok(()), Some(emb), Some(vector)) = (
        st.index.vector_available(),
        st.index.embedder.clone(),
        st.index.vector.clone(),
    ) else {
        return Ok((out, DedupMode::Degraded));
    };
    let text = format!("{}\n{body}", title.trim());
    let e = match tokio::task::spawn_blocking(move || emb.embed_query(&text)).await {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "эмбеддинг insight для дедупликации не удался: только заголовок");
            return Ok((out, DedupMode::Degraded));
        }
        Err(j) => {
            tracing::error!(
                panic = j.is_panic(),
                "эмбеддинг insight для дедупликации аварийно завершился"
            );
            return Ok((out, DedupMode::Degraded));
        }
    };
    let filter = VectorFilter {
        scopes: vec![VectorScope {
            user_id: owner,
            space_ids: vec![space],
        }],
        kinds: vec!["insight".into()],
        exclude_statuses: vec!["superseded".into(), "retracted".into()],
        area: area.clone(),
        ..VectorFilter::default()
    };
    let hits = match vector.nearest_dense(&e.dense, &filter, DEDUP_NEAREST).await {
        Ok(h) => h,
        Err(err) => {
            tracing::warn!(error = %err, "векторный поиск дублей insight не удался: только заголовок");
            return Ok((out, DedupMode::Degraded));
        }
    };
    let threshold = st.cfg.insights.dedup_threshold;
    // лучшая оценка на запись (у записи несколько чанков), по убыванию
    let mut best: HashMap<Id, f32> = HashMap::new();
    for h in hits.iter().filter(|h| {
        h.user_id == owner && h.target.kind == TargetKind::Entry && h.score >= threshold
    }) {
        let s = best.entry(h.target.id).or_insert(h.score);
        *s = s.max(h.score);
    }
    let mut ranked: Vec<(Id, f32)> = best.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (id, score) in ranked {
        if let Some(c) = out.iter_mut().find(|c| c.id == id) {
            c.score = Some(score);
            continue;
        }
        if out.len() >= DEDUP_CANDIDATES_MAX {
            break;
        }
        // источник истины — SQLite: точка могла пережить правку или удаление
        let e = match get_entry(db, id).await {
            Ok(e) => e,
            Err(zam_store::StoreError::NotFound) => continue,
            Err(e) => return Err(e.into()),
        };
        let live = e.deleted_at.is_none()
            && e.kind == EntryKind::Insight
            && e.space_id == space
            && e.area == area;
        let status = match e.status {
            Some(s @ (EntryStatus::Active | EntryStatus::Revisit)) if live => s,
            _ => continue,
        };
        out.push(DedupCandidate {
            id: e.id,
            title: e.title,
            status,
            certainty: e.certainty,
            reason: "vector",
            score: Some(score),
        });
    }
    out.truncate(DEDUP_CANDIDATES_MAX);
    Ok((out, DedupMode::Full))
}

/// Запись insight с её триггерами как JSON (`triggers` — массив, у не-insight — нет поля).
pub async fn entry_json(db: &Db, e: &Entry) -> Result<Value, ApiError> {
    let mut v = serde_json::to_value(e).map_err(|e| ApiError::Internal(e.to_string()))?;
    if e.kind == EntryKind::Insight
        && let Value::Object(o) = &mut v
    {
        o.insert("triggers".into(), json!(entry_triggers(db, e.id).await?));
    }
    Ok(v)
}

/// Область записи: `global` — Knowledge пользователя запроса (пространство пути проверено
/// на чтение), иначе пространство пути (решение или Knowledge).
async fn target_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    space: &str,
    scope: Option<&str>,
) -> Result<MemScope, ApiError> {
    match scope {
        Some("global") => {
            space_scope(st, p, headers, space, Mode::Read).await?;
            space_scope(st, p, headers, "knowledge", Mode::Write).await
        }
        None | Some("solution") => {
            let sc = space_scope(st, p, headers, space, Mode::Write).await?;
            match (sc.space.kind, scope) {
                (SpaceKind::Solution, _) | (SpaceKind::Knowledge, None) => Ok(sc),
                (SpaceKind::Knowledge, Some(_)) => Err(unprocessable(
                    "scope: solution — запись в пространстве решения; для Knowledge — scope: global",
                )),
                _ => Err(unprocessable(
                    "insight ведётся в пространстве решения или в Knowledge (scope: global)",
                )),
            }
        }
        Some(other) => Err(unprocessable(format!(
            "scope: solution или global, не «{other}»"
        ))),
    }
}

/// Новая запись insight из тела запроса (проверенного [`check_request`]).
fn new_entry(
    req: InsightReq,
    space: Id,
    key: Option<String>,
    triggers: Vec<NewTrigger>,
    links: Vec<(EdgeType, TargetRef)>,
) -> NewEntry {
    NewEntry {
        space_id: space,
        kind: EntryKind::Insight,
        title: req.title,
        body: req.body,
        data: req.data,
        area: req.area,
        tags: req.tags,
        status: req.status,
        certainty: req.certainty,
        idempotency_key: key,
        triggers,
        links,
    }
}

/// Проверка полей insight (общая для создания и замещения): заголовок, `data`, размер тела,
/// статус, триггеры.
fn check_request(st: &AppState, req: &mut InsightReq) -> Result<Vec<NewTrigger>, ApiError> {
    if req.kind.is_some_and(|k| k != EntryKind::Insight) {
        return Err(unprocessable("kind: insight"));
    }
    check_title(&req.title)?;
    check_insight_data(&req.data)?;
    check_size(st, &req.body)?;
    check_new_status(req.status)?;
    parse_triggers(req.triggers.take().unwrap_or_default())
}

/// 409 `/problems/possible-duplicate` с кандидатами и режимом проверки.
fn duplicate(candidates: Vec<DedupCandidate>, mode: DedupMode) -> ApiError {
    let mut extra = Map::new();
    extra.insert("candidates".into(), json!(candidates));
    extra.insert("dedup".into(), json!(mode.as_str()));
    ApiError::PossibleDuplicate {
        detail: format!(
            "похоже на уже записанный insight ({} шт.): допишите к нему (PATCH append_body), \
             замените его (supersedes_id) или запишите всё равно (force: true)",
            candidates.len()
        ),
        extra,
    }
}

/// `POST /spaces/{space}/entries` с `kind: insight` (тело уже прочитано как JSON):
/// 201 / 200 (повтор по ключу) / 409 possible-duplicate / 422.
pub async fn create(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    client: &Client,
    space: &str,
    body: Value,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut req: InsightReq = serde_json::from_value(body)
        .map_err(|e| ApiError::BadRequest(format!("некорректное тело: {e}")))?;
    let sc = target_scope(st, p, headers, space, req.scope.as_deref()).await?;
    let triggers = check_request(st, &mut req)?;
    let raw_links = req.links.take().unwrap_or_default();
    let links = insight_links(
        st, p, headers, &sc.db, sc.owner, &sc.space, sc.role, raw_links,
    )
    .await?;
    let attrib = attribution(p, headers, req.message.take())?;
    let key = idempotency_key(headers)?;
    let repeat = match &key {
        Some(k) => entry_by_idempotency_key(&sc.db, &attrib, k)
            .await?
            .is_some(),
        None => false,
    };
    if let Some(raw) = req.supersedes_id.take() {
        let old = super::parse_id(&raw)?;
        let o = match get_entry(&sc.db, old).await {
            Ok(o) if o.deleted_at.is_none() => o,
            Ok(_) | Err(zam_store::StoreError::NotFound) => return Err(ApiError::NotFound),
            Err(e) => return Err(e.into()),
        };
        // в базе владельца чужого решения участнику не существует ничего вне пространства
        // решения (Knowledge владельца и т. п.) — 404, а не 422
        if o.space_id != sc.space.id && sc.owner != sc.user.id {
            return Err(ApiError::NotFound);
        }
        if o.space_id != sc.space.id || o.kind != EntryKind::Insight {
            return Err(unprocessable(
                "supersedes_id: insight того же пространства, что и новая запись",
            ));
        }
        let new = new_entry(req, sc.space.id, key, triggers, links);
        let (n, o) = supersede_entry(&sc.db, old, new, &attrib).await?;
        st.index.notify();
        // аудит — как у POST /entries/{id}/supersede (повтор по ключу — без новой строки)
        if !repeat {
            audit_node(
                st,
                p,
                client,
                sc.user.id,
                sc.owner,
                "entry.supersede",
                old,
                audit_details(sc.space.id, EntryKind::Insight),
            )
            .await?;
        }
        let mut v = entry_json(&sc.db, &n).await?;
        super::secrets::attach_entry(&mut v, &n);
        v["superseded"] =
            serde_json::to_value(&o).map_err(|e| ApiError::Internal(e.to_string()))?;
        v["dedup"] = json!(DedupMode::Skipped.as_str());
        let status = if repeat {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        };
        return Ok((status, Json(v)));
    }
    let mode = if req.force || repeat {
        DedupMode::Skipped
    } else {
        let area = req.area.clone().or_else(|| attrib.area.clone());
        let (cands, mode) = dedup(
            st,
            &sc.db,
            sc.owner,
            sc.space.id,
            &req.title,
            &req.body,
            area.as_deref(),
        )
        .await?;
        if !cands.is_empty() {
            return Err(duplicate(cands, mode));
        }
        mode
    };
    let new = new_entry(req, sc.space.id, key, triggers, links);
    let (e, created) = create_entry(&sc.db, new, &attrib).await?;
    if created {
        st.index.notify();
    }
    let mut v = entry_json(&sc.db, &e).await?;
    super::secrets::attach_entry(&mut v, &e);
    v["dedup"] = json!(mode.as_str());
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(v)))
}

/// Запись по id — живой insight (иначе 404 / 422).
fn live_insight(sc: &EntryScope) -> Result<(), ApiError> {
    live_entry(sc)?;
    if sc.entry.kind != EntryKind::Insight {
        return Err(unprocessable("операция только для insight"));
    }
    Ok(())
}

/// `POST /entries/{id}/supersede {новая запись}` → 201 `{entry, superseded}` (повтор по
/// `Idempotency-Key` — 200); старая — `superseded`, новая — со ссылкой на неё; ничего не
/// удаляется. Уже `superseded`/`retracted` → 409. Аудит `entry.supersede`.
pub async fn supersede(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live_insight(&sc)?;
    let mut req: InsightReq = json_body(body)?;
    if req.scope.is_some() || req.supersedes_id.is_some() {
        return Err(unprocessable(
            "scope и supersedes_id задаются замещаемой записью, а не телом",
        ));
    }
    let triggers = check_request(&st, &mut req)?;
    let raw_links = req.links.take().unwrap_or_default();
    let links = insight_links(
        &st, &p, &headers, &sc.db, sc.owner, &sc.space, sc.role, raw_links,
    )
    .await?;
    let attrib = attribution(&p, &headers, req.message.take())?;
    let key = idempotency_key(&headers)?;
    let repeat = match &key {
        Some(k) => entry_by_idempotency_key(&sc.db, &attrib, k)
            .await?
            .is_some(),
        None => false,
    };
    let new = new_entry(req, sc.space.id, key, triggers, links);
    let (n, o) = supersede_entry(&sc.db, sc.entry.id, new, &attrib).await?;
    st.index.notify();
    if !repeat {
        audit_node(
            &st,
            &p,
            &client,
            sc.user.id,
            sc.owner,
            "entry.supersede",
            sc.entry.id,
            audit_details(sc.space.id, sc.entry.kind),
        )
        .await?;
    }
    let status = if repeat {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let mut entry = entry_json(&sc.db, &n).await?;
    super::secrets::attach_entry(&mut entry, &n);
    Ok((
        status,
        Json(json!({
            "entry": entry,
            "superseded": entry_json(&sc.db, &o).await?,
        })),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetractReq {
    #[serde(default)]
    reason: Option<String>,
    message: Option<String>,
}

/// `POST /entries/{id}/retract {reason}` → 200 запись `retracted` с `retract_reason`;
/// пустая или длинная причина → 422; уже отозвана → 409. Отзыв `superseded` разрешён.
/// Аудит `entry.retract` (без причины: это текст записи).
pub async fn retract(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live_insight(&sc)?;
    let req: RetractReq = json_body(body)?;
    let reason = req.reason.unwrap_or_default();
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(unprocessable("reason: причина отзыва обязательна"));
    }
    if reason.chars().count() > REASON_MAX_CHARS {
        return Err(unprocessable(format!(
            "reason: не длиннее {REASON_MAX_CHARS} символов"
        )));
    }
    let attrib = attribution(&p, &headers, req.message)?;
    let e = retract_entry(&sc.db, sc.entry.id, reason, &attrib).await?;
    st.index.notify();
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "entry.retract",
        sc.entry.id,
        audit_details(sc.space.id, sc.entry.kind),
    )
    .await?;
    Ok(Json(entry_json(&sc.db, &e).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insight_data_rules() {
        assert!(check_insight_data(&json!({"kind": "gotcha"})).is_ok());
        assert!(
            check_insight_data(&json!({"kind": "experiment",
                "experiment": {"hypothesis": "h", "result": "refuted"},
                "evidence": {"checked": "c", "method": "m", "result": "r"}}))
            .is_ok()
        );
        for bad in [
            json!({}),
            json!({"kind": "fix"}),
            json!({"kind": "gotcha", "experiment": "x"}),
            json!({"kind": "gotcha", "evidence": []}),
            json!({"kind": "experiment", "experiment": {"result": "maybe"}}),
            json!([]),
        ] {
            assert!(check_insight_data(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn triggers_parsed_with_index() {
        let ok = parse_triggers(vec![
            json!({"type": "file_glob", "pattern": "**/*.sh", "tools": ["Write"]}),
            json!({"type": "keyword", "pattern": "crlf"}),
        ])
        .unwrap();
        assert_eq!(ok.len(), 2);
        for (raw, idx) in [
            (
                vec![
                    json!({"type": "file_glob", "pattern": "*.sh"}),
                    json!({"type": "glob", "pattern": "x"}),
                ],
                "triggers[1]",
            ),
            (
                vec![json!({"type": "command_regex", "pattern": "(a)\\1"})],
                "triggers[0]",
            ),
            (
                vec![json!({"type": "keyword", "pattern": "x", "extra": 1})],
                "triggers[0]",
            ),
        ] {
            match parse_triggers(raw) {
                Err(ApiError::Unprocessable(m)) => assert!(m.starts_with(idx), "{m}"),
                other => panic!("{:?}", other.map(|v| v.len())),
            }
        }
        assert!(parse_triggers(vec![json!({"type": "keyword", "pattern": "x"}); 33]).is_err());
    }
}
