//! Vault (§7.6, §12): секреты пользователя.
//!
//! - `GET /spaces/{space}/vault?q=` — метаданные (без значений), `q` — поиск `vault_fts` по
//!   имени и открытым полям (слова от 3 символов — префиксом, порядок `bm25`); запасной путь
//!   для `q` короче 3 символов — подстрока имени, `data.url`, `data.description`;
//! - `POST /spaces/{space}/vault {title, data: {type, username?, url?, description?, purpose?,
//!   address?, limits?, notes?}, secret?}` → 201 (имя занято в области → 409). `secret`
//!   обязателен, кроме типов `host` и `environment` (окружение, M4.5 §8): у них без значения
//!   в базе пустой блоб, раскрытие отдаёт `secret: null`;
//! - `GET /vault/{id}` — метаданные; `?reveal=1` — и значение (`Cache-Control: no-store`,
//!   аудит `vault.reveal` с `{item, session, machine}`, `actor` — id токена);
//! - `PATCH /vault/{id} {title?, data?, secret?}` (`If-Match: <rev>` необязателен) — новая
//!   ревизия; `DELETE /vault/{id}` — мягко, аудит `vault.delete`;
//! - `GET /vault/{id}/revisions` — ревизии без значений; `?reveal=1&rev=N` — значение
//!   старой ревизии, только администратору (аудит `vault.reveal` с `rev`); администратору
//!   история доступна и после удаления записи.
//!
//! **Область.** `{space}` — `knowledge` (личный Vault) или GUID доступного решения (своего
//! или одобренного участника): записи **пользователя запроса** в его собственной базе с
//! привязкой `agent_solution_id`. Vault владельца и других участников не виден никому, кроме
//! администратора с `X-Zam-User`; `rules`, `human`, id пространства → 404. Доступ к решению
//! проверяется на каждом запросе (в том числе по id записи): ожидание, отклонение, отзыв —
//! 403 `/problems/solution-access`. Запись — токен `rw` и режим участника `rw`
//! ([`super::scope::effective_write`]).
//!
//! **Блокировка** (решение владельца 2026-10-26; в M3 — 503 на всех эндпоинтах). Без
//! действующего мастер-ключа метаданные отвечают 200 с полем верхнего уровня `locked`
//! (`true` и `reason` при блокировке, иначе `false`): список, `GET /vault/{id}` без `reveal`,
//! `GET /vault/{id}/revisions` без `reveal`; в них только открытые поля и даты, без значений
//! и шифротекста. Значения (`reveal=1`), создание, правка и удаление — 503
//! `/problems/vault-locked`.
//!
//! Значения не попадают в логи, аудит, FTS и Qdrant: тело запроса не логируется, ошибки
//! разбора тела — без цитаты значения, у Vault нет чанков и заданий индексации.
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use zam_core::{Id, SolutionGuid};
use zam_store::{
    Db, User, VaultMeta, VaultScope, check_vault_title, vault_delete, vault_get, vault_get_any,
    vault_list, vault_put, vault_revision_secret, vault_revisions, vault_update,
};
use zeroize::Zeroizing;

use super::nodes::{actor, write::if_match};
use super::scope::{Role, effective_write, solution_role};
use super::{parse_id, query};
use crate::app::AppState;
use crate::attrib::{MACHINE_HEADER, SESSION_HEADER, attribution, header_value};
use crate::auth::{Client, Principal, target_user};
use crate::error::ApiError;
use crate::vault::VaultState;

/// Допустимые `data.type`.
pub const VAULT_TYPES: [&str; 9] = [
    "password",
    "token",
    "ssh_key",
    "connection_string",
    "url",
    "repo",
    "note",
    "host",
    "environment",
];
/// Предел значения секрета, байт UTF-8. [решение исполнителя]
pub const SECRET_MAX_BYTES: usize = 64 * 1024;
/// Пределы открытых полей `data`, символов. [решение исполнителя]
const USERNAME_MAX_CHARS: usize = 200;
const URL_MAX_CHARS: usize = 2000;
const DESCRIPTION_MAX_CHARS: usize = 2000;
const PURPOSE_MAX_CHARS: usize = 500;
const ADDRESS_MAX_CHARS: usize = 500;
const LIMITS_MAX_CHARS: usize = 1000;
const NOTES_MAX_CHARS: usize = 2000;

fn unprocessable(m: impl Into<String>) -> ApiError {
    ApiError::Unprocessable(m.into())
}

