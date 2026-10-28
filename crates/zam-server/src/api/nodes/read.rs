//! Чтение узла: `GET /nodes/{id}`, `GET /nodes/by-path`, `GET /nodes/{id}/outline`.
//!
//! Параметры `GET /nodes/{id}` (и `by-path`):
//! - `format=json` (по умолчанию) → `{node, content, rev}`;
//! - `format=md` → байты ревизии как сохранены (`text/markdown`, `ETag: "<rev>"`);
//! - `format=html` → `{html, toc, warnings, rev}` из кэша рендера;
//! - `tier=L0` → `{l0, rev}` (сводка текущей ревизии);
//! - `tier=L1` → `{l1, rev}`: `note_l1` (§8.3) — оглавление и первое предложение каждой
//!   секции строками `<отступ><заголовок> — <предложение>`, ≤ 1200 символов; `L2` — полный текст;
//! - `section="A > B"` → только байты секции (нет — 404, неоднозначно — 422). Путь — заголовки
//!   от верхнего; если такого полного пути нет, подходит **хвост** пути (`Тесты` для
//!   `Сборка > Тесты`), когда он однозначен **[решение исполнителя, проверка на стенде M4]**;
//! - `rev=N` → старая ревизия.
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use zam_markdown::{
    NOTE_L1_MAX_CHARS, OutlineItem, SectionLookup, note_l1, outline as doc_outline, parse,
    resolve_section,
};
use zam_store::{NodeKind, NodePath, get_node_by_path, node_content};

use crate::api::query;
use crate::api::render::{RenderCtx, Viewer, cache_get, cache_key, render, render_cached};
use crate::api::scope::{Mode, NodeScope, live, node_scope, space_scope};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

#[derive(Deserialize, Default)]
pub struct ReadQuery {
    format: Option<String>,
    tier: Option<String>,
    section: Option<String>,
    rev: Option<i64>,
    /// Только для `by-path`.
    space: Option<String>,
    /// Только для `by-path`.
    path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Md,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    L0,
    L1,
    L2,
}

fn parse_format(s: Option<&str>) -> Result<Format, ApiError> {
    match s.unwrap_or("json") {
        "json" => Ok(Format::Json),
        "md" => Ok(Format::Md),
        "html" => Ok(Format::Html),
        _ => Err(ApiError::BadRequest("format: json, md или html".into())),
    }
}

pub(crate) fn parse_tier(s: Option<&str>) -> Result<Tier, ApiError> {
    match s.map(str::to_ascii_uppercase).as_deref() {
        None | Some("L2") => Ok(Tier::L2),
        Some("L0") => Ok(Tier::L0),
        Some("L1") => Ok(Tier::L1),
        _ => Err(ApiError::BadRequest("tier: L0, L1 или L2".into())),
    }
}

pub async fn by_id(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    q: Result<Query<ReadQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let sc = node_scope(&st, &p, &headers, &id, Mode::Read).await?;
    read(&st, &p, sc, q).await
}

pub async fn by_path(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<ReadQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let (Some(space), Some(path)) = (q.space.as_deref(), q.path.as_deref()) else {
        return Err(ApiError::BadRequest("нужны параметры space и path".into()));
    };
    let sc = space_scope(&st, &p, &headers, space, Mode::Read).await?;
    let node = get_node_by_path(&sc.db, sc.space.id, &NodePath::parse(path)?).await?;
    let ns = NodeScope {
        user: sc.user,
        owner: sc.owner,
        db: sc.db,
        space: sc.space,
        node,
        role: sc.role,
    };
    read(&st, &p, ns, q).await
}

/// `GET /nodes/{id}/outline` → `[OutlineItem]` текущей ревизии.
pub async fn outline(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<OutlineItem>>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live(&sc)?;
    if sc.node.kind == NodeKind::Folder {
        return Ok(Json(Vec::new()));
    }
    let (_, text) = node_content(&sc.db, sc.node.id, None).await?;
    Ok(Json(doc_outline(&parse(&text))))
}

