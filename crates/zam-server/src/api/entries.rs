//! Записи журналов решения (§7.2–7.5, §12): Changelog, Releases и общие операции записей.
//!
//! - `GET/POST /spaces/{space}/entries` — лента (фильтры `kind`, `status`, `certainty`,
//!   `area`, `tag`, `data_kind` — вид changelog или insight из `data.kind`, `since`, `until`;
//!   курсор `cursor`, `limit`) и создание (`Idempotency-Key`: 201, повтор —
//!   200; ключ действует в границах автора, расхождение запроса → 422
//!   `/problems/idempotency-mismatch`, ключ удалённой записи → 409);
//! - `GET/PATCH/DELETE /entries/{id}` — `PATCH` и `DELETE` требуют `If-Match: <rev>` (§11.3,
//!   решение владельца 2026-10-26): без него 428 `/problems/precondition-required`,
//!   расхождение — 409 с текущей ревизией `rev`; ревизия проверяется раньше прав;
//!   `POST /entries/{id}/restore` (`If-Match` необязателен, при наличии проверяется),
//!   `GET /entries/{id}/revisions`; `GET /entries/{id}` отдаёт и `usage: {shown, useful,
//!   useless}` (§10.5, оценка — [`super::feedback`]), список — у каждой insight (задача 13);
//! - `GET /spaces/{space}/entries/counts?kind=<вид>` — число живых записей вида по статусам с
//!   фильтрами ленты (`area`, `tag`, `origin`, `priority`, `from_idea`): чипы статусов ленты
//!   идей WebUI (M6.5 §9).
//!
//! `kind: task` — журнал задач (M4.5 §3, [`super::tasks`]): в решении и Knowledge, `data`
//! по [`zam_store::TaskData`]; `PATCH` меняет `status` (с `next_step`, `blocked_by`, `reason`;
//! переоткрытие `done`/`dropped` → `open` — только с `reason`, иначе 422 `reason-required`) и
//! поля задачи слиянием; у `done` в ответах — `duration_min`; у задачи из идеи (системное ребро
//! `derived_from` на живую идею) — `idea {id, title, status, space_id}` и `ideas` (все её идеи), у ленты —
//! фильтр `from_idea=1` (M6.5 §9).
//! `kind: question` — вопрос к человеку (M4.5 §4, [`super::questions`]): в решении и Knowledge,
//! `data` по [`zam_store::QuestionData`] (`options`, `recommendation`); совпавший заголовок
//! `open`/`answered` вопроса — 409 `possible-duplicate` (`force` пишет всё равно); `PATCH` —
//! `data` пока `open` и `status: obsolete` с `reason`; ответ — `POST /entries/{id}/answer`.
//! `kind: idea` — идея (M5.7, [`super::ideas`]): в решении и Knowledge; `data.origin` у токена
//! обязателен, у WebUI — всегда `human`; `PATCH status` — 422, содержимое — только в `new`,
//! `deferred` и `returned` (иначе 409); токен по идее человека — 403 `idea-human-owned`, кроме
//! `append_body` (и на `DELETE`); правка человеком (сессия WebUI или токен с `by: human`, M6.5
//! §6) заголовка или тела возвращённой идеи переводит её в `new`; лента — фильтры `origin`,
//! `priority`; `GET` — ещё `task` и `question`, лента — `task`.
//! `kind: report` — отчёт (M5.5, [`super::reports`]): `multipart/form-data` или JSON с
//! `images`, только агентом и только в решении; ответ — `{id, url}`.
//! Changelog и release создаются только в пространстве решения (иначе 422); `kind: insight`
//! уходит в [`super::insights::create`] (дедупликация, `scope`, триггеры, замещение).
//! У insight `GET`/`PATCH` отдают и `triggers`; `PATCH` принимает `status: active | revisit`
//! (из `superseded`/`retracted` и в них — 422 **[решение исполнителя]**: замещение и отзыв —
//! отдельные операции, возврата нет), `triggers` (набор заменяется целиком, ошибка — 422
//! `triggers[i]: …`) и `certainty` (`null` — снять) с `reason` — по матрице прав §7.5
//! ([`crate::certainty`]): агенту смена статуса — с `reason`, `→ theorem` — ещё и с
//! доказательством (иначе 422); человеку в WebUI причина необязательна.
//! Доступ — как у узлов: участник общего решения работает в базе владельца только с
//! записями пространства решения, автор записи — он сам. В логи и аудит текст записей не
//! попадает.
//!
//! **Агенту по токену** (задача 5 M7.1, спецификация M7 §4.3.1 п. 3,
//! [`super::agent_view::view_entry_json_owned`]) запись в ответах — карточка, лента, ревизии, эхо
//! создания, правки и восстановления — с метками вместо значений: в заголовке, теле, строках
//! `data`, тегах и области — с номером `‹secret-N›` (номер маски в поле, круг правок 3 задачи 6),
//! в прочих полях — `‹secret›`; `secrets_detected` — по исходнику. Сессии WebUI — исходник. Правку
//! агента (`PATCH`) по этому виду сервер возвращает к исходнику по номерам меток (задача 6 M7.1,
//! M7 §4.3.2, [`super::secret_restore`]); метка без номера маски записывается как есть с
//! предупреждением `secret_mark_kept`.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use zam_core::{Id, TargetKind};
use zam_store::{
    AnswerBy, Certainty, Entry, EntryFilter, EntryKind, EntryPatch, EntryStatus, NewEntry,
    SpaceKind, create_entry, delete_entry, entry_by_idempotency_key, entry_revisions, list_entries,
    restore_entry, update_entry, usage_counts,
};

use super::agent_view;
use super::insights::{entry_json, parse_triggers};
use super::nodes::{audit_node, write::if_match};
use super::render::Viewer;
use super::reports::ReportInput;
use super::scope::{
    Mode, deny_report_mutation, entry_scope, live_entry, report_trash_allowed, space_scope,
    trash_allowed,
};
use super::secret_restore;
use super::{json_body, query};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Допустимые `data.kind` записи changelog.
pub const CHANGELOG_KINDS: [&str; 7] = [
    "feat", "fix", "refactor", "docs", "research", "ops", "other",
];
/// Предел длины массивов `data.refs.*`.
pub(crate) const REFS_MAX: usize = 200;
/// Предел длины одной ссылки, символов. [решение исполнителя]
pub(crate) const REF_MAX_CHARS: usize = 1000;
/// Предел числа коммитов релиза.
const COMMITS_MAX: usize = 5000;
/// Предел длины версии релиза, символов.
pub(crate) const VERSION_MAX_CHARS: usize = 64;
/// Предел длины `git_tag` и `sha`, символов. [решение исполнителя]
const TAG_MAX_CHARS: usize = 200;
const SUBJECT_MAX_CHARS: usize = 1000;
/// Лента по умолчанию.
const LIST_DEFAULT: usize = 50;

