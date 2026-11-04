//! Возможности сервера по версии из `GET /api/v1/health` (спецификация M6 §3.3, I8).
//!
//! Базы замера — бинарники тегов `v0.0.5-m4`, `v0.0.9-m5.6`, `v0.0.10-m5.7` и текущая сборка.
//! У старого сервера конфигурация с `deny_unknown_fields`: переопределение `ZAM_*` ключа, которого
//! в его версии нет, уронило бы старт. Поэтому xtask узнаёт версию (`version`, `git`) и шлёт
//! только ключи из таблицы [`KEYS`] «ключ → версия появления» ([`filter_overrides`]).
//!
//! Версия разбирается вручную (`0.0.10-m5.7` → 0, 0, 10, `m5.7`), без `semver`. Сборка этапа M6
//! до выпуска сообщает ещё `0.0.10-m5.7` (версия меняется в задаче 13c), поэтому `0.0.10-m5.7` с
//! `git`, отличным от коммита выпуска ([`RELEASE_M57_GIT`]), считается сборкой M6 **[решение
//! исполнителя задачи 1a1]**. Бинарник тега, собранный из `git archive` внутри рабочей копии,
//! получил бы `git` текущей ветки — его собирают с `ZAM_GIT_SHA=<коммит тега>` (eval/README.md).

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

/// Коммит выпуска `v0.0.10-m5.7` (`git rev-parse --short=12`).
pub const RELEASE_M57_GIT: &str = "91363056d3ba";

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
    /// `git` из `/health` (`None` — поля нет).
    pub git: Option<String>,
    /// Сборка после выпуска своей версии (этап M6 до `0.0.11-m6`).
    pub next: bool,
    /// Явное фокусное чтение (M6 §6).
    pub focus: bool,
    /// Вид `idea` и подсказки идей (M5.7).
    pub ideas: bool,
    /// Снимок сессии (M6 §8а).
    pub snapshot: bool,
    pub keys: BTreeSet<&'static str>,
}

const M4: (u32, u32, u32) = (0, 0, 5);
const M45: (u32, u32, u32) = (0, 0, 6);
const M5: (u32, u32, u32) = (0, 0, 7);
const M56: (u32, u32, u32) = (0, 0, 9);
const M57: (u32, u32, u32) = (0, 0, 10);
const M6: (u32, u32, u32) = (0, 0, 11);

/// Ключи конфигурации сервера, которые замер может переопределить, и версия их появления
/// (сверено с `crates/zam-server/src/config.rs` тегов `v0.0.5-m4` … `v0.0.10-m5.7`; ключи M6 —
/// спецификация M6 §14). Ключ M6 сборка этапа знает, только когда его ввела своя задача.
pub const KEYS: &[(&str, (u32, u32, u32))] = &[
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
    ("search.budget_tokens", M6),
    ("search.brief_min_rel", M6),
    ("search.kind_boost_mode", M6),
    ("search.w_attach", M6),
    ("read.focus_leaves", M6),
    ("read.focus_min_tokens", M6),
    ("read.focus_timeout_ms", M6),
    ("recall.digest_entries", M6),
    ("recall.digest_code_map_tokens", M6),
    ("recall.snippet_tokens", M6),
    ("recall.prompt_min_score_fts", M6),
    ("recall.snapshot_tokens", M6),
    ("recall.snapshot_stale_min", M6),
    ("recall.snapshot_digest_days", M6),
    ("snapshot.max_bytes", M6),
    ("snapshot.keep_days", M6),
];

/// Возможности версии `v` с `git` из `/health`.
pub fn caps_for(v: &Version, git: Option<&str>) -> ServerCaps {
    let release = |g: &str| {
        !g.is_empty() && (RELEASE_M57_GIT.starts_with(g) || g.starts_with(RELEASE_M57_GIT))
    };
    // сборка этапа M6 до выпуска: 0.0.10-m5.7 не с коммита выпуска
    let next = v.triple() == M57 && !git.is_some_and(release);
    let eff = if next { M6 } else { v.triple() };
    ServerCaps {
        version: v.clone(),
        git: git.map(str::to_string),
        next,
        focus: eff >= M6,
        ideas: eff >= M57,
        snapshot: eff >= M6,
        keys: KEYS
            .iter()
            .filter(|(_, since)| *since <= eff)
            .map(|(k, _)| *k)
            .collect(),
    }
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

/// Переопределения `ZAM_*`, ключи которых есть в версии сервера (порядок сохраняется).
pub fn filter_overrides(caps: &ServerCaps, env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(k, _)| known(caps, k))
        .cloned()
        .collect()
}

fn known(caps: &ServerCaps, env: &str) -> bool {
    env_to_key(env).is_some_and(|key| caps.keys.contains(key.as_str()))
}

/// [`filter_overrides`] и имена отброшенных (в отчёт прогона).
pub fn split_overrides(
    caps: &ServerCaps,
    env: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    let dropped = env
        .iter()
        .filter(|(k, _)| !known(caps, k))
        .map(|(k, _)| k.clone())
        .collect();
    (filter_overrides(caps, env), dropped)
}

/// `GET <base>/api/v1/health` → возможности сервера. Версия старше `0.0.5-m4` не поддерживается.
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
    let raw = v["version"].as_str().context("нет version в /health")?;
    let ver = parse_version(raw).with_context(|| format!("версия сервера «{raw}»"))?;
    if ver.triple() < M4 {
        bail!("сервер {ver}: замер M6 поддерживает версии от 0.0.5-m4");
    }
    Ok(caps_for(&ver, v["git"].as_str()))
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

    /// Ключа нет в версии сервера — переопределение отбрасывается (у старого сервера
    /// `deny_unknown_fields`: неизвестный ключ уронил бы старт).
    #[test]
    fn filter_overrides_drops_unknown_keys() {
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let over = [
            env("ZAM_SEARCH__BUDGET_TOKENS", "600"),
            env("ZAM_SEARCH__W_FTS", "0.8"),
            env("ZAM_RECALL__BRIEF_TOKENS", "500"),
            env("ZAM_IDEAS__PROACTIVE", "false"),
            env("ZAM_NO_SUCH__KEY", "1"),
        ];
        let m4 = caps_for(&parse_version("0.0.5-m4").unwrap(), None);
        assert_eq!(
            filter_overrides(&m4, &over),
            [env("ZAM_SEARCH__W_FTS", "0.8")]
        );
        let m57 = caps_for(
            &parse_version("0.0.10-m5.7").unwrap(),
            Some(RELEASE_M57_GIT),
        );
        assert!(m57.ideas && !m57.focus && !m57.snapshot && !m57.next);
        assert_eq!(
            filter_overrides(&m57, &over),
            [
                env("ZAM_SEARCH__W_FTS", "0.8"),
                env("ZAM_RECALL__BRIEF_TOKENS", "500"),
                env("ZAM_IDEAS__PROACTIVE", "false"),
            ]
        );
        let (_, dropped) = split_overrides(&m57, &over);
        assert_eq!(dropped, ["ZAM_SEARCH__BUDGET_TOKENS", "ZAM_NO_SUCH__KEY"]);
        // сборка после выпуска 0.0.10-m5.7 (другой git) — ключи M6
        let dev = caps_for(&parse_version("0.0.10-m5.7").unwrap(), Some("04f9d85aaaaa"));
        assert!(dev.focus && dev.snapshot && dev.next);
        assert_eq!(filter_overrides(&dev, &over).len(), 4);
        let m6 = caps_for(&parse_version("0.0.11-m6").unwrap(), None);
        assert!(m6.keys.contains("search.budget_tokens") && !m6.next);
    }
}
