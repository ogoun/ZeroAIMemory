//! Вопросы к человеку (M4.5 §4): ответ на вопрос и дубликаты вопросов.
//!
//! - `POST /entries/{id}/answer {text?, option?, channel?, by?}` → 200
//!   `{entry, decision, decision_id}`: вопрос `open` → `answered` с `data.answer`, новый insight
//!   `decision` и ребро `derived_from` решение → вопрос — одной транзакцией в базе, где лежит
//!   вопрос (у общего решения участника — в базе владельца; нужна запись `rw`). Нужен `text`,
//!   `option` (ключ варианта) или оба. Повторный ответ — 409 `/problems/already-answered` с
//!   `decision_id`; устаревший вопрос — 409; неизвестный вариант и пустой ответ — 422. Аудит
//!   `entry.answer` в `system.db` (без текста ответа);
//! - `by`: у токена — обязателен (нет — 400 `/problems/by-required`, решение контроллера по
//!   финальному ревью M4.5, I-2: умолчание `human` молча делало ответ агента самому себе
//!   аксиомой); `human` — агент записывает слова человека (решение `axiom`, как по прямому
//!   указанию, §7.5 ТЗ), `agent` — решил сам (`hypothesis`). У сессии WebUI по умолчанию
//!   `human`. `channel` по умолчанию: `webui` у сессии WebUI, `chat` у токена; токен с
//!   `webui` — 422;
//! - вопрос задаётся общим путём `POST /spaces/{space}/entries` (`kind: question`,
//!   [`super::entries::create`]); дубликат по нормализованному заголовку среди `open` и
//!   `answered` — 409 `/problems/possible-duplicate` с `candidates` (`status`, у `answered` —
//!   `answer`), `force: true` записывает всё равно ([`question_duplicate`]);
//! - правка вопроса — `PATCH /entries/{id}`: `data` (`options`, `recommendation`) пока вопрос
//!   `open`; `status: obsolete` — с `reason` (иначе 422 `reason-required`);
//! - вопрос-предложение идеи (`data.idea`, M5.7 §5.3): нужен `option` `accept|defer|decline`
//!   (иначе 422 `/problems/option-required`), `horizon` — только у `defer` (нет — сегодня +
//!   `[ideas] defer_days`); `by: agent` — 403 `/problems/idea-human-decision` до чтения идеи.
//!   Ответ переводит идею той же транзакцией и возвращает ещё `idea` и `task?`.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_store::{
    AnswerBy, Channel, EntryKind, EntryStatus, NewAnswer, QuestionCandidate, QuestionData,
    QuestionError, answer_question, question_duplicates,
};

use super::entries::{audit_details, check_size, unprocessable};
use super::json_body;
use super::nodes::audit_node;
use super::scope::{Mode, deny_report_mutation, entry_scope, live_entry};
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
            QuestionError::UnknownOption(_)
            | QuestionError::Empty
            | QuestionError::NotQuestion
            | QuestionError::HorizonNotAllowed => unprocessable(e.to_string()),
            QuestionError::OptionRequired => ApiError::OptionRequired,
            QuestionError::ByAgentOnProposal => ApiError::IdeaHumanDecision,
            QuestionError::Idea(e) => e.into(),
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
    /// Только вопрос-предложение идеи с `option: defer` (M5.7 §5.3): «отложить до».
    horizon: Option<String>,
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
    deny_report_mutation(&p, &sc)?;
    if sc.entry.kind != EntryKind::Question {
        return Err(unprocessable(
            "ответ есть только у вопросов (kind: question)",
        ));
    }
    let req: AnswerReq = json_body(body)?;
    if let Some(t) = &req.text {
        check_size(&st, t)?;
    }
    // токену умолчания `by` нет: чей ответ — знает только агент (финальное ревью M4.5, I-2)
    let (by, channel) = super::ideas::by_channel(&p, req.by, req.channel)?;
    // вопрос-предложение идеи (M5.7 §5.3): агент не отвечает за человека — проверка по вопросу,
    // до чтения идеи (ответ не раскрывает её состояние)
    let proposal = QuestionData::from_stored(&sc.entry.data).idea.is_some();
    if proposal && by == AnswerBy::Agent {
        return Err(ApiError::IdeaHumanDecision);
    }
    let mut horizon = super::ideas::parse_horizon(req.horizon.as_deref())?;
    if proposal && horizon.is_none() && req.option.as_deref().map(str::trim) == Some("defer") {
        horizon = Some(super::ideas::default_horizon(&st)?);
    }
    let attrib = attribution(&p, &headers, req.message)?;
    let zam_store::Answered {
        question: q,
        decision: d,
        idea,
        task,
    } = answer_question(
        &sc.db,
        sc.entry.id,
        NewAnswer {
            text: req.text,
            option: req.option,
            by,
            channel,
            horizon,
        },
        &attrib,
    )
    .await?;
    st.index.notify();
    let mut details = audit_details(sc.space.id, EntryKind::Question);
    details["by"] = json!(by.as_str());
    details["channel"] = json!(channel.as_str());
    details["decision"] = json!(d.id);
    if let Some(i) = &idea {
        details["idea"] = json!(i.id);
    }
    if let Some(t) = &task {
        details["task"] = json!(t.id);
    }
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
    let mut out = json!({
        "entry": entry,
        "decision": decision,
        "decision_id": d.id,
    });
    if let Some(i) = &idea {
        let mut v = json!(i);
        super::secrets::attach_entry(&mut v, i);
        out["idea"] = v;
    }
    if let Some(t) = &task {
        let mut v = json!(t);
        super::secrets::attach_entry(&mut v, t);
        out["task"] = v;
    }
    Ok(Json(out))
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