pub(crate) fn unprocessable(m: impl Into<String>) -> ApiError {
    ApiError::Unprocessable(m.into())
}

/// Массив строк до `max` элементов длиной до `item_max` символов.
pub(crate) fn string_array(
    v: &Value,
    what: &str,
    max: usize,
    item_max: usize,
) -> Result<(), ApiError> {
    let a = v
        .as_array()
        .ok_or_else(|| unprocessable(format!("{what}: ожидается массив строк")))?;
    if a.len() > max {
        return Err(unprocessable(format!("{what}: не больше {max} элементов")));
    }
    for x in a {
        match x.as_str() {
            Some(s) if s.chars().count() <= item_max => {}
            _ => {
                return Err(unprocessable(format!(
                    "{what}: строки до {item_max} символов"
                )));
            }
        }
    }
    Ok(())
}

/// `data` changelog: `kind` из [`CHANGELOG_KINDS`], `refs {commits, files, notes, issues}`.
fn check_changelog(data: &Value) -> Result<(), ApiError> {
    let kind = data.get("kind").and_then(Value::as_str);
    if !kind.is_some_and(|k| CHANGELOG_KINDS.contains(&k)) {
        return Err(unprocessable(format!(
            "data.kind: одно из {}",
            CHANGELOG_KINDS.join(", ")
        )));
    }
    if let Some(refs) = data.get("refs") {
        let o = refs
            .as_object()
            .ok_or_else(|| unprocessable("data.refs: ожидается объект"))?;
        for (k, v) in o {
            if !["commits", "files", "notes", "issues"].contains(&k.as_str()) {
                return Err(unprocessable(format!("data.refs.{k}: неизвестное поле")));
            }
            string_array(v, &format!("data.refs.{k}"), REFS_MAX, REF_MAX_CHARS)?;
        }
    }
    Ok(())
}

/// Сегодняшняя дата UTC, `YYYY-MM-DD`.
fn today_utc() -> String {
    zam_store::time::utc_today().to_string()
}

/// `data` релиза: версия, дата (по умолчанию сегодня UTC), `git_tag?`, `commits?`.
/// Возвращает нормализованную `data` (версия без пробелов по краям, дата заполнена).
fn check_release(data: &Value) -> Result<Value, ApiError> {
    let mut d = data.clone();
    let o = d.as_object_mut().expect("data — объект");
    let version = o
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if version.is_empty() || version.chars().count() > VERSION_MAX_CHARS {
        return Err(unprocessable(format!(
            "data.version: от 1 до {VERSION_MAX_CHARS} символов"
        )));
    }
    let version = version.to_string();
    o.insert("version".into(), json!(version));
    match o.get("date") {
        None | Some(Value::Null) => {
            o.insert("date".into(), json!(today_utc()));
        }
        Some(Value::String(s)) if s.len() == 10 && s.parse::<Date>().is_ok() => {}
        Some(_) => return Err(unprocessable("data.date: формат YYYY-MM-DD")),
    }
    match o.get("git_tag") {
        None | Some(Value::Null) => {
            o.remove("git_tag");
        }
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= TAG_MAX_CHARS => {}
        Some(_) => {
            return Err(unprocessable(format!(
                "data.git_tag: строка до {TAG_MAX_CHARS} символов"
            )));
        }
    }
    match o.get("commits") {
        None | Some(Value::Null) => {
            o.remove("commits");
        }
        Some(Value::Array(a)) => {
            if a.len() > COMMITS_MAX {
                return Err(unprocessable(format!(
                    "data.commits: не больше {COMMITS_MAX} элементов"
                )));
            }
            for c in a {
                let sha_ok = c
                    .get("sha")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.chars().count() <= TAG_MAX_CHARS);
                let subject_ok = match c.get("subject") {
                    None | Some(Value::Null) => true,
                    Some(Value::String(s)) => s.chars().count() <= SUBJECT_MAX_CHARS,
                    Some(_) => false,
                };
                if !sha_ok || !subject_ok {
                    return Err(unprocessable(
                        "data.commits: объекты {sha, subject?} (sha — непустая строка)",
                    ));
                }
            }
        }
        Some(_) => return Err(unprocessable("data.commits: ожидается массив")),
    }
    Ok(d)
}

/// Проверка и нормализация `data` по виду записи.
fn check_data(kind: EntryKind, data: &Value) -> Result<Value, ApiError> {
    if !data.is_object() {
        return Err(unprocessable("data должно быть JSON-объектом"));
    }
    match kind {
        EntryKind::Changelog => check_changelog(data).map(|()| data.clone()),
        EntryKind::Release => check_release(data),
        EntryKind::Insight => Err(unprocessable(
            "insight создаётся отдельным API инсайтов, а не через /entries",
        )),
        EntryKind::Term => Err(unprocessable(
            "term пишется через POST /spaces/{space}/entries с kind: term",
        )),
        // только создание (`PATCH` задачи — `task_data_patch`): статус новой задачи — `open`, его
        // меняет правка, и 422 говорит какая (M6.6 §8, решение координатора 19)
        EntryKind::Task if data.get("status").is_some() => {
            Err(unprocessable(super::tasks::STATUS_ON_CREATE))
        }
        EntryKind::Task => zam_store::check_task(data)
            .map(|t| t.to_value())
            .map_err(unprocessable),
        EntryKind::Question => zam_store::check_question(data)
            .map(|q| q.to_value())
            .map_err(unprocessable),
        EntryKind::Report => Err(unprocessable(
            "report создаётся отдельным путём создания отчёта (с файлами)",
        )),
        // `origin` и дата `horizon` новой идеи — [`super::ideas::create_data`]; здесь — патч
        // полей клиента (`priority`, `horizon`, `links`)
        EntryKind::Idea => zam_store::IdeaData::default()
            .apply_patch(data)
            .map(|()| data.clone())
            .map_err(unprocessable),
    }
}

pub(crate) fn check_size(st: &AppState, body: &str) -> Result<(), ApiError> {
    let max = st.cfg.limits.note_max_bytes;
    if body.len() > max {
        return Err(ApiError::PayloadTooLarge(format!(
            "тело записи — {} байт, предел {max}",
            body.len()
        )));
    }
    Ok(())
}

