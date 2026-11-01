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
//! **Конвейер (задача 5 M5).** Задание извлечения (`upsert` документа в `pending`,
//! [`crate::claim_extract_jobs`]) выполняет конвейер документов сервера: вид по содержимому,
//! кэш [`extract_cached`] или разбор, [`put_extract`], [`replace_attachment_chunks`] (нарезка
//! единиц, сканер секретов, `done`). Новая нарезка ставит `upsert` ради точек — его выполняет
//! воркер векторного индекса по [`attachment_chunks_for_index`]. Неудача — `failed` с причиной
//! ([`error_code`]); повтор — reindex ([`reindex_docs`]). Отпечаток нарезки вложений
//! ([`rechunk_docs_if_stale`], `meta.chunker_attachments`) включает [`EXTRACTOR_VERSION`]: при
//! расхождении документы снова `pending` (кэш той же версии — без разбора).
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
use crate::jobs::{
    AttachmentPayload, IndexChunk, JobOp, JobOutcome, NodePayload, PRIORITY_ATTACHMENT,
    complete_in, enqueue_target, give_up_in, has_active_target,
};
use crate::nodes::{Attribution, context_prefix, token_counter};
use crate::paths::path_ancestors;
use crate::time::{fmt_ts, ts_at};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Уровень чанка вложения (`chunks.level`).
pub const LEVEL_ATTACHMENT: &str = "attachment";
/// Буква ссылки на фрагмент вложения: `a:<chunk_id>` — карточка поиска, `/read` (с `locator`
/// чанка по умолчанию), сессии, CLI (M5, задача 6). Документ наружу — полем `attachment_id`
/// (внутренняя форма цели — `d:<id>`, [`TargetKind::Attachment`]); блоб — `b:<sha256>`.
pub const CHUNK_REF_LETTER: &str = "a";

/// Ссылка на фрагмент вложения `a:<chunk_id>` ([`CHUNK_REF_LETTER`]).
pub fn chunk_ref(chunk_id: &str) -> String {
    format!("{CHUNK_REF_LETTER}:{chunk_id}")
}

/// Есть ли в пространствах `spaces` хоть один чанк вложения живого документа живой заметки —
/// кандидат точек Qdrant. По нему поиск решает, запускать ли prefetch вложений (раунд правок 2
/// задачи 6 M5). Проверяются чанки, а не `extract_state`: после смены версии извлечения
/// (`rechunk_docs_if_stale`: `done` → `pending`) и при неудаче (`failed`) чанки и точки остаются,
/// и вложение должно искаться (раунд 3). Проход — по частичному индексу `chunks_attachment`.
/// Human вызывающий в `spaces` не передаёт. Пустой список — `false`.
pub async fn has_attachment_chunks(db: &Db, spaces: Vec<Id>) -> Result<bool, StoreError> {
    if spaces.is_empty() {
        return Ok(false);
    }
    let list =
        serde_json::Value::from(spaces.iter().map(Id::to_string).collect::<Vec<_>>()).to_string();
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT EXISTS (SELECT 1 FROM chunks c INDEXED BY chunks_attachment
             JOIN attachment_docs ad ON ad.id = c.attachment_id
             JOIN nodes n ON n.id = c.node_id
             WHERE c.attachment_id IS NOT NULL AND ad.deleted_at IS NULL AND n.deleted_at IS NULL
               AND n.space_id IN (SELECT value FROM json_each(?1)))",
            [list],
            |r| r.get(0),
        )?)
    })
    .await
}

