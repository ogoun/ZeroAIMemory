//! Замер ТС-1 M6 через MCP (`cargo xtask eval --tokens --via mcp`, спецификация M6 §3.1–§3.6).
//!
//! Симулированный агент (без LLM, §3.4) ходит в `POST /mcp` синхронным [`McpClient`]; у каждого
//! пути своя явная сессия (`X-Zam-Session`). Пути сценария, по порядку:
//! - **(в) наивный** — `memory_search` `mode=chunks`, бюджет [`NAIVE_BUDGET`]. Идёт первым: MCP не
//!   передаёт `no_cache`, а после (а) в кэше уже лежала бы выжимка вопроса;
//! - **(а) протокол** — `memory_search` brief → оракул выбирает ожидаемые источники из карточек →
//!   `memory_read` выбранных, бюджет [`READ_BUDGET`] → **дочитывание** один раз на источник, если
//!   нагет не выдан, а у элемента есть пометка пропуска (`…ещё N токенов`, `(budget)`,
//!   `[пропущено: <путь> · ~N t]`; сначала названные секции, иначе L2 источника отдельным
//!   вызовом, бюджет [`READ_BUDGET`]) → `memory_cache_put` выжимки;
//! - **справочная политика без оракула** — карточки 1–3 того же поиска по порядку, чтение и
//!   дочитывание по тем же правилам (своя сессия, без выжимки);
//! - **(б) повтор** — `memory_search` brief другой сессией: выжимка кэша первой.
//!
//! Токены — сумма текстов `result.content` вызовов (токенизатор bge-m3). Полнота — нагеты
//! (§3.2): найдены, если целиком есть в выданном агенту тексте пути. Отчёт хранит ответы
//! целиком. Перед прогоном сверяются отпечатки с `eval/nuggets.lock`; сравнение плеч
//! ([`compare`]) — только на одних отпечатках корпуса и нагетов и одной части разбиения.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Counter, NAIVE_BUDGET, PathRun, READ_BUDGET, Scenario, load_scenarios, mapped_refs,
    read_elements, reread_items, reread_need, summary_of, validate_scenarios_with,
};
use crate::eval::caps;
use crate::eval::corpus::{Corpus, EVAL_SOLUTION, load_insights};
use crate::eval::hints::refs_in_text;
use crate::eval::mcp_client::McpClient;
use crate::eval::metrics::{
    BOOTSTRAP_M6, Estimate, SEED, micro_recall, net_losses, paired_diff_weighted,
    ratio_estimate_with,
};
use crate::eval::nuggets::{self, normalize_md};
use crate::eval::run::{self, RunSpec, Session};
use crate::eval::split::{Part, split};
use crate::eval::{Device, EvalArgs, Mode, Paths, grid};

/// Карточек справочной политики без оракула.
pub const REFERENCE_CARDS: usize = 3;
/// Порог неинфериорности: нижняя граница одностороннего 95 % интервала Δ микро-полноты.
pub const MIN_D_MICRO: f64 = -0.02;
/// Порог чистых потерь.
pub const MAX_NET_LOSSES: i64 = 1;

/// Путь агента с нагетами.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPath {
    pub steps: Vec<super::Step>,
    pub tokens: usize,
    /// Найдено нагетов.
    pub found: usize,
    /// Все нагеты найдены (сценарий без нагетов — все ожидаемые источники прочитаны без
    /// пропусков).
    pub complete: bool,
    /// Прочитанные источники (`expect` или карточки справочной политики).
    pub read: Vec<String>,
    /// Дочитанные источники.
    pub reread: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl AgentPath {
    fn path_run(&self) -> PathRun {
        PathRun {
            steps: self.steps.clone(),
            tokens: self.tokens,
            complete: self.complete,
            note: self.note.clone(),
        }
    }

    fn push(&mut self, call: &str, text: &str, counter: &Counter) {
        let mut p = self.path_run();
        p.push_step(call, text, counter, true);
        self.steps = p.steps;
        self.tokens = p.tokens;
    }

    fn delivered(&self) -> String {
        self.path_run().delivered()
    }
}

/// Итог сценария.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpScenario {
    pub id: String,
    pub kind: String,
    pub split: String,
    pub question: String,
    pub expect: Vec<String>,
    pub nuggets_total: usize,
    /// Токены выжимки агента (вывод агента, не ответ сервера).
    pub summary_tokens: usize,
    pub protocol: AgentPath,
    pub reference: AgentPath,
    pub cached: PathRun,
    pub naive: AgentPath,
}

