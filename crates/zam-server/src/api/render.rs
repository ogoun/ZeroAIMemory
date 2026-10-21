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
//! Ссылки на вложения `_attachments/<sha16>/<имя>` тоже разрешаются заранее
//! (`resolve_attachment`): сначала в пространстве заметки, затем в любом доступном
//! зрителю (по токену — кроме Human; участнику общего решения — только пространство
//! решения, §11.6). URL — `api/v1/blobs/<sha256>/<имя в
//! percent-encoding>`; администратору, действующему от имени другого пользователя,
//! добавляется `?user=<id>` (браузер не передаёт `X-Zam-User` в `<img src>`).
//!
//! **Кэш.** LRU на 512 записей; ключ — `(content_hash, RENDER_VERSION, space_id,
//! user_id, viewer, поколение)`: `user_id` — владелец базы заметки; href вики-ссылок
//! зависят от узлов пространства, URL вложений — ещё и от вида зрителя ([`Viewer`]:
//! токен не видит вложений Human, участник — вложений вне решения, администратору от
//! имени другого нужен `?user=`).
//! Поколение ([`RenderGenerations`]) растёт при создании, переносе, удалении и
//! восстановлении узлов пространства и при загрузке вложения, поэтому появление или
//! исчезновение цели ссылки сразу видно в HTML. Поколение читается до разрешения
//! ссылок: рендер, начатый до изменения, попадает в кэш под старым ключом.
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
    HEADER_ID_PREFIX, LinkKind, LinkResolution, RENDER_VERSION, Rendered, Resolver, attachment_ref,
    highlight_css, parse, render_html, slug,
};
use zam_store::{
    AttachScope, Db, NodeKind, NodePath, find_by_name, get_node_by_path, resolve_attachment,
};

use super::json_body;
use super::scope::{Mode, Role, node_scope};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Предел тела `POST /render`, байт (1 МБ).
pub const RENDER_BODY_MAX: usize = 1024 * 1024;
/// Записей в кэше рендера.
pub const RENDER_CACHE_ENTRIES: usize = 512;
/// HTML крупнее не кэшируется: 512 записей по несколько МБ — слишком много памяти.
pub const RENDER_CACHE_MAX_HTML: usize = 256 * 1024;
/// Не больше стольких разных целей вики-ссылок (и отдельно — вложений) разрешается
/// в одной заметке; остальные считаются битыми (защита от тысяч запросов к БД на
/// один рендер).
const WIKI_TARGETS_MAX: usize = 500;

/// Кто смотрит отрендеренный HTML: от этого зависят URL вложений.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Viewer {
    /// Токен: вложения узлов Human не разрешаются.
    Token,
    /// Токен участника общего решения: только вложения пространства решения.
    Member,
    /// Администратор в своей базе.
    Admin,
    /// Администратор от имени другого пользователя: URL блобов с `?user=<id>`.
    AdminFor(Id),
}

impl Viewer {
    /// Вид зрителя для принципала `p` с ролью `role`, читающего базу пользователя `owner`.
    pub fn of(st: &AppState, p: &Principal, owner: Id, role: Role) -> Viewer {
        match p {
            Principal::Token(_) if matches!(role, Role::Member(_)) => Viewer::Member,
            Principal::Token(_) => Viewer::Token,
            Principal::Admin { .. } if owner == st.admin_id => Viewer::Admin,
            Principal::Admin { .. } => Viewer::AdminFor(owner),
        }
    }
}

/// Контекст разрешения ссылок: пространство заметки и зритель.
#[derive(Debug, Clone, Copy)]
pub struct RenderCtx {
    pub space: Id,
    pub viewer: Viewer,
}

/// `(content_hash, RENDER_VERSION, space_id, user_id, viewer, поколение)`.
pub type RenderKey = (String, u32, Id, Id, Viewer, u64);
/// Кэш HTML заметок.
pub type RenderCache = Arc<Mutex<LruCache<RenderKey, Arc<Rendered>>>>;

/// Поколения разрешения ссылок для ключа кэша рендера.
///
/// Вики-ссылки разрешаются в пространстве заметки, поэтому создание, перенос,
/// удаление и восстановление узлов увеличивают счётчик `(user, space)`. Вложения
/// разрешаются во всех пространствах пользователя — загрузка увеличивает его общий
/// счётчик. Поколение пространства — сумма двух счётчиков: она растёт при любом
/// изменении, и старые записи кэша больше не находятся (их вытеснит LRU).
#[derive(Debug, Default)]
pub struct RenderGenerations {
    /// `(user, Some(space))` — узлы пространства; `(user, None)` — вложения пользователя.
    counters: Mutex<HashMap<(Id, Option<Id>), u64>>,
}

impl RenderGenerations {
    fn bump(&self, key: (Id, Option<Id>)) {
        let mut m = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        *m.entry(key).or_default() += 1;
    }

    /// Изменился состав узлов пространства `space` пользователя `user`.
    pub fn bump_space(&self, user: Id, space: Id) {
        self.bump((user, Some(space)));
    }

    /// У пользователя `user` появилось вложение.
    pub fn bump_user(&self, user: Id) {
        self.bump((user, None));
    }

    /// Текущее поколение пространства; читать до разрешения ссылок.
    pub fn get(&self, user: Id, space: Id) -> u64 {
        let m = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let n = |k| m.get(&k).copied().unwrap_or(0);
        n((user, Some(space))) + n((user, None))
    }
}

pub fn new_cache() -> RenderCache {
    let n = NonZeroUsize::new(RENDER_CACHE_ENTRIES).expect("ненулевой размер");
    Arc::new(Mutex::new(LruCache::new(n)))
}

