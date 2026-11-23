//! Настройки пользователя на сервере (§10.3 «Настройки», решение контроллера R5, задача 13 M4):
//! переключатели типов подсказок хуков `hooks.<тип>`, с M5.7 (§8) выключатель проактивности
//! идей `ideas.proactive`, с M6.5 (§7) режим идей `ideas.mode`, с M7.3 (§6.1 п. 2) настройки
//! исполнителя внутренних работ `works.*` ([`crate::works::settings`]).
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
//! - `PATCH {hooks?: {<тип>: bool, …}, ideas?: {proactive?: bool, mode?: "review"|"propose"},
//!   works?: {mode?, kinds?: {<вид>: режим | null}, model?: {…} | null, limits?: {…} | null,
//!   check?: "on"|"off"}}`: неизвестный тип или поле, значение не того типа (в том числе `null`
//!   там, где снимать нечего) и пустая правка — 400; строка `ideas.mode` не из `review |
//!   propose` и строка режима или `works.check` не из своих значений — 422; `works.pause` — 400
//!   (паузу пишет сервер); режим `model` без подключения или при `[works] model_access = "off"`,
//!   пределы выше потолков `[works]`, неизвестная запись Vault подключения — 422. Ответ — все
//!   настройки `{hooks: {…}, ideas: {proactive, mode}, works: {…}}`. Аудит `user.settings.update
//!   {key, from, to}` на каждое изменение (у `works.model` — без `body_extra`). `PATCH`, сменивший
//!   действующий режим вида, зовёт снятие его открытых работ
//!   ([`crate::works::queue::on_mode_change`]).
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
//!   Точность без TTL: каждый принятый `PATCH` (свой и администратора, и без изменений) после
//!   записи увеличивает поколение и удаляет запись пользователя; значение, прочитанное, пока шла
//!   правка, в кэш не попадает.
//!   Кроме `PATCH`, `user_settings` в процессе пишет только сервер — паузу исполнителя
//!   `works.pause` ([`crate::works::settings::set_pause`], тот же сброс кэша после записи);
//!   `zam-server restore` переписывает `system.db` только при остановленном сервере (лок каталога
//!   данных).

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
use crate::config::{IdeasMode, ModelAccess, WorksMode, WorksSection};
use crate::error::ApiError;
use crate::works::settings::{
    self as works_settings, BODY_EXTRA_MAX_BYTES, Limits, MAX_TOKENS_RANGE, MODEL_NAME_MAX,
    ModelAuth, ModelConn, TokenField, UserWorks, WORKS_CHECK, WORKS_KIND_PREFIX, WORKS_LIMITS,
    WORKS_MODE, WORKS_MODEL,
};
use zam_store::works::WorkKind;

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
/// `system.db` и кладёт значение в кэш, только если поколение не изменилось (`store`); каждый
/// `PATCH` настроек — [`Self::invalidate`] (после записи). Сверка поколения со вставкой и сдвиг
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
    /// правки). `default_mode` — `[ideas] default_mode` сервера: его нет в ключе кэша, поэтому
    /// вход снаружи — только [`cached_user_flags`] (умолчание из конфигурации сервера; ревью
    /// задачи 4, M3).
    async fn get(
        &self,
        sys: &SystemStore,
        user: Id,
        default_mode: IdeasMode,
    ) -> Result<UserFlags, zam_store::StoreError> {
        self.load(user, user_flags(sys, user, default_mode)).await
    }

    /// Шаг [`Self::get`] с чтением `read` (тест порядка — ревью задачи 4, M4): из кэша или
    /// чтением, в кэш — если поколение за чтение не сменилось. Будущее `read` ленивое: чтение
    /// базы идёт только в `read.await`, после снимка поколения.
    async fn load(
        &self,
        user: Id,
        read: impl Future<Output = Result<UserFlags, zam_store::StoreError>>,
    ) -> Result<UserFlags, zam_store::StoreError> {
        if let Some(f) = self.cached(user) {
            return Ok(f);
        }
        // поколение — до чтения базы: правка, записанная за чтение, его сдвинет
        let generation = self.generation();
        let f = read.await?;
        self.loads.fetch_add(1, Ordering::Relaxed);
        self.store(user, f, generation);
        Ok(f)
    }

    /// `PATCH` настроек `user` (после записи, если она была): сдвиг поколения (идущие чтения своё
    /// значение не сохранят) и удаление его записи.
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
    works: Option<Map<String, Value>>,
}

