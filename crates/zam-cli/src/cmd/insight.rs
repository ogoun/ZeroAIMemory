//! Insights (§7.5, §13.2): `zam log insight …` и `zam insight list|show|supersede|retract|
//! revisit|triggers|append|certify|feedback`.
//!
//! - `zam log insight --kind K --title T [--scope solution|global] [--certainty C]
//!   [--reason R] [--evidence-file F] [--trigger TYPE:PATTERN]… [--tag X]… [--force] [--supersedes ID
//!   [--supersedes-rev REV]] [--area A] [--solution GUID] [--key K] [--body-file F | stdin]` —
//!   запись insight в решение
//!   (`solution.toml` или `--solution`) или, при `--scope global`, в Knowledge. `--evidence-file`
//!   — JSON-объект `{checked, method, result}` → `data.evidence`; `--trigger` —
//!   `file_glob:**/*.sh`, `command_regex:…`, `error_regex:…`, `keyword:…` (без инструментов:
//!   любые); `--ref <путь>` (повторяемый) — ссылки на файлы с хэшем и коммитом (M4.5 §6.1).
//!   Ответ 409 (похожий insight уже есть) печатает кандидатов и подсказки, код 4.
//!   Успех → `<id>@<rev>`; проверка только по заголовку (нет модели) — пометка в stderr.
//!   `--certainty axiom` — только по прямому указанию пользователя и только с `--reason`
//!   (иначе код 6 до запроса); `--reason` уходит сообщением ревизии (`message`) (стенд M4).
//!   Без `--certainty` сервер ставит агенту `hypothesis` — об этом строка в stderr (M6.6 §9).
//!   Доказательство без файла — `--evidence-checked C --evidence-method M --evidence-result R`
//!   (все три вместе, не с `--evidence-file`). Поля решения `data.decision` (ADR) — `--context`,
//!   `--option` (до 8), `--chosen`, `--rationale`, `--consequences`, `--decided-by
//!   human|agent`, `--revisit-if`: только у `--kind decision` (у `supersede` без `--kind` — вид
//!   старой записи), иначе код 6 до записи.
//! - `zam insight list [--status S,…] [--global] [--solution GUID] [--limit N | --all]` — лента
//!   insight (страница — умолчание сервера, 50; обход и подсказка — [`super::list_entries`]);
//!   `show ID` — карточка с текстом и триггерами; `supersede ID --title T [--kind K] [--rev REV]
//!   …` — замещение (вид по умолчанию — у старой записи); текст с метками `‹secret-…›` из вида
//!   старой записи — с её ревизией: `--rev` у `supersede` (`If-Match`), `--supersedes-rev` у
//!   `log insight --supersedes` (M7 §4.3.2 п. 3; без ревизии сервер отвечает 428);
//!   `retract ID --reason R`; `revisit ID
//!   [--reason R]`; `append ID [--body-file F | stdin] [--by human|agent]` — дописать к телу
//!   (`--by human` у идеи — слова человека: возвращённая идея снова на разбор, M6.5 §6).
//! - `zam insight certify ID axiom|theorem|hypothesis|none --reason R [--evidence-file F]` —
//!   смена эпистемического статуса (§7.5) обычным токеном: `theorem` — с доказательством
//!   (`--evidence-file` дописывается в `data.evidence`); `axiom` агент ставит и меняет только
//!   по прямому указанию пользователя (§14.2), сервер это не проверяет.
//!
//! - `zam insight triggers ID (--trigger TYPE:PATTERN… | --clear)` — набор триггеров целиком без
//!   замещения записи (M6.6 §8): `[]` при `--clear`; печать — `<id>@<rev> триггеров K` и строки
//!   `триггер: вид:шаблон`, как в `show`.
//! - `zam insight feedback ID useful|useless [--session S]` — оценка полезности записи (§10.5,
//!   `POST /entries/{id}/feedback`, источник `agent`); печатает `<id> <оценка>`. Показы и оценки
//!   (`usage`) — в `insight show`.
//!
//! `certify`, `append`, `revisit` и `triggers` правят запись с ревизией (§11.3): `rev` — из
//! `GET /entries/{id}`, запрос — с `If-Match`; запись изменилась между чтением и правкой (409) —
//! «запись изменилась (rev N → M): перечитайте», код 4.
use std::path::Path;

