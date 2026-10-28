//! Чтение пачкой `POST /read` (§9.3 шаг 2, §12; задача 6 M4).
//!
//! ```text
//! POST /read {items: [{id, tier?, section?, locator?, max_tokens?, if_rev?}],
//!             session_id?, format?: "json"|"md", budget_tokens?}
//! ```
//!
//! - `id`: `n:<id>` (узел), `i:|c:|r:|t:|k:|q:<id>` (запись журнала; `e:` тоже), голый id (узел, иначе
//!   запись) или путь `<пространство>:/<путь>` (`knowledge:/Инструменты/Bash`). Не больше
//!   `read.max_items` (20) элементов, пусто или больше — 422; `locator` — 422
//!   `locator-unsupported` (этап M5). Некорректный `tier`, нулевые `max_tokens` и
//!   `budget_tokens` — 400. Всё это проверяется до чтения;
//! - `tier` — `L0` | `L1` | `L2` (по умолчанию). Узел: сводка, оглавление с первым предложением
//!   секций, текст. Запись журнала **[решение исполнителя]**: L0 — заголовок, L1 — `note_l1`
//!   тела (нет — заголовок), L2 — тело. `section` — путь заголовков (как в `GET /nodes`);
//! - `if_rev` равен текущей ревизии → элемент `unchanged` без текста (и без учёта показа);
//! - **бюджет**: `budget_tokens` (по умолчанию `read.budget_tokens` = 1500) на весь ответ,
//!   заголовки и текст. Элемент, не влезший целиком (или длиннее своего `max_tokens`),
//!   обрезается с пометкой `…ещё N токенов`; обрезается и первый элемент (хоть что-то
//!   отдаётся всегда), следующие — только заголовок и `(budget)`. В `~T tok` заголовка —
//!   размер полного текста элемента (как в карточке поиска), в последней строке и в
//!   `total_tokens` — оценка выданного;
//! - ошибка по элементу лежит в самом элементе, ответ остаётся 200: `error` —
//!   `not_found` (нет, удалён, Human по токену, Vault, чужая база вне решения, неизвестная
//!   секция), `forbidden`, `solution_access` (+ `state`, `agent_solution_id`), `invalid`
//!   (папка без текста, неоднозначная секция, некорректный путь). Сбой хранилища — 500;
//! - **`session_seen`** (§6.3, решение контроллера R10/R11): сессия — `session_id` из тела
//!   или `X-Zam-Session`, лежит в базе пользователя запроса. Цель отмечается целиком
//!   (`part = WHOLE_TARGET`, ревизия выданного текста) только когда отдан **полный текст L2
//!   без `section` и без обрезки** по `max_tokens` и бюджету; секция, L0/L1, обрезанный текст,
//!   `unchanged`, `(budget)` и ошибки — не отмечаются. Касание сессии и отметка — одна
//!   транзакция; сбой учёта — WARN (без id сессии и текста), ответ не портится. Некорректный
//!   id сессии — 400;
//! - если у какого-то элемента ошибка, в ответе (md и json) заголовок `X-Zam-Read-Error: <код первой>`:
//!   клиенту не нужно разбирать Markdown, где текст заметки мог бы выглядеть как заголовок ошибки;
//! - ответ `json` (по умолчанию, как у поиска): `{items: [...], total_tokens}`; `md` —
//!   заголовок на элемент `### n:<id> <пространство>/<путь> › <секция>  rev<N> · ~<T> tok`
//!   (у записи — как карточка §9.3: `### i:<id> insight/gotcha «заголовок»  active ·
//!   hypothesis · rev2 · ~120 tok`), текст и последняя строка `~<total> tok`.
//!
//! Маршрут — служебный, не в `memory_routes`: `/read` читает память и не пишет в неё, потому
//! `memory_writes` сессии не растёт (решение контроллера R12).
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_search::truncate_to_tokens;
use zam_store::{
    EntryKind, NodeKind, NodePath, SeenItem, SeenKey, StoreError, WHOLE_TARGET, count_tokens,
    get_node_by_path, mark_seen, node_content,
};