/// Тело запроса Vault: ошибка разбора — без цитаты тела (в нём значение секрета).
fn vault_body<T: DeserializeOwned>(b: Result<Bytes, BytesRejection>) -> Result<T, ApiError> {
    let b = match b {
        Ok(b) => b,
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return Err(ApiError::PayloadTooLarge(
                "тело запроса превышает предел".into(),
            ));
        }
        Err(_) => return Err(ApiError::BadRequest("некорректное тело запроса".into())),
    };
    serde_json::from_slice(&b).map_err(|e| {
        ApiError::BadRequest(format!(
            "некорректное тело запроса Vault (строка {}, столбец {}): ожидается JSON-объект \
             с полями title, data, secret",
            e.line(),
            e.column()
        ))
    })
}

/// `data` записи: объект `{type, username?, url?, description?, purpose?, address?, limits?,
/// notes?}`; `null` — поле не задано.
fn check_data(d: &Value) -> Result<Value, ApiError> {
    let o = d
        .as_object()
        .ok_or_else(|| unprocessable("data должно быть JSON-объектом"))?;
    let mut out = Map::new();
    for (k, v) in o {
        let max = match k.as_str() {
            "type" => {
                let t = v.as_str().unwrap_or("");
                if !VAULT_TYPES.contains(&t) {
                    return Err(unprocessable(format!(
                        "data.type: одно из {}",
                        VAULT_TYPES.join(", ")
                    )));
                }
                out.insert(k.clone(), v.clone());
                continue;
            }
            "username" => USERNAME_MAX_CHARS,
            "url" => URL_MAX_CHARS,
            "description" => DESCRIPTION_MAX_CHARS,
            "purpose" => PURPOSE_MAX_CHARS,
            "address" => ADDRESS_MAX_CHARS,
            "limits" => LIMITS_MAX_CHARS,
            "notes" => NOTES_MAX_CHARS,
            _ => return Err(unprocessable(format!("data.{k}: неизвестное поле"))),
        };
        match v {
            Value::Null => {}
            Value::String(s) if s.chars().count() <= max => {
                out.insert(k.clone(), v.clone());
            }
            _ => {
                return Err(unprocessable(format!("data.{k}: строка до {max} символов")));
            }
        }
    }
    if !out.contains_key("type") {
        return Err(unprocessable(format!(
            "data.type обязателен: одно из {}",
            VAULT_TYPES.join(", ")
        )));
    }
    Ok(Value::Object(out))
}

/// Тип записи окружения: значение необязательно.
fn env_type(data: &Value) -> bool {
    data.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| zam_store::VAULT_ENV_TYPES.contains(&t))
}

fn secret_required() -> ApiError {
    unprocessable("secret обязателен (необязателен только у типов host и environment)")
}

/// Значение: непустое, до [`SECRET_MAX_BYTES`].
fn check_secret(s: &str) -> Result<(), ApiError> {
    if s.is_empty() {
        return Err(unprocessable("secret: пустое значение"));
    }
    if s.len() > SECRET_MAX_BYTES {
        return Err(ApiError::PayloadTooLarge(format!(
            "secret длиннее {SECRET_MAX_BYTES} байт"
        )));
    }
    Ok(())
}

fn check_title(t: &str) -> Result<String, ApiError> {
    check_vault_title(t).map_err(|e| unprocessable(e.to_string()))
}

/// `{space}` Vault: `knowledge` или GUID решения; прочее → 404.
fn parse_scope(raw: &str) -> Result<VaultScope, ApiError> {
    match raw.trim() {
        "knowledge" => Ok(VaultScope::Knowledge),
        s => s
            .parse::<SolutionGuid>()
            .map(VaultScope::Solution)
            .map_err(|_| ApiError::NotFound),
    }
}

/// Роль принципала в области Vault: личный Vault — `Personal` (администратор — `Admin`);
/// Vault решения — роль в решении по реестру (каждый запрос: отзыв действует сразу).
async fn scope_role(
    st: &AppState,
    p: &Principal,
    user: &User,
    scope: &VaultScope,
) -> Result<Role, ApiError> {
    match scope {
        VaultScope::Knowledge => Ok(match p {
            Principal::Admin { .. } => Role::Admin,
            Principal::Token(_) | Principal::User { .. } => Role::Personal,
        }),
        VaultScope::Solution(g) => {
            let info = st.system.get_solution(g).await?.ok_or(ApiError::NotFound)?;
            solution_role(st, p, user, &info).await
        }
    }
}

