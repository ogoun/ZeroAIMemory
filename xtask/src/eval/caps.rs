//! Возможности сервера по `GET /api/v1/health` (спецификация M6 §3.3, I8).
//!
//! Базы замера — бинарники тегов `v0.0.5-m4`, `v0.0.9-m5.6`, `v0.0.10-m5.7` и сборка ветки M6.
//! У старого сервера конфигурация с `deny_unknown_fields`: переопределение `ZAM_*` ключа, которого
//! в его версии нет, уронило бы старт. Поэтому xtask узнаёт возможности сервера и шлёт только
//! ключи, которые он знает ([`filter_overrides`]).
//!
//! **Как узнаётся сервер** ([`caps_from`]):
//! - сборка ветки M6 объявляет себя сама: `/health` отдаёт `features` — массив строк
//!   (`zam_server::api::health::FEATURES`), в нём всегда `"m6"`. **Правило для задач M6**: задача,
//!   которая вводит ключ конфигурации M6 или возможность, добавляет в `FEATURES` её строку, а
//!   ключ в [`KEYS`] помечен этой строкой ([`Since::Feature`]): `budget` (§4.2, задача 5),
//!   `snippet` (§4.3, задача 5), `focus` (§6, задача 7b), `digest` (§8, задача 8a),
//!   `hint_thresholds` (§9, задача 9), `kind_boost` (§10), `attach_weight` (§11), `snapshot`
//!   (§8а, задача 17c). Пока строки нет, ключ этой сборке не шлётся (а прогон с ним — не
//!   сравнивается);
//! - `features` нет — выпуск по таблице [`RELEASES`]: версия и коммит (`git`, 12 знаков) должны
//!   совпасть с тегом. `git = unknown` или другой коммит у старой версии — ошибка: бинарник тега
//!   собран не из тега или без `ZAM_GIT_SHA` (из `git archive` внутри рабочей копии `build.rs`
//!   взял бы коммит текущей ветки) — его собирают с `ZAM_GIT_SHA=<12 знаков коммита тега>`.
//!
//! Версия разбирается вручную (`0.0.10-m5.7` → 0, 0, 10, `m5.7`), без `semver`.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

/// Коммит выпуска `v0.0.10-m5.7` (`git rev-parse --short=12`).
pub const RELEASE_M57_GIT: &str = "91363056d3ba";

/// Выпуски, которые можно взять базой: версия и коммит тега (12 знаков).
pub const RELEASES: &[(&str, &str)] = &[
    ("0.0.5-m4", "56f1873c18b9"),
    ("0.0.6-m4.5", "6dee9f90ad8a"),
    ("0.0.7-m5", "f3a247f8bd53"),
    ("0.0.8-m5.5", "075c51fb7696"),
    ("0.0.9-m5.6", "97825a10bc19"),
    ("0.0.10-m5.7", RELEASE_M57_GIT),
];

/// Строка `features` сборки ветки M6.
pub const FEATURE_M6: &str = "m6";

/// Версия сервера `major.minor.patch[-stage]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub stage: String,
}

impl Version {
    fn triple(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.stage.is_empty() {
            write!(f, "-{}", self.stage)?;
        }
        Ok(())
    }
}

/// `0.0.10-m5.7` → `Version`; не три числа — `None`.
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let (nums, stage) = s.split_once('-').unwrap_or((s, ""));
    let mut it = nums.split('.').map(|x| x.parse::<u32>().ok());
    let (major, minor, patch) = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
        stage: stage.to_string(),
    })
}

