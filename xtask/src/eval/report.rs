//! Отчёты eval: JSON (полные данные, по запросам) и Markdown (таблицы для людей) в
//! `eval/results/`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use super::corpus::Kind;
use super::metrics::{Aggregate, Estimate};
use super::run::{Breakdown, QueryRun, RunResult, TOP_K};

/// `0.812 [0.75–0.86]`.
pub fn est(e: &Estimate) -> String {
    // + 0.0: без «-0.000»
    format!("{:.3} [{:.3}–{:.3}]", e.mean + 0.0, e.lo + 0.0, e.hi + 0.0)
}

/// Строка таблицы метрик.
pub fn row(name: &str, a: &Aggregate) -> String {
    format!(
        "| {name} | {} | {} | {} | {} |",
        a.n,
        est(&a.recall5),
        est(&a.mrr10),
        est(&a.ndcg10)
    )
}

pub const TABLE_HEAD: &str =
    "| Запросы | n | recall@5 | MRR@10 | nDCG@10 |\n|---|---:|---|---|---|";

/// Таблица метрик по группам.
pub fn metrics_table(m: &Breakdown) -> String {
    let mut s = String::from(TABLE_HEAD);
    s.push('\n');
    s.push_str(&row("все", &m.overall));
    for (k, a) in &m.by_kind {
        let _ = write!(s, "\n{}", row(k, a));
    }
    if m.disjoint.n > 0 {
        let _ = write!(s, "\n{}", row("перефраз без общих слов", &m.disjoint));
    }
    if m.long.n > 0 {
        let _ = write!(s, "\n{}", row("длинные заметки", &m.long));
    }
    if m.fts_only.n > 0 {
        let _ = write!(s, "\n{}", row("FTS-only (index: false)", &m.fts_only));
    }
    for (l, a) in &m.by_lang {
        let _ = write!(s, "\n{}", row(&format!("запрос {l}"), a));
    }
    s
}

/// Таблица метрик M3 (устаревание, надёжность, листы длинных заметок); пусто — в наборе
/// нет запросов M3.
pub fn m3_table(r: &RunResult) -> String {
    let m = &r.metrics.overall;
    let n = |f: fn(&QueryRun) -> bool| r.queries.iter().filter(|q| f(q)).count();
    let rows: [(&str, usize, Option<Estimate>); 7] = [
        (
            "cancelled@5 (цель 0)",
            n(|q| q.metrics.cancelled5.is_some()),
            m.cancelled5,
        ),
        (
            "cancelled@5 с include_history (контроль, > 0)",
            n(|q| q.cancelled5_history.is_some()),
            r.metrics.cancelled5_history,
        ),
        (
            "reliable_first, бусты certainty сервера",
            n(|q| q.metrics.reliable_first.is_some()),
            m.reliable_first,
        ),
        (
            "reliable_first без бустов",
            n(|q| q.reliable_first_no_boost.is_some()),
            r.metrics.reliable_first_no_boost,
        ),
        (
            "chunk-recall@5 (свой текст листа)",
            n(|q| q.metrics.chunk_recall5.is_some()),
            m.chunk_recall5,
        ),
        (
            "chunk-nDCG@10",
            n(|q| q.metrics.chunk_ndcg10.is_some()),
            m.chunk_ndcg10,
        ),
        (
            "chunk-recall@5 с перекрытием (справочно)",
            n(|q| q.kind == Kind::Chunk),
            r.metrics.chunk_recall5_with_overlap,
        ),
    ];
    let mut s = String::new();
    for (name, count, e) in rows {
        if let Some(e) = e {
            if s.is_empty() {
                s.push_str("| Метрика M3 | n | значение |\n|---|---:|---|");
            }
            let _ = write!(s, "\n| {name} | {count} | {} |", est(&e));
        }
    }
    s
}

/// `+0.012 [-0.004…+0.030]`.
pub fn diff(e: &Estimate) -> String {
    format!(
        "{:+.3} [{:+.3}…{:+.3}]",
        e.mean + 0.0,
        e.lo + 0.0,
        e.hi + 0.0
    )
}

