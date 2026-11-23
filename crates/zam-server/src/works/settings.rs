//! Настройки исполнителя внутренних работ пользователя `works.*` (M7.3 §6.1 п. 2, §6.2 пп. 1, 5).
//!
//! Хранятся в `user_settings` (`system.db`) рядом с `hooks.*` и `ideas.*`; правит их
//! `PATCH /auth/settings {works: {…}}` ([`crate::api::user_settings`]) — сессия пользователя или
//! администратор, по токену — 403:
//! - [`WORKS_MODE`] — `off | agent | model`; нет строки — `[works] default_mode` (выбор, равный
//!   умолчанию сервера, не хранится);
//! - [`WORKS_KIND_PREFIX`]`<вид>` — переопределение режима вида, `null` — снять;
//! - [`WORKS_MODEL`] — подключение [`ModelConn`] (запись личного Vault по имени или `v:<id>`,
//!   хранится id) или `null`; проверку адреса записи и закрепление [`ModelConn::pinned`]
//!   добавляет клиент модели (задача 2 плана);
//! - [`WORKS_LIMITS`] — `{daily_tokens, monthly_tokens}`, не больше потолков `[works]`;
//! - [`WORKS_CHECK`] — `on | off` (нет строки — `on`): ночная проверка базы пользователя;
//! - [`WORKS_PAUSE`] — пауза исполнителя: пишет **только сервер** ([`set_pause`]), `PATCH` — 400.
//!
//! Действующий режим вида — чистая функция [`effective_mode`].

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::works::WorkKind;
use zam_store::{StoreError, SystemStore};

use crate::app::AppState;
use crate::config::{ModelAccess, WorksMode, WorksSection};

/// Общий режим исполнителя пользователя: `off | agent | model`.
pub const WORKS_MODE: &str = "works.mode";
/// Префикс переопределения режима вида: `works.kind.probes`, `works.kind.rules`.
pub const WORKS_KIND_PREFIX: &str = "works.kind.";
/// Подключённая модель ([`ModelConn`]).
pub const WORKS_MODEL: &str = "works.model";
/// Пределы токенов пользователя ([`Limits`]).
pub const WORKS_LIMITS: &str = "works.limits";
/// Ночная проверка базы пользователя: `on | off`.
pub const WORKS_CHECK: &str = "works.check";
/// Пауза исполнителя ([`Pause`]); пишет только сервер.
pub const WORKS_PAUSE: &str = "works.pause";

/// Причина `off` действующего режима: `[works] enabled = false`.
pub const OFF_DISABLED: &str = "disabled";
/// Причина `off` режима `model`: `[works] model_access = "off"`.
pub const OFF_MODEL_ACCESS: &str = "model_access_off";
/// Причина `off` режима `model`: нет подключения `works.model`.
pub const OFF_NO_MODEL: &str = "no_model";
/// Причина `off` режима `model`: исполнитель на паузе (§6.2 п. 5).
pub const OFF_PAUSED: &str = "paused";

/// Предел ответа модели по умолчанию и диапазон (§6.2 п. 1).
pub const MAX_TOKENS_DEFAULT: u32 = 4000;
pub const MAX_TOKENS_RANGE: std::ops::RangeInclusive<u32> = 256..=32_000;
/// Имя модели у поставщика — символов.
pub const MODEL_NAME_MAX: usize = 200;
/// `body_extra` — JSON-объект не больше стольких байт.
pub const BODY_EXTRA_MAX_BYTES: usize = 4096;

/// Заголовок авторизации запроса к модели.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelAuth {
    /// `Authorization: Bearer <значение записи Vault>`.
    #[default]
    Key,
    /// Без заголовка (локальный сервер без ключа).
    None,
}

/// Поле предела ответа в теле запроса (§6.2 п. 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenField {
    /// `max_completion_tokens` для `api.openai.com`, иначе `max_tokens`.
    #[default]
    Auto,
    MaxTokens,
    MaxCompletionTokens,
}

/// Базовый адрес модели без `userinfo`, `?…` и `#…`: схема, хост, порт, путь. Хранит настройка
/// (закрепление адреса записи Vault, §6.2 п. 1), разбирает клиент модели (задача 2 плана).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseUrl {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Закреплённый при сохранении подключения адрес записи Vault (§6.2 п. 1).
pub type PinnedAddr = BaseUrl;