use serde_json::{Value, json};

use super::log::read_text;
use super::{Ctx, Paging, emit, emit_json, list_entries, solution_guid};
use crate::client::{CliError, EXIT_CONFLICT, read_json};
use crate::target::Loc;

/// Виды триггеров (`--trigger TYPE:PATTERN`).
const TRIGGER_KINDS: [&str; 4] = ["file_glob", "command_regex", "error_regex", "keyword"];

/// Вариантов `data.decision.options` — как у сервера
/// (`zam_server::api::insights::DECISION_OPTIONS_MAX`, 422 сверх него).
const DECISION_OPTIONS_MAX: usize = 8;

/// Строка stderr, когда `--certainty` не задан и сервер поставил insight агента умолчание
/// `hypothesis` (M6.6 §9, замечание 8).
const DEFAULT_CERTAINTY_NOTE: &str =
    "certainty: hypothesis (умолчание агента; проверено — --certainty theorem и доказательство)";

/// Поля решения `data.decision` (ADR, M4.5 §5.1) из флагов `--context`, `--option`, `--chosen`,
/// `--rationale`, `--consequences`, `--decided-by`, `--revisit-if` (M6.6 §9, замечание 8).
#[derive(Default)]
pub struct Decision<'a> {
    pub context: Option<&'a str>,
    /// Отвергнутые варианты (`--option`, до [`DECISION_OPTIONS_MAX`]).
    pub options: &'a [String],
    pub chosen: Option<&'a str>,
    pub rationale: Option<&'a str>,
    pub consequences: Option<&'a str>,
    /// `human | agent` (проверил clap).
    pub decided_by: Option<&'a str>,
    pub revisit_if: Option<&'a str>,
}

impl Decision<'_> {
    /// Строковые поля: флаг, ключ `data.decision`, значение.
    fn texts(&self) -> [(&'static str, &'static str, Option<&str>); 6] {
        [
            ("--context", "context", self.context),
            ("--chosen", "chosen", self.chosen),
            ("--rationale", "rationale", self.rationale),
            ("--consequences", "consequences", self.consequences),
            ("--decided-by", "decided_by", self.decided_by),
            ("--revisit-if", "revisit_if", self.revisit_if),
        ]
    }

    /// `data.decision` из заданных флагов; ни одного — `None`. Вариантов больше
    /// [`DECISION_OPTIONS_MAX`] — код 6: проверка без вида записи, до любого запроса.
    fn data(&self) -> Result<Option<Value>, CliError> {
        if self.options.len() > DECISION_OPTIONS_MAX {
            return Err(CliError::input(format!(
                "--option: не больше {DECISION_OPTIONS_MAX} вариантов решения (задано {})",
                self.options.len()
            )));
        }
        let mut d = serde_json::Map::new();
        if !self.options.is_empty() {
            d.insert("options".into(), json!(self.options));
        }
        for (_, key, v) in self.texts() {
            if let Some(v) = v {
                d.insert(key.into(), json!(v));
            }
        }
        Ok((!d.is_empty()).then_some(Value::Object(d)))
    }

    /// Заданные флаги решения — для текста ошибки.
    fn given(&self) -> Vec<&'static str> {
        let mut g: Vec<&str> = self
            .texts()
            .iter()
            .filter(|(_, _, v)| v.is_some())
            .map(|(flag, _, _)| *flag)
            .collect();
        if !self.options.is_empty() {
            g.insert(0, "--option");
        }
        g
    }
}

/// `data.decision` записи вида `kind`: флаги решения — только у `decision` (у замещения без
/// `--kind` — вид старой записи), иначе код 6 до запроса записи.
fn decision_for(d: &Decision, kind: &str) -> Result<Option<Value>, CliError> {
    let Some(v) = d.data()? else {
        return Ok(None);
    };
    if kind != "decision" {
        return Err(CliError::input(format!(
            "{}: поля решения — только у --kind decision (вид записи — {kind})",
            d.given().join(", ")
        )));
    }
    Ok(Some(v))
}

/// Доказательство флагами `--evidence-checked`, `--evidence-method`, `--evidence-result`
/// (только все три — `requires_all` clap) → `data.evidence`.
pub struct Evidence<'a> {
    pub checked: &'a str,
    pub method: &'a str,
    pub result: &'a str,
}