/// Сводка пути.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathTotals {
    pub tokens_total: usize,
    pub tokens_mean: Option<Estimate>,
    pub micro_recall: f64,
    pub full: usize,
}

/// Сводка прогона.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSummary {
    pub scenarios: usize,
    pub nuggets_total: usize,
    pub protocol: PathTotals,
    pub reference: PathTotals,
    pub cached: PathTotals,
    pub naive: PathTotals,
    pub summary_tokens_total: usize,
    /// `Σ (а) / Σ (в)` с 95 % интервалом ([`BOOTSTRAP_M6`] выборок).
    pub protocol_to_naive: Option<Estimate>,
}

/// Сведения о прогоне (плече).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpInfo {
    pub date: String,
    pub git: String,
    pub label: Option<String>,
    /// Точка сетки (`None` — без сетки).
    pub point: Option<String>,
    pub split: String,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub device: Option<String>,
    pub server_version: String,
    pub server_git: Option<String>,
    /// Сборка этапа M6 до выпуска ([`caps::ServerCaps::next`]).
    pub server_next: bool,
    pub focus: bool,
    pub ideas: bool,
    pub snapshot: bool,
    /// Переопределения, отправленные серверу, и отброшенные (ключа нет в его версии).
    pub overrides: BTreeMap<String, String>,
    pub overrides_dropped: Vec<String>,
    pub corpus_fingerprint: String,
    pub nuggets_fingerprint: String,
    /// Отпечатки сверены с `nuggets.lock` (`false` — замка нет, прогон до заморозки).
    pub frozen: bool,
    /// Цели и сценарии, исключённые из всех плеч (старый сервер их не умеет), с причиной.
    pub excluded: Vec<String>,
    pub counter: String,
    pub read_budget: usize,
}

/// Итог плеча.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpResult {
    pub info: McpInfo,
    pub summary: McpSummary,
    pub scenarios: Vec<McpScenario>,
}

/// Сравнение плеча с базой (спецификация M6 §3.5).
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub base: String,
    pub arm: String,
    pub split: String,
    pub scenarios: usize,
    /// Сценарии только в одном из плеч (в сравнение не входят).
    pub only_base: Vec<String>,
    pub only_arm: Vec<String>,
    pub base_tokens: usize,
    pub arm_tokens: usize,
    /// `R = Σ плеча / Σ базы`, парный бутстреп сценариев (95 %).
    pub r: Option<Estimate>,
    pub base_micro: f64,
    pub arm_micro: f64,
    /// Δ микро-полноты (плечо − база), односторонние 95 % границы.
    pub d_micro: Estimate,
    pub net_losses: i64,
    /// `d_micro.lo ≥ −0,02` и `L ≤ 1`.
    pub noninferior: bool,
}

/// Сводка пути по сценариям.
fn totals(paths: &[(&AgentPath, usize)]) -> PathTotals {
    let tokens: Vec<f64> = paths.iter().map(|(p, _)| p.tokens as f64).collect();
    PathTotals {
        tokens_total: paths.iter().map(|(p, _)| p.tokens).sum(),
        tokens_mean: (!tokens.is_empty()).then(|| crate::eval::metrics::estimate(&tokens)),
        micro_recall: micro_recall(&paths.iter().map(|(p, n)| (p.found, *n)).collect::<Vec<_>>()),
        full: paths.iter().filter(|(p, _)| p.complete).count(),
    }
}

/// Сводка плеча.
pub fn summarize(runs: &[McpScenario]) -> McpSummary {
    let col = |f: fn(&McpScenario) -> &AgentPath| {
        runs.iter()
            .map(|r| (f(r), r.nuggets_total))
            .collect::<Vec<_>>()
    };
    let cached = PathTotals {
        tokens_total: runs.iter().map(|r| r.cached.tokens).sum(),
        full: runs.iter().filter(|r| r.cached.complete).count(),
        ..PathTotals::default()
    };
    let pairs: Vec<(f64, f64)> = runs
        .iter()
        .map(|r| (r.protocol.tokens as f64, r.naive.tokens as f64))
        .collect();
    McpSummary {
        scenarios: runs.len(),
        nuggets_total: runs.iter().map(|r| r.nuggets_total).sum(),
        protocol: totals(&col(|r| &r.protocol)),
        reference: totals(&col(|r| &r.reference)),
        cached,
        naive: totals(&col(|r| &r.naive)),
        summary_tokens_total: runs.iter().map(|r| r.summary_tokens).sum(),
        protocol_to_naive: ratio_estimate_with(&pairs, BOOTSTRAP_M6, SEED),
    }
}

