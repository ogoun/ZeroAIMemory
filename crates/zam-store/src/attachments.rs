//! Вложения как цель индексации (спецификация M5 §3): документы `attachment_docs`, кэш
//! извлечения `blob_extract` и чанки вложений (`chunks.attachment_id`).
//!
//! **Документ вложения** — пара (узел, блоб, имя) из `attachments` со своим id: цель заданий
//! `index_jobs` (`target_kind = 'attachment'`, [`TargetKind::Attachment`]) и владелец чанков.
//! Чанки и права — на каждое вложение (один блоб в двух заметках — два документа со своими
//! чанками и областью узла-владельца); извлечение — одно на `blob_sha256` и
//! [`EXTRACTOR_VERSION`] в базе пользователя (блоб другого пользователя — в его базе, кэш не
//! общий).
//!
//! **Изоляция от заметки** (Review Focus 2): чанк вложения несёт `node_id` узла-владельца и
//! `attachment_id`; код заметки (нарезка, сравнение нарезки, удаление чанков, пересчёт
//! префиксов, векторный индекс узла, поиск) берёт только `attachment_id IS NULL`. Операции над
//! узлом каскадируют на его документы: удаление — `deleted_at` меткой узла и `delete`,
//! восстановление — снятие метки и `upsert`, перенос — новый `context_prefix` и `upsert`.
//! Чанки вложений удалённого узла остаются (скрыты меткой), восстановление их возвращает.
//!
//! **Human** — только FTS: извлечение (задание) нужно и Human-вложению, а задание ради точек
//! Qdrant (новая нарезка, перенос, восстановление) ему не ставится; векторный индекс Human не
//! берёт и сам (как у заметок, `jobs::index_chunks`).
//!
//! Типы извлечения — из `zam-docs` ([`Extracted`], [`Unit`], [`Locator`], [`DocKind`]);
//! [`ExtractState`] — состояние строки базы, поэтому здесь.

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zam_core::{Id, TargetKind, TargetRef};
use zam_markdown::mask_text;

pub use zam_docs::{DocKind, EXTRACTOR_VERSION, Extracted, Locator, Unit};

use crate::cache::{StaleKind, invalidate_target};
use crate::certainty::{CertaintyChange, check_certainty};
use crate::embeddings::embed_key_of;
use crate::entries::Certainty;
use crate::jobs::{JobOp, PRIORITY_WRITE, enqueue_target, has_active_target};
use crate::nodes::{Attribution, context_prefix, token_counter};
use crate::time::{fmt_ts, ts_at};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Уровень чанка вложения (`chunks.level`).
pub const LEVEL_ATTACHMENT: &str = "attachment";
/// Ревизия заданий и чанков вложения: у документа ревизий нет (содержимое блоба неизменно).
const DOC_REV: i64 = 0;
/// Уровень сжатия zstd `blob_extract.units`: быстрый, текст сжимается в 3–5 раз.
const ZSTD_LEVEL: i32 = 3;
/// Предел длины `extract_error`, символов.
const EXTRACT_ERROR_MAX_CHARS: usize = 1000;

/// Пространство имён UUIDv5 id чанков вложений (отлично от blake3-схемы чанков заметок).
fn chunk_namespace() -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"https://zeroaimemory/chunk/attachment",
    )
}

/// Состояние извлечения текста вложения (`attachment_docs.extract_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractState {
    /// Ждёт извлечения (задание `Attachment` в очереди).
    Pending,
    /// Текст извлечён, чанки записаны.
    Done,
    /// Разбор не удался (`extract_error` — причина).
    Failed,
    /// Вид не поддерживается (изображение, архив, видео…): без задания.
    Unsupported,
}

impl ExtractState {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtractState::Pending => "pending",
            ExtractState::Done => "done",
            ExtractState::Failed => "failed",
            ExtractState::Unsupported => "unsupported",
        }
    }
}

impl ToSql for ExtractState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for ExtractState {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "pending" => Ok(ExtractState::Pending),
            "done" => Ok(ExtractState::Done),
            "failed" => Ok(ExtractState::Failed),
            "unsupported" => Ok(ExtractState::Unsupported),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестное состояние извлечения: {other}").into(),
            )),
        }
    }
}