async fn read(
    st: &AppState,
    p: &Principal,
    sc: NodeScope,
    q: ReadQuery,
) -> Result<Response, ApiError> {
    // Доступ уже проверен в node_scope / space_scope.
    live(&sc)?;
    let format = parse_format(q.format.as_deref())?;
    let tier = parse_tier(q.tier.as_deref())?;
    let node = &sc.node;
    let ctx = RenderCtx {
        space: sc.space.id,
        viewer: Viewer::of(st, p, sc.owner, sc.role),
    };
    if tier == Tier::L0 {
        return Ok(Json(json!({ "l0": node.l0, "rev": node.current_rev })).into_response());
    }
    if node.kind == NodeKind::Folder {
        return match (tier, format) {
            (Tier::L2, Format::Json) => {
                Ok(Json(json!({ "node": node, "content": null, "rev": 0 })).into_response())
            }
            _ => Err(ApiError::BadRequest("узел — папка: текста нет".into())),
        };
    }
    let current = q.rev.is_none_or(|r| r == node.current_rev);

    // Попадание в кэш HTML текущей ревизии — без чтения текста.
    if tier == Tier::L2
        && format == Format::Html
        && q.section.is_none()
        && current
        && let Some(hash) = &node.content_hash
        && let Some(r) = cache_get(st, &cache_key(st, hash, sc.space.id, sc.owner, ctx.viewer))
    {
        return Ok(html_json(&r, node.current_rev));
    }

    // Ровно та ревизия, что в снимке узла: иначе запись между чтением узла и текста
    // дала бы текст rev N+1 при `node`/хэше rev N (и отравила бы кэш HTML).
    let (rev, text) =
        node_content(&sc.db, node.id, Some(q.rev.unwrap_or(node.current_rev))).await?;
    let (text, whole) = match q.section.as_deref() {
        Some(path) => (section_slice(&text, path)?.to_string(), false),
        None => (text, true),
    };
    if tier == Tier::L1 {
        return Ok(Json(json!({ "l1": l1(&text), "rev": rev })).into_response());
    }
    Ok(match format {
        Format::Md => markdown_response(text, rev),
        Format::Json => Json(json!({ "node": node, "content": text, "rev": rev })).into_response(),
        Format::Html if whole => {
            let hash = key_hash(node, rev, &text);
            let key = cache_key(st, &hash, sc.space.id, sc.owner, ctx.viewer);
            let r = render_cached(st, key, &sc.db, ctx, text).await?;
            html_json(&r, rev)
        }
        // Секция рендерится без кэша: ключ по хэшу заметки ей не подходит.
        Format::Html => html_json(&render(&sc.db, Some(ctx), text).await?, rev),
    })
}

fn html_json(r: &zam_markdown::Rendered, rev: i64) -> Response {
    Json(json!({ "html": r.html, "toc": r.toc, "warnings": r.warnings, "rev": rev }))
        .into_response()
}

/// Текст ревизии как есть: `text/markdown; charset=utf-8`, `ETag: "<rev>"`.
pub(crate) fn markdown_response(text: String, rev: i64) -> Response {
    let mut r = text.into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    h.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{rev}\"")).expect("число — допустимый заголовок"),
    );
    r
}

/// Хэш для ключа кэша — по ревизии, текст которой действительно прочитан:
/// `content_hash` узла годится только для его `current_rev` (ревизии неизменяемы).
fn key_hash(node: &zam_store::Node, rev_read: i64, text: &str) -> String {
    match &node.content_hash {
        Some(h) if rev_read == node.current_rev => h.clone(),
        _ => sha256_hex(text.as_bytes()),
    }
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Байты секции `path` (весь её диапазон, с заголовком и подсекциями).
/// Сначала — полный путь заголовков от верхнего; если такого нет — секции, чей путь
/// кончается `path` (агент обычно знает только свой заголовок: `Тесты` вместо
/// `Сборка > Тесты`). Нет секции → 404, путь у нескольких секций → 422.
pub(crate) fn section_slice<'a>(text: &'a str, path: &str) -> Result<&'a str, ApiError> {
    match resolve_section(&parse(text), path) {
        SectionLookup::Found(s) => text.get(s.range.clone()).ok_or(ApiError::NotFound),
        SectionLookup::NotFound => Err(ApiError::NotFound),
        SectionLookup::Ambiguous => Err(ApiError::Unprocessable(format!(
            "путь секции неоднозначен: {path}"
        ))),
    }
}

