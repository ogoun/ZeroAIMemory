//! `GET /spaces/{space}/export?paths=&kinds=&history=[&probe=true]` (M6 §12б.5) — архив zip
//! пространства потоком (zip64, без `Seek`: [`zip::ZipWriter::new_stream`]).
//!
//! **Порядок.** Права (`space_scope`, чтение) → слот экспорта ([`EXPORT_SLOTS`] на сервер и
//! один на пользователя, занято — 503 `/problems/busy` с `Retry-After`; если загрузка не
//! началась — повторите позже) → аудит `space.export` (не записан — 500 без тела) → ответ с
//! потоком. Архив пишет поток
//! `spawn_blocking` внутри одной транзакции чтения ([`zam_store::export_space`]): заметки,
//! записи и ревизии читаются и пишутся по одной, байты уходят через ограниченный канал в тело
//! ответа. `probe=true` — та же проверка прав, фильтра и слота без архива и аудита (204): WebUI
//! зовёт её перед скачиванием, чтобы показать ошибку текстом.
//!
//! **Память** (оценка, ревью I1). Пик не зависит от объёма текстов и истории: метаданные узлов
//! пространства (путь, L0, frontmatter — порядка 1 КиБ на узел; нужны заранее для имён файлов),
//! множество занятых имён, id записей, список блобов; один текст за раз (заметка, запись или
//! ревизия — дважды: строка и frontmatter+текст), буфер чтения блоба 256 КиБ, канал
//! [`CHANNEL`] × [`CHUNK`] = 2 МиБ, состояние deflate (~0,5 МиБ). Итого на экспорт — несколько
//! МиБ плюс ~1 КиБ на узел и самая большая заметка ×2.
//!
//! **Соединение.** Транзакция чтения держит одно соединение пула читателей на всё время потока —
//! поэтому слотов меньше, чем читателей; пока она идёт, контрольные точки базы — только
//! `PASSIVE` ([`zam_store::Db::long_reader`]). Поток обрывается ([`Guard`]): клиент не забирает
//! кусок дольше [`STALL`], экспорт дольше `export.max_secs` (4 ч), сервер останавливается
//! ([`AppState::begin_stop`]) — транзакция закрывается, соединение возвращается в пул.
//!
//! **Права** (§12б.5): что принципал читает, то и выгружается. Токен — без Human (`space_scope`
//! → 404) и без отчётов; сессия WebUI — с ними; участник решения — пространство решения, связи
//! — только внутри него ([`graph_spaces`]); администратор — с `X-Zam-User` (`?user=` у ссылки
//! WebUI, как у вложений). Vault, карта кода, эмбеддинги, кэш выжимок, сессии, корзина в
//! снимок не входят ([`zam_store::export`]).
//!
//! **Аудит** — две записи: `space.export {space, user, owner, filter}` до тела (не записалась —
//! 500, архив не отдаётся) и `space.export.done {space, user, files, bytes, complete, reason}` по
//! окончании потока, в том числе при обрыве (`complete: false`, `reason`: `client` | `stall` |
//! `deadline` | `stop` | `error`); `bytes` — несжатый объём файлов архива без описи.
//! Ошибка посреди потока обрывает тело (клиент видит незавершённый архив); `zam export` пишет во
//! временный файл и публикует его только после полного архива.
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use zam_core::{Id, TargetKind};
use zam_store::time::fmt_ts;
use zam_store::{
    BlobStore, EntryKind, ExportSelect, NodePath, SpaceReader, StoreError, export_space,
};
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

/// Действие аудита до тела.
pub const AUDIT_ACTION: &str = "space.export";
/// Действие аудита по окончании потока.
pub const AUDIT_DONE: &str = "space.export.done";
/// Экспортов одновременно на сервер (каждый держит одно из соединений пула читателей); у
/// пользователя — один.
pub const EXPORT_SLOTS: usize = 2;
/// `Retry-After` при занятых слотах, с.
const RETRY_AFTER_S: u64 = 30;
/// Размер куска тела ответа, байт.
const CHUNK: usize = 256 * 1024;
/// Кусков в канале между потоком zip и телом ответа.
const CHANNEL: usize = 8;
/// Клиент не забирает тело дольше — поток обрывается.
const STALL: Duration = Duration::from_secs(10 * 60);
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
    /// Только проверка (204), без архива и аудита.
    probe: Option<bool>,
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

