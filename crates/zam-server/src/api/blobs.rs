//! Вложения (§6.5, §12): `POST /blobs` и `GET /blobs/{sha256}/{filename}`.
//!
//! **Загрузка.** Multipart: сначала поле `node` (id живой заметки), затем `file`
//! с именем файла. Доступ — как запись в пространство узла; файл ложится в каталог блобов
//! владельца базы узла (узел общего решения — к владельцу решения, §11.6). Файл пишется потоком
//! (`BlobStore::put`), в памяти целиком не держится; больше
//! `limits.blob_max_bytes` → 413. MIME — `mime_guess` по имени, иначе тип части,
//! иначе `application/octet-stream`. Ответ 201 `{sha256, link, mime, size}`.
//!
//! **Отдача.** Пара (sha256, имя) должна быть привязана к узлу, который принципал
//! может читать (по токену — не в Human), иначе 404: сначала личная база, затем базы
//! владельцев чужих решений — только узлы пространства решения (отозванный или
//! неодобренный доступ — 403 `solution-access`). `?user=` выбирает пользователя
//! только для администратора (браузер не ставит `X-Zam-User` в `<img src>`);
//! для токена игнорируется, как и `X-Zam-User`. Файл отдаёт `ServeFile`
//! (`Range`/206, `If-Modified-Since`); `ETag` — `"<sha256>"` (содержимое неизменно),
//! `If-None-Match` проверяется здесь же. Заголовки безопасности — на любом ответе:
//! `nosniff`, CSP `sandbox`, `Content-Disposition` (`inline` только для безопасных
//! для показа типов). Ответы помечаются [`NoCompress`]: сжатие ломает `Range` и
//! бесполезно для медиа.
use axum::Json;
use axum::body::Body;
use axum::extract::multipart::{Field, MultipartError, MultipartRejection};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::TryStreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::io::StreamReader;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use zam_core::Id;
use zam_store::blobs::{check_filename, is_sha256};
use zam_store::{BlobStore, NodeKind, StoreError, attach, blob_meta, blob_meta_in_space};

use super::query;
use super::scope::{Mode, foreign_solutions, live, node_scope, user_db};
use crate::app::AppState;
use crate::auth::{Principal, USER_HEADER};
use crate::error::ApiError;

/// CSP ответа с блобом: документ (HTML, SVG) открывается в песочнице без скриптов.
pub const BLOB_CSP: &str =
    "sandbox; default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'";
/// Кэширование блоба: содержимое по адресу неизменно.
const BLOB_CACHE: &str = "private, max-age=31536000, immutable";
/// Предел длины значения поля `node`, байт (id — 26 символов).
const NODE_FIELD_MAX: usize = 128;

/// Метка ответа, который не сжимается (см. `app::router`).
#[derive(Debug, Clone, Copy)]
pub struct NoCompress;

/// Обрыв потока тела из-за предела `DefaultBodyLimit`.
#[derive(Debug)]
struct BodyTooLarge;
impl std::fmt::Display for BodyTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("тело запроса превышает предел")
    }
}
impl std::error::Error for BodyTooLarge {}

/// Ошибка разбора multipart клиента (не сервера).
#[derive(Debug)]
struct ClientStream(String);
impl std::fmt::Display for ClientStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ClientStream {}

fn stream_err(e: MultipartError) -> std::io::Error {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        std::io::Error::other(BodyTooLarge)
    } else {
        std::io::Error::new(std::io::ErrorKind::InvalidData, ClientStream(e.body_text()))
    }
}

fn multipart_err(e: MultipartError) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::PayloadTooLarge("тело запроса превышает предел".into())
    } else {
        ApiError::BadRequest(format!("некорректный multipart: {}", e.body_text()))
    }
}

/// Ошибка записи блоба → ответ.
fn put_err(e: StoreError, limit: u64) -> ApiError {
    match e {
        StoreError::BadInput(m) if m == "too large" => {
            ApiError::PayloadTooLarge(format!("вложение больше предела {limit} байт"))
        }
        StoreError::Io(io) if io.get_ref().is_some_and(|r| r.is::<BodyTooLarge>()) => {
            ApiError::PayloadTooLarge(format!("вложение больше предела {limit} байт"))
        }
        StoreError::Io(io) if io.get_ref().is_some_and(|r| r.is::<ClientStream>()) => {
            ApiError::BadRequest(format!("некорректный multipart: {io}"))
        }
        other => other.into(),
    }
}

