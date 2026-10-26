//! Insights (§7.5, §13.2): `zam log insight …` и `zam insight list|show|supersede|retract|
//! revisit|append`.
//!
//! - `zam log insight --kind K --title T [--scope solution|global] [--certainty C]
//!   [--evidence-file F] [--trigger TYPE:PATTERN]… [--tag X]… [--force] [--supersedes ID]
//!   [--area A] [--solution GUID] [--key K] [--body-file F | stdin]` — запись insight в решение
//!   (`solution.toml` или `--solution`) или, при `--scope global`, в Knowledge. `--evidence-file`
//!   — JSON-объект `{checked, method, result}` → `data.evidence`; `--trigger` —
//!   `file_glob:**/*.sh`, `command_regex:…`, `error_regex:…`, `keyword:…` (без инструментов:
//!   любые). Ответ 409 (похожий insight уже есть) печатает кандидатов и подсказки, код 4.
//!   Успех → `<id>@<rev>`; проверка только по заголовку (нет модели) — пометка в stderr.
//! - `zam insight list [--status S,…] [--global] [--solution GUID] [--limit N]` — лента insight;
//!   `show ID` — карточка с текстом и триггерами; `supersede ID --title T [--kind K] …` —
//!   замещение (вид по умолчанию — у старой записи); `retract ID --reason R`; `revisit ID
//!   [--reason R]`; `append ID [--body-file F | stdin]` — дописать к телу.
//! - `zam insight certify ID axiom|theorem|hypothesis|none --reason R [--evidence-file F]
//!   [--human]` — смена эпистемического статуса (§7.5): `theorem` от агента — с доказательством
//!   (`--evidence-file` дописывается в `data.evidence`), `axiom` и любая правка `axiom` — только
//!   с `--human`: адрес и токен — только из `~/.zeroaimemory/config.toml` (`url`,
//!   `human_token`), только в интерактивном терминале с подтверждением вводом id записи.
use std::path::Path;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, emit, emit_json, solution_guid};
use crate::client::{CliError, EXIT_CONFLICT, read_json};
use crate::target::Loc;

/// Виды триггеров (`--trigger TYPE:PATTERN`).
const TRIGGER_KINDS: [&str; 4] = ["file_glob", "command_regex", "error_regex", "keyword"];

/// Общие поля новой записи insight (`log insight` и `insight supersede`).
pub struct Fields<'a> {
    pub kind: Option<&'a str>,
    pub title: &'a str,
    pub body_file: Option<&'a Path>,
    pub certainty: Option<&'a str>,
    pub evidence_file: Option<&'a Path>,
    pub triggers: &'a [String],
    pub tags: &'a [String],
    pub area: Option<&'a str>,
    pub key: Option<&'a str>,
}

pub struct Log<'a> {
    pub fields: Fields<'a>,
    pub scope: Option<&'a str>,
    pub force: bool,
    pub supersedes: Option<&'a str>,
    pub solution: Option<&'a str>,
}

/// `file_glob:**/*.sh` → `{type, pattern}`; неизвестный вид или пустой шаблон → код 6.
pub fn parse_trigger(raw: &str) -> Result<Value, CliError> {
    let bad = || {
        CliError::input(format!(
            "--trigger «{raw}»: формат ВИД:ШАБЛОН, вид — {}",
            TRIGGER_KINDS.join(", ")
        ))
    };
    let (k, p) = raw.split_once(':').ok_or_else(bad)?;
    if !TRIGGER_KINDS.contains(&k) || p.is_empty() {
        return Err(bad());
    }
    Ok(json!({ "type": k, "pattern": p }))
}

/// `data.evidence` из JSON-файла (объект).
fn read_evidence(p: &Path) -> Result<Value, CliError> {
    let text = std::fs::read_to_string(p)
        .map_err(|e| CliError::input(format!("чтение {}: {e}", p.display())))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| {
        CliError::input(format!(
            "--evidence-file: JSON-объект {{checked, method, result}}: {e}"
        ))
    })?;
    if !v.is_object() {
        return Err(CliError::input(
            "--evidence-file: JSON-объект {checked, method, result}",
        ));
    }
    Ok(v)
}

/// Тело запроса новой записи insight (без `scope`, `force`, `supersedes_id`).
fn insight_body(f: &Fields, kind: &str) -> Result<Value, CliError> {
    let mut data = json!({ "kind": kind });
    if let Some(p) = f.evidence_file {
        data["evidence"] = read_evidence(p)?;
    }
    let triggers = f
        .triggers
        .iter()
        .map(|t| parse_trigger(t))
        .collect::<Result<Vec<_>, _>>()?;
    let mut body = json!({
        "kind": "insight",
        "title": f.title,
        "body": read_text(f.body_file)?,
        "data": data,
        "tags": f.tags,
        "triggers": triggers,
    });
    if let Some(c) = f.certainty {
        body["certainty"] = json!(c);
    }
    if let Some(a) = f.area {
        body["area"] = json!(a);
    }
    Ok(body)
}