/// Обрыв потока экспорта: срок `export.max_secs` или остановка сервера.
#[derive(Clone)]
pub(crate) struct Guard {
    deadline: Instant,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Причина обрыва (`reason` аудита `space.export.done`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbortKind {
    Stop,
    Deadline,
    Stall,
    Client,
}

impl AbortKind {
    fn code(self) -> &'static str {
        match self {
            AbortKind::Stop => "stop",
            AbortKind::Deadline => "deadline",
            AbortKind::Stall => "stall",
            AbortKind::Client => "client",
        }
    }
}

/// Ошибка обрыва внутри `io::Error` (`io::Error::other(Abort(..))`): причина достаётся
/// `downcast`, а не по тексту.
#[derive(Debug)]
pub(crate) struct Abort(pub(crate) AbortKind);

impl std::fmt::Display for Abort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            AbortKind::Stop => "сервер останавливается",
            AbortKind::Deadline => "экспорт дольше export.max_secs",
            AbortKind::Stall => "клиент не забирает архив",
            AbortKind::Client => "клиент закрыл соединение",
        })
    }
}

impl std::error::Error for Abort {}

/// Ошибка ввода-вывода обрыва вида `k`. Вид — `Other`: `Interrupted` `write_all` повторяет
/// бесконечно (так зависал первый вариант остановки).
fn abort(k: AbortKind) -> io::Error {
    io::Error::other(Abort(k))
}

/// Причина обрыва ошибки `e` (`None` — не обрыв, а сбой).
fn abort_kind(e: &io::Error) -> Option<AbortKind> {
    e.get_ref()
        .and_then(|x| x.downcast_ref::<Abort>())
        .map(|a| a.0)
}

impl Guard {
    fn check(&self) -> io::Result<()> {
        if self.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(abort(AbortKind::Stop));
        }
        if Instant::now() >= self.deadline {
            return Err(abort(AbortKind::Deadline));
        }
        Ok(())
    }
}

/// `Write` в канал тела ответа кусками по [`CHUNK`]; каждая запись проверяет [`Guard`],
/// отправка — [`send_stalled`]. После первой ошибки поток мёртв: `write` и `flush` сразу
/// возвращают ошибку и ничего не шлют (завершение zip в `Drop` не ждёт `stall` и не пишет в
/// оборванное тело).
struct ChanWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
    guard: Guard,
    stall: Duration,
    dead: Option<AbortKind>,
}

impl ChanWriter {
    fn new(tx: mpsc::Sender<io::Result<Bytes>>, guard: Guard, stall: Duration) -> ChanWriter {
        ChanWriter {
            tx,
            buf: Vec::with_capacity(CHUNK),
            guard,
            stall,
            dead: None,
        }
    }

    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let b = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        send_stalled(&self.tx, Ok(b), self.stall, Some(&self.guard))
    }

    /// Мёртвый поток — ошибка сразу; иначе `f`, и её ошибка делает поток мёртвым.
    fn alive(&mut self, f: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        if let Some(k) = self.dead {
            return Err(abort(k));
        }
        f(self).inspect_err(|e| {
            self.dead = Some(abort_kind(e).unwrap_or(AbortKind::Client));
            self.buf.clear();
        })
    }
}

/// Кусок в канал тела: получатель ушёл — `Client`; не забирает дольше `stall` — `Stall`;
/// пока ждём — проверяется `guard` (срок, остановка).
fn send_stalled(
    tx: &mpsc::Sender<io::Result<Bytes>>,
    item: io::Result<Bytes>,
    stall: Duration,
    guard: Option<&Guard>,
) -> io::Result<()> {
    let mut item = item;
    let t0 = Instant::now();
    loop {
        match tx.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Closed(_)) => return Err(abort(AbortKind::Client)),
            Err(TrySendError::Full(back)) if t0.elapsed() < stall => {
                if let Some(g) = guard {
                    g.check()?;
                }
                item = back;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Full(_)) => return Err(abort(AbortKind::Stall)),
        }
    }
}

impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.alive(|w| {
            w.guard.check()?;
            w.buf.extend_from_slice(data);
            if w.buf.len() >= CHUNK {
                w.send()?;
            }
            Ok(())
        })?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.alive(Self::send)
    }
}

/// Ошибка обрыва для тела ответа: её выдаёт поток тела ([`body_stream`]) вне очереди
/// кусков — тело не кончается штатным концом chunked даже при полном канале.
#[derive(Default)]
pub(crate) struct AbortCell(std::sync::Mutex<Option<String>>);

impl AbortCell {
    fn set(&self, msg: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
    }

    fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Поток тела ответа: куски из канала; ошибка обрыва ([`AbortCell`]) — сразу, раньше
/// оставшихся кусков, и после неё поток кончается ошибкой (hyper рвёт соединение).
fn body_stream(
    rx: mpsc::Receiver<io::Result<Bytes>>,
    cell: std::sync::Arc<AbortCell>,
) -> impl futures_util::Stream<Item = io::Result<Bytes>> {
    futures_util::stream::unfold(Some(rx), move |rx| {
        let cell = cell.clone();
        async move {
            let mut rx = rx?;
            if let Some(e) = cell.take() {
                return Some((Err(io::Error::other(e)), None));
            }
            match rx.recv().await {
                Some(x) => Some((x, Some(rx))),
                // отправители ушли: обрыв (ячейка ставится раньше, чем уходят отправители)
                None => cell.take().map(|e| (Err(io::Error::other(e)), None)),
            }
        }
    })
}

fn opts(method: CompressionMethod, len: u64) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(method)
        .large_file(len >= ZIP64_FROM)
}

fn io_store(e: io::Error) -> StoreError {
    StoreError::Io(e)
}

fn zip_store(e: zip::result::ZipError) -> StoreError {
    match e {
        // ошибка записи (в том числе обрыв `Abort`) — как есть: причина достаётся `downcast`
        zip::result::ZipError::Io(e) => StoreError::Io(e),
        e => StoreError::Io(io::Error::other(e.to_string())),
    }
}

fn json_store(e: serde_json::Error) -> StoreError {
    StoreError::Io(io::Error::other(e))
}

/// Блоб архива: путь на диске сервера и размер.
struct BlobItem {
    name: String,
    path: PathBuf,
    len: u64,
}

/// Итог потока: файлов и байт (без описи), ошибка.
#[derive(Default)]
struct Stats {
    files: usize,
    bytes: u64,
    error: Option<String>,
    /// Код причины обрыва (`reason`); `error` без причины — `error`.
    reason: Option<&'static str>,
}

/// Код причины ошибки экспорта.
fn reason_of(e: &StoreError) -> &'static str {
    match e {
        StoreError::Io(io) => abort_kind(io).map_or("error", AbortKind::code),
        _ => "error",
    }
}

/// Запись архива в поток zip (внутри транзакции [`export_space`]).
struct ArchiveWriter<W: Write> {
    z: zip::ZipWriter<zip::write::StreamWriter<W>>,
    fp: Fingerprint,
    files: usize,
    bytes: u64,
    store: BlobStore,
    taken: HashSet<String>,
    att: HashMap<(String, String), String>,
    blobs: Vec<BlobItem>,
    missing: Vec<String>,
}

impl<W: Write> ArchiveWriter<W> {
    fn text(&mut self, name: &str, data: &[u8]) -> Result<(), StoreError> {
        self.z
            .start_file(name, opts(CompressionMethod::Deflated, data.len() as u64))
            .map_err(zip_store)?;
        self.fp.file(name, data);
        self.z.write_all(data).map_err(io_store)?;
        self.files += 1;
        self.bytes += data.len() as u64;
        Ok(())
    }

