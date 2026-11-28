//! Предложения человеку — кандидаты в Rules (M7.3 §6.7 пп. 2, 4, 5; признак `proposals`).
//!
//! - `POST /proposals {type: "rule", path, content, reason, basis?, session_id?}` — спонтанный
//!   кандидат агента (MCP `memory_write … candidate`, `zam rules candidate`): только токен `rw`
//!   (токен `ro` — 403; человек пишет заметку Rules сам — 403); проверка — общая с элементом
//!   работы вида `rules` ([`crate::works::rules::check_candidate`]): путь Rules — 400, текст или
//!   довод — 422, путь занят — 409 `code: path_taken` с советом другого пути, почти-дубль заметки
//!   Rules — 409 `code: rule_duplicate` с `similar`; тот же путь и текст — прежнее предложение
//!   (200 `existing: true`, статус — каким он стал); открытых кандидатов
//!   [`zam_store::proposals::PROPOSALS_OPEN_MAX`] — 409 `/problems/proposals-full`. Ответ 201
//!   `{proposal: "o:<id>", status: "proposed", existing: false}`. Автор — сессия запроса
//!   (`X-Zam-Session`, `session_id`), если есть.
//! - `GET /proposals?type=&status=&author=&limit=` → `{proposals: […]}`: `type` — `rule` (по
//!   умолчанию; иное — 400), `status` — один из статусов (без него — все, кроме `skipped`: отказ
//!   исполнителя человеку не показывается), `author` — `me` (агенты пользователя запроса, то же,
//!   что `agent`) | `agent` | `model`; новые первыми, `limit` 1..=200 (50).
//! - `GET /proposals/counts` → `{rules}` — открытых кандидатов (метка дерева «кандидаты: N»).
//! - `GET /proposals/{id}` (`o:<id>` или id) → карточка и `rules_tokens {now, after}` (оценка
//!   Rules в дайджесте сейчас и с кандидатом, §6.7 п. 4), `sources` (ссылки основания с
//!   заголовками; токену заметки Human и отчёты — как несуществующие).
//! - `POST /proposals/{id}/accept {path?, content?}` — **только сессия WebUI** (токену — 403):
//!   путь и текст можно поправить (человек вправе дописать адрес или пример — решение 58; маска
//!   секрета — 422); заметка Rules создаётся **тем же кодом, что заметка WebUI**
//!   (`nodes::write::create_note_in`), автор — человек, сообщение ревизии — «из предложения
//!   o:<id> (model:<имя> | сессия <id>)» (решение 60); путь занят — 409 `code: path_taken`;
//!   предложение `accepted` с `result_ref: n:<id>`; не `proposed` — 409 `code: not_open`.
//!   Ответ `{proposal, note}`. Аудит `proposal.accept {id, type, result}` — без текста.
//! - `POST /proposals/{id}/decline {note?}` — только сессия WebUI: `declined` (источник работы
//!   больше не предлагается ни в какой ревизии — решение 64); аудит `proposal.decline {id,
//!   type}`.
//! - `POST /proposals/{id}/withdraw` — автор отзывает своё: токен `rw` пользователя, предложение
//!   агента (`author_kind = agent`); человеку — 403 (он отклоняет), предложению модели — 403.
//!
//!
//! **Уборка** (M7.5 §8.7, тип `tidy`, признак `tidy`): предложения создаёт только внутренняя
//! работа вида `tidy` (`POST /proposals` с `type: tidy` — 400). Карточка — ещё `candidate`,
//! `signals`, `records` (ссылка, ревизия, L1, `human_decision` у `axiom`), `ops`, `op` и `stale`
//! (запись изменилась после предложения: «Применить» — 409). «Применить» — `POST
//! /proposals/{id}/accept {text?}` (только сессия WebUI; `text` — правка человеком текста `merge`
//! или `append`): ревизии всех записей сверяются до первой операции — не та — предложение
//! `stale`, 409 `code: stale`; операция — **тем же кодом, что у человека в WebUI** (замещение,
//! отзыв, `PATCH` записи, создание термина), от имени человека, сообщение ревизии «из
//! предложения o:<id> (model:<имя> | сессия <id>)» ([`accept_tidy`]). Удаления нет. «Отклонить» —
//! `declined`: тот же кандидат по ссылкам больше не предлагается ни в какой ревизии.
//!
//! Ответы токену — с маской `‹secret›` (вид агента); человеку — исходник. Предложения —
//! служебные данные (ссылка `o:`): `memory_read` их не читает, в экспорт и архив пространства не
//! входят; маршруты — вне `memory_routes` (предложение само ничего не меняет в памяти).

use std::sync::LazyLock;

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::proposals::{
    self as pr, Decided, Decision, DecisionKind, InsertOutcome, ListFilter, NewProposal, Proposal,
    ProposalAuthor, ProposalStatus, ProposalType,
};

use super::agent_view;
use super::nodes::write::{NoteReq, create_note_in};
use super::scope::{Mode, require_rw, space_scope};
use super::sessions::{McpOrigin, request_session};
use super::{json_body, query};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;
use crate::works::package::{MASK_LITERAL_REASON, check_mask_literal};
use crate::works::rules::{self, RuleError};

