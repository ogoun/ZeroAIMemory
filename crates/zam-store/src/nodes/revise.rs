//! Правка существующей заметки (§11.3 ТЗ): замена текста с проверкой ревизии
//! (`If-Match`), PATCH секций относительно `base_rev` и откат к старой ревизии. PATCH по виду
//! текста пишущего ([`patch_note_viewed`], [`ViewedText`]) — агент правит заметку по маске
//! `‹secret›`, значения возвращает сервер (задача 6 M7.1).
//!
//! Каждая операция — одна транзакция писателя. Новая ревизия только дописывается
//! (§4.1 п.4); frontmatter, L0, `content_hash`, размер и чанки пересчитываются так же,
//! как при создании. Текст с тем же `content_hash`, что у текущей ревизии, новой
//! ревизии не создаёт — возвращается текущий узел.

use rusqlite::{Connection, params};
use zam_core::{Id, TargetKind, TargetRef};
use zam_markdown::{PatchError, PatchOp, apply_patch, section_bytes};

use super::chunks::{index_state, note_meta, space_kind};
use super::idempotency::{NodeKey, NodeOp, opt_part};
use super::write::store_revision;
use super::{Attribution, AuthorKind, Node, NodeKind, load_node, revision_text, sha256_hex};
use crate::cache::StaleKind;
use crate::certainty::{CertaintyChange, check_certainty, check_final_state};
use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Ошибка правки заметки. Сервер отображает `RevMismatch` и `SectionChanged` в 409,
/// `Patch` — в 422, `TooLarge` — в 413. Сообщения не содержат текста заметки (его нельзя
/// писать в логи); `Debug` тоже: текст `RevMismatch::current` в нём скрыт.
#[derive(thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// `base_rev` не совпадает с текущей ревизией; `current` — полный текст текущей.
    #[error("ревизия устарела: текущая ревизия {current_rev}")]
    RevMismatch { current_rev: i64, current: String },
    #[error("операция PATCH не применима: {0}")]
    Patch(#[from] PatchError),
    /// Затронутая секция изменилась после `base_rev`.
    #[error("секция изменена после базовой ревизии: текущая ревизия {current_rev}")]
    SectionChanged { section: String, current_rev: i64 },
    /// Итоговый текст PATCH длиннее предела (байт).
    #[error("заметка после правки — {size} байт, предел {max}")]
    TooLarge { size: usize, max: usize },
}

// Вручную: текст заметки (`RevMismatch::current`) не должен попасть в отладочный вывод.
impl std::fmt::Debug for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Store(e) => f.debug_tuple("Store").field(e).finish(),
            WriteError::RevMismatch {
                current_rev,
                current,
            } => f
                .debug_struct("RevMismatch")
                .field("current_rev", current_rev)
                .field("current_len", &current.len())
                .finish_non_exhaustive(),
            WriteError::Patch(e) => f.debug_tuple("Patch").field(e).finish(),
            WriteError::SectionChanged {
                section,
                current_rev,
            } => f
                .debug_struct("SectionChanged")
                .field("section", section)
                .field("current_rev", current_rev)
                .finish(),
            WriteError::TooLarge { size, max } => f
                .debug_struct("TooLarge")
                .field("size", size)
                .field("max", max)
                .finish(),
        }
    }
}

/// Заменяет текст заметки. `base_rev` должен совпадать с текущей ревизией, иначе
/// `RevMismatch` с текущим текстом. `l0 = Some` — сводка агента; `None` — L0
/// пересчитывается по тексту (прежняя сводка агента не сохраняется, §8.3).
/// Нет узла или он удалён → `NotFound`; папка → `BadInput`.
pub async fn update_note(
    db: &Db,
    id: Id,
    base_rev: i64,
    content: String,
    l0: Option<String>,
    a: Attribution,
) -> Result<Node, WriteError> {
    update_note_keyed(db, id, base_rev, content, l0, a, None)
        .await
        .map(|(n, _)| n)
}

