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
//!   вызовом, бюджет [`READ_BUDGET`]) → `memory_cache_put` выжимки прочитанного и дочитанного;
//! - **справочная политика без оракула** — карточки 1–3 того же поиска по порядку, чтение и
//!   дочитывание по тем же правилам (своя сессия, без выжимки);
//! - **(б) повтор** — `memory_search` brief другой сессией: выжимка кэша первой.
//!
//! **Кэш между сценариями.** После (б) выжимка сценария удаляется (`DELETE /api/v1/cache/{id}`,
//! есть с M4; вне MCP, токены не считаются): иначе похожий вопрос следующего сценария получил бы
//! чужую выжимку семантическим совпадением. Сторож — первая строка ответов (а) и (в): `[cache]`
//! там — чужое попадание (счётчик и примечание в отчёте; на holdout — ошибка).
//!
//! Токены — сумма текстов `result.content` вызовов (токенизатор bge-m3). Полнота — нагеты
//! (§3.2): найдены, если целиком есть в выданном агенту тексте пути. Отчёт хранит ответы
//! целиком. Перед прогоном сверяются отпечатки с `eval/nuggets.lock`; сравнение плеч
//! ([`compare`]) — только на одних отпечатках, части разбиения, бюджете чтения, счётчике,
//! модели и устройстве, без отброшенных переопределений. Сравнение пишется в
//! `<префикс плеча>-vs-<база>.{json,md}`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Counter, NAIVE_BUDGET, PathRun, READ_BUDGET, Scenario, load_scenarios, mapped_refs,
    read_elements, reread_items, reread_need, summary_of, validate_scenarios_with,
};
use crate::eval::caps;
use crate::eval::corpus::{Corpus, EVAL_SOLUTION, INSIGHT_REF, load_insights};
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
/// Пометка выжимки кэша в первой строке ответа поиска.
const CACHE_MARK: &str = "[cache]";

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

    /// Дописать примечание через «; ».
    fn add_note(&mut self, n: &str) {
        if !self.note.is_empty() {
            self.note.push_str("; ");
        }
        self.note.push_str(n);
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
    /// Пути, где первой пришла чужая выжимка кэша (`(а)`, `(в)`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub foreign_cache: Vec<String>,
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
    /// Чужих попаданий в кэш (первая строка (а) или (в) — `[cache]`).
    pub foreign_cache_hits: usize,
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
    /// Сборка ветки M6 (`features` в `/health`, [`caps::ServerCaps::m6`]).
    pub server_m6: bool,
    pub server_features: Vec<String>,
    pub focus: bool,
    pub ideas: bool,
    pub snapshot: bool,
    /// Переопределения, отправленные серверу, и отброшенные (ключа нет в его версии): плечо с
    /// отброшенными к сравнению не допускается.
    pub overrides: BTreeMap<String, String>,
    pub overrides_dropped: Vec<String>,
    pub corpus_fingerprint: String,
    pub nuggets_fingerprint: String,
    /// Отпечатки сверены с `nuggets.lock` (`false` — замка нет, прогон до заморозки).
    pub frozen: bool,
    /// Сценарии, исключённые из всех плеч (старый сервер их не умеет): `<id>: <причина>`.
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

impl McpResult {
    /// id исключённых сценариев (`<id>: <причина>`).
    fn excluded_ids(&self) -> BTreeSet<&str> {
        self.info
            .excluded
            .iter()
            .map(|e| e.split_once(':').map_or(e.as_str(), |(id, _)| id).trim())
            .collect()
    }
}

/// Параметры бутстрепа сравнения.
#[derive(Debug, Clone, Serialize)]
pub struct Bootstrap {
    pub samples: usize,
    pub seed: u64,
    /// Интервал `R`.
    pub r_interval: &'static str,
    /// Границы Δ микро-полноты.
    pub d_micro_interval: &'static str,
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
    /// Δ микро-полноты (плечо − база), односторонние 95 % границы; `None` — нагетов нет.
    pub d_micro: Option<Estimate>,
    pub net_losses: i64,
    /// `d_micro.lo ≥ −0,02` и `L ≤ 1` (без нагетов — нет).
    pub noninferior: bool,
    pub bootstrap: Bootstrap,
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
        foreign_cache_hits: runs.iter().map(|r| r.foreign_cache.len()).sum(),
    }
}

