//! `zam read <target>… [--tier L0|L1|L2] [--section "A > B"] [--rev N]`.
//!
//! - L2 (по умолчанию) — байты ревизии как сохранены (CRLF, BOM), без добавлений;
//! - L0 и L1 — текст сводки и перевод строки;
//! - `--json` — ответ сервера (`{node, content, rev}`, `{l0, rev}`, `{l1, rev}`);
//!   при нескольких целях — массив;
//! - несколько целей: перед каждой строка `--- <цель> ---` (с новой строки). Ошибка
//!   одной цели печатается в stderr, остальные читаются; код выхода — первой ошибки.
use std::io::{ErrorKind, Read, Write};

use serde_json::Value;

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, EXIT_UNREACHABLE, into_reader, read_json};

/// Параметры чтения.
pub struct Opts<'a> {
    pub tier: Option<&'a str>,
    pub section: Option<&'a str>,
    pub rev: Option<i64>,
}

/// Результат одной цели.
enum Out {
    /// JSON для общего вывода.
    Json(Value),
    Printed {
        ends_with_newline: bool,
    },
}

pub fn run(ctx: &Ctx, targets: &[String], o: &Opts) -> Result<(), CliError> {
    let many = targets.len() > 1;
    let mut first_err: Option<CliError> = None;
    let mut jsons = Vec::new();
    // вывод предыдущей цели закончился переводом строки
    let mut at_line_start = true;
    for t in targets {
        if many && !ctx.json {
            let nl = if at_line_start { "" } else { "\n" };
            emit(format!("{nl}--- {t} ---\n").as_bytes())?;
            at_line_start = true;
        }
        match one(ctx, t, o) {
            Ok(Out::Json(v)) => jsons.push(v),
            Ok(Out::Printed { ends_with_newline }) => at_line_start = ends_with_newline,
            Err(e) => {
                let e = e.context(t);
                eprintln!("{e}");
                first_err.get_or_insert(e);
            }
        }
    }
    if ctx.json {
        if many {
            emit_json(&Value::Array(jsons))?;
        } else if let Some(v) = jsons.pop() {
            emit_json(&v)?;
        }
    }
    match first_err {
        // текст уже напечатан
        Some(e) => Err(CliError::new(e.code, String::new())),
        None => Ok(()),
    }
}

/// Одна цель: печать или JSON.
fn one(ctx: &Ctx, raw: &str, o: &Opts) -> Result<Out, CliError> {
    let loc = ctx.locate(raw)?;
    let tier = o.tier.map(str::to_ascii_uppercase);
    let rev = o.rev.map(|r| r.to_string());
    let summary = matches!(tier.as_deref(), Some("L0" | "L1"));
    let format = if ctx.json || summary { "json" } else { "md" };
    let mut q: Vec<(&str, &str)> = vec![("format", format)];
    if let Some(t) = tier.as_deref() {
        q.push(("tier", t));
    }
    if let Some(s) = o.section {
        q.push(("section", s));
    }
    if let Some(r) = rev.as_deref() {
        q.push(("rev", r));
    }
    let resp = ctx.get_node(&loc, &q)?;
    if ctx.json {
        return Ok(Out::Json(read_json(resp)?));
    }
    if summary {
        let v = read_json(resp)?;
        let key = if tier.as_deref() == Some("L0") {
            "l0"
        } else {
            "l1"
        };
        let mut text = v[key].as_str().unwrap_or("").to_string();
        text.push('\n');
        emit(text.as_bytes())?;
        return Ok(Out::Printed {
            ends_with_newline: true,
        });
    }
    copy_out(&mut into_reader(resp))
}

/// Тело ответа в stdout как есть; запоминается, был ли последний байт `\n`.
fn copy_out(r: &mut impl Read) -> Result<Out, CliError> {
    let mut out = std::io::stdout().lock();
    let mut buf = vec![0u8; 64 * 1024];
    let mut last = b'\n';
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(CliError::new(
                    EXIT_UNREACHABLE,
                    format!("чтение ответа: {e}"),
                ));
            }
        };
        last = buf[n - 1];
        match out.write_all(&buf[..n]) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => break,
            Err(e) => return Err(CliError::config(format!("запись в stdout: {e}"))),
        }
    }
    let _ = out.flush();
    Ok(Out::Printed {
        ends_with_newline: last == b'\n',
    })
}