/// Заголовки записи: `Idempotency-Key`, `X-Zam-Area`.
fn write_headers<'a>(f: &Fields<'a>) -> Vec<(&'static str, &'a str)> {
    let mut h = Vec::new();
    if let Some(k) = f.key {
        h.push(("Idempotency-Key", k));
    }
    if let Some(a) = f.area {
        h.push(("X-Zam-Area", a));
    }
    h
}

/// 409 possible-duplicate → кандидаты и подсказки в stderr, код 4; прочие ошибки — как есть.
fn explain_duplicate(e: CliError) -> CliError {
    let Some(p) = e
        .problem
        .as_ref()
        .filter(|p| p["type"] == "/problems/possible-duplicate")
    else {
        return e;
    };
    let mut msg = String::from("похожий insight уже записан");
    if p["dedup"] == "degraded" {
        msg.push_str(" (проверка только по заголовку: векторный канал недоступен)");
    }
    msg.push_str(":\n");
    for c in p["candidates"].as_array().into_iter().flatten() {
        let score = c["score"]
            .as_f64()
            .map(|s| format!(" {s:.2}"))
            .unwrap_or_default();
        msg.push_str(&format!(
            "  {} [{}] «{}» — {}{score}\n",
            c["id"].as_str().unwrap_or("?"),
            c["status"].as_str().unwrap_or("?"),
            c["title"].as_str().unwrap_or(""),
            c["reason"].as_str().unwrap_or("?"),
        ));
    }
    msg.push_str(
        "что дальше:\n  дописать к существующему: zam insight append <id> < дополнение.md\n  \
         заменить его новой записью: повторите с --supersedes <id>\n  \
         записать всё равно: повторите с --force",
    );
    CliError {
        code: EXIT_CONFLICT,
        msg,
        problem: e.problem,
    }
}

/// Печать созданной записи: `<id>@<rev>` или JSON `{entry, created}`.
fn emit_entry(ctx: &Ctx, e: &Value, created: bool) -> Result<(), CliError> {
    if ctx.json {
        return emit_json(&json!({ "entry": e, "created": created }));
    }
    if e["dedup"] == "degraded" {
        eprintln!("дубли проверены только по заголовку: векторный канал недоступен");
    }
    emit(format!("{}@{}\n", e["id"].as_str().unwrap_or("?"), e["rev"]).as_bytes())
}

/// `zam log insight`.
pub fn log(ctx: &Ctx, o: &Log) -> Result<(), CliError> {
    let kind = o
        .fields
        .kind
        .ok_or_else(|| CliError::input("--kind: вид инсайта обязателен"))?;
    let mut body = insight_body(&o.fields, kind)?;
    let global = o.scope == Some("global");
    let space = if global {
        body["scope"] = json!("global");
        "knowledge".to_string()
    } else {
        let guid = solution_guid(ctx, o.solution)?;
        let own = ctx.solution.as_ref().is_some_and(|s| s.id == guid);
        ctx.ensure_solution(&Loc::Path {
            space: guid.clone(),
            path: String::new(),
            own_solution: own,
        })?;
        guid
    };
    if o.force {
        body["force"] = json!(true);
    }
    if let Some(s) = o.supersedes {
        body["supersedes_id"] = json!(s);
    }
    let r = ctx
        .client
        .post_json_with(
            &format!("/spaces/{space}/entries"),
            &body,
            &write_headers(&o.fields),
        )
        .map_err(explain_duplicate)?;
    let created = r.status().as_u16() == 201;
    emit_entry(ctx, &read_json(r)?, created)
}

/// Пространство ленты: `knowledge` при `--global`, иначе решение.
fn list_space(ctx: &Ctx, global: bool, solution: Option<&str>) -> Result<String, CliError> {
    if global {
        Ok("knowledge".into())
    } else {
        solution_guid(ctx, solution)
    }
}

