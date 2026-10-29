//! Настройки пользователя на сервере (§10.3 «Настройки», решение контроллера R5, задача 13 M4):
//! переключатели типов подсказок хуков `hooks.<тип>`.
//!
//! - Хранение — `system.db`, таблица `user_settings` (миграция 0007): настройка — часть учётной
//!   записи, как пароль и токены, а не память; читается без открытия базы пользователя
//!   **[решение исполнителя, задача 13 M4]**.
//! - Ключи — `hooks.session_start | hooks.prompt | hooks.pre_tool | hooks.post_tool |
//!   hooks.checkpoint` и с M4.5 (§9.6) `hooks.subagent_start | hooks.file_context |
//!   hooks.session_journal | hooks.recovery`, значение — `bool`; нет строки — `true` (новые
//!   ключи у старых пользователей включены).
//! - `GET/PATCH /auth/settings` — свои (сессия пользователя; администратор — своего
//!   пользователя); по токену — 403: агент свои подсказки не выключает.
//!   `GET/PATCH /admin/users/{id}/settings` — администратор, любой пользователь.
//! - `PATCH {hooks: {<тип>: bool, …}}`: неизвестный тип или поле — 400, пустая правка — 400.
//!   Ответ — все настройки `{hooks: {…}}`. Аудит `user.settings.update {key, from, to}` на
//!   каждое изменение.
//! - Сервер отдаёт их в `/recall(session_start)` полем `hooks`; выключенный тип `/recall`
//!   (`session_start`, `prompt`, `post_tool`) отвечает пустым `text` и показ не учитывает (N4).

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::SystemStore;

use crate::app::AppState;
use crate::auth::{AdminOnly, Client, Principal};
use crate::error::ApiError;

use super::{parse_body, parse_id};

/// Типы подсказок хуков (порядок — порядок в ответе; M4.5 §9.6 добавил последние четыре).
pub const HOOK_KINDS: [&str; 9] = [
    "session_start",
    "prompt",
    "pre_tool",
    "post_tool",
    "checkpoint",
    "subagent_start",
    "file_context",
    "session_journal",
    "recovery",
];

/// Префикс ключей переключателей в `user_settings`.
const HOOKS_PREFIX: &str = "hooks.";

/// Переключатели типов подсказок пользователя (по умолчанию все включены).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookFlags([bool; HOOK_KINDS.len()]);

impl Default for HookFlags {
    fn default() -> Self {
        HookFlags([true; HOOK_KINDS.len()])
    }
}

impl HookFlags {
    /// Включён ли тип `kind` (один из [`HOOK_KINDS`]; неизвестный — включён).
    pub fn enabled(&self, kind: &str) -> bool {
        HOOK_KINDS
            .iter()
            .position(|k| *k == kind)
            .is_none_or(|i| self.0[i])
    }

    /// `{session_start: bool, …}`.
    pub fn to_json(self) -> Value {
        let mut m = Map::new();
        for (i, k) in HOOK_KINDS.iter().enumerate() {
            m.insert((*k).into(), Value::Bool(self.0[i]));
        }
        Value::Object(m)
    }
}

/// Переключатели пользователя из `user_settings`; нечитаемое значение — `true` (как без строки).
pub async fn hook_flags(sys: &SystemStore, user: Id) -> Result<HookFlags, zam_store::StoreError> {
    let mut f = HookFlags::default();
    for (key, value) in sys.user_settings(user).await? {
        let Some(kind) = key.strip_prefix(HOOKS_PREFIX) else {
            continue;
        };
        if let (Some(i), Some(b)) = (HOOK_KINDS.iter().position(|k| *k == kind), value.as_bool()) {
            f.0[i] = b;
        }
    }
    Ok(f)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    hooks: Option<Map<String, Value>>,
}

/// Правки из тела `PATCH`: `(ключ, значение)`.
fn parse_patch(body: &Bytes) -> Result<Vec<(String, Value)>, ApiError> {
    let req: SettingsPatch = parse_body(body)?;
    let hooks = req.hooks.unwrap_or_default();
    if hooks.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "нужно поле hooks: {{{}: bool}}",
            HOOK_KINDS.join("|")
        )));
    }
    let mut out = Vec::with_capacity(hooks.len());
    for (k, v) in hooks {
        if !HOOK_KINDS.contains(&k.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "hooks: неизвестный тип «{k}» — {}",
                HOOK_KINDS.join(" | ")
            )));
        }
        let Value::Bool(b) = v else {
            return Err(ApiError::BadRequest(format!("hooks.{k}: true или false")));
        };
        out.push((format!("{HOOKS_PREFIX}{k}"), Value::Bool(b)));
    }
    Ok(out)
}

