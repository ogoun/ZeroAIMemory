//! Кэш эмбеддингов чанков (§6.3 ТЗ, таблица `embeddings`).
//!
//! Ключ — `(text_hash, model_id)`, где `text_hash` — blake3 ровно той строки, что
//! подаётся в модель ([`embed_key_of`]: контекстный префикс чанка + текст; формула
//! одна на весь проект, `zam_index::embed_key` её реэкспортирует). Ключ чанка хранится
//! в `chunks.embed_key`. Одинаковая эмбеддируемая строка векторизуется один раз, а
//! Qdrant пересобирается из кэша без инференса.
//!
//! Владелец (`owner`): у эмбеддингов чанков `NULL`, у служебных (предложения L0 заметки,
//! L0 папки) — id узла. Сборка мусора [`gc_embeddings`] удаляет строки чанков, на ключ
//! которых не ссылается ни один чанк, и служебные строки удалённых узлов; устаревшие
//! служебные строки живого узла снимает [`prune_owned_embeddings`].
//! Кодирование: `dense` — подряд f16 little-endian; `sparse` — повторяющиеся
//! пары (u32 LE id токена, f16 LE вес). Хранилище не зависит от `zam-ml`:
//! [`Embedding`] — собственный тип.

use std::collections::HashMap;

use half::f16;
use jiff::Timestamp;
use rusqlite::params;
use zam_core::Id;

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

/// Строка, которая уходит в модель для чанка: `context_prefix` участвует в эмбеддинге
/// (§8.2 п. 6). Пустой префикс — только текст.
pub fn embed_input_of(context_prefix: &str, text: &str) -> String {
    if context_prefix.is_empty() {
        text.to_string()
    } else {
        format!(
            "{context_prefix}
{text}"
        )
    }
}

/// Ключ кэша `embeddings` чанка: blake3 (hex) ровно строки [`embed_input_of`].
/// Единственная реализация формулы: её пишет запись чанка (`chunks.embed_key`), её
/// реэкспортирует `zam_index` (воркер, поиск, пересборка из кэша).
pub fn embed_key_of(context_prefix: &str, text: &str) -> String {
    let mut h = blake3::Hasher::new();
    if !context_prefix.is_empty() {
        h.update(context_prefix.as_bytes());
        h.update(
            b"
",
        );
    }
    h.update(text.as_bytes());
    h.finalize().to_hex().to_string()
}

/// Сохраняет эмбеддинги чанков модели `model_id` (существующие по тому же ключу
/// заменяются). Одна транзакция писателя на весь пакет.
pub async fn put_embeddings(
    db: &Db,
    model_id: &str,
    items: Vec<(String, Embedding)>,
) -> Result<(), StoreError> {
    put(db, model_id, None, items).await
}

/// Сохраняет служебные эмбеддинги узла `owner` (предложения L0, L0 папки): они живут,
/// пока жив узел ([`gc_embeddings`]) и пока входят в его текущий набор
/// ([`prune_owned_embeddings`]).
pub async fn put_owned_embeddings(
    db: &Db,
    model_id: &str,
    owner: Id,
    items: Vec<(String, Embedding)>,
) -> Result<(), StoreError> {
    put(db, model_id, Some(owner), items).await
}

async fn put(
    db: &Db,
    model_id: &str,
    owner: Option<Id>,
    items: Vec<(String, Embedding)>,
) -> Result<(), StoreError> {
    if items.is_empty() {
        return Ok(());
    }
    let model_id = model_id.to_string();
    db.write(move |tx| {
        let now = fmt_ts(Timestamp::now());
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO embeddings (text_hash, model_id, dense, sparse, created_at,
                                                owner)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (hash, e) in &items {
            st.execute(params![
                hash,
                model_id,
                encode_dense(&e.dense),
                encode_sparse(&e.sparse),
                now,
                owner
            ])?;
        }
        Ok(())
    })
    .await
}

/// Удаляет служебные эмбеддинги модели `model_id` узла `owner`, не входящие в `keep`
/// (текущий набор ключей L0 узла). Возвращает число удалённых строк.
pub async fn prune_owned_embeddings(
    db: &Db,
    model_id: &str,
    owner: Id,
    keep: &[String],
) -> Result<u64, StoreError> {
    let model_id = model_id.to_string();
    let keep = serde_json::Value::from(keep.to_vec()).to_string();
    db.write(move |tx| {
        let n = tx
            .prepare_cached(
                "DELETE FROM embeddings
                 WHERE owner = ?1 AND model_id = ?2
                   AND text_hash NOT IN (SELECT value FROM json_each(?3))",
            )?
            .execute(params![owner, model_id, keep])?;
        Ok(n as u64)
    })
    .await
}

/// Условие «строка кэша больше не нужна» (алиас `e` — embeddings):
/// - эмбеддинг чанка (`owner IS NULL`), на ключ которого не ссылается ни один чанк;
/// - служебный эмбеддинг удалённого (в корзине или стёртого) узла.
const ORPHAN: &str = "CASE WHEN e.owner IS NULL
     THEN NOT EXISTS (SELECT 1 FROM chunks c WHERE c.embed_key = e.text_hash)
     ELSE NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = e.owner AND n.deleted_at IS NULL)
     END";

/// Сборка мусора кэша: удаляет до `limit` ненужных строк любой модели (см. [`ORPHAN`]).
///
/// Поиск кандидатов идёт в транзакции чтения, а удаление — короткой транзакцией писателя
/// с повторной проверкой условия: запись заметок не ждёт просмотра всей таблицы, а ключ,
/// на который успел сослаться новый чанк, не удаляется. Вызывающий повторяет, пока
/// возвращается `limit`. Пока у какого-либо чанка нет `embed_key` (база до v5 ещё
/// не дозаполнена), ничего не удаляется: иначе пропали бы живые эмбеддинги.
pub async fn gc_embeddings(db: &Db, limit: usize) -> Result<u64, StoreError> {
    if limit == 0 {
        return Ok(0);
    }
    let lim = i64::try_from(limit).unwrap_or(i64::MAX);
    let cands: Vec<(String, String)> = db
        .read(move |c| {
            let unkeyed: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM chunks WHERE embed_key IS NULL)",
                [],
                |r| r.get(0),
            )?;
            if unkeyed {
                return Ok(Vec::new());
            }
            let mut st = c.prepare_cached(&format!(
                "SELECT e.text_hash, e.model_id FROM embeddings e WHERE {ORPHAN} LIMIT ?1"
            ))?;
            let rows = st.query_map([lim], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await?;
    if cands.is_empty() {
        return Ok(0);
    }
    db.write(move |tx| {
        let mut del = tx.prepare_cached(&format!(
            "DELETE FROM embeddings AS e WHERE e.text_hash = ?1 AND e.model_id = ?2 AND {ORPHAN}"
        ))?;
        let mut n = 0u64;
        for (hash, model) in &cands {
            n += del.execute(params![hash, model])? as u64;
        }
        Ok(n)
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