/// Предложений в ответе `GET /proposals` по умолчанию.
pub const LIST_LIMIT_DEFAULT: usize = 50;
/// `detail` ответа 403 токену `ro` (§6.7 п. 2).
pub const RW_REQUIRED_DETAIL: &str = "кандидат в Rules требует токен rw";
/// `detail` ответа 403 токену на «Принять» и «Отклонить» (§6.7 п. 4).
pub const HUMAN_ONLY_DETAIL: &str = "кандидата в Rules принимает и отклоняет только человек \
     (WebUI «Кандидаты в Rules»); своё агент отзывает — POST /proposals/{id}/withdraw";

/// Решения по предложениям — по одному: у принятия создание заметки и решение — два шага, и
/// отклонение или отзыв между ними оставили бы заметку при ответе 409 (ревью r1, Minor 1); второе
/// решение того же кандидата ждёт первое и видит его итог.
static DECIDING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// Предложение по ссылке `o:<id>` или id.
fn proposal_id(raw: &str) -> Result<Id, ApiError> {
    let s = raw.trim();
    s.strip_prefix("o:")
        .unwrap_or(s)
        .parse()
        .map_err(|_| ApiError::BadRequest("предложение: o:<id>".into()))
}

/// Тип по строке запроса: `rule` (по умолчанию) | `tidy`; иное — 400.
fn parse_type(raw: Option<&str>) -> Result<ProposalType, ApiError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(ProposalType::Rule),
        Some(s) => {
            ProposalType::parse(s).ok_or_else(|| ApiError::BadRequest("type: rule | tidy".into()))
        }
    }
}

/// Предложение как JSON (карточка списка); токену — с маской. У `tidy` — ещё поля кандидата
/// уборки из `payload` (`candidate`, `signals`, `records`, `ops`, `op`); `stale` ставит
/// вызывающий ([`mark_live`]).
pub fn proposal_json(p: &Principal, x: &Proposal) -> Value {
    let ts = |t: Option<jiff::Timestamp>| t.map(zam_store::time::fmt_ts);
    let mut v = json!({
        "id": format!("o:{}", x.id),
        "type": x.ptype.as_str(),
        "status": x.status.as_str(),
        "space": x.space_id,
        "path": x.path(),
        "content": x.content(),
        "reason": x.reason,
        "basis": x.basis,
        "targets": x.targets,
        "author_kind": x.author_kind.as_str(),
        "author": x.author,
        "work": x.work_id.map(|w| format!("j:{w}")),
        "session": x.session_id,
        "shell": rules::has_shell(x.content()),
        "created_at": zam_store::time::fmt_ts(x.created_at),
        "decided_at": ts(x.decided_at),
        "decided_by": x.decided_by,
        "decision_note": x.decision_note,
        "result_ref": x.result_ref,
    });
    if x.ptype == ProposalType::Tidy {
        for k in ["candidate", "signals", "records", "ops", "op"] {
            v[k] = x.payload.get(k).cloned().unwrap_or(Value::Null);
        }
        v["stale"] = json!(x.status == ProposalStatus::Stale);
    }
    if agent_view::masks(p) {
        agent_view::mask_value(&mut v);
    }
    v
}

/// Пометка `stale` карточек уборки `cards` (по порядку `items`): предложение `stale` или
/// открытое, чья запись удалена или сменила ревизию (одно чтение на выборку).
async fn mark_live(
    db: &zam_store::Db,
    items: &[Proposal],
    cards: &mut [Value],
) -> Result<(), ApiError> {
    let refs: Vec<(String, i64)> = items
        .iter()
        .filter(|x| x.ptype == ProposalType::Tidy && x.status == ProposalStatus::Proposed)
        .flat_map(|x| x.targets.iter().map(|t| (t.target.clone(), t.rev)))
        .collect();
    if refs.is_empty() {
        return Ok(());
    }
    let live = zam_store::works::live_refs(db, &refs).await?;
    for (x, v) in items.iter().zip(cards.iter_mut()) {
        if x.ptype == ProposalType::Tidy && x.status == ProposalStatus::Proposed {
            let ok = x
                .targets
                .iter()
                .all(|t| live.contains(&(t.target.clone(), t.rev)));
            v["stale"] = json!(!ok);
        }
    }
    Ok(())
}

/// База пользователя запроса.
async fn user_db(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
) -> Result<(Id, zam_store::Db), ApiError> {
    let user = crate::auth::target_user(st, p, headers).await?.id;
    Ok((user, st.users.get(user).await?))
}

/// Предложение `raw` базы `db`; нет или неизвестного типа — 404.
async fn load(db: &zam_store::Db, raw: &str) -> Result<Proposal, ApiError> {
    pr::get(db, proposal_id(raw)?)
        .await?
        .ok_or(ApiError::NotFound)
}

