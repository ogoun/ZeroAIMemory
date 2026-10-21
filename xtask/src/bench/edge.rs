//! `cargo xtask bench --edge` — managed Qdrant против встроенного Qdrant Edge (задача 16 M2,
//! `docs/adr/0001-qdrant-edge.md`).
//!
//! Замеряет то, что решает ADR (критерий исследования 05: «не переходить до 1.0, если p95
//! выигрывает меньше 2× или RSS выше»):
//! - время чистой release-сборки и размер исполняемого файла без Edge и с Edge (пример
//!   `zam-index/examples/index_bench`, профиль release поставки: thin LTO, 1 codegen unit);
//! - число крейтов в дереве `zam-index` без feature `edge` и с ней (`cargo tree`);
//! - полную пересборку индекса (1 млн × 1024 по умолчанию): upsert пачками по 256 до 8
//!   одновременно, затем ожидание конца индексации (HNSW);
//! - p50/p95/p99 гибридного запроса (dense + sparse + dense по заметкам, RRF) в `--runs`
//!   отдельных процессах, RSS после открытия, после запросов и пиковый (managed — сумма
//!   процесса Qdrant и клиента), совпадение выдачи вариантов.
//!
//! Каждый замер — отдельный процесс `index_bench` (своя сборка на вариант: managed — без
//! Edge в бинарнике). Данные — `target/bench-edge/`, после прогона удаляются (кроме `--keep`);
//! итог — `eval/results/<дата>-bench-edge[-метка].{json,md}`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::BenchArgs;
use crate::eval::run::{git_rev, qdrant_bin, today};

/// Варианты: имя и feature сборки.
const VARIANTS: [(&str, Option<&str>); 2] = [("managed", None), ("edge", Some("edge"))];

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "index_bench.exe"
    } else {
        "index_bench"
    }
}

fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// Чистая release-сборка примера в своём каталоге: (секунды, путь к exe, размер).
fn build(
    repo: &Path,
    target: &Path,
    feature: Option<&str>,
    clean: bool,
) -> Result<(Option<f64>, PathBuf, u64)> {
    if clean && target.exists() {
        fs::remove_dir_all(target).with_context(|| format!("очистка {}", target.display()))?;
    }
    let mut cmd = Command::new(cargo());
    cmd.current_dir(repo)
        .args([
            "build",
            "--release",
            "-p",
            "zam-index",
            "--example",
            "index_bench",
        ])
        .env("CARGO_TARGET_DIR", target);
    if let Some(f) = feature {
        cmd.args(["--features", f]);
    }
    println!(
        "сборка index_bench ({}) в {}",
        feature.unwrap_or("без Edge"),
        target.display()
    );
    let t = Instant::now();
    let st = cmd.status().context("cargo build")?;
    if !st.success() {
        bail!("сборка index_bench не удалась");
    }
    let secs = t.elapsed().as_secs_f64();
    let exe = target.join("release").join("examples").join(exe_name());
    let size = fs::metadata(&exe)
        .with_context(|| format!("нет {}", exe.display()))?
        .len();
    Ok((clean.then_some(secs), exe, size))
}