/// Доли каналов найденных заметок.
pub fn channels_line(m: &Breakdown) -> String {
    let parts: Vec<String> = m
        .channels
        .iter()
        .map(|(k, v)| format!("{k} {:.0} %", v * 100.0))
        .collect();
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(", ")
    }
}

/// Имя файла результата: `<дата>[-<префикс>]-<модель|fts>-<режим>[-<метка>]`.
pub fn file_stem(r: &RunResult) -> String {
    // FTS-only без модели: `<дата>-fts`; `--prefix` — сразу после даты
    let date = match &r.info.prefix {
        Some(p) => format!("{}-{p}", r.info.date),
        None => r.info.date.clone(),
    };
    let mut s = match &r.info.model {
        Some(m) => format!("{date}-{m}-{}", r.info.mode.file_tag()),
        None => format!("{date}-fts"),
    };
    if let Some(l) = &r.info.label {
        s.push('-');
        s.push_str(l);
    }
    s
}

/// Компактный JSON (одна строка): результаты прогонов с поэлементными запросами — сотни
/// КБ, отступы удваивали размер `eval/results/` (прежние файлы не переписываются).
fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(path, serde_json::to_string(v)? + "\n")?;
    Ok(())
}

/// Пишет `<stem>.json` и `<stem>.md`.
pub fn write_run(dir: &Path, r: &RunResult) -> Result<(PathBuf, PathBuf)> {
    write_run_as(dir, &file_stem(r), r)
}

/// Пишет `<stem>.json` и `<stem>.md` с заданным именем (итоговые прогоны M4.5).
pub fn write_run_as(dir: &Path, stem: &str, r: &RunResult) -> Result<(PathBuf, PathBuf)> {
    let json = dir.join(format!("{stem}.json"));
    let md = dir.join(format!("{stem}.md"));
    write_json(&json, r)?;
    fs::write(&md, run_md(r))?;
    Ok((json, md))
}