use super::json_body;
use super::nodes::read::{Tier, l1, parse_tier, section_slice};
use super::scope::{
    EntryScope, Mode, NodeScope, entry_scope, live, live_entry, node_scope, space_scope,
};
use super::search::md_inline;
use super::sessions::request_session;
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    items: Vec<ReadItem>,
    session_id: Option<String>,
    format: Option<String>,
    budget_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadItem {
    id: String,
    tier: Option<String>,
    section: Option<String>,
    /// Страница или лист документа — этап M5.
    locator: Option<Value>,
    max_tokens: Option<usize>,
    if_rev: Option<i64>,
}

/// Ошибка элемента: код, пояснение и дополнительные поля.
struct Fail {
    code: &'static str,
    detail: Option<String>,
    extra: Vec<(&'static str, Value)>,
}

impl Fail {
    fn new(code: &'static str, detail: Option<String>) -> Fail {
        Fail {
            code,
            detail,
            extra: Vec::new(),
        }
    }
}

/// Сбой чтения элемента: ошибка самого элемента (остаётся в ответе) или сбой запроса.
enum Failure {
    Item(Fail),
    Fatal(ApiError),
}

impl From<ApiError> for Failure {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::NotFound => Failure::Item(Fail::new("not_found", None)),
            ApiError::Forbidden | ApiError::Denied(_) => {
                Failure::Item(Fail::new("forbidden", None))
            }
            ApiError::SolutionAccess { guid, state } => Failure::Item(Fail {
                code: "solution_access",
                detail: None,
                extra: vec![("state", json!(state)), ("agent_solution_id", json!(guid))],
            }),
            ApiError::BadRequest(m) | ApiError::Unprocessable(m) => {
                Failure::Item(Fail::new("invalid", Some(m)))
            }
            other => Failure::Fatal(other),
        }
    }
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Self {
        ApiError::from(e).into()
    }
}

/// Что получено по элементу.
enum Content {
    /// `if_rev` совпал: текста нет.
    Unchanged,
    /// Текст выбранного уровня; `whole` — полный текст L2 без секции.
    Text { text: String, whole: bool },
}

/// Прочитанный элемент (до упаковки в бюджет).
struct Fetched {
    /// `id` как в запросе.
    raw: String,
    /// Каноническая ссылка `n:<id>` | `i:|c:|r:|t:|k:|q:<id>`.
    reference: String,
    /// `node` или вид записи (`insight`, `changelog`, `release`, `term`, `task`, `question`).
    kind: &'static str,
    /// Начало заголовка: ссылка и путь узла либо вид и заголовок записи.
    head: String,
    space: String,
    path: Option<String>,
    title: Option<String>,
    status: Option<String>,
    certainty: Option<String>,
    section: Option<String>,
    tier: Tier,
    rev: i64,
    max_tokens: Option<usize>,
    seen: SeenKey,
    content: Content,
}

/// Результат по элементу после упаковки.
enum Piece {
    Err {
        raw: String,
        fail: Fail,
    },
    Unchanged(Fetched),
    /// Не влез в бюджет: заголовок и `(budget)`.
    Budget {
        f: Fetched,
        full: usize,
    },
    Text {
        f: Fetched,
        text: String,
        full: usize,
        /// Токенов не выдано (обрезка по `max_tokens` или бюджету).
        more: usize,
        /// Отдан полный текст L2 без секции — цель выдана целиком.
        given_whole: bool,
    },
}

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::L0 => "L0",
        Tier::L1 => "L1",
        Tier::L2 => "L2",
    }
}

/// Узел или запись, найденные по `id` элемента.
enum Located {
    Node(NodeScope),
    Entry(EntryScope),
}

fn miss() -> Failure {
    Failure::Item(Fail::new("not_found", None))
}

/// Префикс ссылки элемента: `n` (узел) или буква записи (`i|c|r|t|k|q|e`) и остаток; без
/// известного префикса — `(None, raw)`.
fn split_ref(raw: &str) -> (Option<&str>, &str) {
    match raw.split_once(':') {
        Some((k, rest)) if k == "n" || zam_store::is_entry_ref_letter(k) => (Some(k), rest),
        _ => (None, raw),
    }
}

