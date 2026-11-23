//! `zam check` — ночная проверка памяти (M7.3 §6.5 п. 7): `status` (последний вердикт, пробы,
//! выключатели), `report [--json]` (вердикт, метрики к медиане базы, тренд, проваленные пробы с
//! советами), `run [--no-wait]` (ручной прогон `POST /check/run`, ожидание конца). Время — UTC.
//! Сервер без признака `memory_check` (`/health.features`) — код 1 с подсказкой обновить
//! `zam-server`.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Ctx, emit, emit_json, old_server};
use crate::client::{CliError, read_json};

/// Признак сервера (`/health.features`).
pub const FEATURE: &str = "memory_check";
/// Текст для старого сервера.
pub const OLD_SERVER: &str = "сервер не поддерживает проверку памяти (/check): обновите zam-server";
/// Ожидание конца ручного прогона (предел `check_max_min` — до 240 мин).
const RUN_WAIT: Duration = Duration::from_secs(4 * 60 * 60 + 60);
/// Опрос прогона.
const POLL: Duration = Duration::from_secs(2);

fn status_json(ctx: &Ctx) -> Result<Value, CliError> {
    ctx.client
        .get("/check/status", &[])
        .and_then(read_json)
        .map_err(|e| old_server(ctx, e, &[404], FEATURE, OLD_SERVER))
}

/// Доля как процент с одним знаком (`—` — нет значения).
fn pct(v: &Value) -> String {
    v.as_f64()
        .map_or_else(|| "—".into(), |x| format!("{:.1} %", x * 100.0))
}

fn ms(v: &Value) -> String {
    v.as_f64()
        .map_or_else(|| "—".into(), |x| format!("{x:.0} мс"))
}

/// Время хранилища (`…T…Z`) — как у агента: UTC `ГГГГ-ММ-ДД ЧЧ:ММ`.
fn when(v: &Value) -> String {
    v.as_str().map_or_else(
        || "—".into(),
        |s| zam_core::time::agent_ts(s).unwrap_or_else(|| s.into()),
    )
}