/// Markdown прогона.
pub fn run_md(r: &RunResult) -> String {
    let i = &r.info;
    let mut s = format!(
        "# Eval: {}, {}\n\n",
        i.mode.as_str(),
        i.model.as_deref().unwrap_or("без моделей (FTS-only)")
    );
    let _ = writeln!(s, "- дата {}, коммит `{}`", i.date, i.git);
    let _ = writeln!(
        s,
        "- корпус: {} заметок (отпечаток `{}`), запросов {} (отпечаток `{}`)",
        i.notes, i.corpus_fingerprint, i.queries, i.queries_fingerprint
    );
    if let Some(m) = &i.model_id {
        let _ = writeln!(
            s,
            "- модель `{m}`, устройство {} ({})",
            i.device.as_deref().unwrap_or("?"),
            i.device_reason.as_deref().unwrap_or("")
        );
    }
    if let Some(rr) = &i.reranker {
        let _ = writeln!(s, "- реранкер `{rr}`");
    }
    if !i.sets.is_empty() {
        let sets: Vec<String> = i.sets.iter().map(|(k, v)| format!("`{k}={v}`")).collect();
        let _ = writeln!(s, "- конфигурация: {}", sets.join(", "));
    }
    let _ = writeln!(
        s,
        "- действующие `[search]` сервера: `{}`; `[index]`: `{}`",
        i.server_config["search"], i.server_config["index"]
    );
    if i.index_secs > 0.0 {
        let _ = writeln!(s, "- индексация корпуса: {:.1} с", i.index_secs);
    }
    let _ = writeln!(
        s,
        "- время запроса (клиент): p50 {:.0} мс, p95 {:.0} мс",
        i.query_ms_p50, i.query_ms_p95
    );
    let _ = writeln!(
        s,
        "\nВ скобках — 95 % интервал бутстрепа по запросам (eval/README.md).\n"
    );
    s.push_str(&metrics_table(&r.metrics));
    let _ = writeln!(
        s,
        "\n\nКаналы, которыми найдены ожидаемые заметки в top-{TOP_K}: {}.",
        channels_line(&r.metrics)
    );
    if let Some(sp) = &r.split {
        let _ = writeln!(
            s,
            "\nРазрезано границами листов доказательств запросов `chunk`: {} из {} (из них целиком в \
             листе вместе с перекрытием — {}); листов длинных заметок {}. Потолок chunk-recall@5 — \
             {:.3}.",
            sp.split,
            sp.queries,
            sp.split_whole_with_overlap,
            sp.long_leaves,
            1.0 - sp.split as f64 / sp.queries.max(1) as f64
        );
    }
    let m3 = m3_table(r);
    if !m3.is_empty() {
        let _ = writeln!(
            s,
            "\n## Метрики M3\n\ncancelled@5 — доля запросов `superseded` с замещённым или \
             отозванным insight в top-5; reliable_first — доля запросов `certainty`, где \
             theorem/axiom выше противоречащей hypothesis; chunk-* — по листам `mode = chunks` \
             (лист целиком содержит `evidence`, перекрытие с предыдущим листом не \
             считается).\n\n{m3}"
        );
    }
    if let Some(c) = &r.compare {
        let _ = writeln!(
            s,
            "\n## Сравнение с прежним прогоном\n\n`{}` (запросы `{}`, корпус `{}`): общих \
             запросов {}.\n\n| Разность (новый − прежний) | n | значение |\n|---|---:|---|\n\
             | Δ recall@5 | {} | {} |\n| Δ nDCG@10 | {} | {} |\n\
             | Δ nDCG@10 без группы e | {} | {} |\n\
             | Δ nDCG@10 без новых целей корпуса | {} | {} |\n\n\
             Общий запрос — тот же id, вид, `expect`{}; метрики нового прогона — по разметке \
             прежнего (`expect`; с `also`, если прежний прогон его знает — тогда и `also` \
             тот же). «Без новых целей» — из выдачи убраны цели, которых не было в корпусе \
             прежнего коммита (заметки его `eval/corpus` и insight его `eval/insights.toml` \
             остаются).\n\n{}",
            c.against,
            c.against_queries_fingerprint,
            c.against_corpus_fingerprint,
            c.n,
            c.n,
            diff(&c.d_recall5),
            c.n,
            diff(&c.d_ndcg10),
            c.n_without_e,
            diff(&c.d_ndcg10_without_e),
            c.n,
            c.d_ndcg10_without_new.as_ref().map_or("—".into(), diff),
            if c.text_checked {
                " и текст"
            } else {
                " (текст прежнего прогона не найден — не сверялся)"
            },
            if c.regression {
                "**Регрессия:** интервал Δ nDCG@10 целиком ниже нуля."
            } else {
                "Интервал Δ nDCG@10 не лежит целиком ниже нуля — регрессии нет."
            }
        );
    }
    let misses: Vec<_> = r
        .queries
        .iter()
        .filter(|q| q.metrics.mrr10 == 0.0)
        .collect();
    let _ = writeln!(
        s,
        "\n## Промахи: ожидаемых заметок нет в top-{TOP_K} ({})\n",
        misses.len()
    );
    for q in misses {
        let top: Vec<&str> = q.hits.iter().take(3).map(|h| h.path.as_str()).collect();
        let _ = writeln!(
            s,
            "- `{}` ({}{}): ожидалось {}; top-3: {}",
            q.id,
            q.kind,
            if q.disjoint {
                ", без общих слов"
            } else {
                ""
            },
            q.expect.join(", "),
            if top.is_empty() {
                "пусто".to_string()
            } else {
                top.join(", ")
            }
        );
    }
    s
}

/// Краткая сводка в консоль.
pub fn print_run(r: &RunResult) {
    let m = &r.metrics.overall;
    let opt = |e: Option<Estimate>| e.map_or("—".to_string(), |e| est(&e));
    println!(
        "  M3: cancelled@5 {}, с историей {}, reliable_first {} (без бустов {}), \
         chunk-recall@5 {}, chunk-nDCG@10 {}",
        opt(m.cancelled5),
        opt(r.metrics.cancelled5_history),
        opt(m.reliable_first),
        opt(r.metrics.reliable_first_no_boost),
        opt(m.chunk_recall5),
        opt(m.chunk_ndcg10)
    );
    println!(
        "{} {}: recall@5 {}, MRR@10 {}, nDCG@10 {} (n = {}); без общих слов nDCG@10 {}",
        r.info.model.as_deref().unwrap_or("fts"),
        r.info.mode.as_str(),
        est(&m.recall5),
        est(&m.mrr10),
        est(&m.ndcg10),
        m.n,
        est(&r.metrics.disjoint.ndcg10)
    );
}