/// Подключение `works.model` (§6.2 п. 1). `vault` — id записи личного Vault (в JSON — `v:<id>`):
/// адрес — её `data.url` (иначе `data.address`), ключ — её значение.
///
/// Хранимый формат (строка `user_settings`) читается **без** `deny_unknown_fields`, как и
/// [`BaseUrl`] и [`Limits`]: строку, которую записала более новая версия с лишним полем, сервер
/// после отката читает, а не теряет подключение. Строгость — на входе `PATCH`
/// (`api::user_settings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConn {
    /// Протокол; в M7.3 — `openai` (OpenAI-совместимый Chat Completions).
    pub protocol: String,
    #[serde(with = "vault_ref")]
    pub vault: Id,
    /// Имя модели у поставщика (1..=200 символов).
    pub model: String,
    #[serde(default)]
    pub auth: ModelAuth,
    /// Предел ответа (256..=32000).
    #[serde(default = "max_tokens_default")]
    pub max_tokens: u32,
    #[serde(default)]
    pub token_field: TokenField,
    /// Слияние в тело запроса верхним уровнем (до 4 КиБ).
    #[serde(default)]
    pub body_extra: Map<String, Value>,
    /// Закреплённый адрес записи Vault (заполняет клиент модели, задача 2 плана).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<PinnedAddr>,
}

fn max_tokens_default() -> u32 {
    MAX_TOKENS_DEFAULT
}

/// `vault` подключения в JSON — `v:<id>` (как ссылка на запись Vault в выдаче агенту); при
/// чтении принимается и id без буквы.
mod vault_ref {
    use serde::{Deserialize, Deserializer, Serializer};
    use zam_core::Id;

