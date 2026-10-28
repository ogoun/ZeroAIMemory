//! `zam glossary [<термин>] [--solution GUID]` — словарь домена (M4.5 §2).
//!
//! Без аргумента — список действующих терминов решения строками «имя — определение»;
//! с именем или псевдонимом — карточка термина (псевдонимы, определение, «не путать с»,
//! идентификаторы кода, подробности). `--json` — ответ сервера как есть
//! (`GET /spaces/{guid}/glossary[?term=…]`). Термин не найден → код 5.
//! Запись терминов — `zam log term`.
use serde_json::Value;

use super::{Ctx, emit, emit_json, solution_guid};
use crate::client::{CliError, EXIT_NOT_FOUND, read_json};
use crate::target::Loc;

/// Строки списка: `имя — определение`.
fn list_text(v: &Value) -> String {
    let mut out = String::new();
    for t in v["items"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "{} — {}\n",
            t["title"].as_str().unwrap_or("?"),
            t["definition"].as_str().unwrap_or("")
        ));
    }
    out
}

/// Строка `Заголовок: a, b` для непустого массива строк.
fn list_line(label: &str, v: &Value) -> Option<String> {
    let items: Vec<&str> = v.as_array()?.iter().filter_map(Value::as_str).collect();
    (!items.is_empty()).then(|| format!("{label}: {}\n", items.join(", ")))
}

/// Карточка термина (запись `Entry`).
fn card_text(e: &Value) -> String {
    let d = &e["data"];
    let mut out = format!("{}\n", e["title"].as_str().unwrap_or("?"));
    out.push_str(&format!(
        "Определение: {}\n",
        d["definition"].as_str().unwrap_or("")
    ));
    for (label, key) in [
        ("Псевдонимы", "aliases"),
        ("Не путать с", "not_to_confuse"),
        ("Код", "code"),
    ] {
        if let Some(l) = list_line(label, &d[key]) {
            out.push_str(&l);
        }
    }
    if let Some(c) = e["certainty"].as_str() {
        out.push_str(&format!("Достоверность: {c}\n"));
    }
    out.push_str(&format!(
        "t:{} rev{}\n",
        e["id"].as_str().unwrap_or("?"),
        e["rev"]
    ));
    let body = e["body"].as_str().unwrap_or("").trim();
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    out
}

/// `zam glossary`.
pub fn run(ctx: &Ctx, term: Option<&str>, solution: Option<&str>) -> Result<(), CliError> {
    let space = solution_guid(ctx, solution)?;
    // решение из solution.toml регистрируется (идемпотентно), как при записи
    let own = ctx.solution.as_ref().is_some_and(|s| s.id == space);
    ctx.ensure_solution(&Loc::Path {
        space: space.clone(),
        path: String::new(),
        own_solution: own,
    })?;
    let path = format!("/spaces/{space}/glossary");
    match term.map(str::trim).filter(|t| !t.is_empty()) {
        Some(name) => {
            let r = ctx.client.get(&path, &[("term", name)]).map_err(|e| {
                if e.code == EXIT_NOT_FOUND {
                    CliError {
                        code: EXIT_NOT_FOUND,
                        msg: format!("термин «{name}» не найден среди имён и псевдонимов"),
                        problem: e.problem,
                    }
                } else {
                    e
                }
            })?;
            let e = read_json(r)?;
            if ctx.json {
                return emit_json(&e);
            }
            emit(card_text(&e).as_bytes())
        }
        None => {
            let v = read_json(ctx.client.get(&path, &[])?)?;
            if ctx.json {
                return emit_json(&v);
            }
            emit(list_text(&v).as_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn list_lines() {
        let v = json!({"items": [
            {"title": "Выжимка", "definition": "Короткая сводка"},
            {"title": "Чанк", "definition": "Кусок текста"},
        ]});
        assert_eq!(
            list_text(&v),
            "Выжимка — Короткая сводка\nЧанк — Кусок текста\n"
        );
        assert_eq!(list_text(&json!({"items": []})), "");
    }

    #[test]
    fn card_lines() {
        let e = json!({
            "id": "abc", "rev": 2, "title": "Выжимка", "body": "Подробности\n",
            "certainty": "hypothesis",
            "data": {"definition": "Сводка", "aliases": ["summary", "digest"],
                     "not_to_confuse": ["Заметка"], "code": []},
        });
        assert_eq!(
            card_text(&e),
            "Выжимка\nОпределение: Сводка\nПсевдонимы: summary, digest\nНе путать с: Заметка\n\
             Достоверность: hypothesis\nt:abc rev2\n\nПодробности\n"
        );
    }
}
