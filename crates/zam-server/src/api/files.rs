//! Ссылки знаний на файлы решения (спецификация M4.5 §6.1–6.3; хранилище —
//! [`zam_store::file_refs`]).
//!
//! - **Ссылки записи журнала** — поле запроса `refs: {files: [{path, hash?, commit?}]}`
//!   ([`parse_refs`]): до 64, путь нормализуется (`\` → `/`, `./` и `..` сворачиваются;
//!   абсолютный, выше корня, пустой, длиннее 1000 символов, с управляющими символами — 422
//!   `refs.files[i]: …`), `hash` — blake3 содержимого (64 hex), `commit` — sha git (4–64 hex).
//!   M5.6 §8: ссылка на символ — `symbol` (qual, ≤ 512, без `/`) и `symbol_hash` (`body_hash`,
//!   16 hex) или `path` вида `a.rs#qual` без `symbol` ([`zam_code::split_ref`]); повтор
//!   `(путь, символ)` — первая. У заметок ссылки — список `refs:` во frontmatter (`путь` или
//!   `путь#qual`, без хэша; ненормализуемые пропускаются с предупреждением `invalid_ref`).
//! - `POST /files/changed {solution, files: [{path, hash, symbols?: [{qual, body_hash}]}]}` (до
//!   500) → `{stale: N}`: сообщение клиента `zam` об изменении файлов (тела файлов не
//!   передаются); ссылки решения на эти пути становятся `stale` (хэш другой) или `ok` (хэш
//!   совпал); у ссылки без хэша присланный хэш становится ориентиром (`ok`). Ссылки на символ
//!   при `symbols` судятся по телу (`symbol_changed`, `symbol_removed`; список — полный разбор
//!   файла), без `symbols` — по хэшу файла. `N` — сколько ссылок этих путей после этого `stale`.
//! - `POST /files/verified {solution, files: [{path, hash?, symbols?}]}` → `{verified: N}`:
//!   «проверено, знание актуально» — ссылки решения на пути снова `ok` с новым хэшем (у
//!   символьных — хэш тела из `symbols`).
//!
//!   Оба маршрута — право записи в решение (`ro`-токен и участник `ro` — 403); меняются
//!   ссылки базы владельца решения и, у участника, ещё его личной базы (Knowledge со ссылками
//!   на это решение). Ошибка в файле — 422 `files[i]: …`. Маршруты служебные, не в
//!   `memory_routes`: текст памяти они не пишут, `memory_writes` не растёт.
//! - `GET /file-refs?solution=<guid>` (или `X-Zam-Solution`) → `{etag, refs: [{path, hash?,
//!   items: [{id, kind, title, state, symbol?}]}]}`: индекс для кэша хуков (до 5 записей на
//!   путь, символьные, затем новые первыми; `id` — ссылка `n:`/`i:`/`c:`/…). Участнику —
//!   только пространство решения и его Knowledge. `ETag` — blake3 канонического JSON `{refs}`,
//!   `If-None-Match` → 304.
//! - `GET /graph?file=<path>` ([`super::graph`]; `путь#qual` — путь символа) и `filters.file`
//!   поиска — «что память знает об этом файле».
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
use zam_code::{QUAL_MAX_CHARS, SymbolHash, split_ref};
use zam_core::{Id, SolutionGuid, TargetRef};
use zam_store::file_refs::{FILES_MAX, REFS_MAX};
use zam_store::{
    ChangedFile, Db, FileRef, PathRefs, SpaceKind, check_commit, check_hash, files_changed,
    files_verified, normalize_ref_path, refs_index, refs_to_path, space_of_kind,
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

/// Символ ссылки (`qual`, M5.6 §8): 1..=512 символов без управляющих и без `/` (иначе `путь#qual`
/// разбирался бы как путь), `#` — только суффикс одноимённых `#N`, у файла кода `path` (`None` —
/// путь не известен: хэши символов сообщения) — то же правило, что у [`zam_code::split_ref`]
/// (ревью задачи 6 M5.6, M4); иначе 422 `<what>: …`.
fn check_symbol(q: &str, path: Option<&str>, what: &str) -> Result<(), ApiError> {
    if q.is_empty() {
        return Err(unprocessable(format!("{what}: пустая строка")));
    }
    if q.chars().count() > QUAL_MAX_CHARS {
        return Err(unprocessable(format!(
            "{what}: длиннее {QUAL_MAX_CHARS} символов"
        )));
    }
    if q.chars().any(char::is_control) {
        return Err(unprocessable(format!("{what}: управляющие символы")));
    }
    if q.contains('/') {
        return Err(unprocessable(format!("{what}: «/» в qual недопустим")));
    }
    if let Some(p) = path
        && zam_code::lang_of(p).is_none()
    {
        return Err(unprocessable(format!(
            "{what}: символ — только у файла кода (а не у «{p}»)"
        )));
    }
    // то же правило, что у разбора `путь#qual`; итог от пути кода не зависит (`qual` без `/`),
    // поэтому — короткий путь кода: длинный путь отвергнет нормализация, а не этот разбор
    let raw = format!("a.rs#{q}");
    if split_ref(&raw) != ("a.rs", Some(q)) {
        return Err(unprocessable(format!(
            "{what}: недопустимый qual (`#` — только суффикс одноимённых `#N`; якорь строки \
             `L10`, `L10-L20` — не символ)"
        )));
    }
    Ok(())
}

/// Хэш тела символа: 16 hex (в нижний регистр) или `None`.
fn body_hash_of(s: &str) -> Option<String> {
    let s = s.trim();
    (s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// Хэши тел символов файла `{what}.symbols: [{qual, body_hash}]` (`/files/changed`,
/// `/files/verified`, `/code/impact`): нет поля или `null` — `None`; больше `max` символов —
/// у `lenient` (сообщения хуков и `zam refs`) `None`: файл судится по хэшу, как неполный разбор
/// (§12 «превышение — partial»; ревью задачи 6, I2), иначе 422; неверный `qual` или
/// `body_hash` — 422 `<what>.symbols…`. Список считается полным: не
/// найденный в нём символ ссылки — `symbol_removed` (клиент шлёт символы, только если разбор
/// полный — `zam_code::symbol_hashes`; неполный — без поля, ссылки судятся по хэшу файла).
pub(crate) fn symbol_hashes_of(
    v: Option<&Value>,
    max: u32,
    lenient: bool,
    what: &str,
) -> Result<Option<Vec<SymbolHash>>, ApiError> {
    let a = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(unprocessable(format!("{what}.symbols: ожидается массив"))),
    };
    if a.len() > max as usize {
        if lenient {
            return Ok(None);
        }
        return Err(unprocessable(format!(
            "{what}.symbols: не больше {max} символов (code.max_symbols_per_file)"
        )));
    }
    let mut out = Vec::with_capacity(a.len());
    for (j, s) in a.iter().enumerate() {
        let w = format!("{what}.symbols[{j}]");
        let so = s
            .as_object()
            .ok_or_else(|| unprocessable(format!("{w}: ожидается объект {{qual, body_hash}}")))?;
        if let Some(k) = so
            .keys()
            .find(|k| !["qual", "body_hash"].contains(&k.as_str()))
        {
            return Err(unprocessable(format!("{w}.{k}: неизвестное поле")));
        }
        let qual = opt_str(so, "qual", &w)?
            .ok_or_else(|| unprocessable(format!("{w}.qual: обязательное поле")))?;
        check_symbol(qual, None, &format!("{w}.qual"))?;
        let body_hash = opt_str(so, "body_hash", &w)?
            .and_then(body_hash_of)
            .ok_or_else(|| unprocessable(format!("{w}.body_hash: 16 hex-символов")))?;
        out.push(SymbolHash {
            qual: qual.to_string(),
            body_hash,
        });
    }
    Ok(Some(out))
}

/// `refs` запроса записи журнала: `{files: [{path, symbol?, hash?, symbol_hash?, commit?}]}` →
/// ссылки (повтор `(путь, символ)` — первая). `path` вида `a.rs#qual` без `symbol` — ссылка на
/// символ ([`zam_code::split_ref`], M5.6 §8); `symbol` вместе с `путь#qual` — 422. `null` и
/// `{}` — пустой набор; ошибки — 422 `refs…`.
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
            unprocessable(format!(
                "{what}: ожидается объект {{path, symbol?, hash?, symbol_hash?, commit?}}"
            ))
        })?;
        if let Some(k) = o
            .keys()
            .find(|k| !["path", "symbol", "hash", "symbol_hash", "commit"].contains(&k.as_str()))
        {
            return Err(unprocessable(format!("{what}.{k}: неизвестное поле")));
        }
        let raw = opt_str(o, "path", &what)?
            .ok_or_else(|| unprocessable(format!("{what}.path: обязательное поле")))?;
        let (raw_path, symbol) = match opt_str(o, "symbol", &what)? {
            Some(s) => {
                if split_ref(raw.trim()).1.is_some() {
                    return Err(unprocessable(format!(
                        "{what}.symbol: символ уже задан в path (путь#qual)"
                    )));
                }
                check_symbol(s, Some(raw.trim()), &format!("{what}.symbol"))?;
                (raw, Some(s.to_string()))
            }
            None => {
                let (p, s) = split_ref(raw.trim());
                (p, s.map(str::to_string))
            }
        };
        let path = path_of(raw_path, &format!("{what}.path"))?;
        let hash = hash_of(o, &what)?;
        let symbol_hash = opt_str(o, "symbol_hash", &what)?
            .map(|h| {
                if symbol.is_none() {
                    return Err(unprocessable(format!(
                        "{what}.symbol_hash: только у ссылки на символ (symbol или путь#qual)"
                    )));
                }
                body_hash_of(h).ok_or_else(|| {
                    unprocessable(format!("{what}.symbol_hash: body_hash символа, 16 hex"))
                })
            })
            .transpose()?;
        let commit = opt_str(o, "commit", &what)?
            .map(|c| {
                check_commit(c).ok_or_else(|| {
                    unprocessable(format!("{what}.commit: sha коммита, 4–64 hex-символа"))
                })
            })
            .transpose()?;
        if !out.iter().any(|r| r.path == path && r.symbol == symbol) {
            out.push(FileRef {
                symbol,
                hash,
                symbol_hash,
                commit,
                ..FileRef::path_only(path)
            });
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

/// Файл сообщения: путь, хэш и хэши тел символов, если присланы.
type MsgFile = (String, Option<String>, Option<Vec<SymbolHash>>);

/// Файлы сообщения `[{path, hash, symbols?}]`; `need_hash` — хэш обязателен; символов в файле —
/// не больше `max_symbols`. 422 `files[i]: …`.
fn parse_files(
    files: &[Value],
    need_hash: bool,
    max_symbols: u32,
) -> Result<Vec<MsgFile>, ApiError> {
    if files.len() > FILES_MAX {
        return Err(unprocessable(format!(
            "files: не больше {FILES_MAX} за запрос"
        )));
    }
    let mut out = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let what = format!("files[{i}]");
        let o = f.as_object().ok_or_else(|| {
            unprocessable(format!("{what}: ожидается объект {{path, hash, symbols?}}"))
        })?;
        if let Some(k) = o
            .keys()
            .find(|k| !["path", "hash", "symbols"].contains(&k.as_str()))
        {
            return Err(unprocessable(format!("{what}.{k}: неизвестное поле")));
        }
        let raw = opt_str(o, "path", &what)?
            .ok_or_else(|| unprocessable(format!("{what}.path: обязательное поле")))?;
        let path = path_of(raw, &what)?;
        let hash = hash_of(o, &what)?;
        if need_hash && hash.is_none() {
            return Err(unprocessable(format!("{what}.hash: обязательное поле")));
        }
        let symbols = symbol_hashes_of(o.get("symbols"), max_symbols, true, &what)?;
        out.push((path, hash, symbols));
    }
    Ok(out)
}

