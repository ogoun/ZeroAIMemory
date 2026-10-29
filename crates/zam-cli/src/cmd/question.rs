//! `zam question` — вопросы к человеку (M4.5 §4): `list`, `ask`, `answer`.
//!
//! - `list [--status S,…] [--global] [--solution GUID]` — строки `<id> [статус] «вопрос»`; без
//!   `--status` — `open`. У `answered` — ответ, у `open` — варианты;
//! - `ask --title T [--option key=text]… [--recommend key] [--body-file F | stdin] [--force]` —
//!   новый вопрос (`<id>@<rev>`); тот же заголовок у `open` или `answered` — код 4 со списком
//!   кандидатов (у отвеченного — его ответ); `--force` записывает всё равно;
//! - `answer <q:id> (--option key | --text …) [--channel chat|cli]` — ответ человека, записанный
//!   по его словам (`by: human`); становится решением `i:<id>`. Уже отвечен — код 4.
//!
//! Ссылка — `q:<id>`, `e:<id>` или голый id.
use std::path::Path;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, emit, emit_json, post_entry, solution_guid};
use crate::client::{CliError, EXIT_CONFLICT, read_json};

/// Статусы ленты по умолчанию.
const OPEN: &str = "open";

/// `q:<id>` / `e:<id>` … → голый id.
fn bare_id(raw: &str) -> &str {
    let t = raw.trim();
    match t.split_once(':') {
        Some((p, id)) if ["i", "c", "r", "t", "k", "q", "e"].contains(&p) => id,
        _ => t,
    }
}

/// Ответ кратко: текст варианта, иначе свободный текст, иначе ключ.
fn answer_brief(e: &Value) -> String {
    let a = &e["data"]["answer"];
    let text = a["option"]
        .as_str()
        .and_then(|k| {
            e["data"]["options"]
                .as_array()?
                .iter()
                .find(|o| o["key"] == k)?["text"]
                .as_str()
        })
        .or_else(|| a["text"].as_str())
        .or_else(|| a["option"].as_str())
        .unwrap_or("");
    text.to_string()
}

/// Строка ленты: `<id> [статус] «вопрос»`, далее — варианты или ответ.
fn line(e: &Value) -> String {
    let mut s = format!(
        "{} [{}] «{}»",
        e["id"].as_str().unwrap_or("?"),
        e["status"].as_str().unwrap_or("?"),
        e["title"].as_str().unwrap_or("")
    );
    if e["status"] == "answered" {
        s.push_str(&format!(" → {}", answer_brief(e)));
        if let Some(d) = e["data"]["answer"]["decision_id"].as_str() {
            s.push_str(&format!(" · решение i:{d}"));
        }
    } else if let Some(opts) = e["data"]["options"].as_array().filter(|o| !o.is_empty()) {
        let rec = e["data"]["recommendation"].as_str();
        let list: Vec<String> = opts
            .iter()
            .map(|o| {
                let k = o["key"].as_str().unwrap_or("?");
                let star = if rec == Some(k) { "*" } else { "" };
                format!("{k}{star}={}", o["text"].as_str().unwrap_or(""))
            })
            .collect();
        s.push_str(&format!(" · варианты: {}", list.join("; ")));
    }
    s
}

/// `zam question list`.
pub fn list(
    ctx: &Ctx,
    status: Option<&str>,
    global: bool,
    solution: Option<&str>,
) -> Result<(), CliError> {
    let space = if global {
        "knowledge".to_string()
    } else {
        solution_guid(ctx, solution)?
    };
    let v = read_json(ctx.client.get(
        &format!("/spaces/{space}/entries"),
        &[
            ("kind", "question"),
            ("status", status.unwrap_or(OPEN)),
            ("limit", "200"),
        ],
    )?)?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut out = String::new();
    for e in v["items"].as_array().into_iter().flatten() {
        out.push_str(&line(e));
        out.push('\n');
    }
    emit(out.as_bytes())
}

