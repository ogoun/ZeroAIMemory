//! Превью вложений (M5 §5, задача 8): `GET /api/v1/previews/{sha256}?kind=sheet|thumb|text`.
//!
//! Путь не `/blobs/{sha}/preview`: он совпал бы с `/blobs/{sha}/{filename}` (карта кода M5).
//!
//! - `kind=sheet&sheet=<N>&from=<строка>&rows=<1..=1000>` — JSON страницы листа: лист — **номер**
//!   в порядке книги (с 1), а не имя; `sheets` — `{no, name, secret, ref_no}`: имя через сканер
//!   секретов (`mask_text`), `secret` — имя замаскировано, `ref_no` — номер `sheet:#N` того же
//!   листа в `/read` и карточках поиска (порядок листов среди единиц, `api::doc_locator`) — у
//!   каждого листа с единицами извлечения, когда документ извлечён (иначе `null`); `header`,
//!   `rows`, `first_row`,
//!   `total_rows`, `cols`, `warnings`. Сессии WebUI ячейки — как в файле (превью показывает файл,
//!   как скачивание блоба); токену — с маской секретов (ниже).
//! - `kind=thumb&page=<N>&px=<1..=1024>` — PNG: страница PDF (pdfium) или изображение PNG, JPEG,
//!   GIF, WebP (вид — по сигнатуре; SVG — не изображение); по умолчанию `page=1`, `px=256`.
//! - `kind=text` — JSON: PPTX — `{format: "slides", slides: [{slide, title, text, notes}]}`;
//!   текстовый файл — `{format: "html", syntax, html, highlighted, highlight_skipped,
//!   truncated}`: HTML syntect с классами `hl-*` (`/api/v1/render/highlight.css`), только
//!   `<span class>` и экранированный текст; читается начало файла до [`TEXT_MAX`] байт,
//!   подсвечиваются первые 64 КиБ; разметка больше 40 × исходник (`[` в JSON и т. п.) — весь
//!   текст экранирован, `highlight_skipped: "too_large"`. HTML от рабочего процесса проверяется
//!   (`zam_markdown::is_highlight_html`): иной — отброшен, текст экранируется здесь.
//!
//! **Токену** (агент: задача 5 M7.1, спецификация M7 §4.3.1 п. 7 — **[решение координатора]**)
//! текст и ячейки — с `‹secret›` вместо значений ([`crate::api::agent_view`]): лист — маска
//! заголовка и строк страницы при ответе (кэш страницы общий, сырой); слайды PPTX — маска текста;
//! HTML подсветки — по маскированному тексту файла (маска всего начала файла до подсветки: по
//! разметке маска не работает), в своём файле кэша `m<RULES_VERSION>-…`. ETag маски — свой (у
//! листа — с суффиксом `.m<RULES_VERSION>`). Миниатюры и сам файл (`GET /blobs/…`) — как есть.
//!
//! Неудача построения (422) помнится: свойство файла (`parse_error`, `too_large`, `unsupported`)
//! — 5 мин, сбой процесса (`parse_timeout`, `parse_crash`, `parse_oom`) — 45 с
//! ([`crate::docs::preview::failure_ttl`]); повтор отвечает сразу, без нового процесса. Испорченный файл кэша удаляется и строится заново.
//!
//! **Права — как у скачивания блоба:** пара (sha, узел) в доступном принципалу пространстве
//! (по токену — без Human; участник решения — только пространство решения; `?user=` —
//! администратору). Отличие от скачивания: недоступное (в том числе неодобренный запрос доступа к
//! решению) — тот же 404, что и несуществующий sha, без 403 `solution-access`: превью не
//! подтверждает существование файла.
//!
//! **Файлы отчётов** (M6 §16 п. 15, миниатюры ленты «Отчёты») — права как у файла отчёта
//! (`GET /entries/{id}/attachments/…`): только сессии человека — в своей базе и в решениях с
//! одобренным доступом, только у живой записи вида `report`. Токену отчёты не видны (M5.5 §7),
//! поэтому и превью их файла — 404, как несуществующий sha: вопрос «есть ли в отчёте файл с таким
//! содержимым» токену не отвечается.
//!
//! **Ответ:** `ETag` — sha, ключ кэша (параметры, версии, отпечаток лимитов), у листа — и
//! отпечаток номеров `ref_no`; `If-None-Match` → 304. `Cache-Control: private,
//! max-age=3600`, `X-Content-Type-Options: nosniff`, CSP `sandbox; default-src 'none'`.
//! Ошибки построения (файл не того вида, повреждён, лист не найден, сбой или срок рабочего
//! процесса) — 422 с кодом в `detail`; pdfium недоступен — 503.

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use zam_core::Id;
use zam_docs::{DocError, DocKind, Limits, Locator};
use zam_markdown::mask_text;
use zam_markdown::secrets::RULES_VERSION;
use zam_store::blobs::is_sha256;
use zam_store::{
    BlobStore, Db, blob_meta_by_sha, blob_meta_by_sha_in_space, report_blob_meta_by_sha,
};