/// [`update_note`] с `Idempotency-Key` (`key`, [`super::idempotency`]): повтор того же запроса —
/// узел в текущем виде и `false` (без новой ревизии и без проверки `base_rev`); другой запрос с
/// тем же ключом — `IdempotencyMismatch`. Иначе — правка и `true`.
pub async fn update_note_keyed(
    db: &Db,
    id: Id,
    base_rev: i64,
    content: String,
    l0: Option<String>,
    a: Attribution,
    key: Option<String>,
) -> Result<(Node, bool), WriteError> {
    write_note(db, move |tx| {
        let k = NodeKey::new(
            key,
            &a,
            NodeOp::Replace,
            &[
                id.to_string().as_bytes(),
                &base_rev.to_le_bytes(),
                content.as_bytes(),
                &opt_part(l0.as_deref()),
            ],
        )?;
        if let Some(k) = &k
            && let Some(n) = k.replay(tx)?
        {
            return Ok((n, false));
        }
        let node = live_note(tx, id)?;
        if base_rev != node.current_rev {
            return Err(rev_mismatch(tx, &node)?);
        }
        let n = commit_text(tx, node, &content, l0.as_deref(), &a)?;
        if let Some(k) = &k {
            k.remember(tx, n.id)?;
        }
        Ok((n, true))
    })
    .await
}

/// Применяет операции PATCH последовательно к тексту текущей ревизии.
///
/// Для операции с секцией сравниваются байты секции в `base_rev` и в текущей
/// ревизии (до применения всей цепочки): различаются (или секция есть только в
/// одной из них) → `SectionChanged`. При `base_rev == current_rev` проверка не
/// нужна; `Append` и `Prepend` применяются всегда. `base_rev` новее текущей →
/// `RevMismatch`, такой ревизии нет → `NotFound`. Любая ошибка — ничего не записано.
pub async fn patch_note(
    db: &Db,
    id: Id,
    base_rev: i64,
    ops: Vec<PatchOp>,
    a: Attribution,
) -> Result<Node, WriteError> {
    patch_note_within(db, id, base_rev, ops, a, usize::MAX).await
}

/// [`patch_note`] с пределом размера итогового текста: длиннее `max_bytes` →
/// [`WriteError::TooLarge`], ничего не записано.
pub async fn patch_note_within(
    db: &Db,
    id: Id,
    base_rev: i64,
    ops: Vec<PatchOp>,
    a: Attribution,
    max_bytes: usize,
) -> Result<Node, WriteError> {
    patch_note_keyed(db, id, base_rev, ops, a, max_bytes, None)
        .await
        .map(|(n, _)| n)
}

/// [`patch_note_within`] с `Idempotency-Key` (`key`): повтор того же запроса — узел в текущем
/// виде и `false` (операции второй раз не применяются: два `append` — одна вставка); другой
/// запрос с тем же ключом — `IdempotencyMismatch`. Иначе — правка и `true`.
pub async fn patch_note_keyed(
    db: &Db,
    id: Id,
    base_rev: i64,
    ops: Vec<PatchOp>,
    a: Attribution,
    max_bytes: usize,
    key: Option<String>,
) -> Result<(Node, bool), WriteError> {
    patch_note_viewed(db, id, base_rev, ops, a, max_bytes, key, None)
        .await
        .map(|p| (p.node, p.fresh))
}

/// Текст ревизии, каким его видит пишущий (спецификация M7 §4.3.2, задача 6 M7.1): агент по
/// токену читает заметку с `‹secret›` вместо значений и адресует операции PATCH по этому виду.
/// Хранилище не знает, что это маска: вид и обратный путь даёт сервер.
pub trait ViewedText {
    /// Текст ревизии для пишущего.
    fn view(&self) -> &str;
    /// Правленный вид `edited` — обратно в текст хранилища: текст и число мест вида без пары в
    /// исходнике (у маски — меток `‹secret›`, записанных как есть).
    fn write_back(&self, edited: &str) -> (String, usize);
}

/// Строит [`ViewedText`] по тексту ревизии (`fn`: без состояния, можно в транзакцию писателя).
pub type TextView = for<'a> fn(&'a str) -> Box<dyn ViewedText + 'a>;

/// Итог [`patch_note_viewed`].
#[derive(Debug)]
pub struct Patched {
    pub node: Node,
    /// `false` — повтор по `Idempotency-Key`: операции второй раз не применялись.
    pub fresh: bool,
    /// Мест вида без пары в исходнике ([`ViewedText::write_back`]); без вида и у повтора — 0.
    pub kept: usize,
}

