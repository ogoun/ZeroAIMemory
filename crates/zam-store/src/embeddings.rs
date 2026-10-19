//! Кэш эмбеддингов чанков (§6.3 ТЗ, таблица `embeddings`).
//!
//! Ключ — `(text_hash, model_id)`, где `text_hash` — blake3 ровно той строки, что
//! подаётся в модель (`zam_index::embed_key`: контекстный префикс чанка + текст).
//! Одинаковая эмбеддируемая строка векторизуется один раз, а Qdrant пересобирается
//! из кэша без инференса.
//! Кодирование: `dense` — подряд f16 little-endian; `sparse` — повторяющиеся
//! пары (u32 LE id токена, f16 LE вес). Хранилище не зависит от `zam-ml`:
//! [`Embedding`] — собственный тип.

use std::collections::HashMap;

use half::f16;
use jiff::Timestamp;
use rusqlite::params;

use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Эмбеддинг чанка: плотный вектор и разреженные веса токенов.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Embedding {
    pub dense: Vec<f32>,
    pub sparse: Vec<(u32, f32)>,
}

/// Байт на значение dense.
const DENSE_ITEM: usize = 2;
/// Байт на пару sparse.
const SPARSE_ITEM: usize = 6;

pub(crate) fn encode_dense(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * DENSE_ITEM);
    for x in v {
        out.extend_from_slice(&f16::from_f32(*x).to_le_bytes());
    }
    out
}

pub(crate) fn encode_sparse(v: &[(u32, f32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * SPARSE_ITEM);
    for (id, w) in v {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&f16::from_f32(*w).to_le_bytes());
    }
    out
}

pub(crate) fn decode_dense(b: &[u8]) -> Result<Vec<f32>, StoreError> {
    let (items, rest) = b.as_chunks::<DENSE_ITEM>();
    if !rest.is_empty() {
        return Err(StoreError::Corrupt(format!(
            "dense-вектор: {} байт не кратно {DENSE_ITEM}",
            b.len()
        )));
    }
    Ok(items
        .iter()
        .map(|c| f16::from_le_bytes(*c).to_f32())
        .collect())
}

pub(crate) fn decode_sparse(b: &[u8]) -> Result<Vec<(u32, f32)>, StoreError> {
    let (items, rest) = b.as_chunks::<SPARSE_ITEM>();
    if !rest.is_empty() {
        return Err(StoreError::Corrupt(format!(
            "sparse-вектор: {} байт не кратно {SPARSE_ITEM}",
            b.len()
        )));
    }
    Ok(items
        .iter()
        .map(|c| {
            (
                u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f16::from_le_bytes([c[4], c[5]]).to_f32(),
            )
        })
        .collect())
}

/// Сохраняет эмбеддинги модели `model_id` (существующие по тому же ключу заменяются).
/// Одна транзакция писателя на весь пакет.
pub async fn put_embeddings(
    db: &Db,
    model_id: &str,
    items: Vec<(String, Embedding)>,
) -> Result<(), StoreError> {
    if items.is_empty() {
        return Ok(());
    }
    let model_id = model_id.to_string();
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO embeddings (text_hash, model_id, dense, sparse, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (hash, e) in &items {
            st.execute(params![
                hash,
                model_id,
                encode_dense(&e.dense),
                encode_sparse(&e.sparse),
                now
            ])?;
        }
        Ok(())
    })
    .await
}

/// Эмбеддинги модели `model_id` для `hashes`; отсутствующие в кэше в ответ не входят.
/// Повреждённый BLOB → `Corrupt`.
pub async fn get_embeddings(
    db: &Db,
    model_id: &str,
    hashes: &[String],
) -> Result<HashMap<String, Embedding>, StoreError> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let model_id = model_id.to_string();
    let list = serde_json::Value::from(hashes.to_vec()).to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT e.text_hash, e.dense, e.sparse
             FROM json_each(?1) j
             CROSS JOIN embeddings e ON e.text_hash = j.value AND e.model_id = ?2",
        )?;
        let rows = st.query_map(params![list, model_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (hash, dense, sparse) = row?;
            let e = Embedding {
                dense: decode_dense(&dense)?,
                sparse: decode_sparse(&sparse)?,
            };
            out.insert(hash, e);
        }
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_layout() {
        let d = encode_dense(&[1.0, -2.0]);
        assert_eq!(d, vec![0x00, 0x3c, 0x00, 0xc0]);
        let s = encode_sparse(&[(0x0102_0304, 1.0)]);
        assert_eq!(s, vec![0x04, 0x03, 0x02, 0x01, 0x00, 0x3c]);
        assert_eq!(decode_dense(&d).unwrap(), vec![1.0, -2.0]);
        assert_eq!(decode_sparse(&s).unwrap(), vec![(0x0102_0304, 1.0)]);
        assert!(decode_sparse(&[0; 5]).is_err());
    }
}