/// Заголовок: непустой, до 500 символов — иначе 422 (как прочие ошибки проверки).
pub(crate) fn check_title(t: &str) -> Result<(), ApiError> {
    let t = t.trim();
    if t.is_empty() || t.chars().count() > zam_store::TITLE_MAX_CHARS {
        return Err(unprocessable(format!(
            "title: от 1 до {} символов",
            zam_store::TITLE_MAX_CHARS
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    kind: EntryKind,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default = "empty_object")]
    data: Value,
    #[serde(default)]
    tags: Vec<String>,
    area: Option<String>,
    message: Option<String>,
    /// Только question: записать, даже если такой вопрос уже есть.
    #[serde(default)]
    force: bool,
    /// Ссылки на файлы `{files: [{path, hash?, commit?}]}` (M4.5 §6.1, [`super::files`]).
    refs: Option<Value>,
}

pub(crate) fn empty_object() -> Value {
    json!({})
}

/// `Idempotency-Key` из заголовка: UTF-8; пустой равен отсутствию.
pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(v) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let s = std::str::from_utf8(v.as_bytes())
        .map_err(|_| ApiError::BadRequest("Idempotency-Key: UTF-8".into()))?
        .trim();
    Ok((!s.is_empty()).then(|| s.to_string()))
}

/// `POST /spaces/{space}/entries` → 201 `Entry`; повтор с тем же `Idempotency-Key` — 200.
/// `kind: insight` и `kind: term` — [`super::insights::create`]; отчёт (`multipart/form-data`
/// или JSON с `kind: report`) — [`super::reports::create`]. JSON читается с пределом
/// [`super::entries_json_max`]; запись не-отчёта больше [`super::note_body_max`] — 413 до
/// построения `Value`; тело больше `note_body_max` ждёт разрешения [`super::big_body`] (503).
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(space): Path<String>,
    req: Request,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if super::reports::is_multipart(&headers) {
        let mp = Multipart::from_request(req, &st)
            .await
            .map_err(|e| ApiError::BadRequest(format!("ожидается multipart: {e}")))?;
        return super::reports::create(
            &st,
            &p,
            &headers,
            &client,
            &space,
            ReportInput::Multipart(mp),
        )
        .await;
    }
    let json_max = super::entries_json_max(&st.cfg);
    let declared = super::big_body::content_length(&headers);
    // тело больше `note_body_max` держит разрешение большого тела до конца обработки
    let held = super::big_body::permit_held(req.extensions());
    // не-отчёт с `kind` в начале тела — 413 до ожидания разрешения
    let (bytes, _big) = st
        .big_bodies
        .read(req.into_body(), json_max, declared, held, |prefix| {
            super::big_body::kind_prefix(prefix) == Some(false)
        })
        .await?;
    // вид — без построения `Value`: отчёт разбирается из байт сразу в типы
    if super::reports::json_is_report(&bytes) {
        return super::reports::create(
            &st,
            &p,
            &headers,
            &client,
            &space,
            ReportInput::Json(bytes),
        )
        .await;
    }
    if bytes.len() > super::note_body_max(&st.cfg.limits) {
        return Err(ApiError::PayloadTooLarge(
            "тело запроса превышает предел".into(),
        ));
    }
    let raw: Value = super::parse_body(&bytes)?;
    if matches!(
        raw.get("kind")
            .and_then(Value::as_str)
            .and_then(EntryKind::parse),
        Some(EntryKind::Insight | EntryKind::Term)
    ) {
        return super::insights::create(&st, &p, &headers, &client, &space, raw).await;
    }
    let req: Create = serde_json::from_value(raw)
        .map_err(|e| ApiError::BadRequest(format!("некорректное тело: {e}")))?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Write).await?;
    // вид проверяется после доступа к пространству: без прав 403/404, а не 422
    if matches!(
        req.kind,
        EntryKind::Task | EntryKind::Question | EntryKind::Idea
    ) {
        // задачи, вопросы и идеи ведутся в решении и Knowledge (Rules и Human журналов не имеют)
        if !matches!(sc.space.kind, SpaceKind::Solution | SpaceKind::Knowledge) {
            return Err(unprocessable(
                "задачи, вопросы и идеи ведутся только в пространстве решения или Knowledge",
            ));
        }
    } else if sc.space.kind != SpaceKind::Solution {
        return Err(unprocessable(
            "changelog и release ведутся только в пространстве решения",
        ));
    }
    check_title(&req.title)?;
    let refs = super::files::refs_field(req.refs.as_ref())?;
    let key = idempotency_key(&headers)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let mut data = if req.kind == EntryKind::Idea {
        // повтор по ключу сверяет хранилище без часов: дата `horizon` здесь не проверяется
        let repeat = match &key {
            Some(k) => entry_by_idempotency_key(&sc.db, &attrib, k)
                .await?
                .is_some(),
            None => false,
        };
        super::ideas::create_data(&p, &req.data, !repeat)?
    } else {
        check_data(req.kind, &req.data)?
    };
    check_size(&st, &req.body)?;
    if req.kind == EntryKind::Question && !req.force {
        // повтор по ключу дубликатом не считается (иначе запись нашла бы саму себя)
        let repeat = match &key {
            Some(k) => entry_by_idempotency_key(&sc.db, &attrib, k)
                .await?
                .is_some(),
            None => false,
        };
        if !repeat
            && let Some(dup) =
                super::questions::question_duplicate(&p, &sc.db, sc.space.id, &req.title).await?
        {
            return Err(dup);
        }
    }
    // повтор без даты после смены суток: отпечаток снимается с исходного запроса, поэтому
    // подставленная при первом создании дата берётся у уже созданной записи
    if req.kind == EntryKind::Release
        && req.data.get("date").is_none_or(Value::is_null)
        && let Some(k) = &key
        && let Some(ex) = entry_by_idempotency_key(&sc.db, &attrib, k).await?
        && let Some(first) = entry_revisions(&sc.db, ex.id).await?.into_iter().next()
        && let Some(d) = first.data.get("date")
    {
        data["date"] = d.clone();
    }
    let new = NewEntry {
        space_id: sc.space.id,
        kind: req.kind,
        title: req.title,
        body: req.body,
        data,
        area: req.area,
        tags: req.tags,
        status: None,
        certainty: None,
        idempotency_key: key.clone(),
        triggers: Vec::new(),
        links: Vec::new(),
        refs,
    };
    let (mut entry, created) = create_entry(&sc.db, new, &attrib)
        .await
        .map_err(|e| agent_view::view_error(&p, e.into()))?;
    if created {
        st.index.notify();
    }
    // создание по токену: базы нет — метка `‹secret›` значения не хранит (решение
    // координатора по задаче 6 M7.1); повтор по ключу — без предупреждения
    let kept = if created && agent_view::masks(&p) {
        secret_restore::EntryTexts::of(&mut entry).marks()
    } else {
        0
    };
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let mut v = json!(entry);
    super::secrets::attach_entry(&mut v, &entry);
    super::tasks::attach_task(&mut v, &entry, &[]);
    super::glossary::attach_warnings(
        &mut v,
        super::glossary::alias_warnings(&st, &sc.db, sc.space.id, &entry.title, &entry.body)
            .await?,
    );
    let mut v = agent_view::view_entry_json_owned(&p, v).await?;
    agent_view::view_json(&p, &mut v, &["warnings"]);
    secret_restore::attach_kept(&mut v, kept);
    Ok((status, Json(v)))
}

