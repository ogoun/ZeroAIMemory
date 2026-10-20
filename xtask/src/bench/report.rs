//! Отчёт bench: таблица «метрика — цель §17 — замер — итог», файлы JSON и Markdown.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{BenchResult, Pass, Profile};

/// Байты → «N МиБ» (все объёмы памяти в отчёте — двоичные единицы).
pub fn mb(b: Option<u64>) -> String {
    match b {
        Some(b) => format!("{:.0} МиБ", b as f64 / 1_048_576.0),
        None => "—".into(),
    }
}

/// Строка таблицы.
struct Row {
    metric: String,
    target: String,
    value: String,
    /// `None` — цели нет (справочно) или нет замера.
    ok: Option<bool>,
}

fn row(metric: &str, target: &str, value: String, ok: Option<bool>) -> Row {
    Row {
        metric: metric.into(),
        target: target.into(),
        value,
        ok,
    }
}

/// p95 клиента прохода против предела `limit_ms`.
fn pass_row(metric: &str, p: &Option<Pass>, limit_ms: Option<f64>) -> Row {
    let target = limit_ms.map_or("—".into(), |l| format!("p95 ≤ {}", ms(l)));
    match p.as_ref().and_then(|p| p.client.as_ref().map(|c| (p, c))) {
        Some((p, c)) => {
            let mut v = format!(
                "p50 {} / p95 {} / max {} (n = {})",
                ms(c.p50),
                ms(c.p95),
                ms(c.max),
                c.n
            );
            if p.queue_before > 0 || p.queue_after > 0 {
                v.push_str(&format!("; очередь {} → {}", p.queue_before, p.queue_after));
            }
            if p.warnings > 0 {
                v.push_str(&format!("; без реранка: {}", p.warnings));
            }
            if p.under_load.is_some() {
                v.push_str(&format!(
                    "; воркер за проход: заданий {}, инференс {}",
                    p.jobs_done,
                    ms(p.index_embed_ms)
                ));
                if p.attempts > 1 {
                    v.push_str(&format!(" (попытка {})", p.attempts));
                }
            }
            if p.under_load == Some(false) {
                v.push_str(" — **недействителен: воркер не индексировал**");
                return row(metric, &target, v, None);
            }
            row(
                metric,
                &target,
                v,
                limit_ms.map(|l| c.p95 <= l && p.warnings == 0),
            )
        }
        None => row(metric, &target, "не замерялось".into(), None),
    }
}

/// Миллисекунды: до 10 — с десятыми, больше 1000 — в секундах.
fn ms(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.2} с", v / 1000.0)
    } else if v >= 10.0 {
        format!("{v:.0} мс")
    } else {
        format!("{v:.1} мс")
    }
}

