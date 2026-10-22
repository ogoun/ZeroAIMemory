//! Записи журналов решения (§7.2–7.5, §12): Changelog, Releases и общие операции записей.
//!
//! - `GET/POST /spaces/{space}/entries` — лента и создание (`Idempotency-Key`: 201, повтор —
//!   200; ключ действует в границах автора, расхождение запроса → 422
//!   `/problems/idempotency-mismatch`, ключ удалённой записи → 409);
//! - `GET/PATCH/DELETE /entries/{id}` (`If-Match: <rev>` необязателен, расхождение → 409),
//!   `POST /entries/{id}/restore`, `GET /entries/{id}/revisions`.
//!
//! Changelog и release создаются только в пространстве решения (иначе 422); `kind: insight`
//! уходит в [`super::insights::create`] (дедупликация, `scope`, триггеры, замещение).
//! У insight `GET`/`PATCH` отдают и `triggers`; `PATCH` принимает `status: active | revisit`
//! (из `superseded`/`retracted` и в них — 422 **[решение исполнителя]**: замещение и отзыв —
//! отдельные операции, возврата нет), `triggers` (набор заменяется целиком, ошибка — 422
//! `triggers[i]: …`) и `certainty` (`null` — снять) с `reason` — по матрице прав §7.5
//! ([`crate::certainty`]): запись `axiom` агент не меняет и не удаляет (403).
//! Доступ — как у узлов: участник общего решения работает в базе владельца только с
//! записями пространства решения, автор записи — он сам. В логи и аудит текст записей не
//! попадает.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use zam_store::{
    Certainty, Entry, EntryFilter, EntryKind, EntryPatch, EntryStatus, NewEntry, SpaceKind,
    create_entry, delete_entry, entry_by_idempotency_key, entry_revisions, list_entries,
    restore_entry, update_entry,
};

use super::insights::{entry_json, parse_triggers};
use super::nodes::{audit_node, write::if_match};
use super::scope::{Mode, entry_scope, live_entry, space_scope};
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
const REFS_MAX: usize = 200;
/// Предел длины одной ссылки, символов. [решение исполнителя]
const REF_MAX_CHARS: usize = 1000;
/// Предел числа коммитов релиза.
const COMMITS_MAX: usize = 5000;
/// Предел длины версии релиза, символов.
const VERSION_MAX_CHARS: usize = 64;
/// Предел длины `git_tag` и `sha`, символов. [решение исполнителя]
const TAG_MAX_CHARS: usize = 200;
const SUBJECT_MAX_CHARS: usize = 1000;
/// Лента по умолчанию.
const LIST_DEFAULT: usize = 50;

pub(crate) fn unprocessable(m: impl Into<String>) -> ApiError {
    ApiError::Unprocessable(m.into())
}

/// Массив строк до `max` элементов длиной до `item_max` символов.
fn string_array(v: &Value, what: &str, max: usize, item_max: usize) -> Result<(), ApiError> {
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
    Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string()
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
/// `kind: insight` — [`super::insights::create`].
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(space): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let raw: Value = json_body(body)?;
    if raw.get("kind").and_then(Value::as_str) == Some(EntryKind::Insight.as_str()) {
        return super::insights::create(&st, &p, &headers, &client, &space, raw).await;
    }
    let req: Create = serde_json::from_value(raw)
        .map_err(|e| ApiError::BadRequest(format!("некорректное тело: {e}")))?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Write).await?;
    if sc.space.kind != SpaceKind::Solution {
        return Err(unprocessable(
            "changelog и release ведутся только в пространстве решения",
        ));
    }
    check_title(&req.title)?;
    let mut data = check_data(req.kind, &req.data)?;
    check_size(&st, &req.body)?;
    let attrib = attribution(&p, &headers, req.message)?;
    let key = idempotency_key(&headers)?;
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
    };
    let (entry, created) = create_entry(&sc.db, new, &attrib).await?;
    if created {
        st.index.notify();
    }
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let mut v = json!(entry);
    super::secrets::attach_entry(&mut v, &entry);
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
    since: Option<String>,
    until: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
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

/// `GET /spaces/{space}/entries` → `{items, next_cursor}`.
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
        Some(k) => {
            Some(EntryKind::parse(k).ok_or_else(|| ApiError::BadRequest(format!("kind: «{k}»")))?)
        }
        None => None,
    };
    let f = EntryFilter {
        space_id: sc.space.id,
        kind,
        statuses: parse_list(q.status.as_deref(), "status", EntryStatus::parse)?,
        certainty: parse_list(q.certainty.as_deref(), "certainty", Certainty::parse)?,
        area: q.area.filter(|a| !a.is_empty()),
        tag: q.tag.filter(|a| !a.is_empty()),
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
    };
    let (items, next) = list_entries(&sc.db, f).await?;
    Ok(Json(json!({ "items": items, "next_cursor": next })))
}

/// `GET /entries/{id}` → `Entry` (у insight — с `triggers`).
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live_entry(&sc)?;
    Ok(Json(entry_json(&sc.db, &sc.entry).await?))
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
    message: Option<String>,
}

/// Смена статуса insight правкой: только `active ↔ revisit` — иначе 422.
fn check_status_change(e: &Entry, s: EntryStatus) -> Result<(), ApiError> {
    if e.kind != EntryKind::Insight {
        return Err(unprocessable("status есть только у insight"));
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

/// `PATCH /entries/{id}` (необязательный `If-Match: <rev>`) → 200 `Entry` / 409 / 413 / 422.
pub async fn patch(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live_entry(&sc)?;
    let req: Patch = json_body(body)?;
    let rev = headers
        .contains_key(header::IF_MATCH)
        .then(|| if_match(&headers))
        .transpose()?;
    for t in [&req.body, &req.append_body].into_iter().flatten() {
        check_size(&st, t)?;
    }
    if let Some(t) = &req.title {
        check_title(t)?;
    }
    if let Some(s) = req.status {
        check_status_change(&sc.entry, s)?;
    }
    if req.certainty.is_some() && sc.entry.kind != EntryKind::Insight {
        return Err(unprocessable("certainty есть только у insight"));
    }
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
            super::insights::check_insight_data(d)?;
            Some(d.clone())
        }
        (Some(d), kind) => Some(check_data(kind, d)?),
        (None, _) => None,
    };
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
    };
    let before = sc.entry.rev;
    let e = update_entry(&sc.db, sc.entry.id, rev, patch, &attrib).await?;
    if e.rev != before {
        st.index.notify();
    }
    let mut v = entry_json(&sc.db, &e).await?;
    super::secrets::attach_entry(&mut v, &e);
    Ok(Json(v))
}

/// Детали аудита операций записи (без текста записи).
pub(crate) fn audit_details(space: zam_core::Id, kind: EntryKind) -> Value {
    json!({ "space": space, "kind": kind.as_str() })
}

/// `DELETE /entries/{id}` — мягкое удаление → 204; аудит `entry.delete`.
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live_entry(&sc)?;
    let attrib = attribution(&p, &headers, None)?;
    delete_entry(&sc.db, sc.entry.id, &attrib).await?;
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

/// `POST /entries/{id}/restore` → 200 `Entry`; аудит `entry.restore`.
pub async fn restore(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<Json<Entry>, ApiError> {
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Write).await?;
    let attrib = attribution(&p, &headers, None)?;
    let e = restore_entry(&sc.db, sc.entry.id, &attrib).await?;
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
    Ok(Json(e))
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
