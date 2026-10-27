//! `--compare`: парная разность с прежним прогоном eval по общим запросам (задача 16 M3).
//!
//! **Общий запрос** — тот же `id`, вид, `expect` и текст. Тексты прежнего прогона — из его JSON
//! (поле `text`, с M3), из `eval/queries.toml` его коммита или текущие — тот источник, на
//! котором воспроизводится отпечаток запросов прежнего прогона ([`old_texts`]); не нашёлся —
//! сравнение по `id`, виду и `expect`, и это видно в отчёте (`text_checked = false`).
//!
//! **Метрики нового прогона пересчитываются по разметке прежнего**: если у запроса прежнего
//! прогона нет поля `also` (прогоны M2 — разметка их не знала), новый считается по одному
//! `expect`; если есть (прогоны M3 и новее: метрики там уже с `also`), запрос общий только при
//! том же `also`, и новый считается по `expect` и `also` — иначе сравнение M3 → M4 занижало бы
//! новый прогон на запросах с `also` **[решение исполнителя, задача 15 M4]**.
//!
//! **Без новых целей** — та же разность, но из выдачи нового прогона убраны цели, которых не было
//! в корпусе прежнего (заметки вне `git ls-tree <коммит> eval/corpus` и insight вне
//! `eval/insights.toml` того коммита): так видно, изменился ли порядок прежних целей между собой
//! или разность дали новые дистракторы.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use super::corpus::INSIGHT_REF;
use super::metrics::{Estimate, paired_diff, score};
use super::run::{QueryRun, RunResult};

/// Парная разность с прежним прогоном по общим запросам.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// Файл прежнего прогона.
    pub against: String,
    pub against_git: String,
    pub against_queries_fingerprint: String,
    pub against_corpus_fingerprint: String,
    /// Совпадение текста запроса проверено (текст прежнего прогона найден).
    pub text_checked: bool,
    pub n: usize,
    pub d_recall5: Estimate,
    pub d_ndcg10: Estimate,
    /// Без группы `e` (её заметки переехали в пространство решения: префикс контекста
    /// чанков другой).
    pub n_without_e: usize,
    pub d_ndcg10_without_e: Estimate,
    /// Без целей, которых не было в прежнем корпусе (`None` — корпус прежнего коммита не
    /// прочитан).
    pub d_ndcg10_without_new: Option<Estimate>,
    /// Интервал Δ nDCG@10 целиком ниже нуля — регрессия, разбирается.
    pub regression: bool,
}

/// `git <args>` в репозитории; неуспех — `None`.
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Коммит прежнего прогона: `abc1234` или `abc1234+dirty.…` → `abc1234`.
fn commit_of(info_git: &str) -> &str {
    info_git.split('+').next().unwrap_or("")
}

/// Тексты запросов прежнего прогона: первый источник, на котором сходится его отпечаток
/// запросов (`info.queries_fingerprint`), — JSON прогона (поле `text`, с M3), `queries.toml`
/// коммита прогона, текущие запросы. Коммит «грязного» прогона мог содержать другие тексты,
/// поэтому источник без совпадения отпечатка не используется; ни один не сошёлся — `None`.
fn old_texts(repo: &Path, v: &Value, current: &[QueryRun]) -> Option<HashMap<String, String>> {
    let want = v["info"]["queries_fingerprint"].as_str()?;
    let from_json: HashMap<String, String> = v["queries"]
        .as_array()?
        .iter()
        .filter_map(|q| Some((q["id"].as_str()?.into(), q["text"].as_str()?.into())))
        .collect();
    let from_git = || -> Option<HashMap<String, String>> {
        let commit = commit_of(v["info"]["git"].as_str()?);
        let toml_text = git(repo, &["show", &format!("{commit}:eval/queries.toml")])?;
        let t: toml::Value = toml::from_str(&toml_text).ok()?;
        Some(
            t.get("query")?
                .as_array()?
                .iter()
                .filter_map(|q| {
                    Some((
                        q.get("id")?.as_str()?.into(),
                        q.get("text")?.as_str()?.into(),
                    ))
                })
                .collect(),
        )
    };
    let from_current: HashMap<String, String> = current
        .iter()
        .map(|q| (q.id.clone(), q.text.clone()))
        .collect();
    [Some(from_json), from_git(), Some(from_current)]
        .into_iter()
        .flatten()
        .find(|m| !m.is_empty() && old_fingerprint(v, m).as_deref() == Some(want))
}

/// Отпечаток запросов прежнего прогона (как [`super::run::queries_fingerprint`] у запросов без
/// полей M3) при текстах `texts`; текста какого-то запроса нет — `None`.
fn old_fingerprint(v: &Value, texts: &HashMap<String, String>) -> Option<String> {
    let mut s = String::new();
    for q in v["queries"].as_array()? {
        let id = q["id"].as_str()?;
        let expect: Vec<&str> = q["expect"]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        s.push_str(&format!(
            "{id}\0{}\0{}\0{}\0",
            texts.get(id)?,
            expect.join(","),
            q["kind"].as_str()?
        ));
    }
    Some(super::corpus::fnv1a(s.into_bytes()))
}