    /// Путь пары (sha256, имя) в архиве; нет файла блоба — `None` (пара — в `missing_blobs`).
    fn attachment(&mut self, sha256: &str, filename: &str) -> Option<String> {
        let k = (sha256.to_string(), filename.to_string());
        if let Some(f) = self.att.get(&k) {
            return Some(f.clone());
        }
        let path = self.store.path_of(sha256);
        let len = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m.len(),
            _ => {
                tracing::warn!(sha256 = %sha256, "экспорт: файла блоба нет на диске");
                self.missing.push(format!("{sha256}/{filename}"));
                return None;
            }
        };
        let name = attachment_file(sha256, filename, &mut self.taken);
        self.blobs.push(BlobItem {
            name: name.clone(),
            path,
            len,
        });
        self.att.insert(k, name.clone());
        Some(name)
    }

    /// Файл истории: строки JSONL по одной ревизии.
    fn history(&mut self, r: &SpaceReader, kind: TargetKind, id: Id) -> Result<(), StoreError> {
        let name = format!("{HISTORY_DIR}/{id}.jsonl");
        let bound = r.history_bound(kind, id)?;
        self.z
            .start_file(name.as_str(), opts(CompressionMethod::Deflated, bound))
            .map_err(zip_store)?;
        self.fp.begin(&name);
        let mut line = Vec::new();
        let mut put = |w: &mut Self, v: &dyn erased::Ser| -> Result<(), StoreError> {
            line.clear();
            v.to_writer(&mut line).map_err(json_store)?;
            line.push(b'\n');
            w.fp.update(&line);
            w.z.write_all(&line).map_err(io_store)?;
            w.bytes += line.len() as u64;
            Ok(())
        };
        match kind {
            TargetKind::Node => r.for_each_node_revision(id, |rev| put(self, &rev))?,
            _ => r.for_each_entry_revision(id, |rev| put(self, &rev))?,
        }
        self.fp.end();
        self.files += 1;
        Ok(())
    }

    fn blob(&mut self, b: &BlobItem, buf: &mut [u8]) -> Result<(), StoreError> {
        self.z
            .start_file(b.name.as_str(), opts(CompressionMethod::Stored, b.len))
            .map_err(zip_store)?;
        self.fp.begin(&b.name);
        let mut f = std::fs::File::open(&b.path).map_err(io_store)?;
        let mut n = 0u64;
        loop {
            let k = f.read(buf).map_err(io_store)?;
            if k == 0 {
                break;
            }
            n += k as u64;
            self.fp.update(&buf[..k]);
            self.z.write_all(&buf[..k]).map_err(io_store)?;
        }
        self.fp.end();
        if n != b.len {
            return Err(io_store(io::Error::other(format!(
                "файл блоба {}: {n} байт вместо {}",
                b.name, b.len
            ))));
        }
        self.files += 1;
        self.bytes += n;
        Ok(())
    }
}

/// Сериализация ревизии двух типов одним замыканием.
mod erased {
    pub trait Ser {
        fn to_writer(&self, w: &mut Vec<u8>) -> serde_json::Result<()>;
    }
    impl<T: serde::Serialize> Ser for T {
        fn to_writer(&self, w: &mut Vec<u8>) -> serde_json::Result<()> {
            serde_json::to_writer(w, self)
        }
    }
}

/// Архив пространства в `w` (см. модуль). Порядок: заметки, папки, записи, связи, история,
/// блобы, опись.
fn write_archive<W: Write>(
    r: &SpaceReader,
    w: W,
    store: BlobStore,
    history: bool,
    mut manifest: ExportManifest,
    stats: &mut Stats,
) -> Result<W, StoreError> {
    let mut a = ArchiveWriter {
        z: zip::ZipWriter::new_stream(w),
        fp: Fingerprint::default(),
        files: 0,
        bytes: 0,
        store,
        taken: HashSet::new(),
        att: HashMap::new(),
        blobs: Vec::new(),
        missing: Vec::new(),
    };
    let res = write_parts(r, &mut a, history);
    stats.files = a.files;
    stats.bytes = a.bytes;
    res?;
    manifest.files = a.files;
    manifest.archive_fingerprint = a.fp.hex();
    manifest.missing_blobs = std::mem::take(&mut a.missing);
    let m = serde_json::to_vec_pretty(&manifest).map_err(json_store)?;
    a.z.start_file(
        MANIFEST_FILE,
        opts(CompressionMethod::Deflated, m.len() as u64),
    )
    .map_err(zip_store)?;
    a.z.write_all(&m).map_err(io_store)?;
    Ok(a.z.finish().map_err(zip_store)?.into_inner())
}