/// Документ вложения узла.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentDoc {
    pub id: Id,
    pub node_id: Id,
    pub blob_sha256: String,
    pub filename: String,
    pub mime: Option<String>,
    /// Размер файла, байт.
    pub size: u64,
    pub extract_state: ExtractState,
    /// Причина `failed` (без текста документа).
    pub extract_error: Option<String>,
    /// Есть страницы без текстового слоя (OCR — вне M5).
    pub ocr_needed: bool,
    #[serde(serialize_with = "crate::time::ser_ts")]
    pub created_at: Timestamp,
}

/// Имя вида документа для `blob_extract.kind`.
fn kind_from_str(s: &str) -> Option<DocKind> {
    Some(match s {
        "pdf" => DocKind::Pdf,
        "docx" => DocKind::Docx,
        "pptx" => DocKind::Pptx,
        "xlsx" => DocKind::Xlsx,
        "xls" => DocKind::Xls,
        "ods" => DocKind::Ods,
        "csv" => DocKind::Csv,
        "text" => DocKind::Text,
        _ => return None,
    })
}

fn att_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Attachment,
        id,
    }
}

/// Ставит задание `op` документу вложения (в транзакции писателя).
pub(crate) fn enqueue_doc(c: &Connection, id: Id, op: JobOp, now: &str) -> Result<(), StoreError> {
    enqueue_target(
        c,
        TargetKind::Attachment,
        id,
        DOC_REV,
        op,
        PRIORITY_WRITE,
        now,
    )
}

/// Что известно о файле при привязке ([`crate::blobs::attach`]): вид по `zam_docs::sniff` и
/// размер против `docs.max_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocIntake {
    /// Поддерживаемый вид: `pending` с заданием `Attachment`.
    Index(DocKind),
    /// Поддерживаемый вид, но файл больше `docs.max_bytes`: `failed` с `too_large` без задания
    /// (разбор всё равно отказал бы `DocError::TooLarge`).
    TooLarge(DocKind),
    /// Вид не поддерживается: `unsupported` без задания.
    Unsupported,
}

impl DocIntake {
    /// Решение по виду `kind` (`None` — не поддерживается) и размеру файла.
    pub fn classify(kind: Option<DocKind>, size: u64, max_bytes: u64) -> DocIntake {
        match kind {
            None => DocIntake::Unsupported,
            Some(k) if size > max_bytes => DocIntake::TooLarge(k),
            Some(k) => DocIntake::Index(k),
        }
    }

    /// Начальные `extract_state` и `extract_error` документа.
    fn state(self) -> (ExtractState, Option<&'static str>) {
        match self {
            DocIntake::Index(_) => (ExtractState::Pending, None),
            DocIntake::TooLarge(_) => (ExtractState::Failed, Some(TOO_LARGE)),
            DocIntake::Unsupported => (ExtractState::Unsupported, None),
        }
    }
}

/// `extract_error` файла больше `docs.max_bytes`.
pub const TOO_LARGE: &str = "too_large";

