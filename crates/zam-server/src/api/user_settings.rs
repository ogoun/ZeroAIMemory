//! Настройки пользователя на сервере (§10.3 «Настройки», решение контроллера R5, задача 13 M4):
//! переключатели типов подсказок хуков `hooks.<тип>`, с M5.7 (§8) выключатель проактивности
//! идей `ideas.proactive`, с M6.5 (§7) режим идей `ideas.mode`.
//!
//! - Хранение — `system.db`, таблица `user_settings` (миграция 0007): настройка — часть учётной
//!   записи, как пароль и токены, а не память; читается без открытия базы пользователя
//!   **[решение исполнителя, задача 13 M4]**.
//! - Ключи — `hooks.session_start | hooks.prompt | hooks.pre_tool | hooks.post_tool |
//!   hooks.checkpoint` и с M4.5 (§9.6) `hooks.subagent_start | hooks.file_context |
//!   hooks.session_journal | hooks.recovery`, с M5.6 (§10) `hooks.code_map`, значение — `bool`;
//!   нет строки — `true` (новые ключи у старых пользователей включены); `ideas.proactive` —
//!   `bool`, нет строки — `true`, действует вместе с `[ideas] proactive` сервера;
//!   `ideas.mode` — строка `review | propose`, нет строки — `[ideas] default_mode` сервера.
//! - `GET/PATCH /auth/settings` — свои (сессия пользователя; администратор — своего
//!   пользователя); по токену — 403: агент свои подсказки не выключает.
//!   `GET/PATCH /admin/users/{id}/settings` — администратор, любой пользователь.
//! - `PATCH {hooks?: {<тип>: bool, …}, ideas?: {proactive?: bool, mode?: "review"|"propose"}}`:
//!   неизвестный тип или поле, значение не того типа (в том числе `null`) и пустая правка — 400;
//!   строка `ideas.mode` не из `review | propose` — 422. Ответ — все настройки `{hooks: {…},
//!   ideas: {proactive, mode}}`. Аудит `user.settings.update {key, from, to}` на каждое изменение.
//! - Выбор, совпавший с действующим умолчанием, не сохраняется: такой пользователь пойдёт за
//!   новым `[ideas] default_mode`, если администратор его сменит (как `hooks` — за `true`).
//!   Переключение `ideas.mode` обратно на умолчание сервера удаляет сохранённую строку
//!   (финальное ревью M6.5, M3).
//! - Сервер отдаёт их в `/recall(session_start)` полем `hooks`; выключенный тип `/recall`
//!   (`session_start`, `prompt`, `post_tool`) отвечает пустым `text` и показ не учитывает (N4).
//! - Режим идей решения — режим **владельца базы** ([`ideas_mode`]): идеи общего решения лежат в
//!   его базе (M6.5 §7 «Общие решения»).
//! - Кэш процесса [`UserFlagsCache`] (`AppState::user_flags`, M6.6 §5.3): `/recall` (настройки
//!   пользователя запроса и режим идей владельца общего решения) и [`ideas_mode`] читают
//!   настройки через него ([`cached_user_flags`]), `GET`/`PATCH` настроек — из `system.db`.
//!   Точность без TTL: `PATCH` (свой и администратора) после записи увеличивает поколение и
//!   удаляет запись пользователя; значение, прочитанное, пока шла правка, в кэш не попадает.
//!   Других писателей `user_settings` в процессе нет: `zam-server restore` переписывает
//!   `system.db` только при остановленном сервере (лок каталога данных).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::SystemStore;

use crate::app::AppState;
use crate::auth::{AdminOnly, Client, Principal};
use crate::config::IdeasMode;
use crate::error::ApiError;

use super::{parse_body, parse_id};

/// Типы подсказок хуков (порядок — порядок в ответе; M4.5 §9.6 добавил четыре после
/// `checkpoint`, M5.6 §10 — `code_map`: строка карты в дайджесте, раздел `### Код` пакета под
/// задачу и автосинхронизация карты из хуков).
pub const HOOK_KINDS: [&str; 10] = [
    "session_start",
    "prompt",
    "pre_tool",
    "post_tool",
    "checkpoint",
    "subagent_start",
    "file_context",
    "session_journal",
    "recovery",
    "code_map",
];