#[derive(Deserialize)]
pub struct ListQuery {
    kind: Option<String>,
    /// Через запятую.
    status: Option<String>,
    /// Через запятую.
    certainty: Option<String>,
    area: Option<String>,
    tag: Option<String>,
    /// `data.kind`: вид changelog ([`CHANGELOG_KINDS`]) или insight
    /// ([`super::insights::INSIGHT_KINDS`]).
    data_kind: Option<String>,
    /// `human` | `agent`: решения по `data.decision.decided_by` (M4.5, спецификация §5.1).
    decided_by: Option<String>,
    since: Option<String>,
    until: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    /// `only` — корзина (только `kind=report`, только владельцу решения в WebUI).
    deleted: Option<String>,
    /// Только `kind=idea`: `human` | `agent` (M5.7 §7.1).
    origin: Option<String>,
    /// Только `kind=idea`: `low` | `normal` | `high`.
    priority: Option<String>,
    /// Только `kind=task`: `1` — задачи из идей (пункты плана, M6.5 §9).
    from_idea: Option<String>,
}

/// Флаг запроса: нет, пусто, `0`, `false` — выключен, `1`, `true` — включён, иное — 400.
fn parse_flag(raw: Option<&str>, what: &str) -> Result<bool, ApiError> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None | Some("0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(v) => Err(ApiError::BadRequest(format!(
            "{what}: неизвестное значение «{v}»; допустимы 1, 0"
        ))),
    }
}

/// `deleted` ленты: нет или пусто — живые записи, `only` — корзина; иное — 400.
fn parse_deleted(raw: Option<&str>) -> Result<bool, ApiError> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(false),
        Some("only") => Ok(true),
        Some(v) => Err(ApiError::BadRequest(format!(
            "deleted: неизвестное значение «{v}»; допустимо only"
        ))),
    }
}

/// `data_kind` ленты: пусто — без фильтра, иначе один из видов changelog или insight (400).
fn parse_data_kind(raw: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(k) = raw.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) else {
        return Ok(None);
    };
    if CHANGELOG_KINDS.contains(&k.as_str()) || super::insights::INSIGHT_KINDS.contains(&k.as_str())
    {
        Ok(Some(k))
    } else {
        Err(ApiError::BadRequest(format!(
            "data_kind: неизвестное значение «{k}»"
        )))
    }
}

/// `decided_by`: пусто — без фильтра, иначе `human` или `agent` (400); `what` — имя поля в тексте.
pub(crate) fn parse_decided_by(raw: Option<&str>, what: &str) -> Result<Option<String>, ApiError> {
    match raw.map(str::trim).filter(|k| !k.is_empty()) {
        None => Ok(None),
        Some(k @ ("human" | "agent")) => Ok(Some(k.to_string())),
        Some(k) => Err(ApiError::BadRequest(format!(
            "{what}: неизвестное значение «{k}»; допустимы human, agent"
        ))),
    }
}

/// Список значений через запятую → перечисление; неизвестное → 400.
fn parse_list<T>(
    raw: Option<&str>,
    what: &str,
    f: fn(&str) -> Option<T>,
) -> Result<Vec<T>, ApiError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            f(s).ok_or_else(|| ApiError::BadRequest(format!("{what}: неизвестное значение «{s}»")))
        })
        .collect()
}

/// Одно значение фильтра ленты: пусто — без фильтра, неизвестное — 400.
fn parse_one<T>(
    raw: Option<&str>,
    what: &str,
    f: fn(&str) -> Option<T>,
) -> Result<Option<T>, ApiError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => f(s)
            .map(Some)
            .ok_or_else(|| ApiError::BadRequest(format!("{what}: неизвестное значение «{s}»"))),
    }
}

/// Момент времени: RFC 3339 или дата `YYYY-MM-DD` (начало суток UTC).
fn parse_time(raw: &str, what: &str) -> Result<Timestamp, ApiError> {
    if let Ok(t) = raw.parse::<Timestamp>() {
        return Ok(t);
    }
    raw.parse::<Date>()
        .ok()
        .and_then(|d| d.to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map(|z| z.timestamp())
        .ok_or_else(|| ApiError::BadRequest(format!("{what}: RFC 3339 или YYYY-MM-DD")))
}

/// `GET /spaces/{space}/entries` → `{items, next_cursor}`. Без `kind` отчётов в ленте нет;
/// `kind=report` — только сессиям WebUI (токену — 400, как неизвестный вид: отчёт агенту не
/// виден, §7), у элементов — `first_image` и нет `body`; `deleted=only` — корзина отчётов
/// ([`trash_allowed`], иначе 403).
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let kind = match q.kind.as_deref() {
        Some(k) => Some(
            EntryKind::parse(k)
                .filter(|k| p.is_human() || k.agent_visible())
                .ok_or_else(|| ApiError::BadRequest(format!("kind: «{k}»")))?,
        ),
        None => None,
    };
    let deleted_only = parse_deleted(q.deleted.as_deref())?;
    if deleted_only {
        if kind != Some(EntryKind::Report) {
            return Err(ApiError::BadRequest(
                "deleted=only — только у ленты отчётов (kind=report)".into(),
            ));
        }
        trash_allowed(&st, &p, sc.owner, sc.role)?;
    }
    // фильтры по `data` идеи — только у ленты идей (M5.7 §7.1)
    let idea_filter = [&q.origin, &q.priority]
        .into_iter()
        .any(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()));
    if idea_filter && kind != Some(EntryKind::Idea) {
        return Err(unprocessable(
            "origin и priority — фильтры ленты идей (kind=idea)",
        ));
    }
    let from_idea = parse_flag(q.from_idea.as_deref(), "from_idea")?;
    if from_idea && kind != Some(EntryKind::Task) {
        return Err(unprocessable("from_idea — фильтр ленты задач (kind=task)"));
    }
    let f = EntryFilter {
        space_id: sc.space.id,
        kind,
        statuses: parse_list(q.status.as_deref(), "status", EntryStatus::parse)?,
        certainty: parse_list(q.certainty.as_deref(), "certainty", Certainty::parse)?,
        area: q.area.filter(|a| !a.is_empty()),
        tag: q.tag.filter(|a| !a.is_empty()),
        data_kind: parse_data_kind(q.data_kind)?,
        decided_by: parse_decided_by(q.decided_by.as_deref(), "decided_by")?,
        since: q
            .since
            .as_deref()
            .map(|t| parse_time(t, "since"))
            .transpose()?,
        until: q
            .until
            .as_deref()
            .map(|t| parse_time(t, "until"))
            .transpose()?,
        cursor: q.cursor.filter(|c| !c.is_empty()),
        limit: q.limit.unwrap_or(LIST_DEFAULT),
        // без вида — журналы без отчётов (у отчётов свой журнал, `kind=report`)
        with_reports: false,
        deleted_only,
        origin: parse_one(q.origin.as_deref(), "origin", zam_store::IdeaOrigin::parse)?,
        priority: parse_one(
            q.priority.as_deref(),
            "priority",
            zam_store::IdeaPriority::parse,
        )?,
        from_idea: from_idea.then(|| super::tasks::idea_spaces(sc.role, sc.space.id)),
    };
    let (items, next) = list_entries(&sc.db, f).await?;
    // у insight — показы и оценки (столбец списка Insights WebUI, задача 13), одним запросом
    let ids: Vec<Id> = items
        .iter()
        .filter(|e| e.kind == EntryKind::Insight)
        .map(|e| e.id)
        .collect();
    let usage = zam_store::usage_counts_many(&sc.db, TargetKind::Entry, &ids).await?;
    // у задачи — идеи плана, у идеи — её задача (M6.5 §9): по запросу на страницу, без N+1
    let task_ids: Vec<Id> = items
        .iter()
        .filter(|e| e.kind == EntryKind::Task)
        .map(|e| e.id)
        .collect();
    let task_ideas = zam_store::ideas_of_tasks(
        &sc.db,
        &task_ids,
        &super::tasks::idea_spaces(sc.role, sc.space.id),
    )
    .await?;
    let idea_tasks = super::ideas::idea_tasks_many(&sc.db, &items).await?;
    let mut out = Vec::with_capacity(items.len());
    for e in &items {
        let mut v = serde_json::to_value(e)
            .map_err(|e| ApiError::Internal(format!("запись не сериализуется: {e}")))?;
        if e.kind == EntryKind::Insight {
            v["usage"] = json!(usage.get(&e.id).copied().unwrap_or_default());
        }
        super::tasks::attach_task(&mut v, e, task_ideas.get(&e.id).map_or(&[], Vec::as_slice));
        super::ideas::attach_idea_task(&mut v, e, &idea_tasks);
        out.push(v);
    }
    if kind == Some(EntryKind::Report) {
        let viewer = Viewer::of(&st, &p, sc.owner, sc.role);
        super::reports::view::report_feed(&sc.db, &items, &mut out, viewer).await?;
    }
    // лента до 200 записей с телами — маска одним проходом, длинная — в пуле
    let out = agent_view::view_entry_json_owned(&p, Value::Array(out)).await?;
    Ok(Json(json!({ "items": out, "next_cursor": next })))
}

