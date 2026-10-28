//! Вопросы к человеку (M4.5 §4): ответ на вопрос и дубликаты вопросов.
//!
//! - `POST /entries/{id}/answer {text?, option?, channel?, by?}` → 200
//!   `{entry, decision, decision_id}`: вопрос `open` → `answered` с `data.answer`, новый insight
//!   `decision` и ребро `derived_from` решение → вопрос — одной транзакцией в базе, где лежит
//!   вопрос (у общего решения участника — в базе владельца; нужна запись `rw`). Нужен `text`,
//!   `option` (ключ варианта) или оба. Повторный ответ — 409 `/problems/already-answered` с
//!   `decision_id`; устаревший вопрос — 409; неизвестный вариант и пустой ответ — 422. Аудит
//!   `entry.answer` в `system.db` (без текста ответа);
//! - `by` по умолчанию `human`: у сессии WebUI и у токена с `channel: chat | cli` (агент
//!   записывает слова человека — правило блока инструкции, как `axiom` по прямому указанию,
//!   §7.5 ТЗ); `agent` — только явно (решение — `hypothesis`). `channel` по умолчанию: `webui`
//!   у сессии WebUI, `chat` у токена; токен с `webui` — 422;
//! - вопрос задаётся общим путём `POST /spaces/{space}/entries` (`kind: question`,
//!   [`super::entries::create`]); дубликат по нормализованному заголовку среди `open` и
//!   `answered` — 409 `/problems/possible-duplicate` с `candidates` (`status`, у `answered` —
//!   `answer`), `force: true` записывает всё равно ([`question_duplicate`]);
//! - правка вопроса — `PATCH /entries/{id}`: `data` (`options`, `recommendation`) пока вопрос
//!   `open`; `status: obsolete` — с `reason` (иначе 422 `reason-required`).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_store::{
    AnswerBy, Channel, EntryKind, EntryStatus, NewAnswer, QuestionCandidate, QuestionError,
    answer_question, question_duplicates,
};

use super::entries::{audit_details, check_size, unprocessable};
use super::json_body;
use super::nodes::audit_node;
use super::scope::{Mode, entry_scope, live_entry};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

impl From<QuestionError> for ApiError {
    fn from(e: QuestionError) -> Self {
        match e {
            QuestionError::AlreadyAnswered { decision_id } => {
                ApiError::AlreadyAnswered { decision_id }
            }
            QuestionError::Obsolete => ApiError::Conflict {
                detail: e.to_string(),
                extra: Map::new(),
            },
            QuestionError::UnknownOption(_) | QuestionError::Empty | QuestionError::NotQuestion => {
                unprocessable(e.to_string())
            }
            QuestionError::Store(e) => e.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerReq {
    text: Option<String>,
    option: Option<String>,
    channel: Option<Channel>,
    by: Option<AnswerBy>,
    message: Option<String>,
}

/// `POST /entries/{id}/answer`.
pub async fn answer(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live_entry(&sc)?;
    if sc.entry.kind != EntryKind::Question {
        return Err(unprocessable(
            "ответ есть только у вопросов (kind: question)",
        ));
    }
    let req: AnswerReq = json_body(body)?;
    if let Some(t) = &req.text {
        check_size(&st, t)?;
    }
    let session = matches!(p, Principal::User { .. } | Principal::Admin { .. });
    let channel = req.channel.unwrap_or(if session {
        Channel::Webui
    } else {
        Channel::Chat
    });
    if !session && channel == Channel::Webui {
        return Err(unprocessable(
            "channel: webui — ответ из WebUI; агент отвечает с channel chat или cli",
        ));
    }
    let by = req.by.unwrap_or(AnswerBy::Human);
    let attrib = attribution(&p, &headers, req.message)?;
    let (q, d) = answer_question(
        &sc.db,
        sc.entry.id,
        NewAnswer {
            text: req.text,
            option: req.option,
            by,
            channel,
        },
        &attrib,
    )
    .await?;
    st.index.notify();
    let mut details = audit_details(sc.space.id, EntryKind::Question);
    details["by"] = json!(by.as_str());
    details["channel"] = json!(channel.as_str());
    details["decision"] = json!(d.id);
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "entry.answer",
        q.id,
        details,
    )
    .await?;
    let mut decision = json!(d);
    super::secrets::attach_entry(&mut decision, &d);
    let mut entry = json!(q);
    super::secrets::attach_entry(&mut entry, &q);
    Ok(Json(json!({
        "entry": entry,
        "decision": decision,
        "decision_id": d.id,
    })))
}

/// 409 `possible-duplicate` нового вопроса: совпал нормализованный заголовок `open` или
/// `answered` вопроса пространства. `None` — дублей нет.
pub async fn question_duplicate(
    db: &zam_store::Db,
    space: zam_core::Id,
    title: &str,
) -> Result<Option<ApiError>, ApiError> {
    let cands = question_duplicates(db, space, title).await?;
    if cands.is_empty() {
        return Ok(None);
    }
    Ok(Some(duplicate(cands)))
}

fn duplicate(candidates: Vec<QuestionCandidate>) -> ApiError {
    let answered = candidates.iter().any(|c| c.status == EntryStatus::Answered);
    let mut extra = Map::new();
    extra.insert("candidates".into(), json!(candidates));
    extra.insert("dedup".into(), json!("title"));
    ApiError::PossibleDuplicate {
        detail: if answered {
            "такой вопрос уже отвечен (ответ — в candidates): не задавайте его снова или запишите \
             всё равно (force: true)"
        } else {
            "такой вопрос уже задан и ждёт ответа: не дублируйте его или запишите всё равно \
             (force: true)"
        }
        .to_string(),
        extra,
    }
}
