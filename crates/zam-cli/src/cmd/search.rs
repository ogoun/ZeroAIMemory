//! `zam search <query> [--all] [--mode brief|chunks|notes] [--top-k N] [--rerank] [--budget N]
//! [--kind K]… [--insight-kind K]… [--certainty C]… [--status S]… [--decided-by human|agent]
//! [--file P] [--history]`.
//!
//! Markdown от сервера по умолчанию, `--json` — JSON ответа. Область: решение из
//! `solution.toml` (если есть) плюс Knowledge и Rules; `--all` — все решения. `--rerank` —
//! серверный реранк первых кандидатов (`rerank: "on"`, по умолчанию выключен). Бюджет и фильтры
//! (M6.6 §9, замечание 11) — поля запроса, которые сервер знает с 0.0.6-m4.5: `budget_tokens`,
//! `filters.{kinds, insight_kinds, certainty, status, decided_by, file}`, `include_history`;
//! путь `--file` нормализуется как путь ссылки ([`refs::ref_path`]); незаданные поля не
//! уходят (тело — как раньше).
//!
//! stdout — ответ сервера как есть (в md строки `> degraded: …` и `> warning: …` остаются
//! для агента). Деградация векторного канала (`degraded_reason`) и предупреждения
//! (`warnings`, например, о реранкере) дополнительно печатаются в stderr по-русски — в
//! обоих форматах, чтобы человек видел их и при `--json`, и при выводе в файл. Так же —
//! усечение выдачи: «показано K из L: бюджет — ещё N (--budget), слабые — M» по полю
//! `omitted` (JSON) или заголовку `X-Zam-Omitted` (Markdown); у сервера до 0.0.13-m6.6 их нет —
//! строки нет.
use serde_json::{Map, Value, json};

use super::{Ctx, emit, refs};
use crate::client::{CliError, read_text};

/// Заголовок ответа Markdown с числами усечения (`budget=N; weak=M`, сервер 0.0.13-m6.6).
const OMITTED_HEADER: &str = "x-zam-omitted";

pub struct Opts<'a> {
    pub all: bool,
    pub mode: &'a str,
    pub top_k: Option<u32>,
    pub rerank: bool,
    /// `budget_tokens` (1–200000, проверил clap).
    pub budget: Option<u32>,
    pub kinds: &'a [String],
    /// Виды insight (проверил clap).
    pub insight_kinds: &'a [String],
    /// `axiom | theorem | hypothesis` (проверил clap).
    pub certainty: &'a [String],
    pub status: &'a [String],
    /// `human | agent` (проверил clap).
    pub decided_by: Option<&'a str>,
    /// Путь файла как введён (нормализует [`file_filter`]).
    pub file: Option<&'a str>,
    pub history: bool,
}

/// Путь `--file` в форме пути ссылки: относительно корня проекта, `/`, регистр файловой системы
/// ([`refs::ref_path`], как у `zam links file:`); вне проекта и неверный — код 6.
fn file_filter(raw: &str) -> Result<String, CliError> {
    let (cwd, root) = refs::cli_root();
    refs::ref_path(raw, &cwd, &root).map_err(|e| CliError::input(format!("--file {raw}: {e}")))
}

/// `filters` запроса: только заданные флаги; ни одного — `None` (поля в теле нет).
fn filters(o: &Opts, file: Option<&str>) -> Option<Value> {
    let mut f = Map::new();
    for (k, v) in [
        ("kinds", o.kinds),
        ("insight_kinds", o.insight_kinds),
        ("certainty", o.certainty),
        ("status", o.status),
    ] {
        if !v.is_empty() {
            f.insert(k.into(), json!(v));
        }
    }
    if let Some(d) = o.decided_by {
        f.insert("decided_by".into(), json!(d));
    }
    if let Some(p) = file {
        f.insert("file".into(), json!(p));
    }
    (!f.is_empty()).then_some(Value::Object(f))
}

/// Тело запроса `/search`; `file` — уже нормализованный путь `--file`.
fn body(ctx: &Ctx, query: &str, o: &Opts, file: Option<&str>) -> Value {
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
    if let Some(b) = o.budget {
        body["budget_tokens"] = json!(b);
    }
    if let Some(f) = filters(o, file) {
        body["filters"] = f;
    }
    if o.history {
        body["include_history"] = json!(true);
    }
    body
}