/// `zam insight list`.
pub fn list(
    ctx: &Ctx,
    status: Option<&str>,
    global: bool,
    solution: Option<&str>,
    limit: Option<u32>,
) -> Result<(), CliError> {
    let space = list_space(ctx, global, solution)?;
    let limit = limit.map(|l| l.to_string());
    let mut q: Vec<(&str, &str)> = vec![("kind", "insight")];
    if let Some(s) = status {
        q.push(("status", s));
    }
    if let Some(l) = &limit {
        q.push(("limit", l));
    }
    let v = read_json(ctx.client.get(&format!("/spaces/{space}/entries"), &q)?)?;
    if ctx.json {
        return emit_json(&v);
    }
    let mut out = String::new();
    for e in v["items"].as_array().into_iter().flatten() {
        out.push_str(&card(e));
        out.push('\n');
    }
    emit(out.as_bytes())
}

/// Строка карточки: `<id> [status] <вид> · <certainty> «title»`.
fn card(e: &Value) -> String {
    let mut meta = vec![
        format!("[{}]", e["status"].as_str().unwrap_or("?")),
        e["data"]["kind"].as_str().unwrap_or("?").to_string(),
    ];
    if let Some(c) = e["certainty"].as_str() {
        meta.push(c.to_string());
    }
    format!(
        "{} {} «{}»",
        e["id"].as_str().unwrap_or("?"),
        meta.join(" "),
        e["title"].as_str().unwrap_or("")
    )
}

fn get_entry(ctx: &Ctx, id: &str) -> Result<Value, CliError> {
    read_json(ctx.client.get(&format!("/entries/{id}"), &[])?)
}

/// `zam insight show <id>`.
pub fn show(ctx: &Ctx, id: &str) -> Result<(), CliError> {
    let e = get_entry(ctx, id)?;
    if ctx.json {
        return emit_json(&e);
    }
    let mut out = format!("{}  rev{}\n", card(&e), e["rev"]);
    if let Some(r) = e["retract_reason"].as_str() {
        out.push_str(&format!("отозвана: {r}\n"));
    }
    for (k, label) in [
        ("supersedes_id", "замещает"),
        ("superseded_by_id", "замещена"),
    ] {
        if let Some(x) = e[k].as_str() {
            out.push_str(&format!("{label}: {x}\n"));
        }
    }
    for t in e["triggers"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "триггер: {}:{}\n",
            t["type"].as_str().unwrap_or("?"),
            t["pattern"].as_str().unwrap_or("")
        ));
    }
    let body = e["body"].as_str().unwrap_or("");
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body.trim_end());
        out.push('\n');
    }
    emit(out.as_bytes())
}

/// `zam insight supersede <id>`: новая запись замещает старую (вид — у старой, если не задан).
pub fn supersede(ctx: &Ctx, id: &str, f: &Fields) -> Result<(), CliError> {
    let kind = match f.kind {
        Some(k) => k.to_string(),
        None => get_entry(ctx, id)?["data"]["kind"]
            .as_str()
            .unwrap_or("discovery")
            .to_string(),
    };
    let body = insight_body(f, &kind)?;
    let r = ctx.client.post_json_with(
        &format!("/entries/{id}/supersede"),
        &body,
        &write_headers(f),
    )?;
    let created = r.status().as_u16() == 201;
    let v = read_json(r)?;
    if ctx.json {
        return emit_json(
            &json!({ "entry": v["entry"], "superseded": v["superseded"], "created": created }),
        );
    }
    emit(
        format!(
            "{}@{} (замещает {id})\n",
            v["entry"]["id"].as_str().unwrap_or("?"),
            v["entry"]["rev"]
        )
        .as_bytes(),
    )
}

/// Печать изменённой записи: `<id>@<rev> <status>` или JSON.
fn emit_changed(ctx: &Ctx, e: &Value) -> Result<(), CliError> {
    if ctx.json {
        return emit_json(e);
    }
    emit(
        format!(
            "{}@{} {}\n",
            e["id"].as_str().unwrap_or("?"),
            e["rev"],
            e["status"].as_str().unwrap_or("")
        )
        .as_bytes(),
    )
}

/// `zam insight retract <id> --reason R`.
pub fn retract(ctx: &Ctx, id: &str, reason: &str) -> Result<(), CliError> {
    let r = ctx.client.post_json(
        &format!("/entries/{id}/retract"),
        &json!({ "reason": reason }),
    )?;
    emit_changed(ctx, &read_json(r)?)
}

/// `zam insight revisit <id> [--reason R]`.
pub fn revisit(ctx: &Ctx, id: &str, reason: Option<&str>) -> Result<(), CliError> {
    let mut body = json!({ "status": "revisit" });
    if let Some(r) = reason {
        body["reason"] = json!(r);
    }
    let r = ctx.client.patch_json(&format!("/entries/{id}"), &body)?;
    emit_changed(ctx, &read_json(r)?)
}