/// Поля блокировки в ответе метаданных: `locked` и при блокировке `reason`.
fn put_lock(st: &AppState, o: &mut Map<String, Value>) {
    match st.vault.state() {
        VaultState::Ready => {
            o.insert("locked".into(), json!(false));
        }
        VaultState::Locked { reason } => {
            o.insert("locked".into(), json!(true));
            o.insert("reason".into(), json!(reason));
        }
    }
}

/// Область запроса: пользователь (владелец Vault) и его база.
struct Area {
    user: User,
    db: Db,
}

async fn area(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    scope: &VaultScope,
    write: bool,
) -> Result<Area, ApiError> {
    // метаданные отвечают и при блокировке, запись требует мастер-ключ
    if write {
        st.vault.ready()?;
    }
    let user = target_user(st, p, headers).await?;
    let role = scope_role(st, p, &user, scope).await?;
    if write {
        effective_write(p, role)?;
    }
    let db = st.users.get(user.id).await?;
    Ok(Area { user, db })
}

/// Живая запись пользователя запроса по id с проверкой доступа к её области. `keys` —
/// операция требует мастер-ключ (запись, значение): заблокированный Vault → 503.
async fn item(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw_id: &str,
    write: bool,
    keys: bool,
) -> Result<(Area, VaultMeta, Vec<u8>), ApiError> {
    if keys || write {
        st.vault.ready()?;
    }
    let id = parse_id(raw_id)?;
    let user = target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    // только база пользователя запроса: чужой Vault не существует (404)
    let (meta, blob) = vault_get(&db, id).await?;
    let role = scope_role(st, p, &user, &meta.scope).await?;
    if write {
        effective_write(p, role)?;
    }
    Ok((Area { user, db }, meta, blob))
}

/// Запись для истории: администратору — и удалённая (история не теряется с удалением,
/// §7.6 «администратор видит всё»), токену — только живая ([`item`]).
async fn history_item(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw_id: &str,
    keys: bool,
) -> Result<(Area, VaultMeta), ApiError> {
    if !matches!(p, Principal::Admin { .. }) {
        let (a, m, _) = item(st, p, headers, raw_id, false, keys).await?;
        return Ok((a, m));
    }
    if keys {
        st.vault.ready()?;
    }
    let id = parse_id(raw_id)?;
    let user = target_user(st, p, headers).await?;
    let db = st.users.get(user.id).await?;
    let (meta, _deleted) = vault_get_any(&db, id).await?;
    scope_role(st, p, &user, &meta.scope).await?;
    Ok((Area { user, db }, meta))
}

/// JSON метаданных: `space` (`knowledge` или GUID), `agent_solution_id`.
fn meta_json(m: &VaultMeta) -> Value {
    let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut v {
        let guid = m.agent_solution_id().map(|g| g.as_str().to_string());
        o.insert(
            "space".into(),
            json!(guid.clone().unwrap_or_else(|| "knowledge".into())),
        );
        o.insert("agent_solution_id".into(), json!(guid));
    }
    v
}

/// Ответ со значением: не кэшируется.
fn revealed(v: Value) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Аудит операции Vault (без значения): `{item, session, machine, rev?}`.
#[allow(clippy::too_many_arguments)]
async fn audit(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    client: &Client,
    user: Id,
    action: &str,
    item: Id,
    rev: Option<i64>,
) -> Result<(), ApiError> {
    let mut details = json!({
        "item": item,
        "session": header_value(headers, SESSION_HEADER)?,
        "machine": header_value(headers, MACHINE_HEADER)?,
    });
    if let (Some(r), Value::Object(o)) = (rev, &mut details) {
        o.insert("rev".into(), json!(r));
    }
    st.system
        .audit(
            &actor(p),
            Some(user),
            action,
            Some(&item.to_string()),
            Some(&client.ip.to_string()),
            details,
        )
        .await?;
    Ok(())
}

/// Значение как текст (записывается сервером только из строк JSON).
fn secret_text(plain: &[u8]) -> Result<String, ApiError> {
    String::from_utf8(plain.to_vec())
        .map_err(|_| ApiError::Internal("значение Vault — не UTF-8".into()))
}

/// Раскрытое значение: строка, у записи окружения без значения (пустой блоб) — `null`.
async fn secret_of(
    st: &AppState,
    user: Id,
    item: Id,
    rev: i64,
    blob: &[u8],
) -> Result<Value, ApiError> {
    if blob.is_empty() {
        return Ok(Value::Null);
    }
    let plain = st.vault.decrypt(user, item, rev, blob).await?;
    Ok(Value::String(secret_text(&plain)?))
}