/// Возможности сервера: признаки и ключи конфигурации, которые можно переопределять.
#[derive(Debug, Clone, Serialize)]
pub struct ServerCaps {
    pub version: Version,
    /// `git` из `/health`.
    pub git: Option<String>,
    /// `features` из `/health` (пусто у выпусков до M6).
    pub features: Vec<String>,
    /// Сборка ветки M6 (`features` содержит `"m6"`).
    pub m6: bool,
    /// Явное фокусное чтение (M6 §6, `features`: `focus`).
    pub focus: bool,
    /// Вид `idea` и подсказки идей (M5.7).
    pub ideas: bool,
    /// Связь `contradicts` (`POST /edges`, M4.5): у B4 её нет — пары корпуса не связываются,
    /// затронутые сценарии исключаются (`info.excluded`).
    pub contradicts: bool,
    /// Вложения-документы (`POST /blobs`, фрагменты `a:<chunk>` в поиске; M5): у B4 их нет —
    /// набор `eval/docs` не грузится, сценарии на вложения исключаются (`info.excluded`).
    pub attachments: bool,
    /// Снимок сессии (M6 §8а, `features`: `snapshot`).
    pub snapshot: bool,
    pub keys: BTreeSet<&'static str>,
}

impl ServerCaps {
    /// Метка сервера в имени отчёта: `0.0.10-m5.7` или `0.0.10-m5.7+m6`.
    pub fn tag(&self) -> String {
        if self.m6 {
            format!("{}+{FEATURE_M6}", self.version)
        } else {
            self.version.to_string()
        }
    }
}

/// С какой версии или возможности сервер знает ключ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// Выпуск (major, minor, patch).
    Version((u32, u32, u32)),
    /// Строка `features` сборки ветки M6.
    Feature(&'static str),
}

use Since::{Feature as F, Version as V};

const M4: Since = V((0, 0, 5));
const M45: Since = V((0, 0, 6));
const M5: Since = V((0, 0, 7));
const M56: Since = V((0, 0, 9));
const M57: Since = V((0, 0, 10));

/// Ключи конфигурации сервера, которые замер может переопределить, и с какой версии или
/// возможности они есть (сверено с `crates/zam-server/src/config.rs` тегов `v0.0.5-m4` …
/// `v0.0.10-m5.7`; ключи M6 — спецификация M6 §14). Ключ вне таблицы — ошибка до старта.
pub const KEYS: &[(&str, Since)] = &[
    ("search.rrf_k", M4),
    ("search.w_vector", M4),
    ("search.w_fts", M4),
    ("search.w_fts_relaxed", M4),
    ("search.w_trigram", M4),
    ("search.recency_half_life_days", M4),
    ("search.max_per_node", M4),
    ("search.boost_gotcha", M4),
    ("search.boost_failure", M4),
    ("search.boost_area", M4),
    ("search.boost_axiom", M4),
    ("search.boost_theorem", M4),
    ("search.boost_hypothesis", M4),
    ("search.graph_weight", M4),
    ("search.w_folder", M4),
    ("search.max_folders", M4),
    ("read.max_items", M4),
    ("read.budget_tokens", M4),
    ("recall.digest_tokens", M4),
    ("recall.prompt_min_score", M4),
    ("recall.timeout_ms", M4),
    ("cache.summary_tokens", M4),
    ("cache.semantic_threshold", M4),
    ("cache.new_sources_threshold", M4),
    ("cache.ttl_days", M4),
    ("cache.max_entries", M4),
    ("cache.cross_session", M4),
    ("index.batch_tokens", M4),
    ("index.yield_ms", M4),
    ("index.rollup_debounce_ms", M4),
    ("index.chunk_target_min", M4),
    ("index.chunk_target_max", M4),
    ("index.chunk_hard_max", M4),
    ("index.chunk_merge_below", M4),
    ("index.chunk_overlap_sentences", M4),
    ("index.chunk_boundary_lambda", M4),
    ("index.chunk_cohesion", M4),
    ("index.chunk_prefix_l0", M4),
    ("index.chunk_heading_path_h4", M4),
    ("search.w_fts_alias", M45),
    ("search.contradicts_order", M45),
    ("recall.recovery_tokens", M45),
    ("recall.brief_tokens", M45),
    ("search.max_per_attachment", M5),
    ("search.attach_prefetch", M5),
    ("code.enabled", M56),
    ("code.max_files", M56),
    ("code.max_symbols_per_file", M56),
    ("code.max_refs_per_file", M56),
    ("code.max_request_bytes", M56),
    ("code.map_tokens", M56),
    ("code.brief_tokens", M56),
    ("code.rank_timeout_ms", M56),
    ("ideas.proactive", M57),
    ("ideas.digest_max", M57),
    ("ideas.prompt_max", M57),
    ("ideas.prompt_top_k", M57),
    ("ideas.defer_days", M57),
    ("search.budget_tokens", F("budget")),
    ("search.brief_min_rel", F("budget")),
    ("recall.snippet_tokens", F("snippet")),
    ("read.focus_leaves", F("focus")),
    ("read.focus_min_tokens", F("focus")),
    ("read.focus_timeout_ms", F("focus")),
    ("recall.digest_entries", F("digest")),
    ("recall.digest_code_map_tokens", F("digest")),
    ("recall.prompt_min_score_fts", F("hint_thresholds")),
    ("search.kind_boost_mode", F("kind_boost")),
    ("search.w_attach", F("attach_weight")),
    ("recall.snapshot_tokens", F("snapshot")),
    ("recall.snapshot_stale_min", F("snapshot")),
    ("recall.snapshot_digest_days", F("snapshot")),
    ("snapshot.max_bytes", F("snapshot")),
    ("snapshot.keep_days", F("snapshot")),
];