/// L1 (§8.3): оглавление и первое предложение каждой секции — тот же `note_l1`, что
/// чанкер индексирует как сводку заметки, не длиннее [`NOTE_L1_MAX_CHARS`] символов.
pub(crate) fn l1(text: &str) -> String {
    note_l1(text, &parse(text), NOTE_L1_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hash_follows_revision_read() {
        let now = jiff::Timestamp::now();
        let text = "текст rev 1
";
        let node = zam_store::Node {
            id: zam_core::Id::new(),
            space_id: zam_core::Id::new(),
            parent_id: None,
            kind: NodeKind::Note,
            name: "n".into(),
            path: "n".into(),
            current_rev: 2,
            content_hash: Some("hash-of-rev-2".into()),
            frontmatter: None,
            l0: None,
            l0_source: None,
            index_state: "pending".into(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            has_children: false,
            certainty: None,
        };
        assert_eq!(key_hash(&node, 2, "что угодно"), "hash-of-rev-2");
        // прочитана другая ревизия — ключ по её собственному тексту
        assert_eq!(key_hash(&node, 1, text), sha256_hex(text.as_bytes()));
        assert_eq!(key_hash(&node, 3, text), sha256_hex(text.as_bytes()));
    }

    #[test]
    fn l1_outline_and_limit() {
        let t = "# A
Первое. Второе.
## B
Текст B.
# C
";
        assert_eq!(
            l1(t),
            "A — Первое.
  B — Текст B.
C"
        );
        let big: String = (0..200)
            .map(|i| {
                format!(
                    "# Заголовок {i}
Текст.
"
                )
            })
            .collect();
        assert!(l1(&big).chars().count() <= NOTE_L1_MAX_CHARS);
    }

    #[test]
    fn section_slice_cases() {
        let t = "# A\nx\n## B\ny\n# A\nz\n";
        assert_eq!(section_slice(t, "A > B").unwrap(), "## B\ny\n");
        assert!(matches!(
            section_slice(t, "A"),
            Err(ApiError::Unprocessable(_))
        ));
        assert!(matches!(section_slice(t, "Q"), Err(ApiError::NotFound)));
        assert!(matches!(section_slice(t, "A >"), Err(ApiError::NotFound)));
        // стенд Debian 13: хвост пути заголовков, если полного нет и он однозначен
        let t = "# Сборка\nx\n## Тесты\ny\n### Unit\nu\n";
        assert_eq!(
            section_slice(t, "Тесты").unwrap(),
            "## Тесты\ny\n### Unit\nu\n"
        );
        assert_eq!(section_slice(t, "Тесты > Unit").unwrap(), "### Unit\nu\n");
        assert_eq!(section_slice(t, "Unit").unwrap(), "### Unit\nu\n");
        assert!(matches!(
            section_slice(t, "Сборка > Unit"),
            Err(ApiError::NotFound)
        ));
        let t = "# A\n## Тесты\n1\n# B\n## Тесты\n2\n";
        assert!(matches!(
            section_slice(t, "Тесты"),
            Err(ApiError::Unprocessable(_))
        ));
        assert_eq!(section_slice(t, "B > Тесты").unwrap(), "## Тесты\n2\n");
        // полный путь сильнее хвоста: верхний «Тесты» не спорит с «A > Тесты»
        let t = "# Тесты\n0\n# A\n## Тесты\n1\n";
        assert_eq!(section_slice(t, "Тесты").unwrap(), "# Тесты\n0\n");
    }
}