/// Префикс ключей переключателей в `user_settings`.
const HOOKS_PREFIX: &str = "hooks.";
/// Ключ выключателя проактивности идей (M5.7 §8).
pub const IDEAS_PROACTIVE: &str = "ideas.proactive";
/// Ключ режима идей (M6.5 §7): `review | propose`.
pub const IDEAS_MODE: &str = "ideas.mode";

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

/// Действующие значения всех настроек пользователя (нет строки — `true`, у `ideas.mode` —
/// умолчание сервера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserFlags {
    /// Типы подсказок хуков `hooks.<тип>`.
    pub hooks: HookFlags,
    /// Проактивность идей `ideas.proactive` (действует вместе с `[ideas] proactive`).
    pub ideas_proactive: bool,
    /// Режим идей `ideas.mode` этого пользователя (нет строки — `[ideas] default_mode` сервера).
    /// Для идей чужой (общей) базы действует режим её владельца — [`ideas_mode`]`(st, owner)`.
    pub ideas_mode: IdeasMode,
}

impl UserFlags {
    /// Настройки без строк `user_settings`: всё включено, режим идей — `default_mode` сервера.
    pub fn defaults(default_mode: IdeasMode) -> UserFlags {
        UserFlags {
            hooks: HookFlags::default(),
            ideas_proactive: true,
            ideas_mode: default_mode,
        }
    }
}

/// Все настройки пользователя одним чтением `user_settings`; `default_mode` — `[ideas]
/// default_mode` сервера. Нечитаемое значение — как без строки.
pub async fn user_flags(
    sys: &SystemStore,
    user: Id,
    default_mode: IdeasMode,
) -> Result<UserFlags, zam_store::StoreError> {
    let mut f = UserFlags::defaults(default_mode);
    for (key, value) in sys.user_settings(user).await? {
        if key == IDEAS_MODE {
            if let Some(m) = value.as_str().and_then(IdeasMode::parse) {
                f.ideas_mode = m;
            }
            continue;
        }
        let Some(b) = value.as_bool() else {
            continue;
        };
        if key == IDEAS_PROACTIVE {
            f.ideas_proactive = b;
        } else if let Some(i) = key
            .strip_prefix(HOOKS_PREFIX)
            .and_then(|kind| HOOK_KINDS.iter().position(|k| *k == kind))
        {
            f.hooks.0[i] = b;
        }
    }
    Ok(f)
}

/// Кэш настроек пользователей процесса (M6.6 §5.3, по образцу [`crate::glossary::GlossaryCache`]):
/// [`UserFlags`] по пользователю и счётчик поколения. Читатель запоминает поколение до чтения
/// `system.db` и кладёт значение в кэш, только если поколение не изменилось (`store`);
/// правка настроек — [`Self::invalidate`] после записи. Сверка поколения со вставкой и сдвиг
/// поколения с удалением записи идут под одним замком: устаревшее значение не переживёт правку.
/// Значения — с `[ideas] default_mode` сервера (неизменен до перезапуска). Записи не вытесняются:
/// пользователей единицы, удаления пользователей нет.
#[derive(Default)]
pub struct UserFlagsCache {
    map: Mutex<HashMap<Id, UserFlags>>,
    generation: AtomicU64,
    loads: AtomicUsize,
}

impl UserFlagsCache {
    /// Настройки `user`: из кэша или одним чтением [`user_flags`] (в кэш — если за чтение не было
    /// правки). `default_mode` — `[ideas] default_mode` сервера.
    pub async fn get(
        &self,
        sys: &SystemStore,
        user: Id,
        default_mode: IdeasMode,
    ) -> Result<UserFlags, zam_store::StoreError> {
        if let Some(f) = self.cached(user) {
            return Ok(f);
        }
        let generation = self.generation();
        let f = user_flags(sys, user, default_mode).await?;
        self.loads.fetch_add(1, Ordering::Relaxed);
        self.store(user, f, generation);
        Ok(f)
    }