use super::agent_view;
use super::blobs::NoCompress;
use super::doc_locator::Sheets;
use super::query;
use super::scope::{foreign_solutions, owner_db, skip_foreign, user_db};
use crate::app::AppState;
use crate::auth::{Principal, USER_HEADER};
use crate::docs::protocol::{WireHtml, WireSheetPage};
use crate::docs::{ParseFailure, PreviewKey};
use crate::error::ApiError;

/// CSP ответа превью: ни скриптов, ни загрузок, песочница.
pub const PREVIEW_CSP: &str = "sandbox; default-src 'none'";
/// Кэширование в браузере: ответ по ключу неизменен, права перепроверяются через час.
const PREVIEW_CACHE: &str = "private, max-age=3600";
/// Строк в окне листа по умолчанию и наибольшее.
const ROWS_DEFAULT: u32 = 200;
const ROWS_MAX: u32 = 1000;
/// Миниатюра по умолчанию, пикселей большей стороны.
const PX_DEFAULT: u32 = 256;
/// Начало текстового файла для `kind=text`, байт.
pub const TEXT_MAX: u64 = 1024 * 1024;
/// Запас за [`TEXT_MAX`] для маски агенту, байт: ключ на границе обрезки маска видит целиком
/// (иначе его начало вышло бы префиксом), выдача — всё равно до [`TEXT_MAX`] ([`masked_head`]).
const MASK_LOOKAHEAD: u64 = 4 * 1024;
/// Начало файла для определения вида (как `zam_docs::sniff_file`).
const HEAD: u64 = 64 * 1024 + 16;

#[derive(Deserialize, Default)]
pub struct PreviewQuery {
    kind: Option<String>,
    sheet: Option<u32>,
    from: Option<u32>,
    rows: Option<u32>,
    page: Option<u32>,
    px: Option<u32>,
    /// Пользователь (только для администратора, как у скачивания блоба).
    user: Option<String>,
}

fn bad(m: &str) -> ApiError {
    ApiError::BadRequest(m.into())
}

/// Параметры превью → ключ (только проверенные числа).
fn key_of(q: &PreviewQuery) -> Result<PreviewKey, ApiError> {
    let pos = |v: Option<u32>, d: u32, name: &str| match v {
        None => Ok(d),
        Some(0) => Err(bad(&format!("{name}: от 1"))),
        Some(n) => Ok(n),
    };
    match q.kind.as_deref() {
        Some("sheet") => {
            let rows = pos(q.rows, ROWS_DEFAULT, "rows")?;
            if rows > ROWS_MAX {
                return Err(bad(&format!("rows: от 1 до {ROWS_MAX}")));
            }
            Ok(PreviewKey::Sheet {
                no: pos(q.sheet, 1, "sheet")?,
                from: pos(q.from, 1, "from")?,
                rows,
            })
        }
        Some("thumb") => {
            let px = pos(q.px, PX_DEFAULT, "px")?;
            if px > zam_docs::thumb::THUMB_MAX_PX {
                return Err(bad(&format!(
                    "px: от 1 до {}",
                    zam_docs::thumb::THUMB_MAX_PX
                )));
            }
            Ok(PreviewKey::Thumb {
                page: pos(q.page, 1, "page")?,
                px,
            })
        }
        Some("text") => Ok(PreviewKey::Text),
        _ => Err(bad("kind: sheet, thumb или text")),
    }
}