fn rows(r: &BenchResult) -> Vec<Row> {
    let gpu = r.profile == Some(Profile::Gpu);
    let mut out = Vec::new();
    let search = if gpu { 50.0 } else { 120.0 };
    out.push(pass_row(
        "Поиск `chunks` без реранка, пустая очередь",
        &r.search_empty_queue,
        Some(search),
    ));
    out.push(pass_row(
        "Поиск `chunks` без реранка, полная очередь",
        &r.search_full_queue,
        Some(search),
    ));
    out.push(pass_row(
        "Поиск `chunks`, повтор (кэш запросов)",
        &r.search_cached,
        None,
    ));
    out.push(pass_row("Поиск FTS-only", &r.fts_only, Some(20.0)));
    out.push(pass_row(
        &format!(
            "Поиск с реранком 20×256 ({})",
            r.reranker.as_deref().unwrap_or("—")
        ),
        &r.rerank,
        Some(if gpu { 150.0 } else { 1500.0 }),
    ));
    // §17: «чанк 400 токенов» — итог по токенам/с (чанки корпуса — ~200 токенов): CPU — 3
    // чанка по 400 = 1200 токенов/с; GPU — цель предложена по замеру (3000 токенов/с ≈ 7,5
    // чанка по 400), ждёт решения владельца — справочно
    for (name, run) in [("корпус eval", &r.index_base), ("копии", &r.index_copies)] {
        let target = if gpu {
            "предложено ≥ 3000 ток./с (≈ 7,5 чанка по 400), ждёт решения владельца"
        } else {
            "≥ 3 чанка по 400 ток./с (≥ 1200 ток./с)"
        };
        out.push(match run {
            Some(i) => row(
                &format!("Индексация, {name}"),
                target,
                format!(
                    "{:.2} чанка/с ({} чанков ~{:.0} ток. за {:.0} с; {:.0} ток./с ≈ {:.2} чанка по 400 ток./с; инференс {:.0} с, Qdrant {:.0} с)",
                    i.chunks_per_sec,
                    i.chunks,
                    i.avg_tokens,
                    i.secs,
                    i.chunks_per_sec * i.avg_tokens,
                    i.chunks_per_sec * i.avg_tokens / 400.0,
                    i.embed_secs,
                    i.qdrant_secs
                ),
                (!gpu).then_some(i.chunks_per_sec * i.avg_tokens >= 1200.0),
            ),
            None => row(&format!("Индексация, {name}"), target, "не замерялось".into(), None),
        });
    }
    let wl = if gpu { 1000.0 } else { 5000.0 };
    out.push(
        match r
            .write
            .as_ref()
            .and_then(|w| w.to_vector.as_ref().map(|t| (w, t)))
        {
            Some((w, t)) => row(
                "Запись (≤ 10 чанков) до векторного индекса, очередь пуста",
                &format!("≤ {}", ms(wl)),
                {
                    let mut v = format!(
                        "p50 {} / p95 {} / max {} (n = {}; больше 10 чанков, не в замере: {}); чанков {:.1} (max {})",
                        ms(t.p50),
                        ms(t.p95),
                        ms(t.max),
                        t.n,
                        w.excluded,
                        w.chunks_avg,
                        w.chunks_max
                    );
                    let part = |name: &str, s: &Option<super::Stats>| {
                        s.as_ref().map_or(String::new(), |s| {
                            format!("{name} {} / {}", ms(s.p50), ms(s.p95))
                        })
                    };
                    v.push_str(&format!(
                        "; p50 / p95 по частям: {}, {}, {}, {}",
                        part("POST", &w.post),
                        part("инференс", &w.embed),
                        part("Qdrant", &w.qdrant),
                        part("остальное", &w.other)
                    ));
                    v
                },
                Some(t.p95 <= wl && w.chunks_max <= 10),
            ),
            None => row(
                "Запись до векторного индекса",
                &format!("≤ {}", ms(wl)),
                "не замерялось".into(),
                None,
            ),
        },
    );
    if let Some(p) = r.index_copies.as_ref().and_then(|i| i.post.as_ref()) {
        out.push(row(
            "Запись заметки до FTS при полной очереди индексации (загрузка копий)",
            "≤ 30 мс",
            format!(
                "p50 {} / p95 {} / max {} (n = {})",
                ms(p.p50),
                ms(p.p95),
                ms(p.max),
                p.n
            ),
            Some(p.p95 <= 30.0),
        ));
    }
    out.push(match r.write.as_ref().and_then(|w| w.post.as_ref()) {
        Some(p) => row(
            "Запись заметки до FTS, очередь пуста",
            "≤ 30 мс",
            format!("p50 {} / p95 {}", ms(p.p50), ms(p.p95)),
            Some(p.p95 <= 30.0),
        ),
        None => row(
            "Запись заметки до FTS",
            "≤ 30 мс",
            "не замерялось".into(),
            None,
        ),
    });
    out.push(match &r.reindex {
        Some(x) => row(
            "`reindex` из `embeddings`",
            "≥ 2000 чанков/с",
            format!(
                "{:.0} точек/с ({} точек за {})",
                x.points_per_sec,
                x.points,
                ms(x.elapsed_ms)
            ),
            Some(x.points_per_sec >= 2000.0),
        ),
        None => row(
            "`reindex` из `embeddings`",
            "≥ 2000 чанков/с",
            "не замерялось".into(),
            None,
        ),
    });
    // цели ТЗ — в МБ/ГБ (10^6/10^9 байт), замеры — в МиБ: сравниваются байты
    let mb_ok = |b: Option<u64>, lim_mb: f64| b.map(|b| b as f64 <= lim_mb * 1e6);
    out.push(row(
        "RSS в простое без моделей, пустой каталог",
        "≤ 50 МБ (47,7 МиБ)",
        mb(r.rss_idle_no_models),
        mb_ok(r.rss_idle_no_models, 50.0),
    ));
    out.push(row(
        "RSS без моделей, 10 тыс. заметок, после поиска",
        "≤ 50 МБ (47,7 МиБ)",
        mb(r.rss_fts_loaded),
        mb_ok(r.rss_fts_loaded, 50.0),
    ));
    let model_lim = if gpu { 600.0 } else { 1100.0 };
    let model_target = if gpu {
        "≤ 0,6 ГБ (572 МиБ)"
    } else {
        "≤ 1,1 ГБ (1049 МиБ)"
    };
    let with_private = |rss: Option<u64>, private: Option<u64>| match private {
        Some(_) => format!("{} (частная {})", mb(rss), mb(private)),
        None => mb(rss),
    };
    out.push(row(
        "RSS с bge-m3 в простое",
        model_target,
        with_private(r.rss_idle_model, r.private_idle_model),
        mb_ok(r.rss_idle_model, model_lim),
    ));
    out.push(row(
        "RSS с bge-m3 после поиска",
        model_target,
        with_private(r.rss_after_search, r.private_after_search),
        mb_ok(r.rss_after_search, model_lim),
    ));
    let extra = match (r.rss_after_rerank, r.rss_after_search) {
        (Some(a), Some(b)) => Some(a.saturating_sub(b)),
        _ => None,
    };
    out.push(row(
        "Реранкер, дополнительно",
        if gpu {
            "VRAM"
        } else {
            "≤ 0,5 ГБ (477 МиБ)"
        },
        format!("{} (всего {})", mb(extra), mb(r.rss_after_rerank)),
        (!gpu).then(|| mb_ok(extra, 500.0)).flatten(),
    ));
    let fmax = |f: &dyn Fn(&super::Startup) -> Option<f64>| {
        r.startup
            .iter()
            .filter_map(f)
            .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
    };
    out.push(match fmax(&|s| s.qdrant_from_start_ms) {
        None => row(
            "Старт до готовности Qdrant",
            "≤ 5 с",
            "не замерялось".into(),
            None,
        ),
        Some(q) => row(
            "Старт `zam-server` до готовности Qdrant (коллекция на диске)",
            "≤ 5 с",
            format!(
                "max {} из {} стартов (сам процесс Qdrant — max {})",
                ms(q),
                r.startup.len(),
                fmax(&|s| s.qdrant_ready_ms).map_or("—".into(), ms)
            ),
            Some(q <= 5000.0),
        ),
    });
    if let Some(v) = fmax(&|s| Some(s.ready_ms)) {
        out.push(row(
            "Старт до готовности векторного канала (модель + Qdrant)",
            "—",
            format!(
                "max {} (загрузка модели — max {}; до /health — max {})",
                ms(v),
                fmax(&|s| s.model_load_ms).map_or("—".into(), ms),
                fmax(&|s| Some(s.health_ms)).map_or("—".into(), ms)
            ),
            None,
        ));
    }
    if let Some(k) = &r.kill {
        let v = match &k.down {
            Some(d) => format!(
                "простой: p50 {} / p95 {} / max {} (n = {}); первый ответ {}; до деградации {}; восстановление {} (первый ответ нового Qdrant {}); до убийства p95 {}",
                ms(d.p50),
                ms(d.p95),
                ms(d.max),
                d.n,
                k.first_after_kill_ms.map_or("—".into(), ms),
                k.to_degraded_ms.map_or("—".into(), ms),
                k.to_restored_ms.map_or("—".into(), ms),
                k.first_restored_ms.map_or("—".into(), ms),
                k.before.as_ref().map_or("—".into(), |b| ms(b.p95))
            ),
            None => "нет ответов во время простоя".into(),
        };
        // ответ при упавшем Qdrant не дольше бюджета поиска без реранка плюс FTS-only:
        // эмбеддинг запроса считается до обращения к Qdrant, отказ должен быть мгновенным
        let lim = 20.0 + search;
        out.push(row(
            "Поиск при упавшем Qdrant (до перезапуска супервизором)",
            &format!("max ≤ {}", ms(lim)),
            v,
            k.down.as_ref().map(|d| d.max <= lim),
        ));
    }
    out
}

