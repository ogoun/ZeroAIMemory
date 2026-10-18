//! `zam write <target> [--file f.md] [--message M] [--base-rev N] [--l0 TEXT]`.
//!
//! Текст — из `--file` или stdin (байты как есть, проверяется только UTF-8).
//! Узла нет → `POST /nodes`; есть → `PUT` с `If-Match` (`--base-rev` или текущая
//! ревизия). 409 → дифф `base → текущая` в stderr, код 4. Успех → `<id>@<rev>`.
//! Перед записью в решение из `solution.toml` оно регистрируется (идемпотентно).
use std::io::Read;
use std::path::Path;

use serde_json::{Value, json};

use super::{Ctx, emit_written};
use crate::client::{CliError, EXIT_CONFLICT, EXIT_NOT_FOUND, read_json};
use crate::target::Loc;

pub struct Opts<'a> {
    pub file: Option<&'a Path>,
    pub message: Option<&'a str>,
    pub base_rev: Option<i64>,
    pub l0: Option<&'a str>,
}

/// Текст заметки из файла или stdin; не UTF-8 → код 6.
fn content(file: Option<&Path>) -> Result<String, CliError> {
    let bytes = match file {
        Some(p) => {
            std::fs::read(p).map_err(|e| CliError::input(format!("чтение {}: {e}", p.display())))?
        }
        None => {
            let mut b = Vec::new();
            std::io::stdin()
                .lock()
                .read_to_end(&mut b)
                .map_err(|e| CliError::input(format!("чтение stdin: {e}")))?;
            b
        }
    };
    String::from_utf8(bytes).map_err(|e| {
        CliError::input(format!(
            "текст заметки — не UTF-8 (байт {})",
            e.utf8_error().valid_up_to()
        ))
    })
}

pub fn run(ctx: &Ctx, raw: &str, o: &Opts) -> Result<(), CliError> {
    let loc = ctx.locate(raw)?;
    if let Loc::Path { path, .. } = &loc
        && path.is_empty()
    {
        return Err(CliError::input("не задан путь заметки"));
    }
    let text = content(o.file)?;
    ctx.ensure_solution(&loc)?;

    // существующий узел?
    let existing = match ctx.node_json(&loc) {
        Ok(v) => Some(v),
        Err(e) if e.code == EXIT_NOT_FOUND && matches!(loc, Loc::Path { .. }) => None,
        Err(e) => return Err(e),
    };
    let mut body = json!({ "content": text });
    if let Some(m) = o.message {
        body["message"] = json!(m);
    }
    if let Some(l0) = o.l0 {
        body["l0"] = json!(l0);
    }

    let Some(cur) = existing else {
        let Loc::Path { space, path, .. } = &loc else {
            unreachable!("id без узла — ошибка выше");
        };
        if o.base_rev.is_some() {
            return Err(CliError::new(
                EXIT_NOT_FOUND,
                format!("заметки нет, --base-rev неприменим: {raw}"),
            ));
        }
        body["space"] = json!(space);
        body["path"] = json!(path);
        body["kind"] = json!("note");
        let node = read_json(ctx.client.post_json("/nodes", &body)?)?;
        return emit_written(ctx, &node, true);
    };

    let node = &cur["node"];
    if node["kind"] == "folder" {
        return Err(CliError::input(format!("{raw} — папка, а не заметка")));
    }
    let id = node["id"].as_str().unwrap_or_default();
    let base = o
        .base_rev
        .or_else(|| node["current_rev"].as_i64())
        .unwrap_or(0);
    match ctx.client.put_json(&format!("/nodes/{id}"), &body, base) {
        Ok(r) => emit_written(ctx, &read_json(r)?, false),
        Err(e) if e.code == EXIT_CONFLICT => Err(conflict(e)),
        Err(e) => Err(e),
    }
}

/// 409: текст и дифф из problem — в stderr (через сообщение ошибки).
fn conflict(mut e: CliError) -> CliError {
    let p = e.problem.take().unwrap_or(Value::Null);
    if let Some(d) = p["diff"].as_str() {
        e.msg.push('\n');
        e.msg.push_str(d.trim_end_matches('\n'));
    }
    e.context("конфликт")
}