/// Файл блоба, доступный принципалу.
struct Located {
    owner: Id,
    db: Db,
    size: u64,
    filename: String,
}

/// Владелец и база с привязкой блоба `sha` к узлу, доступному принципалу: личная база (Human —
/// только сессии человека), затем базы владельцев решений — только их пространство и только при
/// одобренном доступе. Сессии человека — ещё и файл живого отчёта (`entry_attachments`) в тех же
/// базах и пространствах; токену файлы отчётов не существуют. Всё недоступное —
/// [`ApiError::NotFound`], как несуществующее.
async fn locate(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    sha: &str,
) -> Result<Located, ApiError> {
    let (user, db) = user_db(st, p, headers).await?;
    // отчёт — только человеку (как `reports::view::attachment`)
    let reports = p.is_human();
    let mut found = blob_meta_by_sha(&db, sha, p.is_human()).await?;
    if found.is_none() && reports {
        found = report_blob_meta_by_sha(&db, sha, None).await?;
    }
    if let Some((_, size, filename)) = found {
        return Ok(Located {
            owner: user.id,
            db,
            size,
            filename,
        });
    }
    for (info, role) in foreign_solutions(st, p, &user).await? {
        // неодобренный или отозванный доступ — как отсутствие (без 403 solution-access)
        if role.is_err() {
            continue;
        }
        let Some(odb) = owner_db(st, &info).await else {
            continue;
        };
        let mut found = blob_meta_by_sha_in_space(&odb, sha, info.space_id).await;
        if reports && matches!(found, Ok(None)) {
            found = report_blob_meta_by_sha(&odb, sha, Some(info.space_id)).await;
        }
        match found {
            Ok(Some((_, size, filename))) => {
                return Ok(Located {
                    owner: info.owner_id,
                    db: odb,
                    size,
                    filename,
                });
            }
            Ok(None) => {}
            Err(e) => skip_foreign(&info, &e),
        }
    }
    Err(ApiError::NotFound)
}

/// Начало файла: до `n` байт и признак, что файл длиннее.
async fn read_head(path: &std::path::Path, n: u64) -> Result<(Vec<u8>, bool), ApiError> {
    let f = tokio::fs::File::open(path)
        .await
        .map_err(|e| ApiError::Internal(format!("файл блоба: {e}")))?;
    let mut buf = Vec::new();
    f.take(n + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| ApiError::Internal(format!("файл блоба: {e}")))?;
    let more = buf.len() as u64 > n;
    buf.truncate(n as usize);
    Ok((buf, more))
}

/// Вид документа файла (`sniff_file` с лимитами `[docs]`: zip — по каталогу без чтения файла).
async fn sniff(
    path: std::path::PathBuf,
    name: String,
    limits: Limits,
) -> Result<Option<DocKind>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let f = std::io::BufReader::new(std::fs::File::open(&path)?);
        zam_docs::sniff_file(f, &name, &limits)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("вид файла: {e}")))?
    .map_err(|e| ApiError::Internal(format!("вид файла: {e}")))
}

fn unfit(what: &str) -> ApiError {
    ApiError::Unprocessable(format!("превью {what} недоступно для этого файла"))
}