/// Узел-владелец фрагмента вложения `chunk_id` живого документа (`attachment_docs.deleted_at IS
/// NULL`) живой заметки; `space` — только в этом пространстве (участник общего решения видит
/// лишь пространство решения). `None` — нет такого чанка, документ или заметка скрыты, другое
/// пространство. Права проверяет вызывающий (источник выжимки `a:<chunk>`, задача 6 M5).
pub async fn attachment_chunk_owner(
    db: &Db,
    chunk_id: String,
    space: Option<Id>,
) -> Result<Option<Id>, StoreError> {
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            "SELECT c.node_id FROM chunks c
             JOIN attachment_docs ad ON ad.id = c.attachment_id
             JOIN nodes n ON n.id = c.node_id
             WHERE c.id = ?1 AND ad.deleted_at IS NULL AND n.deleted_at IS NULL
               AND (?2 IS NULL OR n.space_id = ?2)",
            rusqlite::params![chunk_id, space],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
}
/// Ревизия заданий и чанков вложения: у документа ревизий нет (содержимое блоба неизменно).
const DOC_REV: i64 = 0;
/// Уровень сжатия zstd `blob_extract.units`: быстрый, текст сжимается в 3–5 раз.
const ZSTD_LEVEL: i32 = 3;
/// Предел длины `extract_error`, символов.
const EXTRACT_ERROR_MAX_CHARS: usize = 1000;
/// Ключ `meta`: отпечаток нарезки вложений базы ([`attachment_chunker_fingerprint`]).
pub const CHUNKER_ATTACHMENTS_KEY: &str = "chunker_attachments";

/// Причина `failed`: pdfium не загружен (PDF не разобрать, пока библиотеки нет; при следующем
/// запуске с pdfium такие документы извлекаются снова).
pub const PDFIUM_UNAVAILABLE: &str = "pdfium_unavailable";
/// Причина `failed`: файла блоба нет на диске.
pub const BLOB_MISSING: &str = "blob_missing";

/// Код причины `extract_error`: часть до `:` (`parse_error: zip: …` → `parse_error`).
pub fn error_code(e: &str) -> &str {
    e.split(':').next().unwrap_or(e).trim()
}

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
        PRIORITY_ATTACHMENT,
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