fn not_open(x: &Proposal) -> ApiError {
    let mut extra = serde_json::Map::new();
    extra.insert("code".into(), json!("not_open"));
    extra.insert("status".into(), json!(x.status.as_str()));
    ApiError::Conflict {
        detail: format!(
            "предложение o:{} — {}: решение принимается только по открытому (proposed)",
            x.id,
            x.status.as_str()
        ),
        extra,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    #[serde(rename = "type")]
    ptype: String,
    path: String,
    content: String,
    reason: String,
    #[serde(default)]
    basis: Vec<String>,
    session_id: Option<String>,
}

/// Ссылки основания: до [`rules::BASIS_MAX`], каждая — `<буква>:<id>` (или `file:`/`commit:`),
/// без повторов.
fn check_basis(basis: Vec<String>) -> Result<Vec<String>, ApiError> {
    if basis.len() > rules::BASIS_MAX {
        return Err(ApiError::BadRequest(format!(
            "basis: не больше {} ссылок",
            rules::BASIS_MAX
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for b in basis {
        let b = b.trim().to_string();
        let ok = match b.split_once(':') {
            Some(("file" | "commit", rest)) => !rest.is_empty() && rest.len() <= 512,
            Some((letter, id)) => {
                letter.len() == 1
                    && letter.chars().all(|c| c.is_ascii_lowercase())
                    && id.parse::<Id>().is_ok()
            }
            None => false,
        };
        if !ok {
            return Err(ApiError::BadRequest(format!(
                "basis: «{b}» — ссылка вида i:<id>, n:<id>, file:<путь> или commit:<sha>"
            )));
        }
        if !out.contains(&b) {
            out.push(b);
        }
    }
    Ok(out)
}

/// `POST /proposals` → 201 (новое) | 200 (прежнее).
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    origin: Option<Extension<McpOrigin>>,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if p.is_human() {
        return Err(ApiError::Denied(
            "кандидата в Rules предлагает агент; человек пишет заметку Rules сам".into(),
        ));
    }
    require_rw(&p).map_err(|_| ApiError::Denied(RW_REQUIRED_DETAIL.into()))?;
    let b: CreateBody = json_body(body)?;
    if parse_type(Some(&b.ptype))? != ProposalType::Rule {
        return Err(ApiError::BadRequest(
            "type: rule (кандидатов уборки предлагает внутренняя работа tidy)".into(),
        ));
    }
    let basis = check_basis(b.basis)?;
    let session = request_session(
        &st,
        &p,
        &headers,
        origin.as_deref(),
        b.session_id.as_deref(),
    )
    .await?
    .map(|s| s.touch.id);
    let (user, db) = user_db(&st, &p, &headers).await?;
    let c = rules::check_candidate(&st, &db, user, &b.path, &b.content, &b.reason).await?;
    let space = rules::rules_space(&db).await?;
    let new = NewProposal {
        space,
        ptype: ProposalType::Rule,
        status: ProposalStatus::Proposed,
        targets: Vec::new(),
        payload: json!({"path": c.path.as_str(), "content": c.content}),
        reason: Some(c.reason),
        basis,
        dedup_key: Some(pr::key_text(
            ProposalType::Rule,
            c.path.as_str(),
            &c.content,
        )),
        author_kind: ProposalAuthor::Agent,
        author: session.clone(),
        work_id: None,
        session_id: session,
    };
    match pr::insert(&db, new).await? {
        InsertOutcome::Inserted(id) => Ok((
            StatusCode::CREATED,
            Json(json!({"proposal": format!("o:{id}"), "status": "proposed", "existing": false})),
        )),
        InsertOutcome::Existing(id) => {
            let status = pr::get(&db, id)
                .await?
                .map_or("proposed", |x| x.status.as_str());
            Ok((
                StatusCode::OK,
                Json(json!({"proposal": format!("o:{id}"), "status": status, "existing": true})),
            ))
        }
        InsertOutcome::Full => Err(ApiError::ProposalsFull(format!(
            "открытых кандидатов в Rules — {}: дождитесь решения человека (WebUI «Кандидаты в \
             Rules») или отзовите своё (POST /proposals/{{id}}/withdraw)",
            pr::PROPOSALS_OPEN_MAX
        ))),
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(rename = "type")]
    ptype: Option<String>,
    status: Option<String>,
    author: Option<String>,
    limit: Option<usize>,
}

/// `GET /proposals?type=&status=&author=&limit=` → `{proposals}`.
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let ptype = parse_type(q.ptype.as_deref())?;
    let status = match q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(ProposalStatus::parse(s).ok_or_else(|| {
            ApiError::BadRequest(
                "status: proposed | accepted | declined | withdrawn | stale | skipped".into(),
            )
        })?),
    };
    let author_kind = match q.author.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some("me" | "agent") => Some(ProposalAuthor::Agent),
        Some("model") => Some(ProposalAuthor::Model),
        Some(_) => return Err(ApiError::BadRequest("author: me | agent | model".into())),
    };
    let limit = q.limit.unwrap_or(LIST_LIMIT_DEFAULT);
    if !(1..=pr::LIST_MAX).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "limit: от 1 до {}",
            pr::LIST_MAX
        )));
    }
    let (_, db) = user_db(&st, &p, &headers).await?;
    let items = pr::list(
        &db,
        ListFilter {
            ptype,
            status,
            author_kind,
            limit,
        },
    )
    .await?;
    let mut cards: Vec<Value> = items.iter().map(|x| proposal_json(&p, x)).collect();
    mark_live(&db, &items, &mut cards).await?;
    Ok(Json(json!({ "proposals": cards })))
}

