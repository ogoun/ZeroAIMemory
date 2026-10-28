//! Раздача клиента `zam` (§13.2, §15.5): `GET /downloads/zam/{os}-{arch}` без аутентификации.
//!
//! Платформы — `windows-x64` и `linux-x64`. Файл ищется в каталоге комплекта
//! (`server.downloads_dir`, по умолчанию — каталог исполняемого файла `zam-server`):
//! сначала `downloads/<платформа>/zam[.exe]` (клиент другой ОС, его кладёт `xtask package`),
//! затем — для платформы самого сервера — `zam[.exe]` рядом с `zam-server`. Нет файла — 404.
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::app::AppState;
use crate::error::ApiError;

/// Платформы раздачи: (имя, файл).
const PLATFORMS: [(&str, &str); 2] = [("windows-x64", "zam.exe"), ("linux-x64", "zam")];

/// Платформа, под которую собран сервер.
pub const OWN_PLATFORM: &str = if cfg!(windows) {
    "windows-x64"
} else {
    "linux-x64"
};

/// Каталог комплекта: настройка или каталог исполняемого файла.
fn bundle_dir(st: &AppState) -> Option<PathBuf> {
    if let Some(d) = &st.cfg.server.downloads_dir {
        return Some(d.clone());
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
}

/// Путь к бинарнику платформы (если есть).
pub fn locate(dir: &Path, platform: &str) -> Option<PathBuf> {
    let (_, file) = PLATFORMS.iter().find(|(p, _)| *p == platform)?;
    let nested = dir.join("downloads").join(platform).join(file);
    if nested.is_file() {
        return Some(nested);
    }
    let own = dir.join(file);
    (platform == OWN_PLATFORM && own.is_file()).then_some(own)
}

/// Заголовки запроса, которые учитывает `ServeFile`.
const FORWARDED: [header::HeaderName; 4] = [
    header::RANGE,
    header::IF_RANGE,
    header::IF_MODIFIED_SINCE,
    header::IF_UNMODIFIED_SINCE,
];

pub async fn zam(
    State(st): State<AppState>,
    UrlPath(platform): UrlPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = bundle_dir(&st)
        .and_then(|d| locate(&d, &platform))
        .ok_or(ApiError::NotFound)?;
    let file = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("zam")
        .to_string();
    let mut req = Request::builder().method(method).uri("/");
    for name in FORWARDED {
        for v in headers.get_all(&name) {
            req = req.header(&name, v);
        }
    }
    let req = req
        .body(Body::empty())
        .map_err(|e| ApiError::Internal(format!("запрос к файлу: {e}")))?;
    let resp = ServeFile::new(&path)
        .oneshot(req)
        .await
        .map_err(|e| ApiError::Internal(format!("отдача файла: {e}")))?;
    let mut r = resp.map(Body::new).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{file}\""))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r.extensions_mut().insert(super::blobs::NoCompress);
    Ok(r)
}