#[derive(Deserialize)]
pub struct CountsQuery {
    kind: Option<String>,
    area: Option<String>,
    tag: Option<String>,
    /// Только `kind=idea`: `human` | `agent`.
    origin: Option<String>,
    /// Только `kind=idea`: `low` | `normal` | `high`.
    priority: Option<String>,
    /// Только `kind=task`: `1` — задачи из идей.
    from_idea: Option<String>,
}

/// `GET /spaces/{space}/entries/counts?kind=<вид>` → `{<статус>: n}` — все статусы вида
/// ([`zam_store::allowed_statuses`], нулевые тоже) с фильтрами ленты [`list`]: `area`,
/// `tag`, у идей `origin` и `priority`, у задач `from_idea` (M6.5 §9, чипы статусов ленты идей
/// WebUI). Без `kind` или у вида без статусов — 400; фильтр не своего вида — 422; вид, невидимый
/// агенту, токену — 400, как у ленты.
pub async fn counts(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<CountsQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let raw = q
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| ApiError::BadRequest("kind: обязателен".into()))?;
    let kind = EntryKind::parse(raw)
        .filter(|k| p.is_human() || k.agent_visible())
        .ok_or_else(|| ApiError::BadRequest(format!("kind: «{raw}»")))?;
    if zam_store::allowed_statuses(kind).is_empty() {
        return Err(ApiError::BadRequest(format!(
            "kind: у вида «{raw}» нет статусов"
        )));
    }
    let idea_filter = [&q.origin, &q.priority]
        .into_iter()
        .any(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()));
    if idea_filter && kind != EntryKind::Idea {
        return Err(unprocessable(
            "origin и priority — фильтры ленты идей (kind=idea)",
        ));
    }
    let from_idea = parse_flag(q.from_idea.as_deref(), "from_idea")?;
    if from_idea && kind != EntryKind::Task {
        return Err(unprocessable("from_idea — фильтр ленты задач (kind=task)"));
    }
    let f = EntryFilter {
        space_id: sc.space.id,
        kind: Some(kind),
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: q.area.filter(|a| !a.is_empty()),
        tag: q.tag.filter(|a| !a.is_empty()),
        data_kind: None,
        decided_by: None,
        since: None,
        until: None,
        cursor: None,
        limit: 1,
        with_reports: false,
        deleted_only: false,
        origin: parse_one(q.origin.as_deref(), "origin", zam_store::IdeaOrigin::parse)?,
        priority: parse_one(
            q.priority.as_deref(),
            "priority",
            zam_store::IdeaPriority::parse,
        )?,
        from_idea: from_idea.then(|| super::tasks::idea_spaces(sc.role, sc.space.id)),
    };
    let counts = zam_store::count_by_status(&sc.db, f).await?;
    let mut out = serde_json::Map::new();
    for (s, n) in counts {
        out.insert(s.as_str().into(), json!(n));
    }
    Ok(Json(Value::Object(out)))
}

#[derive(Deserialize)]
pub struct GetQuery {
    /// `json` (по умолчанию) | `html` — страница отчёта ([`super::reports::view::get_html`]).
    format: Option<String>,
}

/// `GET /entries/{id}[?format=json|html]` → `Entry` (у insight — с `triggers`); `format=html` —
/// только у отчёта и только человеку: `{entry, html, toc, warnings, files}` (у прочих видов —
/// 400, токену отчёт — 404).
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    q: Result<Query<GetQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let html = match q.format.as_deref().map(str::trim) {
        None | Some("" | "json") => false,
        Some("html") => true,
        Some(f) => {
            return Err(ApiError::BadRequest(format!(
                "format: «{f}»; допустимы json, html"
            )));
        }
    };
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live_entry(&sc)?;
    if html {
        // страница отчёта — только человеку: токену отчёт «не существует» (§7)
        if sc.entry.kind == EntryKind::Report && !p.is_human() {
            return Err(ApiError::NotFound);
        }
        let viewer = Viewer::of(&st, &p, sc.owner, sc.role);
        return super::reports::view::get_html(&st, &p, &sc, viewer).await;
    }
    let mut v = entry_json(&p, &sc.db, &sc.entry, sc.role).await?;
    let links = super::ideas::idea_links(&sc.db, &sc.entry).await?;
    super::ideas::attach_idea(&mut v, &sc.entry, &links);
    // заголовок связанной задачи идеи (запись уже маскирована `entry_json`)
    agent_view::view_json(&p, &mut v, &["task.title"]);
    super::ideas::attach_reviewers(&st, &mut v, &sc.entry).await?;
    // показы и оценки — из базы, где лежит запись (§10.5)
    let target = zam_core::TargetRef {
        kind: zam_core::TargetKind::Entry,
        id: sc.entry.id,
    };
    v["usage"] = json!(usage_counts(&sc.db, target).await?);
    Ok(Json(v))
}