    /// Правка настроек `user` записана: сдвиг поколения (идущие чтения своё значение не
    /// сохранят) и удаление его записи.
    pub fn invalidate(&self, user: Id) {
        let mut m = self.lock();
        self.generation.fetch_add(1, Ordering::SeqCst);
        m.remove(&user);
    }

    /// Сколько раз настройки читались из `system.db` (проверка кэша в тестах).
    pub fn loads(&self) -> usize {
        self.loads.load(Ordering::Relaxed)
    }

    fn cached(&self, user: Id) -> Option<UserFlags> {
        self.lock().get(&user).copied()
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Значение, прочитанное с поколением `generation`, — в кэш, если поколение то же.
    fn store(&self, user: Id, flags: UserFlags, generation: u64) {
        let mut m = self.lock();
        if self.generation() == generation {
            m.insert(user, flags);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, UserFlags>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Настройки пользователя через кэш процесса ([`UserFlagsCache`], `st.user_flags`).
pub async fn cached_user_flags(
    st: &AppState,
    user: Id,
) -> Result<UserFlags, zam_store::StoreError> {
    st.user_flags
        .get(&st.system, user, st.cfg.ideas.default_mode)
        .await
}

/// Переключатели типов подсказок пользователя ([`user_flags`]).
pub async fn hook_flags(sys: &SystemStore, user: Id) -> Result<HookFlags, zam_store::StoreError> {
    Ok(user_flags(sys, user, IdeasMode::default()).await?.hooks)
}

/// Проактивность идей пользователя (`ideas.proactive`, [`user_flags`]).
pub async fn ideas_proactive(sys: &SystemStore, user: Id) -> Result<bool, ApiError> {
    Ok(user_flags(sys, user, IdeasMode::default())
        .await?
        .ideas_proactive)
}

/// Режим идей базы `owner` (M6.5 §7): настройка `ideas.mode` владельца базы или `[ideas]
/// default_mode` сервера. В общем решении действует режим владельца, а не участника: идеи лежат
/// в его базе. Читается через кэш процесса ([`cached_user_flags`], M6.6 §5.3).
pub async fn ideas_mode(st: &AppState, owner: Id) -> Result<IdeasMode, ApiError> {
    Ok(cached_user_flags(st, owner).await?.ideas_mode)
}

impl UserFlags {
    /// Действующее значение по ключу `user_settings` (JSON, как в строке настройки).
    fn value(&self, key: &str) -> Value {
        match key {
            IDEAS_PROACTIVE => Value::Bool(self.ideas_proactive),
            IDEAS_MODE => json!(self.ideas_mode.as_str()),
            _ => Value::Bool(
                self.hooks
                    .enabled(key.strip_prefix(HOOKS_PREFIX).unwrap_or(key)),
            ),
        }
    }

    fn to_json(self) -> Value {
        json!({
            "hooks": self.hooks.to_json(),
            "ideas": {"proactive": self.ideas_proactive, "mode": self.ideas_mode.as_str()},
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    hooks: Option<Map<String, Value>>,
    ideas: Option<Map<String, Value>>,
}

/// Правки из тела `PATCH`: `(ключ, значение)`.
fn parse_patch(body: &Bytes) -> Result<Vec<(String, Value)>, ApiError> {
    let req: SettingsPatch = parse_body(body)?;
    let hooks = req.hooks.unwrap_or_default();
    let ideas = req.ideas.unwrap_or_default();
    if hooks.is_empty() && ideas.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "нужно поле hooks: {{{}: bool}} или ideas: {{proactive: bool, mode: review|propose}}",
            HOOK_KINDS.join("|")
        )));
    }
    let mut out = Vec::with_capacity(hooks.len() + ideas.len());
    for (k, v) in ideas {
        match k.as_str() {
            "proactive" => {
                // null — не «оставить как есть», а ошибка: значение задаётся явно
                let Value::Bool(b) = v else {
                    return Err(ApiError::BadRequest(
                        "ideas.proactive: true или false".into(),
                    ));
                };
                out.push((IDEAS_PROACTIVE.to_string(), Value::Bool(b)));
            }
            // не строка (в том числе null) — 400, как тип `proactive`; строка не из режимов — 422
            // (значение по смыслу, как у полей операций идей)
            "mode" => {
                let Value::String(s) = v else {
                    return Err(ApiError::BadRequest(
                        "ideas.mode: строка review или propose".into(),
                    ));
                };
                let Some(m) = IdeasMode::parse(s.trim()) else {
                    return Err(ApiError::Unprocessable(
                        "ideas.mode: review — агент сам разбирает новые идеи, propose — \
                         предлагает их человеку вопросом"
                            .into(),
                    ));
                };
                out.push((IDEAS_MODE.to_string(), json!(m.as_str())));
            }
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "ideas: неизвестное поле «{k}» — proactive или mode"
                )));
            }
        }
    }
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
    Ok(user_flags(&st.system, user, st.cfg.ideas.default_mode)
        .await?
        .to_json())
}