async fn settings_json(st: &AppState, user: Id) -> Result<Value, ApiError> {
    Ok(json!({ "hooks": hook_flags(&st.system, user).await?.to_json() }))
}

/// Сохраняет правки и пишет аудит каждого изменения. Правка сравнивается с действующим
/// значением (нет строки — `true`): совпало — ни записи, ни аудита (fix round 1, I-2).
async fn apply(
    st: &AppState,
    actor: &str,
    user: Id,
    ip: &str,
    body: &Bytes,
) -> Result<Json<Value>, ApiError> {
    let edits = parse_patch(body)?;
    let current = hook_flags(&st.system, user).await?;
    let edits: Vec<(String, Value)> = edits
        .into_iter()
        .filter(|(key, v)| {
            let kind = key.strip_prefix(HOOKS_PREFIX).unwrap_or(key);
            v.as_bool() != Some(current.enabled(kind))
        })
        .collect();
    if !edits.is_empty() {
        let changed = st.system.set_user_settings(user, edits.clone()).await?;
        for (key, _) in changed {
            let kind = key.strip_prefix(HOOKS_PREFIX).unwrap_or(&key);
            let to = edits
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(true);
            st.system
                .audit(
                    actor,
                    Some(user),
                    "user.settings.update",
                    Some(&user.to_string()),
                    Some(ip),
                    json!({ "key": key, "from": current.enabled(kind), "to": to }),
                )
                .await?;
        }
    } else {
        // пользователь должен существовать и при правке без изменений
        st.system.get_user(user).await?.ok_or(ApiError::NotFound)?;
    }
    Ok(Json(settings_json(st, user).await?))
}

/// Пользователь, чьи настройки правит сессия WebUI: свой (у администратора — его
/// пользователь). Токен — 403.
fn own_user(st: &AppState, p: &Principal) -> Result<Id, ApiError> {
    match p {
        Principal::User { user_id, .. } => Ok(*user_id),
        Principal::Admin { .. } => Ok(st.admin_id),
        Principal::Token(_) => Err(ApiError::Forbidden),
    }
}

/// `GET /auth/settings` → `{hooks: {…}}`.
pub async fn get_own(State(st): State<AppState>, p: Principal) -> Result<Json<Value>, ApiError> {
    let user = own_user(&st, &p)?;
    Ok(Json(settings_json(&st, user).await?))
}

/// `PATCH /auth/settings {hooks: {<тип>: bool}}` → `{hooks: {…}}`.
pub async fn patch_own(
    State(st): State<AppState>,
    client: Client,
    p: Principal,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let user = own_user(&st, &p)?;
    apply(&st, &p.actor(), user, &client.ip.to_string(), &body).await
}

async fn existing_user(st: &AppState, id: &str) -> Result<Id, ApiError> {
    let id = parse_id(id)?;
    st.system.get_user(id).await?.ok_or(ApiError::NotFound)?;
    Ok(id)
}

/// `GET /admin/users/{id}/settings` → `{hooks: {…}}`.
pub async fn get_user(
    _: AdminOnly,
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = existing_user(&st, &id).await?;
    Ok(Json(settings_json(&st, user).await?))
}

/// `PATCH /admin/users/{id}/settings {hooks: {<тип>: bool}}` → `{hooks: {…}}`.
pub async fn patch_user(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let user = existing_user(&st, &id).await?;
    apply(&st, "admin", user, &client.ip.to_string(), &body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_default_and_json() {
        let f = HookFlags::default();
        assert!(HOOK_KINDS.iter().all(|k| f.enabled(k)));
        assert_eq!(
            f.to_json(),
            json!({"session_start": true, "prompt": true, "pre_tool": true,
                   "post_tool": true, "checkpoint": true, "subagent_start": true,
                   "file_context": true, "session_journal": true, "recovery": true})
        );
    }

    #[test]
    fn patch_validation() {
        let ok = parse_patch(&Bytes::from_static(br#"{"hooks":{"prompt":false}}"#)).unwrap();
        assert_eq!(ok, vec![("hooks.prompt".to_string(), Value::Bool(false))]);
        let ok = parse_patch(&Bytes::from_static(br#"{"hooks":{"file_context":false}}"#)).unwrap();
        assert_eq!(
            ok,
            vec![("hooks.file_context".to_string(), Value::Bool(false))]
        );
        for bad in [
            r#"{}"#,
            r#"{"hooks":{}}"#,
            r#"{"hooks":{"stop":false}}"#,
            r#"{"hooks":{"prompt":"no"}}"#,
            r#"{"other":1}"#,
        ] {
            assert!(parse_patch(&Bytes::from(bad.to_string())).is_err(), "{bad}");
        }
    }
}