/// `Some(None)` для `null`, `Some(Some(v))` для значения, `None` для отсутствующего поля.
fn double_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    title: Option<String>,
    body: Option<String>,
    append_body: Option<String>,
    data: Option<Value>,
    tags: Option<Vec<String>>,
    #[serde(default, deserialize_with = "double_option")]
    area: Option<Option<String>>,
    /// Только insight: `active` | `revisit`.
    status: Option<EntryStatus>,
    /// Только insight: новый набор триггеров целиком.
    triggers: Option<Vec<Value>>,
    /// Только insight: `axiom | theorem | hypothesis`, `null` — снять статус; смена — по
    /// матрице прав (§7.5) с `reason`, `theorem` от агента — с `data.evidence`.
    #[serde(default, deserialize_with = "double_option")]
    certainty: Option<Option<Certainty>>,
    reason: Option<String>,
    /// Только task: следующий шаг (`null` — снять), то же, что `data.next_step`.
    #[serde(default, deserialize_with = "double_option")]
    next_step: Option<Option<String>>,
    /// Только task: блокер (`null` — снять), то же, что `data.blocked_by`.
    #[serde(default, deserialize_with = "double_option")]
    blocked_by: Option<Option<String>>,
    message: Option<String>,
    /// Новый набор ссылок на файлы `{files: [...]}` целиком (M4.5 §6.1); ревизии не создаёт.
    refs: Option<Value>,
    /// Только токен и только с правкой идеи (`title`, `body`, `append_body`; M6.5 §6): чья это
    /// правка — `human` (агент записывает слова человека из чата: возвращённая идея снова идёт
    /// на разбор) или `agent` (по умолчанию). Сессия WebUI — всегда человек.
    by: Option<AnswerBy>,
}

/// `by` правки (M6.5 §6): только у токена и только вместе с правкой заголовка или тела идеи,
/// иначе 422.
fn check_patch_by(p: &Principal, e: &Entry, req: &Patch) -> Result<(), ApiError> {
    if req.by.is_none() {
        return Ok(());
    }
    if p.is_human() {
        return Err(unprocessable(
            "by: только у токена агента — правка из WebUI и так правка человека",
        ));
    }
    let edit = req.title.is_some() || req.body.is_some() || req.append_body.is_some();
    if e.kind != EntryKind::Idea || !edit {
        return Err(unprocessable(
            "by: только у правки идеи — вместе с title, body или append_body",
        ));
    }
    Ok(())
}

/// Смена статуса task: переходы §3 M4.5; переоткрытие без `reason` — 422 `reason-required`.
fn check_task_status(e: &Entry, s: EntryStatus, reason: Option<&str>) -> Result<(), ApiError> {
    let from = e.status.unwrap_or(EntryStatus::Open);
    zam_store::check_task_transition(from, s, reason).map_err(|code| match code {
        "reason-required" => ApiError::ReasonRequired(
            "переоткрытие задачи (done/dropped → open) требует reason".into(),
        ),
        "not-a-task-status" => {
            unprocessable("status задачи: open, in_progress, blocked, done или dropped")
        }
        _ => unprocessable(format!(
            "переход {} → {} не разрешён: из done/dropped — только в open с reason",
            from.as_str(),
            s.as_str()
        )),
    })
}

/// Смена статуса question правкой: только `obsolete` с `reason`; `answered` — ответом.
fn check_question_status(e: &Entry, s: EntryStatus, reason: Option<&str>) -> Result<(), ApiError> {
    let from = e.status.unwrap_or(EntryStatus::Open);
    zam_store::check_question_transition(from, s, reason).map_err(|code| match code {
        "reason-required" => {
            ApiError::ReasonRequired("устаревание вопроса (obsolete) требует reason".into())
        }
        "answer-only" => unprocessable("answered ставится только ответом: POST …/answer"),
        "not-a-question-status" => unprocessable("status вопроса: open, answered или obsolete"),
        _ => unprocessable(format!(
            "переход {} → {} не разрешён: из obsolete статус не меняется, answered в open не \
             возвращается",
            from.as_str(),
            s.as_str()
        )),
    })
}

/// Смена статуса insight правкой: только `active ↔ revisit` — иначе 422.
fn check_status_change(e: &Entry, s: EntryStatus, reason: Option<&str>) -> Result<(), ApiError> {
    if e.kind == EntryKind::Task {
        return check_task_status(e, s, reason);
    }
    if e.kind == EntryKind::Question {
        return check_question_status(e, s, reason);
    }
    if e.kind == EntryKind::Idea {
        return Err(super::ideas::status_patch_error());
    }
    if e.kind != EntryKind::Insight {
        return Err(unprocessable(
            "status есть только у insight, task и question",
        ));
    }
    if !matches!(s, EntryStatus::Active | EntryStatus::Revisit) {
        return Err(unprocessable(
            "status: правкой — только active или revisit; superseded — POST …/supersede, \
             retracted — POST …/retract",
        ));
    }
    if let Some(old @ (EntryStatus::Superseded | EntryStatus::Retracted)) = e.status
        && old != s
    {
        return Err(unprocessable(format!(
            "запись {}: статус больше не меняется (возврата из замещения и отзыва нет)",
            old.as_str()
        )));
    }
    Ok(())
}

/// Патч `data` задачи из `data`, `next_step` и `blocked_by` тела (верхнего уровня — сильнее);
/// проверяется на копии прежней `data`, в хранилище уходит как слияние. У прочих видов
/// `next_step` и `blocked_by` — 422.
fn task_data_patch(e: &Entry, req: &Patch) -> Result<Option<Value>, ApiError> {
    if e.kind != EntryKind::Task {
        if req.next_step.is_some() || req.blocked_by.is_some() {
            return Err(unprocessable("next_step и blocked_by есть только у task"));
        }
        return Ok(None);
    }
    let mut patch = match &req.data {
        Some(Value::Object(o)) => o.clone(),
        Some(_) => return Err(unprocessable("data должно быть JSON-объектом")),
        None => serde_json::Map::new(),
    };
    for (k, v) in [
        ("next_step", &req.next_step),
        ("blocked_by", &req.blocked_by),
    ] {
        if let Some(v) = v {
            patch.insert(k.into(), json!(v));
        }
    }
    if patch.is_empty() {
        return Ok(None);
    }
    let patch = Value::Object(patch);
    zam_store::TaskData::from_stored(&e.data)
        .apply_patch(&patch)
        .map_err(unprocessable)?;
    Ok(Some(patch))
}