/// Флаг `reveal`: `1`/`true` — да, `0`/`false`/нет — нет.
fn flag(v: Option<&str>) -> Result<bool, ApiError> {
    match v {
        None | Some("0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(_) => Err(ApiError::BadRequest("reveal: 1 или 0".into())),
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    q: Option<String>,
}

/// `GET /spaces/{space}/vault?q=` → `{items}` (без значений).
pub async fn list(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let scope = parse_scope(&space)?;
    let a = area(&st, &p, &headers, &scope, false).await?;
    let items = vault_list(&a.db, &scope, q.q.as_deref()).await?;
    let mut v = json!({ "items": items.iter().map(meta_json).collect::<Vec<_>>() });
    if let Value::Object(o) = &mut v {
        put_lock(&st, o);
    }
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    title: String,
    data: Value,
    secret: Option<Zeroizing<String>>,
}

/// `POST /spaces/{space}/vault` → 201 метаданные.
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let scope = parse_scope(&space)?;
    let a = area(&st, &p, &headers, &scope, true).await?;
    let req: Create = vault_body(body)?;
    let title = check_title(&req.title)?;
    let data = check_data(&req.data)?;
    let attrib = attribution(&p, &headers, None)?;
    let id = Id::new();
    let blob = match &req.secret {
        Some(s) => {
            check_secret(s)?;
            st.vault.encrypt(a.user.id, id, 1, s.as_bytes()).await?
        }
        // окружение без значения: пустой блоб, шифровать нечего
        None if env_type(&data) => Vec::new(),
        None => return Err(secret_required()),
    };
    let m = vault_put(&a.db, id, &scope, &title, data, blob, &attrib).await?;
    Ok((StatusCode::CREATED, Json(meta_json(&m))))
}

#[derive(Deserialize)]
pub struct GetQuery {
    reveal: Option<String>,
}

/// `GET /vault/{id}` → метаданные; `?reveal=1` — со значением и аудитом.
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    q: Result<Query<GetQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let reveal = flag(q.reveal.as_deref())?;
    let (a, m, blob) = item(&st, &p, &headers, &id, false, reveal).await?;
    let mut v = meta_json(&m);
    if !reveal {
        if let Value::Object(o) = &mut v {
            put_lock(&st, o);
        }
        return Ok(Json(v).into_response());
    }
    let secret = secret_of(&st, a.user.id, m.id, m.rev, &blob).await?;
    // аудит до выдачи: не записан — значение не выдаётся
    audit(
        &st,
        &p,
        &headers,
        &client,
        a.user.id,
        "vault.reveal",
        m.id,
        None,
    )
    .await?;
    if let Value::Object(o) = &mut v {
        o.insert("secret".into(), secret);
    }
    Ok(revealed(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    title: Option<String>,
    data: Option<Value>,
    secret: Option<Zeroizing<String>>,
}

/// `PATCH /vault/{id}` (необязательный `If-Match: <rev>`) → 200 метаданные / 409.
/// Каждая правка — новая ревизия со своим шифротекстом (AAD связан с ревизией): без нового
/// значения прежнее перешифровывается. Пустая правка — запись как есть.
pub async fn patch(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let (a, m, blob) = item(&st, &p, &headers, &id, true, true).await?;
    let req: Patch = vault_body(body)?;
    let expected = if headers.contains_key(header::IF_MATCH) {
        if_match(&headers)?
    } else {
        m.rev
    };
    let title = req.title.as_deref().map(check_title).transpose()?;
    let data = req.data.as_ref().map(check_data).transpose()?;
    if let Some(s) = &req.secret {
        check_secret(s)?;
    }
    if title.is_none() && data.is_none() && req.secret.is_none() {
        if expected != m.rev {
            return Err(ApiError::conflict(format!(
                "ревизия записи Vault {}, а не {expected}",
                m.rev
            )));
        }
        return Ok(Json(meta_json(&m)));
    }
    let attrib = attribution(&p, &headers, None)?;
    // тип после правки без значения — только окружение (пустой блоб у прежнего типа не в счёт)
    let after_env = data.as_ref().map_or_else(|| env_type(&m.data), env_type);
    let new_blob = match &req.secret {
        Some(s) => {
            st.vault
                .encrypt(a.user.id, m.id, expected + 1, s.as_bytes())
                .await?
        }
        None if blob.is_empty() => {
            if !after_env {
                return Err(secret_required());
            }
            Vec::new()
        }
        None => {
            let plain = st.vault.decrypt(a.user.id, m.id, m.rev, &blob).await?;
            st.vault
                .encrypt(a.user.id, m.id, expected + 1, &plain)
                .await?
        }
    };
    let m = vault_update(&a.db, m.id, expected, title, data, new_blob, &attrib).await?;
    Ok(Json(meta_json(&m)))
}

/// `DELETE /vault/{id}` — мягкое удаление → 204; аудит `vault.delete`.
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (a, m, _) = item(&st, &p, &headers, &id, true, true).await?;
    let attrib = attribution(&p, &headers, None)?;
    vault_delete(&a.db, m.id, &attrib).await?;
    audit(
        &st,
        &p,
        &headers,
        &client,
        a.user.id,
        "vault.delete",
        m.id,
        None,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RevQuery {
    reveal: Option<String>,
    rev: Option<i64>,
}

/// `GET /vault/{id}/revisions` → `{items}` без значений; `?reveal=1&rev=N` — значение
/// ревизии `N` (только администратору, аудит `vault.reveal` с `rev`).
pub async fn revisions(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
    q: Result<Query<RevQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let reveal = flag(q.reveal.as_deref())?;
    let (a, m) = history_item(&st, &p, &headers, &id, reveal).await?;
    let items = vault_revisions(&a.db, m.id).await?;
    if !reveal {
        let mut v = json!({ "items": items });
        if let Value::Object(o) = &mut v {
            put_lock(&st, o);
        }
        return Ok(Json(v).into_response());
    }
    if !matches!(p, Principal::Admin { .. }) {
        return Err(ApiError::Denied(
            "значение прежней ревизии раскрывает только администратор".into(),
        ));
    }
    let rev = q
        .rev
        .ok_or_else(|| ApiError::BadRequest("reveal=1: укажите rev".into()))?;
    let r = items
        .into_iter()
        .find(|r| r.rev == rev)
        .ok_or(ApiError::NotFound)?;
    let blob = vault_revision_secret(&a.db, m.id, rev).await?;
    let secret = secret_of(&st, a.user.id, m.id, rev, &blob).await?;
    audit(
        &st,
        &p,
        &headers,
        &client,
        a.user.id,
        "vault.reveal",
        m.id,
        Some(rev),
    )
    .await?;
    let mut v = serde_json::to_value(&r).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut v {
        o.insert("id".into(), json!(m.id));
        o.insert("secret".into(), secret);
    }
    Ok(revealed(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_rules() {
        let d =
            check_data(&json!({"type": "token", "url": "https://x", "username": null})).unwrap();
        assert_eq!(d, json!({"type": "token", "url": "https://x"}));
        for t in VAULT_TYPES {
            assert!(check_data(&json!({ "type": t })).is_ok(), "{t}");
        }
        let full = json!({"type": "host", "purpose": "p".repeat(500), "address": "a".repeat(500),
            "limits": "l".repeat(1000), "notes": "n".repeat(2000)});
        assert!(check_data(&full).is_ok());
        assert!(env_type(&full) && !env_type(&json!({"type": "note"})));
        for bad in [
            json!([]),
            json!({}),
            json!({"type": "magic"}),
            json!({"type": 1}),
            json!({"type": "note", "extra": "x"}),
            json!({"type": "note", "url": 5}),
            json!({"type": "note", "username": "u".repeat(201)}),
            json!({"type": "host", "purpose": "p".repeat(501)}),
            json!({"type": "host", "address": "a".repeat(501)}),
            json!({"type": "host", "limits": "l".repeat(1001)}),
            json!({"type": "host", "notes": "n".repeat(2001)}),
        ] {
            assert!(
                matches!(check_data(&bad), Err(ApiError::Unprocessable(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn scope_forms() {
        assert_eq!(parse_scope("knowledge").unwrap(), VaultScope::Knowledge);
        assert!(matches!(
            parse_scope("0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e31").unwrap(),
            VaultScope::Solution(_)
        ));
        for bad in ["rules", "human", "Knowledge", "", &Id::new().to_string()] {
            assert!(matches!(parse_scope(bad), Err(ApiError::NotFound)), "{bad}");
        }
    }

    #[test]
    fn body_errors_do_not_quote_values() {
        let b = Bytes::from_static(br#"{"title": "a", "data": {}, "secret": 12345678987654321}"#);
        let e = vault_body::<Create>(Ok(b)).err().unwrap();
        assert!(!e.to_string().contains("12345678987654321"), "{e}");
        assert!(check_secret("").is_err());
        assert!(matches!(
            check_secret(&"x".repeat(SECRET_MAX_BYTES + 1)),
            Err(ApiError::PayloadTooLarge(_))
        ));
        assert!(flag(Some("1")).unwrap());
        assert!(flag(Some("yes")).is_err());
    }
}
