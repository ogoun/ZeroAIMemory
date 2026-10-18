//! `POST /search` (§9.1–9.2) — в M1 только полнотекстовый канал (FTS5), векторного
//! индекса нет: ответ всегда `degraded: true`.
//!
//! **Тело** (остальные поля §9.1 принимаются и пока игнорируются):
//! - `query` — до 512 символов, синтаксис FTS5 в нём не действует;
//! - `scope`: `solution`, `include_knowledge` (true), `include_rules` (true),
//!   `all_solutions` (false), `path_prefix`, `spaces: [SpaceRef]`, `include_human`;
//! - `mode`: `brief` (по умолчанию) | `chunks` | `notes`;
//! - `top_k` (10, 1..=100), `budget_tokens` (800), `format`: `json` (по умолчанию) | `md`.
//!
//! **Область.** `scope.spaces` задаёт пространства явно; иначе — `scope.solution`
//! (или заголовок `X-Zam-Solution`) плюс Knowledge и Rules по флагам, а при
//! `all_solutions` — все решения. Неизвестное решение или пространство из списка
//! просто не добавляет пространства (ответ 200, возможно пустой). Human — только
//! администратору и только с `include_human: true`; токен его не видит никогда.
//!
//! **Бюджет** — приблизительно, символы / 4 **[решение исполнителя до токенизатора
//! M2]**: элементы добавляются, пока сумма длин пути, заголовка, сниппета (или L0)
//! и текста (`chunks`) не превысит `budget_tokens × 4`; в `chunks` текст последнего
//! элемента урезается до остатка (`text_truncated: true`). Хотя бы один элемент
//! возвращается всегда.
//!
//! **Режимы.** `brief` — карточки со `snippet`; `chunks` — плюс `text`; `notes` — по
//! одному элементу на узел (лучший чанк), `l0` вместо `snippet`.
//!
//! **Формат `md`** — строки `- [<пространство>/<путь> › <секция>] (<id>@<rev>) <сниппет>`,
//! маркеры совпадений — `**…**`; в `chunks` под строкой — текст с отступом.
//! В JSON маркеры остаются U+E000/U+E001.
use std::collections::HashMap;
use std::time::Instant;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_store::time::fmt_ts;
use zam_store::{
    Db, FtsHit, FtsQuery, MARK_END, MARK_START, Space, SpaceKind, get_node, get_solution,
    list_spaces, search_fts,
};