/// Смена GUID решения: живым документам пространства — `upsert`: извлечённым — новый
/// `solution_guid` в payload точек, `pending` — извлечение (его задание могло быть закрыто
/// воркером до задачи 5 M5 или заменено `delete`).
pub(crate) fn requeue_space_docs(c: &Connection, space: Id, now: &str) -> Result<(), StoreError> {
    let ids: Vec<Id> = c
        .prepare_cached(
            "SELECT d.id FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
             JOIN spaces s ON s.id = n.space_id
             WHERE n.space_id = ?1 AND n.deleted_at IS NULL AND d.deleted_at IS NULL
               AND (d.extract_state = 'pending'
                    OR (d.extract_state = 'done' AND s.kind <> 'human'))",
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
    // закэшированная неудача разбора — не извлечение ([`cached_extract`])
    if kind == KIND_FAILED || kind == KIND_UNSUPPORTED {
        return Ok(None);
    }
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

/// Куски единиц документа: `(locator, heading_path, текст, токенов)`. Значения секретов
/// маскируются до нарезки (секрет не разрезается между кусками), единица длиннее
/// `chunk_hard_max` режется по предложениям ([`zam_markdown::split_plain`]: те же пороги и
/// счётчик токенов, что у заметок); `locator` и `heading_path` у кусков — единицы.
pub fn attachment_pieces(units: &[Unit]) -> Vec<(String, String, String, i64)> {
    let p = crate::nodes::chunk_params();
    let tc = token_counter();
    let mut out = Vec::with_capacity(units.len());
    for u in units {
        let locator = u.locator.render();
        let heading = mask_text(&u.heading_path.join(" > "));
        let text = mask_text(&u.text);
        for r in zam_markdown::split_plain(&text, &p, tc.as_ref()) {
            let piece = text[r].trim();
            if piece.is_empty() {
                continue;
            }
            out.push((
                locator.clone(),
                heading.clone(),
                piece.to_string(),
                tc.count(piece) as i64,
            ));
        }
    }
    out
}

/// Заменяет чанки документа `att` кусками единиц `units` ([`attachment_pieces`], в их порядке):
/// `level = 'attachment'`, `locator`, `blob_sha256`, `heading_path` через ` > `, текст с
/// замаскированными секретами (§7.6), префикс [`attachment_prefix`]; документ — `done`
/// (`ocr_needed` — из кэша извлечения), его неудавшиеся задания снимаются. Совпадающая нарезка не
/// переписывается. Новая нарезка ставит `upsert` документа (точки Qdrant), кроме Human, и
/// отмечает выжимки узла-владельца устаревшими (§9.4: источник изменился). Документ откреплён
/// или скрыт удалением узла → `NotFound` (результат бросается: восстановление поставит задание).
/// Нарезка идёт в пуле блокирующих задач, не в исполнителе tokio.
pub async fn replace_attachment_chunks(db: &Db, att: Id, units: &[Unit]) -> Result<(), StoreError> {
    let pieces = pieces_blocking(units).await?;
    db.write(move |t| {
        if write_chunks(t, att, &pieces)? {
            Ok(())
        } else {
            Err(StoreError::NotFound)
        }
    })
    .await
}

/// Кусок единицы документа: `(locator, heading_path, текст, токенов)`.
type Piece = (String, String, String, i64);

/// [`attachment_pieces`] в пуле блокирующих задач.
async fn pieces_blocking(units: &[Unit]) -> Result<Vec<Piece>, StoreError> {
    let units = units.to_vec();
    tokio::task::spawn_blocking(move || attachment_pieces(&units))
        .await
        .map_err(|e| StoreError::Corrupt(format!("нарезка вложения: {e}")))
}

/// Тело [`replace_attachment_chunks`] в транзакции писателя; `false` — документ скрыт или
/// откреплён (ничего не записано). Документ, который был не `done` (перенарезка, повтор после
/// неудачи, восстановление узла), получает `upsert` ради точек и при той же нарезке: пока он
/// был `pending`, его точки могли удалить (reindex из кэша их не восстанавливает, удаление узла
/// убирает), а задание точек ему не ставилось.
fn write_chunks(t: &Connection, att: Id, units: &[Piece]) -> Result<bool, StoreError> {
    {
        let doc: Option<(Id, String, String, String, String, SpaceKind, ExtractState)> = t
            .prepare_cached(
                "SELECT d.node_id, d.blob_sha256, d.filename, n.path, s.name, s.kind,
                        d.extract_state
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
                    r.get(6)?,
                ))
            })
            .optional()?;
        let Some((node, sha, filename, path, space, kind, prev)) = doc else {
            return Ok(false);
        };
        let points = kind != SpaceKind::Human;
        let now = fmt_ts(Timestamp::now());
        // документ извлечён: `done`, причина прежней неудачи и её задания сняты
        t.prepare_cached(
            "UPDATE attachment_docs
             SET extract_state = 'done', extract_error = NULL,
                 ocr_needed = coalesce((SELECT x.ocr_needed FROM blob_extract x
                                        WHERE x.blob_sha256 = attachment_docs.blob_sha256
                                          AND x.extractor_version = ?2), ocr_needed)
             WHERE id = ?1",
        )?
        .execute(params![att, EXTRACTOR_VERSION])?;
        t.prepare_cached(
            "DELETE FROM index_jobs
             WHERE target_kind = 'attachment' AND target_id = ?1 AND state = 'failed'",
        )?
        .execute([att])?;
        let rows: Vec<AttChunk> = units
            .iter()
            .enumerate()
            .map(|(i, (locator, heading, text, tokens))| {
                let ordinal = i as i64;
                let text_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                let prefix = attachment_prefix(&space, &path, &filename, heading);
                AttChunk {
                    id: attachment_chunk_id(att, ordinal, &text_hash),
                    heading_path: heading.clone(),
                    ordinal,
                    embed_key: embed_key_of(&prefix, text),
                    context_prefix: prefix,
                    token_count: *tokens,
                    text_hash,
                    text: text.clone(),
                    locator: locator.clone(),
                }
            })
            .collect();
        if same_chunks(t, att, &sha, node, &rows)? {
            if prev != ExtractState::Done && points {
                enqueue_doc(t, att, JobOp::Upsert, &now)?;
            }
            return Ok(true);
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
        if !token_counter().is_exact() {
            t.prepare_cached("INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')")?
                .execute([crate::user_db::CHUNKED_APPROX])?;
        }
        if points {
            enqueue_doc(t, att, JobOp::Upsert, &now)?;
        }
        // источник выжимок изменился (§9.4): текст вложения — часть заметки
        invalidate_target(
            t,
            TargetRef {
                kind: TargetKind::Node,
                id: node,
            },
            StaleKind::Changed,
        )?;
        Ok(true)
    }
}