async fn next_field(mp: &mut Multipart) -> Result<Option<Field<'_>>, ApiError> {
    mp.next_field().await.map_err(multipart_err)
}

/// Значение короткого текстового поля без чтения лишнего в память.
async fn small_text(mut f: Field<'_>, max: usize) -> Result<String, ApiError> {
    let mut buf = Vec::new();
    while let Some(chunk) = f.chunk().await.map_err(multipart_err)? {
        buf.extend_from_slice(&chunk);
        if buf.len() > max {
            return Err(ApiError::BadRequest("поле node слишком длинное".into()));
        }
    }
    String::from_utf8(buf).map_err(|_| ApiError::BadRequest("поле node не UTF-8".into()))
}

/// MIME: по имени, иначе тип части (если разбирается), иначе octet-stream.
pub fn pick_mime(filename: &str, part: Option<&str>) -> String {
    if let Some(m) = mime_guess::from_path(filename).first() {
        return m.essence_str().to_string();
    }
    part.and_then(|p| p.parse::<mime_guess::mime::Mime>().ok())
        .map(|m| m.essence_str().to_string())
        .unwrap_or_else(|| "application/octet-stream".into())
}

/// `POST /blobs` → 201 `{sha256, link, mime, size}`.
pub async fn upload(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    mp: Result<Multipart, MultipartRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut mp = mp.map_err(|e| ApiError::BadRequest(format!("ожидается multipart: {e}")))?;
    let f = next_field(&mut mp)
        .await?
        .ok_or_else(|| ApiError::BadRequest("нет поля node".into()))?;
    if f.name() != Some("node") {
        return Err(ApiError::BadRequest(
            "первым должно идти поле node, затем file".into(),
        ));
    }
    let node = small_text(f, NODE_FIELD_MAX).await?;
    let sc = node_scope(&st, &p, &headers, node.trim(), Mode::Write).await?;
    live(&sc)?;
    if sc.node.kind != NodeKind::Note {
        return Err(ApiError::BadRequest(
            "вложение привязывается только к заметке".into(),
        ));
    }
    let f = next_field(&mut mp)
        .await?
        .ok_or_else(|| ApiError::BadRequest("нет поля file".into()))?;
    if f.name() != Some("file") {
        return Err(ApiError::BadRequest(
            "после node ожидается поле file".into(),
        ));
    }
    let filename = f
        .file_name()
        .map(str::to_string)
        .ok_or_else(|| ApiError::BadRequest("у поля file нет имени файла".into()))?;
    check_filename(&filename)?;
    let mime = pick_mime(&filename, f.content_type());
    let limit = st.cfg.limits.blob_max_bytes as u64;
    let store = BlobStore::new(&st.data_dir.blobs_dir(), sc.owner);
    let reader = StreamReader::new(f.map_err(stream_err));
    let (sha, size) = store
        .put(reader, limit)
        .await
        .map_err(|e| put_err(e, limit))?;
    let link = attach(&sc.db, sc.node.id, &sha, size, &filename, &mime).await?;
    st.render_gen.bump_user(sc.owner);
    tracing::info!(node = %sc.node.id, sha256 = %sha, size, "вложение загружено");
    Ok((
        StatusCode::CREATED,
        Json(json!({ "sha256": sha, "link": link, "mime": mime, "size": size })),
    ))
}

#[derive(Deserialize, Default)]
pub struct BlobQuery {
    /// Пользователь (только для администратора).
    user: Option<String>,
}

/// Показывается в браузере сам (`inline`); остальное — только скачивание.
fn inline_ok(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    (m.starts_with("image/") && m != "image/svg+xml")
        || m.starts_with("video/")
        || m.starts_with("audio/")
        || m == "application/pdf"
}