/// Находит узел или запись по `id` элемента с проверкой доступа (Human по токену и чужая база
/// вне решения — `not_found`).
async fn resolve(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
) -> Result<Located, Failure> {
    let raw = raw.trim();
    if let Some((space, path)) = raw.split_once(":/")
        && !space.is_empty()
    {
        let path = NodePath::parse(path).map_err(ApiError::from)?;
        let sc = space_scope(st, p, headers, space, Mode::Read).await?;
        let node = get_node_by_path(&sc.db, sc.space.id, &path).await?;
        return Ok(Located::Node(NodeScope {
            user: sc.user,
            owner: sc.owner,
            db: sc.db,
            space: sc.space,
            node,
            role: sc.role,
        }));
    }
    let (prefix, rest) = split_ref(raw);
    rest.parse::<Id>().map_err(|_| miss())?;
    match prefix {
        Some("n") => Ok(Located::Node(
            node_scope(st, p, headers, rest, Mode::Read).await?,
        )),
        Some(_) => Ok(Located::Entry(
            entry_scope(st, p, headers, rest, Mode::Read).await?,
        )),
        None => match node_scope(st, p, headers, rest, Mode::Read).await {
            Ok(sc) => Ok(Located::Node(sc)),
            Err(ApiError::NotFound) => Ok(Located::Entry(
                entry_scope(st, p, headers, rest, Mode::Read).await?,
            )),
            Err(e) => Err(e.into()),
        },
    }
}

/// Секция `section`: нет такой — `not_found` с пояснением.
fn slice<'a>(text: &'a str, section: &str) -> Result<&'a str, Failure> {
    section_slice(text, section).map_err(|e| match e {
        ApiError::NotFound => {
            Failure::Item(Fail::new("not_found", Some("секция не найдена".into())))
        }
        other => other.into(),
    })
}

