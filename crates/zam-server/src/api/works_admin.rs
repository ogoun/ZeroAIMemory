//! Настройки сервера внутренних работ в WebUI администратора (M7.6 §9.9, решения 171–173;
//! признак `admin_works_settings`). Только сессия администратора (`AdminOnly`: токен и сессия
//! пользователя — 403).
//!
//! - `GET /admin/works/settings` → `{rev, fields: {<ключ [works]>: {value, file, source,
//!   applies, options?, min?, max?}}}` — действующее значение, значение файла конфигурации,
//!   источник (`webui` — задано в WebUI, `config` — файл), когда действует (`next_call` — со
//!   следующего обращения, `next_night` — со следующего планирования ночи; перезапуск не нужен
//!   ни одному ключу), допустимые значения. То же — поле `settings` сводки `GET /admin/works`.
//! - `PATCH /admin/works/settings {rev, values: {<ключ>: значение | null}}` — значение из WebUI
//!   (сильнее файла) или `null` — «Вернуть к файлу». Проверка — та же, что у конфигурации
//!   ([`crate::works::server_settings::patched`]): незнакомый ключ, неверный тип, вне диапазона —
//!   400 с ключом. `rev` — номер правки, которую видел администратор: другой — 409 `code:
//!   settings_changed` с нынешним `rev` (набранное в форме не теряется, WebUI перечитывает
//!   значения). Правка без изменений — 200 без записи и аудита. Иначе — запись строки
//!   `works.server` (`system.db`, с проверкой прежней), новый `rev`, применение без перезапуска
//!   (действующая секция процесса, слоты `max_parallel`) и аудит `admin.works.settings {rev,
//!   changes: {<ключ>: {from, to, source}}}` — что было → стало (`source` — источник после
//!   правки). Ответ — как у `GET`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::parse_body;
use crate::app::AppState;
use crate::auth::{AdminOnly, Client};
use crate::error::ApiError;
use crate::works::server_settings::{self, KEYS, SETTING_KEY, Stored};

/// Автор аудита правок администратора.
const ACTOR: &str = "admin";
/// Действие аудита правки настроек сервера внутренних работ.
pub const AUDIT_ACTION: &str = "admin.works.settings";

/// `GET /admin/works/settings`.
pub async fn get_settings(_: AdminOnly, State(st): State<AppState>) -> Json<Value> {
    Json(st.works_cfg.to_json())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    rev: u64,
    values: Map<String, Value>,
}

/// 409: настройки изменились после того, как их прочитал администратор.
fn changed(rev: u64) -> ApiError {
    ApiError::conflict_code(
        "settings_changed",
        format!(
            "настройки сервера уже изменены (правка {rev}): перечитайте значения и сохраните ещё \
             раз"
        ),
        &[("rev", json!(rev))],
    )
}

/// `PATCH /admin/works/settings {rev, values}`.
pub async fn patch_settings(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let req: SettingsPatch = parse_body(&body)?;
    if req.values.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "values: хотя бы один ключ — {}",
            KEYS.join(", ")
        )));
    }
    let row = st.system.get_setting::<Value>(SETTING_KEY).await?;
    let cur = Stored::from_row(row.as_ref());
    if req.rev != cur.rev {
        return Err(changed(cur.rev));
    }
    let file = st.works_cfg.file();
    let next =
        server_settings::patched(file, &cur.values, &req.values).map_err(ApiError::BadRequest)?;
    if next.values == cur.values {
        return Ok(Json(st.works_cfg.to_json()));
    }
    let before = st.works();
    let stored = Stored {
        rev: cur.rev + 1,
        values: next.values,
    };
    let value = serde_json::to_value(&stored)
        .map_err(|e| ApiError::Internal(format!("настройки сервера: {e}")))?;
    if !st
        .system
        .swap_setting(SETTING_KEY, row, Some(value))
        .await?
    {
        // строку записал другой запрос между чтением и записью
        let now = st.system.get_setting::<Value>(SETTING_KEY).await?;
        return Err(changed(Stored::from_row(now.as_ref()).rev));
    }
    let mut changes = Map::new();
    for key in KEYS {
        let (from, to) = (
            server_settings::value_of(&before, key),
            server_settings::value_of(&next.effective, key),
        );
        let (was, now) = (
            cur.values.contains_key(*key),
            stored.values.contains_key(*key),
        );
        if from != to || was != now {
            changes.insert(
                (*key).to_string(),
                json!({"from": from, "to": to, "source": if now { "webui" } else { "config" }}),
            );
        }
    }
    let rev = stored.rev;
    let parallel = next.effective.max_parallel;
    if st.works_cfg.set(stored, next.effective) {
        st.model_calls.resize(parallel);
    }
    st.system
        .audit(
            ACTOR,
            None,
            AUDIT_ACTION,
            Some(SETTING_KEY),
            Some(&client.ip.to_string()),
            json!({"rev": rev, "changes": changes}),
        )
        .await?;
    Ok(Json(st.works_cfg.to_json()))
}
