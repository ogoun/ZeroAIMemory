//! WebUI: статические ассеты из `web/dist` (встроены через memory-serve) и SPA fallback.
use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use memory_serve::{Asset, MemoryServe};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::error::not_found;

/// `index.html` из `web/dist` (или заглушка), см. build.rs.
const INDEX_TEMPLATE: &str = include_str!(env!("ZAM_WEB_INDEX"));

/// Политика для HTML: без inline-скриптов, фреймы запрещены.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'";

/// Готовый index.html (с подставленным base href); дёшево клонируется.
#[derive(Clone)]
pub struct Index(Arc<str>);

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Подставляет `<base href="{base_path}/">`: заменяет имеющийся `<base href="/"`, иначе вставляет в `<head>`.
fn render_index(template: &str, base_path: &str) -> String {
    let tag = format!(r#"<base href="{}/">"#, escape_attr(base_path));
    if let Some(start) = template.find(r#"<base href="/""#) {
        let end = template[start..]
            .find('>')
            .map_or(template.len(), |i| start + i + 1);
        format!("{}{tag}{}", &template[..start], &template[end..])
    } else if let Some(i) = template.find("<head>") {
        let at = i + "<head>".len();
        format!("{}{tag}{}", &template[..at], &template[at..])
    } else {
        template.to_string()
    }
}

impl Index {
    pub fn new(base_path: &str) -> Self {
        Self(render_index(INDEX_TEMPLATE, base_path).into())
    }

    /// Ответ 200 с HTML и заголовками безопасности.
    pub fn response(&self) -> Response {
        html(self)
    }
}

fn static_header(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(name, HeaderValue::from_static(value))
}

/// Маршруты ассетов (кроме index.html). Пусто, если `web/dist` не собран.
pub fn assets_router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    let sets: &[(&str, &[Asset])] = include!(concat!(env!("OUT_DIR"), "/memory_serve_assets.rs"));
    let router = match sets.first() {
        Some((_, assets)) if !assets.is_empty() => MemoryServe::new(assets)
            .index_file(None)
            .fallback(None)
            .into_router(),
        _ => Router::new(),
    };
    router
        .layer(static_header(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(static_header(header::REFERRER_POLICY, "same-origin"))
}

/// Для путей вне `/api` и `/mcp` — index.html (SPA), для остальных — problem+json 404.
pub async fn fallback(index: Index, method: Method, uri: Uri) -> Response {
    let p = uri.path();
    let reserved = ["/api", "/mcp", "/downloads"]
        .iter()
        .any(|r| p == *r || p.strip_prefix(r).is_some_and(|t| t.starts_with('/')));
    // Путь к файлу (точка в последнем сегменте) — не SPA-маршрут: нет ассета, значит 404.
    let file_like = p.rsplit('/').next().is_some_and(|l| l.contains('.'));
    if reserved || file_like || !matches!(method, Method::GET | Method::HEAD) {
        return not_found().await.into_response();
    }
    html(&index)
}

fn html(index: &Index) -> Response {
    let mut r = (StatusCode::OK, index.0.to_string()).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_href_replaced_in_both_spellings() {
        for t in [
            r#"<head><base href="/"><title>x</title>"#,
            r#"<head><base href="/" /><title>x</title>"#,
        ] {
            assert_eq!(
                render_index(t, "/m"),
                r#"<head><base href="/m/"><title>x</title>"#
            );
        }
    }

    #[test]
    fn base_href_inserted_when_missing() {
        assert_eq!(
            render_index("<head><title>x</title>", ""),
            r#"<head><base href="/"><title>x</title>"#
        );
    }

    #[test]
    fn base_path_is_escaped() {
        let out = render_index(r#"<base href="/">"#, r#"/a"><script>"#);
        assert_eq!(out, r#"<base href="/a&quot;&gt;&lt;script&gt;/">"#);
    }
}
