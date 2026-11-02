//! WebUI: статические ассеты из `web/dist` (встроены через memory-serve), SPA fallback и
//! документ просмотрщика PDF `/viewer/pdf` со своей CSP (M5).
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use memory_serve::{Asset, MemoryServe};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::error::not_found;

/// `index.html` из `web/dist` (или заглушка), см. build.rs.
const INDEX_TEMPLATE: &str = include_str!(env!("ZAM_WEB_INDEX"));

/// `viewer-pdf.html` — документ просмотрщика PDF (pdf.js) из `web/dist` (или заглушка).
const VIEWER_PDF_TEMPLATE: &str = include_str!(env!("ZAM_WEB_VIEWER_PDF"));

/// Политика для HTML: без inline-скриптов, фреймы запрещены.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'";

/// Политика только документа `/viewer/pdf` (M5, спецификация §5 и §11 п. 3): главная [`CSP`]
/// и `'wasm-unsafe-eval'` (декодеры JPEG 2000, JBIG2 и ICC pdf.js — WebAssembly), воркер pdf.js
/// (`worker-src 'self' blob:`), встраивание только в WebUI (`frame-ancestors 'self'`), без
/// отправки форм. `'unsafe-eval'` нет: JavaScript PDF и `eval` в документе не исполняются.
/// Директива `sandbox` (раунд правок 1 задачи 9) — непрозрачный источник документа и при прямом
/// открытии, и без атрибута `sandbox` фрейма: pdf.js не видит ни DOM WebUI, ни cookie сессии, а
/// запросы к API уходят как межсайтовые (без CORS — отказ). Скрипты и всплывающие окна
/// (ссылки PDF — http/https/mailto/tel, noopener) разрешены; навигация WebUI, формы,
/// модальные окна — нет.
pub const VIEWER_PDF_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; \
worker-src 'self' blob:; frame-ancestors 'self'; base-uri 'self'; form-action 'none'; \
sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox";

/// Готовая страница WebUI (с подставленным base href) и её CSP; дёшево клонируется.
#[derive(Clone)]
pub struct Index(Arc<str>, &'static str);

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
    /// Главная страница (SPA).
    pub fn new(base_path: &str) -> Self {
        Self(render_index(INDEX_TEMPLATE, base_path).into(), CSP)
    }

    /// Документ просмотрщика PDF (`/viewer/pdf`) — со своей CSP [`VIEWER_PDF_CSP`].
    pub fn viewer_pdf(base_path: &str) -> Self {
        Self(
            render_index(VIEWER_PDF_TEMPLATE, base_path).into(),
            VIEWER_PDF_CSP,
        )
    }

    /// Ответ 200 с HTML и заголовками безопасности.
    pub fn response(&self) -> Response {
        html(self)
    }
}

fn static_header(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(name, HeaderValue::from_static(value))
}

/// Публичная статика сборки, которую читает документ просмотрщика PDF с непрозрачным источником
/// (модули и стили страницы — `crossorigin`, данные и воркер pdf.js — `fetch`/`import()`):
/// `Access-Control-Allow-Origin: *` только здесь. Пользовательских данных в этих каталогах нет —
/// только файлы `web/dist` (код, стили, CMap, WebAssembly, ICC); API и блобы — никогда.
const PUBLIC_STATIC: [&str; 2] = ["/assets/", "/pdfjs/"];

async fn cors_public_static(req: Request, next: Next) -> Response {
    let public = PUBLIC_STATIC
        .iter()
        .any(|p| req.uri().path().starts_with(p));
    let mut r = next.run(req).await;
    if public {
        r.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    r
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
        .layer(axum::middleware::from_fn(cors_public_static))
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
        HeaderValue::from_static(index.1),
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
    fn viewer_csp_is_main_plus_wasm_and_worker() {
        // главная политика + только оговорённые директивы (Review Focus 5 M5)
        let main: Vec<&str> = CSP.split("; ").collect();
        let viewer: Vec<&str> = VIEWER_PDF_CSP.split("; ").collect();
        assert!(!CSP.contains("wasm-unsafe-eval"));
        assert!(!VIEWER_PDF_CSP.contains("'unsafe-eval'"));
        let replaced = ["script-src 'self'", "frame-ancestors 'none'"];
        for d in main.iter().filter(|d| !replaced.contains(d)) {
            assert!(viewer.contains(d), "в CSP просмотрщика нет {d:?}");
        }
        let extra: Vec<&str> = viewer
            .iter()
            .copied()
            .filter(|d| !main.contains(d))
            .collect();
        assert_eq!(
            extra,
            [
                "script-src 'self' 'wasm-unsafe-eval'",
                "worker-src 'self' blob:",
                "frame-ancestors 'self'",
                "form-action 'none'",
                "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox"
            ]
        );
    }

    #[test]
    fn base_path_is_escaped() {
        let out = render_index(r#"<base href="/">"#, r#"/a"><script>"#);
        assert_eq!(out, r#"<base href="/a&quot;&gt;&lt;script&gt;/">"#);
    }
}
