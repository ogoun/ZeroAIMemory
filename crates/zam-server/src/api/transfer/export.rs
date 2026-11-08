//! `GET /spaces/{space}/export?paths=&kinds=&history=` (M6 §12б.5) — архив zip пространства
//! потоком (zip64, без `Seek`: [`zip::ZipWriter::new_stream`]).
//!
//! **Порядок.** Права (`space_scope`, чтение) → снимок базы одной транзакцией
//! ([`zam_store::export_space`]) → план файлов архива (тексты в памяти, блобы — путями на диске,
//! отсутствующие файлы блобов — в `missing_blobs` описи с WARN) → аудит `space.export` →
//! поток: запись zip в отдельном потоке ОС, байты — через ограниченный канал в тело ответа.
//! Соединение с базой держится только на время снимка: медленный клиент читателя не держит.
//! Ошибка чтения блоба посреди потока обрывает тело (клиент видит незавершённый архив);
//! клиент `zam export` пишет во временный файл и публикует его только после полного ответа.
//!
//! **Права** (§12б.5): что принципал читает, то и выгружается. Токен — без Human (`space_scope`
//! → 404) и без отчётов; сессия WebUI — с ними; участник решения — пространство решения, связи
//! — только внутри него ([`graph_spaces`]); администратор — с `X-Zam-User` (`?user=` у ссылки
//! WebUI, как у вложений). Vault, карта кода, эмбеддинги, кэш выжимок, сессии, корзина в
//! снимок не входят ([`zam_store::export`]).
//!
//! **Аудит** `space.export {space, owner, files, bytes}` пишется до отдачи тела (`bytes` —
//! несжатый объём файлов архива без описи): запись есть и при оборванной загрузке.
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::PathBuf;

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zam_core::Id;
use zam_store::time::fmt_ts;
use zam_store::{BlobStore, EntryKind, ExportSelect, NodePath, SpaceDump, export_space};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use super::archive::{
    EXPORT_FORMAT, ExportFilter, ExportManifest, FOLDERS_FILE, Fingerprint, HISTORY_DIR,
    JOURNALS_DIR, LINKS_FILE, MANIFEST_FILE, attachment_file, file_name_for, front_matter,
};
use crate::api::blobs::{NoCompress, admin_user};
use crate::api::query;
use crate::api::scope::{Mode, graph_spaces, space_scope};
use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Действие аудита.
pub const AUDIT_ACTION: &str = "space.export";
/// Размер куска тела ответа, байт.
const CHUNK: usize = 256 * 1024;
/// Кусков в канале между потоком zip и телом ответа.
const CHANNEL: usize = 8;
/// Файл больше — запись zip64 (без `Seek` размер в заголовке не исправить).
const ZIP64_FROM: u64 = 0xFFFF_0000;

#[derive(Deserialize)]
pub struct ExportQuery {
    /// Префиксы путей через запятую.
    paths: Option<String>,
    /// Виды записей через запятую.
    kinds: Option<String>,
    /// История ревизий (по умолчанию — да).
    history: Option<bool>,
    /// `X-Zam-User` администратора для ссылки WebUI (браузер заголовок не ставит).
    user: Option<String>,
}