/// Необязательный `If-Match: <rev>` (supersede, retract, restore — §11.3): есть — проверяется.
pub(crate) fn optional_if_match(headers: &HeaderMap) -> Result<Option<i64>, ApiError> {
    headers
        .contains_key(header::IF_MATCH)
        .then(|| if_match(headers))
        .transpose()
}

/// `PATCH /entries/{id}` с `If-Match: <rev>` → 200 `Entry`; без заголовка — 428; устаревшая
/// ревизия — 409 с `rev` текущей; 413 / 422.
pub async fn patch(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live_entry(&sc)?;
    // отчёт неизменен: любое поле — 409 раньше `If-Match` и разбора тела
    deny_report_mutation(&p, &sc)?;
    let rev = Some(if_match(&headers)?);
    let mut req: Patch = json_body(body)?;
    // агент правил вид ревизии `If-Match`: она устарела — 409 хранилища раньше восстановления
    // по текущей (круг правок 2 задачи 6, B4; как `PUT` заметки, M6 круга 1) — иначе правка по
    // старому виду дала бы 422 вместо 409
    if agent_view::masks(&p) {
        stale_entry(&sc.entry, rev)?;
    }
    // агент правил нумерованный вид записи: значения под `‹secret-N›` — обратно до проверок
    // (M7 §4.3.2), длинное тело с метками — в пуле; сессия WebUI — как есть
    let big_body = req.body.as_ref().is_some_and(|b| {
        b.len() + sc.entry.body.len() > agent_view::BLOCKING_FROM && secret_restore::has_mark(b)
    });
    let kept = if !agent_view::masks(&p) {
        0
    } else if !big_body {
        restore_patch(&sc.entry, &mut req)?
    } else {
        let base = sc.entry.clone();
        let (r, k) = agent_view::blocking(move || {
            let mut r = req;
            let k = restore_patch(&base, &mut r);
            (r, k)
        })
        .await?;
        req = r;
        k?
    };
    check_patch_by(&p, &sc.entry, &req)?;
    // правку сделал человек: сессия WebUI или слова человека токеном (`by: human`, M6.5 §6) —
    // возвращённая идея снова идёт на разбор
    let human_edit = p.is_human() || req.by == Some(AnswerBy::Human);
    // токен по идее человека — только дополнение `append_body` (M5.7 §7.1), до записи
    let content = req.title.is_some()
        || req.body.is_some()
        || req.data.is_some()
        || req.tags.is_some()
        || req.area.is_some()
        || req.refs.is_some();
    super::ideas::deny_token_on_human_idea(&p, &sc.entry, content)?;
    for t in [&req.body, &req.append_body].into_iter().flatten() {
        check_size(&st, t)?;
    }
    if let Some(t) = &req.title {
        check_title(t)?;
    }
    if let Some(s) = req.status {
        check_status_change(&sc.entry, s, req.reason.as_deref())?;
    }
    if req.certainty.is_some() && !zam_store::has_certainty(sc.entry.kind) {
        return Err(unprocessable("certainty есть только у insight и term"));
    }
    let task_patch = task_data_patch(&sc.entry, &req)?;
    let refs = super::files::refs_field(req.refs.as_ref())?;
    let triggers = match req.triggers {
        Some(_) if sc.entry.kind != EntryKind::Insight => {
            return Err(unprocessable("triggers есть только у insight"));
        }
        Some(raw) => Some(parse_triggers(raw)?),
        None => None,
    };
    // release: поля, не указанные в PATCH, сохраняются (слияние на верхнем уровне);
    // changelog: data заменяется целиком
    let data = match (&req.data, sc.entry.kind) {
        (_, EntryKind::Task) => task_patch,
        // term: имя и data проверяются вместе (422 `title: …` / `data.<поле>: …`)
        (_, EntryKind::Term) if req.title.is_some() || req.data.is_some() => {
            let title = req.title.as_deref().unwrap_or(&sc.entry.title);
            let data = req.data.as_ref().unwrap_or(&sc.entry.data);
            let (_, td) = zam_store::check_term(title, data).map_err(unprocessable)?;
            req.data.as_ref().map(|_| td.to_value())
        }
        (Some(Value::Object(new)), EntryKind::Release) => {
            let mut merged = sc.entry.data.clone();
            if let Some(m) = merged.as_object_mut() {
                for (k, v) in new {
                    m.insert(k.clone(), v.clone());
                }
            }
            Some(check_data(sc.entry.kind, &merged)?)
        }
        (Some(d), EntryKind::Insight) => {
            // ADR-ключи верхнего уровня, уже лежащие в записи, правке не мешают (ревью 13a, m1)
            super::insights::check_insight_data_patch(d, &sc.entry.data)?;
            Some(d.clone())
        }
        (Some(d), EntryKind::Idea) => {
            // слияние полей клиента; `origin` и поля сервера — 422 (хранилище сохраняет их)
            zam_store::IdeaData::from_stored(&sc.entry.data)
                .apply_patch(d)
                .map_err(unprocessable)?;
            Some(d.clone())
        }
        (Some(d), EntryKind::Question) => {
            zam_store::QuestionData::from_stored(&sc.entry.data)
                .apply_patch(d)
                .map_err(unprocessable)?;
            Some(d.clone())
        }
        (Some(d), kind) => Some(check_data(kind, d)?),
        (None, _) => None,
    };
    // псевдонимы термина приходят только с `data`: правка без неё о них не предупреждает
    let term_data = sc.entry.kind == EntryKind::Term && data.is_some();
    let attrib = attribution(&p, &headers, req.message)?;
    let patch = EntryPatch {
        title: req.title,
        body: req.body,
        append_body: req.append_body,
        data,
        tags: req.tags,
        area: req.area,
        status: req.status,
        certainty: req.certainty,
        reason: req.reason,
        triggers,
        refs,
        human_edit,
    };
    let before = sc.entry.rev;
    let e = update_entry(&sc.db, sc.entry.id, rev, patch, &attrib)
        .await
        .map_err(|e| agent_view::view_error(&p, e.into()))?;
    if e.rev != before {
        st.index.notify();
    }
    let mut v = entry_json(&p, &sc.db, &e, sc.role).await?;
    // находки — у агента только новые: восстановленные и прежние значения ему не новость (M4)
    if agent_view::masks(&p) {
        super::secrets::attach_entry_new(&mut v, &e, &sc.entry);
    } else {
        super::secrets::attach_entry(&mut v, &e);
    }
    if e.kind != EntryKind::Term {
        super::glossary::attach_warnings(
            &mut v,
            super::glossary::alias_warnings(&st, &sc.db, sc.space.id, &e.title, &e.body).await?,
        );
    } else if term_data {
        super::glossary::attach_warnings(&mut v, super::glossary::broad_alias_warnings(&e.data));
    }
    agent_view::view_json(&p, &mut v, &["warnings"]);
    secret_restore::attach_kept(&mut v, kept);
    Ok(Json(v))
}

