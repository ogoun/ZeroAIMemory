//! Инструменты MCP (§13.1): определения (имена, описания до 300 символов, схемы аргументов) и
//! вызов маршрутов REST через [`Rest`] (решение контроллера R12 — без обхода слоёв).
//!
//! - **Ответ** — компактный Markdown; `format: "json"` — JSON ответа REST в тексте (без
//!   дубля в `structuredContent`). Последняя строка любого ответа, в том числе ошибки, — `~N tok`
//!   (счётчик чанкера; у `memory_read` в Markdown — итог самого `/read`).
//! - **Ошибки сервиса** — результат `isError: true` с текстом problem (`title`, `detail`) и
//!   подсказкой: 409 — текущая ревизия, дифф (у заметок) и что перечитать; 428 — какую ревизию
//!   передать; 409 `possible-duplicate` — кандидаты и выбор (append, supersede, force).
//!   Неизвестный инструмент — ошибка JSON-RPC (`invalid_params`), неверные аргументы —
//!   `isError: true`.
//! - **Пространство по умолчанию** (`memory_tree`, `memory_write`, `memory_log`,
//!   `memory_vault`) **[решение исполнителя]** — решение из `X-Zam-Solution`, без него —
//!   `knowledge`.
//! - **Ссылки** `i:|c:|r:|e:<id>` (как в карточках поиска) принимаются везде, где REST ждёт id
//!   записи; в `memory_link` они приводятся к `e:<id>` маршрута `/edges`.
use std::sync::{Arc, LazyLock};

use axum::http::{HeaderName, Method, StatusCode, header};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations};
use serde_json::{Map, Value, json};
use zam_store::count_tokens;

use super::{Reply, Rest};

/// Предел диффа конфликта ревизии в тексте ошибки, символов.
const DIFF_MAX_CHARS: usize = 4000;

/// Имена инструментов (§13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolName {
    Search,
    Read,
    CachePut,
    Tree,
    Write,
    Log,
    UpdateEntry,
    Link,
    Recall,
    Vault,
}