fn write_parts<W: Write>(
    r: &SpaceReader,
    a: &mut ArchiveWriter<W>,
    history: bool,
) -> Result<(), StoreError> {
    let (folders, notes) = r.nodes()?;
    // имена: от глубоких путей к мелким (каталог раньше одноимённого файла, см. file_name_for)
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|&x, &y| {
        let (px, py) = (&notes[x].path, &notes[y].path);
        py.matches('/')
            .count()
            .cmp(&px.matches('/').count())
            .then(px.cmp(py))
    });
    let mut taken = HashSet::new();
    let mut names = vec![String::new(); notes.len()];
    for i in order {
        names[i] = file_name_for(&notes[i].path, &mut taken);
    }
    let mut srcs: HashSet<Id> = HashSet::new();
    for (node, name) in notes.iter().zip(&names) {
        srcs.insert(node.id);
        let mut files = Vec::new();
        for att in r.note_attachments(node.id)? {
            let file = a.attachment(&att.sha256, &att.filename);
            files.push(json!({
                "sha256": att.sha256, "name": att.filename, "mime": att.mime, "size": att.size,
                "created_at": fmt_ts(att.created_at), "file": file, "doc": att.doc,
            }));
        }
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
            ("certainty", json!(node.certainty)),
            ("tags", tags),
            ("refs", json!(r.refs("node", node.id)?)),
            ("l0", json!(node.l0)),
            ("l0_source", json!(node.l0_source)),
            ("created_at", json!(fmt_ts(node.created_at))),
            ("updated_at", json!(fmt_ts(node.updated_at))),
            ("attachments", Value::Array(files)),
        ]);
        text.push_str(&r.note_content(node)?);
        a.text(&format!("{name}.md"), text.as_bytes())?;
    }
    if !folders.is_empty() {
        let mut rows = Vec::new();
        for f in &folders {
            serde_json::to_writer(
                &mut rows,
                &json!({
                    "id": f.id, "path": f.path, "l0": f.l0, "l0_source": f.l0_source,
                    "created_at": fmt_ts(f.created_at),
                }),
            )
            .map_err(json_store)?;
            rows.push(b'\n');
        }
        a.text(FOLDERS_FILE, &rows)?;
    }
    let mut entries: Vec<Id> = Vec::new();
    r.for_each_entry(|e| {
        let x = &e.entry;
        srcs.insert(x.id);
        entries.push(x.id);
        let mut files = Vec::new();
        for f in &e.files {
            let file = a.attachment(&f.sha256, &f.filename);
            files.push(json!({
                "sha256": f.sha256, "name": f.filename, "mime": f.mime, "size": f.size,
                "created_at": fmt_ts(f.created_at), "file": file,
            }));
        }
        let mut text = front_matter(&[
            ("id", json!(x.id)),
            ("kind", json!(x.kind)),
            ("title", json!(x.title)),
            ("status", json!(x.status)),
            ("certainty", json!(x.certainty)),
            ("data", x.data.clone()),
            ("tags", json!(x.tags)),
            ("area", json!(x.area)),
            ("refs", json!(e.refs)),
            (
                "triggers",
                serde_json::to_value(&e.triggers).map_err(json_store)?,
            ),
            ("supersedes_id", json!(x.supersedes_id)),
            ("superseded_by_id", json!(x.superseded_by_id)),
            ("retract_reason", json!(x.retract_reason)),
            ("rev", json!(x.rev)),
            (
                "author",
                serde_json::to_value(&x.author).map_err(json_store)?,
            ),
            ("created_at", json!(fmt_ts(x.created_at))),
            ("updated_at", json!(fmt_ts(x.updated_at))),
            ("files", Value::Array(files)),
        ]);
        text.push_str(&x.body);
        a.text(
            &format!("{JOURNALS_DIR}/{}/{}.md", x.kind.as_str(), x.id),
            text.as_bytes(),
        )
    })?;
    let links = r.links(&srcs)?;
    if !links.is_empty() {
        let mut rows = Vec::new();
        for l in &links {
            serde_json::to_writer(
                &mut rows,
                &json!({
                    "src": l.src, "type": l.kind, "dst": l.dst, "dst_space": l.dst_space,
                    "anchor": l.dst_anchor,
                }),
            )
            .map_err(json_store)?;
            rows.push(b'\n');
        }
        a.text(LINKS_FILE, &rows)?;
    }
    if history {
        for n in &notes {
            a.history(r, TargetKind::Node, n.id)?;
        }
        for id in &entries {
            a.history(r, TargetKind::Entry, *id)?;
        }
    }
    let blobs = std::mem::take(&mut a.blobs);
    let mut buf = vec![0u8; CHUNK];
    for b in &blobs {
        a.blob(b, &mut buf)?;
    }
    Ok(())
}

