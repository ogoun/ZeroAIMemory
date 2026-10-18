//! Рендер Markdown (§12, §15.2): `POST /render` (предпросмотр редактора),
//! `GET /render/highlight.css`, резолвер ссылок сервера и кэш HTML заметок.
//!
//! **Резолвер.** `Resolver` из `zam-markdown` синхронный, а поиск узлов — асинхронный,
//! поэтому цели вики-ссылок разрешаются заранее, до рендера:
//! - цель декодируется из percent-encoding;
//! - цель с `/` — путь в том же пространстве (`get_node_by_path`, только заметки);
//! - иначе `find_by_name` — первая по `path`;
//! - href — `#/n/<node_id>`, с якорем `#h-<slug(якорь)>`;
//! - не найдено → `None` (битая ссылка, предупреждение рендера).
//!
//! Вложения в этой задаче не разрешаются (`attachment` → `None`): их подключает задача 9.
//!
//! **Кэш.** LRU на 512 записей; ключ — `(content_hash, RENDER_VERSION, space_id, user_id)`:
//! href вики-ссылок зависят от узлов пространства и от пользователя (`X-Zam-User`).
//! Ничего больше не инвалидируется: если после рендера появилась или исчезла цель
//! вики-ссылки, кэшированный HTML покажет её прежнее состояние до изменения самой
//! заметки (или вытеснения записи) — это допустимо.
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use lru::LruCache;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_markdown::{
    HEADER_ID_PREFIX, LinkKind, LinkResolution, RENDER_VERSION, Rendered, Resolver, highlight_css,
    parse, render_html, slug,
};
use zam_store::{Db, NodeKind, NodePath, find_by_name, get_node_by_path};

use super::json_body;
use super::scope::{Mode, node_scope};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Предел тела `POST /render`, байт (1 МБ).
pub const RENDER_BODY_MAX: usize = 1024 * 1024;
/// Записей в кэше рендера.
pub const RENDER_CACHE_ENTRIES: usize = 512;
/// HTML крупнее не кэшируется: 512 записей по несколько МБ — слишком много памяти.
pub const RENDER_CACHE_MAX_HTML: usize = 256 * 1024;
/// Не больше стольких разных целей вики-ссылок разрешается в одной заметке;
/// остальные считаются битыми (защита от тысяч запросов к БД на один рендер).
const WIKI_TARGETS_MAX: usize = 500;

/// `(content_hash, RENDER_VERSION, space_id, user_id)`.
pub type RenderKey = (String, u32, Id, Id);
/// Кэш HTML заметок.
pub type RenderCache = Arc<Mutex<LruCache<RenderKey, Arc<Rendered>>>>;

pub fn new_cache() -> RenderCache {
    let n = NonZeroUsize::new(RENDER_CACHE_ENTRIES).expect("ненулевой размер");
    Arc::new(Mutex::new(LruCache::new(n)))
}

/// Ключ кэша для текста с хэшем `content_hash` в пространстве `space` пользователя `user`.
pub fn cache_key(content_hash: &str, space: Id, user: Id) -> RenderKey {
    (content_hash.to_string(), RENDER_VERSION, space, user)
}

/// Запись из кэша (отравленный мьютекс не мешает: кэш — только ускорение).
pub fn cache_get(st: &AppState, key: &RenderKey) -> Option<Arc<Rendered>> {
    let mut c = st.render_cache.lock().unwrap_or_else(|e| e.into_inner());
    c.get(key).cloned()
}

fn cache_put(st: &AppState, key: RenderKey, r: Arc<Rendered>) {
    let mut c = st.render_cache.lock().unwrap_or_else(|e| e.into_inner());
    c.put(key, r);
}

/// Рендер с кэшем по ключу `key`.
pub async fn render_cached(
    st: &AppState,
    key: RenderKey,
    db: &Db,
    space: Id,
    text: String,
) -> Result<Arc<Rendered>, ApiError> {
    if let Some(r) = cache_get(st, &key) {
        return Ok(r);
    }
    let r = Arc::new(render(db, Some(space), text).await?);
    if cacheable(&r) {
        cache_put(st, key, Arc::clone(&r));
    }
    Ok(r)
}

/// Кэшируется только HTML не больше [`RENDER_CACHE_MAX_HTML`].
fn cacheable(r: &Rendered) -> bool {
    r.html.len() <= RENDER_CACHE_MAX_HTML
}

/// Рендер текста: вики-ссылки разрешаются в пространстве `space` (`None` — не разрешаются).
/// Сам рендер (CPU) — в `spawn_blocking`.
pub async fn render(db: &Db, space: Option<Id>, text: String) -> Result<Rendered, ApiError> {
    let links = match space {
        Some(s) => resolve_wiki(db, s, &text).await?,
        None => HashMap::new(),
    };
    tokio::task::spawn_blocking(move || render_html(&text, &MapResolver { links }))
        .await
        .map_err(|e| ApiError::Internal(format!("рендер прерван: {e}")))
}