/// Итог задания извлечения документа ([`finish_extract`]).
#[derive(Debug, Clone, PartialEq)]
pub enum ExtractEnd {
    /// Текст извлечён: единицы документа (нарезка, чанки, `done`).
    Units(Vec<Unit>),
    /// Вид не поддерживается.
    Unsupported,
    /// Окончательная неудача: причина для `extract_error` (код и область, без текста документа).
    Failed(String),
}

/// Итог задания извлечения `job_id`/`claim_gen` документа `att` **одной транзакцией**: чанки и
/// `done` | `unsupported` | `failed` с причиной — вместе с закрытием задания (неудача — задание
/// сразу `failed`, оно в `failed_items`). Между записью состояния и закрытием задания не
/// вклинится сброс заданий после аварии. Документ скрыт или откреплён — результат бросается,
/// задание закрывается. Нарезка — в пуле блокирующих задач до транзакции.
pub async fn finish_extract(
    db: &Db,
    job_id: i64,
    claim_gen: i64,
    att: Id,
    end: ExtractEnd,
) -> Result<JobOutcome, StoreError> {
    let pieces = match &end {
        ExtractEnd::Units(u) => Some(pieces_blocking(u).await?),
        ExtractEnd::Unsupported | ExtractEnd::Failed(_) => None,
    };
    db.write(move |t| match end {
        ExtractEnd::Units(_) => {
            write_chunks(t, att, pieces.as_deref().unwrap_or_default())?;
            complete_in(t, job_id, claim_gen)
        }
        ExtractEnd::Unsupported => {
            t.prepare_cached(
                "UPDATE attachment_docs SET extract_state = 'unsupported', extract_error = NULL
                 WHERE id = ?1",
            )?
            .execute([att])?;
            complete_in(t, job_id, claim_gen)
        }
        ExtractEnd::Failed(reason) => {
            let reason: String = reason.chars().take(EXTRACT_ERROR_MAX_CHARS).collect();
            t.prepare_cached(
                "UPDATE attachment_docs SET extract_state = 'failed', extract_error = ?2
                 WHERE id = ?1",
            )?
            .execute(params![att, reason])?;
            give_up_in(t, job_id, claim_gen, &reason)
        }
    })
    .await
}

/// Вид строки кэша `blob_extract` с детерминированной неудачей разбора (не единицы).
const KIND_FAILED: &str = "failed";
const KIND_UNSUPPORTED: &str = "unsupported";

/// Извлечение блоба из кэша с учётом закэшированных неудач.
#[derive(Debug, Clone, PartialEq)]
pub enum CachedExtract {
    Extracted(Extracted),
    /// Разбор этой версии извлечения не удался детерминированно: причина.
    Failed(String),
    /// Разборщик этой версии вид не поддерживает.
    Unsupported,
}

