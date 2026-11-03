//! Отчёт для человека (задача 3 M5.5, спецификация §5, §8): страница
//! `GET /entries/{id}?format=html`, файлы `GET /entries/{id}/attachments/{sha256}/{имя}`,
//! лента `?kind=report` (миниатюры, без тел).
//!
//! **Страница** — `{entry, html, toc, warnings, files}` только у отчёта (у прочих видов
//! `format=html` — 400). Рендер — общий кэш ([`render::cached`]) с ключом записи
//! `entry:<id>:<rev>`: отчёт неизменен, HTML устаревает только с вики-ссылками (поколение
//! пространства в ключе). Ссылки `_attachments/<sha16>/<имя>` разрешаются **только** по
//! `entry_attachments` этой записи ([`ReportResolver`]) → [`file_url`]; вложения заметок и чужих
//! записей отчёт не показывает. Вики-ссылки — как у заметок (только отрисовка, рёбер нет).
//!
//! **Файлы** — только сессиям WebUI (токену — 404: отчёт агенту не виден, §7) и только живой
//! записи вида `report`, пара (sha256, имя) — именно этой записи, иначе 404. Заголовки — общий
//! [`blobs::serve`]: картинки — `inline`, SVG — `image/svg+xml`, всё — с CSP
//! [`blobs::BLOB_CSP`] (`sandbox`) и `nosniff`, прочие файлы — `attachment`; `ETag` — sha256,
//! `Range`. `?user=` — только администратору (браузер не ставит `X-Zam-User` в `<img src>`).
use std::collections::HashMap;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::response::Response;
use serde_json::{Value, json};
use zam_core::Id;
use zam_markdown::{LinkResolution, Resolver};
use zam_store::{Entry, EntryAttachment, EntryKind, entry_attachment, entry_attachments};

use crate::api::blobs::{self, BlobQuery};
use crate::api::insights::entry_json;
use crate::api::query;
use crate::api::render::{self, Viewer, cache_key, percent_encode, wiki_link};
use crate::api::scope::{EntryScope, Mode, entry_scope, live_entry};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Длина префикса sha256 в ссылке тела (`_attachments/<sha16>/…`).
const SHA16: usize = 16;

/// URL файла отчёта для WebUI (относительный, как у блобов заметок): `api/v1/entries/<id>/
/// attachments/<sha256>/<имя в percent-encoding>`; администратору от имени другого —
/// `?user=<владелец>`.
pub fn file_url(entry: Id, sha256: &str, filename: &str, viewer: Viewer) -> String {
    let mut url = format!(
        "api/v1/entries/{entry}/attachments/{sha256}/{}",
        percent_encode(filename)
    );
    if let Viewer::AdminFor(u) = viewer {
        url.push_str(&format!("?user={u}"));
    }
    url
}

/// Файл отчёта в ответе: поля [`EntryAttachment`] и `url`.
fn file_json(f: &EntryAttachment, viewer: Viewer) -> Value {
    let mut v = json!(f);
    v["url"] = json!(file_url(f.entry_id, &f.sha256, &f.filename, viewer));
    v
}

/// Резолвер страницы отчёта: файлы — только этой записи, вики-ссылки — заранее разрешённые.
struct ReportResolver {
    /// `(sha16, имя)` → URL файла записи.
    files: HashMap<(String, String), String>,
    /// Цель вики-ссылки → id заметки ([`render::resolve_wiki`]).
    wiki: HashMap<String, Id>,
}

impl ReportResolver {
    fn new(files: &[EntryAttachment], wiki: HashMap<String, Id>, viewer: Viewer) -> Self {
        let files = files
            .iter()
            .map(|f| {
                (
                    (f.sha256[..SHA16].to_string(), f.filename.clone()),
                    file_url(f.entry_id, &f.sha256, &f.filename, viewer),
                )
            })
            .collect();
        ReportResolver { files, wiki }
    }
}

impl Resolver for ReportResolver {
    fn wiki(&self, target: &str, anchor: Option<&str>) -> Option<LinkResolution> {
        wiki_link(&self.wiki, target, anchor)
    }

    fn attachment(&self, sha16: &str, filename: &str) -> Option<String> {
        self.files
            .get(&(sha16.to_ascii_lowercase(), filename.to_string()))
            .cloned()
    }
}