/// `GET /proposals/counts` → `{rules, tidy}`.
pub async fn counts(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, db) = user_db(&st, &p, &headers).await?;
    let c = pr::counts(&db).await?;
    Ok(Json(json!({"rules": c.rules, "tidy": c.tidy})))
}

/// Ссылки основания и источников с заголовками (записи журнала и заметки базы). Ссылки `basis`
/// задаёт агент, поэтому доступ — по принципалу (ревью r1, Important 2): токену заметки и записи
/// пространств, которых он не читает (Human), и скрытые записи (`hidden_entry`: отчёт) — как
/// несуществующие, `{ref, rev, live: false}` без пути, вида и заголовка.
async fn sources(p: &Principal, db: &zam_store::Db, x: &Proposal) -> Result<Vec<Value>, ApiError> {
    let spaces: std::collections::HashMap<Id, zam_store::Space> = zam_store::list_spaces(db)
        .await?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    let readable = |space: Id| {
        spaces
            .get(&space)
            .is_some_and(|s| super::scope::token_can_read(p, s))
    };
    let mut refs: Vec<(String, Option<i64>)> = x
        .targets
        .iter()
        .map(|t| (t.target.clone(), Some(t.rev)))
        .collect();
    for b in &x.basis {
        if !refs.iter().any(|(r, _)| r == b) {
            refs.push((b.clone(), None));
        }
    }
    let mut out = Vec::new();
    for (r, rev) in refs {
        let (letter, raw) = r.split_once(':').unwrap_or(("", ""));
        let id = raw.parse::<Id>().ok();
        let found = match (letter, id) {
            ("n", Some(id)) => match zam_store::get_node(db, id).await {
                Ok(n) if n.deleted_at.is_none() && readable(n.space_id) => {
                    Some((n.path, n.current_rev, "note".into()))
                }
                Ok(_) | Err(zam_store::StoreError::NotFound) => None,
                Err(e) => return Err(e.into()),
            },
            (l, Some(id)) if zam_store::is_entry_ref_letter(l) => {
                match zam_store::get_entry(db, id).await {
                    Ok(e)
                        if e.deleted_at.is_none()
                            && readable(e.space_id)
                            && !super::scope::hidden_entry(p, &e) =>
                    {
                        Some((e.title, e.rev, e.kind.as_str().to_string()))
                    }
                    Ok(_) | Err(zam_store::StoreError::NotFound) => None,
                    Err(e) => return Err(e.into()),
                }
            }
            _ => None,
        };
        out.push(match found {
            Some((title, cur, kind)) => {
                json!({"ref": r, "rev": rev, "current_rev": cur, "kind": kind, "title": title,
                       "live": true})
            }
            None => json!({"ref": r, "rev": rev, "live": false}),
        });
    }
    Ok(out)
}

/// `GET /proposals/{id}` → карточка, `rules_tokens {now, after}`, `sources`.
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (_, db) = user_db(&st, &p, &headers).await?;
    let x = load(&db, &raw).await?;
    let mut v = proposal_json(&p, &x);
    if x.ptype == ProposalType::Rule {
        let space = rules::rules_space(&db).await?;
        // прирост — только у открытого: принятый уже в сумме своей заметкой
        let add = (x.status == ProposalStatus::Proposed).then(|| (x.path(), x.content()));
        let (now, after) = rules::rules_tokens(&db, space, add).await?;
        v["rules_tokens"] = json!({"now": now, "after": after});
    } else {
        mark_live(&db, std::slice::from_ref(&x), std::slice::from_mut(&mut v)).await?;
    }
    let mut src = Value::from(sources(&p, &db, &x).await?);
    if agent_view::masks(&p) {
        agent_view::mask_value(&mut src);
    }
    v["sources"] = src;
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AcceptBody {
    /// Кандидат в Rules: путь заметки.
    path: Option<String>,
    /// Кандидат в Rules: текст правила.
    content: Option<String>,
    /// Кандидат уборки: текст `merge` или `append`, поправленный человеком.
    text: Option<String>,
}

/// Тело или пусто (`{}` по умолчанию).
fn body_or_default<T: Default + serde::de::DeserializeOwned>(
    body: Result<Bytes, BytesRejection>,
) -> Result<T, ApiError> {
    match &body {
        Ok(b) if b.iter().all(u8::is_ascii_whitespace) => Ok(T::default()),
        _ => json_body(body),
    }
}

