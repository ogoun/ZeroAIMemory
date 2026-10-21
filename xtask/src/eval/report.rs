//! Отчёты eval: JSON (полные данные, по запросам) и Markdown (таблицы для людей) в
//! `eval/results/`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use super::metrics::{Aggregate, Estimate};
use super::run::{Breakdown, RunResult, TOP_K};

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
    for (l, a) in &m.by_lang {
        let _ = write!(s, "\n{}", row(&format!("запрос {l}"), a));
    }
    s
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
    let stem = file_stem(r);
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