/// [`patch_note_keyed`] по виду текста `view` (M7 §4.3.2): операции применяются к виду текущей
/// ревизии, проверка «секция не менялась после `base_rev`» сравнивает виды обеих ревизий
/// (заголовки пишущий видел в виде), итог возвращается к исходнику текущей ревизии
/// ([`ViewedText::write_back`]); предел `max_bytes` — по итоговому тексту. `None` — как
/// [`patch_note_keyed`]. Отпечаток `Idempotency-Key` — по операциям запроса.
#[allow(clippy::too_many_arguments)]
pub async fn patch_note_viewed(
    db: &Db,
    id: Id,
    base_rev: i64,
    ops: Vec<PatchOp>,
    a: Attribution,
    max_bytes: usize,
    key: Option<String>,
    view: Option<TextView>,
) -> Result<Patched, WriteError> {
    let ops_bytes = match key {
        Some(_) => serde_json::to_vec(&ops).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        None => Vec::new(),
    };
    write_note(db, move |tx| {
        let k = NodeKey::new(
            key,
            &a,
            NodeOp::Patch,
            &[
                id.to_string().as_bytes(),
                &base_rev.to_le_bytes(),
                &ops_bytes,
            ],
        )?;
        if let Some(k) = &k
            && let Some(node) = k.replay(tx)?
        {
            return Ok(Patched {
                node,
                fresh: false,
                kept: 0,
            });
        }
        let (node, kept) = patch_tx(tx, id, base_rev, &ops, &a, max_bytes, view)?;
        if let Some(k) = &k {
            k.remember(tx, node.id)?;
        }
        Ok(Patched {
            node,
            fresh: true,
            kept,
        })
    })
    .await
}

/// Тело [`patch_note_viewed`] в транзакции писателя: узел и число мест вида без пары.
fn patch_tx(
    tx: &Connection,
    id: Id,
    base_rev: i64,
    ops: &[PatchOp],
    a: &Attribution,
    max_bytes: usize,
    view: Option<TextView>,
) -> Result<(Node, usize), WriteError> {
    let node = live_note(tx, id)?;
    if ops.is_empty() {
        return Err(StoreError::BadInput("пустой список операций PATCH".into()).into());
    }
    let cur_rev = node.current_rev;
    if base_rev > cur_rev {
        return Err(rev_mismatch(tx, &node)?);
    }
    let current = revision_text(tx, id, cur_rev)?;
    let base = if base_rev == cur_rev {
        None
    } else {
        Some(revision_text(tx, id, base_rev)?)
    };
    // вид пишущего: операции и проверка секций — по нему (без вида — по исходнику)
    let current_seen = view.map(|v| v(&current));
    let base_seen = match (view, &base) {
        (Some(v), Some(b)) => Some(v(b)),
        _ => None,
    };
    let current_text = current_seen.as_ref().map_or(current.as_str(), |s| s.view());
    let base_text = match &base_seen {
        Some(s) => Some(s.view()),
        None => base.as_deref(),
    };
    let mut text = current_text.to_string();
    for op in ops {
        if let (Some(base), Some(section)) = (base_text, op_section(op))
            && section_bytes(base, section) != section_bytes(current_text, section)
        {
            return Err(WriteError::SectionChanged {
                section: section.to_string(),
                current_rev: cur_rev,
            });
        }
        text = apply_patch(&text, op)?.text;
    }
    let (text, kept) = match &current_seen {
        Some(s) => s.write_back(&text),
        None => (text, 0),
    };
    if text.len() > max_bytes {
        return Err(WriteError::TooLarge {
            size: text.len(),
            max: max_bytes,
        });
    }
    Ok((commit_text(tx, node, &text, None, a)?, kept))
}

/// Откат: новая ревизия с текстом ревизии `to_rev`, L0 пересчитывается по тексту.
/// Нет узла, он удалён или нет такой ревизии → `NotFound`; папка → `BadInput`.
/// Текст `to_rev` совпадает с текущим → новой ревизии нет.
pub async fn revert_note(db: &Db, id: Id, to_rev: i64, a: Attribution) -> Result<Node, StoreError> {
    db.write(move |tx| {
        let node = live_note(tx, id)?;
        let text = revision_text(tx, id, to_rev)?;
        commit_text(tx, node, &text, None, &a)
    })
    .await
}