/// Тело `PATCH` смены статуса: `certainty` (`none` → `null`), причина и — при доказательстве —
/// `data` записи с `evidence` (insight `data` заменяется целиком, поэтому берётся текущая).
fn certify_body(
    current: Option<&Value>,
    certainty: &str,
    reason: &str,
    evidence: Option<Value>,
) -> Value {
    let c = if certainty == "none" {
        Value::Null
    } else {
        json!(certainty)
    };
    let mut body = json!({ "certainty": c, "reason": reason });
    if let (Some(ev), Some(cur)) = (evidence, current) {
        let mut data = cur["data"].clone();
        if !data.is_object() {
            data = json!({});
        }
        data["evidence"] = ev;
        body["data"] = data;
    }
    body
}

/// `zam insight certify <id> <certainty> --reason R [--evidence-file F]`.
pub fn certify(
    ctx: &Ctx,
    id: &str,
    certainty: &str,
    reason: &str,
    evidence_file: Option<&Path>,
) -> Result<(), CliError> {
    if reason.trim().is_empty() {
        return Err(CliError::input(
            "--reason: причина смены статуса обязательна",
        ));
    }
    let evidence = evidence_file.map(read_evidence).transpose()?;
    let current = match evidence {
        Some(_) => Some(get_entry(ctx, id)?),
        None => None,
    };
    let body = certify_body(current.as_ref(), certainty, reason, evidence);
    let r = ctx.client.patch_json(&format!("/entries/{id}"), &body)?;
    let e = read_json(r)?;
    if ctx.json {
        return emit_json(&e);
    }
    emit(
        format!(
            "{}@{} {}\n",
            e["id"].as_str().unwrap_or("?"),
            e["rev"],
            e["certainty"].as_str().unwrap_or("none")
        )
        .as_bytes(),
    )
}

/// `zam insight append <id>` — текст из `--body-file` или stdin; пустой → код 6.
pub fn append(ctx: &Ctx, id: &str, body_file: Option<&Path>) -> Result<(), CliError> {
    let text = read_text(body_file)?;
    if text.trim().is_empty() {
        return Err(CliError::input(
            "нечего дописывать: текст из --body-file или stdin",
        ));
    }
    let r = ctx
        .client
        .patch_json(&format!("/entries/{id}"), &json!({ "append_body": text }))?;
    emit_changed(ctx, &read_json(r)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_flag_forms() {
        assert_eq!(
            parse_trigger("file_glob:**/*.sh").unwrap(),
            json!({"type": "file_glob", "pattern": "**/*.sh"})
        );
        // двоеточие внутри шаблона сохраняется
        assert_eq!(
            parse_trigger("error_regex:bad interpreter: /bin/bash").unwrap()["pattern"],
            "bad interpreter: /bin/bash"
        );
        for bad in ["**/*.sh", "glob:*.sh", "keyword:", ":x"] {
            assert_eq!(
                parse_trigger(bad).unwrap_err().code,
                crate::client::EXIT_INPUT,
                "{bad}"
            );
        }
    }

    #[test]
    fn certify_body_forms() {
        assert_eq!(
            certify_body(None, "axiom", "R", None),
            json!({"certainty": "axiom", "reason": "R"})
        );
        assert_eq!(
            certify_body(None, "none", "R", None),
            json!({"certainty": null, "reason": "R"})
        );
        let cur = json!({"data": {"kind": "gotcha", "experiment": {"result": "confirmed"}}});
        let ev = json!({"checked": "c", "method": "m", "result": "r"});
        let b = certify_body(Some(&cur), "theorem", "R", Some(ev.clone()));
        assert_eq!(b["data"]["kind"], "gotcha");
        assert_eq!(b["data"]["experiment"]["result"], "confirmed");
        assert_eq!(b["data"]["evidence"], ev);
    }

    #[test]
    fn duplicate_problem_explained() {
        let e = CliError {
            code: crate::client::EXIT_CONFLICT,
            msg: "x".into(),
            problem: Some(json!({
                "type": "/problems/possible-duplicate",
                "dedup": "full",
                "candidates": [{"id": "01A", "status": "active", "title": "LF", "reason": "vector", "score": 0.93}]
            })),
        };
        let m = explain_duplicate(e).msg;
        assert!(m.contains("01A [active] «LF» — vector 0.93"), "{m}");
        assert!(m.contains("--supersedes") && m.contains("--force") && m.contains("append"));
        // прочие ошибки не меняются
        let other = CliError::input("y");
        assert_eq!(explain_duplicate(other).msg, "y");
    }
}