/// Цели вики-ссылок документа → id заметок (неразрешённые отсутствуют).
async fn resolve_wiki(db: &Db, space: Id, text: &str) -> Result<HashMap<String, Id>, ApiError> {
    let mut targets: Vec<String> = parse(text)
        .links
        .into_iter()
        .filter(|l| l.kind == LinkKind::Wiki)
        .map(|l| l.target)
        .collect();
    targets.sort();
    targets.dedup();
    targets.truncate(WIKI_TARGETS_MAX);
    let mut out = HashMap::new();
    for raw in targets {
        if let Some(id) = resolve_one(db, space, &raw).await? {
            out.insert(raw, id);
        }
    }
    Ok(out)
}

async fn resolve_one(db: &Db, space: Id, raw: &str) -> Result<Option<Id>, ApiError> {
    let target = percent_decode(raw);
    let target = target.trim();
    if target.is_empty() {
        return Ok(None);
    }
    if target.contains('/') {
        let Ok(path) = NodePath::parse(target) else {
            return Ok(None);
        };
        return match get_node_by_path(db, space, &path).await {
            Ok(n) if n.kind == NodeKind::Note => Ok(Some(n.id)),
            Ok(_) | Err(zam_store::StoreError::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        };
    }
    Ok(find_by_name(db, space, target)
        .await?
        .into_iter()
        .next()
        .map(|n| n.id))
}

/// Декодирует `%XX`; некорректные последовательности и не-UTF-8 остаются как есть.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Резолвер по заранее разрешённым целям.
struct MapResolver {
    links: HashMap<String, Id>,
}

impl Resolver for MapResolver {
    fn wiki(&self, target: &str, anchor: Option<&str>) -> Option<LinkResolution> {
        let id = self.links.get(target)?;
        let mut href = format!("#/n/{id}");
        if let Some(a) = anchor {
            href.push('#');
            href.push_str(HEADER_ID_PREFIX);
            href.push_str(&slug(&percent_decode(a)));
        }
        Some(LinkResolution { href, exists: true })
    }

    fn attachment(&self, _sha16: &str, _filename: &str) -> Option<String> {
        None
    }
}

#[derive(Deserialize)]
pub struct Preview {
    md: String,
    /// Узел, в пространстве которого разрешаются вики-ссылки.
    base_node: Option<String>,
}

/// `POST /render {md, base_node?}` → `{html, toc, links, warnings}`; тело ≤ 1 МБ.
pub async fn preview(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let req: Preview = json_body(body)?;
    let (db, space) = match &req.base_node {
        Some(id) => {
            let sc = node_scope(&st, &p, &headers, id, Mode::Read).await?;
            (Some(sc.db), Some(sc.space.id))
        }
        None => (None, None),
    };
    let r = match db {
        Some(db) => render(&db, space, req.md).await?,
        None => {
            let md = req.md;
            tokio::task::spawn_blocking(move || render_html(&md, &zam_markdown::NoResolve))
                .await
                .map_err(|e| ApiError::Internal(format!("рендер прерван: {e}")))?
        }
    };
    Ok(Json(json!({
        "html": r.html,
        "toc": r.toc,
        "links": r.links,
        "warnings": r.warnings,
    })))
}

/// `GET /render/highlight.css` — стили подсветки кода; кэшируется сутки.
pub async fn highlight(_: Principal) -> Response {
    let mut r = highlight_css().into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_html_not_cacheable() {
        let mk = |n: usize| Rendered {
            html: "x".repeat(n),
            toc: Vec::new(),
            links: Vec::new(),
            warnings: Vec::new(),
        };
        assert!(cacheable(&mk(RENDER_CACHE_MAX_HTML)));
        assert!(!cacheable(&mk(RENDER_CACHE_MAX_HTML + 1)));
    }

    #[test]
    fn percent_decode_cases() {
        assert_eq!(percent_decode("%D0%A6%D0%B5%D0%BB%D1%8C"), "Цель");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%FF"), "%FF");
        assert_eq!(percent_decode("Цель"), "Цель");
    }

    #[test]
    fn resolver_href() {
        let id = Id::new();
        let r = MapResolver {
            links: HashMap::from([("Цель".to_string(), id)]),
        };
        assert_eq!(
            r.wiki("Цель", Some("Мой Раздел")).unwrap().href,
            format!("#/n/{id}#h-мой-раздел")
        );
        assert_eq!(r.wiki("Цель", None).unwrap().href, format!("#/n/{id}"));
        assert!(r.wiki("Нет", None).is_none());
        assert!(r.attachment("0123456789abcdef", "a.png").is_none());
    }
}
