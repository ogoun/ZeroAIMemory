//! Настройки сервера внутренних работ из WebUI администратора (M7.6 §9.9, решения 171–173).
//!
//! Секция `[works]` конфигурации (файл, окружение) — умолчание; значение, заданное
//! администратором в WebUI («Администрирование → Внутренние работы», `PATCH
//! /admin/works/settings`), сильнее файла. Хранение — строка [`SETTING_KEY`] таблицы `settings`
//! `system.db`, как прочие настройки администратора (`/admin/settings`): `{rev, values: {<ключ
//! [works]>: значение}}`; ключа нет в `values` — действует значение файла. Действующая секция —
//! файл, поверх него `values`, проверка [`WorksSection::validate`] — та же, что у конфигурации
//! (диапазоны — [`crate::config::WORKS_RANGES`]).
//!
//! Держит действующую секцию [`LiveWorks`] процесса; читатели берут снимок
//! [`crate::app::AppState::works`] на одно обращение. Новое значение действует без перезапуска:
//! пределы токенов, доступ, параллельность, срок, пакет, выключатель — со следующего обращения
//! или запуска ([`Applies::NextCall`]; параллельность — [`super::exec::CallSlots::resize`]),
//! `at`, `tz`, `window_min`, `check` — со следующего тика ночного исполнителя
//! ([`Applies::NextNight`]).
//!
//! Строка читается без строгости (после отката на старую версию незнакомый ключ или значение
//! вне диапазона пропускаются с WARN, остальное действует); вход `PATCH` — строгий
//! ([`patched`]).

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::{WORKS_RANGES, WorksSection};

/// Ключ строки настроек сервера внутренних работ в `settings` (`system.db`).
pub const SETTING_KEY: &str = "works.server";

/// Ключи `[works]`, которые администратор меняет в WebUI, — вся секция, в порядке формы.
pub const KEYS: &[&str] = &[
    "enabled",
    "default_mode",
    "model_access",
    "allow_loopback",
    "max_daily_tokens",
    "max_monthly_tokens",
    "max_parallel",
    "call_timeout_s",
    "batch_records",
    "at",
    "tz",
    "window_min",
    "check",
    "check_max_min",
];

/// Когда новое значение ключа начинает действовать (перезапуск не нужен ни одному).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Со следующего обращения к модели или запуска (ручного, ночного, проверки).
    NextCall,
    /// Со следующего планирования ночи (тик ночного исполнителя).
    NextNight,
}

impl Applies {
    pub fn of(key: &str) -> Applies {
        match key {
            "at" | "tz" | "window_min" | "check" => Applies::NextNight,
            _ => Applies::NextCall,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Applies::NextCall => "next_call",
            Applies::NextNight => "next_night",
        }
    }
}

/// Значения ключа-перечисления (`None` — ключ не перечисление).
pub fn options(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "default_mode" => Some(&["off", "agent", "model"]),
        "model_access" => Some(&["any", "lan", "off"]),
        "check" => Some(&["nightly", "off"]),
        _ => None,
    }
}

/// Диапазон числового ключа ([`WORKS_RANGES`]).
pub fn range(key: &str) -> Option<(u64, u64)> {
    WORKS_RANGES
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|&(_, lo, hi)| (lo, hi))
}

/// Хранимая строка: номер правки и значения из WebUI. Читается без `deny_unknown_fields`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    /// Номер правки: растёт на 1 с каждым сохранением, меняющим значения (`PATCH` с другим
    /// номером — 409, набранное в форме не теряется).
    #[serde(default)]
    pub rev: u64,
    /// Значения из WebUI по ключам `[works]`.
    #[serde(default)]
    pub values: Map<String, Value>,
}

impl Stored {
    /// Строка `settings` (JSON); нечитаемая — как без строки, с WARN.
    pub fn from_row(row: Option<&Value>) -> Stored {
        match row {
            None => Stored::default(),
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "настройки сервера внутренних работ (WebUI) не прочитаны: действует файл конфигурации");
                Stored::default()
            }),
        }
    }
}

/// Чем ключ проверяется человеку: тип значения и допустимое.
fn expected(key: &str) -> String {
    if let Some(o) = options(key) {
        return format!("одно из: {}", o.join(", "));
    }
    if let Some((lo, hi)) = range(key) {
        return format!("целое от {lo} до {hi}");
    }
    match key {
        "enabled" | "allow_loopback" => "true или false".into(),
        "at" => "время HH:MM, например 04:30".into(),
        "tz" => "пояс: имя IANA, UTC или ±HH:MM".into(),
        _ => "значение ключа [works]".into(),
    }
}

