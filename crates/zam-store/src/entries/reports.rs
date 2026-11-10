//! Отчёты (спецификация M5.5 §4–5): запись вида `report` с файлами одной транзакцией, файлы
//! отчёта (`entry_attachments`) и первая картинка тела для миниатюры ленты.
//!
//! Файл отчёта — пара (запись, sha256, имя), как вложение заметки: строка `blobs` создаётся при
//! первой паре (`ON CONFLICT DO NOTHING`), каждая новая пара — единица `blobs.refcount`. Байты
//! блоба пишет сервер ([`crate::BlobStore`]) до вызова [`create_report`]. Строки
//! `entry_attachments` только дописываются (триггеры миграции 0022; открепление и очистка — со
//! сборщиком мусора блобов M7).

use std::collections::{BTreeSet, HashMap};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use zam_core::Id;

use super::write::{by_key, check_key, same_request, scoped_key, space_kind_of};
use super::{Entry, EntryKind, NewEntry, insert_entry, load_entry};
use crate::blobs::{LINK_SHA_LEN, check_filename, is_sha256};
use crate::nodes::Attribution;
use crate::time::{fmt_ts, ts_at};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Файл нового отчёта: блоб уже записан в [`crate::BlobStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReportFile {
    /// 64 hex в нижнем регистре.
    pub sha256: String,
    /// Имя — один сегмент пути (как у вложения заметки).
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

/// Файл отчёта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryAttachment {
    pub entry_id: Id,
    pub sha256: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
}

fn bad(msg: impl Into<String>) -> StoreError {
    StoreError::BadInput(msg.into())
}

pub(crate) const ATT_COLS: &str = "entry_id, blob_sha256, filename, mime, size, created_at";

pub(crate) fn att_row(r: &Row) -> rusqlite::Result<EntryAttachment> {
    let size: i64 = r.get(4)?;
    Ok(EntryAttachment {
        entry_id: r.get(0)?,
        sha256: r.get(1)?,
        filename: r.get(2)?,
        mime: r.get(3)?,
        size: u64::try_from(size).unwrap_or_default(),
        created_at: ts_at(r, 5)?,
    })
}

/// Проверенные файлы без повторов пар (имя, sha256), в порядке первого упоминания.
fn check_files(files: Vec<NewReportFile>) -> Result<Vec<NewReportFile>, StoreError> {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        if !is_sha256(&f.sha256) {
            return Err(bad(format!(
                "файл «{}»: sha256 — 64 символа hex в нижнем регистре",
                f.filename
            )));
        }
        check_filename(&f.filename)?;
        if f.mime.trim().is_empty() {
            return Err(bad(format!("файл «{}»: пустой MIME", f.filename)));
        }
        if i64::try_from(f.size).is_err() {
            return Err(bad(format!("файл «{}»: размер", f.filename)));
        }
        if seen.insert((f.filename.clone(), f.sha256.clone())) {
            out.push(f);
        }
    }
    Ok(out)
}

/// Отсортированные пары (имя, sha256) — часть отпечатка запроса для `Idempotency-Key`.
fn pairs_of(files: &[NewReportFile]) -> BTreeSet<(String, String)> {
    files
        .iter()
        .map(|f| (f.filename.clone(), f.sha256.clone()))
        .collect()
}

fn stored_pairs(c: &Connection, entry: Id) -> Result<BTreeSet<(String, String)>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT filename, blob_sha256 FROM entry_attachments WHERE entry_id = ?1",
    )?;
    let rows = st.query_map([entry], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Создаёт отчёт одной транзакцией писателя: запись вида `report` (без статуса и `certainty`,
/// ревизия 1, `index_state = excluded`), чанки и FTS, строки `blobs` и `entry_attachments`
/// (повтор пары в `files` — одна строка), `refcount + 1` на каждую пару. Сбой на любом шаге —
/// ничего не записано. Повтор `idempotency_key` возвращает ту же запись и `false`, если
/// совпали отпечаток [`create_entry`](super::create_entry) и набор пар (имя, sha256) — иначе
/// `IdempotencyMismatch`. Не `report`, пространство не решения, `links` не пусто, `refs`
/// задан, триггеры, статус или `certainty`, неверный sha256, имя или MIME файла, размер,
/// не совпавший с уже записанным блобом, → `BadInput`; `created_at` записи и её файлов — одно
/// время (так триггер 0022 `entry_attachments_report_only` пропускает только их);
/// нет пространства → `NotFound`. Ссылки тела на файлы проверяет сервер.
pub async fn create_report(
    db: &Db,
    new: NewEntry,
    files: Vec<NewReportFile>,
    attrib: &Attribution,
) -> Result<(Entry, bool), StoreError> {
    if new.kind != EntryKind::Report {
        return Err(bad("create_report создаёт только отчёты (kind: report)"));
    }
    if !new.links.is_empty() {
        return Err(bad("у отчёта нет связей: ссылки — только data.refs"));
    }
    if new.refs.is_some() {
        return Err(bad(
            "у отчёта нет ссылок на файлы refs: ссылки — только data.refs",
        ));
    }
    let files = check_files(files)?;
    let a = attrib.clone();
    db.write(move |tx| {
        match space_kind_of(tx, new.space_id)? {
            SpaceKind::Solution => {}
            k @ (SpaceKind::Rules | SpaceKind::Human | SpaceKind::Knowledge) => {
                return Err(bad(format!(
                    "отчёт пишется только в пространство решения, а не в {}",
                    k.as_str()
                )));
            }
        }
        let key = check_key(new.idempotency_key.clone())?.map(|k| scoped_key(&a, &k));
        if let Some(k) = &key
            && let Some(ex) = by_key(tx, k)?
        {
            if ex.kind == EntryKind::Report && stored_pairs(tx, ex.id)? != pairs_of(&files) {
                return Err(StoreError::IdempotencyMismatch(
                    "Idempotency-Key уже использован запросом с другими файлами".into(),
                ));
            }
            return same_request(tx, ex, &new).map(|ex| (ex, false));
        }
        let now = fmt_ts(crate::time::now());
        let id = insert_entry(tx, new, key, &a, None, &now)?;
        for f in &files {
            let size = i64::try_from(f.size).map_err(|_| bad("размер файла"))?;
            tx.prepare_cached(
                "INSERT INTO blobs (sha256, size, mime, refcount, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4) ON CONFLICT (sha256) DO NOTHING",
            )?
            .execute(params![f.sha256, size, f.mime, now])?;
            // блоб уже был: размер по sha256 обязан совпасть (иначе файл описан неверно)
            let stored: i64 = tx
                .prepare_cached("SELECT size FROM blobs WHERE sha256 = ?1")?
                .query_row([&f.sha256], |r| r.get(0))?;
            if stored != size {
                return Err(bad(format!(
                    "файл «{}»: размер {size} не совпадает с размером блоба {stored}",
                    f.filename
                )));
            }
            tx.prepare_cached(
                "INSERT INTO entry_attachments (entry_id, blob_sha256, filename, mime, size,
                                                created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![id, f.sha256, f.filename, f.mime, size, now])?;
            tx.prepare_cached("UPDATE blobs SET refcount = refcount + 1 WHERE sha256 = ?1")?
                .execute([&f.sha256])?;
        }
        Ok((load_entry(tx, id)?, true))
    })
    .await
}

/// Файлы записи по порядку добавления (затем по имени). Нет файлов или записи — пусто.
pub async fn entry_attachments(db: &Db, entry: Id) -> Result<Vec<EntryAttachment>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {ATT_COLS} FROM entry_attachments WHERE entry_id = ?1
             ORDER BY created_at, rowid"
        ))?;
        let rows = st.query_map([entry], att_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Файл записи по ссылке `_attachments/<sha16>/<имя>`: `sha16` — первые 16 символов hex без
/// учёта регистра, имя — точно. Не такой `sha16` или нет пары — `None`.
pub async fn entry_attachment(
    db: &Db,
    entry: Id,
    sha16: &str,
    filename: &str,
) -> Result<Option<EntryAttachment>, StoreError> {
    let sha16 = sha16.to_ascii_lowercase();
    if sha16.len() != LINK_SHA_LEN || !sha16.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let name = filename.to_string();
    db.read(move |c| {
        Ok(c.prepare_cached(&format!(
            "SELECT {ATT_COLS} FROM entry_attachments
             WHERE entry_id = ?1 AND substr(blob_sha256, 1, 16) = ?2 AND filename = ?3
             ORDER BY created_at, rowid LIMIT 1"
        ))?
        .query_row(params![entry, sha16, name], att_row)
        .optional()?)
    })
    .await
}

/// Файл живого отчёта по sha256 содержимого — `(mime, размер, имя)` — для превью
/// (`GET /previews/{sha256}`, миниатюры ленты): запись вида `report`, не удалена; `space` —
/// только в этом пространстве (участник общего решения в базе владельца). Несколько записей с
/// тем же содержимым — последняя привязка. Права проверяет вызывающий: отчёты видны только
/// сессии человека.
pub async fn report_blob_meta_by_sha(
    db: &Db,
    sha256: &str,
    space: Option<Id>,
) -> Result<Option<(String, u64, String)>, StoreError> {
    let sha = sha256.to_string();
    db.read(move |c| {
        let row: Option<(String, i64, String)> = c
            .query_row(
                "SELECT a.mime, a.size, a.filename
                 FROM entry_attachments a
                 JOIN entries e ON e.id = a.entry_id
                 WHERE a.blob_sha256 = ?1 AND e.kind = 'report' AND e.deleted_at IS NULL
                   AND (?2 IS NULL OR e.space_id = ?2)
                 ORDER BY a.created_at DESC, a.rowid DESC
                 LIMIT 1",
                params![sha, space],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row.map(|(m, s, f)| (m, u64::try_from(s).unwrap_or(0), f)))
    })
    .await
}

/// Первая картинка каждой записи `entries` в порядке тела: первая ссылка-картинка
/// `![…](_attachments/<sha16>/<имя>)`, у которой есть файл записи с MIME `image/*`. Записи без
/// такой картинки (и несуществующие) в ответ не попадают. Миниатюра ленты отчётов.
pub async fn first_images(
    db: &Db,
    entries: &[Id],
) -> Result<HashMap<Id, EntryAttachment>, StoreError> {
    if entries.is_empty() {
        return Ok(HashMap::new());
    }
    let ids = entries.to_vec();
    db.read(move |c| {
        let mut out = HashMap::new();
        let mut body_of = c.prepare_cached("SELECT body FROM entries WHERE id = ?1")?;
        let mut files_of = c.prepare_cached(&format!(
            "SELECT {ATT_COLS} FROM entry_attachments
             WHERE entry_id = ?1 AND mime LIKE 'image/%'"
        ))?;
        for id in ids {
            if out.contains_key(&id) {
                continue;
            }
            let Some(body) = body_of
                .query_row([id], |r| r.get::<_, String>(0))
                .optional()?
            else {
                continue;
            };
            let files: Vec<EntryAttachment> = files_of
                .query_map([id], att_row)?
                .collect::<Result<_, _>>()?;
            if files.is_empty() {
                continue;
            }
            let doc = zam_markdown::parse(&body);
            let first = doc
                .links
                .iter()
                .filter(|l| l.kind == zam_markdown::LinkKind::Image)
                .filter_map(|l| zam_markdown::attachment_ref(&l.target))
                .find_map(|(sha16, name)| {
                    let name = name?;
                    files
                        .iter()
                        .find(|f| f.sha256.starts_with(&sha16) && f.filename == name)
                        .cloned()
                });
            if let Some(f) = first {
                out.insert(id, f);
            }
        }
        Ok(out)
    })
    .await
}