impl ToolName {
    pub const ALL: [ToolName; 10] = [
        ToolName::Search,
        ToolName::Read,
        ToolName::CachePut,
        ToolName::Tree,
        ToolName::Write,
        ToolName::Log,
        ToolName::UpdateEntry,
        ToolName::Link,
        ToolName::Recall,
        ToolName::Vault,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ToolName::Search => "memory_search",
            ToolName::Read => "memory_read",
            ToolName::CachePut => "memory_cache_put",
            ToolName::Tree => "memory_tree",
            ToolName::Write => "memory_write",
            ToolName::Log => "memory_log",
            ToolName::UpdateEntry => "memory_update_entry",
            ToolName::Link => "memory_link",
            ToolName::Recall => "memory_recall",
            ToolName::Vault => "memory_vault",
        }
    }

    pub fn parse(s: &str) -> Option<ToolName> {
        ToolName::ALL.into_iter().find(|t| t.as_str() == s)
    }

    fn description(self) -> &'static str {
        match self {
            ToolName::Search => {
                "Поиск в памяти (Knowledge, Rules, текущее решение). Сначала готовые выжимки кэша, \
                 затем карточки: id, путь, ревизия, ~токены; выданное в этой сессии помечено seen. \
                 Дальше — memory_read нужного."
            }
            ToolName::Read => {
                "Чтение пачкой до 20 целей: n:<id>, i:|c:|r:<id> или путь <пространство>:/<путь>. \
                 tier L0|L1|L2, section, max_tokens; if_rev равна текущей — unchanged без текста. \
                 Ошибка по элементу — в самом элементе."
            }
            ToolName::CachePut => {
                "Сохранить выжимку ответа на запрос (до 600 токенов) с источниками [{id, rev?, \
                 section?}]: следующий поиск того же запроса получит её первой. promote {as: \
                 note|insight, path?, kind?} — сразу превратить в заметку или insight."
            }
            ToolName::Tree => {
                "Дерево узлов пространства с L0: space — knowledge, rules или GUID решения (по \
                 умолчанию решение из X-Zam-Solution, иначе knowledge), path — поддерево, depth \
                 1–5."
            }
            ToolName::Write => {
                "Запись заметки по пути [<пространство>:/]<путь>: без base_rev — создать (content); \
                 с base_rev — заменить content или применить ops к секциям. Ревизия устарела — \
                 ошибка 409 с диффом: перечитать и повторить."
            }
            ToolName::Log => {
                "Новая запись журнала: kind changelog|release (только в решении) или insight \
                 (data.kind, triggers, links, scope solution|global, certainty). Похожий insight — \
                 ошибка 409 с кандидатами: append, supersede или force."
            }
            ToolName::UpdateEntry => {
                "Изменить запись журнала: action supersede|retract|revisit|certify|append|feedback; \
                 rev — текущая ревизия (кроме feedback). certify — certainty и reason (axiom — \
                 только по указанию пользователя); feedback — value useful|useless."
            }
            ToolName::Link => {
                "Связь графа: src, type, dst (n:<id>, i:<id>; dst также [[Имя]]) — создать явную \
                 связь; remove: <edge_id> — удалить её."
            }
            ToolName::Recall => {
                "Подсказки памяти к событию: session_start — дайджест (Rules, решение, gotcha, \
                 revisit, changelog), prompt — к запросу пользователя, post_tool — к ошибке \
                 инструмента (error, tool). Повторы в сессии отсекаются."
            }
            ToolName::Vault => {
                "Vault: list — секреты без значений, get — значение по name (раскрытие пишется в \
                 аудит), set — создать или обновить name = value. scope: global (личный) или \
                 solution (решение из X-Zam-Solution)."
            }
        }
    }

    /// Только чтение памяти (подсказка клиенту; `memory_recall` пишет лишь учёт показов).
    fn read_only(self) -> bool {
        matches!(
            self,
            ToolName::Search | ToolName::Read | ToolName::Tree | ToolName::Recall
        )
    }

    fn schema(self) -> Value {
        let fmt = json!({"type": "string", "enum": ["md", "json"],
                         "description": "md (по умолчанию) или json"});
        let (props, required): (Value, &[&str]) = match self {
            ToolName::Search => (
                json!({
                    "query": {"type": "string", "description": "запрос, до 512 символов"},
                    "mode": {"type": "string", "enum": ["brief", "chunks", "notes"]},
                    "scope": {"type": "object", "description":
                        "solution, include_knowledge, include_rules, all_solutions, path_prefix, area"},
                    "filters": {"type": "object", "description": "kinds, status, certainty"},
                    "top_k": {"type": "integer", "minimum": 1, "maximum": 100},
                    "budget_tokens": {"type": "integer", "minimum": 1},
                    "expand": {"type": "string", "enum": ["none", "section", "neighbors", "graph"]},
                    "rerank": {"type": "string", "enum": ["off", "on"]},
                    "format": fmt,
                }),
                &["query"],
            ),
            ToolName::Read => (
                json!({
                    "items": {"type": "array", "minItems": 1, "maxItems": 20, "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "tier": {"type": "string", "enum": ["L0", "L1", "L2"]},
                            "section": {"type": "string", "description": "путь заголовков «A > B»"},
                            "max_tokens": {"type": "integer", "minimum": 1},
                            "if_rev": {"type": "integer"},
                        },
                        "required": ["id"],
                    }},
                    "budget_tokens": {"type": "integer", "minimum": 1},
                    "format": fmt,
                }),
                &["items"],
            ),
            ToolName::CachePut => (
                json!({
                    "query": {"type": "string"},
                    "summary": {"type": "string", "description": "выжимка, до 600 токенов"},
                    "sources": {"type": "array", "minItems": 1, "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "rev": {"type": "integer"},
                            "section": {"type": "string"},
                        },
                        "required": ["id"],
                    }},
                    "promote": {"type": "object", "properties": {
                        "as": {"type": "string", "enum": ["note", "insight"]},
                        "path": {"type": "string"},
                        "kind": {"type": "string"},
                    }, "required": ["as"]},
                    "scope": {"type": "object", "description": "как у memory_search"},
                    "filters": {"type": "object", "description": "как у memory_search"},
                    "format": fmt,
                }),
                &["query", "summary", "sources"],
            ),
            ToolName::Tree => (
                json!({
                    "space": {"type": "string"},
                    "path": {"type": "string"},
                    "depth": {"type": "integer", "minimum": 1, "maximum": 5},
                    "format": fmt,
                }),
                &[],
            ),
            ToolName::Write => (
                json!({
                    "path": {"type": "string", "description": "[<пространство>:/]<путь>"},
                    "content": {"type": "string"},
                    "ops": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "op": {"type": "string", "enum": ["replace_section",
                                "append_to_section", "insert_after_heading", "append", "prepend"]},
                            "section": {"type": "string"},
                            "content": {"type": "string"},
                        },
                        "required": ["op", "content"],
                    }},
                    "base_rev": {"type": "integer"},
                    "message": {"type": "string"},
                    "l0": {"type": "string"},
                    "format": fmt,
                }),
                &["path"],
            ),
            ToolName::Log => (
                json!({
                    "kind": {"type": "string", "enum": ["changelog", "insight", "release"]},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "data": {"type": "object", "description": "insight: kind, experiment, evidence"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "certainty": {"type": "string", "enum": ["axiom", "theorem", "hypothesis"]},
                    "triggers": {"type": "array", "items": {"type": "object"}},
                    "links": {"type": "array", "items": {"type": "object"}},
                    "scope": {"type": "string", "enum": ["solution", "global"]},
                    "space": {"type": "string"},
                    "force": {"type": "boolean"},
                    "message": {"type": "string"},
                    "idempotency_key": {"type": "string"},
                    "format": fmt,
                }),
                &["kind", "title"],
            ),
            ToolName::UpdateEntry => (
                json!({
                    "id": {"type": "string"},
                    "rev": {"type": "integer"},
                    "action": {"type": "string", "enum": ["supersede", "retract", "revisit",
                        "certify", "append", "feedback"]},
                    "title": {"type": "string"},
                    "body": {"type": "string", "description": "supersede — тело; append — дописать"},
                    "data": {"type": "object"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "triggers": {"type": "array", "items": {"type": "object"}},
                    "links": {"type": "array", "items": {"type": "object"}},
                    "certainty": {"type": ["string", "null"],
                        "enum": ["axiom", "theorem", "hypothesis", null]},
                    "reason": {"type": "string"},
                    "value": {"type": "string", "enum": ["useful", "useless"]},
                    "message": {"type": "string"},
                    "format": fmt,
                }),
                &["id", "action"],
            ),
            ToolName::Link => (
                json!({
                    "src": {"type": "string"},
                    "type": {"type": "string"},
                    "dst": {"type": "string"},
                    "remove": {"type": "string", "description": "id связи"},
                    "format": fmt,
                }),
                &[],
            ),
            ToolName::Recall => (
                json!({
                    "event": {"type": "string", "enum": ["session_start", "prompt", "post_tool"]},
                    "prompt": {"type": "string"},
                    "error": {"type": "string"},
                    "tool": {"type": "string"},
                    "session_id": {"type": "string",
                        "description": "по умолчанию — X-Zam-Session"},
                    "budget_tokens": {"type": "integer", "minimum": 1},
                    "format": fmt,
                }),
                &["event"],
            ),
            ToolName::Vault => (
                json!({
                    "action": {"type": "string", "enum": ["get", "set", "list"]},
                    "name": {"type": "string"},
                    "id": {"type": "string"},
                    "value": {"type": "string"},
                    "type": {"type": "string", "description":
                        "set: тип нового секрета (password по умолчанию)"},
                    "scope": {"type": "string", "description":
                        "global, solution или GUID решения"},
                    "format": fmt,
                }),
                &["action"],
            ),
        };
        json!({"type": "object", "properties": props, "required": required})
    }
}

