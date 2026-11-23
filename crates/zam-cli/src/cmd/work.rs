//! `zam work` — внутренние работы памяти (M7.3 §6.3 п. 8): состояние исполнителя, пакет работы,
//! итог и возврат в очередь.
//!
//! - `zam work status` — режимы видов, очередь, последняя ночь, расход токенов модели (`GET
//!   /works/usage`, `GET /works`).
//! - `zam work next [--kind K] [--budget N]` — пакет `POST /recall {event: "work"}`: захват
//!   старейшей работы для сессии агента (`ZAM_SESSION` или сессия хуков); без сессии сервер
//!   отвечает 400 — итог принимает только захватившая сессия.
//! - `zam work put <файл.json> [--key K]` — итог `POST /works/{id}/result`: файл — `{work:
//!   "j:<id>", items: […], skip?}` (как `data` у `memory_log kind=work`).
//! - `zam work release j:<id>` — вернуть работу в очередь без итога.
//!
//! Сервер без признака `works` в `GET /health` `features` — код 1 с подсказкой обновить
//! `zam-server` ([`OLD_SERVER`]).
use std::path::Path;

use serde_json::{Value, json};

use super::{Ctx, emit, emit_json, has_feature, old_server};
use crate::client::{CliError, read_json};

/// Признак внутренних работ в `GET /health` `features` (`docs/api-compat.md`).
const FEATURE: &str = "works";
/// Сообщение для сервера без внутренних работ.
pub const OLD_SERVER: &str =
    "сервер не поддерживает внутренние работы: нужен zam-server 0.0.16-m7.3 или новее";

/// Ошибка запроса возможности внутренних работ: так отвечает старый сервер (404 — нет маршрута,
/// 400 — неизвестное событие `work` у `/recall`, 405 — нет метода).
fn old(ctx: &Ctx, e: CliError) -> CliError {
    old_server(ctx, e, &[400, 404, 405], FEATURE, OLD_SERVER)
}

/// Голый id работы из `j:<id>` или id.
fn work_id(raw: &str) -> Result<String, CliError> {
    let s = raw.trim();
    let id = s.strip_prefix("j:").unwrap_or(s);
    if id.is_empty() || id.contains(['/', '?', '#', ' ']) {
        return Err(CliError::input("работа: j:<id>"));
    }
    Ok(id.to_string())
}

/// `zam work status`.
pub fn status(ctx: &Ctx) -> Result<(), CliError> {
    let health = read_json(ctx.client.get("/health", &[])?)?;
    if !has_feature(&health, FEATURE) {
        return Err(CliError::config(OLD_SERVER));
    }
    let usage = ctx
        .client
        .get("/works/usage", &[])
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    let works = ctx
        .client
        .get("/works", &[("limit", "20")])
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    if ctx.json {
        return emit_json(&json!({"usage": usage, "works": works["works"]}));
    }
    let mut out = String::new();
    let modes: Vec<String> = usage["modes"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| match v["reason"].as_str() {
                    Some(r) => format!("{k} {} ({r})", v["mode"].as_str().unwrap_or("?")),
                    None => format!("{k} {}", v["mode"].as_str().unwrap_or("?")),
                })
                .collect()
        })
        .unwrap_or_default();
    out.push_str(&format!("режимы: {}\n", modes.join(", ")));
    let list = works["works"].as_array().cloned().unwrap_or_default();
    let open: Vec<String> = list
        .iter()
        .filter(|w| matches!(w["state"].as_str(), Some("queued" | "running")))
        .map(|w| {
            format!(
                "{} {} [{}] {}",
                w["id"].as_str().unwrap_or("?"),
                w["kind"].as_str().unwrap_or("?"),
                w["state"].as_str().unwrap_or("?"),
                w["mode"].as_str().unwrap_or("?"),
            )
        })
        .collect();
    if open.is_empty() {
        out.push_str("очередь: пусто\n");
    } else {
        out.push_str(&format!("очередь: {}\n", open.len()));
        for l in &open {
            out.push_str(&format!("  {l}\n"));
        }
    }
    let night = &usage["night"];
    out.push_str(&format!(
        "последняя ночь: {} (окно {} {}, {} мин)\n",
        usage["last_night"].as_str().unwrap_or("—"),
        night["at"].as_str().unwrap_or("?"),
        night["tz"].as_str().unwrap_or("?"),
        night["window_min"],
    ));
    out.push_str(&format!(
        "токены модели: сутки {} из {}, месяц {} из {}\n",
        usage["day"]["tokens"],
        usage["day"]["limit"],
        usage["month"]["tokens"],
        usage["month"]["limit"],
    ));
    for w in list
        .iter()
        .filter(|w| !matches!(w["state"].as_str(), Some("queued" | "running")))
        .take(5)
    {
        out.push_str(&format!(
            "  {} {} [{}] {}\n",
            w["id"].as_str().unwrap_or("?"),
            w["kind"].as_str().unwrap_or("?"),
            w["state"].as_str().unwrap_or("?"),
            w["finished_at"]
                .as_str()
                .map(super::agent_time)
                .unwrap_or_default(),
        ));
    }
    emit(out.as_bytes())
}

