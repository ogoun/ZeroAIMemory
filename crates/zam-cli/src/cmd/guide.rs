//! `zam guide` — полный протокол работы агента с памятью (`GET /api/v1/agent-guide`, §9.3, §14):
//! Markdown в stdout.
use super::{Ctx, emit};
use crate::client::{CliError, read_text};

pub fn run(ctx: &Ctx, lang: &str) -> Result<(), CliError> {
    let r = ctx.client.get("/agent-guide", &[("lang", lang)])?;
    let mut text = read_text(r)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    emit(text.as_bytes())
}