/// `GET /entries/{id}?format=html` живой записи `sc` для зрителя `viewer`: только отчёт (иначе
/// 400) → `{entry, html, toc, warnings, files}`.
pub async fn get_html(
    st: &AppState,
    sc: &EntryScope,
    viewer: Viewer,
) -> Result<Json<Value>, ApiError> {
    let e = &sc.entry;
    if e.kind != EntryKind::Report {
        return Err(ApiError::BadRequest(
            "format=html — только у отчёта (kind: report)".into(),
        ));
    }
    let files = entry_attachments(&sc.db, e.id).await?;
    // отчёт неизменен: ключ — запись и ревизия; поколение пространства — для вики-ссылок
    let key = cache_key(
        st,
        &format!("entry:{}:{}", e.id, e.rev),
        sc.space.id,
        sc.owner,
        viewer,
    );
    let r = render::cached(st, key, async {
        let wiki = render::resolve_wiki(&sc.db, sc.space.id, &e.body).await?;
        let resolver = ReportResolver::new(&files, wiki, viewer);
        render::render_with(e.body.clone(), resolver).await
    })
    .await?;
    Ok(Json(json!({
        "entry": entry_json(&sc.db, e).await?,
        "html": r.html,
        "toc": r.toc,
        "warnings": r.warnings,
        "files": files.iter().map(|f| file_json(f, viewer)).collect::<Vec<_>>(),
    })))
}

/// `GET /entries/{id}/attachments/{sha256}/{filename}[?user=]` — файл отчёта (см. модуль).
pub async fn attachment(
    State(st): State<AppState>,
    p: Principal,
    method: Method,
    mut headers: HeaderMap,
    Path((id, sha, filename)): Path<(String, String, String)>,
    q: Result<Query<BlobQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    // отчёт — только человеку: токену маршрут «не существует» (до разбора и чтения базы)
    if !p.is_human() {
        return Err(ApiError::NotFound);
    }
    let q = query(q)?;
    blobs::check_sha(&sha)?;
    blobs::admin_user(&p, &mut headers, &q)?;
    let sc = entry_scope(&st, &p, &headers, &id, Mode::Read).await?;
    live_entry(&sc)?;
    if sc.entry.kind != EntryKind::Report {
        return Err(ApiError::NotFound);
    }
    let f = entry_attachment(&sc.db, sc.entry.id, &sha[..SHA16], &filename)
        .await?
        .filter(|f| f.sha256 == sha)
        .ok_or(ApiError::NotFound)?;
    blobs::serve(
        &st,
        sc.owner,
        &f.sha256,
        &f.filename,
        &f.mime,
        method,
        &headers,
    )
    .await
}

/// Элементы ленты отчётов: без `body` (до 50 тел по `note_max_bytes` ленте не нужны — тело
/// читает страница `format=html`), поле `first_image` (`{sha256, filename, mime, size, url}`
/// первой картинки тела с файлом записи или `null`; у удалённых — `null`, их файлы не
/// отдаются). `out[i]` — JSON `items[i]`.
pub async fn report_feed(
    db: &zam_store::Db,
    items: &[Entry],
    out: &mut [Value],
    viewer: Viewer,
) -> Result<(), ApiError> {
    let ids: Vec<Id> = items
        .iter()
        .filter(|e| e.kind == EntryKind::Report && e.deleted_at.is_none())
        .map(|e| e.id)
        .collect();
    let firsts = zam_store::first_images(db, &ids).await?;
    for (e, v) in items.iter().zip(out.iter_mut()) {
        if e.kind != EntryKind::Report {
            continue;
        }
        if let Some(o) = v.as_object_mut() {
            o.remove("body");
        }
        v["first_image"] = firsts
            .get(&e.id)
            .map_or(Value::Null, |f| file_json(f, viewer));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(entry: Id, name: &str) -> EntryAttachment {
        EntryAttachment {
            entry_id: entry,
            sha256: "ab".repeat(32),
            filename: name.into(),
            mime: "image/png".into(),
            size: 1,
            created_at: jiff::Timestamp::now(),
        }
    }

    #[test]
    fn resolver_only_entry_files_and_admin_user() {
        let (id, owner) = (Id::new(), Id::new());
        let files = [att(id, "кот 1.png")];
        let r = ReportResolver::new(&files, HashMap::new(), Viewer::Admin);
        let sha = "ab".repeat(32);
        assert_eq!(
            r.attachment(&"AB".repeat(8), "кот 1.png").as_deref(),
            Some(
                format!("api/v1/entries/{id}/attachments/{sha}/%D0%BA%D0%BE%D1%82%201.png")
                    .as_str()
            )
        );
        assert!(r.attachment(&"ab".repeat(8), "other.png").is_none());
        assert!(r.attachment(&"cd".repeat(8), "кот 1.png").is_none());
        assert!(r.wiki("Нет", None).is_none());
        let r = ReportResolver::new(&files, HashMap::new(), Viewer::AdminFor(owner));
        assert!(
            r.attachment(&"ab".repeat(8), "кот 1.png")
                .unwrap()
                .ends_with(&format!("?user={owner}"))
        );
    }
}
