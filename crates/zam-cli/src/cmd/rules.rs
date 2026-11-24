//! `zam rules` — кандидаты в Rules (M7.3 §6.7 п. 2): агент предлагает правило, человек решает
//! в WebUI «Кандидаты в Rules».
//!
//! - `zam rules candidate <путь> --file F --reason R [--basis ID]…` — спонтанный кандидат
//!   `POST /proposals {type: "rule", path, content, reason, basis}`: путь — `rules:/<путь>` или
//!   путь заметки Rules, текст — из файла (до 200 токенов, без URL и блоков кода), довод — почему
//!   это правило для всех проектов (20–2000 символов). Тот же путь и текст — прежнее предложение.
//! - `zam rules candidates [--status S]` — свои кандидаты (`GET /proposals?author=me`), по
//!   умолчанию — все, кроме отказов исполнителя.
//!
//! Сервер без признака `proposals` в `GET /health` `features` — код 1 с подсказкой обновить
//! `zam-server` ([`OLD_SERVER`]).
use std::path::Path;

use serde_json::{Value, json};

use super::{Ctx, emit, emit_json, old_server};
use crate::client::{CliError, read_json};

/// Признак кандидатов в Rules в `GET /health` `features` (`docs/api-compat.md`).
const FEATURE: &str = "proposals";
/// Сообщение для сервера без кандидатов в Rules.
pub const OLD_SERVER: &str = "сервер не поддерживает кандидаты в Rules (/proposals): обновите \
     zam-server до 0.0.16-m7.3 или новее";

/// Ошибка запроса кандидатов: так отвечает старый сервер (404 — нет маршрута, 405 — нет метода).
fn old(ctx: &Ctx, e: CliError) -> CliError {
    old_server(ctx, e, &[404, 405], FEATURE, OLD_SERVER)
}

/// Путь заметки Rules из `rules:/<путь>` или пути; другое пространство — ошибка ввода.
fn rule_path(raw: &str) -> Result<String, CliError> {
    use zam_core::space::{SpaceSel, split_space};
    // префикс пространства — по общему правилу CLI (`zam tree|read|write`)
    let s = match split_space(raw.trim()) {
        (SpaceSel::Fixed("rules") | SpaceSel::Default, p) => p,
        _ => "",
    };
    if s.is_empty() || s.contains(":/") {
        return Err(CliError::input(
            "путь кандидата — rules:/<путь> или путь заметки Rules (кандидат бывает только в Rules)",
        ));
    }
    Ok(s.to_string())
}

/// `zam rules candidate <путь> --file F --reason R [--basis ID]…`.
pub fn candidate(
    ctx: &Ctx,
    path: &str,
    file: &Path,
    reason: &str,
    basis: &[String],
) -> Result<(), CliError> {
    let path = rule_path(path)?;
    let content = std::fs::read_to_string(file)
        .map_err(|e| CliError::input(format!("{}: {e}", file.display())))?;
    let mut body = json!({"type": "rule", "path": path, "content": content, "reason": reason});
    if !basis.is_empty() {
        body["basis"] = json!(basis);
    }
    let v = ctx
        .client
        .post_json("/proposals", &body)
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    if ctx.json {
        return emit_json(&v);
    }
    let o = v["proposal"].as_str().unwrap_or("?");
    let line = if v["existing"] == true {
        format!(
            "{o} — уже есть ({}): тот же путь и текст\n",
            v["status"].as_str().unwrap_or("?")
        )
    } else {
        format!(
            "{o} — кандидат в Rules rules:/{path}: решает человек (WebUI «Кандидаты в Rules»)\n"
        )
    };
    emit(line.as_bytes())
}

/// Строка кандидата: `o:<id> [статус] rules:/<путь> · <первая строка текста> · <время UTC>`.
fn line(p: &Value) -> String {
    let first = p["content"]
        .as_str()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let first: String = match first.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &first[..i]),
        None => first.to_string(),
    };
    format!(
        "{} [{}] rules:/{} · {first} · {}\n",
        p["id"].as_str().unwrap_or("?"),
        p["status"].as_str().unwrap_or("?"),
        p["path"].as_str().unwrap_or("?"),
        p["created_at"]
            .as_str()
            .map(super::agent_time)
            .unwrap_or_default(),
    )
}

/// `zam rules candidates [--status S]`.
pub fn candidates(ctx: &Ctx, status: Option<&str>) -> Result<(), CliError> {
    let mut q = vec![("author", "me")];
    if let Some(s) = status {
        q.push(("status", s));
    }
    let v = ctx
        .client
        .get("/proposals", &q)
        .and_then(read_json)
        .map_err(|e| old(ctx, e))?;
    if ctx.json {
        return emit_json(&v);
    }
    let list = v["proposals"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return emit("кандидатов нет\n".as_bytes());
    }
    let out: String = list.iter().map(line).collect();
    emit(out.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Путь: префикс `rules:` снимается, чужое пространство и пусто — отказ.
    #[test]
    fn rule_path_cases() {
        assert_eq!(rule_path("rules:/a/b").unwrap(), "a/b");
        assert_eq!(rule_path("a/b").unwrap(), "a/b");
        for bad in ["", "rules:", "knowledge:/a", "knowledge:a", "x:/y"] {
            assert!(rule_path(bad).is_err(), "{bad}");
        }
    }

    /// Строка кандидата — ссылка, статус, путь, первая строка текста.
    #[test]
    fn line_cases() {
        let p = json!({"id": "o:1", "status": "proposed", "path": "a/b",
                       "content": "\n# Правило\nТекст", "created_at": "2026-11-24T10:00:00.000000Z"});
        let l = line(&p);
        assert!(
            l.starts_with("o:1 [proposed] rules:/a/b · # Правило · "),
            "{l}"
        );
    }
}
