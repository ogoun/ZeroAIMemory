//! `--compare`: парная разность с прежним прогоном eval по общим запросам (задача 16 M3).
//!
//! **Общий запрос** — тот же `id`, вид, `expect` и текст. Текст берётся из прежнего JSON
//! (поле `text` у запроса, с M3) или, у прогонов до M3, из `eval/queries.toml` коммита прежнего
//! прогона (`git show <коммит>:eval/queries.toml`); не нашёлся — сравнение по `id`, виду и
//! `expect`, и это видно в отчёте (`text_checked = false`).
//!
//! **Метрики нового прогона пересчитываются по `expect`**: `also` (дополнительно релевантные
//! цели M3) в сравнении не участвует — разметка прежнего прогона их не знала.
//!
//! **Без новых целей** — та же разность, но из выдачи нового прогона убраны цели, которых не было
//! в корпусе прежнего (insight и заметки вне `git ls-tree <коммит> eval/corpus`): так видно,
//! изменился ли порядок прежних заметок между собой или разность дали новые дистракторы.

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

/// Тексты запросов прежнего прогона: из JSON или из `queries.toml` его коммита.
fn old_texts(repo: &Path, v: &Value) -> Option<HashMap<String, String>> {
    let from_json: HashMap<String, String> = v["queries"]
        .as_array()?
        .iter()
        .filter_map(|q| Some((q["id"].as_str()?.into(), q["text"].as_str()?.into())))
        .collect();
    if !from_json.is_empty() {
        return Some(from_json);
    }
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
}

/// Пути заметок прежнего корпуса (`git ls-tree` коммита прежнего прогона).
fn old_corpus(repo: &Path, v: &Value) -> Option<BTreeSet<String>> {
    let commit = commit_of(v["info"]["git"].as_str()?);
    let out = git(
        repo,
        &["ls-tree", "-r", "--name-only", commit, "--", "eval/corpus"],
    )?;
    let set: BTreeSet<String> = out
        .lines()
        .filter_map(|l| l.strip_prefix("eval/corpus/")?.strip_suffix(".md"))
        .map(String::from)
        .collect();
    (!set.is_empty()).then_some(set)
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
    let texts = old_texts(repo, &v);
    let corpus = old_corpus(repo, &v);
    let mut pairs: Vec<(&QueryRun, &Value)> = Vec::new();
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
        if same_expect && same_text && o["kind"] == q.kind.as_str() {
            pairs.push((q, o));
        }
    }
    let ranked = |q: &QueryRun| q.hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>();
    // новый прогон — по `expect`, без `also`
    let new_m = |q: &QueryRun| score(&ranked(q), &q.expect);
    let new_without_new = |q: &QueryRun, set: &BTreeSet<String>| {
        let kept: Vec<String> = ranked(q)
            .into_iter()
            .filter(|p| !p.starts_with(INSIGHT_REF) && set.contains(p))
            .collect();
        score(&kept, &q.expect)
    };
    let old_m = |o: &Value, k: &str| o["metrics"][k].as_f64().unwrap_or(0.0);
    let all: Vec<&(&QueryRun, &Value)> = pairs.iter().collect();
    let no_e: Vec<&(&QueryRun, &Value)> = pairs
        .iter()
        .filter(|(q, _)| !q.id.starts_with("e-"))
        .collect();
    let diff = |sel: &[&(&QueryRun, &Value)], k: &str, f: &dyn Fn(&QueryRun) -> f64| {
        let a: Vec<f64> = sel.iter().map(|(_, o)| old_m(o, k)).collect();
        let b: Vec<f64> = sel.iter().map(|(q, _)| f(q)).collect();
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

    #[test]
    fn commit_of_strips_dirty_suffix() {
        assert_eq!(commit_of("75b02b4+dirty.7ba8a50d"), "75b02b4");
        assert_eq!(commit_of("a2ae4a7"), "a2ae4a7");
    }
}