/// Сбой построения → ответ: код без подробностей разборщика (область ошибки — `DocError::area`).
fn failed(st: &AppState, f: ParseFailure, pdf: bool) -> ApiError {
    match f {
        ParseFailure::Doc(DocError::Unsupported)
            if pdf && st.previews.pool().pdfium_error().is_some() =>
        {
            ApiError::Unavailable("pdfium недоступен: превью PDF не строятся".into())
        }
        ParseFailure::Doc(DocError::Internal(m)) => {
            ApiError::Internal(format!("превью: внутренняя ошибка разбора: {m}"))
        }
        ParseFailure::Doc(e @ DocError::Malformed(_)) => {
            ApiError::Unprocessable(format!("превью не построено: parse_error: {}", e.area()))
        }
        ParseFailure::Doc(DocError::TooLarge) => {
            ApiError::Unprocessable("превью не построено: too_large".into())
        }
        ParseFailure::Doc(DocError::Unsupported) => {
            ApiError::Unprocessable("превью не построено: unsupported".into())
        }
        f => ApiError::Unprocessable(format!("превью не построено: {}", f.reason())),
    }
}

fn etag_hits(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

/// Ответ превью с заголовками безопасности и кэширования (304 — без тела).
fn respond(body: Option<Vec<u8>>, ctype: &'static str, etag: &str) -> Response {
    let mut r = match body {
        Some(b) => (StatusCode::OK, Body::from(b)).into_response(),
        None => StatusCode::NOT_MODIFIED.into_response(),
    };
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PREVIEW_CSP),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(PREVIEW_CACHE),
    );
    h.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("ETag из hex, цифр и ASCII ключа"),
    );
    if ctype == "image/png" {
        r.extensions_mut().insert(NoCompress);
    }
    r
}