/// Имя файла архива для `Content-Disposition`: ASCII, `zam-<вид>[-<guid8>]-<дата>.zip`.
fn download_name(sc_kind: &str, guid: Option<&str>, created_at: &str) -> String {
    let date = created_at.get(..10).unwrap_or("export");
    match guid {
        Some(g) => format!("zam-{sc_kind}-{}-{date}.zip", g.get(..8).unwrap_or(g)),
        None => format!("zam-{sc_kind}-{date}.zip"),
    }
}

/// Слот экспорта: разрешение семафора сервера и отметка пользователя; освобождается при
/// уничтожении.
pub(crate) struct Slot {
    _permit: tokio::sync::OwnedSemaphorePermit,
    users: std::sync::Arc<std::sync::Mutex<HashSet<Id>>>,
    user: Id,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.user);
    }
}

/// Слот экспорта пользователя `user`; занят (у сервера или у пользователя) — 503 с
/// `Retry-After`.
fn slot(st: &AppState, user: Id) -> Result<Slot, ApiError> {
    let busy = || ApiError::ExportBusy {
        retry_after_s: RETRY_AFTER_S,
    };
    let mut users = st.export_users.lock().unwrap_or_else(|e| e.into_inner());
    if users.contains(&user) {
        return Err(busy());
    }
    let permit = st
        .export_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| busy())?;
    users.insert(user);
    Ok(Slot {
        _permit: permit,
        users: st.export_users.clone(),
        user,
    })
}