/// Путь занят к моменту принятия — 409 с подсказкой передать другой `path`.
fn path_taken(e: RuleError) -> ApiError {
    match e {
        RuleError::PathTaken { path, node } => {
            let mut extra = serde_json::Map::new();
            extra.insert("code".into(), json!("path_taken"));
            extra.insert("path".into(), json!(path));
            extra.insert("node".into(), json!(format!("n:{node}")));
            ApiError::Conflict {
                detail: format!(
                    "путь rules:/{path} уже занят заметкой n:{node} — передайте другой path \
                     (POST /proposals/{{id}}/accept {{path, content}}) или дополните ту заметку"
                ),
                extra,
            }
        }
        e => e.into(),
    }
}

/// `POST /proposals/{id}/accept {path?, content?}` → `{proposal, note}`.
pub async fn accept(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    if !p.is_human() {
        return Err(ApiError::Denied(HUMAN_ONLY_DETAIL.into()));
    }
    let b: AcceptBody = body_or_default(body)?;
    let (user, db) = user_db(&st, &p, &headers).await?;
    let _one = DECIDING.lock().await;
    let x = load(&db, &raw).await?;
    if x.status != ProposalStatus::Proposed {
        return Err(not_open(&x));
    }
    if x.ptype == ProposalType::Tidy {
        if b.path.is_some() || b.content.is_some() {
            return Err(ApiError::BadRequest(
                "path и content — у кандидата в Rules; у уборки — text (merge, append)".into(),
            ));
        }
        return accept_tidy(&st, client, &p, &headers, user, &db, x, b.text).await;
    }
    if b.text.is_some() {
        return Err(ApiError::BadRequest(
            "text — у кандидата уборки; у кандидата в Rules — path и content".into(),
        ));
    }
    let path = rules::rule_path(b.path.as_deref().unwrap_or(x.path()))?;
    let content = b
        .content
        .unwrap_or_else(|| x.content().to_string())
        .trim()
        .to_string();
    if content.is_empty() {
        return Err(ApiError::Unprocessable(
            "content: пустой текст правила".into(),
        ));
    }
    // текст с маской секрета принять нельзя (решение 59): заглушка заменила бы значение
    if check_mask_literal(&content).is_err() {
        return Err(ApiError::Unprocessable(MASK_LITERAL_REASON.into()));
    }
    let space = rules::rules_space(&db).await?;
    rules::check_free(&db, space, &path)
        .await
        .map_err(path_taken)?;
    let sc = space_scope(&st, &p, &headers, "rules", Mode::Write).await?;
    let note = NoteReq {
        path: path.clone(),
        content,
        l0: None,
        message: Some(rules::origin_message(&x)),
        key: None,
    };
    let (node, _, note_json) = match create_note_in(&st, &p, &headers, &sc, note).await {
        Ok(r) => r,
        // путь заняли между проверкой и записью
        Err(e @ ApiError::Conflict { .. }) => {
            return Err(match rules::check_free(&db, space, &path).await {
                Err(taken @ RuleError::PathTaken { .. }) => path_taken(taken),
                _ => e,
            });
        }
        Err(e) => return Err(e),
    };
    let result = format!("n:{}", node.id);
    let d = Decision {
        to: DecisionKind::Accepted,
        by: p.actor(),
        note: None,
        result_ref: Some(result.clone()),
    };
    let x = match pr::decide(&db, x.id, d).await? {
        Decided::Done(x) => x,
        Decided::NotOpen(x) => return Err(not_open(&x)),
    };
    st.system
        .audit(
            &p.actor(),
            Some(user),
            "proposal.accept",
            Some(&format!("o:{}", x.id)),
            Some(&client.ip.to_string()),
            json!({"id": format!("o:{}", x.id), "type": x.ptype.as_str(), "result": result}),
        )
        .await?;
    Ok(Json(
        json!({"proposal": proposal_json(&p, &x), "note": note_json}),
    ))
}

/// 409 «цель изменилась после предложения» (`code: stale`; кандидат вернётся следующей ночью).
fn stale_conflict(x: &Proposal) -> ApiError {
    let mut extra = serde_json::Map::new();
    extra.insert("code".into(), json!("stale"));
    extra.insert("status".into(), json!(x.status.as_str()));
    ApiError::Conflict {
        detail: format!(
            "предложение o:{}: запись изменилась после предложения (ревизия не та) — оно \
             устарело, кандидат вернётся следующей ночью с новыми ревизиями",
            x.id
        ),
        extra,
    }
}

/// Устаревшая ревизия записи у операции (409 хранилища с `rev`).
fn is_stale_rev(e: &ApiError) -> bool {
    matches!(e, ApiError::Conflict { extra, .. } if extra.contains_key("rev"))
}

/// Заголовки вызова операции от имени человека: заголовки запроса «Применить» (пользователь
/// запроса), `If-Match` — ревизия записи из предложения, без `Idempotency-Key`.
fn op_headers(headers: &HeaderMap, rev: i64) -> HeaderMap {
    let mut h = headers.clone();
    h.remove("idempotency-key");
    if let Ok(v) = axum::http::HeaderValue::from_str(&rev.to_string()) {
        h.insert(axum::http::header::IF_MATCH, v);
    }
    h
}

/// Тело JSON вызова операции.
fn op_body(v: Value) -> Result<Bytes, BytesRejection> {
    Ok(Bytes::from(v.to_string()))
}

