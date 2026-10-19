//! `zam search <query> [--all] [--mode brief|chunks|notes] [--top-k N] [--rerank]`.
//!
//! Markdown от сервера по умолчанию, `--json` — JSON ответа. Область: решение из
//! `solution.toml` (если есть) плюс Knowledge и Rules; `--all` — все решения. `--rerank` —
//! серверный реранк первых кандидатов (`rerank: "on"`, по умолчанию выключен).
//!
//! stdout — ответ сервера как есть (в md строки `> degraded: …` и `> warning: …` остаются
//! для агента). Деградация векторного канала (`degraded_reason`) и предупреждения
//! (`warnings`, например, о реранкере) дополнительно печатаются в stderr по-русски — в
//! обоих форматах, чтобы человек видел их и при `--json`, и при выводе в файл.
use serde_json::{Value, json};

use super::{Ctx, emit};
use crate::client::{CliError, read_text};

pub struct Opts<'a> {
    pub all: bool,
    pub mode: &'a str,
    pub top_k: Option<u32>,
    pub rerank: bool,
}

/// Тело запроса `/search`.
fn body(ctx: &Ctx, query: &str, o: &Opts) -> Value {
    let mut scope = json!({ "all_solutions": o.all });
    if let Some(s) = &ctx.solution {
        scope["solution"] = json!(s.id);
    }
    let mut body = json!({
        "query": query,
        "scope": scope,
        "mode": o.mode,
        "format": if ctx.json { "json" } else { "md" },
    });
    if let Some(k) = o.top_k {
        body["top_k"] = json!(k);
    }
    if o.rerank {
        body["rerank"] = json!("on");
    }
    body
}

pub fn run(ctx: &Ctx, query: &str, o: &Opts) -> Result<(), CliError> {
    let mut text = read_text(ctx.client.post_json("/search", &body(ctx, query, o))?)?;
    for n in notices(&text, ctx.json) {
        eprintln!("{n}");
    }
    // в md пустая выдача — пустое тело или только служебные строки сервера (`> degraded: …`,
    // `> warning: …`): их тоже печатаем, агент должен знать, что искал только FTS
    let empty = if ctx.json {
        text.is_empty()
    } else {
        text.lines().all(|l| {
            l.trim().is_empty() || l.starts_with("> degraded:") || l.starts_with("> warning:")
        })
    };
    if empty {
        eprintln!("ничего не найдено");
        if text.trim().is_empty() {
            return Ok(());
        }
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    emit(text.as_bytes())
}

/// Предупреждение ответа (`warnings`) — по-русски.
fn warning_text(w: &str) -> &str {
    match w {
        "reranker not installed" => {
            "реранкер не установлен (zam-server models fetch --reranker), результаты без реранка"
        }
        "reranker failed" => "реранкер не сработал (см. журнал сервера), результаты без реранка",
        other => other,
    }
}

/// Строки для stderr: деградация и предупреждения из ответа (JSON или Markdown). Текст
/// причины берётся из ответа сервера (`degraded_text` или строка `> degraded: <код> —
/// <текст> (только FTS)`): словарь причин один — `DegradedReason::text` сервера.
fn notices(text: &str, json: bool) -> Vec<String> {
    // (код, текст)
    let (reason, warnings): (Option<(String, String)>, Vec<String>) = if json {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let reason = (v["degraded"] == json!(true)).then(|| {
            let code = v["degraded_reason"].as_str().unwrap_or("").to_string();
            let text = v["degraded_text"].as_str().unwrap_or("").to_string();
            (code, text)
        });
        let warnings = v["warnings"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (reason, warnings)
    } else {
        let reason = text.lines().find_map(|l| {
            let r = l.strip_prefix("> degraded: ")?;
            let r = r.strip_suffix(" (только FTS)").unwrap_or(r).trim();
            let (code, text) = r.split_once(" — ").unwrap_or((r, ""));
            Some((code.to_string(), text.to_string()))
        });
        let warnings = text
            .lines()
            .filter_map(|l| l.strip_prefix("> warning: ").map(|w| w.trim().to_string()))
            .collect();
        (reason, warnings)
    };
    let mut out = Vec::new();
    if let Some((code, text)) = reason {
        let why = match (text.is_empty(), code.is_empty()) {
            (false, false) => format!("{text} [{code}]"),
            (false, true) => text,
            (true, false) => code,
            (true, true) => "причина не указана".to_string(),
        };
        out.push(format!(
            "векторный поиск недоступен: {why} — показаны результаты полнотекстового поиска"
        ));
    }
    out.extend(
        warnings
            .iter()
            .map(|w| format!("предупреждение: {}", warning_text(w))),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_from_json() {
        let t = r#"{"items":[],"degraded":true,"degraded_reason":"no_model","degraded_text":"модели не загружены","warnings":["reranker failed"]}"#;
        assert_eq!(
            notices(t, true),
            [
                "векторный поиск недоступен: модели не загружены [no_model] — показаны результаты полнотекстового поиска",
                "предупреждение: реранкер не сработал (см. журнал сервера), результаты без реранка",
            ]
        );
        // сервер без текста — код
        let t = r#"{"items":[],"degraded":true,"degraded_reason":"x","warnings":[]}"#;
        assert_eq!(
            notices(t, true),
            ["векторный поиск недоступен: x — показаны результаты полнотекстового поиска"]
        );
        let t = r#"{"items":[],"degraded":false,"degraded_reason":null,"warnings":[]}"#;
        assert!(notices(t, true).is_empty());
        // не JSON — молча ничего
        assert!(notices("oops", true).is_empty());
    }

    #[test]
    fn notices_from_markdown() {
        let t = "1. **a**\n> degraded: qdrant_down — Qdrant недоступен (только FTS)\n\
                 > warning: reranker not installed\n";
        let n = notices(t, false);
        assert_eq!(
            n,
            [
                "векторный поиск недоступен: Qdrant недоступен [qdrant_down] — показаны результаты полнотекстового поиска",
                "предупреждение: реранкер не установлен (zam-server models fetch --reranker), результаты без реранка",
            ]
        );
        // старый формат без текста и неизвестное предупреждение — как есть
        let n = notices("> degraded: new_reason (только FTS)\n> warning: x\n", false);
        assert!(n[0].contains(": new_reason — показаны"), "{n:?}");
        assert_eq!(n[1], "предупреждение: x");
        assert!(notices("1. **a**\n", false).is_empty());
    }
}