pub async fn export(
    State(st): State<AppState>,
    p: Principal,
    client: Client,
    mut headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<ExportQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    // сервер останавливается: ни аудита, ни слота — повторить на работающем сервере
    if st.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ApiError::ShuttingDown {
            retry_after_s: RETRY_AFTER_S,
        });
    }
    let q = query(q)?;
    admin_user(&p, &mut headers, q.user.as_deref())?;
    let filter = filter_of(&q)?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let permit = slot(&st, sc.user.id)?;
    if q.probe == Some(true) {
        drop(permit);
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
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
        link_spaces,
    };
    let created_at = fmt_ts(zam_store::time::now());
    let history = filter.history;
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
        files: 0,
        archive_fingerprint: String::new(),
        missing_blobs: Vec::new(),
    };
    let store = BlobStore::new(&st.data_dir.blobs_dir(), sc.owner);
    let audit = AuditCtx {
        actor: p.actor(),
        user: sc.user.id,
        owner: sc.owner,
        space: sc.space.id,
        ip: client.ip.to_string(),
    };
    // до тела: не записался аудит — архива нет (500)
    audit.start(&st, &manifest.filter).await?;
    let guard = Guard {
        deadline: Instant::now() + Duration::from_secs(st.cfg.export.max_secs),
        stop: st.stopping.clone(),
    };

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(CHANNEL);
    let cell = std::sync::Arc::new(AbortCell::default());
    let cell2 = cell.clone();
    let db = sc.db.clone();
    let st2 = st.clone();
    let running = st.export_tasks.clone().enter();
    tokio::spawn(async move {
        let _running = running;
        let space_id = manifest.space;
        let res = export_space(&db, sel, move |r| {
            let mut stats = Stats::default();
            let w = ChanWriter::new(tx.clone(), guard, STALL);
            let res =
                write_archive(r, w, store, history, manifest, &mut stats).and_then(|mut w| {
                    w.flush().map_err(io_store)?;
                    Ok(w)
                });
            let w = match res {
                Ok(w) => Some(w),
                Err(e) => {
                    stats.error = Some(e.to_string());
                    stats.reason = Some(reason_of(&e));
                    // до ухода отправителей: тело кончится ошибкой, а не штатным концом
                    cell2.set(e.to_string());
                    None
                }
            };
            // слот освобождается раньше конца тела (тело кончается с последним отправителем
            // канала): клиент, сразу начавший следующий экспорт, слот получит
            drop(permit);
            drop(w);
            drop(tx);
            Ok(stats)
        })
        .await;
        let stats = res.unwrap_or_else(|e| Stats {
            error: Some(e.to_string()),
            reason: Some("error"),
            ..Stats::default()
        });
        if let Some(e) = &stats.error {
            tracing::warn!(space = %space_id, error = %e, "экспорт оборван");
        }
        if let Err(e) = audit.done(&st2, &stats).await {
            tracing::warn!(space = %space_id, error = %e, "аудит экспорта не записан");
        }
    });
    let stream = body_stream(rx, cell);
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

/// Аудит экспорта без текста: кто, чьё пространство (см. модуль).
struct AuditCtx {
    actor: String,
    user: Id,
    owner: Id,
    space: Id,
    ip: String,
}

impl AuditCtx {
    async fn record(&self, st: &AppState, action: &str, details: Value) -> Result<(), StoreError> {
        st.system
            .audit(
                &self.actor,
                Some(self.user),
                action,
                Some(&self.space.to_string()),
                Some(&self.ip),
                details,
            )
            .await
    }

    /// `space.export` до тела; тестовая точка `export_audit` (фича `test-hooks`) — сбой.
    async fn start(&self, st: &AppState, filter: &ExportFilter) -> Result<(), ApiError> {
        crate::backup::fail_point(&st.cfg.data_dir, "export_audit")
            .map_err(|e| ApiError::Internal(format!("аудит экспорта: {e}")))?;
        self.record(
            st,
            AUDIT_ACTION,
            json!({
                "space": self.space, "user": self.user, "owner": self.owner, "filter": filter,
            }),
        )
        .await
        .map_err(|e| ApiError::Internal(format!("аудит экспорта: {e}")))
    }

