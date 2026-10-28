//! Ссылки знаний на файлы решения (спецификация M4.5 §6.1–6.3; хранилище —
//! [`zam_store::file_refs`]).
//!
//! - **Ссылки записи журнала** — поле запроса `refs: {files: [{path, hash?, commit?}]}`
//!   ([`parse_refs`]): до 64, путь нормализуется (`\` → `/`, `./` и `..` сворачиваются;
//!   абсолютный, выше корня, пустой, длиннее 1000 символов, с управляющими символами — 422
//!   `refs.files[i]: …`), `hash` — blake3 содержимого (64 hex), `commit` — sha git (4–64 hex).
//!   У заметок ссылки — список `refs:` во frontmatter (без хэша; ненормализуемые пути
//!   пропускаются с предупреждением `invalid_ref: <путь>`).
//! - `POST /files/changed {solution, files: [{path, hash}]}` (до 500) → `{stale: N}`:
//!   сообщение клиента `zam` об изменении файлов (тела файлов не передаются); ссылки решения на
//!   эти пути становятся `stale` (хэш другой или ссылка без хэша записана раньше) или `ok`
//!   (хэш совпал). `N` — сколько ссылок этих путей после этого `stale`.
//! - `POST /files/verified {solution, files: [{path, hash?}]}` → `{verified: N}`: «проверено,
//!   знание актуально» — ссылки решения на пути снова `ok` с новым хэшем.
//!
//!   Оба маршрута — право записи в решение (`ro`-токен и участник `ro` — 403); меняются
//!   ссылки базы владельца решения и, у участника, ещё его личной базы (Knowledge со ссылками
//!   на это решение). Ошибка в файле — 422 `files[i]: …`. Маршруты служебные, не в
//!   `memory_routes`: текст памяти они не пишут, `memory_writes` не растёт.
//! - `GET /file-refs?solution=<guid>` (или `X-Zam-Solution`) → `{etag, refs: [{path, hash?,
//!   items: [{id, kind, title, state}]}]}`: индекс для кэша хуков (до 5 записей на путь, новые
//!   первыми; `id` — ссылка `n:`/`i:`/`c:`/…). Участнику — только пространство решения и его
//!   Knowledge. `ETag` — blake3 канонического JSON `{refs}`, `If-None-Match` → 304.
//! - `GET /graph?file=<path>` ([`super::graph`]) и `filters.file` поиска — «что память знает об
//!   этом файле».
use std::collections::BTreeMap;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zam_core::{Id, SolutionGuid, TargetRef};
use zam_store::file_refs::{FILES_MAX, REFS_MAX};
use zam_store::{
    Db, FileRef, PathRefs, SpaceKind, check_commit, check_hash, files_changed, files_verified,
    normalize_ref_path, refs_index, refs_to_path, space_of_kind,
};

use super::entries::unprocessable;
use super::scope::{MemScope, Mode, Role, graph_spaces, space_scope, user_db};
use super::search::SOLUTION_HEADER;
use super::{json_body, query};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Нормализованный путь или 422 `<what>: <причина>`.
fn path_of(raw: &str, what: &str) -> Result<String, ApiError> {
    normalize_ref_path(raw).map_err(|e| unprocessable(format!("{what}: {e}")))
}

/// Необязательная строка поля `key` объекта `o` (`null` — нет).
fn opt_str<'a>(
    o: &'a serde_json::Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<Option<&'a str>, ApiError> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(unprocessable(format!("{what}.{key}: ожидается строка"))),
    }
}

/// Хэш поля `hash` (64 hex) или 422.
fn hash_of(o: &serde_json::Map<String, Value>, what: &str) -> Result<Option<String>, ApiError> {
    opt_str(o, "hash", what)?
        .map(|h| {
            check_hash(h).ok_or_else(|| {
                unprocessable(format!("{what}.hash: blake3 содержимого, 64 hex-символа"))
            })
        })
        .transpose()
}

