//! `zam history <target> [--diff A..B] [--revert N]`.
//!
//! - без флагов — ревизии, новые первыми: строки `rev<TAB>ts<TAB>author_kind<TAB>
//!   machine<TAB>message` (отсутствующее — `-`);
//! - `--diff A..B` — унифицированный дифф (`A..` — до текущей ревизии);
//! - `--revert N` — новая ревизия с текстом `N`, печатается `<id>@<rev>`.
use serde_json::{Value, json};

use super::{Ctx, emit, emit_json, emit_written};
use crate::client::{CliError, read_json, read_text};

/// Действие команды.
pub enum Action {
    List,
    Diff(String),
    Revert(i64),
}

/// `A..B` или `A..` → (from, to).
pub fn parse_range(s: &str) -> Result<(i64, Option<i64>), CliError> {
    let bad = || CliError::input(format!("--diff: ожидается A..B или A.. (получено: {s})"));
    let (a, b) = s.split_once("..").ok_or_else(bad)?;
    let from: i64 = a.trim().parse().map_err(|_| bad())?;
    let to = match b.trim() {
        "" => None,
        b => Some(b.parse::<i64>().map_err(|_| bad())?),
    };
    if from < 0 || to.is_some_and(|t| t < 1) {
        return Err(bad());
    }
    Ok((from, to))
}

pub fn run(ctx: &Ctx, raw: &str, action: &Action) -> Result<(), CliError> {
    // разбор диапазона до сети: ошибка ввода — сразу
    let range = match action {
        Action::Diff(s) => Some(parse_range(s)?),
        _ => None,
    };
    let loc = ctx.locate(raw)?;
    let id = ctx.node_id(&loc)?;
    match action {
        Action::List => {
            let v = read_json(ctx.client.get(&format!("/nodes/{id}/revisions"), &[])?)?;
            if ctx.json {
                return emit_json(&v);
            }
            let items = v.as_array().cloned().unwrap_or_default();
            emit(render(&items).as_bytes())
        }
        Action::Diff(_) => {
            let (from, to) = range.expect("разобран выше");
            let from = from.to_string();
            let to = to.map(|t| t.to_string());
            let mut q = vec![("from", from.as_str())];
            if let Some(t) = to.as_deref() {
                q.push(("to", t));
            }
            let text = read_text(ctx.client.get(&format!("/nodes/{id}/diff"), &q)?)?;
            if ctx.json {
                return emit_json(&json!({ "diff": text }));
            }
            emit(text.as_bytes())
        }
        Action::Revert(rev) => {
            let node = read_json(
                ctx.client
                    .post_json(&format!("/nodes/{id}/revert"), &json!({ "rev": rev }))?,
            )?;
            emit_written(ctx, &node, false)
        }
    }
}

fn field(v: &Value) -> String {
    match v
        .as_str()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
    {
        Some(s) if !s.is_empty() => s,
        _ => "-".into(),
    }
}

fn render(items: &[Value]) -> String {
    items
        .iter()
        .map(|r| {
            format!(
                "{}\t{}\t{}\t{}\t{}\n",
                r["rev"],
                field(&r["ts"]),
                field(&r["author_kind"]),
                field(&r["machine"]),
                field(&r["message"]),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("1..2").unwrap(), (1, Some(2)));
        assert_eq!(parse_range("0..").unwrap(), (0, None));
        for bad in ["abc", "1", "..2", "1..x", "-1..2", "1..0"] {
            assert!(parse_range(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn render_lines() {
        let items = vec![
            json!({"rev": 2, "ts": "2026-10-18T00:00:00Z", "author_kind": "agent", "machine": null, "message": "a\tb\nc"}),
        ];
        assert_eq!(render(&items), "2\t2026-10-18T00:00:00Z\tagent\t-\ta b c\n");
    }
}
