//! `zam read <target>… [--tier L0|L1|L2] [--section "A > B"] [--rev N]`.
//!
//! - L2 (по умолчанию) — байты ревизии как сохранены (CRLF, BOM), без добавлений;
//! - L0 и L1 — текст сводки и перевод строки;
//! - `--json` — ответ сервера (`{node, content, rev}`, `{l0, rev}`, `{l1, rev}`);
//!   при нескольких целях — массив;
//! - несколько целей: перед каждой строка `--- <цель> ---` (с новой строки). Ошибка
//!   одной цели печатается в stderr, остальные читаются; код выхода — первой ошибки.
//!
//! **Пачка** (`--batch`, а также `--max-tokens`, `--budget`, `--if-rev`, `--since-rev`; §9.3
//! шаг 2): одна
//! `POST /read` на все цели — Markdown сервера как есть (заголовок на элемент, `…ещё N токенов`,
//! `(budget)`, последняя строка `~N tok`) или с `--json` ответ сервера. Без этих флагов вывод
//! побайтный, как раньше [решение исполнителя: побайтное чтение M1 не меняется]. Ошибка по
//! элементу остаётся в выводе; код выхода — первой ошибки (`not_found`, `invalid` — как у 404 и
//! 422, `forbidden`, `solution_access` — как у 403). `--rev` с пачкой несовместим.
//!
//! **Изменения** (`--since-rev N`, M4.5 §7): пачка из одной цели с `since_rev` — сервер отдаёт
//! унифицированный дифф с ревизии `N` (`(изменения с revN, дифф)`), полный текст, если дифф
//! длиннее (`(дифф длиннее текста — полный текст)`), или `unchanged`; с `--json` — элемент
//! `{rev, diff}` | `{rev, content, diff_skipped: true}` | `{unchanged: true}`.
//!
//! **Ссылки выдачи** (стенд Debian 13, B2): `n:<id>` — узел, `i:|c:|e:|r:|t:|k:|q:<id>` — запись
//! журнала, как их печатают `zam search` и `/read`. Запись журнала и без пачки читается через `POST
//! /read` (одна цель, без бюджета): печатается текст уровня (`L2` — тело), с `--json` — элемент
//! ответа сервера **[решение исполнителя]**. `--rev` к записи неприменим.
//!
//! Нет секции `--section` — код 5 с подсказкой: путь секции — заголовки от верхнего или
//! однозначный хвост пути.
use std::io::{ErrorKind, Read, Write};

use serde_json::{Value, json};

use super::{Ctx, emit, emit_json};
use crate::client::{
    CliError, EXIT_INPUT, EXIT_NOT_FOUND, EXIT_TOKEN, EXIT_UNREACHABLE, into_reader, read_json,
    read_text,
};
use crate::target::{Loc, entry_ref};

/// Параметры чтения.
pub struct Opts<'a> {
    pub tier: Option<&'a str>,
    pub section: Option<&'a str>,
    pub rev: Option<i64>,
    /// Пачкой через `POST /read` (иначе только если задан один из флагов ниже).
    pub batch: bool,
    pub max_tokens: Option<u32>,
    pub budget: Option<u32>,
    pub if_rev: Option<i64>,
    /// Изменения с ревизии (M4.5 §7).
    pub since_rev: Option<i64>,
}

impl Opts<'_> {
    fn batched(&self) -> bool {
        self.batch
            || self.max_tokens.is_some()
            || self.budget.is_some()
            || self.if_rev.is_some()
            || self.since_rev.is_some()
    }
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
    if o.batched() {
        return batch(ctx, targets, o);
    }
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
    if entry_ref(raw).is_some() {
        return one_entry(ctx, raw, o);
    }
    one_node(ctx, raw, o).map_err(|e| match o.section {
        Some(s) if e.code == EXIT_NOT_FOUND => e.context(format!(
            "нет заметки или секции «{s}» (секция — путь заголовков от верхнего «A > B» или его однозначный хвост)"
        )),
        _ => e,
    })
}