/// Транзакция писателя для операции с [`WriteError`]: ошибки хранилища идут через
/// `Db::write`, конфликты возвращаются как значение. Конфликт выявляется до любой
/// записи, поэтому фиксация такой транзакции ничего не меняет.
async fn write_note<T: Send + 'static>(
    db: &Db,
    f: impl FnOnce(&Connection) -> Result<T, WriteError> + Send + 'static,
) -> Result<T, WriteError> {
    db.write(move |tx| match f(tx) {
        Ok(n) => Ok(Ok(n)),
        Err(WriteError::Store(e)) => Err(e),
        Err(e) => Ok(Err(e)),
    })
    .await?
}

/// Изменение заметки по матрице `certainty` (§7.5; с M4 заметку `axiom` правит любой автор):
/// проверка по текущей ревизии до любой записи ([`commit_text`] — общий шаг `PUT`, `PATCH`,
/// `revert`). Конфликт ревизии (`If-Match`/`base_rev` устарел — 409) и ошибки PATCH
/// проверяются раньше.
fn ensure_may_modify(node: &Node, author: AuthorKind) -> Result<(), StoreError> {
    Ok(check_certainty(
        author,
        node.certainty,
        &CertaintyChange::ModifyOther,
        None,
        None,
    )?)
}

/// Живая заметка: нет или удалена → `NotFound`, папка → `BadInput`.
fn live_note(c: &Connection, id: Id) -> Result<Node, StoreError> {
    let node = load_node(c, id)?;
    if node.deleted_at.is_some() {
        return Err(StoreError::NotFound);
    }
    if node.kind != NodeKind::Note {
        return Err(StoreError::BadInput("узел — папка, а не заметка".into()));
    }
    Ok(node)
}

fn rev_mismatch(c: &Connection, node: &Node) -> Result<WriteError, StoreError> {
    Ok(WriteError::RevMismatch {
        current_rev: node.current_rev,
        current: revision_text(c, node.id, node.current_rev)?,
    })
}

/// Путь секции операции; `None` у `Append` и `Prepend`.
fn op_section(op: &PatchOp) -> Option<&str> {
    match op {
        PatchOp::ReplaceSection { section, .. }
        | PatchOp::AppendToSection { section, .. }
        | PatchOp::InsertAfterHeading { section, .. } => Some(section),
        PatchOp::Append { .. } | PatchOp::Prepend { .. } => None,
    }
}

/// Записывает `content` новой ревизией заметки `node` (если текст изменился). Смена
/// `certainty:` — по матрице прав с причиной из сообщения ревизии (`message`).
fn commit_text(
    tx: &Connection,
    node: Node,
    content: &str,
    agent_l0: Option<&str>,
    a: &Attribution,
) -> Result<Node, StoreError> {
    ensure_may_modify(&node, a.author_kind)?;
    let hash = sha256_hex(content.as_bytes());
    if node.content_hash.as_deref() == Some(hash.as_str()) {
        return Ok(node);
    }
    let now = fmt_ts(crate::time::now());
    let meta = note_meta(content, agent_l0);
    let certainty = meta.certainty()?;
    // theorem агента — с непустым evidence: и после правки
    check_final_state(a.author_kind, certainty, meta.evidence.as_ref())?;
    if certainty != node.certainty {
        check_certainty(
            a.author_kind,
            node.certainty,
            &CertaintyChange::Transition {
                from: node.certainty,
                to: certainty,
            },
            meta.evidence.as_ref(),
            a.message.as_deref(),
        )?;
    }
    let kind = space_kind(tx, node.space_id)?;
    let rev = node.current_rev + 1;
    tx.execute(
        "UPDATE nodes SET current_rev = ?2, content_hash = ?3, frontmatter = ?4, l0 = ?5,
                          l0_source = ?6, index_state = ?7, updated_at = ?8, certainty = ?9
         WHERE id = ?1",
        params![
            node.id,
            rev,
            hash,
            meta.frontmatter.as_ref().map(|v| v.to_string()),
            meta.l0,
            meta.l0_source,
            index_state(&meta, kind),
            now,
            certainty,
        ],
    )?;
    store_revision(
        tx,
        node.id,
        rev,
        content,
        &hash,
        &meta,
        a,
        Some(&node.index_state),
        &now,
    )?;
    // выжимки кэша с этим источником (§9.4): секция с прежним текстом остаётся действующей
    crate::cache::invalidate_revised(
        tx,
        TargetRef {
            kind: TargetKind::Node,
            id: node.id,
        },
        rev,
        content,
        StaleKind::Changed,
    )?;
    load_node(tx, node.id)
}