/// Общие поля новой записи insight (`log insight` и `insight supersede`).
pub struct Fields<'a> {
    pub kind: Option<&'a str>,
    pub title: &'a str,
    pub body_file: Option<&'a Path>,
    pub certainty: Option<&'a str>,
    /// Причина статуса (`message` первой ревизии); для `axiom` обязательна.
    pub reason: Option<&'a str>,
    pub evidence_file: Option<&'a Path>,
    /// Доказательство без файла (с `--evidence-file` не бывает — конфликт clap).
    pub evidence: Option<Evidence<'a>>,
    pub decision: Decision<'a>,
    pub triggers: &'a [String],
    pub tags: &'a [String],
    /// Файлы `--ref` (M4.5 §6.1): ссылки `{path, hash, commit}`; у замещения без `--ref`
    /// сервер наследует ссылки старой записи.
    pub refs: &'a [String],
    pub area: Option<&'a str>,
    pub key: Option<&'a str>,
}

pub struct Log<'a> {
    pub fields: Fields<'a>,
    pub scope: Option<&'a str>,
    pub force: bool,
    pub supersedes: Option<&'a str>,
    /// Ревизия замещаемой записи (`supersedes_rev`), с которой прочитан текст с метками
    /// `‹secret-…›` (M7 §4.3.2 п. 3; без неё — 428).
    pub supersedes_rev: Option<i64>,
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

/// Причина статуса (непустая) и правило аксиомы: `axiom` — только с `--reason`. Проверяется до
/// чтения stdin и запросов: сервер аксиому агента не проверяет (§7.5, решение владельца
/// 2026-10-26), правило инструкции агента CLI держит сам.
fn status_reason<'a>(f: &Fields<'a>) -> Result<Option<&'a str>, CliError> {
    let reason = f.reason.map(str::trim).filter(|r| !r.is_empty());
    if f.certainty == Some("axiom") && reason.is_none() {
        return Err(CliError::input(
            "--certainty axiom ставится только по прямому указанию пользователя: укажите --reason (что именно сказал пользователь)",
        ));
    }
    Ok(reason)
}