/// Фильтр из запроса: пустые элементы списков пропускаются; путь — по правилам [`NodePath`]
/// (без ведущего и конечного `/`), вид — из [`EntryKind`]; ошибка — 400.
fn filter_of(q: &ExportQuery) -> Result<ExportFilter, ApiError> {
    let list = |s: &Option<String>| -> Vec<String> {
        s.as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut paths = Vec::new();
    for p in list(&q.paths) {
        let p = p.trim_matches('/').to_string();
        NodePath::parse(&p).map_err(|e| ApiError::BadRequest(format!("paths: {e}")))?;
        paths.push(p);
    }
    let mut kinds = Vec::new();
    for k in list(&q.kinds) {
        let kind = EntryKind::parse(&k)
            .ok_or_else(|| ApiError::BadRequest(format!("kinds: неизвестный вид «{k}»")))?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    Ok(ExportFilter {
        paths,
        kinds,
        history: q.history.unwrap_or(true),
    })
}

/// Файл архива: текст в памяти или блоб на диске.
enum Item {
    Text {
        name: String,
        data: Vec<u8>,
    },
    Blob {
        name: String,
        path: PathBuf,
        len: u64,
    },
}

impl Item {
    fn len(&self) -> u64 {
        match self {
            Item::Text { data, .. } => data.len() as u64,
            Item::Blob { len, .. } => *len,
        }
    }
}

/// Файлы вложений и отчётов: пара (sha256, имя) → путь в архиве; файл блоба — один раз на пару.
struct Attachments {
    store: BlobStore,
    taken: HashSet<String>,
    files: HashMap<(String, String), String>,
    items: Vec<Item>,
    missing: Vec<String>,
}

impl Attachments {
    /// Путь пары в архиве; нет файла блоба — `None` (пара — в `missing_blobs`).
    async fn add(&mut self, sha256: &str, filename: &str) -> Option<String> {
        let k = (sha256.to_string(), filename.to_string());
        if let Some(f) = self.files.get(&k) {
            return Some(f.clone());
        }
        let path = self.store.path_of(sha256);
        let len = match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_file() => m.len(),
            _ => {
                tracing::warn!(sha256 = %sha256, "экспорт: файла блоба нет на диске");
                self.missing.push(format!("{sha256}/{filename}"));
                return None;
            }
        };
        let name = attachment_file(sha256, filename, &mut self.taken);
        self.items.push(Item::Blob {
            name: name.clone(),
            path,
            len,
        });
        self.files.insert(k, name.clone());
        Some(name)
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::Internal(format!("сериализация экспорта: {e}")))
}

/// Строки JSONL.
fn jsonl<T: serde::Serialize>(rows: &[T]) -> Result<Vec<u8>, ApiError> {
    let mut out = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut out, r)
            .map_err(|e| ApiError::Internal(format!("сериализация экспорта: {e}")))?;
        out.push(b'\n');
    }
    Ok(out)
}

/// План архива: тексты заметок, записей, связей, папок и истории, затем файлы блобов.
async fn plan(d: SpaceDump, att: &mut Attachments) -> Result<Vec<Item>, ApiError> {
    let mut items = Vec::new();
    // имена: от глубоких путей к мелким (каталог раньше одноимённого файла, см. file_name_for)
    let mut order: Vec<usize> = (0..d.notes.len()).collect();
    order.sort_by(|&a, &b| {
        let (pa, pb) = (&d.notes[a].node.path, &d.notes[b].node.path);
        pb.matches('/')
            .count()
            .cmp(&pa.matches('/').count())
            .then(pa.cmp(pb))
    });
    let mut taken = HashSet::new();
    let mut names = vec![String::new(); d.notes.len()];
    for i in order {
        names[i] = file_name_for(&d.notes[i].node.path, &mut taken);
    }
    for (n, name) in d.notes.iter().zip(names) {
        let mut files = Vec::new();
        for a in &n.attachments {
            let file = att.add(&a.sha256, &a.filename).await;
            files.push(json!({
                "sha256": a.sha256, "name": a.filename, "mime": a.mime, "size": a.size,
                "created_at": fmt_ts(a.created_at),
                "file": file,
            }));
        }
        let node = &n.node;
        let tags = node
            .frontmatter
            .as_ref()
            .and_then(|f| f.get("tags"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        let mut text = front_matter(&[
            ("id", json!(node.id)),
            ("path", json!(node.path)),
            ("rev", json!(node.current_rev)),
            ("certainty", to_json(&node.certainty)?),
            ("tags", tags),
            ("refs", to_json(&n.refs)?),
            ("l0", json!(node.l0)),
            ("l0_source", json!(node.l0_source)),
            ("created_at", json!(fmt_ts(node.created_at))),
            ("updated_at", json!(fmt_ts(node.updated_at))),
            ("attachments", Value::Array(files)),
        ]);
        text.push_str(&n.content);
        items.push(Item::Text {
            name: format!("{name}.md"),
            data: text.into_bytes(),
        });
    }
    if !d.folders.is_empty() {
        let rows: Vec<Value> = d
            .folders
            .iter()
            .map(|f| {
                json!({
                    "id": f.id, "path": f.path, "l0": f.l0, "l0_source": f.l0_source,
                    "created_at": fmt_ts(f.created_at),
                })
            })
            .collect();
        items.push(Item::Text {
            name: FOLDERS_FILE.into(),
            data: jsonl(&rows)?,
        });
    }
    for e in &d.entries {
        let mut files = Vec::new();
        for f in &e.files {
            let file = att.add(&f.sha256, &f.filename).await;
            files.push(json!({
                "sha256": f.sha256, "name": f.filename, "mime": f.mime, "size": f.size,
                "created_at": fmt_ts(f.created_at), "file": file,
            }));
        }
        let x = &e.entry;
        let mut text = front_matter(&[
            ("id", json!(x.id)),
            ("kind", json!(x.kind)),
            ("title", json!(x.title)),
            ("status", to_json(&x.status)?),
            ("certainty", to_json(&x.certainty)?),
            ("data", x.data.clone()),
            ("tags", json!(x.tags)),
            ("area", json!(x.area)),
            ("refs", to_json(&e.refs)?),
            ("triggers", to_json(&e.triggers)?),
            ("supersedes_id", json!(x.supersedes_id)),
            ("superseded_by_id", json!(x.superseded_by_id)),
            ("retract_reason", json!(x.retract_reason)),
            ("rev", json!(x.rev)),
            ("author", to_json(&x.author)?),
            ("created_at", json!(fmt_ts(x.created_at))),
            ("updated_at", json!(fmt_ts(x.updated_at))),
            ("files", Value::Array(files)),
        ]);
        text.push_str(&x.body);
        items.push(Item::Text {
            name: format!("{JOURNALS_DIR}/{}/{}.md", x.kind.as_str(), x.id),
            data: text.into_bytes(),
        });
    }
    if !d.links.is_empty() {
        let rows: Vec<Value> = d
            .links
            .iter()
            .map(|l| {
                json!({
                    "src": l.src, "type": l.kind, "dst": l.dst, "dst_space": l.dst_space,
                    "anchor": l.dst_anchor,
                })
            })
            .collect();
        items.push(Item::Text {
            name: LINKS_FILE.into(),
            data: jsonl(&rows)?,
        });
    }
    for (id, revs) in &d.node_history {
        items.push(Item::Text {
            name: format!("{HISTORY_DIR}/{id}.jsonl"),
            data: jsonl(revs)?,
        });
    }
    for (id, revs) in &d.entry_history {
        items.push(Item::Text {
            name: format!("{HISTORY_DIR}/{id}.jsonl"),
            data: jsonl(revs)?,
        });
    }
    items.append(&mut att.items);
    Ok(items)
}

/// `Write` в канал тела ответа кусками по [`CHUNK`]; получатель ушёл — `BrokenPipe`.
struct ChanWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl ChanWriter {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let b = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx
            .blocking_send(Ok(b))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "клиент закрыл соединение"))
    }
}

impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

fn opts(method: CompressionMethod, len: u64) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(method)
        .large_file(len >= ZIP64_FROM)
}

/// Пишет архив `items` и опись в `w` (поток zip): тексты — deflate, блобы — store; отпечаток
/// считается по ходу записи. Размер файла блоба изменился — ошибка (архив был бы неверен).
fn write_archive<W: Write>(w: W, items: &[Item], mut manifest: ExportManifest) -> io::Result<W> {
    let zerr = |e: zip::result::ZipError| io::Error::other(e.to_string());
    let mut z = zip::ZipWriter::new_stream(w);
    let mut fp = Fingerprint::default();
    let mut buf = vec![0u8; CHUNK];
    for it in items {
        match it {
            Item::Text { name, data } => {
                z.start_file(name.as_str(), opts(CompressionMethod::Deflated, it.len()))
                    .map_err(zerr)?;
                fp.file(name, data);
                z.write_all(data)?;
            }
            Item::Blob { name, path, len } => {
                z.start_file(name.as_str(), opts(CompressionMethod::Stored, *len))
                    .map_err(zerr)?;
                fp.begin(name, *len);
                let mut f = std::fs::File::open(path)?;
                let mut n = 0u64;
                loop {
                    let k = f.read(&mut buf)?;
                    if k == 0 {
                        break;
                    }
                    n += k as u64;
                    fp.update(&buf[..k]);
                    z.write_all(&buf[..k])?;
                }
                if n != *len {
                    return Err(io::Error::other(format!(
                        "файл блоба {name}: {n} байт вместо {len}"
                    )));
                }
            }
        }
    }
    manifest.files = items.len();
    manifest.archive_fingerprint = fp.hex();
    let m = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    z.start_file(
        MANIFEST_FILE,
        opts(CompressionMethod::Deflated, m.len() as u64),
    )
    .map_err(zerr)?;
    z.write_all(&m)?;
    Ok(z.finish().map_err(zerr)?.into_inner())
}

/// Имя файла архива для `Content-Disposition`: ASCII, `zam-<вид>[-<guid8>]-<дата>.zip`.
fn download_name(sc_kind: &str, guid: Option<&str>, created_at: &str) -> String {
    let date = created_at.get(..10).unwrap_or("export");
    match guid {
        Some(g) => format!("zam-{sc_kind}-{}-{date}.zip", g.get(..8).unwrap_or(g)),
        None => format!("zam-{sc_kind}-{date}.zip"),
    }
}