/// Пишет JSON и Markdown по имени `stem`.
pub fn write_pair<T: Serialize>(
    dir: &Path,
    stem: &str,
    v: &T,
    md: &str,
) -> Result<(PathBuf, PathBuf)> {
    let json = dir.join(format!("{stem}.json"));
    let mdp = dir.join(format!("{stem}.md"));
    write_json(&json, v)?;
    fs::write(&mdp, md)?;
    Ok((json, mdp))
}

/// Markdown плеча замера M6 (`eval --tokens --via mcp`, спецификация M6 §3.6).
pub fn tokens_m6_md(r: &super::tokens::mcp::McpResult) -> String {
    let i = &r.info;
    let u = &r.summary;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# Замер токенов M6 (ТС-1, MCP): {} {} — {}{} ({})\n",
        i.model.as_deref().unwrap_or("fts"),
        i.device.as_deref().unwrap_or("—"),
        i.split,
        i.point
            .as_deref()
            .map_or(String::new(), |p| format!(", точка {p}")),
        i.date
    );
    let _ = writeln!(
        s,
        "Коммит `{}`, сервер {}{} (git `{}`); отпечатки: корпус `{}`, нагеты `{}`{}; чтение — \
         бюджет {}; токены — {}.\n",
        i.git,
        i.server_version,
        if i.server_m6 {
            format!(" (сборка M6, features {:?})", i.server_features)
        } else {
            String::new()
        },
        i.server_git.as_deref().unwrap_or("—"),
        i.corpus_fingerprint,
        i.nuggets_fingerprint,
        if i.frozen {
            " (заморожены)"
        } else {
            " (замка нет)"
        },
        i.read_budget,
        i.counter
    );
    if !i.overrides.is_empty() || !i.overrides_dropped.is_empty() {
        let kept: Vec<String> = i
            .overrides
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let _ = writeln!(
            s,
            "Переопределения: {}; отброшены (нет в версии сервера; к сравнению плечо не \
             допускается): {}.\n",
            if kept.is_empty() {
                "—".into()
            } else {
                kept.join(", ")
            },
            if i.overrides_dropped.is_empty() {
                "—".into()
            } else {
                i.overrides_dropped.join(", ")
            }
        );
    }
    if !i.excluded.is_empty() {
        let _ = writeln!(s, "Не созданы у сервера: {}.\n", i.excluded.join("; "));
    }
    if !i.excluded_scenarios.is_empty() {
        let _ = writeln!(
            s,
            "Исключено из сравнения всех плеч: {}.\n",
            i.excluded_scenarios.join("; ")
        );
    }
    if !i.l0_excluded.is_empty() {
        let l: Vec<String> = i
            .l0_excluded
            .iter()
            .map(|e| format!("{}#{} {}", e.scenario, e.nugget, e.source))
            .collect();
        let _ = writeln!(
            s,
            "Нагеты в L0 сервера (вне полноты; в сравнении — обоих плеч): {}.\n",
            l.join("; ")
        );
    }
    let _ = writeln!(
        s,
        "| Путь | токенов всего | в среднем на сценарий | микро-полнота | полных |\n|---|---:|---|---:|---:|"
    );
    let mean = |e: &Option<Estimate>| {
        e.as_ref().map_or("—".into(), |e| {
            format!("{:.0} [{:.0}–{:.0}]", e.mean, e.lo, e.hi)
        })
    };
    for (name, p) in [
        (
            "(а) протокол: brief → read → дочитывание → cache put",
            &u.protocol,
        ),
        ("справочно: карточки 1–3 без оракула", &u.reference),
        ("(в) наивный: chunks, бюджет 4000", &u.naive),
    ] {
        let _ = writeln!(
            s,
            "| {name} | {} | {} | {:.3} | {} из {} |",
            p.tokens_total,
            mean(&p.tokens_mean),
            p.micro_recall,
            p.full,
            u.scenarios
        );
    }
    let _ = writeln!(
        s,
        "| (б) повтор: кэш | {} | — | — | {} из {} |",
        u.cached.tokens_total, u.cached.full, u.scenarios
    );
    if let Some(p) = &u.focus {
        let _ = writeln!(
            s,
            "| справочно: явный фокус (`~T` > `read.focus_min_tokens`) | {} | {} | {:.3} | {} из {} |",
            p.tokens_total,
            mean(&p.tokens_mean),
            p.micro_recall,
            p.full,
            u.scenarios
        );
    }
    let _ = writeln!(
        s,
        "\nСценариев {}, нагетов {}; (а) / (в): {}; выжимки агента (вывод, не ответ сервера): {} \
         токенов; чужих попаданий в кэш (первая строка (а) или (в) — выжимка): {}.\n",
        u.scenarios,
        u.nuggets_total,
        u.protocol_to_naive.as_ref().map_or("—".into(), est),
        u.summary_tokens_total,
        u.foreign_cache_hits
    );
    let _ = writeln!(
        s,
        "| id | вид | (а) токены | нагеты | дочитано | справочно | (в) | примечание |\n|---|---|---:|---|---|---:|---:|---|"
    );
    for x in &r.scenarios {
        let note: Vec<&str> = [
            &x.protocol.note,
            &x.reference.note,
            &x.naive.note,
            &x.cached.note,
        ]
        .into_iter()
        .filter(|n| !n.is_empty())
        .map(String::as_str)
        .collect();
        let _ = writeln!(
            s,
            "| {} | {} | {} | {}/{} | {} | {} | {} | {} |",
            x.id,
            x.kind,
            x.protocol.tokens,
            x.protocol.found,
            x.nuggets_total,
            x.protocol.reread.len(),
            x.reference.tokens,
            x.naive.tokens,
            note.join("; ")
        );
    }
    s
}