/// `refs` запроса записи журнала: `{files: [{path, hash?, commit?}]}` → ссылки (повтор пути —
/// первая). `null` и `{}` — пустой набор; ошибки — 422 `refs…`.
pub fn parse_refs(v: &Value) -> Result<Vec<FileRef>, ApiError> {
    let o = match v {
        Value::Null => return Ok(Vec::new()),
        Value::Object(o) => o,
        _ => return Err(unprocessable("refs: ожидается объект {files: [...]}")),
    };
    if let Some(k) = o.keys().find(|k| k.as_str() != "files") {
        return Err(unprocessable(format!("refs.{k}: неизвестное поле")));
    }
    let files = match o.get("files") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(unprocessable("refs.files: ожидается массив")),
    };
    if files.len() > REFS_MAX {
        return Err(unprocessable(format!(
            "refs.files: не больше {REFS_MAX} ссылок"
        )));
    }
    let mut out: Vec<FileRef> = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let what = format!("refs.files[{i}]");
        let o = f.as_object().ok_or_else(|| {
            unprocessable(format!("{what}: ожидается объект {{path, hash?, commit?}}"))
        })?;
        if let Some(k) = o
            .keys()
            .find(|k| !["path", "hash", "commit"].contains(&k.as_str()))
        {
            return Err(unprocessable(format!("{what}.{k}: неизвестное поле")));
        }
        let raw = opt_str(o, "path", &what)?
            .ok_or_else(|| unprocessable(format!("{what}.path: обязательное поле")))?;
        let path = path_of(raw, &format!("{what}.path"))?;
        let hash = hash_of(o, &what)?;
        let commit = opt_str(o, "commit", &what)?
            .map(|c| {
                check_commit(c).ok_or_else(|| {
                    unprocessable(format!("{what}.commit: sha коммита, 4–64 hex-символа"))
                })
            })
            .transpose()?;
        if !out.iter().any(|r| r.path == path) {
            out.push(FileRef { path, hash, commit });
        }
    }
    Ok(out)
}

/// `refs` тела запроса: нет поля — `None`, иначе разобранный набор.
pub fn refs_field(v: Option<&Value>) -> Result<Option<Vec<FileRef>>, ApiError> {
    v.map(parse_refs).transpose()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesReq {
    solution: String,
    files: Vec<Value>,
}

/// GUID решения тела (не GUID — 422).
fn guid_of(raw: &str) -> Result<SolutionGuid, ApiError> {
    raw.trim()
        .parse()
        .map_err(|_| unprocessable("solution: GUID решения (UUID с дефисами)"))
}

/// Файлы сообщения: `(путь, хэш)`; `need_hash` — хэш обязателен. 422 `files[i]: …`.
fn parse_files(
    files: &[Value],
    need_hash: bool,
) -> Result<Vec<(String, Option<String>)>, ApiError> {
    if files.len() > FILES_MAX {
        return Err(unprocessable(format!(
            "files: не больше {FILES_MAX} за запрос"
        )));
    }
    let mut out = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let what = format!("files[{i}]");
        let o = f
            .as_object()
            .ok_or_else(|| unprocessable(format!("{what}: ожидается объект {{path, hash}}")))?;
        if let Some(k) = o.keys().find(|k| !["path", "hash"].contains(&k.as_str())) {
            return Err(unprocessable(format!("{what}.{k}: неизвестное поле")));
        }
        let raw = opt_str(o, "path", &what)?
            .ok_or_else(|| unprocessable(format!("{what}.path: обязательное поле")))?;
        let path = path_of(raw, &what)?;
        let hash = hash_of(o, &what)?;
        if need_hash && hash.is_none() {
            return Err(unprocessable(format!("{what}.hash: обязательное поле")));
        }
        out.push((path, hash));
    }
    Ok(out)
}

/// Базы, ссылки которых меняет сообщение: база владельца решения и, если это другой
/// пользователь, личная база пользователя запроса (его Knowledge со ссылками на решение).
async fn report_dbs(st: &AppState, sc: &MemScope) -> Result<Vec<Db>, ApiError> {
    let mut dbs = vec![sc.db.clone()];
    if sc.owner != sc.user.id {
        dbs.push(st.users.get(sc.user.id).await?);
    }
    Ok(dbs)
}