pub fn run(ctx: &Ctx, query: &str, o: &Opts) -> Result<(), CliError> {
    let file = o.file.map(file_filter).transpose()?;
    let resp = ctx
        .client
        .post_json("/search", &body(ctx, query, o, file.as_deref()))?;
    let header = resp
        .headers()
        .get(OMITTED_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(omitted_header);
    let mut text = read_text(resp)?;
    for n in notices(&text, ctx.json) {
        eprintln!("{n}");
    }
    let omitted = if ctx.json {
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| json_omitted(&v))
    } else {
        header.map(|h| (md_cards(&text), h))
    };
    if let Some(line) = omitted.and_then(|(shown, o)| omitted_notice(shown, o)) {
        eprintln!("{line}");
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
            "реранкер не установлен (zam-server models fetch --reranker; для CPU-реранкера \
             jina — ещё --accept-nc-license), результаты без реранка"
        }
        "index rebuilding" => {
            "индекс пересобирается (reindex): векторная выдача может быть неполной"
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

/// Не вошедшие в ответ карточки (M6.6 §9): не поместились в бюджет и отсечены как слабые.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Omitted {
    budget: u64,
    weak: u64,
}

/// Заголовок [`OMITTED_HEADER`] `budget=N; weak=M`; иная форма — `None` (молча).
fn omitted_header(s: &str) -> Option<Omitted> {
    let (b, w) = s.split_once("; ")?;
    Some(Omitted {
        budget: b.strip_prefix("budget=")?.parse().ok()?,
        weak: w.strip_prefix("weak=")?.parse().ok()?,
    })
}

/// Показано карточек и `omitted` ответа JSON; поля нет (сервер до 0.0.13-m6.6) — `None`.
/// Выжимки шага 0 (`kind: cache`) — не карточки выдачи.
fn json_omitted(v: &Value) -> Option<(usize, Omitted)> {
    let o = &v["omitted"];
    let omitted = Omitted {
        budget: o["budget"].as_u64()?,
        weak: o["weak"].as_u64()?,
    };
    let shown = v["items"]
        .as_array()
        .map_or(0, |a| a.iter().filter(|i| i["kind"] != "cache").count());
    Some((shown, omitted))
}

/// Показано карточек Markdown: номер последней строки `<n>. …`. Карточки нумеруются подряд и
/// идут после выжимок шага 0, их продолжения — с отступа, строки хвоста — с `> `, поэтому
/// последний номер — их число, даже если в тексте выжимки есть нумерованный список.
fn md_cards(md: &str) -> usize {
    md.lines()
        .rev()
        .find_map(|l| l.split_once(". ")?.0.parse::<usize>().ok())
        .unwrap_or(0)
}

/// Строка stderr об усечении (M6.6 §9): «показано K из L: бюджет — ещё N (--budget), слабые —
/// M», L = K + N + M; ничего не срезано — `None`.
fn omitted_notice(shown: usize, o: Omitted) -> Option<String> {
    let more = o.budget + o.weak;
    (more > 0).then(|| {
        format!(
            "показано {shown} из {}: бюджет — ещё {} (--budget), слабые — {}",
            shown as u64 + more,
            o.budget,
            o.weak
        )
    })
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
                 > warning: reranker not installed\n> warning: index rebuilding\n";
        let n = notices(t, false);
        assert_eq!(
            n,
            [
                "векторный поиск недоступен: Qdrant недоступен [qdrant_down] — показаны результаты полнотекстового поиска",
                "предупреждение: реранкер не установлен (zam-server models fetch --reranker; для CPU-реранкера jina — ещё --accept-nc-license), результаты без реранка",
                "предупреждение: индекс пересобирается (reindex): векторная выдача может быть неполной",
            ]
        );
        // старый формат без текста и неизвестное предупреждение — как есть
        let n = notices("> degraded: new_reason (только FTS)\n> warning: x\n", false);
        assert!(n[0].contains(": new_reason — показаны"), "{n:?}");
        assert_eq!(n[1], "предупреждение: x");
        assert!(notices("1. **a**\n", false).is_empty());
    }

    /// M6.6 §9 (замечание 11): строка об усечении — только при N + M > 0, L = K + N + M;
    /// заголовок `X-Zam-Omitted` разбирается строго (иначе — молча, как у старого сервера).
    #[test]
    fn omitted_notice_and_header() {
        assert_eq!(
            omitted_notice(1, Omitted { budget: 2, weak: 1 }).as_deref(),
            Some("показано 1 из 4: бюджет — ещё 2 (--budget), слабые — 1")
        );
        assert_eq!(
            omitted_notice(6, Omitted { budget: 0, weak: 3 }).as_deref(),
            Some("показано 6 из 9: бюджет — ещё 0 (--budget), слабые — 3")
        );
        assert_eq!(omitted_notice(5, Omitted { budget: 0, weak: 0 }), None);
        assert_eq!(
            omitted_header("budget=3; weak=0"),
            Some(Omitted { budget: 3, weak: 0 })
        );
        for bad in ["", "budget=3", "budget=x; weak=1", "weak=1; budget=2"] {
            assert_eq!(omitted_header(bad), None, "{bad}");
        }
        let j = r#"{"items":[{"kind":"cache"},{"kind":"note"},{"kind":"insight"}],"omitted":{"budget":1,"weak":0}}"#;
        let v: Value = serde_json::from_str(j).unwrap();
        assert_eq!(json_omitted(&v), Some((2, Omitted { budget: 1, weak: 0 })));
        assert_eq!(json_omitted(&json!({"items": []})), None);
        // карточки Markdown — строки `<n>. …`, выжимки и строки хвоста не считаются
        let md = "[cache] «q» — выжимка\n   текст\n1. n:01 K/a  [1.0]\n   фрагмент\n\
                  2. i:02 insight/gotcha «x»\n> degraded: x (только FTS)\n";
        assert_eq!(md_cards(md), 2);
    }
}
