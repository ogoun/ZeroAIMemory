//! `zam search <query> [--all] [--mode brief|chunks|notes] [--top-k N]`.
//!
//! Markdown от сервера по умолчанию, `--json` — JSON ответа. Область: решение из
//! `solution.toml` (если есть) плюс Knowledge и Rules; `--all` — все решения.
use serde_json::json;

use super::{Ctx, emit};
use crate::client::{CliError, read_text};

pub struct Opts<'a> {
    pub all: bool,
    pub mode: &'a str,
    pub top_k: Option<u32>,
}

pub fn run(ctx: &Ctx, query: &str, o: &Opts) -> Result<(), CliError> {
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
    let mut text = read_text(ctx.client.post_json("/search", &body)?)?;
    if text.is_empty() {
        eprintln!("ничего не найдено");
        return Ok(());
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    emit(text.as_bytes())
}