/// Определения инструментов (собираются один раз).
pub fn definitions() -> &'static [Tool] {
    static DEFS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
        ToolName::ALL
            .into_iter()
            .map(|t| {
                let Value::Object(schema) = t.schema() else {
                    unreachable!("схема инструмента — объект");
                };
                Tool::new(t.as_str(), t.description(), Arc::new(schema))
                    .with_annotations(ToolAnnotations::new().read_only(t.read_only()))
            })
            .collect()
    });
    &DEFS
}

/// Итог вызова инструмента.
pub struct Output {
    pub text: String,
    pub is_error: bool,
}

impl Output {
    pub fn into_result(self) -> CallToolResult {
        let content = vec![ContentBlock::text(self.text)];
        if self.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
    }
}

/// Успешный результат: Markdown или JSON ответа REST.
enum Done {
    Md(String),
    Json(Value),
}

/// Неуспех инструмента — результат `isError: true`.
enum Fail {
    /// Ответ REST с ошибкой и подсказка инструмента.
    Rest {
        reply: Box<Reply>,
        hint: Option<String>,
    },
    /// Отказ самого инструмента с семантикой HTTP (секрет не найден — 404, имя
    /// неоднозначно — 409): тот же вид текста, что у problem REST.
    Problem { status: StatusCode, detail: String },
    /// Некорректные аргументы.
    Invalid(String),
    /// Сбой внутреннего вызова.
    Internal(String),
}

impl Fail {
    fn hint(self, h: impl Into<String>) -> Fail {
        match self {
            Fail::Rest { reply, .. } => Fail::Rest {
                reply,
                hint: Some(h.into()),
            },
            f => f,
        }
    }
}

type Res = Result<Done, Fail>;

/// Вызов инструмента: ответ или ошибка как результат с `~N tok` в конце.
pub async fn call(tool: ToolName, rest: &Rest, args: JsonObject) -> Output {
    let r = match tool {
        ToolName::Search => search(rest, &args).await,
        ToolName::Read => read(rest, &args).await,
        ToolName::CachePut => cache_put(rest, &args).await,
        ToolName::Tree => tree(rest, &args).await,
        ToolName::Write => write(rest, &args).await,
        ToolName::Log => log(rest, &args).await,
        ToolName::UpdateEntry => update_entry(rest, &args).await,
        ToolName::Link => link(rest, &args).await,
        ToolName::Recall => recall(rest, &args).await,
        ToolName::Vault => vault(rest, &args).await,
    };
    match r {
        Ok(Done::Md(s)) => Output {
            text: with_tokens(s),
            is_error: false,
        },
        Ok(Done::Json(v)) => Output {
            text: with_tokens(v.to_string()),
            is_error: false,
        },
        Err(f) => Output {
            text: with_tokens(fail_text(&f)),
            is_error: true,
        },
    }
}

/// Текст с последней строкой `~N tok` (уже есть — как есть).
fn with_tokens(mut s: String) -> String {
    let last = s.trim_end().rsplit('\n').next().unwrap_or("").trim();
    let has = last
        .strip_prefix('~')
        .and_then(|t| t.strip_suffix(" tok"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if has {
        return s;
    }
    let n = count_tokens(&s);
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&format!("~{n} tok"));
    s
}

/// Текст ошибки: problem (`title`, `detail`) и что делать дальше.
fn fail_text(f: &Fail) -> String {
    match f {
        Fail::Invalid(m) => format!("Ошибка аргументов: {m}"),
        Fail::Problem { status, detail } => format!(
            "Ошибка {} {}: {detail}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Error")
        ),
        Fail::Internal(m) => format!("Внутренняя ошибка MCP: {m}"),
        Fail::Rest { reply, hint } => {
            let mut s = problem_text(reply);
            if let Some(h) = hint {
                s.push('\n');
                s.push_str(h);
            }
            s
        }
    }
}

fn problem_text(reply: &Reply) -> String {
    let p = reply.json();
    let status = reply.status;
    let title = p["title"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("Error").to_string());
    let mut s = format!("Ошибка {} {title}", status.as_u16());
    match p["detail"].as_str() {
        Some(d) if !d.is_empty() => s.push_str(&format!(": {d}")),
        _ if p.is_null() && !reply.body.is_empty() => {
            s.push_str(&format!(": {}", truncate(&reply.text(), 500)));
        }
        _ => {}
    }
    if let Some(code) = p["code"].as_str() {
        s.push_str(&format!(" (код {code})"));
    }
    match status {
        StatusCode::CONFLICT => {
            let rev = p
                .get("current_rev")
                .or_else(|| p.get("rev"))
                .and_then(Value::as_i64);
            if let Some(rev) = rev {
                s.push_str(&format!(
                    "\nТекущая ревизия — {rev}. Перечитайте цель (memory_read), учтите чужие \
                     правки и повторите с ревизией {rev} (base_rev у заметки, rev у записи)."
                ));
            }
            if let Some(c) = p["candidates"].as_array() {
                s.push_str("\nПохожие записи:");
                for x in c {
                    s.push_str(&format!(
                        "\n- i:{} «{}» {}",
                        x["id"].as_str().unwrap_or("?"),
                        x["title"].as_str().unwrap_or(""),
                        x["status"].as_str().unwrap_or("")
                    ));
                }
                s.push_str(
                    "\nДополнить — memory_update_entry action=append; заменить — action=supersede; \
                     записать всё равно — memory_log с force: true.",
                );
            }
            if let Some(d) = p["diff"].as_str().filter(|d| !d.is_empty()) {
                s.push_str(&format!(
                    "\nИзменения с вашей ревизии:\n```diff\n{}\n```",
                    truncate(d, DIFF_MAX_CHARS)
                ));
            }
        }
        StatusCode::PRECONDITION_REQUIRED => s.push_str(
            "\nНужна текущая ревизия: rev у записи журнала, base_rev у заметки — её даёт \
             memory_read (или карточка memory_search).",
        ),
        StatusCode::TOO_MANY_REQUESTS => {
            if let Some(r) = reply
                .headers
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
            {
                s.push_str(&format!("\nПовторите через {r} с."));
            }
        }
        _ => {}
    }
    s
}