/// Детерминированная неудача разбора блоба `sha` текущей версии извлечения — в кэш (повтор того
/// же файла в другой заметке не разбирается): `reason` — `parse_error: …` (код и область), `None`
/// — вид не поддерживается. Сбои процесса (`parse_crash`, `parse_timeout`, `parse_oom`) и
/// отсутствие pdfium не кэшируются: они не свойство файла.
pub async fn put_extract_failure(
    db: &Db,
    sha: &str,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    let (kind, warnings) = match reason {
        Some(r) => (KIND_FAILED, vec![r.to_string()]),
        None => (KIND_UNSUPPORTED, Vec::new()),
    };
    let warnings =
        serde_json::to_string(&warnings).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    let units = zstd::bulk::compress(b"[]", ZSTD_LEVEL)?;
    let sha = sha.to_string();
    db.write(move |t| {
        t.prepare_cached(
            "INSERT OR REPLACE INTO blob_extract (blob_sha256, extractor_version, kind, units,
                                                  warnings, ocr_needed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
        )?
        .execute(params![
            sha,
            EXTRACTOR_VERSION,
            kind,
            units,
            warnings,
            fmt_ts(Timestamp::now())
        ])?;
        Ok(())
    })
    .await
}

/// Кэш извлечения блоба `sha` текущей версии: единицы или закэшированная неудача.
pub async fn cached_extract(db: &Db, sha: &str) -> Result<Option<CachedExtract>, StoreError> {
    let sha2 = sha.to_string();
    let row: Option<(String, String)> = db
        .read(move |c| {
            Ok(c.prepare_cached(
                "SELECT kind, warnings FROM blob_extract
                 WHERE blob_sha256 = ?1 AND extractor_version = ?2",
            )?
            .query_row(params![sha2, EXTRACTOR_VERSION], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
        })
        .await?;
    match row {
        None => Ok(None),
        Some((k, _)) if k == KIND_UNSUPPORTED => Ok(Some(CachedExtract::Unsupported)),
        Some((k, w)) if k == KIND_FAILED => {
            let reason = serde_json::from_str::<Vec<String>>(&w)
                .ok()
                .and_then(|v| v.into_iter().next())
                .unwrap_or_else(|| "parse_error".into());
            Ok(Some(CachedExtract::Failed(reason)))
        }
        Some(_) => Ok(extract_cached(db, sha).await?.map(CachedExtract::Extracted)),
    }
}

/// Что нужно конвейеру документов для задания извлечения документа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractTarget {
    pub node_id: Id,
    pub blob_sha256: String,
    pub filename: String,
    /// Документ и узел-владелец живы (иначе извлекать незачем).
    pub live: bool,
    pub state: ExtractState,
}

/// Документ `att` для извлечения; `None` — документа нет.
pub async fn extract_target(db: &Db, att: Id) -> Result<Option<ExtractTarget>, StoreError> {
    db.read(move |c| {
        Ok(c.prepare_cached(
            "SELECT d.node_id, d.blob_sha256, d.filename,
                    d.deleted_at IS NULL AND n.deleted_at IS NULL, d.extract_state
             FROM attachment_docs d JOIN nodes n ON n.id = d.node_id WHERE d.id = ?1",
        )?
        .query_row([att], |r| {
            Ok(ExtractTarget {
                node_id: r.get(0)?,
                blob_sha256: r.get(1)?,
                filename: r.get(2)?,
                live: r.get(3)?,
                state: r.get(4)?,
            })
        })
        .optional()?)
    })
    .await
}

/// Поля payload документа (живого, извлечённого, не Human, заметка не `index: false`); `None`
/// — точек у документа быть не должно. Нет документа → `NotFound`.
fn doc_payload(
    c: &Connection,
    att: Id,
) -> Result<Option<(NodePayload, AttachmentPayload)>, StoreError> {
    let row = c
        .prepare_cached(
            "SELECT d.node_id, n.space_id, s.kind, s.solution_guid, n.path, n.area, d.created_at,
                    d.blob_sha256,
                    d.deleted_at IS NULL AND n.deleted_at IS NULL AND d.extract_state = 'done'
                      AND n.index_state <> 'excluded' AND s.kind <> 'human'
             FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
             JOIN spaces s ON s.id = n.space_id WHERE d.id = ?1",
        )?
        .query_row([att], |r| {
            Ok((
                NodePayload {
                    node_id: r.get(0)?,
                    space_id: r.get(1)?,
                    space_kind: r.get(2)?,
                    solution_guid: r.get(3)?,
                    path: r.get(4)?,
                    path_ancestors: Vec::new(),
                    tags: Vec::new(),
                    area: r.get(5)?,
                    updated_at: ts_at(r, 6)?,
                    certainty: None,
                },
                r.get::<_, String>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })
        .optional()?;
    let Some((mut node, sha, indexable)) = row else {
        return Err(StoreError::NotFound);
    };
    // Human — только FTS (инвариант): точек нет, даже если задание поставлено в обход
    if !indexable || node.space_kind == SpaceKind::Human {
        return Ok(None);
    }
    node.path_ancestors = path_ancestors(&node.path);
    Ok(Some((
        node,
        AttachmentPayload {
            attachment_id: att,
            blob_sha256: sha,
            locator: None,
        },
    )))
}

/// Чанки документа `att` с полями payload (`node` — узел-владелец).
fn doc_chunks(
    c: &Connection,
    node: &NodePayload,
    pl: &AttachmentPayload,
) -> Result<Vec<IndexChunk>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT id, level, heading_path, ordinal, text, context_prefix, text_hash, token_count,
                rev, embed_key, locator
         FROM chunks WHERE attachment_id = ?1 ORDER BY ordinal",
    )?;
    let rows = st.query_map([pl.attachment_id], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ),
            (
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let ((id, level, heading_path, ordinal), (text, prefix, hash, tokens, rev, key, loc)) =
            row?;
        let Some(level) = crate::jobs::parse_level(&level) else {
            continue;
        };
        let embed_key = key
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| embed_key_of(&prefix, &text));
        out.push(IndexChunk {
            id,
            level,
            heading_path,
            ordinal,
            text,
            context_prefix: prefix,
            text_hash: hash,
            embed_key,
            token_count: tokens,
            rev,
            node: node.clone(),
            entry: None,
            attachment: Some(AttachmentPayload {
                locator: loc,
                ..pl.clone()
            }),
        });
    }
    Ok(out)
}