/// Markdown сравнения плеча с базой (спецификация M6 §3.5).
pub fn comparison_md(c: &super::tokens::mcp::Comparison) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\n## {} против {} ({}, сценариев {})\n",
        c.arm, c.base, c.split, c.scenarios
    );
    let _ = writeln!(
        s,
        "- R = Σ плеча / Σ базы = {} / {}: {} (95 %, парный бутстреп сценариев);",
        c.arm_tokens,
        c.base_tokens,
        c.r.as_ref().map_or("—".into(), est)
    );
    let _ = writeln!(
        s,
        "- микро-полнота: база {:.3}, плечо {:.3}; Δ {} (односторонние 95 % границы; порог \
         нижней ≥ {});",
        c.base_micro,
        c.arm_micro,
        c.d_micro.as_ref().map_or("— (нагетов нет)".into(), est),
        super::tokens::mcp::MIN_D_MICRO
    );
    let _ = writeln!(
        s,
        "- чистые потери L = {} (порог ≤ {}); неинфериорность: {}.",
        c.net_losses,
        super::tokens::mcp::MAX_NET_LOSSES,
        if c.noninferior { "да" } else { "нет" }
    );
    let _ = writeln!(
        s,
        "- бутстреп: {} выборок, зерно {:#x}.",
        c.bootstrap.samples, c.bootstrap.seed
    );
    if !c.only_base.is_empty() || !c.only_arm.is_empty() {
        let _ = writeln!(
            s,
            "- вне сравнения: только в базе {:?}, только в плече {:?}.",
            c.only_base, c.only_arm
        );
    }
    if !c.l0_excluded.is_empty() {
        let l: Vec<String> = c
            .l0_excluded
            .iter()
            .map(|e| format!("{}#{} {}", e.scenario, e.nugget, e.source))
            .collect();
        let _ = writeln!(
            s,
            "- нагеты в L0 сервера хотя бы одного плеча — вне полноты обоих: {}.",
            l.join("; ")
        );
    }
    s
}