/// Создаёт документ пары (узел, блоб, имя) или возвращает откреплённый (с новым `created_at`:
/// порядок списка — по последней привязке) в состоянии по `intake`; задание `Attachment` — только
/// [`DocIntake::Index`]. Живой документ пары не меняется. В транзакции привязки.
pub(crate) fn ensure_doc(
    c: &Connection,
    node: Id,
    sha: &str,
    filename: &str,
    mime: &str,
    intake: DocIntake,
    now: &str,
) -> Result<Id, StoreError> {
    let (state, err) = intake.state();
    let row: Option<(Id, bool)> = c
        .prepare_cached(
            "SELECT id, deleted_at IS NOT NULL FROM attachment_docs
             WHERE node_id = ?1 AND blob_sha256 = ?2 AND filename = ?3",
        )?
        .query_row(params![node, sha, filename], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let id = match row {
        Some((id, false)) => return Ok(id),
        Some((id, true)) => {
            c.prepare_cached(
                "UPDATE attachment_docs
                 SET deleted_at = NULL, extract_state = ?2, extract_error = ?3, ocr_needed = 0,
                     mime = ?4, created_at = ?5
                 WHERE id = ?1",
            )?
            .execute(params![id, state, err, mime, now])?;
            id
        }
        None => {
            let id = Id::new();
            c.prepare_cached(
                "INSERT INTO attachment_docs (id, node_id, blob_sha256, filename, mime,
                                              extract_state, extract_error, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![id, node, sha, filename, mime, state, err, now])?;
            id
        }
    };
    if state == ExtractState::Pending {
        enqueue_doc(c, id, JobOp::Upsert, now)?;
    }
    Ok(id)
}

/// Пространство узла-владельца документа — Human (вложение только в FTS).
fn doc_in_human(c: &Connection, id: Id) -> Result<bool, StoreError> {
    let kind: SpaceKind = c
        .prepare_cached(
            "SELECT s.kind FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
             JOIN spaces s ON s.id = n.space_id WHERE d.id = ?1",
        )?
        .query_row([id], |r| r.get(0))?;
    Ok(kind == SpaceKind::Human)
}

/// Документ скрыт или откреплён: точки убираются (`delete`). Human-документу `delete` нужен,
/// только если у него есть активное задание (снять незавершённое извлечение).
fn enqueue_doc_delete(c: &Connection, id: Id, now: &str) -> Result<(), StoreError> {
    if !doc_in_human(c, id)? || has_active_target(c, att_ref(id))? {
        enqueue_doc(c, id, JobOp::Delete, now)?;
    }
    Ok(())
}

/// Документу нужно задание после восстановления или переноса узла: `pending` — всегда
/// (извлечение не закончено, его задание могло быть заменено `delete`), `done` — ради точек
/// (не Human); `failed` и `unsupported` — нет (повтор — `reindex`).
fn requeue_doc(c: &Connection, id: Id, state: ExtractState, now: &str) -> Result<(), StoreError> {
    let need = match state {
        ExtractState::Pending => true,
        ExtractState::Done => !doc_in_human(c, id)?,
        ExtractState::Failed | ExtractState::Unsupported => false,
    };
    if need {
        enqueue_doc(c, id, JobOp::Upsert, now)?;
    }
    Ok(())
}

/// Удаление узла: живые документы узла получают метку удаления узла `stamp` и `delete`.
/// Чанки остаются (скрыты меткой): восстановление возвращает их без повторного извлечения.
pub(crate) fn hide_node_docs(
    c: &Connection,
    node: Id,
    stamp: &str,
    now: &str,
) -> Result<(), StoreError> {
    let ids: Vec<Id> = c
        .prepare_cached(
            "UPDATE attachment_docs SET deleted_at = ?2
             WHERE node_id = ?1 AND deleted_at IS NULL RETURNING id",
        )?
        .query_map(params![node, stamp], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in ids {
        enqueue_doc_delete(c, id, now)?;
    }
    Ok(())
}

/// Восстановление узла (до снятия его метки): документы, скрытые той же меткой, что у узла,
/// снова живые и получают задание ([`requeue_doc`]). Откреплённые раньше (своя метка) — нет.
pub(crate) fn restore_node_docs(c: &Connection, node: Id, now: &str) -> Result<(), StoreError> {
    let docs: Vec<(Id, ExtractState)> = c
        .prepare_cached(
            "UPDATE attachment_docs SET deleted_at = NULL
             WHERE node_id = ?1
               AND deleted_at = (SELECT deleted_at FROM nodes WHERE id = ?1)
             RETURNING id, extract_state",
        )?
        .query_map([node], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, state) in docs {
        requeue_doc(c, id, state, now)?;
    }
    Ok(())
}

/// `context_prefix` чанка вложения: `[<space>/<путь заметки>] <имя файла> > <heading_path>`
/// (спецификация M5 §3); маскируется сканером секретов целиком, как префикс заметки.
pub(crate) fn attachment_prefix(space: &str, path: &str, filename: &str, heading: &str) -> String {
    context_prefix(space, path, filename, heading, &[])
}

/// Перенос узла: `context_prefix` и `embed_key` чанков всех документов узла (в том числе скрытых
/// удалением — восстановление вернёт их с верным путём) — по новому пути; живым документам
/// живого узла — задание ([`requeue_doc`]). Путь узла — уже новый.
pub(crate) fn refresh_node_docs(
    c: &Connection,
    space: &str,
    node: Id,
    now: &str,
) -> Result<(), StoreError> {
    let (path, live): (String, bool) = c
        .prepare_cached("SELECT path, deleted_at IS NULL FROM nodes WHERE id = ?1")?
        .query_row([node], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let docs: Vec<(Id, String, bool, ExtractState)> = c
        .prepare_cached(
            "SELECT id, filename, deleted_at IS NULL, extract_state FROM attachment_docs
             WHERE node_id = ?1",
        )?
        .query_map([node], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut read =
        c.prepare_cached("SELECT seq, heading_path, text FROM chunks WHERE attachment_id = ?1")?;
    let mut upd =
        c.prepare_cached("UPDATE chunks SET context_prefix = ?2, embed_key = ?3 WHERE seq = ?1")?;
    for (id, filename, doc_live, state) in docs {
        let rows: Vec<(i64, String, String)> = read
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (seq, heading, text) in rows {
            let prefix = attachment_prefix(space, &path, &filename, &heading);
            upd.execute(params![seq, prefix, embed_key_of(&prefix, &text)])?;
        }
        if live && doc_live {
            requeue_doc(c, id, state, now)?;
        }
    }
    Ok(())
}

/// Смена GUID решения: живым извлечённым документам пространства — `upsert` (новый
/// `solution_guid` в payload точек).
pub(crate) fn requeue_space_docs(c: &Connection, space: Id, now: &str) -> Result<(), StoreError> {
    let ids: Vec<Id> = c
        .prepare_cached(
            "SELECT d.id FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
             JOIN spaces s ON s.id = n.space_id
             WHERE n.space_id = ?1 AND n.deleted_at IS NULL AND d.deleted_at IS NULL
               AND d.extract_state = 'done' AND s.kind <> 'human'",
        )?
        .query_map([space], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in ids {
        enqueue_doc(c, id, JobOp::Upsert, now)?;
    }
    Ok(())
}

/// Окончательная неудача задания извлечения ([`crate::jobs::fail_job`]): `pending` → `failed`
/// с причиной, если её не записал воркер.
pub(crate) fn mark_job_failed(c: &Connection, id: Id) -> Result<(), StoreError> {
    c.prepare_cached(
        "UPDATE attachment_docs
         SET extract_state = 'failed', extract_error = coalesce(extract_error, 'index_failed')
         WHERE id = ?1 AND extract_state = 'pending'",
    )?
    .execute([id])?;
    Ok(())
}

/// Живые документы вложений узла `node` (по времени привязки).
pub async fn list_attachments(db: &Db, node: Id) -> Result<Vec<AttachmentDoc>, StoreError> {
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT d.id, d.node_id, d.blob_sha256, d.filename, d.mime, coalesce(a.size, 0),
                    d.extract_state, d.extract_error, d.ocr_needed, d.created_at
             FROM attachment_docs d
             LEFT JOIN attachments a ON a.node_id = d.node_id AND a.blob_sha256 = d.blob_sha256
                                    AND a.filename = d.filename
             WHERE d.node_id = ?1 AND d.deleted_at IS NULL
             ORDER BY d.created_at, d.filename",
        )?;
        let rows = st.query_map([node], |r| {
            Ok(AttachmentDoc {
                id: r.get(0)?,
                node_id: r.get(1)?,
                blob_sha256: r.get(2)?,
                filename: r.get(3)?,
                mime: r.get(4)?,
                size: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                extract_state: r.get(6)?,
                extract_error: r.get(7)?,
                ocr_needed: r.get(8)?,
                created_at: ts_at(r, 9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Открепляет вложение (`sha`, `filename`) от живой заметки `node`: строка `attachments`
/// удаляется, `blobs.refcount − 1`, чанки документа удаляются (FTS — триггером), документ
/// получает метку открепления и `delete` (точки). Блоб при `refcount = 0` остаётся (GC — M7).
/// Нет живой заметки или пары → `NotFound`; права — по матрице `certainty` (§7.5).
pub async fn detach(
    db: &Db,
    node: Id,
    sha: &str,
    filename: &str,
    attrib: &Attribution,
) -> Result<(), StoreError> {
    let (sha, name, author) = (sha.to_string(), filename.to_string(), attrib.author_kind);
    db.write(move |t| {
        let certainty: Option<Option<Certainty>> = t
            .query_row(
                "SELECT certainty FROM nodes
                 WHERE id = ?1 AND kind = 'note' AND deleted_at IS NULL",
                [node],
                |r| r.get(0),
            )
            .optional()?;
        let Some(certainty) = certainty else {
            return Err(StoreError::NotFound);
        };
        check_certainty(author, certainty, &CertaintyChange::ModifyOther, None, None)?;
        let gone = t.execute(
            "DELETE FROM attachments WHERE node_id = ?1 AND blob_sha256 = ?2 AND filename = ?3",
            params![node, sha, name],
        )?;
        if gone == 0 {
            return Err(StoreError::NotFound);
        }
        t.execute(
            "UPDATE blobs SET refcount = max(refcount - 1, 0) WHERE sha256 = ?1",
            [&sha],
        )?;
        let now = fmt_ts(Timestamp::now());
        let doc: Option<Id> = t
            .query_row(
                "UPDATE attachment_docs SET deleted_at = ?4
                 WHERE node_id = ?1 AND blob_sha256 = ?2 AND filename = ?3
                   AND deleted_at IS NULL
                 RETURNING id",
                params![node, sha, name, now],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = doc {
            t.prepare_cached("DELETE FROM chunks WHERE attachment_id = ?1")?
                .execute([id])?;
            enqueue_doc_delete(t, id, &now)?;
        }
        // источник выжимок изменился (§9.4)
        invalidate_target(
            t,
            TargetRef {
                kind: TargetKind::Node,
                id: node,
            },
            StaleKind::Changed,
        )?;
        Ok(())
    })
    .await
}

/// Единица в JSON `blob_extract.units` (в столбце — zstd этого JSON, спецификация M5 §3).
#[derive(Serialize, Deserialize)]
struct UnitJson {
    locator: String,
    heading_path: Vec<String>,
    text: String,
}

/// Извлечение блоба `sha` текущей версии [`EXTRACTOR_VERSION`] из кэша. Строка, которую эта
/// версия не разбирает (повреждена), — `None` с предупреждением: кэш выводится заново.
pub async fn extract_cached(db: &Db, sha: &str) -> Result<Option<Extracted>, StoreError> {
    let sha = sha.to_string();
    let row: Option<(String, Vec<u8>, String, bool)> = db
        .read(move |c| {
            Ok(c.prepare_cached(
                "SELECT kind, units, warnings, ocr_needed FROM blob_extract
                 WHERE blob_sha256 = ?1 AND extractor_version = ?2",
            )?
            .query_row(params![sha, EXTRACTOR_VERSION], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?)
        })
        .await?;
    let Some((kind, units, warnings, ocr_needed)) = row else {
        return Ok(None);
    };
    let parsed = (|| {
        let kind = kind_from_str(&kind)?;
        let json = zstd::stream::decode_all(units.as_slice()).ok()?;
        let units: Vec<UnitJson> = serde_json::from_slice(&json).ok()?;
        let units = units
            .into_iter()
            .map(|u| {
                Some(Unit {
                    locator: Locator::parse(&u.locator).ok()?,
                    heading_path: u.heading_path,
                    text: u.text,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let warnings: Vec<String> = serde_json::from_str(&warnings).ok()?;
        Some(Extracted {
            kind,
            units,
            warnings,
            ocr_needed,
        })
    })();
    if parsed.is_none() {
        tracing::warn!("кэш извлечения блоба не разбирается: будет извлечён заново");
    }
    Ok(parsed)
}

/// Кладёт извлечение блоба `sha` в кэш под текущей [`EXTRACTOR_VERSION`] (повтор заменяет).
pub async fn put_extract(db: &Db, sha: &str, e: &Extracted) -> Result<(), StoreError> {
    let units: Vec<UnitJson> = e
        .units
        .iter()
        .map(|u| UnitJson {
            locator: u.locator.render(),
            heading_path: u.heading_path.clone(),
            text: u.text.clone(),
        })
        .collect();
    let json = serde_json::to_vec(&units).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    let units = zstd::bulk::compress(&json, ZSTD_LEVEL)?;
    drop(json);
    let warnings =
        serde_json::to_string(&e.warnings).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    let (sha, kind, ocr) = (sha.to_string(), e.kind.as_str(), e.ocr_needed);
    db.write(move |t| {
        t.prepare_cached(
            "INSERT OR REPLACE INTO blob_extract (blob_sha256, extractor_version, kind, units,
                                                  warnings, ocr_needed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            sha,
            EXTRACTOR_VERSION,
            kind,
            units,
            warnings,
            ocr,
            fmt_ts(Timestamp::now())
        ])?;
        Ok(())
    })
    .await
}

/// Состояние извлечения документа `att` и причина (`err` — без текста документа, длиннее
/// 1000 символов обрезается). `done` переносит `ocr_needed` из кэша извлечения блоба.
/// Нет документа → `NotFound`.
pub async fn set_extract_state(
    db: &Db,
    att: Id,
    state: ExtractState,
    err: Option<&str>,
) -> Result<(), StoreError> {
    let err: Option<String> = err.map(|e| e.chars().take(EXTRACT_ERROR_MAX_CHARS).collect());
    db.write(move |t| {
        let n = t
            .prepare_cached(
                "UPDATE attachment_docs
                 SET extract_state = ?2, extract_error = ?3,
                     ocr_needed = CASE WHEN ?2 = 'done' THEN coalesce(
                         (SELECT x.ocr_needed FROM blob_extract x
                          WHERE x.blob_sha256 = attachment_docs.blob_sha256
                            AND x.extractor_version = ?4), ocr_needed)
                       ELSE ocr_needed END
                 WHERE id = ?1",
            )?
            .execute(params![att, state, err, EXTRACTOR_VERSION])?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    })
    .await
}

/// Детерминированный id чанка вложения: UUIDv5(`attachment_id ‖ ordinal ‖ text_hash`) в hex
/// (32 символа, как у чанков заметок; пространство имён своё — без коллизий с ними).
pub(crate) fn attachment_chunk_id(att: Id, ordinal: i64, text_hash: &str) -> String {
    let mut name = Vec::with_capacity(16 + 8 + text_hash.len());
    name.extend_from_slice(att.as_uuid().as_bytes());
    name.extend_from_slice(&ordinal.to_le_bytes());
    name.extend_from_slice(text_hash.as_bytes());
    Uuid::new_v5(&chunk_namespace(), &name).simple().to_string()
}

/// Строка чанка вложения до вставки.
struct AttChunk {
    id: String,
    heading_path: String,
    ordinal: i64,
    text: String,
    context_prefix: String,
    token_count: i64,
    text_hash: String,
    embed_key: String,
    locator: String,
}

/// Строка `chunks` (колонки — как в [`same_chunks`]) совпадает с `want`.
fn row_same(r: &rusqlite::Row<'_>, want: &AttChunk, sha: &str, node: Id) -> rusqlite::Result<bool> {
    Ok(r.get_ref(0)?.as_str()? == want.id
        && r.get_ref(1)?.as_str()? == want.heading_path
        && r.get::<_, i64>(2)? == want.ordinal
        && r.get_ref(3)?.as_str()? == want.text
        && r.get_ref(4)?.as_str()? == want.context_prefix
        && r.get::<_, i64>(5)? == want.token_count
        && r.get_ref(6)?.as_str()? == want.text_hash
        && r.get::<_, Option<String>>(7)?.as_deref() == Some(want.embed_key.as_str())
        && r.get::<_, Option<String>>(8)?.as_deref() == Some(want.locator.as_str())
        && r.get::<_, Option<String>>(9)?.as_deref() == Some(sha)
        && r.get::<_, Option<Id>>(10)? == Some(node)
        && r.get_ref(11)?.as_str()? == LEVEL_ATTACHMENT
        && r.get::<_, i64>(12)? == DOC_REV)
}

/// Совпадает ли записанная нарезка документа с `rows` (все записываемые столбцы).
fn same_chunks(
    c: &Connection,
    att: Id,
    sha: &str,
    node: Id,
    rows: &[AttChunk],
) -> Result<bool, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT id, heading_path, ordinal, text, context_prefix, token_count, text_hash,
                embed_key, locator, blob_sha256, node_id, level, rev
         FROM chunks WHERE attachment_id = ?1 ORDER BY ordinal",
    )?;
    let mut stored = st.query([att])?;
    for want in rows {
        let Some(r) = stored.next()? else {
            return Ok(false);
        };
        if !row_same(r, want, sha, node)? {
            return Ok(false);
        }
    }
    Ok(stored.next()?.is_none())
}

/// Заменяет чанки документа `att` единицами `units` (по чанку на единицу, в их порядке; нарезка
/// длинных единиц — у вызывающего): `level = 'attachment'`, `locator`, `blob_sha256`,
/// `heading_path` через ` > `, текст с замаскированными секретами (§7.6), префикс
/// [`attachment_prefix`]. Совпадающая нарезка не переписывается. Новая нарезка ставит `upsert`
/// документа (точки Qdrant), кроме Human. Документ откреплён или скрыт удалением узла →
/// `NotFound` (воркер бросает результат: восстановление поставит задание заново).
pub async fn replace_attachment_chunks(db: &Db, att: Id, units: &[Unit]) -> Result<(), StoreError> {
    let units: Vec<(String, String, String)> = units
        .iter()
        .map(|u| {
            (
                u.locator.render(),
                mask_text(&u.heading_path.join(" > ")),
                mask_text(&u.text),
            )
        })
        .collect();
    db.write(move |t| {
        let doc: Option<(Id, String, String, String, String, SpaceKind)> = t
            .prepare_cached(
                "SELECT d.node_id, d.blob_sha256, d.filename, n.path, s.name, s.kind
                 FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
                 JOIN spaces s ON s.id = n.space_id
                 WHERE d.id = ?1 AND d.deleted_at IS NULL AND n.deleted_at IS NULL",
            )?
            .query_row([att], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .optional()?;
        let Some((node, sha, filename, path, space, kind)) = doc else {
            return Err(StoreError::NotFound);
        };
        let tc = token_counter();
        let rows: Vec<AttChunk> = units
            .iter()
            .enumerate()
            .map(|(i, (locator, heading, text))| {
                let ordinal = i as i64;
                let text_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                let prefix = attachment_prefix(&space, &path, &filename, heading);
                AttChunk {
                    id: attachment_chunk_id(att, ordinal, &text_hash),
                    heading_path: heading.clone(),
                    ordinal,
                    embed_key: embed_key_of(&prefix, text),
                    context_prefix: prefix,
                    token_count: tc.count(text) as i64,
                    text_hash,
                    text: text.clone(),
                    locator: locator.clone(),
                }
            })
            .collect();
        if same_chunks(t, att, &sha, node, &rows)? {
            return Ok(());
        }
        t.prepare_cached("DELETE FROM chunks WHERE attachment_id = ?1")?
            .execute([att])?;
        let mut ins = t.prepare_cached(
            "INSERT INTO chunks (id, node_id, attachment_id, rev, level, heading_path, ordinal,
                                 text, context_prefix, token_count, text_hash, embed_key,
                                 blob_sha256, locator)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for r in &rows {
            ins.execute(params![
                r.id,
                node,
                att,
                DOC_REV,
                LEVEL_ATTACHMENT,
                r.heading_path,
                r.ordinal,
                r.text,
                r.context_prefix,
                r.token_count,
                r.text_hash,
                r.embed_key,
                sha,
                r.locator,
            ])?;
        }
        if !tc.is_exact() {
            t.prepare_cached("INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')")?
                .execute([crate::user_db::CHUNKED_APPROX])?;
        }
        if kind != SpaceKind::Human {
            enqueue_doc(t, att, JobOp::Upsert, &fmt_ts(Timestamp::now()))?;
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_id_deterministic_and_distinct() {
        let a = Id::new();
        let b = Id::new();
        let h = "ab".repeat(32);
        assert_eq!(attachment_chunk_id(a, 0, &h), attachment_chunk_id(a, 0, &h));
        assert_ne!(attachment_chunk_id(a, 0, &h), attachment_chunk_id(a, 1, &h));
        assert_ne!(attachment_chunk_id(a, 0, &h), attachment_chunk_id(b, 0, &h));
        // схема заметки (blake3) с тем же входом даёт другой id
        assert_ne!(
            attachment_chunk_id(a, 0, &h),
            crate::nodes::chunk_id(a, 0, &h)
        );
        assert_eq!(attachment_chunk_id(a, 0, &h).len(), 32);
    }

    #[test]
    fn doc_kind_names_roundtrip() {
        for k in [
            DocKind::Pdf,
            DocKind::Docx,
            DocKind::Pptx,
            DocKind::Xlsx,
            DocKind::Xls,
            DocKind::Ods,
            DocKind::Csv,
            DocKind::Text,
        ] {
            assert_eq!(kind_from_str(k.as_str()), Some(k));
        }
        assert_eq!(kind_from_str("doc"), None);
    }
}