/// Возможности по ответу `/health`.
pub fn caps_from(health: &Value) -> Result<ServerCaps> {
    let raw = health["version"]
        .as_str()
        .context("нет version в /health")?;
    let ver = parse_version(raw).with_context(|| format!("версия сервера «{raw}»"))?;
    let git = health["git"].as_str().map(str::to_string);
    let features: Option<Vec<String>> = health["features"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });
    let (m6, features, max_release) = match features {
        Some(f) => {
            if !f.iter().any(|x| x == FEATURE_M6) {
                bail!("сервер {ver}: в features {f:?} нет «{FEATURE_M6}» — неизвестная сборка");
            }
            (true, f, (0, 0, 10))
        }
        None => {
            let Some((_, sha)) = RELEASES.iter().find(|(v, _)| *v == raw) else {
                bail!(
                    "сервер {ver}: версии нет среди выпусков-баз ({}) и features нет — неизвестная \
                     сборка",
                    RELEASES.iter().map(|r| r.0).collect::<Vec<_>>().join(", ")
                );
            };
            let g = git.as_deref().unwrap_or("");
            let same = !g.is_empty() && g.len() >= 7 && (sha.starts_with(g) || g.starts_with(sha));
            if !same {
                bail!(
                    "сервер {ver}: git «{g}» ≠ коммиту тега {sha}. Бинарник базы собирают из тега \
                     с ZAM_GIT_SHA={sha} (иначе git archive внутри рабочей копии даёт коммит \
                     текущей ветки), сборка ветки M6 объявляет себя features"
                );
            }
            (false, Vec::new(), ver.triple())
        }
    };
    let has = |f: &str| features.iter().any(|x| x == f);
    let keys = KEYS
        .iter()
        .filter(|(_, since)| match since {
            Since::Version(v) => *v <= max_release,
            Since::Feature(f) => has(f),
        })
        .map(|(k, _)| *k)
        .collect();
    Ok(ServerCaps {
        ideas: m6 || ver.triple() >= (0, 0, 10),
        contradicts: m6 || ver.triple() >= (0, 0, 6),
        attachments: m6 || ver.triple() >= (0, 0, 7),
        focus: has("focus"),
        snapshot: has("snapshot"),
        version: ver,
        git,
        m6,
        features,
        keys,
    })
}

/// `ZAM_SEARCH__BUDGET_TOKENS` → `search.budget_tokens`; не переопределение раздела — `None`.
pub fn env_to_key(env: &str) -> Option<String> {
    let (section, key) = env.strip_prefix("ZAM_")?.split_once("__")?;
    Some(format!(
        "{}.{}",
        section.to_ascii_lowercase(),
        key.to_ascii_lowercase()
    ))
}

/// Ключ таблицы [`KEYS`] для имени `ZAM_*`.
fn table_key(env: &str) -> Option<&'static str> {
    let k = env_to_key(env)?;
    KEYS.iter().find(|(x, _)| *x == k).map(|(x, _)| *x)
}