/// Сохраняет правки и пишет аудит каждого изменения. Правка сравнивается с действующим
/// значением (нет строки — `true`, у `ideas.mode` — умолчание сервера): совпало — ни записи, ни
/// аудита (fix round 1, I-2). `ideas.mode`, равный умолчанию сервера, удаляет строку настройки
/// (аудит — как у правки). После записи — [`UserFlagsCache::invalidate`] (M6.6 §5.3).
async fn apply(
    st: &AppState,
    actor: &str,
    user: Id,
    ip: &str,
    body: &Bytes,
) -> Result<Json<Value>, ApiError> {
    let edits = parse_patch(body)?;
    let current = user_flags(&st.system, user, st.cfg.ideas.default_mode).await?;
    let edits: Vec<(String, Value)> = edits
        .into_iter()
        .filter(|(key, v)| *v != current.value(key))
        .collect();
    if !edits.is_empty() {
        // `ideas.mode`, равный умолчанию сервера, не хранится (M6.5 §7): правка на него снимает
        // сохранённый выбор, и пользователь снова идёт за `[ideas] default_mode` (финальное ревью
        // M6.5, M3); аудит — с действующими значениями, как у любой правки
        let default_mode = json!(st.cfg.ideas.default_mode.as_str());
        let stored: Vec<(String, Value)> = edits
            .iter()
            .map(|(k, v)| {
                let v = if k == IDEAS_MODE && *v == default_mode {
                    Value::Null
                } else {
                    v.clone()
                };
                (k.clone(), v)
            })
            .collect();
        let changed = st.system.set_user_settings(user, stored).await;
        // и при ошибке: дошла ли правка до базы, неизвестно
        st.user_flags.invalidate(user);
        for (key, _) in changed? {
            let to = edits
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null);
            st.system
                .audit(
                    actor,
                    Some(user),
                    "user.settings.update",
                    Some(&user.to_string()),
                    Some(ip),
                    json!({ "key": key, "from": current.value(&key), "to": to }),
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

/// `GET /auth/settings` → `{hooks: {…}, ideas: {proactive, mode}}`.
pub async fn get_own(State(st): State<AppState>, p: Principal) -> Result<Json<Value>, ApiError> {
    let user = own_user(&st, &p)?;
    Ok(Json(settings_json(&st, user).await?))
}

/// `PATCH /auth/settings {hooks?: {<тип>: bool}, ideas?: {proactive?, mode?}}` → все настройки.
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

/// `GET /admin/users/{id}/settings` → `{hooks: {…}, ideas: {proactive, mode}}`.
pub async fn get_user(
    _: AdminOnly,
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = existing_user(&st, &id).await?;
    Ok(Json(settings_json(&st, user).await?))
}

/// `PATCH /admin/users/{id}/settings` — как `PATCH /auth/settings`, любой пользователь.
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
                   "file_context": true, "session_journal": true, "recovery": true,
                   "code_map": true})
        );
    }

    #[test]
    fn patch_ideas_proactive() {
        let ok = parse_patch(&Bytes::from_static(
            br#"{"ideas":{"proactive":false},"hooks":{"prompt":true}}"#,
        ))
        .unwrap();
        assert_eq!(
            ok,
            vec![
                (IDEAS_PROACTIVE.to_string(), Value::Bool(false)),
                ("hooks.prompt".to_string(), Value::Bool(true)),
            ]
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
            r#"{"ideas":{}}"#,
            r#"{"ideas":{"proactive":1}}"#,
            r#"{"ideas":{"digest":false}}"#,
            r#"{"ideas":{"proactive":null}}"#,
        ] {
            assert!(parse_patch(&Bytes::from(bad.to_string())).is_err(), "{bad}");
        }
    }

    /// M6.5 §7: `ideas.mode` — строка `review | propose` (с пробелами по краям — та же); другая
    /// строка — 422, не строка (в том числе `null`) — 400, как тип `proactive`.
    #[test]
    fn patch_ideas_mode() {
        let mut ok = parse_patch(&Bytes::from_static(
            br#"{"ideas":{"proactive":true,"mode":" propose "}}"#,
        ))
        .unwrap();
        ok.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            ok,
            vec![
                (IDEAS_MODE.to_string(), json!("propose")),
                (IDEAS_PROACTIVE.to_string(), Value::Bool(true)),
            ]
        );
        for bad in [
            r#"{"ideas":{"mode":"auto"}}"#,
            r#"{"ideas":{"mode":"Review"}}"#,
            r#"{"ideas":{"mode":""}}"#,
        ] {
            let e = parse_patch(&Bytes::from(bad.to_string())).unwrap_err();
            assert!(matches!(e, ApiError::Unprocessable(_)), "{bad}: {e:?}");
        }
        for bad in [
            r#"{"ideas":{"mode":true}}"#,
            r#"{"ideas":{"mode":1}}"#,
            r#"{"ideas":{"mode":null}}"#,
            r#"{"ideas":{"mode":["review"]}}"#,
        ] {
            let e = parse_patch(&Bytes::from(bad.to_string())).unwrap_err();
            assert!(matches!(e, ApiError::BadRequest(_)), "{bad}: {e:?}");
        }
        let f = UserFlags::defaults(IdeasMode::Propose);
        assert_eq!(f.value(IDEAS_MODE), json!("propose"));
        assert_eq!(f.value(IDEAS_PROACTIVE), json!(true));
        assert_eq!(f.value("hooks.prompt"), json!(true));
        assert_eq!(
            f.to_json()["ideas"],
            json!({"proactive": true, "mode": "propose"})
        );
    }

    /// M6.6 §5.3: значение, прочитанное до правки настроек (поколение сменилось за чтение), в кэш
    /// не попадает; прочитанное без правки — попадает; правка удаляет запись пользователя, записи
    /// других остаются.
    #[test]
    fn cache_generation_drops_stale_entry() {
        let c = UserFlagsCache::default();
        let (u, other) = (Id::new(), Id::new());
        let stale = UserFlags::defaults(IdeasMode::Review);
        let fresh = UserFlags::defaults(IdeasMode::Propose);
        // чтение началось, затем `PATCH` — устаревшее значение отбрасывается
        let g = c.generation();
        c.invalidate(u);
        c.store(u, stale, g);
        assert_eq!(c.cached(u), None);
        // чтение после правки — в кэше
        let g = c.generation();
        c.store(u, fresh, g);
        c.store(other, stale, g);
        assert_eq!(c.cached(u), Some(fresh));
        // правка `u` удаляет только его запись
        c.invalidate(u);
        assert_eq!(c.cached(u), None);
        assert_eq!(c.cached(other), Some(stale));
    }
}