/// Базы и пространства, ссылки которых меняет сообщение: в базе владельца решения — видимые
/// запросу (участнику — только пространство решения), у участника — ещё его Knowledge.
pub(crate) async fn report_dbs(
    st: &AppState,
    p: &Principal,
    sc: &MemScope,
) -> Result<Vec<(Db, Vec<Id>)>, ApiError> {
    let spaces = graph_spaces(p, &sc.db, &sc.space, sc.role).await?;
    let mut dbs = vec![(sc.db.clone(), spaces)];
    if sc.owner != sc.user.id {
        let own = st.users.get(sc.user.id).await?;
        let k = space_of_kind(&own, SpaceKind::Knowledge).await?;
        dbs.push((own, vec![k.id]));
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
    let max = st.cfg.code.max_symbols_per_file;
    let files: Vec<ChangedFile> = parse_files(&req.files, true, max)?
        .into_iter()
        .map(|(path, h, symbols)| ChangedFile {
            path,
            hash: h.unwrap_or_default(),
            symbols,
        })
        .collect();
    let now = Timestamp::now();
    let mut stale = 0;
    for (db, spaces) in report_dbs(&st, &p, &sc).await? {
        stale += files_changed(&db, guid.as_str(), &spaces, &files, now).await?;
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
    let files = parse_files(&req.files, false, st.cfg.code.max_symbols_per_file)?;
    let now = Timestamp::now();
    let mut verified = 0;
    for (db, spaces) in report_dbs(&st, &p, &sc).await? {
        verified += files_verified(&db, guid.as_str(), &spaces, &files, now).await?;
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
pub(crate) fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
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

/// Владельцы ссылок на путь `raw` (нормализуется; ошибка — 422 `file: …`; `путь#qual` — знания
/// об этом символе, затем о файле, без ссылок на другие символы, M5.6 §8), видимые запросу: в решении `solution`
/// (база владельца, у участника — только пространство решения) или, без решения, в личной
/// базе. Возвращает базу и ссылки (символьные первыми, затем новые первыми).
pub(crate) async fn file_owners(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
    solution: Option<SolutionGuid>,
) -> Result<(Db, Vec<TargetRef>), ApiError> {
    let (raw_path, symbol) = split_ref(raw.trim());
    let path = path_of(raw_path, "file")?;
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
    let owners = refs_to_path(&db, &path, symbol, &spaces).await?;
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

    /// Финальное ревью ветки M5.6, M1 и C1: якорь строки GitHub — путь целиком, как в 0.0.8;
    /// `symbol` якорем — 422; ссылка `a.rs#` × 200 000 — 422 «длиннее» (путь), быстро.
    #[test]
    fn refs_line_anchor_is_path_and_hostile_ref_is_too_long() {
        let r = parse_refs(&json!({"files": [{"path": "src/a.rs#L10-L20"}]})).unwrap();
        assert_eq!(r, [FileRef::path_only("src/a.rs#L10-L20".into())]);
        let err = |v: Value| match parse_refs(&v) {
            Err(ApiError::Unprocessable(m)) => m,
            other => panic!("ожидалась 422: {other:?}"),
        };
        let m = err(json!({"files": [{"path": "src/a.rs", "symbol": "L10"}]}));
        assert!(m.contains("якорь строки"), "{m}");
        let huge = "a.rs#".repeat(200_000);
        let t = std::time::Instant::now();
        let m = err(json!({"files": [{"path": huge}]}));
        assert!(
            m.contains("длиннее"),
            "{}",
            m.chars().take(80).collect::<String>()
        );
        let code = format!("{huge}x.rs");
        let m = err(json!({"files": [{"path": code, "symbol": "foo"}]}));
        assert!(
            m.contains("длиннее"),
            "{}",
            m.chars().take(80).collect::<String>()
        );
        assert!(
            t.elapsed() < std::time::Duration::from_millis(500),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn index_merge_keeps_order_and_cap() {
        let item = |id: &str| zam_store::RefItem {
            id: id.into(),
            kind: "note".into(),
            title: id.into(),
            state: zam_store::RefState::Ok,
            symbol: None,
            space: None,
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