/// Поле условий замера должно совпасть у плеч.
fn same<T: PartialEq + std::fmt::Debug>(errs: &mut Vec<String>, name: &str, arm: &T, base: &T) {
    if arm != base {
        errs.push(format!("{name} {arm:?} (плечо) ≠ {base:?} (база)"));
    }
}

/// Сравнение плеча `arm` с базой `base`. Условия замера должны совпадать: отпечатки корпуса и
/// нагетов, часть разбиения, бюджет чтения, счётчик, модель и устройство; у обоих нет
/// отброшенных переопределений. Сценарии — общие по id; на holdout сценарий только в одном
/// плече допустим лишь как исключённый (`info.excluded`).
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
    same(&mut errs, "часть разбиения", &ai.split, &bi.split);
    same(&mut errs, "read_budget", &ai.read_budget, &bi.read_budget);
    same(&mut errs, "counter", &ai.counter, &bi.counter);
    same(&mut errs, "модель", &ai.model, &bi.model);
    same(&mut errs, "устройство", &ai.device, &bi.device);
    for (who, i) in [("плечо", ai), ("база", bi)] {
        if !i.overrides_dropped.is_empty() {
            errs.push(format!(
                "{who}: переопределения отброшены сервером ({}) — точка не та, что задана",
                i.overrides_dropped.join(", ")
            ));
        }
    }
    if !errs.is_empty() {
        bail!(
            "плечи сняты в разных условиях — сравнение запрещено (спецификация M6 §3.2, §3.5): {}",
            errs.join("; ")
        );
    }
    // исключённые в любом плече (старый сервер не умеет цель) — вне сравнения обоих плеч
    let excluded: BTreeSet<&str> = base
        .excluded_ids()
        .into_iter()
        .chain(arm.excluded_ids())
        .collect();
    let by_id: BTreeMap<&str, &McpScenario> =
        base.scenarios.iter().map(|s| (s.id.as_str(), s)).collect();
    let arm_ids: BTreeSet<&str> = arm.scenarios.iter().map(|s| s.id.as_str()).collect();
    let pairs: Vec<(&McpScenario, &McpScenario)> = arm
        .scenarios
        .iter()
        .filter(|a| !excluded.contains(a.id.as_str()))
        .filter_map(|a| by_id.get(a.id.as_str()).map(|b| (*b, a)))
        .collect();
    let only_base: Vec<String> = by_id
        .keys()
        .filter(|id| !arm_ids.contains(*id))
        .map(|s| s.to_string())
        .collect();
    let only_arm: Vec<String> = arm_ids
        .iter()
        .filter(|id| !by_id.contains_key(*id))
        .map(|s| s.to_string())
        .collect();
    if ai.split == Part::Holdout.as_str() {
        let stray: Vec<&String> = only_base
            .iter()
            .chain(&only_arm)
            .filter(|id| !excluded.contains(id.as_str()))
            .collect();
        if !stray.is_empty() {
            bail!(
                "holdout: сценарии только в одном плече и не исключённые (info.excluded): \
                 {stray:?} — приёмка идёт на одном наборе"
            );
        }
    }
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
    let d_micro = (w.iter().sum::<f64>() > 0.0)
        .then(|| paired_diff_weighted(&bf, &af, &w, BOOTSTRAP_M6, SEED));
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
    let micro = |f: fn(&(&McpScenario, &McpScenario)) -> usize| {
        micro_recall(
            &pairs
                .iter()
                .map(|p| (f(p), p.0.nuggets_total))
                .collect::<Vec<_>>(),
        )
    };
    Ok(Comparison {
        base: names.0.to_string(),
        arm: names.1.to_string(),
        split: ai.split.clone(),
        scenarios: pairs.len(),
        only_base,
        only_arm,
        base_tokens: pairs.iter().map(|(b, _)| b.protocol.tokens).sum(),
        arm_tokens: pairs.iter().map(|(_, a)| a.protocol.tokens).sum(),
        r: ratio_estimate_with(&ratio_pairs, BOOTSTRAP_M6, SEED),
        base_micro: micro(|(b, _)| b.protocol.found),
        arm_micro: micro(|(_, a)| a.protocol.found),
        noninferior: d_micro.is_some_and(|d| d.lo >= MIN_D_MICRO) && l <= MAX_NET_LOSSES,
        d_micro,
        net_losses: l,
        bootstrap: Bootstrap {
            samples: BOOTSTRAP_M6,
            seed: SEED,
            r_interval: "двусторонний 95 % (перцентили 2,5 и 97,5), парный бутстреп сценариев",
            d_micro_interval: "односторонние 95 % (перцентили 5 и 95), веса — число нагетов",
        },
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

/// Пишет сравнение в `<префикс плеча>-vs-<база>.{json,md}`.
fn write_comparison(arm_prefix: &Path, c: &Comparison) -> Result<(PathBuf, PathBuf)> {
    let stem = format!("{}-vs-{}", arm_prefix.display(), c.base);
    let j = PathBuf::from(format!("{stem}.json"));
    let m = PathBuf::from(format!("{stem}.md"));
    if let Some(d) = j.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(&j, serde_json::to_string_pretty(c)? + "\n")?;
    fs::write(&m, crate::eval::report::comparison_md(c))?;
    Ok((j, m))
}

/// Сравнение с выводом и записью рядом с плечом.
fn compare_and_write(
    base: &McpResult,
    arm: &McpResult,
    names: (&str, &str),
    arm_prefix: &Path,
) -> Result<()> {
    let c = compare(base, arm, names)?;
    print!("{}", crate::eval::report::comparison_md(&c));
    let (j, m) = write_comparison(arm_prefix, &c)?;
    println!("  {}\n  {}", j.display(), m.display());
    Ok(())
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

/// Чтение выбранных `(источник набора, ссылка)` и дочитывание (§3.4, п. 2–3); возвращает
/// прочитанное и дочитанное одним текстом (для выжимки).
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
    let mut all = read.clone();
    let mut pending = Vec::new();
    let mut unparsed = Vec::new();
    for (r, raw) in picked {
        let delivered = normalize_md(&path.delivered());
        if !source_missing(sc, r, &delivered) {
            continue;
        }
        let Some(e) = elements.iter().find(|e| &e.raw == raw) else {
            unparsed.push(r.clone());
            continue;
        };
        if let Some(need) = reread_need(&e.text) {
            let again = c.call(
                "memory_read",
                json!({ "items": reread_items(raw, &need), "budget_tokens": READ_BUDGET }),
            )?;
            path.push(&format!("memory_read дочитывание {r}"), &again, counter);
            path.reread.push(r.clone());
            all.push('\n');
            all.push_str(&again);
            if reread_need(&again).is_some() {
                pending.push(r.clone());
            }
        }
    }
    if !unparsed.is_empty() {
        path.add_note(&format!(
            "элемент ответа memory_read не разобран (дочитывание невозможно): {unparsed:?}"
        ));
    }
    if !pending.is_empty() {
        path.add_note(&format!("после дочитывания ещё пропуски: {pending:?}"));
    }
    Ok(all)
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

/// Первая непустая строка ответа поиска — выжимка кэша.
fn cache_first(text: &str) -> bool {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.contains(CACHE_MARK))
}

/// Ссылка выжимки `c_<id>` из ответа `memory_cache_put`.
fn cache_ref(put: &str) -> Option<String> {
    let b = put.as_bytes();
    (0..b.len().saturating_sub(27)).find_map(|i| {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let ok = prev_ok
            && &b[i..i + 2] == b"c_"
            && b[i + 2..i + 28].iter().all(u8::is_ascii_alphanumeric)
            && b.get(i + 28).is_none_or(|c| !c.is_ascii_alphanumeric());
        ok.then(|| put[i..i + 28].to_string())
    })
}

/// `DELETE /api/v1/cache/{id}` — выжимка сценария больше не попадёт следующим.
fn delete_cache(s: &Session, id: &str) -> Result<()> {
    let a = crate::smoke::agent_with(Duration::from_secs(30));
    let r = a
        .delete(&format!("{}/api/v1/cache/{id}", s.base))
        .header("Authorization", &format!("Bearer {}", s.token()))
        .header("X-Zam-Solution", EVAL_SOLUTION)
        .call()
        .with_context(|| format!("DELETE /cache/{id}"))?;
    let st = r.status().as_u16();
    if st != 204 && st != 200 {
        bail!("DELETE /cache/{id}: HTTP {st}");
    }
    Ok(())
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
    let mut foreign = Vec::new();
    // (в) наивный — первым: MCP не передаёт no_cache, после (а) в кэше была бы выжимка
    let mut naive = AgentPath::default();
    let chunks = client("c")?.call(
        "memory_search",
        json!({ "query": sc.question, "scope": scope, "mode": "chunks",
                "budget_tokens": NAIVE_BUDGET }),
    )?;
    naive.push("memory_search chunks 4000", &chunks, counter);
    if cache_first(&chunks) {
        foreign.push("(в)".to_string());
        naive.add_note("первой пришла чужая выжимка кэша");
    }
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
    if cache_first(&cards) {
        foreign.push("(а)".to_string());
        protocol.add_note("первой пришла чужая выжимка кэша");
    }
    let mut selected: Vec<(String, String)> = Vec::new();
    for (r, raw) in mapped_refs(s, &cards) {
        if sc.expect.contains(&r) && !selected.iter().any(|(x, _)| *x == r) {
            selected.push((r, raw));
        }
    }
    let mut summary_tokens = 0;
    let mut own_cache = None;
    if selected.is_empty() {
        protocol.add_note("ни одного ожидаемого источника в карточках");
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
        own_cache = cache_ref(&put);
        if own_cache.is_none() {
            protocol.add_note("ссылки выжимки нет в ответе memory_cache_put — не удалена");
        }
        let missing: Vec<&String> = sc
            .expect
            .iter()
            .filter(|e| !selected.iter().any(|(r, _)| r == *e))
            .collect();
        if !missing.is_empty() {
            protocol.add_note(&format!("нет в карточках: {missing:?}"));
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
        reference.add_note("карточек нет");
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
    let hit = first_line.contains(CACHE_MARK);
    cached.complete = protocol.complete && hit;
    if !hit {
        cached.note = format!(
            "первая строка: {}",
            first_line.chars().take(80).collect::<String>()
        );
    }
    // выжимка сценария не должна достаться следующему
    if let Some(id) = own_cache {
        delete_cache(s, &id)?;
    }
    if !foreign.is_empty() && part == Part::Holdout {
        bail!(
            "{}: чужая выжимка кэша первой в {} — на holdout замер недействителен",
            sc.id,
            foreign.join(", ")
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
        foreign_cache: foreign,
    })
}

/// Перечень `info.excluded` по парам `contradicts`, которых нет у сервера `server` (B4): каждая
/// пара (`contradicts <a>→<b>: …`) и сценарий, у которого ожидаемый источник — insight пары
/// (`<id>: …`): его выдача у старого сервера без связи иная, и в сравнении плеч его нет.
pub fn excluded_by_edges(
    scenarios: &[&Scenario],
    skipped: &[(String, String)],
    server: &str,
) -> Vec<String> {
    let mut out: Vec<String> = skipped
        .iter()
        .map(|(a, b)| {
            format!(
                "contradicts {a}→{b}: связь не создана — у сервера {server} нет contradicts (с \
                 0.0.6-m4.5)"
            )
        })
        .collect();
    for sc in scenarios {
        let pairs: Vec<String> = skipped
            .iter()
            .filter(|(a, b)| {
                [a, b]
                    .iter()
                    .any(|k| sc.expect.contains(&format!("{INSIGHT_REF}{k}")))
            })
            .map(|(a, b)| format!("{a}→{b}"))
            .collect();
        if !pairs.is_empty() {
            out.push(format!(
                "{}: ожидаемый источник в паре contradicts {} — у сервера {server} связи нет",
                sc.id,
                pairs.join(", ")
            ));
        }
    }
    out
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
    let nf = nuggets::nuggets_fingerprint(&p.scenarios, &p.corpus, &p.insights)?;
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

/// `--out` — каталог: существующий или с `/` (`\`) на конце (создаётся).
fn out_is_dir(o: &Path) -> bool {
    let s = o.to_string_lossy();
    o.is_dir() || s.ends_with('/') || s.ends_with('\\')
}

/// Префикс файлов результата: `--out` — каталог (имя по умолчанию в нём) или префикс пути;
/// каталог префикса создаётся.
fn out_prefix(a: &EvalArgs, p: &Paths, stem: &str, point: Option<&str>) -> Result<PathBuf> {
    let with_point = |base: String| match point {
        Some(pt) => format!("{base}-{pt}"),
        None => base,
    };
    let prefix = match &a.out {
        Some(o) if out_is_dir(o) => {
            fs::create_dir_all(o).with_context(|| format!("каталог --out {}", o.display()))?;
            o.join(with_point(stem.to_string()))
        }
        Some(o) => PathBuf::from(with_point(o.to_string_lossy().into_owned())),
        None => p.results.join(with_point(stem.to_string())),
    };
    if let Some(d) = prefix.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(d).with_context(|| format!("каталог {}", d.display()))?;
    }
    Ok(prefix)
}

fn write(prefix: &Path, r: &McpResult) -> Result<(PathBuf, PathBuf)> {
    let j = PathBuf::from(format!("{}.json", prefix.display()));
    let m = PathBuf::from(format!("{}.md", prefix.display()));
    fs::write(&j, serde_json::to_string(r)? + "\n")?;
    fs::write(&m, crate::eval::report::tokens_m6_md(r))?;
    Ok((j, m))
}

/// Префикс плеча из пути его JSON (без `.json`).
fn prefix_of(json_path: &Path) -> PathBuf {
    json_path.with_extension("")
}

/// Режим без прогона: `--arm <json>` сравнивается с каждым `--compare <json>`; сравнения —
/// рядом с плечом.
fn compare_files(arm_path: &Path, bases: &[PathBuf]) -> Result<()> {
    if bases.is_empty() {
        bail!("--arm: укажите базы --compare <json>");
    }
    let arm = read_result(arm_path)?;
    for b in bases {
        let base = read_result(b)?;
        compare_and_write(
            &base,
            &arm,
            (&file_name(b), &file_name(arm_path)),
            &prefix_of(arm_path),
        )?;
    }
    Ok(())
}

/// Точки прогона: сетка или одна точка из `--set`. `--set` вместе с `--grid` — ошибка: точка
/// сетки описана файлом целиком, иначе результат не воспроизводится по файлу. Ключи вне
/// таблицы замера — ошибка до старта.
fn points(a: &EvalArgs) -> Result<Vec<grid::Point>> {
    let pts = match &a.grid {
        Some(g) => {
            if !a.sets.is_empty() {
                bail!(
                    "--set вместе с --grid: переопределения точки — только в файле сетки ({})",
                    g.display()
                );
            }
            grid::load_points(g)?
        }
        None => {
            let env = run::parse_sets(&a.sets)?
                .into_iter()
                .map(|(k, v)| (run::env_key(&k), v))
                .collect();
            vec![grid::Point {
                name: String::new(),
                env,
            }]
        }
    };
    for pt in &pts {
        let env: Vec<(String, String)> = pt.env.clone().into_iter().collect();
        caps::check_known(&env).with_context(|| {
            if pt.name.is_empty() {
                "--set".to_string()
            } else {
                format!("точка {}", pt.name)
            }
        })?;
    }
    Ok(pts)
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
        "сервер {} (git {}, features {:?}): фокус {}, идеи {}, снимок {}",
        caps.tag(),
        caps.git.as_deref().unwrap_or("—"),
        caps.features,
        caps.focus,
        caps.ideas,
        caps.snapshot
    );
    let mut results: Vec<(String, PathBuf, McpResult)> = Vec::new();
    for pt in &pts {
        let env: Vec<(String, String)> = pt.env.clone().into_iter().collect();
        let (kept, dropped) = caps::split_overrides(&caps, &env)?;
        if !dropped.is_empty() {
            println!(
                "точка {}: ключей нет у сервера {}, отброшены: {} — к сравнению не допускается",
                pt.name,
                caps.tag(),
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
        if !caps.contradicts {
            s.without_contradicts();
        }
        s.load_corpus(&set.corpus)?;
        let excluded = excluded_by_edges(&chosen, s.skipped_edges(), &caps.tag());
        if !excluded.is_empty() {
            println!("исключено (сервер {}): {}", caps.tag(), excluded.join("; "));
        }
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
                server_m6: caps.m6,
                server_features: caps.features.clone(),
                focus: caps.focus,
                ideas: caps.ideas,
                snapshot: caps.snapshot,
                overrides: kept.into_iter().collect(),
                overrides_dropped: dropped,
                corpus_fingerprint: set.corpus_fingerprint.clone(),
                nuggets_fingerprint: set.nuggets_fingerprint.clone(),
                frozen: set.frozen,
                excluded,
                counter: "bge-m3 tokenizer.json".into(),
                read_budget: READ_BUDGET,
            },
            summary: summarize(&runs),
            scenarios: runs,
        };
        let stem = format!(
            "{}-m6-{}-tokens{}-srv{}-{}",
            r.info.date,
            model.as_deref().unwrap_or("fts"),
            a.label
                .as_deref()
                .map_or(String::new(), |l| format!("-{l}")),
            caps.tag(),
            part.as_str()
        );
        let prefix = out_prefix(a, p, &stem, point.as_deref())?;
        let (j, m) = write(&prefix, &r)?;
        print!("{}", crate::eval::report::tokens_m6_md(&r));
        println!("  {}\n  {}", j.display(), m.display());
        results.push((point.unwrap_or_else(|| "default".into()), prefix, r));
    }
    let bases = a
        .compare
        .iter()
        .map(|b| Ok((file_name(b), read_result(b)?)))
        .collect::<Result<Vec<_>>>()?;
    for m in compare_all(&results, &bases)? {
        println!("{m}");
    }
    Ok(())
}

/// Плечо с отброшенными ключами у себя или у базы к сравнению не допускается: причина пропуска.
fn skip_reason(base: &McpResult, arm: &McpResult) -> Option<String> {
    let dropped: Vec<&str> = arm
        .info
        .overrides_dropped
        .iter()
        .chain(&base.info.overrides_dropped)
        .map(String::as_str)
        .collect();
    (!dropped.is_empty()).then(|| {
        format!(
            "не сравнивается — сервер не знает части ключей ({})",
            dropped.join(", ")
        )
    })
}

/// Сравнения прогона: точки сетки против первой и каждая точка против баз `--compare`. Точка с
/// отброшенным ключом (у неё или у базы) пропускается с сообщением, цикл не обрывается;
/// сообщения о пропусках — в ответе.
fn compare_all(
    results: &[(String, PathBuf, McpResult)],
    bases: &[(String, McpResult)],
) -> Result<Vec<String>> {
    let mut skipped = Vec::new();
    let mut one = |base: &McpResult, bname: &str, name: &str, prefix: &Path, r: &McpResult| {
        match skip_reason(base, r) {
            Some(why) => {
                skipped.push(format!("точка {name} против {bname}: {why}"));
                Ok(())
            }
            None => compare_and_write(base, r, (bname, name), prefix),
        }
    };
    if let Some((base_name, _, base)) = results.first() {
        for (name, prefix, r) in &results[1..] {
            one(base, base_name, name, prefix, r)?;
        }
    }
    for (bname, base) in bases {
        for (name, prefix, r) in results {
            one(base, bname, name, prefix, r)?;
        }
    }
    Ok(skipped)
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
                read_budget: READ_BUDGET,
                counter: "bge-m3".into(),
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
        assert!(c.d_micro.unwrap().mean.abs() < 1e-12);
        assert_eq!((c.base_micro, c.arm_micro), (0.75, 0.75));
        assert_eq!(
            (c.bootstrap.samples, c.bootstrap.seed),
            (BOOTSTRAP_M6, SEED)
        );
        let other = arm("d", vec![]);
        let e = compare(&base, &other, ("b4", "m6"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("corpus_fingerprint d (плечо) ≠ c (база)"), "{e}");
    }

    /// Условия замера должны совпадать: отпечаток нагетов, часть, бюджет чтения, счётчик,
    /// модель, устройство, без отброшенных переопределений.
    #[test]
    fn compare_rejects_other_conditions() {
        let base = arm("c", vec![scen("a", 1, path(10, 1, true))]);
        type Spoil = fn(&mut McpInfo);
        let cases: [(&str, Spoil); 7] = [
            ("nuggets_fingerprint", |i| {
                i.nuggets_fingerprint = "n2".into()
            }),
            ("часть разбиения", |i| {
                i.split = "holdout".into()
            }),
            ("read_budget", |i| i.read_budget = 1500),
            ("counter", |i| i.counter = "chars/4".into()),
            ("модель", |i| i.model = Some("bge-m3-int8".into())),
            ("устройство", |i| i.device = Some("cuda".into())),
            ("отброшены", |i| {
                i.overrides_dropped = vec!["ZAM_SEARCH__BUDGET_TOKENS".into()]
            }),
        ];
        for (what, f) in cases {
            let mut m6 = base.clone();
            f(&mut m6.info);
            let e = compare(&base, &m6, ("b", "m")).unwrap_err().to_string();
            assert!(e.contains(what), "{what}: {e}");
        }
    }

    /// Без нагетов Δ микро-полноты нет и неинфериорность не утверждается.
    #[test]
    fn compare_without_nuggets_is_not_noninferior() {
        let base = arm("c", vec![scen("a", 0, path(10, 0, true))]);
        let c = compare(&base, &base.clone(), ("b", "m")).unwrap();
        assert!(c.d_micro.is_none() && !c.noninferior);
    }

    /// На holdout сценарий только в одном плече допустим лишь как исключённый.
    #[test]
    fn holdout_requires_same_scenarios() {
        let mut base = arm(
            "c",
            vec![
                scen("a", 1, path(10, 1, true)),
                scen("x", 1, path(5, 1, true)),
            ],
        );
        base.info.split = "holdout".into();
        let mut m6 = arm("c", vec![scen("a", 1, path(8, 1, true))]);
        m6.info.split = "holdout".into();
        let e = compare(&base, &m6, ("b", "m")).unwrap_err().to_string();
        assert!(e.contains("\"x\""), "{e}");
        m6.info.excluded = vec!["x: вид idea не создаётся в B4".into()];
        let c = compare(&base, &m6, ("b", "m")).unwrap();
        assert_eq!(c.only_base, ["x"]);
    }

    /// Сценарий, исключённый в одном плече (B4 без `contradicts`), не входит в пары обоих.
    #[test]
    fn excluded_scenarios_leave_both_arms() {
        let mut b4 = arm(
            "c",
            vec![
                scen("a", 1, path(10, 1, true)),
                scen("s-19", 1, path(100, 0, false)),
            ],
        );
        b4.info.excluded = vec![
            "contradicts p09-h→p09-t: связь не создана".into(),
            "s-19: ожидаемый источник в паре contradicts".into(),
        ];
        let m6 = arm(
            "c",
            vec![
                scen("a", 1, path(8, 1, true)),
                scen("s-19", 1, path(5, 1, true)),
            ],
        );
        let c = compare(&b4, &m6, ("b4", "m6")).unwrap();
        assert_eq!((c.scenarios, c.base_tokens, c.arm_tokens), (1, 10, 8));
        assert_eq!(c.net_losses, 0);
    }

    /// Пары `contradicts`, которых нет у сервера, и сценарии с insight пары — в `info.excluded`.
    #[test]
    fn excluded_by_missing_contradicts_edges() {
        let sc = |id: &str, expect: &[&str]| Scenario {
            id: id.into(),
            kind: None,
            question: "?".into(),
            expect: expect.iter().map(|s| s.to_string()).collect(),
            nuggets: Vec::new(),
        };
        let (s19, s20, s1) = (
            sc("s-19", &["insight:p09-t"]),
            sc("s-20", &["insight:p12-a", "insight:p02-t"]),
            sc("s-01", &["ru/windows/crlf-bash-scripts"]),
        );
        let skipped = vec![
            ("p09-h".to_string(), "p09-t".to_string()),
            ("p02-h".to_string(), "p02-t".to_string()),
        ];
        let ex = excluded_by_edges(&[&s19, &s20, &s1], &skipped, "0.0.5-m4");
        assert_eq!(ex.len(), 4, "{ex:?}");
        assert!(ex[0].starts_with("contradicts p09-h→p09-t:"), "{ex:?}");
        assert!(ex[2].starts_with("s-19: ") && ex[2].contains("p09-h→p09-t"));
        assert!(ex[3].starts_with("s-20: ") && ex[3].contains("p02-h→p02-t"));
        assert!(excluded_by_edges(&[&s19], &[], "0.0.10-m5.7").is_empty());
    }

    /// Точка сетки с отброшенным ключом пропускается с сообщением, остальные сравниваются, в том
    /// числе с базами `--compare` после неё (цикл не обрывается).
    #[test]
    fn compare_all_skips_points_with_dropped_keys() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("unit-tmp")
            .join("compare-all");
        let _ = fs::remove_dir_all(&dir);
        let sc = || vec![scen("a", 1, path(10, 1, true))];
        let mut bad = arm("c", sc());
        bad.info.overrides_dropped = vec!["ZAM_SEARCH__BUDGET_TOKENS".into()];
        let results = vec![
            ("p0".to_string(), dir.join("p0"), arm("c", sc())),
            ("p1".to_string(), dir.join("p1"), bad),
            ("p2".to_string(), dir.join("p2"), arm("c", sc())),
        ];
        let bases = vec![("b4".to_string(), arm("c", sc()))];
        let skipped = compare_all(&results, &bases).unwrap();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(
            skipped.iter().all(|m| m.starts_with("точка p1 против")),
            "{skipped:?}"
        );
        for f in ["p2-vs-p0.json", "p0-vs-b4.json", "p2-vs-b4.json"] {
            assert!(dir.join(f).is_file(), "{f}");
        }
        assert!(!dir.join("p1-vs-p0.json").exists() && !dir.join("p1-vs-b4.json").exists());
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

    /// Выжимка кэша первой строкой и ссылка выжимки из ответа `memory_cache_put`.
    #[test]
    fn cache_marks() {
        assert!(cache_first("\n[cache] c_X «вопрос»\n### n:..."));
        assert!(!cache_first("### n:x\n[cache] ниже\n"));
        let id = format!("c_{}", "0".repeat(10) + &"A".repeat(16));
        let put = format!("Выжимка {id} сохранена: «q» · 12 tok · источников 1 · valid\n~20 tok");
        assert_eq!(cache_ref(&put), Some(id));
        assert_eq!(cache_ref("Выжимка ? сохранена"), None);
    }

    /// `--out` с разделителем на конце — каталог (создаётся), без — префикс пути.
    #[test]
    fn out_dir_or_prefix() {
        assert!(out_is_dir(Path::new("x/")) && out_is_dir(Path::new("x\\")));
        assert!(!out_is_dir(Path::new("no-such-dir-zz/run")));
        assert_eq!(prefix_of(Path::new("d/a.json")), Path::new("d/a"));
    }
}