/// Сравнение плеча `arm` с базой `base`: отпечатки корпуса и нагетов и часть разбиения должны
/// совпадать; сценарии — общие по id.
pub fn compare(base: &McpResult, arm: &McpResult, names: (&str, &str)) -> Result<Comparison> {
    let (bi, ai) = (&base.info, &arm.info);
    let mut errs = Vec::new();
    if bi.corpus_fingerprint != ai.corpus_fingerprint {
        errs.push(format!(
            "corpus_fingerprint {} (плечо) ≠ {} (база)",
            ai.corpus_fingerprint, bi.corpus_fingerprint
        ));
    }
    if bi.nuggets_fingerprint != ai.nuggets_fingerprint {
        errs.push(format!(
            "nuggets_fingerprint {} (плечо) ≠ {} (база)",
            ai.nuggets_fingerprint, bi.nuggets_fingerprint
        ));
    }
    if bi.split != ai.split {
        errs.push(format!(
            "часть разбиения {} (плечо) ≠ {} (база)",
            ai.split, bi.split
        ));
    }
    if !errs.is_empty() {
        bail!(
            "плечи сняты на разных наборах — сравнение запрещено (спецификация M6 §3.2): {}",
            errs.join("; ")
        );
    }
    let by_id: BTreeMap<&str, &McpScenario> =
        base.scenarios.iter().map(|s| (s.id.as_str(), s)).collect();
    let arm_ids: BTreeSet<&str> = arm.scenarios.iter().map(|s| s.id.as_str()).collect();
    let pairs: Vec<(&McpScenario, &McpScenario)> = arm
        .scenarios
        .iter()
        .filter_map(|a| by_id.get(a.id.as_str()).map(|b| (*b, a)))
        .collect();
    let frac = |p: &AgentPath, n: usize| {
        if n == 0 {
            0.0
        } else {
            p.found as f64 / n as f64
        }
    };
    let w: Vec<f64> = pairs.iter().map(|(b, _)| b.nuggets_total as f64).collect();
    let bf: Vec<f64> = pairs
        .iter()
        .map(|(b, _)| frac(&b.protocol, b.nuggets_total))
        .collect();
    let af: Vec<f64> = pairs
        .iter()
        .map(|(b, a)| frac(&a.protocol, b.nuggets_total))
        .collect();
    let d_micro = paired_diff_weighted(&bf, &af, &w, BOOTSTRAP_M6, SEED);
    let l = net_losses(
        &pairs
            .iter()
            .map(|(b, _)| b.protocol.complete)
            .collect::<Vec<_>>(),
        &pairs
            .iter()
            .map(|(_, a)| a.protocol.complete)
            .collect::<Vec<_>>(),
    );
    let ratio_pairs: Vec<(f64, f64)> = pairs
        .iter()
        .map(|(b, a)| (a.protocol.tokens as f64, b.protocol.tokens as f64))
        .collect();
    Ok(Comparison {
        base: names.0.to_string(),
        arm: names.1.to_string(),
        split: ai.split.clone(),
        scenarios: pairs.len(),
        only_base: by_id
            .keys()
            .filter(|id| !arm_ids.contains(*id))
            .map(|s| s.to_string())
            .collect(),
        only_arm: arm_ids
            .iter()
            .filter(|id| !by_id.contains_key(*id))
            .map(|s| s.to_string())
            .collect(),
        base_tokens: pairs.iter().map(|(b, _)| b.protocol.tokens).sum(),
        arm_tokens: pairs.iter().map(|(_, a)| a.protocol.tokens).sum(),
        r: ratio_estimate_with(&ratio_pairs, BOOTSTRAP_M6, SEED),
        base_micro: micro_recall(
            &pairs
                .iter()
                .map(|(b, _)| (b.protocol.found, b.nuggets_total))
                .collect::<Vec<_>>(),
        ),
        arm_micro: micro_recall(
            &pairs
                .iter()
                .map(|(b, a)| (a.protocol.found, b.nuggets_total))
                .collect::<Vec<_>>(),
        ),
        noninferior: d_micro.lo >= MIN_D_MICRO && l <= MAX_NET_LOSSES,
        d_micro,
        net_losses: l,
    })
}

