//! Ключи идемпотентности записи заметок (`node_idempotency`, миграция 0025; спецификация M6
//! §16 п. 19, CR-20): `Idempotency-Key` у `POST /nodes`, `PUT` и `PATCH /nodes/{id}`.
//!
//! Семантика — как у записей журнала ([`crate::create_entry`]):
//! - ключ — в границах автора (`<пользователь|t<токен>|->:<ключ>`, тот же
//!   `entries::write::scoped_key`), 1..=200 печатных символов;
//! - сверка ключа и сама правка — одна транзакция единственного писателя базы: параллельные
//!   запросы с одним ключом дают одну правку, остальные — повтор;
//! - повтор с тем же отпечатком запроса — узел в **текущем** виде, без новой ревизии; другой
//!   отпечаток (тело, цель, операция) — [`StoreError::IdempotencyMismatch`]; узел удалён в
//!   корзину — [`StoreError::Conflict`];
//! - строка ключа пишется только при успехе правки (конфликт ревизии, ошибка PATCH — ключ не
//!   занят) и живёт, пока жив узел.
//!
//! Отпечаток — blake3 частей запроса с длиной каждой части (без склейки границ); `message`
//! ревизии в него не входит (как у записей журнала).
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::Id;

use super::{Attribution, Node, load_node};
use crate::StoreError;
use crate::entries::{check_key, scoped_key};
use crate::time::fmt_ts;

/// Операция записи заметки под ключом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeOp {
    Create,
    Replace,
    Patch,
}

impl NodeOp {
    fn as_str(self) -> &'static str {
        match self {
            NodeOp::Create => "create",
            NodeOp::Replace => "replace",
            NodeOp::Patch => "patch",
        }
    }
}

/// Ключ запроса: ключ с автором, операция и отпечаток.
pub(crate) struct NodeKey {
    key: String,
    op: NodeOp,
    fingerprint: String,
}

impl NodeKey {
    /// Ключ из `Idempotency-Key` (`None` — без ключа); некорректный ключ — `BadInput`.
    pub(crate) fn new(
        raw: Option<String>,
        a: &Attribution,
        op: NodeOp,
        parts: &[&[u8]],
    ) -> Result<Option<NodeKey>, StoreError> {
        let Some(k) = check_key(raw)? else {
            return Ok(None);
        };
        let mut h = blake3::Hasher::new();
        h.update(op.as_str().as_bytes());
        for p in parts {
            h.update(&(p.len() as u64).to_le_bytes());
            h.update(p);
        }
        Ok(Some(NodeKey {
            key: scoped_key(a, &k),
            op,
            fingerprint: h.finalize().to_hex().to_string(),
        }))
    }

    /// Повтор: узел по ключу, если ключ уже использован этим же запросом. Вызывается в
    /// транзакции писателя до любой проверки и записи.
    pub(crate) fn replay(&self, c: &Connection) -> Result<Option<Node>, StoreError> {
        let row: Option<(String, Id, String)> = c
            .prepare_cached("SELECT op, node_id, fingerprint FROM node_idempotency WHERE key = ?1")?
            .query_row([&self.key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        let Some((op, node, fp)) = row else {
            return Ok(None);
        };
        if op != self.op.as_str() || fp != self.fingerprint {
            return Err(StoreError::IdempotencyMismatch(
                "Idempotency-Key уже использован запросом с другим содержимым".into(),
            ));
        }
        let node = load_node(c, node)?;
        if node.deleted_at.is_some() {
            return Err(StoreError::Conflict(
                "заметка по этому Idempotency-Key удалена: восстановите её или используйте новый \
                 ключ"
                    .into(),
            ));
        }
        Ok(Some(node))
    }

    /// Запоминает ключ успешной правки узла `node` (в той же транзакции).
    pub(crate) fn remember(&self, c: &Connection, node: Id) -> Result<(), StoreError> {
        c.prepare_cached(
            "INSERT INTO node_idempotency (key, op, node_id, fingerprint, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            self.key,
            self.op.as_str(),
            node,
            self.fingerprint,
            fmt_ts(crate::time::now())
        ])?;
        Ok(())
    }
}

/// Часть отпечатка необязательного текста: `None` и `Some("")` различаются.
pub(crate) fn opt_part(s: Option<&str>) -> Vec<u8> {
    match s {
        None => vec![0],
        Some(s) => {
            let mut v = vec![1];
            v.extend_from_slice(s.as_bytes());
            v
        }
    }
}