use super::json_body;
use super::scope::{SpaceRef, canonical_guid, find_space, token_can_read, user_db};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Заголовок текущего решения агента.
pub const SOLUTION_HEADER: &str = "x-zam-solution";
/// Причина деградации в M1.
pub const DEGRADED_REASON: &str = "vector index not available (M1)";
const DEFAULT_TOP_K: usize = 10;
const DEFAULT_BUDGET_TOKENS: usize = 800;
/// Символов на токен в приблизительном бюджете.
const CHARS_PER_TOKEN: usize = 4;
/// Предел top_k в FTS (`zam_store::search`).
const TOP_K_MAX: usize = 100;
/// FTS отдаёт до 3 чанков на узел: для `notes` кандидатов берётся с запасом.
const NOTES_OVERSAMPLE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SearchMode {
    Brief,
    Chunks,
    Notes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutFormat {
    Json,
    Md,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScopeReq {
    solution: Option<String>,
    include_knowledge: Option<bool>,
    include_rules: Option<bool>,
    all_solutions: bool,
    path_prefix: Option<String>,
    spaces: Option<Vec<String>>,
    include_human: bool,
}

#[derive(Deserialize)]
struct SearchReq {
    query: String,
    #[serde(default)]
    scope: ScopeReq,
    mode: Option<SearchMode>,
    top_k: Option<usize>,
    budget_tokens: Option<usize>,
    format: Option<OutFormat>,
}

/// Пространства запроса с учётом политики доступа (см. модуль).
async fn resolve_spaces(
    db: &Db,
    p: &Principal,
    headers: &HeaderMap,
    sc: &ScopeReq,
) -> Result<Vec<Space>, ApiError> {
    let human_ok = matches!(p, Principal::Admin { .. }) && sc.include_human;
    let mut out: Vec<Space> = Vec::new();
    if let Some(list) = &sc.spaces {
        for raw in list {
            // неизвестная форма или пространство — просто не добавляется
            let Ok(r) = SpaceRef::parse(raw) else {
                continue;
            };
            match find_space(db, &r).await {
                Ok(s) => out.push(s),
                Err(ApiError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
    } else {
        let solution = match &sc.solution {
            Some(g) => Some(
                canonical_guid(g.trim())
                    .ok_or_else(|| ApiError::BadRequest("scope.solution: GUID решения".into()))?,
            ),
            // заголовок агента: некорректное значение игнорируется
            None => headers
                .get(SOLUTION_HEADER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| canonical_guid(v.trim())),
        };
        if let Some(g) = solution
            && let Some(s) = get_solution(db, &g).await?
        {
            out.push(s);
        }
        let all = list_spaces(db).await?;
        for s in all {
            let take = match s.kind {
                SpaceKind::Knowledge => sc.include_knowledge.unwrap_or(true),
                SpaceKind::Rules => sc.include_rules.unwrap_or(true),
                SpaceKind::Solution => sc.all_solutions,
                SpaceKind::Human => human_ok,
            };
            if take {
                out.push(s);
            }
        }
    }
    out.retain(|s| token_can_read(p, s) && (s.kind != SpaceKind::Human || human_ok));
    out.sort_by_key(|s| s.id);
    out.dedup_by_key(|s| s.id);
    Ok(out)
}

/// Элемент выдачи до упаковки.
struct Item {
    hit: FtsHit,
    chunk_ids: Vec<String>,
    l0: Option<String>,
}

/// `notes`: по одному элементу на узел (лучший чанк), не больше `top_k`.
fn group_by_node(hits: Vec<FtsHit>, top_k: usize) -> Vec<Item> {
    let mut order: Vec<Item> = Vec::new();
    let mut pos: HashMap<Id, usize> = HashMap::new();
    for h in hits {
        match pos.get(&h.node_id) {
            Some(&i) => order[i].chunk_ids.push(h.chunk_id),
            None => {
                pos.insert(h.node_id, order.len());
                order.push(Item {
                    chunk_ids: vec![h.chunk_id.clone()],
                    hit: h,
                    l0: None,
                });
            }
        }
    }
    order.truncate(top_k);
    order
}

/// Первые `n` символов строки.
fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Упакованный элемент: JSON и (для `md`) строки.
struct Packed {
    item: Item,
    text: Option<(String, bool)>,
}

/// Упаковка в бюджет (см. модуль).
fn pack(items: Vec<Item>, mode: SearchMode, budget_tokens: usize) -> Vec<Packed> {
    let budget = budget_tokens.saturating_mul(CHARS_PER_TOKEN);
    let mut used = 0usize;
    let mut out = Vec::new();
    for it in items {
        let summary = match mode {
            SearchMode::Notes => it.l0.as_deref().unwrap_or(""),
            _ => &it.hit.snippet,
        };
        let base = chars(&it.hit.path) + chars(&it.hit.heading_path) + chars(summary);
        if !out.is_empty() && used + base > budget {
            break;
        }
        let text = if mode == SearchMode::Chunks {
            let room = budget.saturating_sub(used + base);
            if !out.is_empty() && room == 0 {
                break;
            }
            let t = take_chars(&it.hit.text, room);
            let truncated = t.len() < it.hit.text.len();
            used += chars(t);
            Some((t.to_string(), truncated))
        } else {
            None
        };
        used += base;
        out.push(Packed { item: it, text });
    }
    out
}

fn item_json(p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> Value {
    let h = &p.item.hit;
    let mut o = Map::new();
    o.insert("node_id".into(), json!(h.node_id));
    o.insert("space".into(), json!(spaces.get(&h.space_id)));
    o.insert("path".into(), json!(h.path));
    o.insert("heading_path".into(), json!(h.heading_path));
    o.insert("rev".into(), json!(h.rev));
    o.insert("chunk_ids".into(), json!(p.item.chunk_ids));
    o.insert("score".into(), json!(h.score));
    o.insert("updated_at".into(), json!(fmt_ts(h.updated_at)));
    o.insert("kind".into(), json!("note"));
    if mode == SearchMode::Notes {
        o.insert("l0".into(), json!(p.item.l0));
    } else {
        o.insert("snippet".into(), json!(h.snippet));
    }
    if let Some((t, truncated)) = &p.text {
        o.insert("text".into(), json!(t));
        if *truncated {
            o.insert("text_truncated".into(), json!(true));
        }
    }
    Value::Object(o)
}

/// Одна строка: маркеры → `**`, переводы строк → пробелы.
fn md_inline(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\r' | '\n' => ' ',
            c => c,
        })
        .collect::<String>()
        .replace([MARK_START, MARK_END], "**")
}

fn item_md(p: &Packed, mode: SearchMode, spaces: &HashMap<Id, Space>) -> String {
    let h = &p.item.hit;
    let space = spaces.get(&h.space_id).map_or("?", |s| s.name.as_str());
    let mut line = format!("- [{space}/{}", h.path);
    if !h.heading_path.is_empty() {
        line.push_str(" › ");
        line.push_str(&h.heading_path);
    }
    line.push_str(&format!("] ({}@{})", h.node_id, h.rev));
    let summary = match mode {
        SearchMode::Notes => p.item.l0.clone().unwrap_or_default(),
        _ => h.snippet.clone(),
    };
    let summary = md_inline(&summary);
    if !summary.trim().is_empty() {
        line.push(' ');
        line.push_str(summary.trim());
    }
    line.push('\n');
    if let Some((t, _)) = &p.text {
        for l in t.lines() {
            line.push_str("  ");
            line.push_str(l);
            line.push('\n');
        }
    }
    line
}

/// `POST /search`.
pub async fn search(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let req: SearchReq = json_body(body)?;
    let mode = req.mode.unwrap_or(SearchMode::Brief);
    let format = req.format.unwrap_or(OutFormat::Json);
    let top_k = req.top_k.unwrap_or(DEFAULT_TOP_K);
    if !(1..=TOP_K_MAX).contains(&top_k) {
        return Err(ApiError::BadRequest(format!("top_k: от 1 до {TOP_K_MAX}")));
    }
    let budget = req.budget_tokens.unwrap_or(DEFAULT_BUDGET_TOKENS);
    let (_user, db) = user_db(&st, &p, &headers).await?;
    let spaces = resolve_spaces(&db, &p, &headers, &req.scope).await?;

    let started = Instant::now();
    let hits = if spaces.is_empty() {
        // запрос всё равно проверяется (длина), как и при непустой области
        if req.query.chars().count() > zam_store::search::QUERY_MAX_CHARS {
            return Err(ApiError::BadRequest(format!(
                "запрос длиннее {} символов",
                zam_store::search::QUERY_MAX_CHARS
            )));
        }
        Vec::new()
    } else {
        let fetch = match mode {
            SearchMode::Notes => (top_k * NOTES_OVERSAMPLE).min(TOP_K_MAX),
            _ => top_k,
        };
        search_fts(
            &db,
            FtsQuery {
                text: req.query,
                space_ids: spaces.iter().map(|s| s.id).collect(),
                path_prefix: req.scope.path_prefix.clone(),
                top_k: fetch,
            },
        )
        .await?
    };
    let fts_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut items = match mode {
        SearchMode::Notes => group_by_node(hits, top_k),
        _ => hits
            .into_iter()
            .map(|h| Item {
                chunk_ids: vec![h.chunk_id.clone()],
                hit: h,
                l0: None,
            })
            .collect(),
    };
    if mode == SearchMode::Notes {
        for it in &mut items {
            it.l0 = get_node(&db, it.hit.node_id).await?.l0;
        }
    }
    let packed = pack(items, mode, budget);
    let by_id: HashMap<Id, Space> = spaces.into_iter().map(|s| (s.id, s)).collect();

    Ok(match format {
        OutFormat::Json => {
            let items: Vec<Value> = packed.iter().map(|p| item_json(p, mode, &by_id)).collect();
            Json(json!({
                "items": items,
                "degraded": true,
                "degraded_reason": DEGRADED_REASON,
                "pending_count": 0,
                "timings": { "fts_ms": (fts_ms * 100.0).round() / 100.0 },
            }))
            .into_response()
        }
        OutFormat::Md => {
            let body: String = packed.iter().map(|p| item_md(p, mode, &by_id)).collect();
            let mut r = body.into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/markdown; charset=utf-8"),
            );
            r
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(node: Id, text: &str) -> FtsHit {
        FtsHit {
            chunk_id: format!("c{}", text.len()),
            node_id: node,
            space_id: Id::new(),
            path: "p".into(),
            heading_path: String::new(),
            rev: 1,
            score: 1.0,
            updated_at: jiff::Timestamp::now(),
            snippet: format!("{MARK_START}s{MARK_END}"),
            text: text.into(),
        }
    }

    fn items(hits: Vec<FtsHit>) -> Vec<Item> {
        hits.into_iter()
            .map(|h| Item {
                chunk_ids: vec![h.chunk_id.clone()],
                hit: h,
                l0: None,
            })
            .collect()
    }

    #[test]
    fn budget_trims_text_and_keeps_one() {
        let hs = vec![hit(Id::new(), &"а".repeat(100)), hit(Id::new(), "бб")];
        // 10 токенов = 40 символов; база первого — 1 (путь) + 3 (сниппет)
        let p = pack(items(hs), SearchMode::Chunks, 10);
        assert_eq!(p.len(), 1);
        let (t, tr) = p[0].text.clone().unwrap();
        assert_eq!(t.chars().count(), 36);
        assert!(tr);
        let p = pack(items(vec![hit(Id::new(), "x")]), SearchMode::Chunks, 0);
        assert_eq!(p.len(), 1);
        let p = pack(
            items(vec![hit(Id::new(), "x"), hit(Id::new(), "y")]),
            SearchMode::Brief,
            0,
        );
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn notes_group_by_node() {
        let a = Id::new();
        let b = Id::new();
        let g = group_by_node(vec![hit(a, "1"), hit(b, "22"), hit(a, "333")], 10);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].chunk_ids, vec!["c1".to_string(), "c3".to_string()]);
        assert_eq!(group_by_node(vec![hit(a, "1"), hit(b, "2")], 1).len(), 1);
    }

    #[test]
    fn md_markers() {
        assert_eq!(md_inline("a\u{E000}b\u{E001}\nc"), "a**b** c");
    }
}