/// Первые `max` символов (с многоточием, если длиннее).
fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

// ---- аргументы ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Md,
    Json,
}

fn fmt(args: &JsonObject) -> Result<Fmt, Fail> {
    match opt_str(args, "format")? {
        None | Some("md") => Ok(Fmt::Md),
        Some("json") => Ok(Fmt::Json),
        Some(_) => Err(Fail::Invalid("format: md или json".into())),
    }
}

fn opt_str<'a>(args: &'a JsonObject, key: &str) -> Result<Option<&'a str>, Fail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(Fail::Invalid(format!("{key}: строка"))),
    }
}

fn req_str<'a>(args: &'a JsonObject, key: &str) -> Result<&'a str, Fail> {
    opt_str(args, key)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Fail::Invalid(format!("нужен {key}")))
}

/// Целое (число или строка из цифр — так ревизию иногда передают агенты).
fn opt_int(args: &JsonObject, key: &str) -> Result<Option<i64>, Fail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if n.is_i64() => Ok(n.as_i64()),
        Some(Value::String(s)) if s.trim().parse::<i64>().is_ok() => Ok(s.trim().parse().ok()),
        Some(_) => Err(Fail::Invalid(format!("{key}: целое число"))),
    }
}

/// Поля `keys`, заданные в аргументах (без `null`), — тело запроса REST.
fn pick(args: &JsonObject, keys: &[&str]) -> Map<String, Value> {
    keys.iter()
        .filter_map(|k| {
            args.get(*k)
                .filter(|v| !v.is_null())
                .map(|v| (k.to_string(), v.clone()))
        })
        .collect()
}

/// Percent-encoding сегмента пути или значения query: всё, кроме `A–Z a–z 0–9 - . _ ~`.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Пространство по умолчанию: решение из `X-Zam-Solution`, иначе `knowledge`.
fn default_space(rest: &Rest) -> String {
    rest.header(crate::api::search::SOLUTION_HEADER)
        .unwrap_or("knowledge")
        .to_string()
}

/// Id записи журнала без префикса ссылки `i:|c:|r:|e:`.
fn entry_id(raw: &str) -> &str {
    let t = raw.trim();
    ["i:", "c:", "r:", "e:", "entry:"]
        .iter()
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(t)
}

/// Ссылка графа: `i:|c:|r:<id>` → `e:<id>`, остальное — как есть.
fn graph_ref(raw: &str) -> String {
    let t = raw.trim();
    match ["i:", "c:", "r:"].iter().find_map(|p| t.strip_prefix(p)) {
        Some(id) => format!("e:{id}"),
        None => t.to_string(),
    }
}

// ---- вызов REST ----

/// Запрос к REST; не-2xx → [`Fail::Rest`].
async fn call_rest(
    rest: &Rest,
    method: Method,
    uri: &str,
    body: Option<&Value>,
    extra: &[(HeaderName, String)],
) -> Result<Reply, Fail> {
    let r = rest
        .send(method, uri, body, extra)
        .await
        .map_err(Fail::Internal)?;
    if r.status.is_success() {
        Ok(r)
    } else {
        Err(Fail::Rest {
            reply: Box::new(r),
            hint: None,
        })
    }
}

fn if_match(rev: i64) -> (HeaderName, String) {
    (header::IF_MATCH, rev.to_string())
}

/// Ответ REST: JSON как есть или Markdown функцией `md`.
fn done(f: Fmt, reply: &Reply, md: impl FnOnce(&Value) -> String) -> Res {
    let v = reply.json();
    Ok(match f {
        Fmt::Json => Done::Json(v),
        Fmt::Md => Done::Md(md(&v)),
    })
}