/// Область решения для записи сообщения о файлах (право записи; `ro` — 403).
async fn write_scope(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    solution: &str,
) -> Result<(MemScope, SolutionGuid), ApiError> {
    let guid = guid_of(solution)?;
    let sc = space_scope(st, p, headers, guid.as_str(), Mode::Write).await?;
    Ok((sc, guid))
}

/// `POST /files/changed` → `{stale}`.
pub async fn changed(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let req: FilesReq = json_body(body)?;
    let (sc, guid) = write_scope(&st, &p, &headers, &req.solution).await?;
    let files: Vec<(String, String)> = parse_files(&req.files, true)?
        .into_iter()
        .map(|(p, h)| (p, h.unwrap_or_default()))
        .collect();
    let now = Timestamp::now();
    let mut stale = 0;
    for db in report_dbs(&st, &sc).await? {
        stale += files_changed(&db, guid.as_str(), &files, now).await?;
    }
    Ok(Json(json!({ "stale": stale })))
}

/// `POST /files/verified` → `{verified}`.
pub async fn verified(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let req: FilesReq = json_body(body)?;
    let (sc, guid) = write_scope(&st, &p, &headers, &req.solution).await?;
    let files = parse_files(&req.files, false)?;
    let now = Timestamp::now();
    let mut verified = 0;
    for db in report_dbs(&st, &sc).await? {
        verified += files_verified(&db, guid.as_str(), &files, now).await?;
    }
    Ok(Json(json!({ "verified": verified })))
}

#[derive(Deserialize)]
pub struct IndexQuery {
    solution: Option<String>,
}

/// GUID решения: параметр (не GUID — 400), иначе `X-Zam-Solution`; нет обоих — `None`.
pub(crate) fn query_solution(
    raw: Option<&str>,
    headers: &HeaderMap,
) -> Result<Option<SolutionGuid>, ApiError> {
    if let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) {
        return s
            .parse::<SolutionGuid>()
            .map(Some)
            .map_err(|_| ApiError::BadRequest("solution: GUID решения (UUID с дефисами)".into()));
    }
    Ok(headers
        .get(SOLUTION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<SolutionGuid>().ok()))
}

/// Каноническое представление индекса для `ETag`.
#[derive(Serialize)]
struct Canon<'a> {
    refs: &'a [PathRefs],
}

/// Сливает индексы баз по пути: записи первой базы раньше, до 5 на путь, хэш — первый
/// известный.
fn merge_index(parts: Vec<Vec<PathRefs>>) -> Vec<PathRefs> {
    let mut by: BTreeMap<String, PathRefs> = BTreeMap::new();
    for part in parts {
        for p in part {
            match by.get_mut(&p.path) {
                None => {
                    by.insert(p.path.clone(), p);
                }
                Some(cur) => {
                    if cur.hash.is_none() {
                        cur.hash = p.hash;
                    }
                    for it in p.items {
                        if cur.items.len() >= zam_store::file_refs::INDEX_ITEMS_MAX {
                            break;
                        }
                        cur.items.push(it);
                    }
                }
            }
        }
    }
    by.into_values().collect()
}

/// `If-None-Match` совпадает с `etag` (список значений, слабые `W/`, `*`).
fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().trim_start_matches("W/").trim_matches('"'))
        .any(|t| t == "*" || t == etag)
}