/// Правки из тела `PATCH`: `(ключ, значение)`. У `works.model` — вход как в теле (запись Vault по
/// имени): подключение из него собирает [`resolve_model`].
fn parse_patch(body: &Bytes) -> Result<Vec<(String, Value)>, ApiError> {
    let req: SettingsPatch = parse_body(body)?;
    let hooks = req.hooks.unwrap_or_default();
    let ideas = req.ideas.unwrap_or_default();
    let works = req.works.unwrap_or_default();
    if hooks.is_empty() && ideas.is_empty() && works.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "нужно поле hooks: {{{}: bool}}, ideas: {{proactive: bool, mode: review|propose}} или \
             works: {{mode, kinds, model, limits, check}}",
            HOOK_KINDS.join("|")
        )));
    }
    let mut out = Vec::with_capacity(hooks.len() + ideas.len() + works.len());
    parse_works(works, &mut out)?;
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

/// Имена видов работ для текстов ошибок: `probes | rules`.
fn kind_names() -> String {
    WorkKind::ALL
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Режим из значения поля `field`: не строка — 400, строка не из `off | agent | model` — 422.
fn parse_mode(field: &str, v: &Value) -> Result<WorksMode, ApiError> {
    let Value::String(s) = v else {
        return Err(ApiError::BadRequest(format!(
            "{field}: строка off, agent или model"
        )));
    };
    WorksMode::parse(s.trim()).ok_or_else(|| {
        ApiError::Unprocessable(format!(
            "{field}: off — работы нет, agent — работу делает агент в своей сессии, model — \
             подключённая модель (works.model)"
        ))
    })
}

/// Правки `works` (M7.3 §6.1 п. 2) — в `out` ключами `user_settings`. Значение `works.model` — вход
/// без проверки (её делает [`resolve_model`]); `works.pause` — 400: паузу пишет сервер.
fn parse_works(works: Map<String, Value>, out: &mut Vec<(String, Value)>) -> Result<(), ApiError> {
    for (k, v) in works {
        match k.as_str() {
            "mode" => {
                let m = parse_mode("works.mode", &v)?;
                out.push((WORKS_MODE.to_string(), json!(m.as_str())));
            }
            "kinds" => {
                let Value::Object(kinds) = v else {
                    return Err(ApiError::BadRequest(format!(
                        "works.kinds: объект {{<вид>: режим или null}}, вид — {}",
                        kind_names()
                    )));
                };
                if kinds.is_empty() {
                    return Err(ApiError::BadRequest(
                        "works.kinds: пустой объект — укажите вид".into(),
                    ));
                }
                for (name, mv) in kinds {
                    let Some(kind) = WorkKind::parse(&name) else {
                        return Err(ApiError::BadRequest(format!(
                            "works.kinds: неизвестный вид «{name}» — {}",
                            kind_names()
                        )));
                    };
                    let key = format!("{WORKS_KIND_PREFIX}{}", kind.as_str());
                    // null — снять переопределение вида: он снова идёт за общим режимом
                    let value = match mv {
                        Value::Null => Value::Null,
                        other => {
                            json!(parse_mode(&format!("works.kinds.{name}"), &other)?.as_str())
                        }
                    };
                    out.push((key, value));
                }
            }
            "model" => match v {
                Value::Null | Value::Object(_) => out.push((WORKS_MODEL.to_string(), v)),
                _ => {
                    return Err(ApiError::BadRequest(
                        "works.model: объект подключения {protocol, vault, model, …} или null"
                            .into(),
                    ));
                }
            },
            "limits" => {
                let value = match v {
                    Value::Null => Value::Null,
                    other => {
                        let l: Limits = serde_json::from_value(other).map_err(|e| {
                            ApiError::BadRequest(format!(
                                "works.limits: {{daily_tokens, monthly_tokens}} — целые ≥ 0 \
                                 или null: {e}"
                            ))
                        })?;
                        json!(l)
                    }
                };
                out.push((WORKS_LIMITS.to_string(), value));
            }
            "check" => {
                let Value::String(s) = v else {
                    return Err(ApiError::BadRequest(
                        "works.check: строка on или off".into(),
                    ));
                };
                let s = s.trim();
                if s != "on" && s != "off" {
                    return Err(ApiError::Unprocessable(
                        "works.check: on — ночная проверка базы идёт, off — только ручной прогон"
                            .into(),
                    ));
                }
                out.push((WORKS_CHECK.to_string(), json!(s)));
            }
            "pause" => {
                return Err(ApiError::BadRequest(
                    "works.pause пишет только сервер (пауза исполнителя): её снимает сохранение \
                     подключения works.model или успешная проверка подключения"
                        .into(),
                ));
            }
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "works: неизвестное поле «{k}» — mode, kinds, model, limits или check"
                )));
            }
        }
    }
    Ok(())
}