/// Чанки документа вложения для векторного индекса с полями payload: `kind = attachment`,
/// `node_id` — узел-владелец (область, права), `attachment_id`, `blob_sha256`, `locator`.
/// Документ не извлечён, скрыт, откреплён, в Human или у заметки `index: false` — пустой
/// список (точки убираются). Нет документа → `NotFound`.
pub async fn attachment_chunks_for_index(db: &Db, att: Id) -> Result<Vec<IndexChunk>, StoreError> {
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        match doc_payload(&tx, att)? {
            Some((node, pl)) => doc_chunks(&tx, &node, &pl),
            None => Ok(Vec::new()),
        }
    })
    .await
}

/// Документ вложения для пересборки индекса из кэша: его чанки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexAttachment {
    pub attachment_id: Id,
    pub chunks: Vec<IndexChunk>,
}

/// Страница живых извлечённых документов (не Human, заметка индексируется) пространства
/// `space` (`None` — всех) с чанками: по возрастанию id после `after`, не больше `limit`.
pub async fn reindex_attachments_page(
    db: &Db,
    space: Option<Id>,
    after: Option<Id>,
    limit: usize,
) -> Result<Vec<ReindexAttachment>, StoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let ids: Vec<Id> = {
            let mut st = tx.prepare_cached(
                "SELECT d.id FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
                 JOIN spaces s ON s.id = n.space_id
                 WHERE d.extract_state = 'done' AND d.deleted_at IS NULL
                   AND n.deleted_at IS NULL AND n.index_state <> 'excluded'
                   AND s.kind <> 'human'
                   AND (?1 IS NULL OR n.space_id = ?1) AND (?2 IS NULL OR d.id > ?2)
                 ORDER BY d.id LIMIT ?3",
            )?;
            st.query_map(params![space, after, limit], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let chunks = match doc_payload(&tx, id)? {
                Some((node, pl)) => doc_chunks(&tx, &node, &pl)?,
                None => Vec::new(),
            };
            out.push(ReindexAttachment {
                attachment_id: id,
                chunks,
            });
        }
        Ok(out)
    })
    .await
}