/// `key=text` → вариант ответа; пустой ключ или текст — код 6.
pub fn parse_option(raw: &str) -> Result<Value, CliError> {
    match raw.split_once('=') {
        Some((k, t)) if !k.trim().is_empty() && !t.trim().is_empty() => {
            Ok(json!({ "key": k.trim(), "text": t.trim() }))
        }
        _ => Err(CliError::input(format!(
            "--option «{raw}»: формат key=текст варианта"
        ))),
    }
}

/// Параметры `zam question ask`.
pub struct Ask<'a> {
    pub title: &'a str,
    pub options: &'a [String],
    pub recommend: Option<&'a str>,
    pub body_file: Option<&'a Path>,
    pub area: Option<&'a str>,
    pub global: bool,
    pub solution: Option<&'a str>,
    pub key: Option<&'a str>,
    pub force: bool,
}

/// Тело запроса вопроса (`POST /spaces/{space}/entries`, `kind: question`).
pub fn ask_body(o: &Ask, body: &str) -> Result<Value, CliError> {
    let mut data = json!({});
    if !o.options.is_empty() {
        data["options"] = Value::Array(
            o.options
                .iter()
                .map(|x| parse_option(x))
                .collect::<Result<_, _>>()?,
        );
    }
    if let Some(r) = o.recommend {
        data["recommendation"] = json!(r);
    }
    let mut req = json!({ "kind": "question", "title": o.title, "body": body, "data": data });
    if o.force {
        req["force"] = json!(true);
    }
    Ok(req)
}

/// 409 possible-duplicate вопроса → кандидаты (у отвеченного — ответ) в stderr, код 4.
fn explain_duplicate(e: CliError) -> CliError {
    let Some(p) = e
        .problem
        .as_ref()
        .filter(|p| p["type"] == "/problems/possible-duplicate")
    else {
        return e;
    };
    let mut msg = String::from("такой вопрос уже задан:\n");
    for c in p["candidates"].as_array().into_iter().flatten() {
        msg.push_str(&format!(
            "  {} [{}] «{}»",
            c["id"].as_str().unwrap_or("?"),
            c["status"].as_str().unwrap_or("?"),
            c["title"].as_str().unwrap_or("")
        ));
        let a = &c["answer"];
        if a.is_object() {
            let ans = a["text"]
                .as_str()
                .or_else(|| a["option"].as_str())
                .unwrap_or("");
            msg.push_str(&format!(
                " → ответ: {ans} (решение i:{})",
                a["decision_id"].as_str().unwrap_or("?")
            ));
        }
        msg.push('\n');
    }
    msg.push_str("что дальше: используйте ответ или повторите с --force");
    CliError {
        code: EXIT_CONFLICT,
        msg,
        problem: e.problem,
    }
}

/// `zam question ask`.
pub fn ask(ctx: &Ctx, o: &Ask) -> Result<(), CliError> {
    let space = if o.global {
        "knowledge".to_string()
    } else {
        solution_guid(ctx, o.solution)?
    };
    let body = read_text(o.body_file)?;
    let req = ask_body(o, &body)?;
    post_entry(ctx, &space, &req, o.key, o.area).map_err(explain_duplicate)
}

/// Тело `POST /entries/{id}/answer`.
pub fn answer_body(option: Option<&str>, text: Option<&str>, channel: &str) -> Value {
    let mut b = json!({ "channel": channel });
    if let Some(o) = option {
        b["option"] = json!(o);
    }
    if let Some(t) = text {
        b["text"] = json!(t);
    }
    b
}

/// 409 already-answered → «вопрос уже отвечен (решение i:…)», код 4.
fn explain_answered(e: CliError) -> CliError {
    let Some(p) = e
        .problem
        .as_ref()
        .filter(|p| p["type"] == "/problems/already-answered")
    else {
        return e;
    };
    let d = p["decision_id"].as_str().unwrap_or("?");
    CliError {
        code: EXIT_CONFLICT,
        msg: format!("вопрос уже отвечен (решение i:{d})"),
        problem: e.problem,
    }
}