/// Все переопределения — ключи таблицы [`KEYS`]; иначе ошибка (опечатка в сетке не должна
/// молча превращаться в точку без изменения).
pub fn check_known(env: &[(String, String)]) -> Result<()> {
    let bad: Vec<&str> = env
        .iter()
        .filter(|(k, _)| table_key(k).is_none())
        .map(|(k, _)| k.as_str())
        .collect();
    if !bad.is_empty() {
        bail!(
            "переопределения вне таблицы ключей замера (xtask/src/eval/caps.rs, KEYS): {} — \
             опечатка или ключ, которого замер не знает",
            bad.join(", ")
        );
    }
    Ok(())
}

/// Переопределения, ключи которых знает сервер (порядок сохраняется); ключ таблицы новее
/// сервера отбрасывается, ключ вне таблицы — ошибка.
pub fn filter_overrides(
    caps: &ServerCaps,
    env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    check_known(env)?;
    Ok(env
        .iter()
        .filter(|(k, _)| table_key(k).is_some_and(|key| caps.keys.contains(key)))
        .cloned()
        .collect())
}

/// Переопределения `ZAM_*`: (имя, значение).
pub type Overrides = Vec<(String, String)>;

/// [`filter_overrides`] и имена отброшенных (в отчёт; плечо с ними к сравнению не допускается).
pub fn split_overrides(
    caps: &ServerCaps,
    env: &[(String, String)],
) -> Result<(Overrides, Vec<String>)> {
    let kept = filter_overrides(caps, env)?;
    let dropped = env
        .iter()
        .filter(|(k, _)| !kept.iter().any(|(x, _)| x == k))
        .map(|(k, _)| k.clone())
        .collect();
    Ok((kept, dropped))
}