/// `GET /api/v1/previews/{sha256}?kind=…[&user=]`.
pub async fn preview(
    State(st): State<AppState>,
    p: Principal,
    mut headers: HeaderMap,
    Path(sha): Path<String>,
    q: Result<Query<PreviewQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    if !is_sha256(&sha) {
        return Err(bad("sha256: 64 символа hex в нижнем регистре"));
    }
    let key = key_of(&q)?;
    if p.is_admin()
        && let Some(u) = q.user.as_deref()
    {
        let v = HeaderValue::from_str(u).map_err(|_| bad("некорректный user"))?;
        headers.insert(USER_HEADER, v);
    }
    let loc = locate(&st, &p, &headers, &sha).await?;
    let path = BlobStore::new(&st.data_dir.blobs_dir(), loc.owner).path_of(&sha);
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        tracing::warn!(sha256 = %sha, "файл блоба отсутствует при записи в attachments");
        return Err(ApiError::NotFound);
    }
    let svc = st.previews.clone();
    // агенту — маска текста и ячеек (миниатюра — картинка, как есть)
    let masked = agent_view::masks(&p) && !matches!(key, PreviewKey::Thumb { .. });
    let raw_name = svc.file_name(&key, &loc.filename);
    // вид с маской — свой ETag; у текста — и свой файл кэша (HTML строится по маске)
    let tag_name = if masked {
        format!("m{RULES_VERSION}-{raw_name}")
    } else {
        raw_name.clone()
    };
    let name = if masked && key == PreviewKey::Text {
        tag_name.clone()
    } else {
        raw_name
    };
    let base_tag = format!(
        "\"{sha}.{}\"",
        tag_name.rsplit_once('.').map_or(&*tag_name, |(n, _)| n)
    );
    let ctype = match key {
        PreviewKey::Thumb { .. } => "image/png",
        _ => "application/json",
    };
    let sheet = matches!(key, PreviewKey::Sheet { .. });
    // лист: ответ зависит ещё и от номеров `ref_no` — ETag после сборки
    if !sheet && etag_hits(&headers, &base_tag) {
        return Ok(respond(None, ctype, &base_tag));
    }
    // файл кэша проверяется: испорченный удаляется и строится заново (ревью, M-3)
    let cached = match svc.cache().get(loc.owner, &sha, &name).await {
        Some(b) if cache_valid(&key, &b) => Some(b),
        Some(_) => {
            tracing::warn!(sha256 = %sha, file = %name, "файл кэша превью испорчен — удалён");
            svc.cache().remove(loc.owner, &sha, &name).await;
            None
        }
        None => None,
    };
    if !sheet && let Some(b) = cached {
        return Ok(respond(Some(b), ctype, &base_tag));
    }
    let built = match cached {
        Some(b) => Built {
            data: b,
            cache: false,
        },
        None => {
            // недавняя неудача того же построения — без нового процесса (ревью, M-1)
            if let Some(detail) = svc.failed_recently(loc.owner, &sha, &name) {
                return Err(ApiError::Unprocessable(detail));
            }
            let _permit = svc.permit().await;
            match build(&st, &loc, &path, &key, masked).await {
                Ok(b) => b,
                Err(ApiError::Unprocessable(detail)) => {
                    svc.remember_failure(loc.owner, &sha, &name, detail.clone());
                    return Err(ApiError::Unprocessable(detail));
                }
                Err(e) => return Err(e),
            }
        }
    };
    if built.cache {
        svc.cache().put(loc.owner, &sha, &name, built.data.clone());
    }
    match key {
        PreviewKey::Sheet { no, .. } => {
            let page: WireSheetPage = serde_json::from_slice(&built.data)
                .map_err(|e| ApiError::Internal(format!("страница листа превью: {e}")))?;
            let (body, refs) = sheet_json(&loc, &sha, no, page).await;
            // агенту — ячейки с маской (каждая ячейка — свой текст); до 1000 строк листа — в пуле
            // при объёме больше 64 КиБ (круг правок 2 задачи 5 M7.1)
            let body = agent_view::view_json_owned(&p, body, &["header", "rows"]).await?;
            let etag = match refs {
                Some(r) => format!("{}.n{r}\"", base_tag.trim_end_matches('"')),
                None => base_tag,
            };
            if etag_hits(&headers, &etag) {
                return Ok(respond(None, ctype, &etag));
            }
            let body = serde_json::to_vec(&body).expect("JSON сериализуется");
            Ok(respond(Some(body), ctype, &etag))
        }
        _ => Ok(respond(Some(built.data), ctype, &base_tag)),
    }
}

/// Файл кэша цел: лист — страница разбирается, текст — JSON (HTML подсветки — снова через
/// [`zam_markdown::is_highlight_html`]: файл в каталоге превью мог записать не сервер), миниатюра —
/// сигнатура PNG.
fn cache_valid(key: &PreviewKey, b: &[u8]) -> bool {
    match key {
        PreviewKey::Sheet { .. } => serde_json::from_slice::<WireSheetPage>(b).is_ok(),
        PreviewKey::Text => match serde_json::from_slice::<serde_json::Value>(b) {
            Ok(v) if v["format"] == "html" => v["html"]
                .as_str()
                .is_some_and(zam_markdown::is_highlight_html),
            Ok(_) => true,
            Err(_) => false,
        },
        PreviewKey::Thumb { .. } => b.starts_with(b"\x89PNG\r\n\x1a\n"),
    }
}

/// Построенное превью: байты ответа (у листа — сырая страница) и можно ли его кэшировать.
struct Built {
    data: Vec<u8>,
    cache: bool,
}