pub async fn export(
    State(st): State<AppState>,
    p: Principal,
    client: Client,
    mut headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<ExportQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    admin_user(&p, &mut headers, q.user.as_deref())?;
    let filter = filter_of(&q)?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let link_spaces = graph_spaces(&p, &sc.db, &sc.space, sc.role).await?;
    let only_kinds = filter.paths.is_empty() && !filter.kinds.is_empty();
    let only_paths = !filter.paths.is_empty() && filter.kinds.is_empty();
    let sel = ExportSelect {
        space_id: sc.space.id,
        paths: if only_kinds {
            Some(Vec::new())
        } else if filter.paths.is_empty() {
            None
        } else {
            Some(filter.paths.clone())
        },
        kinds: if only_paths {
            Some(Vec::new())
        } else if filter.kinds.is_empty() {
            None
        } else {
            Some(filter.kinds.clone())
        },
        with_reports: p.is_human(),
        history: filter.history,
        link_spaces,
    };
    let dump = export_space(&sc.db, sel).await?;
    let mut att = Attachments {
        store: BlobStore::new(&st.data_dir.blobs_dir(), sc.owner),
        taken: HashSet::new(),
        files: HashMap::new(),
        items: Vec::new(),
        missing: Vec::new(),
    };
    let items = plan(dump, &mut att).await?;
    let created_at = fmt_ts(zam_store::time::now());
    let manifest = ExportManifest {
        format: EXPORT_FORMAT,
        server_version: st.version.to_string(),
        source_user_id: sc.owner,
        space: sc.space.id,
        space_kind: sc.space.kind.as_str().to_string(),
        space_name: sc.space.name.clone(),
        solution_guid: sc.space.solution_guid.clone(),
        created_at: created_at.clone(),
        filter,
        files: items.len(),
        archive_fingerprint: String::new(),
        missing_blobs: std::mem::take(&mut att.missing),
    };
    let bytes: u64 = items.iter().map(Item::len).sum();
    audit(&st, &p, &client, &sc, items.len(), bytes).await?;

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(CHANNEL);
    let space_id = sc.space.id;
    tokio::task::spawn_blocking(move || {
        let w = ChanWriter {
            tx: tx.clone(),
            buf: Vec::with_capacity(CHUNK),
        };
        let res = write_archive(w, &items, manifest).and_then(|mut w| w.flush());
        if let Err(e) = res {
            if e.kind() != io::ErrorKind::BrokenPipe {
                tracing::warn!(space = %space_id, error = %e, "экспорт оборван");
            }
            let _ = tx.blocking_send(Err(e));
        }
    });
    let stream =
        futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) });
    let name = download_name(
        sc.space.kind.as_str(),
        sc.space.solution_guid.as_deref(),
        &created_at,
    );
    let mut r = Body::from_stream(stream).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|e| ApiError::Internal(format!("имя архива: {e}")))?,
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r.extensions_mut().insert(NoCompress);
    Ok(r)
}

/// Аудит `space.export` без текста: пространство, владелец базы, файлы, байты.
async fn audit(
    st: &AppState,
    p: &Principal,
    client: &Client,
    sc: &crate::api::scope::MemScope,
    files: usize,
    bytes: u64,
) -> Result<(), ApiError> {
    let space: Id = sc.space.id;
    st.system
        .audit(
            &p.actor(),
            Some(sc.user.id),
            AUDIT_ACTION,
            Some(&space.to_string()),
            Some(&client.ip.to_string()),
            json!({
                "space": space, "user": sc.user.id, "owner": sc.owner,
                "files": files, "bytes": bytes,
            }),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(paths: Option<&str>, kinds: Option<&str>) -> ExportQuery {
        ExportQuery {
            paths: paths.map(str::to_string),
            kinds: kinds.map(str::to_string),
            history: None,
            user: None,
        }
    }

    #[test]
    fn filter_parsing() {
        let f = filter_of(&q(Some("docs/, a/b ,"), Some("insight,task,insight"))).unwrap();
        assert_eq!(f.paths, vec!["docs", "a/b"]);
        assert_eq!(f.kinds, vec![EntryKind::Insight, EntryKind::Task]);
        assert!(f.history);
        assert!(filter_of(&q(Some("a//b"), None)).is_err());
        assert!(filter_of(&q(None, Some("note"))).is_err());
    }

    #[test]
    fn download_names() {
        assert_eq!(
            download_name("knowledge", None, "2026-11-08T10:00:00Z"),
            "zam-knowledge-2026-11-08.zip"
        );
        assert_eq!(
            download_name("solution", Some("0192f1c4-7a1e"), "2026-11-08T10:00:00Z"),
            "zam-solution-0192f1c4-2026-11-08.zip"
        );
    }
}