/// `upsert` (точки) документам `ids` — промахам кэша эмбеддингов при пересборке индекса.
pub async fn enqueue_attachment_upserts(db: &Db, ids: Vec<Id>) -> Result<u64, StoreError> {
    if ids.is_empty() {
        return Ok(0);
    }
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        for id in &ids {
            enqueue_doc(tx, *id, JobOp::Upsert, &now)?;
        }
        Ok(ids.len() as u64)
    })
    .await
}

/// Живые документы пространства `space` (`None` — всех) для полного reindex и пересборки
/// коллекции: извлечённые (не Human) — `upsert` ради точек; неудавшиеся (кроме `too_large`:
/// файл не изменится) и неподдерживаемые — снова `pending` с заданием (повтор разбора,
/// закэшированные неудачи их блобов снимаются); `pending` без активного задания — задание. Возвращает число
/// поставленных заданий.
pub async fn reindex_docs(db: &Db, space: Option<Id>) -> Result<u64, StoreError> {
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        // полный reindex — способ повтора разбора (спецификация §2): закэшированные неудачи
        // блобов области снимаются, `failed` (кроме `too_large`) и `unsupported` — снова `pending`
        tx.prepare_cached(&format!(
            "DELETE FROM blob_extract
             WHERE kind IN ('{KIND_FAILED}', '{KIND_UNSUPPORTED}')
               AND blob_sha256 IN (
                   SELECT d.blob_sha256 FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
                   WHERE d.deleted_at IS NULL AND n.deleted_at IS NULL
                     AND (?1 IS NULL OR n.space_id = ?1))"
        ))?
        .execute(params![space])?;
        tx.prepare_cached(
            "UPDATE attachment_docs SET extract_state = 'pending', extract_error = NULL
             WHERE (extract_state = 'unsupported'
                    OR (extract_state = 'failed' AND coalesce(extract_error, '') <> ?2))
               AND deleted_at IS NULL
               AND node_id IN (SELECT id FROM nodes
                               WHERE deleted_at IS NULL AND (?1 IS NULL OR space_id = ?1))",
        )?
        .execute(params![space, TOO_LARGE])?;
        let ids: Vec<(Id, bool)> = tx
            .prepare_cached(
                "SELECT d.id, d.extract_state = 'pending' FROM attachment_docs d
                 JOIN nodes n ON n.id = d.node_id JOIN spaces s ON s.id = n.space_id
                 WHERE d.deleted_at IS NULL AND n.deleted_at IS NULL
                   AND (?1 IS NULL OR n.space_id = ?1)
                   AND (d.extract_state = 'pending'
                        OR (d.extract_state = 'done' AND s.kind <> 'human'))
                 ORDER BY d.id",
            )?
            .query_map(params![space], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut n = 0u64;
        for (id, pending) in ids {
            if pending && has_active_target(tx, att_ref(id))? {
                continue;
            }
            enqueue_doc(tx, id, JobOp::Upsert, &now)?;
            n += 1;
        }
        Ok(n)
    })
    .await
}

/// Запуск конвейера документов: живым документам `pending` без активного задания — задание
/// извлечения. Такие остаются после воркера до задачи 5 M5 (задание закрывалось пустым
/// `delete`) и дозаполнения миграции 0019. Возвращает их число.
pub async fn ensure_extract_jobs(db: &Db) -> Result<u64, StoreError> {
    db.write(|tx| {
        let now = fmt_ts(Timestamp::now());
        let ids: Vec<Id> = tx
            .prepare_cached(
                "SELECT d.id FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
                 WHERE d.extract_state = 'pending' AND d.deleted_at IS NULL
                   AND n.deleted_at IS NULL
                   AND NOT EXISTS (SELECT 1 FROM index_jobs j
                                   WHERE j.target_kind = 'attachment' AND j.target_id = d.id
                                     AND j.state IN ('queued', 'running'))",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in &ids {
            enqueue_doc(tx, *id, JobOp::Upsert, &now)?;
        }
        Ok(ids.len() as u64)
    })
    .await
}