/// Id записи по ссылке `i:<id>` / `t:<id>`.
fn ref_id(r: &str) -> Result<String, ApiError> {
    r.split_once(':')
        .map(|(_, id)| id.to_string())
        .ok_or_else(|| ApiError::Internal(format!("предложение уборки: ссылка «{r}»")))
}

/// Ранг достоверности для «меньшей из двух»: `hypothesis` < `theorem` < `axiom`.
fn certainty_rank(c: zam_store::Certainty) -> u8 {
    match c {
        zam_store::Certainty::Hypothesis => 0,
        zam_store::Certainty::Theorem => 1,
        zam_store::Certainty::Axiom => 2,
    }
}

/// Достоверность замещающей записи `merge` (§8.7 п. 3): меньшая из `keep` и `drop` (запись без
/// статуса в минимуме не участвует; обе без статуса — без статуса), но не выше `theorem`, если
/// человек не правил текст.
pub fn merged_certainty(
    keep: Option<zam_store::Certainty>,
    drop: Option<zam_store::Certainty>,
    edited: bool,
) -> Option<zam_store::Certainty> {
    let min = [keep, drop]
        .into_iter()
        .flatten()
        .min_by_key(|c| certainty_rank(*c))?;
    if !edited && min == zam_store::Certainty::Axiom {
        return Some(zam_store::Certainty::Theorem);
    }
    Some(min)
}

/// Контекст операции уборки от имени человека.
struct OpCtx<'a> {
    st: &'a AppState,
    client: Client,
    p: &'a Principal,
    headers: &'a HeaderMap,
    /// «из предложения o:<id> (model:<имя> | сессия <id>)».
    message: String,
}

impl OpCtx<'_> {
    /// Причина ревизии записи: `reason` и происхождение (у записей журнала сообщение ревизии —
    /// её причина), не длиннее `REASON_MAX_CHARS` (обрезается `reason`).
    fn reason(&self, reason: &str) -> String {
        let tail = format!(" — {}", self.message);
        let room = zam_store::REASON_MAX_CHARS.saturating_sub(tail.chars().count());
        let head: String = reason.trim().chars().take(room).collect();
        if head.is_empty() {
            self.message.clone()
        } else {
            format!("{head}{tail}")
        }
    }

    /// Отзыв записи `target` ревизии `rev` с причиной `reason` и происхождением (как `POST
    /// /entries/{id}/retract`).
    async fn retract(&self, target: &str, rev: i64, reason: &str) -> Result<(), ApiError> {
        super::insights::retract(
            State(self.st.clone()),
            self.p.clone(),
            op_headers(self.headers, rev),
            self.client,
            Path(ref_id(target)?),
            op_body(json!({"reason": self.reason(reason), "message": self.message})),
        )
        .await
        .map(|_| ())
    }

    /// Правка записи `target` ревизии `rev` (как `PATCH /entries/{id}`): поля `fields`, причина
    /// ревизии `reason` с происхождением.
    async fn patch(
        &self,
        target: &str,
        rev: i64,
        reason: &str,
        mut fields: Value,
    ) -> Result<(), ApiError> {
        fields["message"] = json!(self.message);
        fields["reason"] = json!(self.reason(reason));
        super::entries::patch(
            State(self.st.clone()),
            self.p.clone(),
            op_headers(self.headers, rev),
            Path(ref_id(target)?),
            op_body(fields),
        )
        .await
        .map(|_| ())
    }
}