/// `zam work next [--kind K] [--budget N]`.
pub fn next(ctx: &Ctx, kind: Option<&str>, budget: Option<u32>) -> Result<(), CliError> {
    let mut body = json!({"event": "work"});
    if let Some(k) = kind {
        body["kind"] = json!(k);
    }
    if let Some(b) = budget {
        body["budget_tokens"] = json!(b);
    }
    let v = ctx
        .client
        .post_json("/recall", &body)
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut out = v["text"]
        .as_str()
        .unwrap_or_default()
        .trim_end()
        .to_string();
    out.push('\n');
    emit(out.as_bytes())
}

/// Строка итога: состояние, счёт и причины отказов.
fn result_text(v: &Value) -> String {
    let n = |k: &str| v[k].as_array().map_or(0, Vec::len);
    let mut s = format!(
        "{}: {} — принято {}, отклонено {}, отказ {}, без итога {}\n",
        v["work"].as_str().unwrap_or("?"),
        v["state"].as_str().unwrap_or("?"),
        n("accepted"),
        n("rejected"),
        n("skip"),
        n("skipped"),
    );
    for r in v["rejected"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "  n {} {}: {}\n",
            r["n"],
            r["target"].as_str().unwrap_or(""),
            r["reason"].as_str().unwrap_or("")
        ));
    }
    s
}

/// `zam work put <файл.json> [--key K]`.
pub fn put(ctx: &Ctx, file: &Path, key: Option<&str>) -> Result<(), CliError> {
    let raw = std::fs::read_to_string(file)
        .map_err(|e| CliError::input(format!("{}: {e}", file.display())))?;
    let data: Value = serde_json::from_str(&raw)
        .map_err(|e| CliError::input(format!("{}: не JSON: {e}", file.display())))?;
    let work = data["work"]
        .as_str()
        .ok_or_else(|| CliError::input("в файле нет work: \"j:<id>\" (работа из пакета)"))?;
    let id = work_id(work)?;
    let mut body = json!({});
    for k in ["items", "skip"] {
        if let Some(v) = data.get(k).filter(|v| !v.is_null()) {
            body[k] = v.clone();
        }
    }
    let headers: Vec<(&str, &str)> = key.map(|k| ("Idempotency-Key", k)).into_iter().collect();
    let v = ctx
        .client
        .post_json_with(&format!("/works/{id}/result"), &body, &headers)
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    if ctx.json {
        return emit_json(&v);
    }
    emit(result_text(&v).as_bytes())
}

/// `zam work release j:<id>`.
pub fn release(ctx: &Ctx, raw: &str) -> Result<(), CliError> {
    let id = work_id(raw)?;
    let v = ctx
        .client
        .post_json(&format!("/works/{id}/release"), &json!({}))
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    if ctx.json {
        return emit_json(&v);
    }
    emit(
        format!(
            "{}: {}\n",
            v["work"].as_str().unwrap_or("?"),
            v["state"].as_str().unwrap_or("?")
        )
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ссылка `j:` и голый id; пусто и пути — отказ.
    #[test]
    fn work_id_cases() {
        assert_eq!(work_id("j:abc").unwrap(), "abc");
        assert_eq!(work_id(" abc ").unwrap(), "abc");
        assert!(work_id("j:").is_err());
        assert!(work_id("a/b").is_err());
    }

    /// Строка итога — счёт и отказы.
    #[test]
    fn result_text_lists_rejects() {
        let v = json!({"work": "j:1", "state": "done", "accepted": [1], "skip": [],
                       "skipped": [3], "rejected": [{"n": 2, "target": "i:x", "reason": "нет"}]});
        let t = result_text(&v);
        assert!(
            t.starts_with("j:1: done — принято 1, отклонено 1, отказ 0, без итога 1"),
            "{t}"
        );
        assert!(t.contains("n 2 i:x: нет"), "{t}");
    }
}
