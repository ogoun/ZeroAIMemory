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

/// Имя файла результата: `<дата>-<модель|fts>-<режим>[-<метка>]`.
pub fn file_stem(r: &RunResult) -> String {
    // FTS-only без модели: `<дата>-fts`
    let mut s = match &r.info.model {
        Some(m) => format!("{}-{m}-{}", r.info.date, r.info.mode.file_tag()),
        None => format!("{}-fts", r.info.date),
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