async fn fetch_node(sc: NodeScope, it: &ReadItem, tier: Tier) -> Result<Fetched, Failure> {
    live(&sc)?;
    let node = &sc.node;
    let section = (tier != Tier::L0)
        .then(|| {
            it.section
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .flatten();
    let content = if it.if_rev == Some(node.current_rev) {
        Content::Unchanged
    } else if tier == Tier::L0 {
        Content::Text {
            text: node.l0.clone().unwrap_or_default(),
            whole: false,
        }
    } else if node.kind == NodeKind::Folder {
        return Err(ApiError::BadRequest("узел — папка: текста нет".into()).into());
    } else {
        // ровно та ревизия, что в снимке узла
        let (_, text) = node_content(&sc.db, node.id, Some(node.current_rev)).await?;
        let (text, whole) = match section {
            Some(s) => (slice(&text, s)?.to_string(), false),
            None => (text, true),
        };
        match tier {
            Tier::L1 => Content::Text {
                text: l1(&text),
                whole: false,
            },
            _ => Content::Text { text, whole },
        }
    };
    let reference = format!("n:{}", node.id);
    Ok(Fetched {
        raw: it.id.clone(),
        head: format!("{reference} {}/{}", sc.space.name, node.path),
        reference,
        kind: "node",
        space: sc.space.name.clone(),
        path: Some(node.path.clone()),
        title: None,
        status: None,
        certainty: node.certainty.map(|c| c.as_str().to_string()),
        section: section.map(str::to_string),
        tier,
        rev: node.current_rev,
        max_tokens: it.max_tokens,
        seen: SeenKey {
            owner_id: sc.owner,
            target: TargetRef {
                kind: TargetKind::Node,
                id: node.id,
            },
            part: WHOLE_TARGET.to_string(),
        },
        content,
    })
}

async fn fetch_entry(sc: EntryScope, it: &ReadItem, tier: Tier) -> Result<Fetched, Failure> {
    live_entry(&sc)?;
    let e = &sc.entry;
    let section = (tier != Tier::L0)
        .then(|| {
            it.section
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .flatten();
    let content = if it.if_rev == Some(e.rev) {
        Content::Unchanged
    } else if tier == Tier::L0 {
        Content::Text {
            text: e.title.clone(),
            whole: false,
        }
    } else {
        let (text, whole) = match section {
            Some(s) => (slice(&e.body, s)?.to_string(), false),
            None => (e.body.clone(), true),
        };
        match tier {
            Tier::L1 => {
                let t = l1(&text);
                Content::Text {
                    text: if t.trim().is_empty() {
                        e.title.clone()
                    } else {
                        t
                    },
                    whole: false,
                }
            }
            _ => Content::Text { text, whole },
        }
    };
    let (letter, kind) = (e.kind.ref_letter(), e.kind.as_str());
    let reference = format!("{letter}:{}", e.id);
    let sub = match e.kind {
        EntryKind::Release => None,
        _ => e.data.get("kind").and_then(Value::as_str),
    };
    let label = match sub {
        Some(k) => format!("{kind}/{}", md_inline(k)),
        None => kind.to_string(),
    };
    Ok(Fetched {
        raw: it.id.clone(),
        head: format!("{reference} {label} «{}»", md_inline(&e.title)),
        reference,
        kind,
        space: sc.space.name.clone(),
        path: None,
        title: Some(e.title.clone()),
        status: e.status.map(|s| s.as_str().to_string()),
        certainty: e.certainty.map(|c| c.as_str().to_string()),
        section: section.map(str::to_string),
        tier,
        rev: e.rev,
        max_tokens: it.max_tokens,
        seen: SeenKey {
            owner_id: sc.owner,
            target: TargetRef {
                kind: TargetKind::Entry,
                id: e.id,
            },
            part: WHOLE_TARGET.to_string(),
        },
        content,
    })
}

async fn fetch(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    it: &ReadItem,
    tier: Tier,
) -> Result<Fetched, Failure> {
    match resolve(st, p, headers, &it.id).await? {
        Located::Node(sc) => fetch_node(sc, it, tier).await,
        Located::Entry(sc) => fetch_entry(sc, it, tier).await,
    }
}

/// Заголовок элемента md: `### <начало>[ › секция]  [статус · ]rev<N> · <хвост>`.
fn header(f: &Fetched, tail: &str) -> String {
    let mut line = format!("### {}", f.head);
    if let Some(s) = &f.section {
        line.push_str(" › ");
        line.push_str(s);
    }
    let mut parts: Vec<String> = [&f.status, &f.certainty]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    parts.push(format!("rev{}", f.rev));
    parts.push(tail.to_string());
    line.push_str(&format!("  {}", parts.join(" · ")));
    line
}

fn err_line(raw: &str, fail: &Fail) -> String {
    format!("### {}  error: {}", md_inline(raw), fail.code)
}

/// Заголовок ответа: `error` первого элемента с ошибкой (нет заголовка — ошибок нет).
pub const ERROR_HEADER: &str = "x-zam-read-error";

/// Строка вместо текста элемента, не влезшего в бюджет.
const BUDGET_LINE: &str = "(budget)";

fn marker(more: usize) -> String {
    format!("…ещё {more} токенов")
}

/// Минимальная цена элемента в ответе: заголовок (и `(budget)` у элемента с текстом).
fn floor_cost(r: &Result<Fetched, (String, Fail)>) -> usize {
    match r {
        Err((raw, fail)) => count_tokens(&err_line(raw, fail)),
        Ok(f) => match &f.content {
            Content::Unchanged => count_tokens(&header(f, "unchanged")),
            Content::Text { text, .. } => {
                let full = count_tokens(text);
                count_tokens(&header(f, &format!("~{full} tok"))) + count_tokens(BUDGET_LINE)
            }
        },
    }
}

/// Упаковка прочитанных элементов в бюджет; возвращает куски и оценку выданных токенов.
///
/// Каждому следующему элементу заранее оставлено место под его заголовок (и `(budget)`), поэтому
/// ответ укладывается в бюджет, если бюджета хватает на заголовки; первый элемент обрезается, но
/// отдаётся всегда.
fn pack(fetched: Vec<Result<Fetched, (String, Fail)>>, budget: usize) -> (Vec<Piece>, usize) {
    // suffix[i] — минимальная цена элементов после i-го
    let floors: Vec<usize> = fetched.iter().map(floor_cost).collect();
    let mut suffix = vec![0usize; floors.len() + 1];
    for i in (0..floors.len()).rev() {
        suffix[i] = suffix[i + 1] + floors[i];
    }
    let mut exhausted = false;
    let mut total = 0usize;
    let mut out = Vec::with_capacity(fetched.len());
    for (i, r) in fetched.into_iter().enumerate() {
        let f = match r {
            Err((raw, fail)) => {
                total += count_tokens(&err_line(&raw, &fail));
                out.push(Piece::Err { raw, fail });
                continue;
            }
            Ok(f) => f,
        };
        let text = match &f.content {
            Content::Unchanged => {
                total += count_tokens(&header(&f, "unchanged"));
                out.push(Piece::Unchanged(f));
                continue;
            }
            Content::Text { text, .. } => text.clone(),
        };
        let whole = matches!(f.content, Content::Text { whole: true, .. });
        let full = count_tokens(&text);
        let hc = count_tokens(&header(&f, &format!("~{full} tok")));
        if exhausted {
            total += hc + count_tokens(BUDGET_LINE);
            out.push(Piece::Budget { f, full });
            continue;
        }
        let mut kept: &str = &text;
        let mut cut = false;
        if let Some(m) = f.max_tokens
            && full > m
        {
            kept = truncate_to_tokens(kept, m, &count_tokens);
            cut = true;
        }
        let have = if cut { count_tokens(kept) } else { full };
        let allowed = budget
            .saturating_sub(total + suffix[i + 1])
            .saturating_sub(hc);
        if have > allowed {
            let room = allowed.saturating_sub(count_tokens(&marker(full)));
            kept = truncate_to_tokens(kept, room, &count_tokens);
            cut = true;
            exhausted = true;
        }
        let delivered = if cut { count_tokens(kept) } else { full };
        let more = full.saturating_sub(delivered);
        total += hc + delivered;
        if cut {
            total += count_tokens(&marker(more));
        }
        out.push(Piece::Text {
            text: kept.to_string(),
            full,
            more,
            given_whole: whole && !cut,
            f,
        });
    }
    (out, total)
}

fn item_json(piece: &Piece) -> Value {
    let mut o = Map::new();
    let base = |f: &Fetched, o: &mut Map<String, Value>| {
        o.insert("id".into(), json!(f.raw));
        o.insert("ref".into(), json!(f.reference));
        o.insert("kind".into(), json!(f.kind));
        o.insert("space".into(), json!(f.space));
        if let Some(p) = &f.path {
            o.insert("path".into(), json!(p));
        }
        if let Some(t) = &f.title {
            o.insert("title".into(), json!(t));
        }
        if let Some(s) = &f.status {
            o.insert("status".into(), json!(s));
        }
        if let Some(c) = &f.certainty {
            o.insert("certainty".into(), json!(c));
        }
        if let Some(s) = &f.section {
            o.insert("section".into(), json!(s));
        }
        o.insert("tier".into(), json!(tier_str(f.tier)));
        o.insert("rev".into(), json!(f.rev));
    };
    match piece {
        Piece::Err { raw, fail } => {
            o.insert("id".into(), json!(raw));
            o.insert("error".into(), json!(fail.code));
            if let Some(d) = &fail.detail {
                o.insert("detail".into(), json!(d));
            }
            for (k, v) in &fail.extra {
                o.insert((*k).into(), v.clone());
            }
        }
        Piece::Unchanged(f) => {
            base(f, &mut o);
            o.insert("unchanged".into(), json!(true));
        }
        Piece::Budget { f, full } => {
            base(f, &mut o);
            o.insert("tokens".into(), json!(full));
            o.insert("budget".into(), json!(true));
        }
        Piece::Text {
            f,
            text,
            full,
            more,
            ..
        } => {
            base(f, &mut o);
            o.insert("tokens".into(), json!(full));
            o.insert("text".into(), json!(text));
            if *more > 0 {
                o.insert("truncated".into(), json!(true));
                o.insert("more_tokens".into(), json!(more));
            }
        }
    }
    Value::Object(o)
}

fn push_block(out: &mut String, line: &str) {
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
}

fn render_md(pieces: &[Piece], total: usize) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Err { raw, fail } => {
                push_block(&mut out, &err_line(raw, fail));
                if let Some(d) = &fail.detail {
                    push_block(&mut out, &format!("({})", md_inline(d)));
                }
            }
            Piece::Unchanged(f) => push_block(&mut out, &header(f, "unchanged")),
            Piece::Budget { f, full } => {
                push_block(&mut out, &header(f, &format!("~{full} tok")));
                push_block(&mut out, BUDGET_LINE);
            }
            Piece::Text {
                f,
                text,
                full,
                more,
                ..
            } => {
                push_block(&mut out, &header(f, &format!("~{full} tok")));
                if !text.is_empty() {
                    push_block(&mut out, text);
                }
                if *more > 0 {
                    push_block(&mut out, &marker(*more));
                }
            }
        }
        out.push('\n');
    }
    out.push_str(&format!("~{total} tok\n"));
    out
}

/// `POST /read`.
pub async fn read(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let req: ReadRequest = json_body(body)?;
    let md = match req.format.as_deref().unwrap_or("json") {
        "json" => false,
        "md" => true,
        _ => return Err(ApiError::BadRequest("format: json или md".into())),
    };
    let max_items = st.cfg.read.max_items;
    if req.items.is_empty() || req.items.len() > max_items {
        return Err(ApiError::Unprocessable(format!(
            "items: от 1 до {max_items} элементов"
        )));
    }
    let budget = req.budget_tokens.unwrap_or(st.cfg.read.budget_tokens);
    if budget == 0 {
        return Err(ApiError::BadRequest("budget_tokens: больше 0".into()));
    }
    let mut tiers = Vec::with_capacity(req.items.len());
    for it in &req.items {
        if it.locator.as_ref().is_some_and(|l| !l.is_null()) {
            return Err(ApiError::LocatorUnsupported(
                "locator (страница, лист) поддерживается с этапа M5".into(),
            ));
        }
        if it.max_tokens == Some(0) {
            return Err(ApiError::BadRequest("max_tokens: больше 0".into()));
        }
        tiers.push(parse_tier(it.tier.as_deref())?);
    }
    // сессия: некорректный id — 400; сбой её базы чтение не валит (WARN)
    let session = match request_session(&st, &p, &headers, req.session_id.as_deref()).await {
        Ok(s) => s,
        // некорректный id — 400, несуществующий пользователь (`X-Zam-User`) — 404
        Err(e @ (ApiError::BadRequest(_) | ApiError::NotFound)) => return Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "чтение: база сессии недоступна, показ не записан");
            None
        }
    };

    let mut fetched = Vec::with_capacity(req.items.len());
    for (it, tier) in req.items.iter().zip(tiers) {
        fetched.push(match fetch(&st, &p, &headers, it, tier).await {
            Ok(f) => Ok(f),
            Err(Failure::Item(fail)) => Err((it.id.clone(), fail)),
            Err(Failure::Fatal(e)) => return Err(e),
        });
    }
    let (pieces, total) = pack(fetched, budget);

    if let Some(s) = &session {
        let given: Vec<SeenItem> = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Text {
                    f,
                    given_whole: true,
                    ..
                } => Some(SeenItem {
                    key: f.seen.clone(),
                    rev: f.rev,
                }),
                _ => None,
            })
            .collect();
        if let Err(e) = mark_seen(&s.db, &s.touch, &given).await {
            tracing::warn!(error = %e, "чтение: отметка выданного сессии не выполнена");
        }
    }

    // код `error` первого элемента с ошибкой — для клиентов, которым md не разобрать
    let first_error = pieces.iter().find_map(|p| match p {
        Piece::Err { fail, .. } => Some(fail.code),
        _ => None,
    });
    let mut r = if md {
        let mut r = render_md(&pieces, total).into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/markdown; charset=utf-8"),
        );
        r
    } else {
        axum::Json(json!({
            "items": pieces.iter().map(item_json).collect::<Vec<_>>(),
            "total_tokens": total,
        }))
        .into_response()
    };
    if let Some(code) = first_error {
        r.headers_mut()
            .insert(ERROR_HEADER, HeaderValue::from_static(code));
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Буквы ссылок записи в `id` элемента: `i|c|r|e` и с M4.5 `t` (term), `k` (task),
    /// `q` (question); неизвестный префикс — путь или голый id.
    #[test]
    fn entry_ref_letters() {
        for p in ["n", "i", "c", "r", "e", "t", "k", "q"] {
            let raw = format!("{p}:01ABC");
            assert_eq!(split_ref(&raw), (Some(p), "01ABC"), "{p}");
        }
        assert_eq!(split_ref("x:01ABC"), (None, "x:01ABC"));
        assert_eq!(split_ref("01ABC"), (None, "01ABC"));
        for k in EntryKind::ALL {
            assert!(zam_store::is_entry_ref_letter(k.ref_letter()));
            assert_eq!(EntryKind::from_ref_letter(k.ref_letter()), Some(k));
        }
        assert_eq!(EntryKind::Term.ref_letter(), "t");
        assert_eq!(EntryKind::Task.ref_letter(), "k");
        assert_eq!(EntryKind::Question.ref_letter(), "q");
    }
}