/// Вход `works.model` (§6.2 п. 1): запись личного Vault — имя или `v:<id>`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelInput {
    #[serde(default = "protocol_default")]
    protocol: String,
    vault: String,
    model: String,
    #[serde(default)]
    auth: ModelAuth,
    #[serde(default = "max_tokens_default")]
    max_tokens: u32,
    #[serde(default)]
    token_field: TokenField,
    #[serde(default)]
    body_extra: Option<Value>,
}

fn protocol_default() -> String {
    "openai".into()
}

fn max_tokens_default() -> u32 {
    works_settings::MAX_TOKENS_DEFAULT
}

/// Подключение `works.model` из входа: форма, пределы полей и запись личного Vault
/// пользователя `user` по имени или `v:<id>` (хранится id). Проверку адреса записи, класс сети и
/// закрепление адреса добавляет клиент модели (задача 2 плана).
async fn resolve_model(st: &AppState, user: Id, raw: Value) -> Result<Value, ApiError> {
    let input: ModelInput = serde_json::from_value(raw)
        .map_err(|e| ApiError::BadRequest(format!("works.model: {e}")))?;
    let model = input.model.trim().to_string();
    if model.is_empty() || model.chars().count() > MODEL_NAME_MAX {
        return Err(ApiError::Unprocessable(format!(
            "works.model.model: имя модели у поставщика, 1..={MODEL_NAME_MAX} символов"
        )));
    }
    if !MAX_TOKENS_RANGE.contains(&input.max_tokens) {
        return Err(ApiError::Unprocessable(format!(
            "works.model.max_tokens: от {} до {}",
            MAX_TOKENS_RANGE.start(),
            MAX_TOKENS_RANGE.end()
        )));
    }
    let body_extra = match input.body_extra {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => {
            return Err(ApiError::Unprocessable(
                "works.model.body_extra: JSON-объект, сливается в тело запроса".into(),
            ));
        }
    };
    if Value::Object(body_extra.clone()).to_string().len() > BODY_EXTRA_MAX_BYTES {
        return Err(ApiError::Unprocessable(format!(
            "works.model.body_extra: не больше {BODY_EXTRA_MAX_BYTES} байт"
        )));
    }
    let wanted = input.vault.trim();
    let by_id = wanted.strip_prefix("v:").and_then(|s| s.parse::<Id>().ok());
    let db = st.users.get(user).await?;
    let items = zam_store::vault_list(&db, &zam_store::VaultScope::Knowledge, None).await?;
    let Some(item) = items
        .iter()
        .find(|m| by_id.map_or(m.title == wanted, |id| m.id == id))
    else {
        return Err(ApiError::Unprocessable(format!(
            "works.model.vault: в личном Vault нет записи «{wanted}» — имя записи или v:<id>"
        )));
    };
    let conn = ModelConn {
        protocol: input.protocol.trim().to_string(),
        vault: item.id,
        model,
        auth: input.auth,
        max_tokens: input.max_tokens,
        token_field: input.token_field,
        body_extra,
        pinned: None,
    };
    Ok(json!(conn))
}