    /// `space.export.done` по окончании потока.
    async fn done(&self, st: &AppState, s: &Stats) -> Result<(), StoreError> {
        let mut d = json!({
            "space": self.space, "user": self.user, "owner": self.owner,
            "files": s.files, "bytes": s.bytes, "complete": s.error.is_none(),
        });
        if let (Some(_), Value::Object(o)) = (&s.error, &mut d) {
            o.insert("reason".into(), json!(s.reason.unwrap_or("error")));
        }
        self.record(st, AUDIT_DONE, d).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(paths: Option<&str>, kinds: Option<&str>) -> ExportQuery {
        ExportQuery {
            paths: paths.map(str::to_string),
            kinds: kinds.map(str::to_string),
            history: None,
            probe: None,
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

    /// Получатель не забирает куски: обрыв через `stall`, а не вечное ожидание.
    #[test]
    fn send_stalled_gives_up() {
        let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(1);
        send_stalled(&tx, Ok(Bytes::from_static(b"a")), STALL, None).unwrap();
        let t0 = Instant::now();
        let e = send_stalled(
            &tx,
            Ok(Bytes::from_static(b"b")),
            Duration::from_millis(50),
            None,
        )
        .unwrap_err();
        assert_eq!(abort_kind(&e), Some(AbortKind::Stall));
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        // остановка сервера прерывает ожидание раньше срока
        let g = Guard {
            deadline: Instant::now() + Duration::from_secs(3600),
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let e = send_stalled(&tx, Ok(Bytes::from_static(b"c")), STALL, Some(&g)).unwrap_err();
        assert_eq!(abort_kind(&e), Some(AbortKind::Stop));
        // `Interrupted` `write_all` повторял бы бесконечно (так зависал первый вариант)
        assert_ne!(e.kind(), io::ErrorKind::Interrupted);
        drop(rx);
        let e = send_stalled(&tx, Ok(Bytes::from_static(b"d")), STALL, None).unwrap_err();
        assert_eq!(abort_kind(&e), Some(AbortKind::Client));
        // причина проходит через ошибку zip и хранилища
        let z = zip_store(zip::result::ZipError::Io(abort(AbortKind::Deadline)));
        assert_eq!(reason_of(&z), "deadline");
        assert_eq!(reason_of(&io_store(io::Error::other("x"))), "error");
    }

    /// После первой ошибки поток мёртв: завершение zip в `Drop` не ждёт `stall` и ничего не
    /// пишет в канал.
    #[test]
    fn dead_writer_drop_does_not_wait_or_write() {
        let (tx, mut rx) = mpsc::channel::<io::Result<Bytes>>(1);
        tx.try_send(Ok(Bytes::from_static(b"first"))).unwrap();
        let g = Guard {
            deadline: Instant::now() + Duration::from_secs(3600),
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // сам писатель: после ошибки `write` и `flush` (дописывание каталога в `Drop`) — сразу
        let mut w = ChanWriter::new(tx.clone(), g.clone(), Duration::from_secs(2));
        let e = w.write_all(&vec![1u8; CHUNK]).unwrap_err();
        assert_eq!(abort_kind(&e), Some(AbortKind::Stall));
        let t0 = Instant::now();
        let e = w.write_all(&vec![2u8; CHUNK]).unwrap_err();
        assert_eq!(abort_kind(&e), Some(AbortKind::Stall));
        assert!(w.flush().is_err());
        assert!(
            t0.elapsed() < Duration::from_millis(1000),
            "мёртвый поток ждал {:?}",
            t0.elapsed()
        );
        drop(w);
        // то же под zip: `Drop` не ждёт
        let w = ChanWriter::new(tx, g, Duration::from_secs(2));
        let mut z = zip::ZipWriter::new_stream(w);
        z.start_file("a.bin", opts(CompressionMethod::Stored, 0))
            .unwrap();
        let e = z.write_all(&vec![7u8; CHUNK + 1]).unwrap_err();
        assert_eq!(abort_kind(&e), Some(AbortKind::Stall));
        let t0 = Instant::now();
        drop(z);
        assert!(
            t0.elapsed() < Duration::from_millis(1000),
            "Drop ждал {:?}",
            t0.elapsed()
        );
        assert!(rx.try_recv().is_ok(), "первый кусок на месте");
        assert!(
            rx.try_recv().is_err(),
            "после ошибки в канал ничего не ушло"
        );
    }

    /// Ошибка обрыва — раньше оставшихся кусков и после ухода отправителей: тело кончается
    /// ошибкой, а не штатно.
    #[tokio::test]
    async fn body_stream_ends_with_error() {
        use futures_util::StreamExt;
        let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(4);
        let cell = std::sync::Arc::new(AbortCell::default());
        tx.try_send(Ok(Bytes::from_static(b"a"))).unwrap();
        let mut st = Box::pin(body_stream(rx, cell.clone()));
        assert!(st.next().await.unwrap().is_ok());
        tx.try_send(Ok(Bytes::from_static(b"b"))).unwrap();
        cell.set("обрыв".into());
        drop(tx);
        assert!(st.next().await.unwrap().is_err(), "ошибка раньше куска «b»");
        assert!(st.next().await.is_none());
        // без обрыва — штатный конец
        let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(1);
        drop(tx);
        let mut st = Box::pin(body_stream(rx, std::sync::Arc::default()));
        assert!(st.next().await.is_none());
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