/// `GET <base>/api/v1/health` → возможности сервера ([`caps_from`]).
pub fn probe(base: &str) -> Result<ServerCaps> {
    let a = crate::smoke::agent_with(Duration::from_secs(10));
    let mut r = a
        .get(&format!("{base}/api/v1/health"))
        .call()
        .context("GET /api/v1/health")?;
    let st = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    if st != 200 {
        bail!("GET /api/v1/health: HTTP {st}: {text}");
    }
    let v: Value = serde_json::from_str(&text).context("ответ /health — не JSON")?;
    caps_from(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_stage() {
        let v = parse_version("0.0.10-m5.7").unwrap();
        assert_eq!(
            (v.major, v.minor, v.patch, v.stage.as_str()),
            (0, 0, 10, "m5.7")
        );
        assert_eq!(v.to_string(), "0.0.10-m5.7");
        let v = parse_version("0.0.5-m4").unwrap();
        assert_eq!((v.patch, v.stage.as_str()), (5, "m4"));
        let v = parse_version("1.2.3").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.stage.as_str()), (1, 2, 3, ""));
        assert!(parse_version("0.0").is_none());
        assert!(parse_version("0.0.1.2").is_none());
        assert!(parse_version("x.0.1-m1").is_none());
        assert!(parse_version("").is_none());
    }

    fn health(v: &str, git: &str, features: Option<&[&str]>) -> Value {
        let mut h = serde_json::json!({ "status": "ok", "version": v, "git": git });
        if let Some(f) = features {
            h["features"] = serde_json::json!(f);
        }
        h
    }

    /// Старые теги — по версии и коммиту выпуска; сборка ветки M6 объявляет себя `features`;
    /// `git = unknown` и чужой коммит у старой версии — ошибка.
    #[test]
    fn caps_from_health_by_release_or_features() {
        let m57 = caps_from(&health("0.0.10-m5.7", "91363056d3ba", None)).unwrap();
        assert!(m57.ideas && !m57.m6 && !m57.focus && !m57.snapshot);
        let m4 = caps_from(&health("0.0.5-m4", "56f1873c18b9", None)).unwrap();
        assert!(!m4.ideas && m4.keys.contains("search.w_fts"));
        // связь `contradicts` — с M4.5 (у B4 её нет: POST /edges отвергает тип)
        assert!(!m4.contradicts && m57.contradicts);
        let m45 = caps_from(&health("0.0.6-m4.5", "6dee9f90ad8a", None)).unwrap();
        assert!(m45.contradicts && !m45.ideas);
        // вложения — с M5 (у B4 и B4.5 их нет)
        assert!(!m4.attachments && !m45.attachments && m57.attachments);
        let m5 = caps_from(&health("0.0.7-m5", "f3a247f8bd53", None)).unwrap();
        assert!(m5.attachments);
        for git in ["unknown", "535910eba54f", ""] {
            let e = caps_from(&health("0.0.10-m5.7", git, None))
                .unwrap_err()
                .to_string();
            assert!(
                e.contains("ZAM_GIT_SHA") && e.contains("91363056d3ba"),
                "{e}"
            );
        }
        // версии без выпуска в таблице и без features — ошибка
        assert!(caps_from(&health("0.0.4-m3", "x", None)).is_err());
        assert!(caps_from(&health("0.0.11-m6", "x", None)).is_err());
        // сборка M6: git любой, ключи M6 — по возможностям сборки
        let m6 = caps_from(&health("0.0.10-m5.7", "unknown", Some(&["m6"]))).unwrap();
        assert!(m6.m6 && m6.ideas && m6.contradicts && m6.attachments && !m6.focus && !m6.snapshot);
        assert!(m6.keys.contains("ideas.proactive") && !m6.keys.contains("search.budget_tokens"));
        let full = caps_from(&health(
            "0.0.10-m5.7",
            "abc",
            Some(&["m6", "budget", "focus", "snapshot"]),
        ))
        .unwrap();
        assert!(full.focus && full.snapshot);
        assert!(
            full.keys.contains("search.budget_tokens")
                && full.keys.contains("read.focus_min_tokens")
        );
        assert!(!full.keys.contains("search.kind_boost_mode"));
        // features без "m6" — не сборка ветки M6
        assert!(caps_from(&health("0.0.10-m5.7", "abc", Some(&["focus"]))).is_err());
    }

    /// Ключ из таблицы, которого нет в версии сервера, отбрасывается (у старого сервера
    /// `deny_unknown_fields`: он уронил бы старт); ключ вне таблицы — ошибка до старта.
    #[test]
    fn filter_overrides_drops_unknown_keys() {
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let over = [
            env("ZAM_SEARCH__BUDGET_TOKENS", "600"),
            env("ZAM_SEARCH__W_FTS", "0.8"),
            env("ZAM_RECALL__BRIEF_TOKENS", "500"),
            env("ZAM_IDEAS__PROACTIVE", "false"),
        ];
        let m4 = caps_from(&health("0.0.5-m4", "56f1873c18b9", None)).unwrap();
        assert_eq!(
            filter_overrides(&m4, &over).unwrap(),
            [env("ZAM_SEARCH__W_FTS", "0.8")]
        );
        let m57 = caps_from(&health("0.0.10-m5.7", RELEASE_M57_GIT, None)).unwrap();
        let (kept, dropped) = split_overrides(&m57, &over).unwrap();
        assert_eq!(
            kept,
            [
                env("ZAM_SEARCH__W_FTS", "0.8"),
                env("ZAM_RECALL__BRIEF_TOKENS", "500"),
                env("ZAM_IDEAS__PROACTIVE", "false"),
            ]
        );
        assert_eq!(dropped, ["ZAM_SEARCH__BUDGET_TOKENS"]);
        let m6 = caps_from(&health("0.0.10-m5.7", "x", Some(&["m6", "budget"]))).unwrap();
        assert_eq!(filter_overrides(&m6, &over).unwrap().len(), 4);
        // ключ вне таблицы (опечатка, неизвестный раздел) — ошибка, а не молчаливый пропуск
        let bad = [env("ZAM_SEARCH__BUDGET_TOKEN", "600")];
        let e = filter_overrides(&m6, &bad).unwrap_err().to_string();
        assert!(e.contains("ZAM_SEARCH__BUDGET_TOKEN"), "{e}");
        assert!(check_known(&bad).is_err());
        assert!(check_known(&[env("SEARCH", "1")]).is_err());
        assert!(check_known(&over).is_ok());
    }
}