/// `zam question answer`.
pub fn answer(
    ctx: &Ctx,
    id: &str,
    option: Option<&str>,
    text: Option<&str>,
    channel: &str,
) -> Result<(), CliError> {
    if option.is_none() && text.is_none() {
        return Err(CliError::input("нужен --option или --text"));
    }
    let id = bare_id(id);
    let r = ctx
        .client
        .post_json(
            &format!("/entries/{id}/answer"),
            &answer_body(option, text, channel),
        )
        .map_err(explain_answered)?;
    let v = read_json(r)?;
    ctx.note_write("question");
    if ctx.json {
        return emit_json(&v);
    }
    emit(
        format!(
            "{id} отвечен → решение i:{}\n",
            v["decision_id"].as_str().unwrap_or("?")
        )
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_options() {
        assert_eq!(bare_id(" q:01ABC "), "01ABC");
        assert_eq!(bare_id("01ABC"), "01ABC");
        assert_eq!(
            parse_option("a=Да = точно").unwrap(),
            json!({"key": "a", "text": "Да = точно"})
        );
        for bad in ["a", "=x", "a=", " = "] {
            assert_eq!(
                parse_option(bad).unwrap_err().code,
                crate::client::EXIT_INPUT,
                "{bad}"
            );
        }
    }

    #[test]
    fn ask_and_answer_bodies() {
        let opts = vec!["a=Да".to_string(), "b=Нет".to_string()];
        let o = Ask {
            title: "Брать?",
            options: &opts,
            recommend: Some("a"),
            body_file: None,
            area: None,
            global: false,
            solution: None,
            key: None,
            force: true,
        };
        let b = ask_body(&o, "контекст").unwrap();
        assert_eq!(b["kind"], "question");
        assert_eq!(b["body"], "контекст");
        assert_eq!(b["data"]["options"][1], json!({"key": "b", "text": "Нет"}));
        assert_eq!(b["data"]["recommendation"], "a");
        assert_eq!(b["force"], true);
        assert_eq!(
            answer_body(Some("a"), None, "chat"),
            json!({"channel": "chat", "option": "a"})
        );
        assert_eq!(
            answer_body(None, Some("т"), "cli"),
            json!({"channel": "cli", "text": "т"})
        );
    }

    #[test]
    fn lines() {
        let open = json!({"id": "01A", "status": "open", "title": "Брать?",
            "data": {"options": [{"key": "a", "text": "Да"}, {"key": "b", "text": "Нет"}],
                     "recommendation": "a"}});
        assert_eq!(line(&open), "01A [open] «Брать?» · варианты: a*=Да; b=Нет");
        let done = json!({"id": "01B", "status": "answered", "title": "Брать?",
            "data": {"options": [{"key": "a", "text": "Да"}],
                     "answer": {"option": "a", "decision_id": "01D"}}});
        assert_eq!(line(&done), "01B [answered] «Брать?» → Да · решение i:01D");
    }

    #[test]
    fn errors_explained() {
        let dup = CliError {
            code: 4,
            msg: "x".into(),
            problem: Some(json!({
                "type": "/problems/possible-duplicate",
                "candidates": [{"id": "01A", "status": "answered", "title": "Брать?",
                                "answer": {"text": "Да", "decision_id": "01D"}}],
            })),
        };
        let r = explain_duplicate(dup);
        assert_eq!(r.code, EXIT_CONFLICT);
        assert!(
            r.msg
                .contains("01A [answered] «Брать?» → ответ: Да (решение i:01D)"),
            "{}",
            r.msg
        );
        let ans = CliError {
            code: 4,
            msg: "x".into(),
            problem: Some(json!({"type": "/problems/already-answered", "decision_id": "01D"})),
        };
        assert_eq!(
            explain_answered(ans).msg,
            "вопрос уже отвечен (решение i:01D)"
        );
    }
}