/// RFC 5987 `attr-char`; остальное — `%XX` от байтов UTF-8.
fn rfc5987(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `Content-Disposition` для имени и типа.
pub fn disposition(mime: &str, filename: &str) -> String {
    if inline_ok(mime) {
        "inline".into()
    } else {
        format!("attachment; filename*=UTF-8''{}", rfc5987(filename))
    }
}

fn if_none_match_hits(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

/// Заголовки безопасности и кэширования любого ответа с блобом.
fn decorate(r: &mut Response, mime: &str, filename: &str, etag: &str) {
    let h = r.headers_mut();
    let ct = HeaderValue::from_str(mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CONTENT_TYPE, ct);
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition(mime, filename))
            .expect("значение из ASCII: тип проверен, имя закодировано"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(BLOB_CSP),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(BLOB_CACHE));
    h.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("sha256 hex — допустимый заголовок"),
    );
    r.extensions_mut().insert(NoCompress);
}

/// Заголовки запроса, которые учитывает `ServeFile`.
const FORWARDED: [header::HeaderName; 4] = [
    header::RANGE,
    header::IF_RANGE,
    header::IF_MODIFIED_SINCE,
    header::IF_UNMODIFIED_SINCE,
];

/// Владелец базы с привязкой пары (sha256, имя) и её MIME: личная база, затем базы
/// владельцев чужих решений в пределах пространства решения.
async fn locate_blob(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    sha: &str,
    filename: &str,
) -> Result<(Id, String), ApiError> {
    let admin = matches!(p, Principal::Admin { .. });
    let (user, db) = user_db(st, p, headers).await?;
    if let Some((mime, _)) = blob_meta(&db, sha, filename, admin).await? {
        return Ok((user.id, mime));
    }
    for (info, role) in foreign_solutions(st, p, &user).await? {
        let odb = st.users.get(info.owner_id).await?;
        if let Some((mime, _)) = blob_meta_in_space(&odb, sha, filename, info.space_id).await? {
            role?;
            return Ok((info.owner_id, mime));
        }
    }
    Err(ApiError::NotFound)
}

/// `GET /blobs/{sha256}/{filename}[?user=]`.
pub async fn download(
    State(st): State<AppState>,
    p: Principal,
    method: Method,
    mut headers: HeaderMap,
    Path((sha, filename)): Path<(String, String)>,
    q: Result<Query<BlobQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    if !is_sha256(&sha) {
        return Err(ApiError::BadRequest(
            "sha256: 64 символа hex в нижнем регистре".into(),
        ));
    }
    let admin = matches!(p, Principal::Admin { .. });
    if admin && let Some(u) = q.user.as_deref() {
        let v = HeaderValue::from_str(u)
            .map_err(|_| ApiError::BadRequest("некорректный user".into()))?;
        headers.insert(USER_HEADER, v);
    }
    let (owner, mime) = locate_blob(&st, &p, &headers, &sha, &filename).await?;
    let path = BlobStore::new(&st.data_dir.blobs_dir(), owner).path_of(&sha);
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        tracing::warn!(sha256 = %sha, "файл блоба отсутствует при записи в attachments");
        return Err(ApiError::NotFound);
    }
    let etag = format!("\"{sha}\"");
    if if_none_match_hits(&headers, &etag) {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        decorate(&mut r, &mime, &filename, &etag);
        return Ok(r);
    }
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
    let mut r = resp.map(Body::new);
    decorate(&mut r, &mime, &filename, &etag);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_rules() {
        for m in ["image/png", "video/mp4", "audio/mpeg", "application/pdf"] {
            assert_eq!(disposition(m, "a"), "inline", "{m}");
        }
        assert_eq!(
            disposition("image/svg+xml", "a b.svg"),
            "attachment; filename*=UTF-8''a%20b.svg"
        );
        assert_eq!(
            disposition("text/html", "\"x\";.html"),
            "attachment; filename*=UTF-8''%22x%22%3B.html"
        );
    }

    #[test]
    fn mime_choice() {
        assert_eq!(pick_mime("a.png", Some("text/plain")), "image/png");
        assert_eq!(pick_mime("a.zamx", Some("text/plain")), "text/plain");
        assert_eq!(
            pick_mime("a.zamx", Some("плохой")),
            "application/octet-stream"
        );
        assert_eq!(pick_mime("a", None), "application/octet-stream");
    }

    #[test]
    fn etag_match() {
        let mut h = HeaderMap::new();
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"x\", W/\"y\""),
        );
        assert!(if_none_match_hits(&h, "\"y\""));
        assert!(!if_none_match_hits(&h, "\"z\""));
    }
}