/// Строки `> warning: …` по массиву `warnings` ответа.
fn warnings_md(v: &Value) -> String {
    v["warnings"]
        .as_array()
        .map(|w| {
            w.iter()
                .map(|x| match x.as_str() {
                    Some(s) => format!("\n> warning: {s}"),
                    None => format!("\n> warning: {x}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- инструменты ----

async fn search(rest: &Rest, args: &JsonObject) -> Res {
    req_str(args, "query")?;
    let f = fmt(args)?;
    let mut body = pick(
        args,
        &[
            "query",
            "mode",
            "scope",
            "filters",
            "top_k",
            "budget_tokens",
            "expand",
            "rerank",
            "include_history",
        ],
    );
    body.insert("format".into(), json!(fmt_str(f)));
    let r = call_rest(
        rest,
        Method::POST,
        "/api/v1/search",
        Some(&Value::Object(body)),
        &[],
    )
    .await?;
    passthrough(f, &r, "Ничего не найдено.")
}

async fn read(rest: &Rest, args: &JsonObject) -> Res {
    if !args.get("items").is_some_and(Value::is_array) {
        return Err(Fail::Invalid("нужен items: [{id, …}]".into()));
    }
    let f = fmt(args)?;
    let mut body = pick(args, &["items", "budget_tokens"]);
    body.insert("format".into(), json!(fmt_str(f)));
    let r = call_rest(
        rest,
        Method::POST,
        "/api/v1/read",
        Some(&Value::Object(body)),
        &[],
    )
    .await?;
    passthrough(f, &r, "")
}

fn fmt_str(f: Fmt) -> &'static str {
    match f {
        Fmt::Md => "md",
        Fmt::Json => "json",
    }
}

/// Ответ REST, уже отформатированный самим маршрутом (`format` в теле).
fn passthrough(f: Fmt, r: &Reply, empty: &str) -> Res {
    Ok(match f {
        Fmt::Json => Done::Json(r.json()),
        Fmt::Md => {
            let t = r.text();
            Done::Md(if t.trim().is_empty() {
                empty.to_string()
            } else {
                t
            })
        }
    })
}

async fn cache_put(rest: &Rest, args: &JsonObject) -> Res {
    req_str(args, "query")?;
    req_str(args, "summary")?;
    let f = fmt(args)?;
    let body = pick(
        args,
        &[
            "query",
            "summary",
            "sources",
            "promote",
            "scope",
            "filters",
            "include_history",
        ],
    );
    let r = call_rest(
        rest,
        Method::POST,
        "/api/v1/cache",
        Some(&Value::Object(body)),
        &[],
    )
    .await?;
    done(f, &r, |v| {
        let n = v["sources"].as_array().map_or(0, Vec::len);
        let mut s = format!(
            "Выжимка {} сохранена: «{}» · {} tok · источников {n} · {}",
            v["ref"].as_str().unwrap_or("?"),
            v["query"].as_str().unwrap_or(""),
            v["tokens"],
            v["state"].as_str().unwrap_or("")
        );
        if let Some(p) = v["promoted"].as_str() {
            s.push_str(&format!("\nПревращена в {p}"));
        }
        s
    })
}

async fn tree(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    let space = opt_str(args, "space")?
        .map(str::to_string)
        .unwrap_or_else(|| default_space(rest));
    let path = opt_str(args, "path")?.unwrap_or("").trim_matches('/');
    let mut uri = format!("/api/v1/spaces/{}/tree?path={}", enc(&space), enc(path));
    if let Some(d) = opt_int(args, "depth")? {
        uri.push_str(&format!("&depth={d}"));
    }
    let r = call_rest(rest, Method::GET, &uri, None, &[]).await?;
    done(f, &r, |v| tree_md(v, path))
}

fn tree_md(v: &Value, base: &str) -> String {
    let base_depth = base.split('/').filter(|s| !s.is_empty()).count();
    let items = v["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut s = format!(
        "{}:/{base} — узлов {}",
        v["space"]["name"].as_str().unwrap_or("?"),
        items.len()
    );
    for n in items {
        let path = n["path"].as_str().unwrap_or("");
        let depth = path.split('/').filter(|s| !s.is_empty()).count();
        let indent = "  ".repeat(depth.saturating_sub(base_depth + 1));
        let folder = n["kind"].as_str() == Some("folder");
        s.push_str(&format!(
            "\n{indent}- n:{} {path}{}",
            n["id"].as_str().unwrap_or("?"),
            if folder { "/" } else { "" }
        ));
        if !folder {
            s.push_str(&format!(" rev{}", n["current_rev"]));
        }
        if let Some(l0) = n["l0"].as_str().filter(|l| !l.is_empty()) {
            s.push_str(&format!(" · {l0}"));
        }
    }
    s
}

/// `[<пространство>:/]<путь>` → (пространство, путь).
fn split_path(rest: &Rest, raw: &str) -> (String, String) {
    match raw.split_once(":/") {
        Some((sp, p)) if !sp.is_empty() && !sp.contains('/') => {
            (sp.to_string(), p.trim_matches('/').to_string())
        }
        _ => (default_space(rest), raw.trim_matches('/').to_string()),
    }
}

/// Id узла по пути (`GET /nodes/by-path`).
async fn node_id(rest: &Rest, space: &str, path: &str) -> Result<String, Fail> {
    // `tier=L0` отдал бы только сводку и ревизию, без id — нужен полный ответ
    let uri = format!(
        "/api/v1/nodes/by-path?space={}&path={}",
        enc(space),
        enc(path)
    );
    let r = call_rest(rest, Method::GET, &uri, None, &[]).await?;
    r.json()["node"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Fail::Internal("ответ by-path без node.id".into()))
}

async fn write(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    let (space, path) = split_path(rest, req_str(args, "path")?);
    let content = opt_str(args, "content")?;
    let ops = args.get("ops").filter(|v| !v.is_null());
    let base_rev = opt_int(args, "base_rev")?;
    let reread = "Перечитайте заметку (memory_read) и повторите с base_rev текущей ревизии.";
    let r = match (content, ops, base_rev) {
        (Some(_), Some(_), _) => {
            return Err(Fail::Invalid("content и ops вместе не передаются".into()));
        }
        (None, None, _) => return Err(Fail::Invalid("нужен content или ops".into())),
        (None, Some(_), None) => {
            return Err(Fail::Invalid(
                "ops требует base_rev — текущую ревизию даёт memory_read".into(),
            ));
        }
        (None, Some(_), Some(rev)) => {
            if args.get("l0").is_some_and(|v| !v.is_null()) {
                return Err(Fail::Invalid("l0 задаётся только вместе с content".into()));
            }
            let id = node_id(rest, &space, &path).await?;
            let mut body = pick(args, &["ops", "message"]);
            body.insert("base_rev".into(), json!(rev));
            call_rest(
                rest,
                Method::PATCH,
                &format!("/api/v1/nodes/{}", enc(&id)),
                Some(&Value::Object(body)),
                &[],
            )
            .await
            .map_err(|e| e.hint(reread))?
        }
        (Some(_), None, Some(rev)) => {
            let id = node_id(rest, &space, &path).await?;
            let body = pick(args, &["content", "l0", "message"]);
            call_rest(
                rest,
                Method::PUT,
                &format!("/api/v1/nodes/{}", enc(&id)),
                Some(&Value::Object(body)),
                &[if_match(rev)],
            )
            .await
            .map_err(|e| e.hint(reread))?
        }
        (Some(_), None, None) => {
            let mut body = pick(args, &["content", "l0", "message"]);
            body.insert("space".into(), json!(space));
            body.insert("path".into(), json!(path));
            body.insert("kind".into(), json!("note"));
            call_rest(
                rest,
                Method::POST,
                "/api/v1/nodes",
                Some(&Value::Object(body)),
                &[],
            )
            .await
            .map_err(|e| {
                e.hint(
                    "Если заметка уже есть — прочитайте её (memory_read) и передайте base_rev, \
                     чтобы заменить текст.",
                )
            })?
        }
    };
    done(f, &r, |v| {
        format!(
            "Записано n:{} {space}:/{} rev{}{}",
            v["id"].as_str().unwrap_or("?"),
            v["path"].as_str().unwrap_or(&path),
            v["current_rev"],
            warnings_md(v)
        )
    })
}

/// Строка записи журнала: `i:<id> insight/<вид> «заголовок» rev<N> · status · certainty`.
fn entry_line(e: &Value) -> String {
    let kind = e["kind"].as_str().unwrap_or("entry");
    let prefix = match kind {
        "insight" => "i",
        "changelog" => "c",
        "release" => "r",
        _ => "e",
    };
    let mut s = format!("{prefix}:{} {kind}", e["id"].as_str().unwrap_or("?"));
    if let Some(k) = e["data"]["kind"].as_str() {
        s.push_str(&format!("/{k}"));
    }
    s.push_str(&format!(
        " «{}» rev{}",
        e["title"].as_str().unwrap_or(""),
        e["rev"]
    ));
    for k in ["status", "certainty"] {
        if let Some(x) = e[k].as_str() {
            s.push_str(&format!(" · {x}"));
        }
    }
    s
}

async fn log(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    let kind = req_str(args, "kind")?;
    if !["changelog", "insight", "release"].contains(&kind) {
        return Err(Fail::Invalid("kind: changelog, insight или release".into()));
    }
    req_str(args, "title")?;
    let space = opt_str(args, "space")?
        .map(str::to_string)
        .unwrap_or_else(|| default_space(rest));
    let body = pick(
        args,
        &[
            "kind",
            "title",
            "body",
            "data",
            "tags",
            "area",
            "message",
            "certainty",
            "status",
            "triggers",
            "links",
            "scope",
            "force",
            "supersedes_id",
        ],
    );
    let extra: Vec<(HeaderName, String)> = opt_str(args, "idempotency_key")?
        .map(|k| (HeaderName::from_static("idempotency-key"), k.to_string()))
        .into_iter()
        .collect();
    let r = call_rest(
        rest,
        Method::POST,
        &format!("/api/v1/spaces/{}/entries", enc(&space)),
        Some(&Value::Object(body)),
        &extra,
    )
    .await?;
    done(f, &r, |v| {
        let mut s = format!("Записано {}", entry_line(v));
        if let Some(d) = v["dedup"].as_str() {
            s.push_str(&format!(" · dedup {d}"));
        }
        if let Some(old) = v.get("superseded").filter(|o| o.is_object()) {
            s.push_str(&format!("\nЗамещена {}", entry_line(old)));
        }
        s.push_str(&warnings_md(v));
        s
    })
}

async fn update_entry(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    let raw_id = entry_id(req_str(args, "id")?);
    let id = enc(raw_id);
    let action = req_str(args, "action")?;
    let rev = opt_int(args, "rev")?;
    let extra: Vec<(HeaderName, String)> = rev.map(if_match).into_iter().collect();
    let entry = format!("/api/v1/entries/{id}");
    let (method, uri, body) = match action {
        "supersede" => (
            Method::POST,
            format!("{entry}/supersede"),
            pick(
                args,
                &[
                    "title",
                    "body",
                    "data",
                    "tags",
                    "area",
                    "message",
                    "certainty",
                    "triggers",
                    "links",
                    "scope",
                ],
            ),
        ),
        "retract" => (
            Method::POST,
            format!("{entry}/retract"),
            pick(args, &["reason", "message"]),
        ),
        "revisit" => {
            let mut b = pick(args, &["message"]);
            b.insert("status".into(), json!("revisit"));
            (Method::PATCH, entry, b)
        }
        "certify" => {
            if !args.contains_key("certainty") {
                return Err(Fail::Invalid(
                    "certify: нужен certainty (axiom, theorem, hypothesis или null)".into(),
                ));
            }
            let mut b = pick(args, &["reason", "data", "message"]);
            b.insert("certainty".into(), args["certainty"].clone());
            (Method::PATCH, entry, b)
        }
        "append" => {
            let text = opt_str(args, "body")?.ok_or_else(|| {
                Fail::Invalid("append: нужен body — текст для дописывания".into())
            })?;
            let mut b = pick(args, &["message"]);
            b.insert("append_body".into(), json!(text));
            (Method::PATCH, entry, b)
        }
        "feedback" => {
            let value = req_str(args, "value")?;
            let body = json!({ "value": value });
            call_rest(
                rest,
                Method::POST,
                &format!("{entry}/feedback"),
                Some(&body),
                &[],
            )
            .await?;
            return Ok(match f {
                Fmt::Json => Done::Json(json!({ "id": raw_id, "value": value })),
                Fmt::Md => Done::Md(format!("Оценка записана: {value}")),
            });
        }
        _ => {
            return Err(Fail::Invalid(
                "action: supersede, retract, revisit, certify, append или feedback".into(),
            ));
        }
    };
    let r = call_rest(rest, method, &uri, Some(&Value::Object(body)), &extra)
        .await
        .map_err(|e| {
            e.hint("Ревизию записи (rev) даёт memory_read её id или карточка memory_search.")
        })?;
    done(f, &r, |v| match v.get("entry").filter(|e| e.is_object()) {
        // замещение: {entry, superseded}
        Some(new) => format!(
            "Новая {}\nЗамещена {}{}",
            entry_line(new),
            entry_line(&v["superseded"]),
            warnings_md(v)
        ),
        None => format!("Обновлено {}{}", entry_line(v), warnings_md(v)),
    })
}

async fn link(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    if let Some(edge) = opt_str(args, "remove")? {
        call_rest(
            rest,
            Method::DELETE,
            &format!("/api/v1/edges/{}", enc(edge.trim())),
            None,
            &[],
        )
        .await?;
        return Ok(match f {
            Fmt::Json => Done::Json(json!({ "removed": edge.trim() })),
            Fmt::Md => Done::Md(format!("Связь {} удалена", edge.trim())),
        });
    }
    let (src, kind, dst) = (
        graph_ref(req_str(args, "src")?),
        req_str(args, "type")?,
        graph_ref(req_str(args, "dst")?),
    );
    let body = json!({ "src": src, "type": kind, "dst": dst });
    let r = call_rest(rest, Method::POST, "/api/v1/edges", Some(&body), &[]).await?;
    done(f, &r, |v| {
        format!(
            "Связь {}: {src} —{kind}→ {dst}",
            v["id"].as_str().unwrap_or("?")
        )
    })
}

async fn recall(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    req_str(args, "event")?;
    // `session_id` не передан — REST возьмёт `X-Zam-Session` исходного запроса
    let body = pick(
        args,
        &[
            "event",
            "prompt",
            "error",
            "tool",
            "session_id",
            "budget_tokens",
            "agent_block_version",
            "source",
        ],
    );
    let r = call_rest(
        rest,
        Method::POST,
        "/api/v1/recall",
        Some(&Value::Object(body)),
        &[],
    )
    .await?;
    done(f, &r, |v| {
        let text = v["text"].as_str().unwrap_or("").trim_end();
        let mut s = if text.is_empty() {
            "Подсказок нет.".to_string()
        } else {
            text.to_string()
        };
        s.push_str(&warnings_md(v));
        s
    })
}

/// Пространство Vault: `global` → `knowledge`, `solution` → решение из `X-Zam-Solution`,
/// иначе — как есть (GUID решения); не задано — по умолчанию. Второе — задано ли явно.
fn vault_space(rest: &Rest, args: &JsonObject) -> Result<(String, bool), Fail> {
    Ok(match opt_str(args, "scope")?.map(str::trim) {
        None | Some("") => (default_space(rest), false),
        Some("global" | "knowledge") => ("knowledge".into(), true),
        Some("solution") => (
            rest.header(crate::api::search::SOLUTION_HEADER)
                .ok_or_else(|| {
                    Fail::Invalid("scope solution: нет заголовка X-Zam-Solution".into())
                })?
                .to_string(),
            true,
        ),
        Some(s) => (s.to_string(), true),
    })
}

/// Секреты пространства с именем `name` без учёта регистра (`GET /spaces/{space}/vault?q=`
/// ищет подстроку без учёта регистра; равенство проверяется здесь).
async fn vault_named(rest: &Rest, space: &str, name: &str) -> Result<Vec<Value>, Fail> {
    let uri = format!("/api/v1/spaces/{}/vault?q={}", enc(space), enc(name));
    let r = call_rest(rest, Method::GET, &uri, None, &[]).await?;
    let lower = name.to_lowercase();
    Ok(r.json()["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|m| {
                    m["title"]
                        .as_str()
                        .is_some_and(|t| t.to_lowercase() == lower)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default())
}

/// Секрет с точно таким именем (регистр важен: уникальный индекс имени его учитывает).
fn exact<'a>(items: &'a [Value], name: &str) -> Option<&'a Value> {
    items.iter().find(|m| m["title"].as_str() == Some(name))
}

/// Пространства поиска `get` по имени: явный `scope` — одно; иначе решение из
/// `X-Zam-Solution` (если есть), затем личный Vault.
fn vault_candidates(rest: &Rest, args: &JsonObject) -> Result<Vec<String>, Fail> {
    let (space, explicit) = vault_space(rest, args)?;
    let mut out = vec![space];
    if !explicit && out[0] != "knowledge" {
        out.push("knowledge".into());
    }
    Ok(out)
}

/// Секрет для `get`: по `id`; по `name` — точное совпадение в пространствах по порядку, иначе
/// единственное совпадение без учёта регистра. Несколько таких — 409 со списком, ни одного —
/// 404 с пространствами поиска.
async fn vault_locate(rest: &Rest, args: &JsonObject) -> Result<Value, Fail> {
    if let Some(id) = opt_str(args, "id")? {
        return Ok(json!({ "id": id.trim() }));
    }
    let name = req_str(args, "name")?.trim();
    let spaces = vault_candidates(rest, args)?;
    let mut found = Vec::new();
    for sp in &spaces {
        found.push(vault_named(rest, sp, name).await?);
    }
    if let Some(m) = found.iter().find_map(|items| exact(items, name)) {
        return Ok(m.clone());
    }
    let mut loose: Vec<Value> = found.into_iter().flatten().collect();
    match loose.len() {
        1 => Ok(loose.remove(0)),
        0 => Err(Fail::Problem {
            status: StatusCode::NOT_FOUND,
            detail: format!("секрет «{name}» не найден (искали: {})", spaces.join(", ")),
        }),
        _ => {
            let list: Vec<String> = loose.iter().map(vault_meta_line).collect();
            Err(Fail::Problem {
                status: StatusCode::CONFLICT,
                detail: format!(
                    "имени «{name}» без учёта регистра соответствуют несколько секретов —                      укажите точное имя или id:
{}",
                    list.join("
")
                ),
            })
        }
    }
}

fn vault_meta_line(m: &Value) -> String {
    format!(
        "- {} «{}» · {} · {} · rev{}",
        m["id"].as_str().unwrap_or("?"),
        m["title"].as_str().unwrap_or(""),
        m["data"]["type"].as_str().unwrap_or("?"),
        m["space"].as_str().unwrap_or("?"),
        m["rev"]
    )
}

fn vault_locked_md(v: &Value) -> String {
    if v["locked"].as_bool() == Some(true) {
        format!(
            "\n> Vault заблокирован: {}",
            v["reason"].as_str().unwrap_or("мастер-ключ недоступен")
        )
    } else {
        String::new()
    }
}

async fn vault(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    match req_str(args, "action")? {
        "list" => {
            let (space, _) = vault_space(rest, args)?;
            let mut uri = format!("/api/v1/spaces/{}/vault", enc(&space));
            if let Some(q) = opt_str(args, "name")? {
                uri.push_str(&format!("?q={}", enc(q)));
            }
            let r = call_rest(rest, Method::GET, &uri, None, &[]).await?;
            done(f, &r, |v| {
                let items = v["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                let mut s = format!("Vault {space}: секретов {}", items.len());
                for m in items {
                    s.push('\n');
                    s.push_str(&vault_meta_line(m));
                }
                s.push_str(&vault_locked_md(v));
                s
            })
        }
        "get" => {
            let m = vault_locate(rest, args).await?;
            let id = m["id"].as_str().unwrap_or("").to_string();
            // раскрытие — `?reveal=1`: аудит `vault.reveal` с сессией и машиной делает REST
            let r = call_rest(
                rest,
                Method::GET,
                &format!("/api/v1/vault/{}?reveal=1", enc(&id)),
                None,
                &[],
            )
            .await?;
            done(f, &r, |v| {
                let mut s = format!(
                    "«{}» ({id}, {})",
                    v["title"].as_str().unwrap_or(""),
                    v["data"]["type"].as_str().unwrap_or("?")
                );
                for k in ["username", "url"] {
                    if let Some(x) = v["data"][k].as_str() {
                        s.push_str(&format!("\n{k}: {x}"));
                    }
                }
                s.push_str(&format!("\nvalue: {}", v["secret"].as_str().unwrap_or("")));
                s
            })
        }
        "set" => {
            let value =
                opt_str(args, "value")?.ok_or_else(|| Fail::Invalid("set: нужен value".into()))?;
            let name = req_str(args, "name")?.trim().to_string();
            let (space, _) = vault_space(rest, args)?;
            // только точное имя: DB_PASS и db_pass — разные секреты
            let same = vault_named(rest, &space, &name).await?;
            let r = match exact(&same, &name) {
                Some(m) => {
                    let id = m["id"].as_str().unwrap_or("").to_string();
                    let body = json!({ "secret": value });
                    call_rest(
                        rest,
                        Method::PATCH,
                        &format!("/api/v1/vault/{}", enc(&id)),
                        Some(&body),
                        &[],
                    )
                    .await?
                }
                None => {
                    let kind = opt_str(args, "type")?.unwrap_or("password");
                    let body = json!({ "title": name, "data": { "type": kind }, "secret": value });
                    call_rest(
                        rest,
                        Method::POST,
                        &format!("/api/v1/spaces/{}/vault", enc(&space)),
                        Some(&body),
                        &[],
                    )
                    .await?
                }
            };
            done(f, &r, |v| format!("Сохранено\n{}", vault_meta_line(v)))
        }
        _ => Err(Fail::Invalid("action: get, set или list".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_short_and_names_unique() {
        let defs = definitions();
        assert_eq!(defs.len(), 10);
        for t in defs {
            let d = t.description.as_deref().unwrap();
            assert!(
                d.chars().count() <= 300,
                "{}: {}",
                t.name,
                d.chars().count()
            );
            assert_eq!(
                ToolName::parse(&t.name).map(ToolName::as_str),
                Some(&*t.name)
            );
        }
    }

    #[test]
    fn tokens_line_appended_once() {
        let s = with_tokens("### n:1 a\nтекст\n~12 tok\n".into());
        assert!(s.ends_with("~12 tok\n"), "{s}");
        let s = with_tokens("Ничего не найдено.".into());
        let last = s.rsplit('\n').next().unwrap();
        assert!(last.starts_with('~') && last.ends_with(" tok"), "{s}");
    }

    #[test]
    fn refs_normalized() {
        assert_eq!(entry_id("i:01ABC"), "01ABC");
        assert_eq!(entry_id(" 01ABC "), "01ABC");
        assert_eq!(graph_ref("i:01ABC"), "e:01ABC");
        assert_eq!(graph_ref("n:01ABC"), "n:01ABC");
        assert_eq!(graph_ref("[[Имя]]"), "[[Имя]]");
        assert_eq!(
            enc("Папка/Заметка 1"),
            "%D0%9F%D0%B0%D0%BF%D0%BA%D0%B0%2F%D0%97%D0%B0%D0%BC%D0%B5%D1%82%D0%BA%D0%B0%201"
        );
    }
}
