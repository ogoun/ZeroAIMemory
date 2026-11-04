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
//! - **Ссылки** `i:|c:|r:|t:|k:|q:|e:<id>` (как в карточках поиска) принимаются везде, где REST
//!   ждёт id записи; в `memory_link` они приводятся к `e:<id>` маршрута `/edges`.
use std::sync::{Arc, LazyLock};

use axum::http::{HeaderName, Method, StatusCode, header};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations};
use serde_json::{Map, Value, json};
use zam_store::count_tokens;

use super::{Reply, Rest};

/// Предел диффа конфликта ревизии в тексте ошибки, символов.
const DIFF_MAX_CHARS: usize = 4000;
/// Что делать с отчётом, который не прошёл через MCP по размеру или сроку.
pub const REPORT_CLI_HINT: &str = "Большой отчёт — используйте `zam report <файл.md>`: \
                                   картинки рядом с файлом, загрузка потоком без base64.";

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
    Code,
}

impl ToolName {
    pub const ALL: [ToolName; 11] = [
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
        ToolName::Code,
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
            ToolName::Code => "memory_code",
        }
    }

    pub fn parse(s: &str) -> Option<ToolName> {
        ToolName::ALL.into_iter().find(|t| t.as_str() == s)
    }

    fn description(self) -> &'static str {
        match self {
            ToolName::Search => {
                "Поиск в памяти (Knowledge, Rules, решение): сначала выжимки кэша, затем \
                 карточки — id, путь, ревизия, возраст, ~токены, seen (уже выдано); вложение — \
                 a:<id>, файл, locator. filters: kinds (и term, task, question, attachment), \
                 insight_kinds, file — знание о файле, decided_by. Дальше — memory_read."
            }
            ToolName::Read => {
                "Чтение пачкой до 20 целей: n:<id>, i:|c:|r:|t:|k:|q:<id>, путь <пространство>:/<путь>; \
                 вложение — a:<id> или b:<sha256>, locator page:N|slide:N|para:N|sheet:Лист!A1:F40, \
                 без него — оглавление. tier, section, max_tokens; if_rev — unchanged; since_rev — \
                 дифф. Ошибка — в элементе."
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
                 ошибка 409 с диффом: перечитать и повторить. Ссылки на файлы — refs: [пути] во \
                 frontmatter."
            }
            ToolName::Log => {
                "Запись журнала: kind changelog|release, insight (data.kind, triggers, scope, \
                 certainty), term (data.definition, aliases), task, idea, question (data.options); \
                 refs.files — файлы. report — отчёт пользователю, неизменяем: images [{name, \
                 data base64}], большой — zam report. Дубль insight — 409: force."
            }
            ToolName::UpdateEntry => {
                "Правка записи: action supersede|retract|revisit|certify|append|feedback|status|\
                 claim|release|answer|propose|decide; rev — ревизия (кроме feedback, claim, \
                 release, answer). certify — certainty, reason; feedback — value; задача: status, \
                 claim, release; вопрос: answer (by обязателен), status obsolete."
            }
            ToolName::Link => {
                "Связь графа: src, type, dst (n:<id>, i:<id>; dst также [[Имя]]) — создать явную \
                 связь; type contradicts — запись возражает axiom/theorem dst; remove: <edge_id> — \
                 удалить связь."
            }
            ToolName::Recall => {
                "Подсказки памяти к событию: session_start — дайджест, prompt — к запросу \
                 пользователя, post_tool — к ошибке инструмента (error, tool), recovery — после \
                 сжатия контекста, brief — пакет под задачу task (задание субагенту, первый \
                 вызов субагента). Повторы в сессии отсекаются, кроме brief и recovery."
            }
            ToolName::Vault => {
                "Vault: list — записи без значений (name — поиск), get — по name или id (v:<id> из \
                 дайджеста; раскрытие пишется в аудит), set — создать или обновить name = value. \
                 Типы host и environment — окружение: purpose, address, limits. scope: global \
                 (личный) или solution (из X-Zam-Solution)."
            }
            ToolName::Code => {
                "Карта кода решения, перед правкой незнакомого кода: map — ключевые файлы и \
                 символы с сигнатурами в бюджете (focus — пути, имена, текст задачи); symbol — \
                 карточка по имени или путь#qual: определение, кто ссылается, знания; refs — кто \
                 ссылается; impact — что затронет правка. Тела кода не хранятся."
            }
        }
    }

    /// Только чтение памяти (подсказка клиенту; `memory_recall` пишет лишь учёт показов).
    fn read_only(self) -> bool {
        matches!(
            self,
            ToolName::Search | ToolName::Read | ToolName::Tree | ToolName::Recall | ToolName::Code
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
                    "filters": {"type": "object", "description": "kinds, status, certainty, insight_kinds (виды insight: gotcha, failure, experiment…), decided_by, file (путь файла)"},
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
                            "since_rev": {"type": "integer", "minimum": 1,
                                          "description": "дифф с этой ревизии"},
                            "locator": {"type": "string",
                                        "description": "часть вложения: page:12, sheet:Итоги!A1:F40"},
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
                    "kind": {"type": "string", "enum": ["changelog", "insight", "release", "term", "task", "idea", "question", "report"]},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "data": {"type": "object", "description": "insight: kind, experiment, evidence; term: definition (до 300 символов), aliases, not_to_confuse, code; question: options [{key, text}] до 8, recommendation (ключ); report: refs {commits, files, notes, tasks}; idea: origin обязателен — human (записана по словам человека) | agent (своя мысль: не предлагается, человек принимает её в WebUI), priority low|normal|high, horizon YYYY-MM-DD («не раньше»), links"},
                    "images": {"type": "array", "description": "report: файлы по ссылкам тела ![…](figs/a.png); name — путь из ссылки, data — base64",
                        "items": {"type": "object", "properties": {
                            "name": {"type": "string"}, "data": {"type": "string"}},
                            "required": ["name", "data"]}},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "certainty": {"type": "string", "enum": ["axiom", "theorem", "hypothesis"]},
                    "triggers": {"type": "array", "items": {"type": "object"}},
                    "links": {"type": "array", "items": {"type": "object"}},
                    "refs": {"type": "object", "description": "ссылки на файлы {files: [{path, hash?, commit?}]}, path от корня решения"},
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
                    "rev": {"type": "integer",
                        "description": "ревизия записи; не нужна у feedback, claim, release, answer, propose, decide"},
                    "action": {"type": "string", "enum": ["supersede", "retract", "revisit",
                        "certify", "append", "feedback", "status", "claim", "release", "answer",
                        "propose", "decide"]},
                    "status": {"type": "string", "description": "задача: open, in_progress, blocked, done, dropped"},
                    "next_step": {"type": ["string", "null"]},
                    "blocked_by": {"type": ["string", "null"]},
                    "ttl_min": {"type": "integer", "minimum": 1, "maximum": 1440},
                    "agent": {"type": "string"},
                    "option": {"type": "string", "description": "answer: ключ варианта вопроса (option, text или оба: text — свободный ответ); у вопроса-предложения идеи обязателен: accept (начать), defer (отложить, с horizon), decline (отказаться)"},
                    "text": {"type": "string", "description": "answer: текст ответа; propose: почему идея подходит сейчас (до 2000 символов)"},
                    "channel": {"type": "string", "enum": ["chat", "cli"],
                        "description": "answer, decide: где человек ответил (по умолчанию chat)"},
                    "by": {"type": "string", "enum": ["human", "agent"],
                        "description": "answer, decide: обязателен — human: слова человека (axiom), agent: решение агента (hypothesis; у идеи — только decline своей)"},
                    "decision": {"type": "string", "enum": ["accept", "defer", "decline", "reopen"],
                        "description": "decide: решение человека по идее w: (by обязателен; без ответа «начать» не начинай); accept создаёт задачу k:, reopen — с reason"},
                    "horizon": {"type": "string", "description": "decide defer и answer defer вопроса-предложения: «не раньше» YYYY-MM-DD (UTC, позже сегодня; по умолчанию +30 дней)"},
                    "space": {"type": "string", "description": "propose, decide accept: пространство будущей задачи — GUID решения или knowledge; по умолчанию — решение запроса"},
                    "title": {"type": "string"},
                    "body": {"type": "string", "description": "supersede — тело; append — дописать"},
                    "data": {"type": "object"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "triggers": {"type": "array", "items": {"type": "object"}},
                    "links": {"type": "array", "items": {"type": "object"}},
                    "refs": {"type": "object", "description": "supersede и append: новый набор ссылок на файлы {files: [...]}"},
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
                    "type": {"type": "string", "description": "links_to, relates_to, depends_on, \
                        part_of, caused_by, fixes, derived_from, mentions, contradicts"},
                    "dst": {"type": "string"},
                    "remove": {"type": "string", "description": "id связи"},
                    "format": fmt,
                }),
                &[],
            ),
            ToolName::Recall => (
                json!({
                    "event": {"type": "string", "enum": ["session_start", "prompt", "post_tool", "recovery", "brief"]},
                    "prompt": {"type": "string"},
                    "task": {"type": "string",
                        "description": "brief: задача субагента, до 2000 символов"},
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
                        "set: тип записи (password по умолчанию; host, environment — окружение, \
                         value необязателен)"},
                    "purpose": {"type": "string", "description": "set: назначение, до 500"},
                    "address": {"type": "string", "description": "set: адрес, до 500"},
                    "limits": {"type": "string", "description": "set: ограничения, до 1000"},
                    "notes": {"type": "string", "description": "set: заметки, до 2000"},
                    "scope": {"type": "string", "description":
                        "global, solution или GUID решения"},
                    "format": fmt,
                }),
                &["action"],
            ),
            ToolName::Code => (
                json!({
                    "action": {"type": "string", "enum": ["map", "symbol", "refs", "impact"]},
                    "query": {"type": "string",
                        "description": "symbol, refs: имя, qual или адрес путь#qual"},
                    "path": {"type": "string", "description":
                        "map — область (поддерево), symbol и refs — файл символа, impact — путь без хэшей символов (все знания о нём)"},
                    "focus": {"type": "object", "description": "map: файлы и имена задачи",
                        "properties": {
                            "paths": {"type": "array", "items": {"type": "string"}},
                            "idents": {"type": "array", "items": {"type": "string"}},
                            "text": {"type": "string", "description": "текст задачи, до 2000 символов"},
                        }},
                    "budget_tokens": {"type": "integer", "minimum": 1, "maximum": 20000,
                        "description": "map: бюджет карты (по умолчанию code.map_tokens)"},
                    "files": {"type": "array", "description":
                        "impact: изменённые файлы; symbols — qual и body_hash рабочей копии (без них — знания о пути)",
                        "items": {"type": "object", "properties": {
                            "path": {"type": "string"},
                            "symbols": {"type": "array", "items": {"type": "object",
                                "properties": {"qual": {"type": "string"},
                                               "body_hash": {"type": "string"}},
                                "required": ["qual", "body_hash"]}},
                        }, "required": ["path"]}},
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
pub async fn call(tool: ToolName, rest: &Rest, mut args: JsonObject) -> Output {
    let r = match tool {
        ToolName::Search => search(rest, &args).await,
        ToolName::Read => read(rest, &args).await,
        ToolName::CachePut => cache_put(rest, &args).await,
        ToolName::Tree => tree(rest, &args).await,
        ToolName::Write => write(rest, &args).await,
        ToolName::Log => log(rest, &mut args).await,
        ToolName::UpdateEntry => update_entry(rest, &args).await,
        ToolName::Link => link(rest, &args).await,
        ToolName::Recall => recall(rest, &args).await,
        ToolName::Vault => vault(rest, &args).await,
        ToolName::Code => code(rest, &args).await,
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

/// Id записи журнала без префикса ссылки `i:|c:|r:|t:|k:|q:|p:|e:` (или `entry:`).
fn entry_id(raw: &str) -> &str {
    let t = raw.trim();
    match t.split_once(':') {
        Some((p, id)) if p == "entry" || zam_store::is_entry_ref_letter(p) => id,
        _ => t,
    }
}

/// Ссылка графа: `i:|c:|r:|t:|k:|q:|p:<id>` → `e:<id>`, остальное — как есть (отчёт `p:` токену
/// — 404 в REST, как несуществующая запись).
fn graph_ref(raw: &str) -> String {
    let t = raw.trim();
    match t.split_once(':') {
        Some((p, id)) if zam_store::EntryKind::from_ref_letter(p).is_some() => format!("e:{id}"),
        _ => t.to_string(),
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

/// Итог решения по идее (`decide`, ответ на вопрос-предложение): `Идея w:<id>: <статус>`, при
/// «начать» — ` → задача k:<id>`.
fn idea_outcome(v: &Value) -> String {
    let idea = if v["idea"].is_object() {
        &v["idea"]
    } else {
        &v["entry"]
    };
    let mut s = format!(
        "Идея w:{}: {}",
        idea["id"].as_str().unwrap_or("?"),
        idea["status"].as_str().unwrap_or("?")
    );
    if let Some(t) = v["task"]["id"].as_str() {
        s.push_str(&format!(" → задача k:{t}"));
    }
    s
}

/// Строка записи журнала: `i:<id> insight/<вид> «заголовок» rev<N> · status · certainty`.
fn entry_line(e: &Value) -> String {
    let kind = e["kind"].as_str().unwrap_or("entry");
    let prefix = zam_store::EntryKind::parse(kind).map_or("e", zam_store::EntryKind::ref_letter);
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

async fn log(rest: &Rest, args: &mut JsonObject) -> Res {
    let f = fmt(args)?;
    let kind = req_str(args, "kind")?;
    if zam_store::EntryKind::parse(kind).is_none() {
        return Err(Fail::Invalid(
            "kind: changelog, insight, release, term, task, idea, question или report".into(),
        ));
    }
    req_str(args, "title")?;
    let report = kind == "report";
    let space = opt_str(args, "space")?
        .map(str::to_string)
        .unwrap_or_else(|| default_space(rest));
    let keys = [
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
        "refs",
        "scope",
        "force",
        "supersedes_id",
    ];
    let mut body = pick(args, &keys);
    if report {
        // у отчёта прочие поля REST отклонит 400 (неизвестное поле) — агент увидит причину;
        // `images` (base64 до `reports.mcp_max_bytes`) переносится без копии
        if let Some(images) = args.remove("images").filter(|v| !v.is_null()) {
            body.insert("images".into(), images);
        }
    }
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
    .await
    .map_err(|e| if report { report_hint(e) } else { e })?;
    if report {
        // отчёт — для человека: агенту только ссылка (тела и `p:` нет, спецификация §7)
        return done(f, &r, |v| {
            format!("Отчёт записан: {}", v["url"].as_str().unwrap_or("?"))
        });
    }
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

/// Подсказка к отказу MCP-отчёта по размеру (413) или сроку чтения тела (408): большой отчёт
/// идёт через `zam report` (multipart потоком, без base64 и предела `reports.mcp_max_bytes`).
fn report_hint(e: Fail) -> Fail {
    match &e {
        Fail::Rest { reply, .. }
            if matches!(
                reply.status,
                StatusCode::PAYLOAD_TOO_LARGE | StatusCode::REQUEST_TIMEOUT
            ) =>
        {
            e.hint(REPORT_CLI_HINT)
        }
        _ => e,
    }
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
                    "refs",
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
            let mut b = pick(args, &["message", "refs"]);
            b.insert("append_body".into(), json!(text));
            (Method::PATCH, entry, b)
        }
        "status" => {
            let status = req_str(args, "status")?;
            let mut b = pick(args, &["reason", "message"]);
            b.insert("status".into(), json!(status));
            // null снимает поле, поэтому — по наличию ключа, а не через pick
            for k in ["next_step", "blocked_by"] {
                if let Some(v) = args.get(k) {
                    b.insert(k.into(), v.clone());
                }
            }
            (Method::PATCH, entry, b)
        }
        "answer" => {
            // `by` обязателен (финальное ревью M4.5, I-2): умолчания нет — чей ответ, знает агент
            if !args.get("by").is_some_and(Value::is_string) {
                return Err(Fail::Invalid(
                    "answer: укажите by: human — ответ человека, by: agent — решение агента".into(),
                ));
            }
            let mut b = pick(
                args,
                &["text", "option", "horizon", "channel", "by", "message"],
            );
            b.entry("channel").or_insert_with(|| json!("chat"));
            let r = call_rest(
                rest,
                Method::POST,
                &format!("{entry}/answer"),
                Some(&Value::Object(b)),
                &[],
            )
            .await?;
            return done(f, &r, |v| {
                let mut s = format!(
                    "Ответ записан {}\nРешение {}",
                    entry_line(&v["entry"]),
                    entry_line(&v["decision"])
                );
                // ответ на вопрос-предложение: переход идеи (и задача при «начать»)
                if v["idea"].is_object() {
                    s.push('\n');
                    s.push_str(&idea_outcome(v));
                }
                s
            });
        }
        "propose" => {
            let b = pick(args, &["text", "space", "message"]);
            let r = call_rest(
                rest,
                Method::POST,
                &format!("{entry}/propose"),
                Some(&Value::Object(b)),
                &[],
            )
            .await?;
            return done(f, &r, |v| {
                format!(
                    "Предложено w:{} → вопрос q:{}\nСпроси человека: начать, отложить или \
                     отказаться; без ответа «начать» не начинай.",
                    v["entry"]["id"].as_str().unwrap_or("?"),
                    v["question"]["id"].as_str().unwrap_or("?")
                )
            });
        }
        "decide" => {
            // как у answer: решение по идее — слова человека или агента, умолчания нет
            if !args.get("by").is_some_and(Value::is_string) {
                return Err(Fail::Invalid(
                    "decide: укажите by: human — решение человека (его слова); by: agent — \
                     только decline своей идеи"
                        .into(),
                ));
            }
            req_str(args, "decision")?;
            let mut b = pick(
                args,
                &[
                    "decision", "horizon", "reason", "by", "channel", "space", "message",
                ],
            );
            b.entry("channel").or_insert_with(|| json!("chat"));
            let r = call_rest(
                rest,
                Method::POST,
                &format!("{entry}/decide"),
                Some(&Value::Object(b)),
                &[],
            )
            .await?;
            return done(f, &r, idea_outcome);
        }
        "claim" => (
            Method::POST,
            format!("{entry}/claim"),
            pick(args, &["ttl_min", "agent"]),
        ),
        "release" => (Method::POST, format!("{entry}/release"), Map::new()),
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
                "action: supersede, retract, revisit, certify, append, feedback, status, claim, \
                 release, answer, propose или decide"
                    .into(),
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
            "task",
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
        // ссылка дайджеста `v:<id>` (раздел «Окружение») — как голый id
        let id = id.trim();
        return Ok(json!({ "id": id.strip_prefix("v:").unwrap_or(id) }));
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
                    "имени «{name}» без учёта регистра соответствуют несколько секретов — \
                     укажите точное имя или id:\n{}",
                    list.join("\n")
                ),
            })
        }
    }
}

/// Открытые поля окружения из аргументов `set` (`purpose`, `address`, `limits`, `notes`):
/// только заданные; пределы проверяет REST.
fn vault_env_fields(args: &JsonObject) -> Result<serde_json::Map<String, Value>, Fail> {
    let mut out = serde_json::Map::new();
    for k in VAULT_ENV_FIELDS {
        if let Some(v) = opt_str(args, k)? {
            out.insert(k.into(), json!(v));
        }
    }
    Ok(out)
}

/// Открытые поля окружения, которые принимает `memory_vault set`.
const VAULT_ENV_FIELDS: [&str; 4] = ["purpose", "address", "limits", "notes"];

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
                for k in zam_store::VAULT_OPEN_FIELDS {
                    if let Some(x) = v["data"][k].as_str() {
                        s.push_str(&format!("\n{k}: {x}"));
                    }
                }
                // у записей окружения значение необязательно: нет — строки `value` нет
                if let Some(x) = v["secret"].as_str() {
                    s.push_str(&format!("\nvalue: {x}"));
                }
                s
            })
        }
        "set" => {
            let value = opt_str(args, "value")?;
            let name = req_str(args, "name")?.trim().to_string();
            let kind = opt_str(args, "type")?;
            let fields = vault_env_fields(args)?;
            let (space, _) = vault_space(rest, args)?;
            // только точное имя: DB_PASS и db_pass — разные секреты
            let same = vault_named(rest, &space, &name).await?;
            let r = match exact(&same, &name) {
                Some(m) => {
                    let id = m["id"].as_str().unwrap_or("").to_string();
                    let mut body = json!({});
                    if let Some(v) = value {
                        body["secret"] = json!(v);
                    }
                    // PATCH заменяет `data` целиком: заданные поля сливаются с прежними
                    if kind.is_some() || !fields.is_empty() {
                        let mut data = m["data"].as_object().cloned().unwrap_or_default();
                        if let Some(k) = kind {
                            data.insert("type".into(), json!(k));
                        }
                        data.extend(fields);
                        body["data"] = Value::Object(data);
                    }
                    if body.as_object().is_some_and(|o| o.is_empty()) {
                        return Err(Fail::Invalid(
                            "set: нечего менять — нужен value или открытые поля".into(),
                        ));
                    }
                    // `data` слита с прочитанной ревизией: правка с `If-Match`, иначе чужая
                    // правка между чтением и записью молча потерялась бы
                    let guard: Vec<(HeaderName, String)> =
                        m["rev"].as_i64().map(if_match).into_iter().collect();
                    call_rest(
                        rest,
                        Method::PATCH,
                        &format!("/api/v1/vault/{}", enc(&id)),
                        Some(&body),
                        &guard,
                    )
                    .await
                    .map_err(|e| match e {
                        Fail::Rest { ref reply, .. } if reply.status == StatusCode::CONFLICT => e
                            .hint(
                                "Запись изменилась, пока шла правка: повторите memory_vault set.",
                            ),
                        e => e,
                    })?
                }
                None => {
                    let kind = kind.unwrap_or("password");
                    if value.is_none() && !zam_store::VAULT_ENV_TYPES.contains(&kind) {
                        return Err(Fail::Invalid(
                            "set: нужен value (без значения — только type host или environment)"
                                .into(),
                        ));
                    }
                    let mut data = serde_json::Map::new();
                    data.insert("type".into(), json!(kind));
                    data.extend(fields);
                    let mut body = json!({ "title": name, "data": data });
                    if let Some(v) = value {
                        body["secret"] = json!(v);
                    }
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

// ---- memory_code (M5.6 §9) ----

/// Символов в списке кандидатов `symbol`/`refs` (поиск `GET /code/symbols`).
const CODE_SYMBOLS_LIST: usize = 20;
/// Строк списков Markdown (кто ссылается, знания, изменённые, потребители) не больше; остаток —
/// «… ещё N», полный список — `format: json`.
const CODE_LIST_MD: usize = 20;
/// «На что ссылается файл» в карточке Markdown — не больше.
const CODE_USES_MD: usize = 15;

/// `memory_code`: карта, карточка символа, «кто ссылается», влияние правки — поверх
/// `GET /code/map`, `GET /code/symbols[/{путь#qual}]`, `POST /code/impact` (права — маршрутов).
async fn code(rest: &Rest, args: &JsonObject) -> Res {
    let f = fmt(args)?;
    match req_str(args, "action")? {
        "map" => code_map(rest, args, f).await,
        "symbol" => code_symbol(rest, args, f, false).await,
        "refs" => code_symbol(rest, args, f, true).await,
        "impact" => code_impact(rest, args, f).await,
        _ => Err(Fail::Invalid("action: map, symbol, refs или impact".into())),
    }
}

/// Непустая строка аргумента без пробелов по краям.
fn opt_text<'a>(args: &'a JsonObject, key: &str) -> Result<Option<&'a str>, Fail> {
    Ok(opt_str(args, key)?.map(str::trim).filter(|s| !s.is_empty()))
}

/// Карта — текст `GET /code/map` как есть (тот же, что у REST и `zam code map`) и
/// предупреждения ответа (`rank pending`, `focus rank timeout`).
async fn code_map(rest: &Rest, args: &JsonObject, f: Fmt) -> Res {
    let mut q = vec![format!("format={}", fmt_str(f))];
    if let Some(b) = opt_int(args, "budget_tokens")? {
        q.push(format!("budget_tokens={b}"));
    }
    match args.get("focus") {
        None | Some(Value::Null) => {}
        Some(v @ Value::Object(_)) => q.push(format!("focus={}", enc(&v.to_string()))),
        Some(_) => {
            return Err(Fail::Invalid(
                "focus: объект {paths?, idents?, text?}".into(),
            ));
        }
    }
    if let Some(p) = opt_text(args, "path")? {
        q.push(format!("area={}", enc(p)));
    }
    let uri = format!("/api/v1/code/map?{}", q.join("&"));
    let r = call_rest(rest, Method::GET, &uri, None, &[]).await?;
    done(f, &r, |v| {
        let mut s = v["text"].as_str().unwrap_or("").trim_end().to_string();
        s.push_str(&warnings_md(v));
        s
    })
}

/// Адрес символа `путь#qual` строки поиска или карточки.
fn sym_ref(v: &Value) -> String {
    format!(
        "{}#{}",
        v["path"].as_str().unwrap_or("?"),
        v["qual"].as_str().unwrap_or("?")
    )
}

/// Предупреждения `extra` и ответа `v` строками `> warning: …` (сначала свои).
fn with_warnings(mut s: String, v: &Value, extra: &[String]) -> String {
    for w in extra {
        s.push_str(&format!("\n> warning: {w}"));
    }
    s.push_str(&warnings_md(v));
    s
}

/// JSON ответа с предупреждениями `extra` в начале массива `warnings`.
fn json_with_warnings(mut v: Value, extra: &[String]) -> Value {
    if extra.is_empty() {
        return v;
    }
    let mut all: Vec<Value> = extra.iter().map(|w| json!(w)).collect();
    if let Some(a) = v["warnings"].as_array() {
        all.extend(a.iter().cloned());
    }
    if let Value::Object(o) = &mut v {
        o.insert("warnings".into(), Value::Array(all));
    }
    v
}

/// `symbol` и `refs`: адрес `путь#qual` — сразу карточка (`path` тогда не нужен —
/// предупреждение); иначе поиск, одно точное совпадение (`qual` или имя) — карточка с
/// предупреждениями поиска, иначе список адресов. Символа нет в карте (REST
/// [`crate::error::CODE_SYMBOL_NOT_FOUND`]) — не ошибка; прочие 404 (карта выключена, нет
/// решения) — ошибка.
async fn code_symbol(rest: &Rest, args: &JsonObject, f: Fmt, refs_only: bool) -> Res {
    let query = req_str(args, "query")?.trim();
    let path = opt_text(args, "path")?;
    let mut extra: Vec<String> = Vec::new();
    let addr = match zam_code::split_ref(query) {
        (_, Some(_)) => {
            if let Some(p) = path {
                extra.push(format!(
                    "path «{}» не учтён: query — уже адрес путь#qual",
                    zam_store::shown_ref(p)
                ));
            }
            query.to_string()
        }
        (_, None) => {
            let mut uri = format!(
                "/api/v1/code/symbols?q={}&limit={CODE_SYMBOLS_LIST}",
                enc(query)
            );
            if let Some(p) = path {
                uri.push_str(&format!("&path={}", enc(p)));
            }
            let r = call_rest(rest, Method::GET, &uri, None, &[]).await?;
            let v = r.json();
            let hits = v["symbols"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let exact: Vec<&Value> = hits
                .iter()
                .filter(|h| h["qual"] == query || h["name"] == query)
                .collect();
            match exact.as_slice() {
                [one] => {
                    // предупреждения поиска (`rank pending`) — и у карточки
                    extra.extend(
                        v["warnings"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_string),
                    );
                    sym_ref(one)
                }
                _ => return done(f, &r, |v| symbols_md(v, query)),
            }
        }
    };
    let uri = format!("/api/v1/code/symbols/{}", enc(&addr));
    let r = match call_rest(rest, Method::GET, &uri, None, &[]).await {
        Ok(r) => r,
        Err(Fail::Rest { reply, .. })
            if reply.json()["type"] == crate::error::CODE_SYMBOL_NOT_FOUND =>
        {
            return Ok(match f {
                Fmt::Json => Done::Json(json_with_warnings(
                    json!({"symbol": null, "ref": addr, "callers": [], "knowledge": [],
                           "warnings": []}),
                    &extra,
                )),
                Fmt::Md => Done::Md(with_warnings(
                    format!(
                        "Символ «{}» в карте не найден: проверьте адрес путь#qual \
                         (memory_code action=symbol по имени) или обновите карту — \
                         `zam code sync`.",
                        zam_store::shown_ref(&addr)
                    ),
                    &Value::Null,
                    &extra,
                )),
            });
        }
        Err(e) => return Err(e),
    };
    let v = r.json();
    Ok(match (f, refs_only) {
        (Fmt::Json, true) => Done::Json(json_with_warnings(
            json!({
                "symbol": v["symbol"],
                "callers": v["callers"],
                "tokens": count_tokens(&json!([v["symbol"], v["callers"]]).to_string()),
                "warnings": [],
            }),
            &extra,
        )),
        (Fmt::Json, false) => Done::Json(json_with_warnings(v, &extra)),
        (Fmt::Md, true) => Done::Md(with_warnings(callers_md(&v), &Value::Null, &extra)),
        (Fmt::Md, false) => Done::Md(with_warnings(card_md(&v), &Value::Null, &extra)),
    })
}

/// Список кандидатов поиска символа: адрес, вид, строка, сигнатура.
fn symbols_md(v: &Value, query: &str) -> String {
    let hits = v["symbols"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if hits.is_empty() {
        return format!(
            "Символ «{query}» в карте не найден (поиск по имени и qual; карту обновляет \
             `zam code sync`).{}",
            warnings_md(v)
        );
    }
    let mut s = format!(
        "Символы «{query}» — {}: уточните path или адрес путь#qual",
        hits.len()
    );
    for h in hits {
        s.push_str(&format!(
            "\n- `{}` {} стр. {}",
            sym_ref(h),
            h["kind"].as_str().unwrap_or("?"),
            h["line"]
        ));
        if let Some(sig) = h["sig"].as_str().filter(|x| !x.is_empty()) {
            s.push_str(&format!(" · {sig}"));
        }
    }
    s.push_str(&warnings_md(v));
    s
}

/// Элементы через `sep` (до `max`, остаток `rest` — «… ещё N»).
fn capped_list(
    items: &[Value],
    max: usize,
    rest: usize,
    sep: &str,
    line: impl Fn(&Value) -> String,
) -> String {
    let mut parts: Vec<String> = items.iter().take(max).map(line).collect();
    let left = items.len().saturating_sub(max) + rest;
    if left > 0 {
        parts.push(format!("… ещё {left} (полный список — format: json)"));
    }
    parts.join(sep)
}

fn caller_line(c: &Value) -> String {
    format!("{} ×{}", c["path"].as_str().unwrap_or("?"), c["count"])
}

/// «Кто ссылается» карточки: файлы со ссылками на имя символа.
fn callers_md(v: &Value) -> String {
    let sym = &v["symbol"];
    let addr = sym_ref(sym);
    let name = sym["name"].as_str().unwrap_or("?");
    let callers = v["callers"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if callers.is_empty() {
        return format!("`{addr}` — ссылок из других файлов не найдено (по имени `{name}`).");
    }
    // популярное имя — сотни файлов: потолок строк, как у прочих списков (ревью задачи 7)
    format!(
        "`{addr}` — кто ссылается (по имени `{name}`), файлов {}:\n{}",
        callers.len(),
        capped_list(callers, CODE_LIST_MD, 0, "\n", |c| format!(
            "- {}",
            caller_line(c)
        ))
    )
}

/// Знание о файле или символе: ссылка, название, вид, состояние ссылки, адрес.
fn knowledge_line(k: &Value) -> String {
    let path = k["path"].as_str().unwrap_or("?");
    let target = match k["symbol"].as_str() {
        Some(q) => format!("{path}#{q}"),
        None => path.to_string(),
    };
    format!(
        "- {} «{}» {} · {} · `{target}`",
        k["id"].as_str().unwrap_or("?"),
        k["title"].as_str().unwrap_or(""),
        k["kind"].as_str().unwrap_or(""),
        k["state"].as_str().unwrap_or("")
    )
}

/// Знания выдачи — до [`CODE_LIST_MD`] строк, остаток (и `knowledge_left` REST) — «… ещё N».
fn knowledge_md(v: &Value, title: &str) -> String {
    let items = v["knowledge"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if items.is_empty() {
        return String::new();
    }
    let left = v["knowledge_left"].as_u64().unwrap_or(0) as usize;
    format!(
        "\n{title} ({}):\n{}",
        items.len() + left,
        capped_list(items, CODE_LIST_MD, left, "\n", knowledge_line)
    )
}

/// Карточка символа: определение, кто ссылается, на что ссылается файл, знания.
fn card_md(v: &Value) -> String {
    let sym = &v["symbol"];
    let mut s = format!(
        "### `{}` — {}, строки {}–{}",
        sym_ref(sym),
        sym["kind"].as_str().unwrap_or("?"),
        sym["line"],
        sym["end_line"]
    );
    if let Some(sig) = sym["sig"].as_str().filter(|x| !x.is_empty()) {
        s.push_str(&format!("\n`{sig}`"));
    }
    let callers = v["callers"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    s.push_str(&if callers.is_empty() {
        "\nКто ссылается: не найдено (по имени)".to_string()
    } else {
        format!(
            "\nКто ссылается: {}",
            capped_list(callers, CODE_LIST_MD, 0, ", ", caller_line)
        )
    });
    let uses = v["uses"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if !uses.is_empty() {
        s.push_str(&format!(
            "\nФайл ссылается на: {}",
            capped_list(uses, CODE_USES_MD, 0, ", ", |u| format!(
                "{} ({}) ×{}",
                u["name"].as_str().unwrap_or("?"),
                u["kind"].as_str().unwrap_or("?"),
                u["count"]
            ))
        ));
    }
    s.push_str(&knowledge_md(v, "Знания"));
    s
}

/// Влияние правки: `files` (`{path, symbols?}`) или один `path` без хэшей символов — знания о
/// пути (у агента без `zam` хэшей нет, §9); решение — из `X-Zam-Solution`.
async fn code_impact(rest: &Rest, args: &JsonObject, f: Fmt) -> Res {
    let files = match (
        args.get("files").filter(|v| !v.is_null()),
        opt_text(args, "path")?,
    ) {
        (Some(_), Some(_)) => {
            return Err(Fail::Invalid("path и files вместе не передаются".into()));
        }
        (Some(v @ Value::Array(_)), None) => v.clone(),
        (Some(_), None) => {
            return Err(Fail::Invalid("files: массив [{path, symbols?}]".into()));
        }
        (None, Some(p)) => json!([{"path": p}]),
        (None, None) => return Err(Fail::Invalid("impact: нужен path или files".into())),
    };
    let solution = rest
        .header(crate::api::search::SOLUTION_HEADER)
        .ok_or_else(|| Fail::Invalid("impact: нужно решение — заголовок X-Zam-Solution".into()))?;
    let body = json!({"solution": solution, "files": files});
    let r = call_rest(rest, Method::POST, "/api/v1/code/impact", Some(&body), &[]).await?;
    done(f, &r, impact_md)
}

fn impact_sym(x: &Value) -> String {
    format!(
        "`{}` ({}, стр. {})",
        sym_ref(x),
        x["kind"].as_str().unwrap_or("?"),
        x["line"]
    )
}

fn consumer_line(c: &Value) -> String {
    let names: Vec<&str> = c["names"]
        .as_array()
        .map(|n| n.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    format!(
        "{} → {} ×{}",
        c["path"].as_str().unwrap_or("?"),
        names.join(", "),
        c["count"]
    )
}

/// Итог влияния: изменённые и удалённые символы, потребители на 1 шаг, знания — каждый список
/// до [`CODE_LIST_MD`] строк (полный — `format: json`).
fn impact_md(v: &Value) -> String {
    let arr = |k: &str| v[k].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let (changed, removed, consumers, paths) = (
        arr("changed"),
        arr("removed"),
        arr("consumers"),
        arr("paths_only"),
    );
    let knowledge = arr("knowledge");
    if changed.is_empty() && removed.is_empty() && consumers.is_empty() && knowledge.is_empty() {
        return format!(
            "Правка не затрагивает символов карты и знаний памяти.{}",
            warnings_md(v)
        );
    }
    let mut s = format!(
        "Влияние правки: изменено символов {}, удалено {}, файлов-потребителей {}, знаний {}",
        changed.len(),
        removed.len(),
        consumers.len(),
        knowledge.len() + v["knowledge_left"].as_u64().unwrap_or(0) as usize
    );
    if !changed.is_empty() {
        s.push_str(&format!(
            "\nИзменены: {}",
            capped_list(changed, CODE_LIST_MD, 0, ", ", impact_sym)
        ));
    }
    if !removed.is_empty() {
        s.push_str(&format!(
            "\nУдалены: {}",
            capped_list(removed, CODE_LIST_MD, 0, ", ", impact_sym)
        ));
    }
    if !consumers.is_empty() {
        s.push_str(&format!(
            "\nПотребители (1 шаг): {}",
            capped_list(consumers, CODE_LIST_MD, 0, "; ", consumer_line)
        ));
    }
    if !paths.is_empty() {
        s.push_str(&format!(
            "\nБез хэшей символов (все знания о пути): {}",
            capped_list(paths, CODE_LIST_MD, 0, ", ", |p| p
                .as_str()
                .unwrap_or("?")
                .to_string())
        ));
    }
    s.push_str(&knowledge_md(v, "Знания — перепроверить"));
    s.push_str(&warnings_md(v));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_short_and_names_unique() {
        let defs = definitions();
        assert_eq!(defs.len(), 11);
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

    /// Буквы ссылок M4.5 (`t:` term, `k:` task, `q:` question) — как `i:|c:|r:|e:`
    /// (спецификация M4.5 §1.3); строка записи печатает букву своего вида.
    #[test]
    fn entry_ref_letters() {
        for p in ["i", "c", "r", "t", "k", "q", "p", "w", "e", "entry"] {
            assert_eq!(entry_id(&format!("{p}:01ABC")), "01ABC", "{p}");
        }
        for p in ["i", "c", "r", "t", "k", "q", "p", "w"] {
            assert_eq!(graph_ref(&format!("{p}:01ABC")), "e:01ABC", "{p}");
        }
        assert_eq!(graph_ref("e:01ABC"), "e:01ABC");
        assert_eq!(graph_ref("knowledge:/a"), "knowledge:/a");
        for (kind, letter) in [
            ("term", "t"),
            ("task", "k"),
            ("question", "q"),
            ("insight", "i"),
            ("report", "p"),
            ("idea", "w"),
        ] {
            let line = entry_line(&json!({"id": "01ABC", "kind": kind, "title": "x", "rev": 1}));
            assert!(
                line.starts_with(&format!("{letter}:01ABC {kind}")),
                "{line}"
            );
        }
        assert!(entry_line(&json!({"id": "01ABC", "kind": "?", "rev": 1})).starts_with("e:01ABC"));
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