/// `found/total (0.750)`.
fn tally(t: &super::sessions::Tally) -> String {
    match t.share() {
        Some(x) => format!("{}/{} ({x:.3})", t.found, t.total),
        None => "—".into(),
    }
}

/// Строки сводки ТС-2 (общие у плеча и сравнения).
fn sessions_summary_md(s: &mut String, name: &str, x: &super::sessions::SessionsSummary) {
    let kv = |m: &std::collections::BTreeMap<String, usize>| -> String {
        if m.is_empty() {
            return "0".into();
        }
        m.iter()
            .map(|(l, n)| format!("{l} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(
        s,
        "- **{name}**: сессий {}{}, токенов {} ({}); обязательные строки {}; нагеты вопросов {}; \
         дайджест при двух стартах {}",
        x.sessions,
        if x.reps > 1 {
            format!(" × {} повторов", x.reps)
        } else {
            String::new()
        },
        x.tokens_total,
        kv(&x.tokens_by_label),
        tally(&x.required),
        tally(&x.nuggets),
        tally(&x.digest_match)
    );
    let kinds: Vec<String> = x
        .hint_by_kind
        .iter()
        .map(|(k, t)| {
            let st = x.hint_by_kind_strict.get(k).copied().unwrap_or_default();
            format!("{k} {} (строго {})", tally(t), tally(&st))
        })
        .collect();
    let _ = writeln!(
        s,
        "  - hint_recall общий {} (строго в событии {}); подсказки-идеи {} (строго {}); по видам: \
         {}; зачёт: {}",
        tally(&x.hint_recall),
        tally(&x.hint_recall_strict),
        tally(&x.idea_hint_recall),
        tally(&x.idea_hint_recall_strict),
        kinds.join("; "),
        kv(&x.hint_credit)
    );
    let _ = writeln!(
        s,
        "  - повторная выдача {} (между каналами {}); идеи: {}; снимок сессии: {}{}{}",
        tally(&x.repeated),
        tally(&x.repeated_cross),
        kv(&x.idea_tokens),
        kv(&x.snapshot_tokens),
        x.rep_noise_p95.map_or(String::new(), |p| format!(
            "; шум повторов (p95 |Δ|/среднее сессии): {p:.4}"
        )),
        if x.bad_outputs > 0 || x.hook_budget_misses > 0 {
            format!(
                "; неразобранных выводов хука: {}, хуков сверх бюджета: {}",
                x.bad_outputs, x.hook_budget_misses
            )
        } else {
            String::new()
        }
    );
    if x.required_by_rep.len() > 1 {
        let worst = x
            .required_by_rep
            .iter()
            .min_by(|a, b| {
                let (sa, sb) = (a.1.share().unwrap_or(1.0), b.1.share().unwrap_or(1.0));
                sa.total_cmp(&sb)
            })
            .map(|(r, t)| format!("r{r} {}", tally(t)))
            .unwrap_or_default();
        let _ = writeln!(s, "  - обязательные строки, худший повтор: {worst}");
    }
    if !x.reps_with_misses.is_empty() {
        let _ = writeln!(
            s,
            "  - повторы с промахом бюджета хука или неразобранным выводом (действительны; блокируют \
             приёмку 13b): {}",
            x.reps_with_misses.join(", ")
        );
    }
    if !x.required_missing.is_empty() {
        let _ = writeln!(s, "  - нет строк: {}", x.required_missing.join("; "));
    }
}

/// Отчёт плеча ТС-2 (спецификация M6 §3.6).
pub fn sessions_md(r: &super::sessions::SessionsResult) -> String {
    let i = &r.info;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# ТС-2 «сессия» — {} ({}, сервер {}, {}, идеи {}, MCP {})\n",
        i.date,
        i.split,
        i.server_version,
        i.model.as_deref().unwrap_or("fts"),
        if i.ideas_flag { "on" } else { "off" },
        i.mcp
    );
    let _ = writeln!(s, "- возможности: {:?}", i.feats);
    let _ = writeln!(
        s,
        "- не воспроизводятся у сервера: {:?}; классы строк, которых нет: {:?}; виды \
         обязательных строк, которых нет: {:?}",
        i.exclusions.labels, i.exclusions.classes, i.exclusions.required
    );
    for n in &i.notes {
        let _ = writeln!(s, "- {n}");
    }
    sessions_summary_md(&mut s, "итог", &r.summary);
    let _ = writeln!(
        s,
        "\n| Сессия | профиль | токенов | обязательные | hint общий | идеи | нагеты | дайджест = |\n\
         |---|---|---:|---|---|---|---|---|"
    );
    for x in &r.sessions {
        let one = super::sessions::summarize(
            std::slice::from_ref(x),
            &super::sessions::own_exclusions(&r.info.feats),
        );
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            x.id,
            x.profile,
            one.tokens_total,
            tally(&one.required),
            tally(&one.hint_recall),
            tally(&one.idea_hint_recall),
            tally(&one.nuggets),
            match x.digest_match {
                Some(true) => "да",
                Some(false) => "нет",
                None => "—",
            }
        );
    }
    let notes: Vec<String> = r
        .sessions
        .iter()
        .flat_map(|x| x.notes.iter().map(move |n| format!("{}: {n}", x.id)))
        .collect();
    if !notes.is_empty() {
        let _ = writeln!(s, "\nПримечания:");
        for n in notes {
            let _ = writeln!(s, "- {n}");
        }
    }
    s
}