/// Тело запроса новой записи insight (без `scope`, `force`, `supersedes_id`). Флаги решения и
/// доказательства проверяются до чтения stdin.
fn insight_body(f: &Fields, kind: &str) -> Result<Value, CliError> {
    let reason = status_reason(f)?;
    let mut data = json!({ "kind": kind });
    if let Some(d) = decision_for(&f.decision, kind)? {
        data["decision"] = d;
    }
    if let Some(p) = f.evidence_file {
        data["evidence"] = read_evidence(p)?;
    } else if let Some(e) = &f.evidence {
        data["evidence"] = json!({ "checked": e.checked, "method": e.method, "result": e.result });
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
    if let Some(r) = reason {
        body["message"] = json!(r);
    }
    if let Some(r) = super::refs::request_refs(f.refs)? {
        body["refs"] = r;
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
    CliError::with_problem(EXIT_CONFLICT, msg, e.problem)
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
    if let Some(r) = o.supersedes_rev {
        body["supersedes_rev"] = json!(r);
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
    if created {
        ctx.note_write("insight");
    }
    let e = read_json(r)?;
    note_default_certainty(&o.fields, &e);
    emit_entry(ctx, &e, created)
}

/// Строка [`DEFAULT_CERTAINTY_NOTE`] в stderr (и с `--json`): `--certainty` не задан, а сервер
/// поставил записи `e` умолчание агента `hypothesis` — агент узнаёт, что запись не `theorem`
/// (M6.6 §9). stdout `<id>@<rev>` не меняется: его разбирают тесты и скрипты.
fn note_default_certainty(f: &Fields, e: &Value) {
    if f.certainty.is_none() && e["certainty"] == "hypothesis" {
        eprintln!("{DEFAULT_CERTAINTY_NOTE}");
    }
}

/// Пространство ленты: `knowledge` при `--global`, иначе решение.
fn list_space(ctx: &Ctx, global: bool, solution: Option<&str>) -> Result<String, CliError> {
    if global {
        Ok("knowledge".into())
    } else {
        solution_guid(ctx, solution)
    }
}

/// `zam insight list`: страница по умолчанию — умолчание сервера (50); обход и подсказка о
/// продолжении — [`list_entries`].
pub fn list(
    ctx: &Ctx,
    status: Option<&str>,
    global: bool,
    solution: Option<&str>,
    page: Paging,
) -> Result<(), CliError> {
    let space = list_space(ctx, global, solution)?;
    let mut q: Vec<(&str, &str)> = vec![("kind", "insight")];
    if let Some(s) = status {
        q.push(("status", s));
    }
    let v = list_entries(ctx, &space, &q, page.limit, page.all)?;
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

/// Ревизия записи из `GET /entries/{id}`.
fn rev_of(e: &Value) -> Result<i64, CliError> {
    e["rev"]
        .as_i64()
        .ok_or_else(|| CliError::new(crate::client::EXIT_UNREACHABLE, "в ответе нет rev записи"))
}

/// 409 с `rev` текущей на правку с ревизией `sent` → «запись изменилась (rev N → M):
/// перечитайте», код [`EXIT_CONFLICT`] (как у заметок, решение контроллера R7); прочие
/// ошибки — как есть.
fn explain_stale(e: CliError, sent: i64) -> CliError {
    let Some(cur) = e
        .problem
        .as_ref()
        .filter(|p| p["status"] == 409)
        .and_then(|p| p["rev"].as_i64())
    else {
        return e;
    };
    CliError::with_problem(
        EXIT_CONFLICT,
        format!("запись изменилась (rev {sent} → {cur}): перечитайте"),
        e.problem,
    )
}

/// `PATCH /entries/{id}` с `If-Match: <rev>` (§11.3).
fn patch_entry(ctx: &Ctx, id: &str, rev: i64, body: &Value) -> Result<Value, CliError> {
    let r = ctx
        .client
        .patch_json_if(&format!("/entries/{id}"), body, rev)
        .map_err(|e| explain_stale(e, rev))?;
    let v = read_json(r)?;
    ctx.note_write("insight");
    Ok(v)
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
    out.push_str(&trigger_lines(&e));
    out.push_str(&usage_line(&e["usage"]));
    let body = e["body"].as_str().unwrap_or("");
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body.trim_end());
        out.push('\n');
    }
    emit(out.as_bytes())
}

/// Строка счётчиков `show`: «показы · открыто · полезно · бесполезно» (M7.3 §6.6 п. 3); у сервера
/// без признака `usage_opened` (0.0.15 и раньше — в `usage` нет `opened`) — прежняя строка без
/// «открыто»; нет `usage` — пусто.
fn usage_line(u: &Value) -> String {
    if !u.is_object() {
        return String::new();
    }
    match u.get("opened") {
        Some(opened) => format!(
            "показы: {} · открыто: {opened} · полезно: {} · бесполезно: {}\n",
            u["shown"], u["useful"], u["useless"]
        ),
        None => format!(
            "показы: {} · полезно: {} · бесполезно: {}\n",
            u["shown"], u["useful"], u["useless"]
        ),
    }
}

/// Строки `триггер: <вид>:<шаблон>` записи (`show`, `triggers`).
fn trigger_lines(e: &Value) -> String {
    e["triggers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            format!(
                "триггер: {}:{}\n",
                t["type"].as_str().unwrap_or("?"),
                t["pattern"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// `zam insight triggers <id> (--trigger ВИД:ШАБЛОН… | --clear)` (M6.6 §8, замечание 13): набор
/// триггеров заменяется целиком без замещения записи — `PATCH {triggers}` с `If-Match` ревизии
/// из `GET`, как у `certify`; пустой набор снимает все. Неверный триггер — код 6 до запросов;
/// запись изменилась между чтением и правкой — код 4. Печать — `<id>@<rev> триггеров K` (как
/// `<id>@<rev> <статус>` соседних правок) и новый набор строками, как в `show`.
pub fn triggers(ctx: &Ctx, id: &str, raw: &[String]) -> Result<(), CliError> {
    let id = crate::target::bare_entry_id(id);
    let set = raw
        .iter()
        .map(|t| parse_trigger(t))
        .collect::<Result<Vec<_>, _>>()?;
    let rev = rev_of(&get_entry(ctx, id)?)?;
    let e = patch_entry(ctx, id, rev, &json!({ "triggers": set }))?;
    if ctx.json {
        return emit_json(&e);
    }
    let n = e["triggers"].as_array().map_or(0, Vec::len);
    let mut out = format!(
        "{}@{} триггеров {n}\n",
        e["id"].as_str().unwrap_or(id),
        e["rev"]
    );
    out.push_str(&trigger_lines(&e));
    emit(out.as_bytes())
}

/// `zam insight supersede <id>`: новая запись замещает старую (вид — у старой, если не задан).
/// Флаги решения — только у решения: число вариантов проверяется до запросов, вид — после
/// чтения старой записи и до замещения (M6.6 §9). `rev` (`--rev`) — `If-Match` замещаемой записи:
/// ревизия, с которой прочитан текст с метками `‹secret-…›` (M7 §4.3.2 п. 3; без неё — 428).
pub fn supersede(ctx: &Ctx, id: &str, f: &Fields, rev: Option<i64>) -> Result<(), CliError> {
    status_reason(f)?;
    f.decision.data()?;
    let kind = match f.kind {
        Some(k) => k.to_string(),
        None => get_entry(ctx, id)?["data"]["kind"]
            .as_str()
            .unwrap_or("discovery")
            .to_string(),
    };
    let body = insight_body(f, &kind)?;
    let rev = rev.map(|r| r.to_string());
    let mut headers = write_headers(f);
    if let Some(r) = &rev {
        headers.push(("If-Match", r.as_str()));
    }
    let r = ctx
        .client
        .post_json_with(&format!("/entries/{id}/supersede"), &body, &headers)?;
    let created = r.status().as_u16() == 201;
    let v = read_json(r)?;
    ctx.note_write("insight");
    note_default_certainty(f, &v["entry"]);
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
    let e = read_json(r)?;
    ctx.note_write("insight");
    emit_changed(ctx, &e)
}

/// `zam insight revisit <id> [--reason R]`.
pub fn revisit(ctx: &Ctx, id: &str, reason: Option<&str>) -> Result<(), CliError> {
    let mut body = json!({ "status": "revisit" });
    if let Some(r) = reason {
        body["reason"] = json!(r);
    }
    let rev = rev_of(&get_entry(ctx, id)?)?;
    emit_changed(ctx, &patch_entry(ctx, id, rev, &body)?)
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
    let current = get_entry(ctx, id)?;
    let body = certify_body(Some(&current), certainty, reason, evidence);
    let e = patch_entry(ctx, id, rev_of(&current)?, &body)?;
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

/// `zam insight feedback <id> useful|useless [--session S]`.
pub fn feedback(ctx: &Ctx, id: &str, value: &str, session: Option<&str>) -> Result<(), CliError> {
    let mut body = json!({ "value": value });
    if let Some(s) = session {
        body["session_id"] = json!(s);
    }
    ctx.client
        .post_json(&format!("/entries/{id}/feedback"), &body)?;
    if ctx.json {
        return emit_json(&json!({ "id": id, "value": value }));
    }
    emit(format!("{id} {value}\n").as_bytes())
}

/// `zam insight append <id> [--by human|agent]` — текст из `--body-file` или stdin; пустой →
/// код 6. `id` — голый или со своей буквой (`i:`, `w:` …). `--by` — чья дописка идеи (M6.5 §6):
/// `human` — слова человека из чата, возвращённая идея снова идёт на разбор; у записи не идеи
/// сервер отвечает 422 (код 6).
pub fn append(
    ctx: &Ctx,
    id: &str,
    body_file: Option<&Path>,
    by: Option<&str>,
) -> Result<(), CliError> {
    let id = crate::target::bare_entry_id(id);
    let text = read_text(body_file)?;
    if text.trim().is_empty() {
        return Err(CliError::input(
            "нечего дописывать: текст из --body-file или stdin",
        ));
    }
    let rev = rev_of(&get_entry(ctx, id)?)?;
    let mut body = json!({ "append_body": text });
    if let Some(by) = by {
        body["by"] = json!(by);
    }
    emit_changed(ctx, &patch_entry(ctx, id, rev, &body)?)
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

    /// Стенд Debian 13: `--certainty axiom` — только с непустой `--reason`; причина —
    /// сообщение ревизии.
    #[test]
    fn axiom_requires_reason() {
        let f = |certainty: Option<&'static str>, reason: Option<&'static str>| Fields {
            kind: Some("decision"),
            title: "T",
            body_file: None,
            certainty,
            reason,
            evidence_file: None,
            evidence: None,
            decision: Decision::default(),
            triggers: &[],
            tags: &[],
            refs: &[],
            area: None,
            key: None,
        };
        for r in [None, Some(""), Some("  ")] {
            let e = status_reason(&f(Some("axiom"), r)).unwrap_err();
            assert_eq!(e.code, crate::client::EXIT_INPUT);
            assert!(e.msg.contains("--reason"), "{}", e.msg);
        }
        assert_eq!(
            status_reason(&f(Some("axiom"), Some(" сказал пользователь "))).unwrap(),
            Some("сказал пользователь")
        );
        assert_eq!(status_reason(&f(Some("hypothesis"), None)).unwrap(), None);
        assert_eq!(status_reason(&f(None, Some("R"))).unwrap(), Some("R"));
    }

    /// M6.6 §9: `data.decision` — только заданные поля; флаги решения у не-решения — код 6 с
    /// именами флагов и видом; вариантов больше 8 — код 6 при любом виде.
    #[test]
    fn decision_fields_and_kind() {
        assert_eq!(decision_for(&Decision::default(), "gotcha").unwrap(), None);
        let opts = ["a".to_string(), "b".to_string()];
        let d = Decision {
            options: &opts,
            chosen: Some("c"),
            decided_by: Some("human"),
            ..Decision::default()
        };
        assert_eq!(
            decision_for(&d, "decision").unwrap(),
            Some(json!({"options": ["a", "b"], "chosen": "c", "decided_by": "human"}))
        );
        let e = decision_for(&d, "gotcha").unwrap_err();
        assert_eq!(e.code, crate::client::EXIT_INPUT);
        assert_eq!(
            e.msg,
            "--option, --chosen, --decided-by: поля решения — только у --kind decision \
             (вид записи — gotcha)"
        );
        let nine: Vec<String> = (1..=9).map(|i| i.to_string()).collect();
        let many = Decision {
            options: &nine,
            ..Decision::default()
        };
        let e = decision_for(&many, "decision").unwrap_err();
        assert_eq!(
            (e.code, e.msg.as_str()),
            (
                crate::client::EXIT_INPUT,
                "--option: не больше 8 вариантов решения (задано 9)"
            )
        );
        assert_eq!(many.data().unwrap_err().code, crate::client::EXIT_INPUT);
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
    fn stale_revision_explained() {
        let e = CliError::with_problem(
            EXIT_CONFLICT,
            "x",
            Some(json!({"type": "/problems/conflict", "status": 409, "rev": 5})),
        );
        let s = explain_stale(e, 3);
        assert_eq!(s.code, EXIT_CONFLICT);
        assert_eq!(s.msg, "запись изменилась (rev 3 → 5): перечитайте");
        // 409 без rev (например, содержимое superseded) — как есть
        let other = CliError::with_problem(
            EXIT_CONFLICT,
            "y",
            Some(json!({"type": "/problems/conflict", "status": 409})),
        );
        let o = explain_stale(other, 3);
        assert_eq!((o.code, o.msg.as_str()), (EXIT_CONFLICT, "y"));
    }

    #[test]
    fn duplicate_problem_explained() {
        let e = CliError::with_problem(
            crate::client::EXIT_CONFLICT,
            "x",
            Some(json!({
                "type": "/problems/possible-duplicate",
                "dedup": "full",
                "candidates": [{"id": "01A", "status": "active", "title": "LF", "reason": "vector", "score": 0.93}]
            })),
        );
        let m = explain_duplicate(e).msg;
        assert!(m.contains("01A [active] «LF» — vector 0.93"), "{m}");
        assert!(m.contains("--supersedes") && m.contains("--force") && m.contains("append"));
        // прочие ошибки не меняются
        let other = CliError::input("y");
        assert_eq!(explain_duplicate(other).msg, "y");
    }
}