/// Markdown-отчёт.
pub fn markdown(r: &BenchResult) -> String {
    let mut s = format!(
        "# bench §17 — профиль {} ({})\n\n- git: `{}`, заметок {}, запросов {}\n- модель: `{}`, устройство: {}\n- машина: {}\n",
        r.profile.map_or("?", |p| p.as_str()),
        r.date,
        r.git,
        r.notes,
        r.queries,
        r.model_id.as_deref().unwrap_or("—"),
        r.device,
        r.host
    );
    if !r.sets.is_empty() {
        let sets: Vec<String> = r.sets.iter().map(|(k, v)| format!("`{k}={v}`")).collect();
        s.push_str(&format!("- переопределения: {}\n", sets.join(", ")));
    }
    if let (Some(p), Some(q)) = (r.qdrant_points, r.qdrant_rss) {
        s.push_str(&format!("- Qdrant: {p} точек, RSS {}\n", mb(Some(q))));
    }
    s.push_str("\n| Метрика | Цель §17 | Замер | Итог |\n|---|---|---|---|\n");
    for x in rows(r) {
        let ok = match x.ok {
            Some(true) => "выполнено",
            Some(false) => "**не выполнено**",
            None => "справочно",
        };
        s.push_str(&format!(
            "| {} | {} | {} | {ok} |\n",
            x.metric, x.target, x.value
        ));
    }
    for (name, p) in [
        ("полная очередь", &r.search_full_queue),
        ("пустая очередь", &r.search_empty_queue),
        ("реранк", &r.rerank),
        ("FTS-only", &r.fts_only),
    ] {
        if let Some(p) = p {
            let parts: Vec<String> = p
                .server
                .iter()
                .map(|(k, v)| format!("{k} p50 {} / p95 {}", ms(v.p50), ms(v.p95)))
                .collect();
            s.push_str(&format!("\nСервер, {name}: {}\n", parts.join("; ")));
        }
    }
    s
}

/// Пишет `<out>/<дата>-bench-<профиль>[-метка].{json,md}`.
pub fn write(out: &Path, r: &BenchResult) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(out).with_context(|| format!("создание {}", out.display()))?;
    let mut stem = format!("{}-bench-{}", r.date, r.profile.map_or("?", |p| p.as_str()));
    if let Some(l) = r.label.as_deref().filter(|l| !l.is_empty()) {
        stem.push('-');
        stem.push_str(l);
    }
    let json = out.join(format!("{stem}.json"));
    let md = out.join(format!("{stem}.md"));
    fs::write(&json, serde_json::to_string_pretty(r)? + "\n")?;
    fs::write(&md, markdown(r))?;
    Ok((json, md))
}