/// Секция `base` с одним значением `key = value` и её проверка; ошибка — причина человеку
/// (`works.<ключ>: …`). Значение — в каноническом виде (как его хранит секция).
pub fn with_value(
    base: &WorksSection,
    key: &str,
    value: &Value,
) -> Result<(WorksSection, Value), String> {
    if !KEYS.contains(&key) {
        return Err(format!(
            "works.{key}: неизвестный ключ — {}",
            KEYS.join(", ")
        ));
    }
    let mut v = serde_json::to_value(base).map_err(|e| e.to_string())?;
    v[key] = value.clone();
    let s: WorksSection =
        serde_json::from_value(v).map_err(|_| format!("works.{key}: {}", expected(key)))?;
    s.validate().map_err(|e| match e {
        crate::config::ConfigError::Invalid { key, message, .. } => format!("{key}: {message}"),
        other => other.to_string(),
    })?;
    let canon = serde_json::to_value(&s).map_err(|e| e.to_string())?[key].clone();
    Ok((s, canon))
}

/// Действующая секция: файл `file`, поверх него `values` по порядку [`KEYS`]. Незнакомый ключ и
/// значение, не прошедшее проверку, пропускаются — их список (ключ, причина) во втором поле.
pub fn effective(file: &WorksSection, values: &Map<String, Value>) -> (WorksSection, Vec<String>) {
    let mut out = file.clone();
    let mut skipped: Vec<String> = values
        .keys()
        .filter(|k| !KEYS.contains(&k.as_str()))
        .map(|k| format!("works.{k}: неизвестный ключ"))
        .collect();
    for key in KEYS {
        let Some(v) = values.get(*key) else {
            continue;
        };
        match with_value(&out, key, v) {
            Ok((s, _)) => out = s,
            Err(e) => skipped.push(e),
        }
    }
    (out, skipped)
}

/// Итог правки `PATCH`: новые значения и действующая секция.
#[derive(Debug)]
pub struct Patched {
    pub values: Map<String, Value>,
    pub effective: WorksSection,
}

/// Правка `edits` (`ключ → значение`, `null` — вернуть к файлу) поверх сохранённых `values`:
/// строгая — незнакомый ключ, неверный тип или значение вне диапазона — ошибка с ключом. Каждое
/// новое значение проверяется на секции файла (ключи независимы), итог — ещё раз целиком.
pub fn patched(
    file: &WorksSection,
    values: &Map<String, Value>,
    edits: &Map<String, Value>,
) -> Result<Patched, String> {
    let mut next = values.clone();
    for (k, v) in edits {
        if v.is_null() {
            if !KEYS.contains(&k.as_str()) {
                return Err(format!("works.{k}: неизвестный ключ — {}", KEYS.join(", ")));
            }
            next.remove(k);
            continue;
        }
        let (_, canon) = with_value(file, k, v)?;
        next.insert(k.clone(), canon);
    }
    let mut eff = file.clone();
    for key in KEYS {
        if let Some(v) = next.get(*key) {
            eff = with_value(&eff, key, v)?.0;
        }
    }
    // значения, которые нынешняя версия не знает (строка после более новой версии), не держат
    // правку: уходят из строки вместе с ней
    next.retain(|k, _| KEYS.contains(&k.as_str()));
    Ok(Patched {
        values: next,
        effective: eff,
    })
}

/// Значение ключа секции (JSON, как в `[works]`).
pub fn value_of(s: &WorksSection, key: &str) -> Value {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.get(key).cloned())
        .unwrap_or(Value::Null)
}

/// Действующая секция `[works]` процесса: файл и значения из WebUI. Снимок — [`Self::get`].
#[derive(Debug)]
pub struct LiveWorks {
    file: WorksSection,
    state: RwLock<State>,
}

#[derive(Debug)]
struct State {
    effective: Arc<WorksSection>,
    stored: Stored,
}

impl LiveWorks {
    /// Из секции файла и сохранённой строки (непрошедшее проверку — WARN, действует файл).
    pub fn new(file: WorksSection, stored: Stored) -> LiveWorks {
        let (eff, skipped) = effective(&file, &stored.values);
        for e in &skipped {
            tracing::warn!(reason = %e, "настройка сервера внутренних работ из WebUI пропущена: действует файл конфигурации");
        }
        LiveWorks {
            file,
            state: RwLock::new(State {
                effective: Arc::new(eff),
                stored,
            }),
        }
    }