/// Строит превью в рабочем процессе; `masked` — текст агенту с маской секретов (лист маскируется
/// при ответе, [`sheet_json`]).
async fn build(
    st: &AppState,
    loc: &Located,
    path: &std::path::Path,
    key: &PreviewKey,
    masked: bool,
) -> Result<Built, ApiError> {
    let svc = &st.previews;
    let limits = svc.limits().clone();
    let pool = svc.pool();
    let too_large = || ApiError::Unprocessable("превью не построено: too_large".into());
    let kind = sniff(path.to_path_buf(), loc.filename.clone(), limits.clone()).await?;
    match *key {
        PreviewKey::Sheet { no, from, rows } => {
            let Some(k @ (DocKind::Xlsx | DocKind::Xls | DocKind::Ods | DocKind::Csv)) = kind
            else {
                return Err(unfit("листа"));
            };
            if loc.size > limits.max_bytes {
                return Err(too_large());
            }
            let bytes = read_all(path).await?;
            let page = pool
                .sheet_page(k, bytes, no, from, rows)
                .await
                .map_err(|f| failed(st, f, false))?;
            // частичная по сроку страница — не в кэш
            let cache = !page.warnings.iter().any(|w| w.contains("timeout_ms"));
            let data = serde_json::to_vec(&page).expect("JSON сериализуется");
            Ok(Built { data, cache })
        }
        PreviewKey::Thumb { page, px } => {
            let (head, _) = read_head(path, HEAD).await?;
            let image = zam_docs::sniff_image(&head);
            if kind != Some(DocKind::Pdf) && image.is_none() {
                return Err(unfit("миниатюры"));
            }
            if loc.size > limits.max_bytes {
                return Err(too_large());
            }
            let bytes = read_all(path).await?;
            let png = match image {
                Some(_) => {
                    if page != 1 {
                        return Err(ApiError::Unprocessable(
                            "превью не построено: у изображения одна страница".into(),
                        ));
                    }
                    pool.image_thumb(bytes, px)
                        .await
                        .map_err(|f| failed(st, f, false))?
                }
                None => pool
                    .thumb(bytes, page, px)
                    .await
                    .map_err(|f| failed(st, f, true))?,
            };
            Ok(Built {
                data: png,
                cache: true,
            })
        }
        PreviewKey::Text => {
            let value = if kind == Some(DocKind::Pptx) {
                if loc.size > limits.max_bytes {
                    return Err(too_large());
                }
                let bytes = read_all(path).await?;
                let x = pool
                    .extract(DocKind::Pptx, bytes)
                    .await
                    .map_err(|f| failed(st, f, false))?;
                let mut v = slides_json(x);
                if masked {
                    agent_view::mask_fields(
                        &mut v,
                        &["slides.title", "slides.text", "slides.notes"],
                    );
                }
                v
            } else {
                // агенту — с запасом за границей: маска по нему, выдача — до `TEXT_MAX`
                let read = if masked {
                    TEXT_MAX + MASK_LOOKAHEAD
                } else {
                    TEXT_MAX
                };
                let (head, more) = read_head(path, read).await?;
                let probe = &head[..head.len().min(TEXT_MAX as usize)];
                let textual = matches!(kind, None | Some(DocKind::Text | DocKind::Csv))
                    && zam_docs::is_text(probe)
                    && zam_docs::sniff_image(probe).is_none();
                if !textual {
                    return Err(unfit("текста"));
                }
                // агенту — подсветка маскированного текста: маска всего прочитанного до разметки
                // (в HTML значение разрезано тегами подсветки, по нему маска не работает)
                let (head, more) = if masked {
                    masked_head(head, more).await?
                } else {
                    (head, more)
                };
                let head = Bytes::from(head);
                let h = pool
                    .highlight(head.clone(), loc.filename.clone(), more)
                    .await
                    .map_err(|f| failed(st, f, false))?;
                let h = checked_html(h, &head, more);
                json!({
                    "kind": "text",
                    "format": "html",
                    "syntax": h.syntax,
                    "html": h.html,
                    "highlighted": h.highlighted,
                    "highlight_skipped": h.skipped,
                    "truncated": more,
                })
            };
            let data = serde_json::to_vec(&value).expect("JSON сериализуется");
            Ok(Built { data, cache: true })
        }
    }
}