/// Есть ли живые документы `failed` с кодом причины `code` ([`error_code`]).
pub async fn has_failed_with(db: &Db, code: &str) -> Result<bool, StoreError> {
    let like = format!("{code}%");
    db.read(move |c| {
        Ok(c.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM attachment_docs
                            WHERE extract_state = 'failed' AND deleted_at IS NULL
                              AND extract_error LIKE ?1)",
        )?
        .query_row([like], |r| r.get(0))?)
    })
    .await
}

/// Живые документы `failed` с кодом причины `code` — снова `pending` с заданием (причина
/// устранена: например, pdfium теперь загружается). Возвращает их число.
pub async fn requeue_failed_with(db: &Db, code: &str) -> Result<u64, StoreError> {
    let like = format!("{code}%");
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let ids: Vec<Id> = tx
            .prepare_cached(
                "UPDATE attachment_docs SET extract_state = 'pending', extract_error = NULL
                 WHERE extract_state = 'failed' AND deleted_at IS NULL AND extract_error LIKE ?1
                   AND node_id IN (SELECT id FROM nodes WHERE deleted_at IS NULL)
                 RETURNING id",
            )?
            .query_map([like], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in &ids {
            enqueue_doc(tx, *id, JobOp::Upsert, &now)?;
        }
        Ok(ids.len() as u64)
    })
    .await
}

/// Отпечаток нарезки вложений процесса: пороги и флаги нарезки (как у заметок), версия
/// извлечения [`EXTRACTOR_VERSION`] и токенизатор (`;tok=…` последним — сравнение
/// [`crate::nodes::same_chunker`]).
pub fn attachment_chunker_fingerprint() -> String {
    format!(
        "{};ext={EXTRACTOR_VERSION};tok={}",
        crate::nodes::chunker_base(),
        token_counter().tokenizer_id()
    )
}

/// При открытии базы: отпечаток нарезки вложений (`meta.chunker_attachments`) расходится с
/// текущим — документы снова `pending` и живым ставится задание извлечения: та же версия
/// извлечения — перенарезка из кэша без разбора, другая — разбор заново. Не трогаются `pending`
/// (их задание и так впереди) и `failed`/`too_large` (файл не изменится). Скрытые удалением
/// узла получают задание при восстановлении. Работа — у конвейера документов (здесь только
/// состояние и задания в одной транзакции), поэтому не фоновая перенарезка с баннером.
/// Возвращает число документов.
pub async fn rechunk_docs_if_stale(db: &Db) -> Result<u64, StoreError> {
    let current = attachment_chunker_fingerprint();
    let exact = crate::nodes::token_counter_exact();
    let n = db
        .write(move |tx| {
            let stored: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key = ?1",
                    [CHUNKER_ATTACHMENTS_KEY],
                    |r| r.get(0),
                )
                .optional()?;
            if crate::nodes::same_chunker(stored.as_deref(), &current, exact) {
                return Ok(0);
            }
            let ids: Vec<(Id, bool)> = tx
                .prepare_cached(
                    "UPDATE attachment_docs SET extract_state = 'pending', extract_error = NULL
                     WHERE extract_state IN ('done', 'unsupported', 'failed')
                       AND NOT (extract_state = 'failed' AND coalesce(extract_error, '') = ?1)
                     RETURNING id, deleted_at IS NULL
                       AND (SELECT deleted_at IS NULL FROM nodes WHERE id = node_id)",
                )?
                .query_map([TOO_LARGE], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let now = fmt_ts(Timestamp::now());
            for (id, live) in &ids {
                if *live {
                    enqueue_doc(tx, *id, JobOp::Upsert, &now)?;
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![CHUNKER_ATTACHMENTS_KEY, current],
            )?;
            Ok(ids.len() as u64)
        })
        .await?;
    if n > 0 {
        tracing::info!(
            docs = n,
            "нарезка вложений изменилась: документы извлекаются заново (из кэша, если он подходит)"
        );
    }
    Ok(n)
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
