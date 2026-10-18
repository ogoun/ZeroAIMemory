//! `zam tree [target] [--depth N]` — дерево пространства с L0.
//!
//! Вывод — список Markdown: по строке на узел, отступ два пробела на уровень,
//! папка — с `/` на конце, сводка L0 — после ` — `.
use serde_json::Value;

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, read_json};
use crate::target::Loc;

pub fn run(ctx: &Ctx, target: Option<&str>, depth: u32) -> Result<(), CliError> {
    let Loc::Path { space, path, .. } = ctx.locate(target.unwrap_or(""))? else {
        return Err(CliError::input("tree принимает [space:]path, а не id узла"));
    };
    let depth = depth.to_string();
    let v = read_json(ctx.client.get(
        &format!("/spaces/{space}/tree"),
        &[("path", path.as_str()), ("depth", depth.as_str())],
    )?)?;
    if ctx.json {
        return emit_json(&v);
    }
    let items = v["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        eprintln!("пусто");
        return Ok(());
    }
    emit(render(&path, &items).as_bytes())
}

fn segments(p: &str) -> usize {
    if p.is_empty() {
        0
    } else {
        p.split('/').count()
    }
}

/// Строки дерева: глубина узла считается от `base`.
fn render(base: &str, items: &[Value]) -> String {
    let base_depth = segments(base);
    let mut out = String::new();
    for it in items {
        let path = it["path"].as_str().unwrap_or("");
        let level = segments(path).saturating_sub(base_depth + 1);
        out.push_str(&"  ".repeat(level));
        out.push_str("- ");
        out.push_str(it["name"].as_str().unwrap_or(path));
        if it["kind"] == "folder" {
            out.push('/');
        }
        if let Some(l0) = it["l0"].as_str().map(one_line).filter(|s| !s.is_empty()) {
            out.push_str(" — ");
            out.push_str(&l0);
        }
        out.push('\n');
    }
    out
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_levels() {
        let items = vec![
            json!({"path": "a", "name": "a", "kind": "folder", "l0": null}),
            json!({"path": "a/b", "name": "b", "kind": "note", "l0": "про\nb"}),
            json!({"path": "c", "name": "c", "kind": "note", "l0": ""}),
        ];
        assert_eq!(render("", &items), "- a/\n  - b — про b\n- c\n");
        let items = vec![json!({"path": "a/b", "name": "b", "kind": "note"})];
        assert_eq!(render("a", &items), "- b\n");
    }
}