/// HTML подсветки от рабочего процесса — только того вида, что строит `zam_markdown`
/// ([`zam_markdown::is_highlight_html`]): WebUI вставляет его через `{@html}`, а процесс разбирает
/// недоверенный файл и мог быть захвачен. Иной HTML отбрасывается — исходный текст экранируется
/// здесь, без подсветки (финальное ревью M5, I-3).
fn checked_html(h: WireHtml, head: &[u8], truncated: bool) -> WireHtml {
    if zam_markdown::is_highlight_html(&h.html) {
        return h;
    }
    tracing::warn!(
        component = "doc-worker",
        "HTML подсветки рабочего процесса не прошёл проверку: текст без подсветки"
    );
    let text = crate::docs::worker::source_text(head, truncated).unwrap_or_default();
    WireHtml {
        html: zam_markdown::escape_html(&text),
        syntax: None,
        highlighted: 0,
        skipped: None,
    }
}

/// Начало текстового файла `head` (прочитано до [`TEXT_MAX`] + [`MASK_LOOKAHEAD`], `more` — файл
/// длиннее) с маской секретов и обрезкой до [`TEXT_MAX`] ([`agent_view::mask_head`]: маска — по
/// всему прочитанному, показ — не дальше `TEXT_MAX` исходника и не длиннее `TEXT_MAX`, метка не
/// разрезается); второе — «показано не всё». Текст — как его подсвечивает рабочий процесс (BOM
/// снят, обрезанный последний символ отброшен, неверный UTF-8 — U+FFFD,
/// [`crate::docs::worker::source_text`]); маска — в пуле блокирующих задач (1 МиБ — до 20 мс).
/// Двоичный файл (нулевой байт) — как есть до `TEXT_MAX`: подсветка его отвергнет.
async fn masked_head(head: Vec<u8>, more: bool) -> Result<(Vec<u8>, bool), ApiError> {
    let max = TEXT_MAX as usize;
    agent_view::blocking(
        move || match crate::docs::worker::source_text(&head, more) {
            Some(t) => {
                let (shown, cut) = agent_view::mask_head(&t, max);
                (shown.into_bytes(), cut || more)
            }
            None => {
                let mut head = head;
                let cut = more || head.len() > max;
                head.truncate(max);
                (head, cut)
            }
        },
    )
    .await
}

async fn read_all(path: &std::path::Path) -> Result<Bytes, ApiError> {
    tokio::fs::read(path)
        .await
        .map(Bytes::from)
        .map_err(|e| ApiError::Internal(format!("файл блоба: {e}")))
}

/// Слайды PPTX из единиц извлечения: текст слайда и заметки докладчика по номеру слайда.
fn slides_json(x: zam_docs::Extracted) -> Value {
    const NOTES: &str = "Заметки докладчика";
    let mut slides: Vec<(u32, Option<String>, String, String)> = Vec::new();
    for u in x.units {
        let Locator::Slide(n) = u.locator else {
            continue;
        };
        let notes = u.heading_path.last().is_some_and(|h| h == NOTES);
        if slides.last().is_none_or(|s| s.0 != n) {
            slides.push((n, None, String::new(), String::new()));
        }
        let s = slides.last_mut().expect("слайд добавлен");
        if s.1.is_none() {
            s.1 = u.heading_path.first().filter(|h| *h != NOTES).cloned();
        }
        let dst = if notes { &mut s.3 } else { &mut s.2 };
        if !dst.is_empty() {
            dst.push('\n');
        }
        dst.push_str(&u.text);
    }
    json!({
        "kind": "text",
        "format": "slides",
        "slides": slides.into_iter().map(|(n, title, text, notes)| json!({
            "slide": n, "title": title, "text": text, "notes": notes,
        })).collect::<Vec<_>>(),
        "warnings": x.warnings,
    })
}