/// Запись журнала `i:|c:|e:|r:|t:|k:|q:<id>`: одна цель `POST /read` без бюджета.
fn one_entry(ctx: &Ctx, raw: &str, o: &Opts) -> Result<Out, CliError> {
    if o.rev.is_some() {
        return Err(CliError::input("--rev неприменим к записи журнала"));
    }
    let tier = o.tier.map(str::to_ascii_uppercase);
    let mut it = json!({ "id": raw });
    if let Some(t) = &tier {
        it["tier"] = json!(t);
    }
    if let Some(s) = o.section {
        it["section"] = json!(s);
    }
    // бюджет не ограничивает одну цель: текст печатается целиком, как у заметки
    let body = json!({ "items": [it], "format": "json", "budget_tokens": u32::MAX });
    let v = read_json(ctx.client.post_json("/read", &body)?)?;
    let item = v["items"][0].clone();
    if let Some(code) = item["error"].as_str() {
        let detail = item["detail"]
            .as_str()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        return Err(CliError::new(error_exit(code), format!("{code}{detail}")));
    }
    if ctx.json {
        return Ok(Out::Json(item));
    }
    let mut text = item["text"].as_str().unwrap_or("").to_string();
    if matches!(tier.as_deref(), Some("L0" | "L1")) && !text.ends_with('\n') {
        text.push('\n');
    }
    emit(text.as_bytes())?;
    Ok(Out::Printed {
        ends_with_newline: text.is_empty() || text.ends_with('\n'),
    })
}

/// Узел по пути или id: печать или JSON.
fn one_node(ctx: &Ctx, raw: &str, o: &Opts) -> Result<Out, CliError> {
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

/// Код выхода по `error` элемента `/read`.
fn error_exit(code: &str) -> u8 {
    match code {
        "not_found" => EXIT_NOT_FOUND,
        "forbidden" | "solution_access" => EXIT_TOKEN,
        _ => EXIT_INPUT,
    }
}

/// `id` элемента `/read` для цели: ссылка записи `i:|c:|e:|r:|t:|k:|q:<id>` как есть, id
/// узла — голый (сервер ищет узел, затем запись), путь — `<пространство>:/<путь>`.
fn item_id(ctx: &Ctx, raw: &str) -> Result<String, CliError> {
    if entry_ref(raw).is_some() {
        return Ok(raw.to_string());
    }
    Ok(match ctx.locate(raw)? {
        Loc::Node(id) => id,
        Loc::Path { space, path, .. } => format!("{space}:/{path}"),
    })
}

/// Пачка: одна `POST /read` на все цели.
fn batch(ctx: &Ctx, targets: &[String], o: &Opts) -> Result<(), CliError> {
    if o.rev.is_some() {
        return Err(CliError::input(
            "--rev несовместим с пачкой (--batch, --max-tokens, --budget, --if-rev, --since-rev)",
        ));
    }
    if (o.if_rev.is_some() || o.since_rev.is_some()) && targets.len() > 1 {
        return Err(CliError::input(
            "--if-rev и --since-rev задают ревизию одной цели: передайте ровно одну цель",
        ));
    }
    let mut items = Vec::with_capacity(targets.len());
    for t in targets {
        let mut it = json!({ "id": item_id(ctx, t)? });
        if let Some(v) = o.tier {
            it["tier"] = json!(v.to_ascii_uppercase());
        }
        if let Some(v) = o.section {
            it["section"] = json!(v);
        }
        if let Some(v) = o.max_tokens {
            it["max_tokens"] = json!(v);
        }
        if let Some(v) = o.if_rev {
            it["if_rev"] = json!(v);
        }
        if let Some(v) = o.since_rev {
            it["since_rev"] = json!(v);
        }
        items.push(it);
    }
    let mut body = json!({ "items": items, "format": if ctx.json { "json" } else { "md" } });
    if let Some(b) = o.budget {
        body["budget_tokens"] = json!(b);
    }
    let resp = ctx.client.post_json("/read", &body)?;
    let first = if ctx.json {
        let v = read_json(resp)?;
        emit_json(&v)?;
        v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|i| i["error"].as_str().map(error_exit))
    } else {
        // ошибки — по заголовку ответа, а не по тексту: заметка может содержать строку `### x  error: y`
        let code = resp
            .headers()
            .get("x-zam-read-error")
            .and_then(|v| v.to_str().ok())
            .map(error_exit);
        emit(read_text(resp)?.as_bytes())?;
        code
    };
    match first {
        Some(code) => Err(CliError::new(code, String::new())),
        None => Ok(()),
    }
}