/// `works.limits` правки — не больше потолков сервера (`[works] max_daily_tokens`,
/// `max_monthly_tokens`): больше — 422 с потолком в `detail`.
fn check_limits(edits: &[(String, Value)], s: &WorksSection) -> Result<(), ApiError> {
    let Some(l) = edits
        .iter()
        .find(|(k, _)| k == WORKS_LIMITS)
        .and_then(|(_, v)| serde_json::from_value::<Limits>(v.clone()).ok())
    else {
        return Ok(());
    };
    if l.daily_tokens > s.max_daily_tokens {
        return Err(ApiError::Unprocessable(format!(
            "works.limits.daily_tokens: не больше потолка сервера {} ([works] max_daily_tokens)",
            s.max_daily_tokens
        )));
    }
    if l.monthly_tokens > s.max_monthly_tokens {
        return Err(ApiError::Unprocessable(format!(
            "works.limits.monthly_tokens: не больше потолка сервера {} ([works] \
             max_monthly_tokens)",
            s.max_monthly_tokens
        )));
    }
    Ok(())
}

/// Правка, выбирающая режим `model` (общий или вида), — только при `[works] model_access` не
/// `off` и с подключением после правки (§6.1 п. 2): иначе 422.
fn check_model_mode(
    edits: &[(String, Value)],
    current: &UserWorks,
    s: &WorksSection,
) -> Result<(), ApiError> {
    let sets_model = edits.iter().any(|(k, v)| {
        (k == WORKS_MODE || k.starts_with(WORKS_KIND_PREFIX))
            && v.as_str() == Some(WorksMode::Model.as_str())
    });
    if !sets_model {
        return Ok(());
    }
    if s.model_access == ModelAccess::Off {
        return Err(ApiError::Unprocessable(
            "works.mode: режим model выключен на сервере ([works] model_access = \"off\")".into(),
        ));
    }
    if current.with_edits(edits).model.is_none() {
        return Err(ApiError::Unprocessable(
            "works.mode: режим model требует подключения works.model (запись личного Vault и \
             модель)"
                .into(),
        ));
    }
    Ok(())
}

/// Значение для аудита: у `works.model` — без `body_extra` (§6.1 п. 2: там особенности
/// поставщика, им не место в журнале аудита).
fn audit_value(key: &str, mut v: Value) -> Value {
    if key == WORKS_MODEL
        && let Some(o) = v.as_object_mut()
    {
        o.remove("body_extra");
    }
    v
}

async fn settings_json(st: &AppState, user: Id) -> Result<Value, ApiError> {
    let mut v = user_flags(&st.system, user, st.cfg.ideas.default_mode)
        .await?
        .to_json();
    v["works"] = works_settings::load(&st.system, user)
        .await?
        .to_json(&st.cfg.works);
    Ok(v)
}

/// Сохраняет правки и пишет аудит каждого изменения. Правка сравнивается с действующим
/// значением (нет строки — `true`, у `ideas.mode` и `works.mode` — умолчание сервера, у
/// `works.check` — `on`, у прочих `works.*` — `null`): совпало — ни записи, ни аудита (fix round
/// 1, I-2). `ideas.mode` и `works.mode`, равные умолчанию сервера, и `works.check = on` удаляют
/// строку настройки (аудит — как у правки). Кэш — [`UserFlagsCache::invalidate`] (M6.6 §5.3):
/// после записи, а у правки без изменений — после проверки пользователя. Правка, сменившая
/// действующий режим вида работ, — [`crate::works::queue::on_mode_change`] со списком видов.
async fn apply(
    st: &AppState,
    actor: &str,
    user: Id,
    ip: &str,
    body: &Bytes,
) -> Result<Json<Value>, ApiError> {
    let mut edits = parse_patch(body)?;
    let current = user_flags(&st.system, user, st.cfg.ideas.default_mode).await?;
    let works_cur = works_settings::load(&st.system, user).await?;
    let works_cfg = &st.cfg.works;
    for (k, v) in edits.iter_mut() {
        if k == WORKS_MODEL && !v.is_null() {
            *v = resolve_model(st, user, std::mem::take(v)).await?;
        }
    }
    check_limits(&edits, works_cfg)?;
    check_model_mode(&edits, &works_cur, works_cfg)?;
    let value_of = |key: &str| {
        if key.starts_with("works.") {
            works_cur.value(key, works_cfg)
        } else {
            current.value(key)
        }
    };
    let edits: Vec<(String, Value)> = edits
        .into_iter()
        .filter(|(key, v)| *v != value_of(key))
        .collect();
    let works_touched = edits.iter().any(|(k, _)| k.starts_with("works."));
    if !edits.is_empty() {
        // `ideas.mode` и `works.mode`, равные умолчанию сервера, и `works.check = on` не хранятся
        // (M6.5 §7, M7.3 §6.1 п. 2): правка на них снимает сохранённый выбор, и пользователь
        // снова идёт за умолчанием (финальное ревью M6.5, M3); аудит — с действующими
        // значениями, как у любой правки
        let ideas_default = json!(st.cfg.ideas.default_mode.as_str());
        let works_default = json!(works_cfg.default_mode.as_str());
        let stored: Vec<(String, Value)> = edits
            .iter()
            .map(|(k, v)| {
                let is_default = (k == IDEAS_MODE && *v == ideas_default)
                    || (k == WORKS_MODE && *v == works_default)
                    || (k == WORKS_CHECK && v.as_str() == Some("on"));
                let v = if is_default { Value::Null } else { v.clone() };
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
                    json!({
                        "key": key,
                        "from": audit_value(&key, value_of(&key)),
                        "to": audit_value(&key, to),
                    }),
                )
                .await?;
        }
        if works_touched {
            notify_mode_change(st, user, &works_cur).await?;
        }
    } else {
        // пользователь должен существовать и при правке без изменений
        st.system.get_user(user).await?.ok_or(ApiError::NotFound)?;
        // кэш сбрасывает и `PATCH` без изменений (решение координатора, ревью задачи 4, M2):
        // правка настроек — и способ самолечения кэша
        st.user_flags.invalidate(user);
    }
    Ok(Json(settings_json(st, user).await?))
}