fn read_result(path: &Path) -> Result<McpResult> {
    let t = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    serde_json::from_str(&t).with_context(|| format!("разбор {}", path.display()))
}

fn file_name(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Нагеты сценария, найденные в тексте.
fn found_nuggets(sc: &Scenario, delivered: &str) -> usize {
    let norm = normalize_md(delivered);
    sc.nuggets
        .iter()
        .filter(|n| nuggets::found(&norm, &n.text))
        .count()
}

/// Источник нужно дочитать: он ожидаемый и его нагет не выдан (без нагетов — всегда).
fn source_missing(sc: &Scenario, source: &str, delivered_norm: &str) -> bool {
    if !sc.expect.iter().any(|e| e == source) {
        return false;
    }
    let mut ns = sc.nuggets_of(source).peekable();
    ns.peek().is_none() || ns.any(|n| !nuggets::found(delivered_norm, &n.text))
}

/// Чтение выбранных `(источник набора, ссылка)` и дочитывание (§3.4, п. 2–3).
fn read_and_reread(
    c: &McpClient,
    counter: &Counter,
    sc: &Scenario,
    picked: &[(String, String)],
    path: &mut AgentPath,
) -> Result<String> {
    let items: Vec<Value> = picked.iter().map(|(_, raw)| json!({ "id": raw })).collect();
    let read = c.call(
        "memory_read",
        json!({ "items": items, "budget_tokens": READ_BUDGET }),
    )?;
    path.push("memory_read", &read, counter);
    path.read = picked.iter().map(|(r, _)| r.clone()).collect();
    let raws: Vec<String> = picked.iter().map(|(_, raw)| raw.clone()).collect();
    let elements = read_elements(&read, &raws);
    let mut pending = Vec::new();
    for (r, raw) in picked {
        let delivered = normalize_md(&path.delivered());
        if !source_missing(sc, r, &delivered) {
            continue;
        }
        let Some(e) = elements.iter().find(|e| &e.raw == raw) else {
            continue;
        };
        if let Some(need) = reread_need(&e.text) {
            let again = c.call(
                "memory_read",
                json!({ "items": reread_items(raw, &need), "budget_tokens": READ_BUDGET }),
            )?;
            path.push(&format!("memory_read дочитывание {r}"), &again, counter);
            path.reread.push(r.clone());
            if reread_need(&again).is_some() {
                pending.push(r.clone());
            }
        }
    }
    if !pending.is_empty() {
        path.note = format!("после дочитывания ещё пропуски: {pending:?}");
    }
    Ok(read)
}

/// Полнота пути: все нагеты (у сценария без нагетов — все ожидаемые прочитаны без пометок).
fn finish(sc: &Scenario, path: &mut AgentPath) {
    path.found = found_nuggets(sc, &path.delivered());
    path.complete = if sc.nuggets.is_empty() {
        sc.expect.iter().all(|e| path.read.contains(e)) && path.note.is_empty()
    } else {
        path.found == sc.nuggets.len()
    };
}

fn run_scenario(
    s: &Session,
    counter: &Counter,
    sc: &Scenario,
    part: Part,
    tag: &str,
) -> Result<McpScenario> {
    let client = |suffix: &str| {
        McpClient::connect(
            &s.base,
            s.token(),
            EVAL_SOLUTION,
            Some(&format!("{tag}-{suffix}")),
        )
    };
    let scope = json!({ "solution": EVAL_SOLUTION });
    let brief = json!({ "query": sc.question, "scope": scope, "mode": "brief" });
    // (в) наивный — первым: MCP не передаёт no_cache, после (а) в кэше была бы выжимка
    let mut naive = AgentPath::default();
    let chunks = client("c")?.call(
        "memory_search",
        json!({ "query": sc.question, "scope": scope, "mode": "chunks",
                "budget_tokens": NAIVE_BUDGET }),
    )?;
    naive.push("memory_search chunks 4000", &chunks, counter);
    naive.read = mapped_refs(s, &chunks)
        .into_iter()
        .map(|(r, _)| r)
        .filter(|r| sc.expect.contains(r))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    finish(sc, &mut naive);
    // (а) протокол
    let ca = client("a")?;
    let mut protocol = AgentPath::default();
    let cards = ca.call("memory_search", brief.clone())?;
    protocol.push("memory_search brief", &cards, counter);
    let mut selected: Vec<(String, String)> = Vec::new();
    for (r, raw) in mapped_refs(s, &cards) {
        if sc.expect.contains(&r) && !selected.iter().any(|(x, _)| *x == r) {
            selected.push((r, raw));
        }
    }
    let mut summary_tokens = 0;
    if selected.is_empty() {
        protocol.note = "ни одного ожидаемого источника в карточках".into();
    } else {
        let read = read_and_reread(&ca, counter, sc, &selected, &mut protocol)?;
        let summary = summary_of(&sc.question, &read);
        summary_tokens = counter.count(&summary);
        let sources: Vec<Value> = selected
            .iter()
            .map(|(_, raw)| json!({ "id": raw }))
            .collect();
        let put = ca.call(
            "memory_cache_put",
            json!({ "query": sc.question, "summary": summary, "sources": sources }),
        )?;
        protocol.push("memory_cache_put", &put, counter);
        let missing: Vec<&String> = sc
            .expect
            .iter()
            .filter(|e| !selected.iter().any(|(r, _)| r == *e))
            .collect();
        if !missing.is_empty() {
            let n = format!("нет в карточках: {missing:?}");
            protocol.note = [protocol.note.as_str(), n.as_str()]
                .iter()
                .filter(|x| !x.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("; ");
        }
    }
    finish(sc, &mut protocol);
    // справочно: карточки 1–3 по порядку, без оракула (поиск — тот же ответ)
    let mut reference = AgentPath::default();
    reference.push("memory_search brief", &cards, counter);
    let first: Vec<(String, String)> = refs_in_text(&cards)
        .into_iter()
        .filter(|(k, _)| matches!(k, 'n' | 'i'))
        .take(REFERENCE_CARDS)
        .map(|(k, id)| {
            let raw = format!("{k}:{id}");
            let r = mapped_refs(s, &raw)
                .into_iter()
                .next()
                .map_or_else(|| raw.clone(), |(r, _)| r);
            (r, raw)
        })
        .collect();
    if first.is_empty() {
        reference.note = "карточек нет".into();
    } else {
        read_and_reread(&client("r")?, counter, sc, &first, &mut reference)?;
    }
    finish(sc, &mut reference);
    // (б) повтор другой сессией: выжимка кэша первым элементом
    let mut cached = PathRun::default();
    let again = client("b")?.call("memory_search", brief)?;
    cached.push_step("memory_search brief (кэш)", &again, counter, true);
    let first_line = again
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let hit = first_line.contains("[cache]");
    cached.complete = protocol.complete && hit;
    if !hit {
        cached.note = format!(
            "первая строка: {}",
            first_line.chars().take(80).collect::<String>()
        );
    }
    Ok(McpScenario {
        id: sc.id.clone(),
        kind: sc.kind().to_string(),
        split: part.as_str().to_string(),
        question: sc.question.clone(),
        expect: sc.expect.clone(),
        nuggets_total: sc.nuggets.len(),
        summary_tokens,
        protocol,
        reference,
        cached,
        naive,
    })
}

/// Набор ТС-1: корпус, сценарии, отпечатки (сверенные с замком).
pub struct Set {
    pub corpus: Corpus,
    pub scenarios: Vec<Scenario>,
    pub nuggets_fingerprint: String,
    pub corpus_fingerprint: String,
    pub frozen: bool,
    pub nuggets: nuggets::Summary,
}

/// Файл сценариев — свой (`--scenarios`): проверка как у фрагмента (без требований к объёму).
fn custom_scenarios(a: &EvalArgs) -> bool {
    a.scenarios.is_some()
}

/// Загружает и проверяет набор ТС-1, сверяет отпечатки с `nuggets.lock` рядом со сценариями.
pub fn load_set(a: &EvalArgs, p: &Paths) -> Result<Set> {
    let mut corpus = Corpus::load(&p.corpus)?;
    corpus.insights = load_insights(&p.insights)?;
    let (scenarios, _) = load_scenarios(&p.scenarios)?;
    let min = if custom_scenarios(a) {
        1
    } else {
        super::MIN_SCENARIOS
    };
    validate_scenarios_with(&scenarios, &corpus, min)?;
    let checks: Vec<nuggets::Check> = scenarios
        .iter()
        .map(|s| nuggets::Check {
            id: &s.id,
            question: &s.question,
            expect: &s.expect,
            nuggets: &s.nuggets,
        })
        .collect();
    let summary = nuggets::validate(&checks, &corpus)?;
    let nf = nuggets::nuggets_fingerprint(&p.scenarios, &p.corpus);
    let cf = nuggets::corpus_fingerprint(&[&p.corpus, &p.docs, &p.insights]);
    let lock = nuggets::check_lock(&nuggets::lock_path(&p.scenarios), &nf, &cf)?;
    Ok(Set {
        corpus,
        scenarios,
        nuggets_fingerprint: nf,
        corpus_fingerprint: cf,
        frozen: lock.is_some(),
        nuggets: summary,
    })
}

/// Печать сводки набора и отпечатков (`eval --check`).
pub fn print_set(set: &Set, p: &Paths) {
    let n = &set.nuggets;
    println!(
        "набор ТС-1 M6: сценариев {} (с нагетами {}, без {}), нагетов {} (с низким пересечением с \
         вопросом {:.0} %)",
        set.scenarios.len(),
        n.scenarios_with_nuggets,
        n.scenarios_without,
        n.nuggets,
        n.low_share() * 100.0
    );
    println!("  nuggets_fingerprint = \"{}\"", set.nuggets_fingerprint);
    println!("  corpus_fingerprint = \"{}\"", set.corpus_fingerprint);
    let lock = nuggets::lock_path(&p.scenarios);
    if set.frozen {
        println!("  отпечатки совпадают с {}", lock.display());
    } else {
        println!(
            "  замка {} нет — набор не заморожен (задача 1b)",
            lock.display()
        );
    }
}

/// Префикс файлов результата: `--out` — каталог (имя по умолчанию в нём) или префикс пути.
fn out_prefix(a: &EvalArgs, p: &Paths, stem: &str, point: Option<&str>) -> PathBuf {
    let with_point = |base: String| match point {
        Some(pt) => format!("{base}-{pt}"),
        None => base,
    };
    match &a.out {
        Some(o) if !o.is_dir() => PathBuf::from(with_point(o.to_string_lossy().into_owned())),
        Some(o) => o.join(with_point(stem.to_string())),
        None => p.results.join(with_point(stem.to_string())),
    }
}

fn write(prefix: &Path, r: &McpResult) -> Result<(PathBuf, PathBuf)> {
    if let Some(d) = prefix.parent() {
        fs::create_dir_all(d)?;
    }
    let j = PathBuf::from(format!("{}.json", prefix.display()));
    let m = PathBuf::from(format!("{}.md", prefix.display()));
    fs::write(&j, serde_json::to_string(r)? + "\n")?;
    fs::write(&m, crate::eval::report::tokens_m6_md(r))?;
    Ok((j, m))
}

/// Режим без прогона: `--arm <json>` сравнивается с каждым `--compare <json>`.
fn compare_files(arm_path: &Path, bases: &[PathBuf]) -> Result<()> {
    if bases.is_empty() {
        bail!("--arm: укажите базы --compare <json>");
    }
    let arm = read_result(arm_path)?;
    let mut out = String::new();
    for b in bases {
        let base = read_result(b)?;
        let c = compare(&base, &arm, (&file_name(b), &file_name(arm_path)))?;
        out.push_str(&crate::eval::report::comparison_md(&c));
    }
    print!("{out}");
    Ok(())
}

/// Точки прогона: сетка или одна точка из `--set`.
fn points(a: &EvalArgs) -> Result<Vec<grid::Point>> {
    if let Some(g) = &a.grid {
        return grid::load_points(g);
    }
    let env = run::parse_sets(&a.sets)?
        .into_iter()
        .map(|(k, v)| (run::env_key(&k), v))
        .collect();
    Ok(vec![grid::Point {
        name: String::new(),
        env,
    }])
}

/// `cargo xtask eval --tokens --via mcp`.
pub fn main(a: &EvalArgs, p: &Paths) -> Result<()> {
    // до любой работы: сетка на holdout запрещена
    grid::refuse_holdout(a.grid.as_deref(), a.split)?;
    if let Some(arm) = &a.arm {
        return compare_files(arm, &a.compare);
    }
    let Some(part) = a.split else {
        bail!("--via mcp: укажите --split dev|holdout");
    };
    let set = load_set(a, p)?;
    print_set(&set, p);
    let pts = points(a)?;
    let items: Vec<(String, String)> = set
        .scenarios
        .iter()
        .map(|s| (s.id.clone(), s.kind().to_string()))
        .collect();
    let holdout = split(&items);
    let chosen: Vec<&Scenario> = set
        .scenarios
        .iter()
        .filter(|s| holdout.contains(&s.id) == (part == Part::Holdout))
        .collect();
    println!(
        "часть {}: сценариев {} из {}",
        part.as_str(),
        chosen.len(),
        set.scenarios.len()
    );
    let counter = Counter::load(&p.repo)?;
    let server = run::build_server(&p.repo, a.server_bin.as_deref())?;
    let only_fts = a.modes.iter().all(|m| *m == Mode::Fts);
    let model = (!only_fts).then(|| {
        a.model
            .clone()
            .unwrap_or_else(|| run::default_model(a.device).to_string())
    });
    let label = a
        .label
        .as_deref()
        .map_or("m6".to_string(), |l| format!("m6-{l}"));
    let spec = |name: String, sets: Vec<(String, String)>| RunSpec {
        name,
        model: model.clone(),
        device: if only_fts { Device::Cpu } else { a.device },
        reranker: None,
        sets,
        keep_cuda_path: a.keep_cuda_path,
        ctrl_break: false,
    };
    // возможности сервера — до переопределений: неизвестный ключ уронил бы старт
    let probe_name = run::run_name(model.as_deref(), Some(&format!("tokens-{label}-probe")));
    let probe = Session::start(p, &spec(probe_name, Vec::new()), &server, true)?;
    let caps = caps::probe(&probe.base);
    probe.stop()?;
    let caps = caps?;
    println!(
        "сервер {}{} (git {}): фокус {}, идеи {}, снимок {}",
        caps.version,
        if caps.next { " (сборка M6)" } else { "" },
        caps.git.as_deref().unwrap_or("—"),
        caps.focus,
        caps.ideas,
        caps.snapshot
    );
    let mut results: Vec<(String, McpResult)> = Vec::new();
    for pt in &pts {
        let env: Vec<(String, String)> = pt.env.clone().into_iter().collect();
        let (kept, dropped) = caps::split_overrides(&caps, &env);
        if !dropped.is_empty() {
            println!(
                "точка {}: ключей нет в версии сервера, отброшены: {}",
                pt.name,
                dropped.join(", ")
            );
        }
        let sets: Vec<(String, String)> = kept
            .iter()
            .filter_map(|(k, v)| caps::env_to_key(k).map(|key| (key, v.clone())))
            .collect();
        let point = (!pt.name.is_empty()).then(|| pt.name.clone());
        let run_label = match &point {
            Some(n) => format!("tokens-{label}-{n}"),
            None => format!("tokens-{label}"),
        };
        // каталог всегда свежий: выжимки и session_seen прошлой точки исказили бы путь (а)
        let mut s = Session::start(
            p,
            &spec(run::run_name(model.as_deref(), Some(&run_label)), sets),
            &server,
            true,
        )?;
        s.check_model()?;
        s.load_corpus(&set.corpus)?;
        if model.is_some() {
            s.wait_indexed()?;
        }
        let mut runs = Vec::with_capacity(chosen.len());
        for sc in &chosen {
            let tag = format!(
                "m6-{}-{}{}",
                run::today(),
                sc.id,
                point.as_deref().map_or(String::new(), |n| format!("-{n}"))
            );
            runs.push(run_scenario(&s, &counter, sc, part, &tag)?);
        }
        let st = s.status.clone();
        s.stop()?;
        let r = McpResult {
            info: McpInfo {
                date: run::today(),
                git: run::git_rev(&p.repo),
                label: a.label.clone(),
                point: point.clone(),
                split: part.as_str().to_string(),
                model: model.clone(),
                model_id: model
                    .as_ref()
                    .map(|_| st["model"]["id"].as_str().unwrap_or_default().to_string()),
                device: model.as_ref().map(|_| {
                    st["device"]["kind"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                }),
                server_version: caps.version.to_string(),
                server_git: caps.git.clone(),
                server_next: caps.next,
                focus: caps.focus,
                ideas: caps.ideas,
                snapshot: caps.snapshot,
                overrides: kept.into_iter().collect(),
                overrides_dropped: dropped,
                corpus_fingerprint: set.corpus_fingerprint.clone(),
                nuggets_fingerprint: set.nuggets_fingerprint.clone(),
                frozen: set.frozen,
                excluded: Vec::new(),
                counter: "bge-m3 tokenizer.json".into(),
                read_budget: READ_BUDGET,
            },
            summary: summarize(&runs),
            scenarios: runs,
        };
        let stem = format!(
            "{}-m6-{}-tokens{}-{}",
            r.info.date,
            model.as_deref().unwrap_or("fts"),
            a.label
                .as_deref()
                .map_or(String::new(), |l| format!("-{l}")),
            part.as_str()
        );
        let (j, m) = write(&out_prefix(a, p, &stem, point.as_deref()), &r)?;
        print!("{}", crate::eval::report::tokens_m6_md(&r));
        println!("  {}\n  {}", j.display(), m.display());
        results.push((point.unwrap_or_else(|| "default".into()), r));
    }
    // сетка: каждая точка против первой
    if results.len() > 1 {
        let (base_name, base) = &results[0];
        for (name, r) in &results[1..] {
            let c = compare(base, r, (base_name, name))?;
            print!("{}", crate::eval::report::comparison_md(&c));
        }
    }
    for b in &a.compare {
        let base = read_result(b)?;
        for (name, r) in &results {
            let c = compare(&base, r, (&file_name(b), name))?;
            print!("{}", crate::eval::report::comparison_md(&c));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::nuggets::Nugget;

    fn path(tokens: usize, found: usize, complete: bool) -> AgentPath {
        AgentPath {
            tokens,
            found,
            complete,
            ..AgentPath::default()
        }
    }

    fn scen(id: &str, n: usize, p: AgentPath) -> McpScenario {
        McpScenario {
            id: id.into(),
            nuggets_total: n,
            protocol: p,
            ..McpScenario::default()
        }
    }

    fn arm(fp: &str, sc: Vec<McpScenario>) -> McpResult {
        McpResult {
            info: McpInfo {
                corpus_fingerprint: fp.into(),
                nuggets_fingerprint: "n".into(),
                split: "dev".into(),
                ..McpInfo::default()
            },
            summary: McpSummary::default(),
            scenarios: sc,
        }
    }

    /// R, Δ микро-полноты и чистые потери — по общим сценариям; лишние — в списках.
    #[test]
    fn compare_pairs_by_id() {
        let base = arm(
            "c",
            vec![
                scen("a", 2, path(1000, 2, true)),
                scen("b", 2, path(1000, 1, false)),
                scen("x", 1, path(500, 1, true)),
            ],
        );
        let m6 = arm(
            "c",
            vec![
                scen("a", 2, path(600, 1, false)),
                scen("b", 2, path(800, 2, true)),
                scen("y", 1, path(1, 1, true)),
            ],
        );
        let c = compare(&base, &m6, ("b4", "m6")).unwrap();
        assert_eq!(c.scenarios, 2);
        assert_eq!(
            (c.only_base.clone(), c.only_arm.clone()),
            (vec!["x".to_string()], vec!["y".to_string()])
        );
        assert!((c.r.unwrap().mean - 0.7).abs() < 1e-12);
        assert_eq!(c.net_losses, 0);
        assert!((c.d_micro.mean - 0.0).abs() < 1e-12);
        assert_eq!((c.base_micro, c.arm_micro), (0.75, 0.75));
        let other = arm("d", vec![]);
        let e = compare(&base, &other, ("b4", "m6"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("corpus_fingerprint d (плечо) ≠ c (база)"), "{e}");
    }

    /// Дочитывание нужно только ожидаемому источнику с невыданным нагетом.
    #[test]
    fn reread_only_for_missing_nuggets() {
        let sc = Scenario {
            id: "s".into(),
            kind: None,
            question: "q".into(),
            expect: vec!["ru/a".into(), "ru/b".into()],
            nuggets: vec![Nugget {
                source: "ru/a".into(),
                text: "нужный фрагмент текста".into(),
            }],
        };
        let shown = normalize_md("…\n**нужный** фрагмент\nтекста");
        assert!(!source_missing(&sc, "ru/a", &shown));
        assert!(source_missing(&sc, "ru/a", "другое"));
        // ожидаемый без нагетов — дочитывается всегда; чужой — никогда
        assert!(source_missing(&sc, "ru/b", &shown));
        assert!(!source_missing(&sc, "ru/zzz", ""));
    }
}