/// `GET /file-refs?solution=` → 200 `{etag, refs}` с `ETag` / 304.
pub async fn index(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<IndexQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let guid = query_solution(q.solution.as_deref(), &headers)?
        .ok_or_else(|| ApiError::BadRequest("нужен solution или X-Zam-Solution".into()))?;
    let sc = space_scope(&st, &p, &headers, guid.as_str(), Mode::Read).await?;
    let spaces = graph_spaces(&p, &sc.db, &sc.space, sc.role).await?;
    let mut parts = vec![refs_index(&sc.db, guid.as_str(), &spaces).await?];
    if sc.owner != sc.user.id {
        // участник: его Knowledge со ссылками на это решение
        let own = st.users.get(sc.user.id).await?;
        let k = space_of_kind(&own, SpaceKind::Knowledge).await?;
        parts.push(refs_index(&own, guid.as_str(), &[k.id]).await?);
    }
    let refs = merge_index(parts);
    let canon = serde_json::to_vec(&Canon { refs: &refs })
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let etag = blake3::hash(&canon).to_hex().to_string();
    let quoted = HeaderValue::from_str(&format!("\"{etag}\""))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut r = if not_modified(&headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(json!({ "etag": etag, "refs": refs })).into_response()
    };
    let h = r.headers_mut();
    h.insert(header::ETAG, quoted);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(r)
}

/// Владельцы ссылок на путь `raw` (нормализуется; ошибка — 422 `file: …`), видимые запросу:
/// в решении `solution` (база владельца, у участника — только пространство решения) или,
/// без решения, в личной базе. Возвращает базу и ссылки (новые первыми).
pub(crate) async fn file_owners(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
    solution: Option<SolutionGuid>,
) -> Result<(Db, Vec<TargetRef>), ApiError> {
    let path = path_of(raw, "file")?;
    let (db, spaces): (Db, Vec<Id>) = match solution {
        Some(g) => {
            let sc = space_scope(st, p, headers, g.as_str(), Mode::Read).await?;
            let spaces = graph_spaces(p, &sc.db, &sc.space, sc.role).await?;
            (sc.db, spaces)
        }
        None => {
            let (_, db) = user_db(st, p, headers).await?;
            let k = space_of_kind(&db, SpaceKind::Knowledge).await?;
            let spaces = graph_spaces(p, &db, &k, Role::Personal).await?;
            (db, spaces)
        }
    };
    let owners = refs_to_path(&db, &path, &spaces).await?;
    Ok((db, owners))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_parsed_and_checked() {
        let h = "a".repeat(64);
        let r = parse_refs(&json!({"files": [
            {"path": "./src/a.rs", "hash": h, "commit": "ABCDEF1"},
            {"path": "src/a.rs"},
            {"path": "b.rs"},
        ]}))
        .unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].path, "src/a.rs");
        assert_eq!(r[0].commit.as_deref(), Some("abcdef1"));
        assert_eq!(r[1], FileRef::path_only("b.rs".into()));
        assert!(parse_refs(&Value::Null).unwrap().is_empty());
        let err = |v: Value| match parse_refs(&v) {
            Err(ApiError::Unprocessable(m)) => m,
            other => panic!("ожидалась 422: {other:?}"),
        };
        assert!(err(json!({"files": [{"path": "../x"}]})).starts_with("refs.files[0].path"));
        assert!(err(json!({"files": [{"path": "a", "hash": "zz"}]})).contains("hash"));
        assert!(err(json!({"files": [{"path": "a", "x": 1}]})).contains("неизвестное поле"));
        assert!(err(json!({"other": []})).contains("refs.other"));
        assert!(err(json!([1])).contains("ожидается объект"));
    }

    #[test]
    fn index_merge_keeps_order_and_cap() {
        let item = |id: &str| zam_store::RefItem {
            id: id.into(),
            kind: "note".into(),
            title: id.into(),
            state: zam_store::RefState::Ok,
        };
        let a = vec![PathRefs {
            path: "b".into(),
            hash: None,
            items: (0..4).map(|i| item(&format!("n:{i}"))).collect(),
        }];
        let b = vec![
            PathRefs {
                path: "a".into(),
                hash: None,
                items: vec![item("n:x")],
            },
            PathRefs {
                path: "b".into(),
                hash: Some("h".into()),
                items: vec![item("n:5"), item("n:6")],
            },
        ];
        let m = merge_index(vec![a, b]);
        assert_eq!(
            m.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(m[1].items.len(), 5);
        assert_eq!(m[1].items[4].id, "n:5");
        assert_eq!(m[1].hash.as_deref(), Some("h"));
    }
}