/// После записи `works.*`: виды, у которых сменился действующий режим ([`UserWorks::effective`]
/// до правки и по записанному), — [`crate::works::queue::on_mode_change`] (снятие их открытых
/// работ, §6.1 п. 2).
async fn notify_mode_change(st: &AppState, user: Id, before: &UserWorks) -> Result<(), ApiError> {
    let after = works_settings::load(&st.system, user).await?;
    let cfg = &st.cfg.works;
    let kinds: Vec<WorkKind> = before
        .effective(cfg)
        .into_iter()
        .zip(after.effective(cfg))
        .filter(|((_, was, _), (_, now, _))| was != now)
        .map(|((k, _, _), _)| k)
        .collect();
    if kinds.is_empty() {
        return Ok(());
    }
    let ctx = crate::works::NightCtx::new(st.clone());
    crate::works::queue::on_mode_change(&ctx, user, &kinds).await?;
    Ok(())
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

/// `GET /auth/settings` → `{hooks: {…}, ideas: {proactive, mode}, works: {…}}`.
pub async fn get_own(State(st): State<AppState>, p: Principal) -> Result<Json<Value>, ApiError> {
    let user = own_user(&st, &p)?;
    Ok(Json(settings_json(&st, user).await?))
}

/// `PATCH /auth/settings {hooks?: {<тип>: bool}, ideas?: {proactive?, mode?}, works?: {…}}` → все
/// настройки.
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

/// `GET /admin/users/{id}/settings` → `{hooks: {…}, ideas: {proactive, mode}, works: {…}}`.
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

    /// Ревью задачи 4, M4: `get` снимает поколение до чтения базы. Правка, записанная за чтение
    /// (`invalidate` внутри `read`), отбрасывает прочитанное — значение отдаётся, но в кэш не
    /// попадает; снимок после чтения сохранил бы устаревшее значение. Чтение без правки — в кэш,
    /// повтор — без чтения.
    #[tokio::test]
    async fn cache_get_snapshots_generation_before_read() {
        let c = UserFlagsCache::default();
        let u = Id::new();
        let stale = UserFlags::defaults(IdeasMode::Review);
        let fresh = UserFlags::defaults(IdeasMode::Propose);
        let got = c
            .load(u, async {
                c.invalidate(u);
                Ok(stale)
            })
            .await
            .unwrap();
        assert_eq!(got, stale);
        assert_eq!(c.cached(u), None, "прочитанное за правку — не в кэше");
        assert_eq!(c.loads(), 1);
        let got = c.load(u, async { Ok(fresh) }).await.unwrap();
        assert_eq!((got, c.cached(u)), (fresh, Some(fresh)));
        let got = c
            .load(u, async {
                panic!("повтор — из кэша, без чтения")
            })
            .await
            .unwrap();
        assert_eq!((got, c.loads()), (fresh, 2));
    }
}