/// JSON страницы листа: имена листов через сканер секретов, номера `ref_no` листов `sheet:#N`
/// (у всех листов с единицами извлечения — как считают `/read` и поиск; пустой лист и лист до
/// готовности извлечения — `null`), признак `secret`. Второе значение — отпечаток номеров для
/// ETag: готовность номеров меняет ETag (раунд правок 1 задачи 9, M-3).
async fn sheet_json(
    loc: &Located,
    sha: &str,
    no: u32,
    p: WireSheetPage,
) -> (Value, Option<String>) {
    let secret: Vec<bool> = p.sheets.iter().map(|n| mask_text(n) != *n).collect();
    let refs = match zam_store::blob_sheet_locators(&loc.db, sha).await {
        Ok(l) => Some(Sheets::of(l.iter().map(String::as_str))),
        Err(e) => {
            tracing::warn!(sha256 = %sha, error = %e, "номера листов превью не прочитаны");
            None
        }
    };
    let ref_no = |name: &str| refs.as_ref().and_then(|s| s.no(name));
    let sheets: Vec<Value> = p
        .sheets
        .iter()
        .zip(&secret)
        .enumerate()
        .map(|(i, (n, s))| {
            json!({"no": i + 1, "name": mask_text(n), "secret": s, "ref_no": ref_no(n)})
        })
        .collect();
    let tag = refs.as_ref().map(|_| {
        let nos: Vec<String> = p
            .sheets
            .iter()
            .map(|n| ref_no(n).map_or_else(|| "-".into(), |v| v.to_string()))
            .collect();
        blake3::hash(nos.join(",").as_bytes()).to_hex()[..8].to_string()
    });
    let body = json!({
        "kind": "sheet",
        "sheet": no,
        "name": mask_text(&p.sheet),
        "sheets": sheets,
        "header": p.header,
        "rows": p.rows,
        "first_row": p.first_row,
        "total_rows": p.total_rows,
        "cols": p.cols,
        "warnings": p.warnings,
    });
    (body, tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Финальное ревью M5, I-3: HTML подсветки не того вида (захваченный рабочий процесс)
    /// отбрасывается — исходный текст экранирован, без подсветки; верный проходит как есть.
    #[test]
    fn worker_html_checked() {
        let src = "a <b> & 'c'\n".as_bytes();
        let evil = WireHtml {
            html: "<img src=x onerror=alert(1)>".into(),
            syntax: Some("Rust".into()),
            highlighted: 10,
            skipped: None,
        };
        let h = checked_html(evil, src, false);
        assert_eq!(h.html, "a &lt;b&gt; &amp; &#39;c&#39;\n");
        assert_eq!((h.syntax, h.highlighted), (None, 0));
        let sneaky = WireHtml {
            html: "<span class=\"hl-a\" onclick=\"x\">a</span>".into(),
            syntax: None,
            highlighted: 0,
            skipped: None,
        };
        assert!(!checked_html(sneaky, src, false).html.contains("onclick"));
        let good = WireHtml::from(zam_markdown::highlight_source(
            "x.rs",
            "fn main() {}\n",
            1024,
        ));
        assert_eq!(checked_html(good.clone(), b"fn main() {}\n", false), good);
    }

    /// HTML подсветки из файла кэша превью проверяется так же, как ответ рабочего процесса
    /// (повторное финальное ревью M5, Minor 1): чужой HTML — файл испорчен.
    #[test]
    fn cached_text_html_checked() {
        let ok = br#"{"format":"html","html":"<span class=\"hl-a\">a</span>"}"#;
        let evil = br#"{"format":"html","html":"<img src=x onerror=alert(1)>"}"#;
        assert!(cache_valid(&PreviewKey::Text, ok));
        assert!(!cache_valid(&PreviewKey::Text, evil));
        assert!(!cache_valid(&PreviewKey::Text, br#"{"format":"html"}"#));
        assert!(cache_valid(
            &PreviewKey::Text,
            br#"{"format":"slides","slides":[]}"#
        ));
        assert!(!cache_valid(&PreviewKey::Text, b"not json"));
    }
}