    pub fn serialize<S: Serializer>(id: &Id, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("v:{id}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Id, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.strip_prefix("v:")
            .unwrap_or(&s)
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Пределы токенов модели пользователя на сутки и месяц UTC (вход + выход).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub daily_tokens: u64,
    pub monthly_tokens: u64,
}

/// Пауза исполнителя (§6.2 п. 5): класс ошибки и с какого момента; у `vault_changed` — хосты
/// «был → стал» (подтверждает только человек).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    pub class: String,
    #[serde(serialize_with = "zam_store::time::ser_ts", deserialize_with = "de_ts")]
    pub since: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

fn de_ts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Timestamp, D::Error> {
    let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
    zam_store::time::parse_ts(&s).map_err(serde::de::Error::custom)
}

/// Настройки исполнителя пользователя (нет строки — умолчание).
#[derive(Debug, Clone, PartialEq)]
pub struct UserWorks {
    /// Общий режим `works.mode`; `None` — `[works] default_mode`.
    pub mode: Option<WorksMode>,
    /// Переопределения режима по видам `works.kind.<вид>`.
    pub kinds: BTreeMap<WorkKind, WorksMode>,
    pub model: Option<ModelConn>,
    pub limits: Option<Limits>,
    /// Ночная проверка базы пользователя (`works.check`; нет строки — `true`).
    pub check: bool,
    pub pause: Option<Pause>,
}

impl Default for UserWorks {
    fn default() -> Self {
        UserWorks {
            mode: None,
            kinds: BTreeMap::new(),
            model: None,
            limits: None,
            check: true,
            pause: None,
        }
    }
}

impl UserWorks {
    /// Настройки из строк `user_settings` (ключи `works.*`; прочие — мимо). Нечитаемое значение
    /// и неизвестный вид — как без строки.
    pub fn from_rows(rows: &[(String, Value)]) -> UserWorks {
        let mut u = UserWorks::default();
        for (key, value) in rows {
            u.set(key, value);
        }
        u
    }

    /// Значение `value` ключа `key` (как в строке `user_settings`); `null` — снять. Ключ не
    /// `works.*` или нечитаемое значение — без изменений.
    fn set(&mut self, key: &str, value: &Value) {
        let mode = || value.as_str().and_then(WorksMode::parse);
        match key {
            WORKS_MODE => self.mode = mode(),
            WORKS_MODEL => self.model = serde_json::from_value(value.clone()).ok(),
            WORKS_LIMITS => self.limits = serde_json::from_value(value.clone()).ok(),
            WORKS_PAUSE => self.pause = serde_json::from_value(value.clone()).ok(),
            WORKS_CHECK => self.check = value.as_str() != Some("off"),
            _ => {
                let Some(kind) = key
                    .strip_prefix(WORKS_KIND_PREFIX)
                    .and_then(WorkKind::parse)
                else {
                    return;
                };
                match mode() {
                    Some(m) => {
                        self.kinds.insert(kind, m);
                    }
                    None => {
                        self.kinds.remove(&kind);
                    }
                }
            }
        }
    }

    /// Те же настройки с правками `edits` (`(ключ, значение)`; `null` — снять).
    pub fn with_edits(&self, edits: &[(String, Value)]) -> UserWorks {
        let mut u = self.clone();
        for (k, v) in edits {
            u.set(k, v);
        }
        u
    }

    /// Действующее значение ключа `works.*` (JSON, как в строке настройки; нет — `null`): у
    /// `works.mode` — с умолчанием сервера, у `works.check` — `on`/`off`.
    pub fn value(&self, key: &str, s: &WorksSection) -> Value {
        match key {
            WORKS_MODE => json!(self.mode.unwrap_or(s.default_mode).as_str()),
            WORKS_MODEL => self.model.as_ref().map_or(Value::Null, |m| json!(m)),
            WORKS_LIMITS => self.limits.map_or(Value::Null, |l| json!(l)),
            WORKS_PAUSE => self.pause.as_ref().map_or(Value::Null, |p| json!(p)),
            WORKS_CHECK => json!(if self.check { "on" } else { "off" }),
            _ => key
                .strip_prefix(WORKS_KIND_PREFIX)
                .and_then(WorkKind::parse)
                .and_then(|k| self.kinds.get(&k))
                .map_or(Value::Null, |m| json!(m.as_str())),
        }
    }

    /// Действующие режимы всех видов ([`WorkKind::ALL`]).
    pub fn effective(&self, s: &WorksSection) -> Vec<(WorkKind, WorksMode, Option<&'static str>)> {
        WorkKind::ALL
            .iter()
            .map(|k| {
                let (m, why) = effective_mode(*k, self, s);
                (*k, m, why)
            })
            .collect()
    }

    /// Поле `works` ответа настроек: общий режим (с умолчанием сервера), переопределения видов,
    /// подключение, пределы, проверка, пауза и действующий режим каждого вида с причиной `off`.
    pub fn to_json(&self, s: &WorksSection) -> Value {
        let kinds: Map<String, Value> = self
            .kinds
            .iter()
            .map(|(k, m)| (k.as_str().to_string(), json!(m.as_str())))
            .collect();
        let effective: Map<String, Value> = self
            .effective(s)
            .into_iter()
            .map(|(k, m, why)| {
                (
                    k.as_str().to_string(),
                    json!({"mode": m.as_str(), "reason": why}),
                )
            })
            .collect();
        json!({
            "mode": self.value(WORKS_MODE, s),
            "kinds": kinds,
            "model": self.value(WORKS_MODEL, s),
            "limits": self.value(WORKS_LIMITS, s),
            "check": self.value(WORKS_CHECK, s),
            "pause": self.value(WORKS_PAUSE, s),
            "effective": effective,
        })
    }
}

/// Действующий режим вида `kind` (§6.1 п. 2): вид пользователя → общий режим пользователя →
/// `[works] default_mode`; `enabled = false` — `off` у всех; `model` при `model_access = "off"`,
/// без подключения или на паузе исполнителя — `off`. Второе — причина `off` (`None` — режим
/// выбран или `off` выбран явно).
pub fn effective_mode(
    kind: WorkKind,
    u: &UserWorks,
    s: &WorksSection,
) -> (WorksMode, Option<&'static str>) {
    if !s.enabled {
        return (WorksMode::Off, Some(OFF_DISABLED));
    }
    let m = u
        .kinds
        .get(&kind)
        .copied()
        .or(u.mode)
        .unwrap_or(s.default_mode);
    if m != WorksMode::Model {
        return (m, None);
    }
    if s.model_access == ModelAccess::Off {
        return (WorksMode::Off, Some(OFF_MODEL_ACCESS));
    }
    if u.model.is_none() {
        return (WorksMode::Off, Some(OFF_NO_MODEL));
    }
    if u.pause.is_some() {
        return (WorksMode::Off, Some(OFF_PAUSED));
    }
    (WorksMode::Model, None)
}

/// Настройки исполнителя пользователя одним чтением `user_settings`.
pub async fn load(system: &SystemStore, user: Id) -> Result<UserWorks, StoreError> {
    Ok(UserWorks::from_rows(&system.user_settings(user).await?))
}

/// Ставит (`Some`) или снимает (`None`) паузу исполнителя пользователя — единственный писатель
/// `works.pause` (`PATCH` её не принимает). После записи — сброс кэша настроек процесса
/// (`AppState::user_flags`), как у `PATCH`.
pub async fn set_pause(st: &AppState, user: Id, pause: Option<Pause>) -> Result<(), StoreError> {
    let value = pause.map_or(Value::Null, |p| json!(p));
    let written = st
        .system
        .set_user_settings(user, vec![(WORKS_PAUSE.to_string(), value)])
        .await;
    // и при ошибке: дошла ли запись до базы, неизвестно
    st.user_flags.invalidate(user);
    written.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> ModelConn {
        ModelConn {
            protocol: "openai".into(),
            vault: Id::new(),
            model: "qwen".into(),
            auth: ModelAuth::Key,
            max_tokens: MAX_TOKENS_DEFAULT,
            token_field: TokenField::Auto,
            body_extra: Map::new(),
            pinned: None,
        }
    }

    fn pause() -> Pause {
        Pause {
            class: "auth".into(),
            since: "2026-11-23T01:00:00Z".parse().unwrap(),
            from: None,
            to: None,
        }
    }

    /// §6.1 п. 2: вид пользователя → общий режим пользователя → `default_mode`; `enabled =
    /// false` — `off` у всех с причиной; `model` при `model_access = "off"`, без подключения и
    /// на паузе — `off` с причиной; `agent` пауза не трогает.
    #[test]
    fn works_mode_resolution_cases() {
        use WorksMode::{Agent, Model, Off};
        let s = WorksSection::default();
        let mut u = UserWorks::default();
        // нет строк — умолчание сервера
        assert_eq!(effective_mode(WorkKind::Probes, &u, &s), (Off, None));
        let agent_default = WorksSection {
            default_mode: Agent,
            ..s.clone()
        };
        assert_eq!(
            effective_mode(WorkKind::Rules, &u, &agent_default),
            (Agent, None)
        );
        // общий режим пользователя — поверх умолчания
        u.mode = Some(Off);
        assert_eq!(
            effective_mode(WorkKind::Rules, &u, &agent_default),
            (Off, None)
        );
        u.mode = Some(Agent);
        assert_eq!(effective_mode(WorkKind::Rules, &u, &s), (Agent, None));
        // вид пользователя — поверх общего
        u.kinds.insert(WorkKind::Probes, Off);
        assert_eq!(effective_mode(WorkKind::Probes, &u, &s), (Off, None));
        assert_eq!(effective_mode(WorkKind::Rules, &u, &s), (Agent, None));
        // общий выключатель
        let disabled = WorksSection {
            enabled: false,
            ..s.clone()
        };
        for k in WorkKind::ALL {
            assert_eq!(effective_mode(*k, &u, &disabled), (Off, Some(OFF_DISABLED)));
        }
        // `model`: без подключения, при `model_access = off`, на паузе
        u.kinds.insert(WorkKind::Probes, Model);
        assert_eq!(
            effective_mode(WorkKind::Probes, &u, &s),
            (Off, Some(OFF_NO_MODEL))
        );
        u.model = Some(conn());
        assert_eq!(effective_mode(WorkKind::Probes, &u, &s), (Model, None));
        let no_model = WorksSection {
            model_access: ModelAccess::Off,
            ..s.clone()
        };
        assert_eq!(
            effective_mode(WorkKind::Probes, &u, &no_model),
            (Off, Some(OFF_MODEL_ACCESS))
        );
        let lan = WorksSection {
            model_access: ModelAccess::Lan,
            ..s.clone()
        };
        assert_eq!(effective_mode(WorkKind::Probes, &u, &lan), (Model, None));
        u.pause = Some(pause());
        assert_eq!(
            effective_mode(WorkKind::Probes, &u, &s),
            (Off, Some(OFF_PAUSED))
        );
        // пауза — только у модели: агентский вид идёт
        assert_eq!(effective_mode(WorkKind::Rules, &u, &s), (Agent, None));
        // `default_mode = model` без подключения — тоже `off` с причиной
        let v = UserWorks::default();
        let model_default = WorksSection {
            default_mode: Model,
            ..s
        };
        assert_eq!(
            effective_mode(WorkKind::Rules, &v, &model_default),
            (Off, Some(OFF_NO_MODEL))
        );
    }

    /// Строки `user_settings` → [`UserWorks`] и обратно ([`UserWorks::value`]): `vault` — `v:<id>`;
    /// нечитаемые значения и неизвестный вид — как без строки; `works.check` — `on`, пока нет
    /// `off`.
    #[test]
    fn rows_roundtrip_and_garbage() {
        let s = WorksSection::default();
        let c = conn();
        let rows = vec![
            (WORKS_MODE.to_string(), json!("agent")),
            ("works.kind.probes".to_string(), json!("model")),
            ("works.kind.tidy".to_string(), json!("agent")),
            ("works.kind.rules".to_string(), json!("bogus")),
            (WORKS_MODEL.to_string(), json!(c)),
            (
                WORKS_LIMITS.to_string(),
                json!({"daily_tokens": 1, "monthly_tokens": 2}),
            ),
            (WORKS_CHECK.to_string(), json!("off")),
            (WORKS_PAUSE.to_string(), json!(pause())),
            ("hooks.prompt".to_string(), json!(false)),
        ];
        let u = UserWorks::from_rows(&rows);
        assert_eq!(u.mode, Some(WorksMode::Agent));
        assert_eq!(
            u.kinds,
            BTreeMap::from([(WorkKind::Probes, WorksMode::Model)])
        );
        assert_eq!(u.model.as_ref(), Some(&c));
        assert_eq!(
            u.limits,
            Some(Limits {
                daily_tokens: 1,
                monthly_tokens: 2
            })
        );
        assert!(!u.check);
        assert_eq!(u.pause, Some(pause()));
        assert_eq!(
            u.value(WORKS_MODEL, &s)["vault"],
            json!(format!("v:{}", c.vault))
        );
        assert_eq!(u.value("works.kind.probes", &s), json!("model"));
        assert_eq!(u.value("works.kind.rules", &s), Value::Null);
        assert_eq!(
            u.value(WORKS_PAUSE, &s)["since"],
            "2026-11-23T01:00:00.000000Z"
        );
        let garbage = UserWorks::from_rows(&[
            (WORKS_MODE.to_string(), json!(1)),
            (WORKS_MODEL.to_string(), json!({"model": "x"})),
            (WORKS_CHECK.to_string(), json!("on")),
        ]);
        assert_eq!(garbage, UserWorks::default());
        // id записи Vault без буквы читается так же
        let mut raw = json!(c);
        raw["vault"] = json!(c.vault.to_string());
        assert_eq!(serde_json::from_value::<ModelConn>(raw).unwrap(), c);
        // правки поверх: `null` снимает
        let v = u.with_edits(&[
            ("works.kind.probes".to_string(), Value::Null),
            (WORKS_MODE.to_string(), Value::Null),
        ]);
        assert!(v.kinds.is_empty() && v.mode.is_none());
        assert_eq!(v.value(WORKS_MODE, &s), json!("off"));
    }

    /// Ревью r1, Minor 3: хранимые строки, записанные более новой версией с лишними полями
    /// (подключение, закреплённый адрес, пределы, пауза), читаются — подключение не пропадает
    /// после отката; строгость — только у входа `PATCH`.
    #[test]
    fn stored_rows_tolerate_unknown_fields() {
        let mut c = conn();
        c.pinned = Some(BaseUrl {
            scheme: "http".into(),
            host: "192.0.2.10".into(),
            port: 8080,
            path: "/v1".into(),
        });
        let mut raw = json!(c);
        raw["future_field"] = json!({"x": 1});
        raw["pinned"]["ip"] = json!("192.0.2.10");
        let rows = vec![
            (WORKS_MODEL.to_string(), raw),
            (
                WORKS_LIMITS.to_string(),
                json!({"daily_tokens": 1, "monthly_tokens": 2, "weekly_tokens": 3}),
            ),
            (
                WORKS_PAUSE.to_string(),
                json!({"class": "auth", "since": "2026-11-23T01:00:00.000000Z", "note": "x"}),
            ),
        ];
        let u = UserWorks::from_rows(&rows);
        assert_eq!(u.model, Some(c));
        assert_eq!(
            u.limits,
            Some(Limits {
                daily_tokens: 1,
                monthly_tokens: 2
            })
        );
        assert_eq!(u.pause, Some(pause()));
    }
}