/// Сравнение плеч ТС-2 на пересечении событий (§3.3): весь набор и профили.
pub fn sessions_comparison_md(c: &super::sessions::SessionsComparison) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\n## ТС-2: {} ({}) против {} ({})\n",
        c.arm_name, c.arm_server, c.base_name, c.base_server
    );
    let _ = writeln!(
        s,
        "- исключено из обоих плеч: события {:?}; классы строк {:?}; обязательные строки {:?}; \
         ожидания {:?}",
        c.exclusions.labels, c.exclusions.classes, c.exclusions.required, c.exclusions.expect
    );
    let d = |e: &Option<super::metrics::Estimate>| e.as_ref().map_or("—".into(), est);
    for p in &c.parts {
        let _ = writeln!(
            s,
            "\n### {} (сессий {})\n\n| Событие | база | плечо | R |\n|---|---:|---:|---|",
            p.part,
            p.sessions.len()
        );
        for l in p.by_label.iter().chain(std::iter::once(&p.total)) {
            let _ = writeln!(
                s,
                "| {} | {:.0} | {:.0} | {} |",
                l.label,
                l.base,
                l.arm,
                l.r.as_ref().map_or("—".into(), est)
            );
        }
        let _ = writeln!(
            s,
            "\n- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние \
             95 %): hint_recall {}, строго {}, обязательные строки {}",
            d(&p.d_hint_recall),
            d(&p.d_hint_recall_strict),
            d(&p.d_required)
        );
        sessions_summary_md(&mut s, "база", &p.base_summary);
        sessions_summary_md(&mut s, "плечо", &p.arm_summary);
    }
    let _ = writeln!(s);
    if let Some(m) = &c.idea_hints_match {
        let _ = writeln!(s, "- подсказки-идеи prompt совпали с базой: {}", tally(m));
    }
    if let Some(p) = c.abs_rel_diff_p95 {
        let _ = writeln!(
            s,
            "- |Δ| / база токенов сессии, 95-й перцентиль: {p:.4}{}",
            if c.same_server {
                " — порог шума A/A (плечи одной версии)"
            } else {
                ""
            }
        );
    }
    let _ = writeln!(
        s,
        "- бутстреп: {} выборок, зерно {:#x} (парный по сессиям; токены сессии — среднее по \
         повторам).",
        c.bootstrap_samples, c.bootstrap_seed
    );
    if !c.only_base.is_empty() || !c.only_arm.is_empty() {
        let _ = writeln!(
            s,
            "- вне сравнения: только в базе {:?}, только в плече {:?}.",
            c.only_base, c.only_arm
        );
    }
    s
}