/// Цели прежнего корпуса: пути заметок (`git ls-tree` коммита прежнего прогона) и
/// `insight:<key>` его `eval/insights.toml` (нет файла — без insight, как в M2).
fn old_corpus(repo: &Path, v: &Value) -> Option<BTreeSet<String>> {
    let commit = commit_of(v["info"]["git"].as_str()?);
    let out = git(
        repo,
        &["ls-tree", "-r", "--name-only", commit, "--", "eval/corpus"],
    )?;
    let mut set: BTreeSet<String> = out
        .lines()
        .filter_map(|l| l.strip_prefix("eval/corpus/")?.strip_suffix(".md"))
        .map(String::from)
        .collect();
    if set.is_empty() {
        return None;
    }
    if let Some(t) = git(repo, &["show", &format!("{commit}:eval/insights.toml")])
        && let Ok(t) = toml::from_str::<toml::Value>(&t)
    {
        set.extend(
            t.get("insight")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter_map(|i| i.get("key")?.as_str())
                .map(|k| format!("{INSIGHT_REF}{k}")),
        );
    }
    Some(set)
}

/// Выдача без новых целей: только цели прежнего корпуса `old` (заметки и insight), порядок
/// прежний.
fn old_targets_only(ranked: Vec<String>, old: &BTreeSet<String>) -> Vec<String> {
    ranked.into_iter().filter(|t| old.contains(t)).collect()
}

/// Релевантные цели нового запроса для сравнения: `expect` и, если прежний запрос знает
/// `also` (поле есть), `also`; `None` — `also` разные (запрос не общий).
fn relevant_for(q: &QueryRun, o: &Value) -> Option<Vec<String>> {
    match o.get("also").and_then(Value::as_array) {
        None => Some(q.expect.clone()),
        Some(a) => {
            let old: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            old.iter()
                .copied()
                .eq(q.also.iter().map(String::as_str))
                .then(|| q.expect.iter().chain(&q.also).cloned().collect())
        }
    }
}