/// Число уникальных пакетов в дереве `zam-index` (рёбра normal и build).
fn crate_count(repo: &Path, feature: Option<&str>, all_targets: bool) -> Result<usize> {
    let mut cmd = Command::new(cargo());
    cmd.current_dir(repo).args([
        "tree",
        "-p",
        "zam-index",
        "-e",
        "normal,build",
        "--prefix",
        "none",
        "-f",
        "{p}",
    ]);
    if let Some(f) = feature {
        cmd.args(["--features", f]);
    }
    if all_targets {
        cmd.args(["--target", "all"]);
    }
    let out = cmd.output().context("cargo tree")?;
    if !out.status.success() {
        bail!("cargo tree: {}", String::from_utf8_lossy(&out.stderr));
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end_matches(" (*)").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    lines.sort();
    lines.dedup();
    Ok(lines.len())
}

/// Запуск `index_bench <cmd>`: stderr — в консоль (ход замера), stdout — строка JSON.
fn run(exe: &Path, cmd: &str, args: &[(&str, String)]) -> Result<Value> {
    let mut c = Command::new(exe);
    c.arg(cmd)
        .env("ZAM_LOG", "warn,qdrant_edge=error")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (k, v) in args {
        c.arg(format!("--{k}")).arg(v);
    }
    let out = c
        .output()
        .with_context(|| format!("запуск {}", exe.display()))?;
    if !out.status.success() {
        bail!("index_bench {cmd}: код {:?}", out.status.code());
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .context("index_bench без JSON")?;
    Ok(serde_json::from_str(line)?)
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Медиана и размах значения `get` по прогонам.
fn spread(runs: &[Value], get: impl Fn(&Value) -> Option<f64>) -> Value {
    let v: Vec<f64> = runs.iter().filter_map(&get).collect();
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    json!({"median": median(v.clone()), "min": min, "max": max, "n": v.len()})
}

/// Доля общих chunk_id в топ-10 выдач двух вариантов (по сохранённым первым запросам).
fn agreement(a: &Value, b: &Value) -> Option<f64> {
    let (a, b) = (a["hits"].as_array()?, b["hits"].as_array()?);
    let mut sum = 0.0;
    let mut n = 0usize;
    for (x, y) in a.iter().zip(b) {
        let x: Vec<&str> = x.as_array()?.iter().filter_map(Value::as_str).collect();
        let y: Vec<&str> = y.as_array()?.iter().filter_map(Value::as_str).collect();
        let common = x.iter().filter(|c| y.contains(c)).count();
        sum += common as f64 / x.len().max(y.len()).max(1) as f64;
        n += 1;
    }
    (n > 0).then(|| sum / n as f64)
}

/// Точка входа `cargo xtask bench --edge`.
pub fn main(a: &BenchArgs, repo: &Path) -> Result<()> {
    let work = repo.join("target").join("bench-edge");
    fs::create_dir_all(&work)?;
    let qdrant = qdrant_bin(repo);
    if !qdrant.is_file() {
        bail!("нет {} (cargo xtask fetch-deps)", qdrant.display());
    }
    let tokenizer = repo
        .join("target")
        .join("models")
        .join("bge-m3-int8")
        .join("tokenizer.json");
    if !tokenizer.is_file() {
        bail!("нет {} (zam-server models fetch)", tokenizer.display());
    }
    let corpus = repo.join("eval").join("corpus");
    let points = a.points.to_string();

    // 1. Сборки, размеры, дерево крейтов.
    let mut builds = serde_json::Map::new();
    let mut exes = Vec::new();
    for (name, feature) in VARIANTS {
        let target = work.join(format!("build-{name}"));
        let (secs, exe, size) = build(repo, &target, feature, !a.no_clean_build)?;
        let crates_host = crate_count(repo, feature, false)?;
        let crates_all = crate_count(repo, feature, true)?;
        println!(
            "  {name}: сборка {secs:?} с, exe {size} Б, крейтов {crates_host} (все цели {crates_all})"
        );
        builds.insert(
            name.into(),
            json!({"build_s": secs, "exe_bytes": size, "crates_host": crates_host, "crates_all_targets": crates_all}),
        );
        exes.push((name, exe));
    }

    // 2. Sparse-окна корпуса (общие для обоих вариантов).
    let sparse = work.join("sparse.bin");
    let sp = run(
        &exes[1].1,
        "sparse",
        &[
            ("tokenizer", tokenizer.display().to_string()),
            ("corpus", corpus.display().to_string()),
            ("out", sparse.display().to_string()),
        ],
    )?;
    println!("  sparse: {sp}");

    // 3. Пересборка и поиск — по очереди (данные вариантов не лежат на диске одновременно).
    let mut results = serde_json::Map::new();
    for (name, exe) in &exes {
        let dir = work.join(format!("data-{name}"));
        let common = vec![
            ("backend", name.to_string()),
            ("dir", dir.display().to_string()),
            ("sparse", sparse.display().to_string()),
            ("points", points.clone()),
            ("dim", a.dim.to_string()),
            ("threads", a.index_threads.to_string()),
            ("qdrant", qdrant.display().to_string()),
        ];
        println!("{name}: пересборка {} точек × {}", a.points, a.dim);
        let b = run(exe, "build", &common)?;
        println!(
            "  upsert {} мс, индексация {} мс, диск {} Б",
            b["upsert_ms"], b["index_ms"], b["disk_bytes"]
        );
        let mut searches = Vec::new();
        for i in 0..a.runs {
            let mut args = common.clone();
            args.push(("queries", a.search_queries.to_string()));
            args.push(("warmup", "100".into()));
            let s = run(exe, "search", &args)?;
            println!(
                "  поиск {}: p50 {} / p95 {} мс, RSS {} Б",
                i + 1,
                s["latency_ms"]["p50"],
                s["latency_ms"]["p95"],
                s["rss_after"]["total"]
            );
            searches.push(s);
        }
        if !a.keep {
            fs::remove_dir_all(&dir).with_context(|| format!("удаление {}", dir.display()))?;
        }
        results.insert(name.to_string(), json!({"build": b, "search": searches}));
    }

    // 4. Сводка и критерий ADR.
    let summary = |name: &str| -> Value {
        let s = results[name]["search"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        json!({
            "p50_ms": spread(&s, |v| v["latency_ms"]["p50"].as_f64()),
            "p95_ms": spread(&s, |v| v["latency_ms"]["p95"].as_f64()),
            "p99_ms": spread(&s, |v| v["latency_ms"]["p99"].as_f64()),
            "p95_filtered_ms": spread(&s, |v| v["latency_filtered_ms"]["p95"].as_f64()),
            "open_ms": spread(&s, |v| v["open_ms"].as_f64()),
            "first_query_ms": spread(&s, |v| v["first_query_ms"].as_f64()),
            "rss_open": spread(&s, |v| v["rss_open"]["total"].as_f64()),
            "rss_after": spread(&s, |v| v["rss_after"]["total"].as_f64()),
            "rss_peak": spread(&s, |v| v["rss_peak"]["total"].as_f64()),
            "self_top10": spread(&s, |v| v["self_top10"].as_f64()),
        })
    };
    let (sm, se) = (summary("managed"), summary("edge"));
    let p95_gain = sm["p95_ms"]["median"].as_f64().unwrap_or(f64::NAN)
        / se["p95_ms"]["median"].as_f64().unwrap_or(f64::NAN);
    let rss_ratio = se["rss_after"]["median"].as_f64().unwrap_or(f64::NAN)
        / sm["rss_after"]["median"].as_f64().unwrap_or(f64::NAN);
    let agree = agreement(
        &results["managed"]["search"][0],
        &results["edge"]["search"][0],
    );
    let switch = p95_gain >= 2.0 && rss_ratio <= 1.0;
    // выдачи — только для сверки, в итоговый JSON не идут
    for r in results.values_mut() {
        if let Some(s) = r["search"].as_array_mut() {
            for v in s {
                if let Some(o) = v.as_object_mut() {
                    o.remove("hits");
                }
            }
        }
    }
    let host = json!({
        "os": std::env::consts::OS,
        "logical_cpus": std::thread::available_parallelism().map_or(0, |n| n.get()),
    });
    let report = json!({
        "date": today(),
        "git": git_rev(repo),
        "label": a.label,
        "host": host,
        "points": a.points,
        "dim": a.dim,
        "index_threads": a.index_threads,
        "runs": a.runs,
        "queries": a.search_queries,
        "sparse": sp,
        "builds": builds,
        "results": results,
        "summary": {"managed": sm, "edge": se},
        "p95_gain": p95_gain,
        "rss_ratio_edge_to_managed": rss_ratio,
        "top10_agreement": agree,
        "criterion": "переход только если p95 выигрывает ≥ 2× и RSS Edge не выше managed",
        "switch": switch,
    });
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| repo.join("eval").join("results"));
    fs::create_dir_all(&out)?;
    let mut stem = format!("{}-bench-edge", today());
    if let Some(l) = a.label.as_deref().filter(|l| !l.is_empty()) {
        stem.push('-');
        stem.push_str(l);
    }
    let (jf, mf) = (
        out.join(format!("{stem}.json")),
        out.join(format!("{stem}.md")),
    );
    fs::write(&jf, serde_json::to_string_pretty(&report)? + "\n")?;
    let md = markdown(&report);
    fs::write(&mf, &md)?;
    println!("{md}\n  {}\n  {}", jf.display(), mf.display());
    if !a.keep {
        // сборки воспроизводимы и весят гигабайты; sparse.bin пересчитывается за секунды
        for (name, _) in VARIANTS {
            let _ = fs::remove_dir_all(work.join(format!("build-{name}")));
        }
        let _ = fs::remove_file(&sparse);
    }
    Ok(())
}

fn mib(v: &Value) -> String {
    v.as_f64()
        .map_or("—".into(), |b| format!("{:.0}", b / 1_048_576.0))
}

fn ms(v: &Value) -> String {
    v.as_f64().map_or("—".into(), |x| format!("{x:.2}"))
}

/// «медиана (мин–макс)» для величины `f` (форматер значения).
fn med(v: &Value, f: fn(&Value) -> String) -> String {
    format!("{} ({}–{})", f(&v["median"]), f(&v["min"]), f(&v["max"]))
}

fn secs(v: &Value) -> String {
    v.as_f64()
        .map_or("—".into(), |x| format!("{:.1}", x / 1000.0))
}

/// Отчёт Markdown: таблица managed / Edge и итог критерия.
pub fn markdown(r: &Value) -> String {
    let (m, e) = (&r["summary"]["managed"], &r["summary"]["edge"]);
    let (bm, be) = (
        &r["results"]["managed"]["build"],
        &r["results"]["edge"]["build"],
    );
    let (cm, ce) = (&r["builds"]["managed"], &r["builds"]["edge"]);
    let mut s = format!(
        "# Bench: managed Qdrant против Qdrant Edge — {}\n\n\
         git `{}`, {} точек × {}, потоков индексации {}, прогонов поиска {} по {} запросов.\n\
         Значения поиска — медиана (мин–макс) по прогонам; RSS managed — Qdrant + клиент.\n\n\
         | Метрика | managed | Edge |\n|---|---|---|\n",
        r["date"].as_str().unwrap_or("?"),
        r["git"].as_str().unwrap_or("?"),
        r["points"],
        r["dim"],
        r["index_threads"],
        r["runs"],
        r["queries"],
    );
    let mut row = |k: &str, a: String, b: String| s.push_str(&format!("| {k} | {a} | {b} |\n"));
    row("p50, мс", med(&m["p50_ms"], ms), med(&e["p50_ms"], ms));
    row("p95, мс", med(&m["p95_ms"], ms), med(&e["p95_ms"], ms));
    row("p99, мс", med(&m["p99_ms"], ms), med(&e["p99_ms"], ms));
    row(
        "p95 с фильтром папки, мс",
        med(&m["p95_filtered_ms"], ms),
        med(&e["p95_filtered_ms"], ms),
    );
    row(
        "первый запрос после открытия, мс",
        med(&m["first_query_ms"], ms),
        med(&e["first_query_ms"], ms),
    );
    row(
        "открытие (старт Qdrant / загрузка шарда), мс",
        med(&m["open_ms"], ms),
        med(&e["open_ms"], ms),
    );
    row(
        "RSS после открытия, МиБ",
        med(&m["rss_open"], mib),
        med(&e["rss_open"], mib),
    );
    row(
        "RSS после запросов, МиБ",
        med(&m["rss_after"], mib),
        med(&e["rss_after"], mib),
    );
    row(
        "RSS пик за запросы, МиБ",
        med(&m["rss_peak"], mib),
        med(&e["rss_peak"], mib),
    );
    row(
        "находит цель в топ-10",
        med(&m["self_top10"], ms),
        med(&e["self_top10"], ms),
    );
    row(
        "пересборка: upsert, с",
        secs(&bm["upsert_ms"]),
        secs(&be["upsert_ms"]),
    );
    row(
        "пересборка: индексация, с",
        secs(&bm["index_ms"]),
        secs(&be["index_ms"]),
    );
    row(
        "пересборка всего, с",
        secs(&bm["total_ms"]),
        secs(&be["total_ms"]),
    );
    row(
        "RSS пик пересборки, МиБ",
        mib(&bm["rss_peak"]["total"]),
        mib(&be["rss_peak"]["total"]),
    );
    row(
        "на диске, МиБ",
        mib(&bm["disk_bytes"]),
        mib(&be["disk_bytes"]),
    );
    row(
        "чистая release-сборка, с",
        cm["build_s"]
            .as_f64()
            .map_or("—".into(), |x| format!("{x:.0}")),
        ce["build_s"]
            .as_f64()
            .map_or("—".into(), |x| format!("{x:.0}")),
    );
    row(
        "размер exe, МиБ",
        mib(&cm["exe_bytes"]),
        mib(&ce["exe_bytes"]),
    );
    row(
        "крейтов (хост / все цели)",
        format!("{} / {}", cm["crates_host"], cm["crates_all_targets"]),
        format!("{} / {}", ce["crates_host"], ce["crates_all_targets"]),
    );
    s.push_str(&format!(
        "\nВыигрыш p95 (managed / Edge): **{:.2}×**; RSS Edge / managed: **{:.2}**; \
         совпадение топ-10 выдач: {}.\n\n\
         Критерий (исследование 05): {}. Итог: **{}**.\n",
        r["p95_gain"].as_f64().unwrap_or(f64::NAN),
        r["rss_ratio_edge_to_managed"].as_f64().unwrap_or(f64::NAN),
        r["top10_agreement"]
            .as_f64()
            .map_or("—".into(), |x| format!("{:.0} %", x * 100.0)),
        r["criterion"].as_str().unwrap_or(""),
        if r["switch"].as_bool() == Some(true) {
            "критерий перехода выполнен"
        } else {
            "не переходить"
        },
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_spread() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), 2.5);
        let runs = vec![json!({"x": 5.0}), json!({"x": 1.0}), json!({"x": 3.0})];
        let s = spread(&runs, |v| v["x"].as_f64());
        assert_eq!(s["median"], 3.0);
        assert_eq!(s["min"], 1.0);
        assert_eq!(s["max"], 5.0);
    }

    #[test]
    fn agreement_counts_common_top10() {
        let a = json!({"hits": [["c1", "c2"], ["c3"]]});
        let b = json!({"hits": [["c2", "c9"], ["c3"]]});
        assert_eq!(agreement(&a, &b), Some(0.75));
        assert_eq!(agreement(&json!({}), &b), None);
    }

    #[test]
    fn markdown_states_decision() {
        let r = json!({
            "date": "2026-10-20", "git": "abc", "points": 10, "dim": 4, "index_threads": 2,
            "runs": 1, "queries": 5, "p95_gain": 1.2, "rss_ratio_edge_to_managed": 0.8,
            "top10_agreement": 0.9, "criterion": "к", "switch": false,
        });
        let md = markdown(&r);
        assert!(md.contains("**1.20×**"), "{md}");
        assert!(md.contains("не переходить"), "{md}");
    }
}
