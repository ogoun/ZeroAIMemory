//! `zam tree [target] [--depth N]` — дерево пространства с L0.
//!
//! Вывод — список Markdown: по строке на узел, отступ два пробела на уровень,
//! папка — с `/` на конце, сводка L0 — после ` — `. L0 папки — свёртка её детей (`<имя>: <L0>`
//! строками): дети есть в выводе — не печатается (повторяет их); папка на границе глубины —
//! первая строка свёртки и «(+K)» (M6.6 §9, 16б; так же MCP `memory_tree`).
use std::collections::HashSet;

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
    // пути родителей узлов вывода — папки, чьи дети показаны
    let shown: HashSet<&str> = items
        .iter()
        .filter_map(|it| Some(it["path"].as_str()?.rsplit_once('/')?.0))
        .collect();
    let mut out = String::new();
    for it in items {
        let path = it["path"].as_str().unwrap_or("");
        let level = segments(path).saturating_sub(base_depth + 1);
        out.push_str(&"  ".repeat(level));
        out.push_str("- ");
        out.push_str(it["name"].as_str().unwrap_or(path));
        let l0 = it["l0"].as_str().unwrap_or("");
        let summary = if it["kind"] == "folder" {
            out.push('/');
            folder_summary(l0, shown.contains(path))
        } else {
            one_line(l0)
        };
        if !summary.is_empty() {
            out.push_str(" — ");
            out.push_str(&summary);
        }
        out.push('\n');
    }
    out
}

/// L0 папки в дереве: её дети показаны — пусто; иначе (граница глубины) — первая непустая
/// строка свёртки и «(+K)», K — остальные непустые строки.
fn folder_summary(l0: &str, children_shown: bool) -> String {
    if children_shown {
        return String::new();
    }
    let mut lines = l0.lines().map(one_line).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return String::new();
    };
    match lines.count() {
        0 => first,
        more => format!("{first} (+{more})"),
    }
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
        // M6.6 §9 (16б): у папки, чьи дети есть в выдаче, L0 (свёртка детей) не печатается; у
        // папки на границе глубины — первая строка свёртки и «(+K)», как у MCP `memory_tree`
        let items = vec![
            json!({"path": "a", "name": "a", "kind": "folder", "l0": "b: про b\nc: про c"}),
            json!({"path": "a/b", "name": "b", "kind": "note", "l0": "про b"}),
            json!({"path": "a/c", "name": "c", "kind": "folder",
                   "l0": "x: про   x\n\ny/: про y\nz"}),
            json!({"path": "d", "name": "d", "kind": "folder", "l0": "e: про e"}),
        ];
        assert_eq!(
            render("", &items),
            "- a/\n  - b — про b\n  - c/ — x: про x (+2)\n- d/ — e: про e\n"
        );
        // поддерево: папка base в выдачу не входит, её дети — с отступа 0
        let items = vec![
            json!({"path": "a/c", "name": "c", "kind": "folder", "l0": "x: про x"}),
            json!({"path": "a/c/x", "name": "x", "kind": "note", "l0": "про x"}),
        ];
        assert_eq!(render("a", &items), "- c/\n  - x — про x\n");
    }
}