/// Сравнение `r` с прежним результатом `prev` (JSON `eval/results/…`); `repo` — для текстов и
/// корпуса прежнего коммита.
pub fn compare(repo: &Path, prev: &Path, r: &RunResult) -> Result<Comparison> {
    let text = fs::read_to_string(prev).with_context(|| format!("чтение {}", prev.display()))?;
    let v: Value =
        serde_json::from_str(&text).with_context(|| format!("разбор {}", prev.display()))?;
    let old: HashMap<&str, &Value> = v["queries"]
        .as_array()
        .context("в прежнем результате нет queries")?
        .iter()
        .filter_map(|q| q["id"].as_str().map(|id| (id, q)))
        .collect();
    let texts = old_texts(repo, &v, &r.queries);
    let corpus = old_corpus(repo, &v);
    let mut pairs: Vec<(&QueryRun, &Value, Vec<String>)> = Vec::new();
    for q in &r.queries {
        let Some(o) = old.get(q.id.as_str()) else {
            continue;
        };
        let same_expect = o["expect"].as_array().is_some_and(|e| {
            e.iter()
                .filter_map(Value::as_str)
                .eq(q.expect.iter().map(String::as_str))
        });
        let same_text = texts
            .as_ref()
            .is_none_or(|t| t.get(&q.id).is_some_and(|t| *t == q.text));
        if same_expect
            && same_text
            && o["kind"] == q.kind.as_str()
            && let Some(rel) = relevant_for(q, o)
        {
            pairs.push((q, o, rel));
        }
    }
    type Pair<'a> = (&'a QueryRun, &'a Value, Vec<String>);
    let ranked = |q: &QueryRun| q.hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>();
    // новый прогон — по разметке прежнего (см. модуль)
    let new_m = |p: &Pair| score(&ranked(p.0), &p.2);
    let new_without_new =
        |p: &Pair, set: &BTreeSet<String>| score(&old_targets_only(ranked(p.0), set), &p.2);
    let old_m = |o: &Value, k: &str| o["metrics"][k].as_f64().unwrap_or(0.0);
    let all: Vec<&Pair> = pairs.iter().collect();
    let no_e: Vec<&Pair> = pairs.iter().filter(|p| !p.0.id.starts_with("e-")).collect();
    let diff = |sel: &[&Pair], k: &str, f: &dyn Fn(&Pair) -> f64| {
        let a: Vec<f64> = sel.iter().map(|p| old_m(p.1, k)).collect();
        let b: Vec<f64> = sel.iter().map(|p| f(p)).collect();
        paired_diff(&a, &b)
    };
    let d_ndcg10 = diff(&all, "ndcg10", &|q| new_m(q).ndcg10);
    Ok(Comparison {
        against: prev.display().to_string(),
        against_git: v["info"]["git"].as_str().unwrap_or_default().into(),
        against_queries_fingerprint: v["info"]["queries_fingerprint"]
            .as_str()
            .unwrap_or_default()
            .into(),
        against_corpus_fingerprint: v["info"]["corpus_fingerprint"]
            .as_str()
            .unwrap_or_default()
            .into(),
        text_checked: texts.is_some(),
        n: all.len(),
        d_recall5: diff(&all, "recall5", &|q| new_m(q).recall5),
        d_ndcg10,
        n_without_e: no_e.len(),
        d_ndcg10_without_e: diff(&no_e, "ndcg10", &|q| new_m(q).ndcg10),
        d_ndcg10_without_new: corpus
            .as_ref()
            .map(|set| diff(&all, "ndcg10", &|q| new_without_new(q, set).ndcg10)),
        regression: d_ndcg10.hi < 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Отпечаток по текстам воспроизводит отпечаток запросов прогона: по нему выбирается
    /// источник текстов прежнего прогона.
    #[test]
    fn old_fingerprint_matches_queries_fingerprint() {
        let q = crate::eval::corpus::tests_query("a-01", "en/long/x");
        let fp = crate::eval::run::queries_fingerprint(std::slice::from_ref(&q));
        let v = serde_json::json!({
            "info": {"queries_fingerprint": fp},
            "queries": [{"id": "a-01", "expect": ["en/long/x"], "kind": "chunk"}],
        });
        let good: HashMap<String, String> = [("a-01".into(), q.text.clone())].into();
        let bad: HashMap<String, String> = [("a-01".into(), "other".into())].into();
        assert_eq!(old_fingerprint(&v, &good), Some(fp.clone()));
        assert_ne!(old_fingerprint(&v, &bad), Some(fp));
        assert_eq!(old_fingerprint(&v, &HashMap::new()), None);
    }

    /// Прежний запрос без поля `also` (M2) — сравнение по `expect`; с полем (M3+) — общий только
    /// при том же `also`, и считается по `expect` и `also`.
    #[test]
    fn relevant_for_follows_old_labels() {
        let mut q = crate::eval::corpus::tests_query("a-01", "ru/a");
        let m2 = serde_json::json!({"id": "a-01"});
        q.also = vec!["insight:x".into()];
        let run = |q: &crate::eval::corpus::Query| QueryRun {
            id: q.id.clone(),
            text: q.text.clone(),
            kind: q.kind,
            lang: q.lang,
            disjoint: false,
            expect: q.expect.clone(),
            also: q.also.clone(),
            forbid: Vec::new(),
            weaker: Vec::new(),
            long: false,
            fts_only: false,
            metrics: Default::default(),
            cancelled5_history: None,
            reliable_first_no_boost: None,
            leaves: Vec::new(),
            hits: Vec::new(),
            ms: 0.0,
            server_ms: Value::Null,
        };
        let r = run(&q);
        assert_eq!(relevant_for(&r, &m2), Some(vec!["ru/a".to_string()]));
        let m3 = serde_json::json!({"id": "a-01", "also": ["insight:x"]});
        assert_eq!(
            relevant_for(&r, &m3),
            Some(vec!["ru/a".to_string(), "insight:x".to_string()])
        );
        let other = serde_json::json!({"id": "a-01", "also": ["insight:y"]});
        assert_eq!(relevant_for(&r, &other), None);
    }

    /// Цели прежнего корпуса — заметки `eval/corpus` и insight `eval/insights.toml` его коммита
    /// (здесь — HEAD: он есть и в неполном клоне CI).
    #[test]
    fn old_corpus_has_notes_and_insights_of_commit() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let head = git(repo, &["rev-parse", "--short", "HEAD"]).unwrap();
        let v = serde_json::json!({"info": {"git": format!("{}+dirty.0", head.trim())}});
        let set = old_corpus(repo, &v).unwrap();
        assert!(set.contains("ru/git/revert-vs-reset"));
        assert!(set.contains("insight:c01-a"));
        assert!(!set.iter().any(|t| t.ends_with(".md")));
        // неизвестный коммит — нет корпуса
        let bad = serde_json::json!({"info": {"git": "0000000"}});
        assert!(old_corpus(repo, &bad).is_none());
    }

    /// «Без новых целей» оставляет заметки и insight прежнего корпуса и убирает только новые.
    #[test]
    fn without_new_keeps_old_notes_and_insights() {
        let old: BTreeSet<String> = ["ru/a", "insight:k1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ranked = vec![
            "insight:new".to_string(),
            "ru/a".to_string(),
            "ru/new".to_string(),
            "insight:k1".to_string(),
        ];
        assert_eq!(old_targets_only(ranked, &old), ["ru/a", "insight:k1"]);
    }

    #[test]
    fn commit_of_strips_dirty_suffix() {
        assert_eq!(commit_of("75b02b4+dirty.7ba8a50d"), "75b02b4");
        assert_eq!(commit_of("a2ae4a7"), "a2ae4a7");
    }
}