/// Ключ кэша для текста с хэшем `content_hash` в пространстве `space` пользователя
/// `user` для зрителя `viewer` при текущем поколении ссылок пространства.
pub fn cache_key(
    st: &AppState,
    content_hash: &str,
    space: Id,
    user: Id,
    viewer: Viewer,
) -> RenderKey {
    (
        content_hash.to_string(),
        RENDER_VERSION,
        space,
        user,
        viewer,
        st.render_gen.get(user, space),
    )
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
    ctx: RenderCtx,
    text: String,
) -> Result<Arc<Rendered>, ApiError> {
    if let Some(r) = cache_get(st, &key) {
        return Ok(r);
    }
    let r = Arc::new(render(db, Some(ctx), text).await?);
    if cacheable(&r) {
        cache_put(st, key, Arc::clone(&r));
    }
    Ok(r)
}

/// Кэшируется только HTML не больше [`RENDER_CACHE_MAX_HTML`].
fn cacheable(r: &Rendered) -> bool {
    r.html.len() <= RENDER_CACHE_MAX_HTML
}

/// Рендер текста: ссылки разрешаются в контексте `ctx` (`None` — не разрешаются).
/// Сам рендер (CPU) — в `spawn_blocking`.
pub async fn render(db: &Db, ctx: Option<RenderCtx>, text: String) -> Result<Rendered, ApiError> {
    let (links, attachments) = match ctx {
        Some(c) => (
            resolve_wiki(db, c.space, &text).await?,
            resolve_attachments(db, c, &text).await?,
        ),
        None => (HashMap::new(), HashMap::new()),
    };
    let resolver = MapResolver { links, attachments };
    tokio::task::spawn_blocking(move || render_html(&text, &resolver))
        .await
        .map_err(|e| ApiError::Internal(format!("рендер прерван: {e}")))
}

/// Ссылки на вложения документа → URL блобов (неразрешённые отсутствуют).
/// Ключ — `(sha16 в нижнем регистре, декодированное имя)`, как их передаёт рендер.
async fn resolve_attachments(
    db: &Db,
    ctx: RenderCtx,
    text: &str,
) -> Result<HashMap<(String, String), String>, ApiError> {
    let mut refs: Vec<(String, String)> = parse(text)
        .links
        .into_iter()
        .filter(|l| l.kind != LinkKind::Wiki)
        .filter_map(|l| match attachment_ref(&l.target) {
            Some((sha16, Some(name))) => Some((sha16, name)),
            _ => None,
        })
        .collect();
    refs.sort();
    refs.dedup();
    refs.truncate(WIKI_TARGETS_MAX);
    let scope = AttachScope {
        prefer_space: Some(ctx.space),
        allow_human: !matches!(ctx.viewer, Viewer::Token | Viewer::Member),
        only_space: ctx.viewer == Viewer::Member,
    };
    let mut out = HashMap::new();
    for (sha16, name) in refs {
        if let Some((sha, _mime)) = resolve_attachment(db, &sha16, &name, scope).await? {
            let mut url = format!("api/v1/blobs/{sha}/{}", percent_encode(&name));
            if let Viewer::AdminFor(u) = ctx.viewer {
                url.push_str(&format!("?user={u}"));
            }
            out.insert((sha16, name), url);
        }
    }
    Ok(out)
}

/// Percent-encoding сегмента пути: всё, кроме `A–Z a–z 0–9 - . _ ~`, — `%XX` байтов UTF-8.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
    /// `(sha16, имя)` → URL блоба.
    attachments: HashMap<(String, String), String>,
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

    fn attachment(&self, sha16: &str, filename: &str) -> Option<String> {
        self.attachments
            .get(&(sha16.to_ascii_lowercase(), filename.to_string()))
            .cloned()
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
    let (db, ctx) = match &req.base_node {
        Some(id) => {
            let sc = node_scope(&st, &p, &headers, id, Mode::Read).await?;
            let ctx = RenderCtx {
                space: sc.space.id,
                viewer: Viewer::of(&st, &p, sc.owner, sc.role),
            };
            (Some(sc.db), Some(ctx))
        }
        None => (None, None),
    };
    let r = match db {
        Some(db) => render(&db, ctx, req.md).await?,
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
    fn generations_per_space_and_user() {
        let g = RenderGenerations::default();
        let (u, s1, s2) = (Id::new(), Id::new(), Id::new());
        assert_eq!((g.get(u, s1), g.get(u, s2)), (0, 0));
        g.bump_space(u, s1);
        assert_eq!((g.get(u, s1), g.get(u, s2)), (1, 0));
        // вложение — во всех пространствах пользователя
        g.bump_user(u);
        assert_eq!((g.get(u, s1), g.get(u, s2)), (2, 1));
        // другой пользователь не затронут
        assert_eq!(g.get(Id::new(), s1), 0);
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
        let sha = "0123456789abcdef".to_string();
        let r = MapResolver {
            links: HashMap::from([("Цель".to_string(), id)]),
            attachments: HashMap::from([(
                (sha.clone(), "a.png".to_string()),
                "api/v1/blobs/x/a.png".to_string(),
            )]),
        };
        assert_eq!(
            r.wiki("Цель", Some("Мой Раздел")).unwrap().href,
            format!("#/n/{id}#h-мой-раздел")
        );
        assert_eq!(r.wiki("Цель", None).unwrap().href, format!("#/n/{id}"));
        assert!(r.wiki("Нет", None).is_none());
        assert_eq!(
            r.attachment("0123456789ABCDEF", "a.png").as_deref(),
            Some("api/v1/blobs/x/a.png")
        );
        assert!(r.attachment(&sha, "b.png").is_none());
        assert_eq!(percent_encode("кот 1.png"), "%D0%BA%D0%BE%D1%82%201.png");
    }
}
