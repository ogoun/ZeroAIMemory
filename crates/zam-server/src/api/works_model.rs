//! Подключённая модель исполнителя (M7.3, спецификация §6.2 пп. 8, 9): проверка подключения и
//! журнал отправок — только человеку (сессия WebUI пользователя или администратор; по токену —
//! 403: агент не проверяет и не читает подключение владельца).
//!
//! - `POST /works/model/test[?user=<id>]` → `{ok, ms, class?, hint?, thinking?}`: фиксированный
//!   запрос без данных памяти ([`crate::works::conn::test_connection`]); строка журнала
//!   `kind = test`; успешная проверка снимает паузу (кроме `vault_changed`). На паузе
//!   `vault_changed` (и при адресе записи Vault, изменённом после сохранения) — 409 «адрес
//!   изменён: был <хост> → стал <хост> — подтвердите» `{code: vault_changed, was, now}` **без
//!   обращения** (подтверждение — `PATCH works.model {confirm_url}`); `[works] enabled =
//!   false` и `model_access = "off"` — 409 `/problems/works-disabled` (один вид, как у ручных
//!   запусков, решение 76; причина — в `detail`; финальное ревью M7.3, Minor 5); у пользователя
//!   уже идёт обращение — 409 `code: model_busy`; нет подключения `works.model` — 422.
//! - `GET /works/calls?days=&user=` → `{days, calls: […]}`: метаданные обращений за `days`
//!   суток (1..=180, по умолчанию 7; не больше [`CALLS_LIMIT`] строк, новые первыми) — без
//!   текста пакета и ответа.
//!
//! Пользователь запроса — как у очереди ([`super::works::works_user`]: `?user=` чужого — только
//! администратору, без него — пользователь запроса, у администратора — `X-Zam-User` или он сам),
//! но только человеку: по токену — 403.
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::work_calls::{self, CallRow};

use super::query;
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;
use crate::works::NightCtx;
use crate::works::conn::{self, TestRefusal};

/// Строк журнала в ответе `GET /works/calls` — не больше.
pub const CALLS_LIMIT: usize = 1000;
/// Окно журнала по умолчанию и наибольшее, суток (хранение — 180 суток).
pub const CALLS_DAYS_DEFAULT: u32 = 7;
pub const CALLS_DAYS_MAX: u32 = 180;

#[derive(Deserialize)]
pub struct UserQuery {
    user: Option<String>,
}

#[derive(Deserialize)]
pub struct CallsQuery {
    user: Option<String>,
    days: Option<u32>,
}

/// Пользователь запроса — только человеку (токен — 403), выбор пользователя — тот же, что у
/// очереди ([`super::works::works_user`]; финальное ревью M7.3, Minor 6: одна политика).
async fn target_user(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    user: Option<&str>,
) -> Result<Id, ApiError> {
    if !p.is_human() {
        return Err(ApiError::Forbidden);
    }
    super::works::works_user(st, p, headers, user).await
}

/// `POST /works/model/test[?user=]` → `{ok, ms, class?, hint?, thinking?}`.
pub async fn test(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<UserQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let user = target_user(&st, &p, &headers, q.user.as_deref()).await?;
    // общий выключатель — тот же ответ, что у ручных запусков (решение координатора 76)
    if !st.cfg.works.enabled {
        return Err(ApiError::WorksDisabled(
            crate::api::works::WORKS_DISABLED_DETAIL.into(),
        ));
    }
    let ctx = NightCtx::new(st.clone());
    let ip = client.ip.to_string();
    match conn::test_connection(&ctx, user, &p.actor(), Some(&ip)).await {
        Ok(out) => Ok(Json(out.to_json())),
        // `model_access = "off"` — тот же вид, что `enabled = false` (финальное ревью M7.3,
        // Minor 5): клиент разбирает один тип проблемы, причина — в `detail`
        Err(TestRefusal::Disabled(why)) => Err(ApiError::WorksDisabled(why.into())),
        Err(TestRefusal::Busy) => Err(ApiError::conflict_code(
            "model_busy",
            "у пользователя уже идёт обращение к модели — повторите позже",
            &[],
        )),
        Err(TestRefusal::NoConnection) => Err(ApiError::Unprocessable(
            "нет подключения works.model: сохраните его в настройках (запись личного Vault и \
             модель)"
                .into(),
        )),
        Err(TestRefusal::VaultChanged { was, now }) => Err(ApiError::conflict_code(
            "vault_changed",
            format!(
                "адрес изменён: был {was} → стал {now} — подтвердите: сохраните подключение с \
                 confirm_url = \"{now}\""
            ),
            &[("was", json!(was)), ("now", json!(now))],
        )),
    }
}

/// Строка журнала в ответе: работа — `j:<id>`, время — формат хранилища.
fn call_json(c: &CallRow) -> Value {
    json!({
        "ts": zam_store::time::fmt_ts(c.ts),
        "work": c.work_id.map(|id| format!("j:{id}")),
        "kind": c.kind,
        "host": c.host,
        "model": c.model,
        "input_tokens": c.input_tokens,
        "output_tokens": c.output_tokens,
        "estimated": c.estimated,
        "ms": c.ms,
        "outcome": c.outcome,
        "http_status": c.http_status,
        "external": c.external,
        "items": c.items,
        "run": c.run,
    })
}

/// `GET /works/calls?days=&user=` → `{days, calls}`.
pub async fn calls(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<CallsQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let user = target_user(&st, &p, &headers, q.user.as_deref()).await?;
    let days = q.days.unwrap_or(CALLS_DAYS_DEFAULT);
    if !(1..=CALLS_DAYS_MAX).contains(&days) {
        return Err(ApiError::BadRequest(format!(
            "days: от 1 до {CALLS_DAYS_MAX}"
        )));
    }
    let since = zam_store::time::now() - jiff::SignedDuration::from_hours(24 * i64::from(days));
    let db = st.users.get(user).await?;
    let rows = work_calls::list_calls(&db, since, CALLS_LIMIT).await?;
    Ok(Json(json!({
        "days": days,
        "calls": rows.iter().map(call_json).collect::<Vec<_>>(),
    })))
}