/// `If-Match` `rev` не совпал с ревизией записи `e` — 409 с текущей `rev`, тот же, что даёт
/// хранилище ([`zam_store::StoreError::StaleRevision`]): сверка раньше восстановления значений
/// под `‹secret›` по виду записи (правка, замещение).
pub(crate) fn stale_entry(e: &Entry, rev: Option<i64>) -> Result<(), ApiError> {
    match rev {
        Some(expected) if expected != e.rev => Err(zam_store::StoreError::StaleRevision {
            expected,
            current: e.rev,
        }
        .into()),
        _ => Ok(()),
    }
}

/// Правка `req` агентом по нумерованному виду записи `e` (M7 §4.3.2, задача 6 M7.1): поля
/// [`secret_restore::EntryTexts`] — значения по номерам меток `‹secret-N›` масок того же поля
/// записи; метки дописки `append_body` — новые. Меток без значения; правка, выводящая секрет
/// из-под маски, — 422.
fn restore_patch(e: &Entry, req: &mut Patch) -> Result<usize, ApiError> {
    let mut data_fields = Vec::new();
    if let Some(Some(s)) = &mut req.next_step {
        data_fields.push(("next_step", s));
    }
    if let Some(Some(s)) = &mut req.blocked_by {
        data_fields.push(("blocked_by", s));
    }
    secret_restore::EntryTexts {
        title: req.title.as_mut(),
        body: req.body.as_mut(),
        append_body: req.append_body.as_deref(),
        data: req.data.as_mut(),
        data_fields,
        tags: req.tags.as_mut(),
        area: req.area.as_mut().and_then(Option::as_mut),
    }
    .restore(e.into())
}

/// Детали аудита операций записи (без текста записи).
pub(crate) fn audit_details(space: zam_core::Id, kind: EntryKind) -> Value {
    json!({ "space": space, "kind": kind.as_str() })
}

/// `DELETE /entries/{id}` с `If-Match: <rev>` — мягкое удаление → 204; без заголовка — 428,
/// устаревшая ревизия — 409; аудит `entry.delete`. Отчёт — только владелец решения в WebUI
/// ([`report_trash_allowed`], иначе 403).
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    report_trash_allowed(&st, &p, &sc)?;
    live_entry(&sc)?;
    let rev = if_match(&headers)?;
    // идею человека в корзину убирает человек (M5.7 §5.6, §9)
    super::ideas::deny_token_on_human_idea(&p, &sc.entry, true)?;
    let attrib = attribution(&p, &headers, None)?;
    delete_entry(&sc.db, sc.entry.id, Some(rev), &attrib).await?;
    st.index.notify();
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "entry.delete",
        sc.entry.id,
        audit_details(sc.space.id, sc.entry.kind),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /entries/{id}/restore` → 200 `Entry`; аудит `entry.restore`. Отчёт — только владелец
/// решения в WebUI ([`report_trash_allowed`], иначе 403).
pub async fn restore(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    report_trash_allowed(&st, &p, &sc)?;
    // идею человека из корзины возвращает человек (M5.7 §7.1, §9)
    super::ideas::deny_token_on_human_idea(&p, &sc.entry, true)?;
    let rev = optional_if_match(&headers)?;
    let attrib = attribution(&p, &headers, None)?;
    // имя термина могли занять, пока он лежал в корзине: 409 term-conflict — с маской агенту
    let e = restore_entry(&sc.db, sc.entry.id, rev, &attrib)
        .await
        .map_err(|e| agent_view::view_error(&p, e.into()))?;
    st.index.notify();
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "entry.restore",
        sc.entry.id,
        audit_details(sc.space.id, sc.entry.kind),
    )
    .await?;
    Ok(Json(agent_view::view_entry_json_owned(&p, json!(e)).await?))
}

/// `GET /entries/{id}/revisions` → `{items}` по возрастанию номера.
pub async fn revisions(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live_entry(&sc)?;
    let items = entry_revisions(&sc.db, sc.entry.id).await?;
    // все ревизии с телами (до 1 МБ каждое) — длинные маскируются в пуле; метки без номеров:
    // номера действуют только у текущей записи (M7 §4.3.2 п. 1, круг правок 4 задачи 6)
    let items = agent_view::view_json_owned(&p, json!(items), agent_view::ENTRY_FIELDS).await?;
    Ok(Json(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_data_rules() {
        assert!(check_changelog(&json!({"kind": "feat"})).is_ok());
        assert!(check_changelog(&json!({"kind": "feat", "refs": {"commits": ["abc"]}})).is_ok());
        for bad in [
            json!({}),
            json!({"kind": "oops"}),
            json!({"kind": 1}),
            json!({"kind": "fix", "refs": []}),
            json!({"kind": "fix", "refs": {"urls": []}}),
            json!({"kind": "fix", "refs": {"files": "a"}}),
            json!({"kind": "fix", "refs": {"files": [1]}}),
            json!({"kind": "fix", "refs": {"issues": vec!["x"; 201]}}),
        ] {
            assert!(check_changelog(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn release_data_rules() {
        let d = check_release(&json!({"version": " 1.0.0 "})).unwrap();
        assert_eq!(d["version"], "1.0.0");
        assert_eq!(d["date"].as_str().unwrap().len(), 10);
        let d = check_release(
            &json!({"version": "1", "date": "2026-02-28", "git_tag": "v1",
            "commits": [{"sha": "a", "subject": "s"}, {"sha": "b"}]}),
        )
        .unwrap();
        assert_eq!(d["date"], "2026-02-28");
        for bad in [
            json!({}),
            json!({"version": ""}),
            json!({"version": "x".repeat(65)}),
            json!({"version": "1", "date": "2026-02-30"}),
            json!({"version": "1", "date": "28.02.2026"}),
            json!({"version": "1", "date": 5}),
            json!({"version": "1", "git_tag": 5}),
            json!({"version": "1", "commits": "a"}),
            json!({"version": "1", "commits": ["abc"]}),
            json!({"version": "1", "commits": [{"subject": "s"}]}),
            json!({"version": "1", "commits": vec![json!({"sha": "a"}); 5001]}),
        ] {
            assert!(check_release(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn time_forms() {
        assert!(parse_time("2026-10-21", "since").is_ok());
        assert!(parse_time("2026-10-21T10:00:00Z", "since").is_ok());
        assert!(parse_time("вчера", "since").is_err());
    }
}