    /// Действующая секция — снимок на одно обращение.
    pub fn get(&self) -> Arc<WorksSection> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .effective
            .clone()
    }

    /// Секция файла конфигурации (умолчание для WebUI).
    pub fn file(&self) -> &WorksSection {
        &self.file
    }

    /// Сохранённая строка (номер правки, значения из WebUI).
    pub fn stored(&self) -> Stored {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .stored
            .clone()
    }

    /// Новые значения после записи в `system.db`. Правка старше действующей (два сохранения
    /// подряд закончились не по порядку) не применяется: `false`.
    pub fn set(&self, stored: Stored, effective: WorksSection) -> bool {
        let mut g = self.state.write().unwrap_or_else(|e| e.into_inner());
        if stored.rev < g.stored.rev {
            return false;
        }
        g.effective = Arc::new(effective);
        g.stored = stored;
        true
    }

    /// `{rev, fields: {<ключ>: {value, file, source, applies, options?, min?, max?}}}` — ответ
    /// `GET /admin/works/settings` и поле `settings` сводки `GET /admin/works`; `source` —
    /// `webui` (задано в WebUI) или `config` (файл конфигурации).
    pub fn to_json(&self) -> Value {
        let (eff, stored) = {
            let g = self.state.read().unwrap_or_else(|e| e.into_inner());
            (g.effective.clone(), g.stored.clone())
        };
        let mut fields = Map::new();
        for key in KEYS {
            let mut f = json!({
                "value": value_of(&eff, key),
                "file": value_of(&self.file, key),
                "source": if stored.values.contains_key(*key) { "webui" } else { "config" },
                "applies": Applies::of(key).as_str(),
            });
            if let Some(o) = options(key) {
                f["options"] = json!(o);
            }
            if let Some((lo, hi)) = range(key) {
                f["min"] = json!(lo);
                f["max"] = json!(hi);
            }
            fields.insert((*key).to_string(), f);
        }
        json!({"rev": stored.rev, "fields": fields})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelAccess;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    /// Каждый ключ секции — в [`KEYS`] (форма WebUI не теряет ключ, добавленный в `[works]`).
    #[test]
    fn keys_cover_section() {
        let v = serde_json::to_value(WorksSection::default()).unwrap();
        let mut all: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let mut keys = KEYS.to_vec();
        all.sort_unstable();
        keys.sort_unstable();
        assert_eq!(all, keys);
    }

    /// Проверка — та же, что у конфигурации: диапазоны, пояс, время, тип; незнакомый ключ —
    /// ошибка с именем; значение — в каноническом виде.
    #[test]
    fn patch_validates_like_config() {
        let file = WorksSection::default();
        let none = Map::new();
        for (k, v, needle) in [
            ("max_parallel", json!(5), "works.max_parallel: от 1 до 4"),
            (
                "max_parallel",
                json!("2"),
                "works.max_parallel: целое от 1 до 4",
            ),
            ("window_min", json!(9), "works.window_min: от 10 до 720"),
            (
                "max_daily_tokens",
                json!(1_000_000_001u64),
                "от 0 до 1000000000",
            ),
            ("tz", json!("Mars/Base"), "works.tz"),
            ("at", json!("4:30"), "works.at"),
            ("model_access", json!("wan"), "any, lan, off"),
            ("enabled", json!(1), "true или false"),
            ("nope", json!(1), "works.nope: неизвестный ключ"),
        ] {
            let e = patched(&file, &none, &obj(json!({ k: v }))).unwrap_err();
            assert!(e.contains(needle), "{k}: {e}");
        }
        let p = patched(
            &file,
            &none,
            &obj(json!({"model_access": "lan", "max_daily_tokens": 500})),
        )
        .unwrap();
        assert_eq!(p.effective.model_access, ModelAccess::Lan);
        assert_eq!(p.effective.max_daily_tokens, 500);
        assert_eq!(
            p.values,
            obj(json!({"model_access": "lan", "max_daily_tokens": 500}))
        );
        // `null` — вернуть к файлу
        let p = patched(&file, &p.values, &obj(json!({"model_access": null}))).unwrap();
        assert_eq!(p.effective.model_access, ModelAccess::Any);
        assert_eq!(p.values, obj(json!({"max_daily_tokens": 500})));
    }

    /// Строка без строгости: незнакомый ключ и значение вне диапазона пропускаются, остальное
    /// действует.
    #[test]
    fn effective_skips_bad_values() {
        let file = WorksSection::default();
        let (eff, skipped) = effective(
            &file,
            &obj(json!({"max_parallel": 9, "batch_records": 20, "future": true})),
        );
        assert_eq!((eff.max_parallel, eff.batch_records), (1, 20));
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        let live = LiveWorks::new(
            file,
            Stored {
                rev: 3,
                values: obj(json!({"batch_records": 20})),
            },
        );
        let j = live.to_json();
        assert_eq!(j["rev"], 3);
        assert_eq!(
            j["fields"]["batch_records"],
            json!({"value": 20, "file": 10, "source": "webui", "applies": "next_call", "min": 1, "max": 50})
        );
        assert_eq!(j["fields"]["model_access"]["source"], "config");
        assert_eq!(
            j["fields"]["model_access"]["options"],
            json!(["any", "lan", "off"])
        );
        assert_eq!(j["fields"]["at"]["applies"], "next_night");
    }
}