/// Вердикт одной строкой.
fn verdict_line(v: &Value) -> String {
    let kind = v["kind"].as_str().unwrap_or("—");
    let mut s = match kind {
        "baseline" => format!(
            "baseline: база копится ({}/{})",
            v["base_n"].as_u64().unwrap_or(0),
            v["base_of"].as_u64().unwrap_or(5)
        ),
        "ok" => "ok: ухудшения нет".into(),
        "regression" => {
            let flags: Vec<&str> = v["flags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["name"].as_str())
                .collect();
            format!("regression: ухудшение поиска ({})", flags.join(", "))
        }
        "changed" => format!(
            "changed: сменился отпечаток ({}: {} → {}) — база начинается заново",
            v["changed"]["what"].as_str().unwrap_or("?"),
            v["changed"]["from"],
            v["changed"]["to"]
        ),
        "degraded" => "degraded: поиск без векторов".into(),
        other => other.to_string(),
    };
    if v["after_update"] == true {
        s.push_str(&format!(
            " · после обновления до {}",
            v["after_update_version"].as_str().unwrap_or("?")
        ));
    }
    if v["few"] == true {
        s.push_str(" · мало проб test");
    }
    s
}

/// `zam check status`.
pub fn status(ctx: &Ctx) -> Result<(), CliError> {
    let v = status_json(ctx)?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut s = String::new();
    let last = &v["last"];
    if last.is_null() {
        s.push_str("прогонов проверки ещё не было\n");
    } else {
        s.push_str(&format!(
            "последний прогон {} ({}, {}): {}\n",
            when(&last["started_at"]),
            last["trigger"].as_str().unwrap_or("?"),
            last["status"].as_str().unwrap_or("?"),
            if last["verdict"].is_null() {
                "без вердикта".into()
            } else {
                verdict_line(&last["verdict"])
            }
        ));
    }
    if let Some(r) = v["running"]["run"].as_str() {
        s.push_str(&format!("идёт прогон {r}\n"));
    }
    let p = &v["probes"];
    s.push_str(&format!(
        "пробы: активных {} (виды {}, части {})\n",
        p["active"], p["by_kind"], p["by_split"]
    ));
    s.push_str(&format!(
        "ночная проверка: сервер {}, пользователь {}\n",
        v["check"]["server"].as_str().unwrap_or("?"),
        v["check"]["user"].as_str().unwrap_or("?")
    ));
    emit(s.as_bytes())
}

/// `zam check report [--json]`.
pub fn report(ctx: &Ctx) -> Result<(), CliError> {
    let v = status_json(ctx)?;
    if ctx.json {
        return emit_json(&v);
    }
    let last = &v["last"];
    if last.is_null() {
        return emit("прогонов проверки ещё не было\n".as_bytes());
    }
    let mut s = format!(
        "прогон {} · {} · {}\nвердикт: {}\n",
        last["run"].as_str().unwrap_or("?"),
        when(&last["started_at"]),
        last["status"].as_str().unwrap_or("?"),
        if last["verdict"].is_null() {
            "нет (прогон не закончен в пределе или пропущен)".into()
        } else {
            verdict_line(&last["verdict"])
        }
    );
    let m = &last["metrics"];
    let base = &last["compared_with"];
    s.push_str("метрики (test · медиана базы):\n");
    for (name, cur, med) in [
        ("hit_search", &m["test"]["hit_search"], &base["hit_search"]),
        ("hit_prompt", &m["test"]["hit_prompt"], &base["hit_prompt"]),
        (
            "negatives_shown",
            &m["negatives_shown"],
            &base["negatives_shown"],
        ),
    ] {
        s.push_str(&format!("  {name}: {} · {}\n", pct(cur), pct(med)));
    }
    s.push_str(&format!(
        "  p95 поиска: {} · {}\n  mrr10 {} · none {} · beyond {} · hard {}\n",
        ms(&m["search_p95_ms"]),
        ms(&base["p95"]),
        m["test"]["mrr10"]
            .as_f64()
            .map_or_else(|| "—".into(), |x| format!("{x:.2}")),
        pct(&m["test"]["none"]),
        pct(&m["test"]["beyond"]),
        pct(&m["test"]["hard"]),
    ));
    let trend = v["trend"].as_array().cloned().unwrap_or_default();
    if !trend.is_empty() {
        s.push_str("тренд (новые первыми):\n");
        for t in trend.iter().take(10) {
            s.push_str(&format!(
                "  {} {} {} hit {} prompt {} p95 {}\n",
                t["date"].as_str().unwrap_or("?"),
                t["trigger"].as_str().unwrap_or("?"),
                t["verdict"]
                    .as_str()
                    .unwrap_or(t["status"].as_str().unwrap_or("?")),
                pct(&t["hit_search"]),
                pct(&t["hit_prompt"]),
                ms(&t["p95"]),
            ));
        }
    }
    let failing = v["failing"].as_array().cloned().unwrap_or_default();
    if !failing.is_empty() {
        s.push_str("проваленные пробы:\n");
        for f in &failing {
            s.push_str(&format!(
                "  {} «{}» → {}: {}\n",
                f["probe"].as_str().unwrap_or("?"),
                f["question"].as_str().unwrap_or(""),
                f["target"].as_str().unwrap_or("—"),
                f["advice_text"].as_str().unwrap_or(""),
            ));
        }
    }
    let advice = v["advice"].as_array().cloned().unwrap_or_default();
    if !advice.is_empty() {
        s.push_str("что чинить:\n");
        for a in &advice {
            s.push_str(&format!("  - {}\n", a["text"].as_str().unwrap_or("")));
        }
    }
    emit(s.as_bytes())
}

/// `zam check run [--no-wait]`: ручной прогон; без `--no-wait` — ждёт его конца.
pub fn run(ctx: &Ctx, wait: bool) -> Result<(), CliError> {
    let v = ctx
        .client
        .post_json("/check/run", &json!({}))
        .and_then(read_json)
        .map_err(|e| old_server(ctx, e, &[404, 405], FEATURE, OLD_SERVER))?;
    let run = v["run"].as_str().unwrap_or_default().to_string();
    if !wait {
        if ctx.json {
            return emit_json(&v);
        }
        return emit(format!("прогон {run} начат: zam check report — итог\n").as_bytes());
    }
    let t0 = Instant::now();
    let path = format!("/check/runs/{run}");
    let done = loop {
        std::thread::sleep(POLL);
        match ctx.client.get(&path, &[]).and_then(read_json) {
            Ok(r) if r["status"] != "running" => break r,
            Ok(_) => {}
            // прогон ещё не записан (ждёт шагов старта сервера)
            Err(e) if e.http_status() == Some(404) => {}
            Err(e) => return Err(e),
        }
        if t0.elapsed() > RUN_WAIT {
            return Err(CliError::config(format!(
                "прогон {run} не закончился за {} мин: zam check report — позже",
                RUN_WAIT.as_secs() / 60
            )));
        }
    };
    if ctx.json {
        return emit_json(&done);
    }
    let verdict = if done["verdict"].is_null() {
        "без вердикта".to_string()
    } else {
        verdict_line(&done["verdict"])
    };
    emit(
        format!(
            "прогон {run}: {} — {verdict}; подробно — zam check report\n",
            done["status"].as_str().unwrap_or("?")
        )
        .as_bytes(),
    )
}