/// «Применить» кандидата уборки `x` (§8.7 п. 3; только сессия WebUI — проверено вызывающим):
/// сверка ревизий **всех** записей до первой операции (не та — `stale`, 409), операция тем же
/// кодом, что у человека в WebUI, от его имени с сообщением ревизии «из предложения o:<id> (…)»;
/// предложение `accepted` с `result_ref`, аудит `proposal.accept`. `text` — правка человеком
/// текста `merge` или `append`. Удаления нет.
#[allow(clippy::too_many_arguments)]
async fn accept_tidy(
    st: &AppState,
    client: Client,
    p: &Principal,
    headers: &HeaderMap,
    user: Id,
    db: &zam_store::Db,
    x: Proposal,
    text: Option<String>,
) -> Result<Json<Value>, ApiError> {
    use zam_store::tidy::Op;
    let refs: Vec<(String, i64)> = x
        .targets
        .iter()
        .map(|t| (t.target.clone(), t.rev))
        .collect();
    let live = zam_store::works::live_refs(db, &refs).await?;
    let mark_stale = async |x: &Proposal| -> Result<ApiError, ApiError> {
        let d = Decision {
            to: DecisionKind::Stale,
            by: p.actor(),
            note: Some("цель изменилась после предложения".into()),
            result_ref: None,
        };
        Ok(match pr::decide(db, x.id, d).await? {
            Decided::Done(x) | Decided::NotOpen(x) => stale_conflict(&x),
        })
    };
    if refs.is_empty() || refs.iter().any(|r| !live.contains(r)) {
        return Err(mark_stale(&x).await?);
    }
    let op = &x.payload["op"];
    let rev_of = |r: &str| refs.iter().find(|(t, _)| t == r).map(|(_, rev)| *rev);
    let field = |k: &str| op[k].as_str().unwrap_or("").to_string();
    let kind = Op::parse(op["op"].as_str().unwrap_or(""))
        .ok_or_else(|| ApiError::Internal(format!("предложение o:{}: операции нет", x.id)))?;
    let text = text.map(|t| t.trim().to_string());
    if text.is_some() && !matches!(kind, Op::Merge | Op::Append) {
        return Err(ApiError::BadRequest(
            "text — правка текста у merge и append".into(),
        ));
    }
    if let Some(t) = &text {
        if t.is_empty() {
            return Err(ApiError::Unprocessable("text: пустой текст".into()));
        }
        if check_mask_literal(t).is_err() {
            return Err(ApiError::Unprocessable(MASK_LITERAL_REASON.into()));
        }
    }
    // довод исполнителя — причина отзыва, пересмотра и правки (с происхождением)
    let reason = x.reason.clone().unwrap_or_default();
    let o = OpCtx {
        st,
        client,
        p,
        headers,
        message: rules::origin_message(&x),
    };
    let target = field("target");
    let mut warnings: Vec<String> = Vec::new();
    let done: Result<String, ApiError> = async {
        let rev = |r: &str| {
            rev_of(r).ok_or_else(|| {
                ApiError::Internal(format!("предложение o:{}: {r} — не запись кандидата", x.id))
            })
        };
        Ok(match kind {
            Op::Merge => {
                let (keep, drop) = (field("keep"), field("drop"));
                let proposed = op["text"].as_str().map(str::to_string);
                let edited = text
                    .as_deref()
                    .is_some_and(|t| proposed.as_deref().map(str::trim) != Some(t));
                match text.clone().or(proposed) {
                    None => {
                        o.retract(&drop, rev(&drop)?, &format!("дубль {keep}")).await?;
                        drop
                    }
                    Some(body) => {
                        let k = zam_store::get_entry(db, ref_id(&keep)?.parse().map_err(|_| {
                            ApiError::Internal(format!("предложение o:{}: {keep}", x.id))
                        })?)
                        .await?;
                        let d = zam_store::get_entry(db, ref_id(&drop)?.parse().map_err(|_| {
                            ApiError::Internal(format!("предложение o:{}: {drop}", x.id))
                        })?)
                        .await?;
                        let triggers: Vec<Value> = zam_store::entry_triggers(db, k.id)
                            .await?
                            .into_iter()
                            .map(|t| json!({"type": t.kind, "pattern": t.pattern, "tools": t.tools}))
                            .collect();
                        let mut req = json!({
                            "title": k.title, "body": body, "data": k.data, "tags": k.tags,
                            "area": k.area, "triggers": triggers, "message": o.message,
                        });
                        if let Some(c) = merged_certainty(k.certainty, d.certainty, edited) {
                            req["certainty"] = json!(c.as_str());
                        }
                        let (_, Json(resp)) = super::insights::supersede(
                            State(st.clone()),
                            p.clone(),
                            op_headers(headers, rev(&keep)?),
                            client,
                            Path(ref_id(&keep)?),
                            op_body(req),
                        )
                        .await?;
                        let new = format!("i:{}", resp["entry"]["id"].as_str().unwrap_or(""));
                        // замещение прошло: отказ отзыва `drop` — предупреждение, а не откат
                        if let Err(e) = o.retract(&drop, rev(&drop)?, &format!("дубль {new}")).await
                        {
                            tracing::warn!(proposal = %x.id, error = %e, "уборка: drop не отозван после замещения keep");
                            warnings.push(format!("{drop} не отозван: {e}"));
                        }
                        new
                    }
                }
            }
            Op::Retract => {
                o.retract(&target, rev(&target)?, &reason).await?;
                target
            }
            Op::Revisit => {
                o.patch(&target, rev(&target)?, &reason, json!({"status": "revisit"}))
                    .await?;
                target
            }
            Op::Triggers => {
                let fields = json!({"triggers": op["triggers"]});
                o.patch(&target, rev(&target)?, &reason, fields).await?;
                target
            }
            Op::Append => {
                let body = text.clone().unwrap_or_else(|| field("text"));
                let fields = json!({"append_body": body});
                o.patch(&target, rev(&target)?, &reason, fields).await?;
                target
            }
            Op::Term if !target.is_empty() => {
                let t = zam_store::get_entry(db, ref_id(&target)?.parse().map_err(|_| {
                    ApiError::Internal(format!("предложение o:{}: {target}", x.id))
                })?)
                .await?;
                let mut data = t.data.clone();
                let mut aliases: Vec<Value> = data["aliases"].as_array().cloned().unwrap_or_default();
                for a in op["aliases"].as_array().into_iter().flatten() {
                    if !aliases.contains(a) {
                        aliases.push(a.clone());
                    }
                }
                data["aliases"] = Value::from(aliases);
                o.patch(&target, rev(&target)?, &reason, json!({"data": data}))
                    .await?;
                target
            }
            Op::Term => {
                let body = json!({
                    "kind": "term", "title": op["title"],
                    "data": {"definition": op["definition"], "aliases": op["aliases"]},
                    "message": o.message,
                });
                let mut h = headers.clone();
                h.remove("idempotency-key");
                let (_, Json(resp)) = super::insights::create(
                    st,
                    p,
                    &h,
                    &client,
                    &x.space_id.to_string(),
                    body,
                )
                .await?;
                format!("t:{}", resp["id"].as_str().unwrap_or(""))
            }
            Op::Keep => return Err(not_open(&x)),
        })
    }
    .await;
    let result = match done {
        Ok(r) => r,
        // запись изменили между сверкой и операцией
        Err(e) if is_stale_rev(&e) => return Err(mark_stale(&x).await?),
        Err(e) => return Err(e),
    };
    let d = Decision {
        to: DecisionKind::Accepted,
        by: p.actor(),
        note: None,
        result_ref: Some(result.clone()),
    };
    let x = match pr::decide(db, x.id, d).await? {
        Decided::Done(x) => x,
        Decided::NotOpen(x) => return Err(not_open(&x)),
    };
    st.system
        .audit(
            &p.actor(),
            Some(user),
            "proposal.accept",
            Some(&format!("o:{}", x.id)),
            Some(&client.ip.to_string()),
            json!({"id": format!("o:{}", x.id), "type": x.ptype.as_str(), "result": result}),
        )
        .await?;
    let mut out = json!({"proposal": proposal_json(p, &x), "result": result});
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DeclineBody {
    note: Option<String>,
}

/// Заметка решения — не длиннее.
const NOTE_MAX_CHARS: usize = 2000;

/// `POST /proposals/{id}/decline {note?}` → карточка.
pub async fn decline(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    if !p.is_human() {
        return Err(ApiError::Denied(HUMAN_ONLY_DETAIL.into()));
    }
    let b: DeclineBody = body_or_default(body)?;
    let note = b
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if note
        .as_ref()
        .is_some_and(|n| n.chars().count() > NOTE_MAX_CHARS)
    {
        return Err(ApiError::BadRequest(format!(
            "note: не длиннее {NOTE_MAX_CHARS} символов"
        )));
    }
    let (user, db) = user_db(&st, &p, &headers).await?;
    let _one = DECIDING.lock().await;
    let x = load(&db, &raw).await?;
    let d = Decision {
        to: DecisionKind::Declined,
        by: p.actor(),
        note,
        result_ref: None,
    };
    let x = match pr::decide(&db, x.id, d).await? {
        Decided::Done(x) => x,
        Decided::NotOpen(x) => return Err(not_open(&x)),
    };
    st.system
        .audit(
            &p.actor(),
            Some(user),
            "proposal.decline",
            Some(&format!("o:{}", x.id)),
            Some(&client.ip.to_string()),
            json!({"id": format!("o:{}", x.id), "type": x.ptype.as_str()}),
        )
        .await?;
    Ok(Json(proposal_json(&p, &x)))
}

/// `POST /proposals/{id}/withdraw` → карточка.
pub async fn withdraw(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if p.is_human() {
        return Err(ApiError::Denied(
            "человек не отзывает, а отклоняет кандидата: POST /proposals/{id}/decline".into(),
        ));
    }
    require_rw(&p).map_err(|_| ApiError::Denied(RW_REQUIRED_DETAIL.into()))?;
    let (_, db) = user_db(&st, &p, &headers).await?;
    let _one = DECIDING.lock().await;
    let x = load(&db, &raw).await?;
    if x.author_kind != ProposalAuthor::Agent {
        return Err(ApiError::Denied(
            "отозвать можно только своё предложение: автор этого — подключённая модель".into(),
        ));
    }
    let d = Decision {
        to: DecisionKind::Withdrawn,
        by: p.actor(),
        note: None,
        result_ref: None,
    };
    match pr::decide(&db, x.id, d).await? {
        Decided::Done(x) => Ok(Json(proposal_json(&p, &x))),
        Decided::NotOpen(x) => Err(not_open(&x)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zam_store::Certainty::{Axiom, Hypothesis, Theorem};

    /// Достоверность замещающей записи `merge` (§8.7 п. 3): меньшая из двух, без правки
    /// человеком — не выше `theorem`; запись без статуса в минимуме не участвует.
    #[test]
    fn merged_certainty_cases() {
        assert_eq!(
            merged_certainty(Some(Axiom), Some(Axiom), false),
            Some(Theorem)
        );
        assert_eq!(
            merged_certainty(Some(Axiom), Some(Axiom), true),
            Some(Axiom)
        );
        assert_eq!(
            merged_certainty(Some(Axiom), Some(Hypothesis), true),
            Some(Hypothesis)
        );
        assert_eq!(merged_certainty(Some(Theorem), None, false), Some(Theorem));
        assert_eq!(merged_certainty(None, None, false), None);
        assert_eq!(merged_certainty(None, Some(Axiom), false), Some(Theorem));
    }
}
