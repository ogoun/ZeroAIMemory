//! Разностная перепроверка базы на секреты при росте версии правил сканера
//! (`zam_secrets::RULES_VERSION`, ревью задачи 15b M6, I2/M2–M5).
//!
//! Сохранённое уже замаскировано правилами своей версии, поэтому вход скана — **сохранённое**, а не
//! исходник: любая находка текущими правилами в `chunks.text`, `chunks.context_prefix` (имя, путь,
//! заголовки, теги — те же, что уходят в payload точек), `nodes.l0` заметок и папок, имени и пути
//! папки без живых заметок (задача 3 M7.1: в префиксы чанков они не входят) или
//! `code_symbols.sig` — значение, которое прежние правила пропустили. Переписываются и получают
//! задания только такие места:
//! - заметка — L0 маскируется, чанки пересобираются из исходника, `upsert` (точки и payload
//!   перепишет воркер; родительские папки он свернёт заново);
//! - запись журнала — чанки пересобираются, `upsert`;
//! - документ вложения — `extract_state = 'pending'` и задание извлечения (текст — из кэша
//!   `blob_extract`, нарезка маскирует его текущими правилами);
//! - L0 папки — маскируется сразу (карточки папок), а `upsert` живой заметки внутри неё
//!   запускает свёртку папки и её точку; у папки без живых заметок точку пересвернёт сверка
//!   точек папок воркера при запуске (ключ точки — по входу эмбеддинга, замаскированному
//!   текущими правилами), имя и путь остаются исходником (это адреса);
//! - сигнатура символа карты кода — `mask_text` на месте.
//!
//! Чтение и скан — пакетами в читателях (`Db::read`), писатель получает только id с находками:
//! открытие базы скан не ждёт, он идёт в фоне вместе с другими разовыми задачами открытия.
//! В лог — только счётчики.

use std::collections::{BTreeSet, HashSet};

use rusqlite::{OptionalExtension, params};
use zam_core::Id;
use zam_markdown::{mask_text, scan_secrets};

use crate::jobs::PRIORITY_REINDEX;
use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Строк сохранённого в одном чтении (в юнит-тестах — 3: граница пакета проверяется на
/// нескольких строках, `delta_hit_on_batch_boundary`).
#[cfg(not(test))]
const READ_BATCH: i64 = 2000;
#[cfg(test)]
const READ_BATCH: i64 = 3;

/// Целей в одной транзакции писателя (в юнит-тестах — 2).
#[cfg(not(test))]
const WRITE_BATCH: usize = 200;
#[cfg(test)]
const WRITE_BATCH: usize = 2;

/// Что нашлось в сохранённом.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Delta {
    pub notes: BTreeSet<Id>,
    pub entries: BTreeSet<Id>,
    pub docs: BTreeSet<Id>,
    pub folders: BTreeSet<Id>,
    /// Сигнатуры символов: `(id, прежняя, замаскированная)`.
    pub sigs: Vec<(i64, String, String)>,
}

impl Delta {
    fn is_empty(&self) -> bool {
        self.notes.is_empty()
            && self.entries.is_empty()
            && self.docs.is_empty()
            && self.folders.is_empty()
            && self.sigs.is_empty()
    }
}

/// Итог перепроверки (счётчики для лога и тестов).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeltaStats {
    pub notes: u64,
    pub entries: u64,
    pub docs: u64,
    pub folders: u64,
    pub sigs: u64,
    pub scan_ms: u64,
}

/// Живые папки без живых заметок на любой глубине: `(id, path, name)`. Потомки — по диапазону
/// пути (`f.path/` ≤ путь < `f.path0`, `'0'` — следующий за `'/'` байт) в `nodes_live_path`.
const BARE_FOLDERS_SQL: &str = "SELECT f.id, f.path, f.name FROM nodes f
     WHERE f.kind = 'folder' AND f.deleted_at IS NULL
       AND NOT EXISTS (
           SELECT 1 FROM nodes d
           WHERE d.space_id = f.space_id AND d.deleted_at IS NULL AND d.kind = 'note'
             AND d.path > f.path || '/' AND d.path < f.path || '0')";

/// Строка чанка: `(seq, node_id, entry_id, attachment_id, text, context_prefix)`.
type ChunkRow = (i64, Option<Id>, Option<Id>, Option<Id>, String, String);

/// Скан сохранённого текущими правилами (только чтение).
pub(crate) async fn scan(db: &Db) -> Result<Delta, StoreError> {
    let mut d = Delta::default();
    // чанки заметок, записей и вложений
    let mut after = 0i64;
    loop {
        let rows: Vec<ChunkRow> = db
            .read(move |c| {
                let mut st = c.prepare_cached(
                    "SELECT seq, node_id, entry_id, attachment_id, text, context_prefix
                     FROM chunks WHERE seq > ?1 ORDER BY seq LIMIT ?2",
                )?;
                let rows = st
                    .query_map(params![after, READ_BATCH], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await?;
        let Some(last) = rows.last().map(|r| r.0) else {
            break;
        };
        let n = rows.len() as i64;
        // префикс у чанков одного владельца общий: каждый проверяется один раз за пакет
        let mut clean: HashSet<String> = HashSet::new();
        for (_, node, entry, att, text, prefix) in rows {
            let hit = !scan_secrets(&text).is_empty()
                || (!clean.contains(&prefix) && {
                    let h = !scan_secrets(&prefix).is_empty();
                    if !h {
                        clean.insert(prefix);
                    }
                    h
                });
            if !hit {
                continue;
            }
            match (att, entry, node) {
                (Some(a), _, _) => d.docs.insert(a),
                (None, Some(e), _) => d.entries.insert(e),
                (None, None, Some(n)) => d.notes.insert(n),
                (None, None, None) => false,
            };
        }
        if n < READ_BATCH {
            break;
        }
        after = last;
    }
    // L0 заметок и папок
    let l0s: Vec<(Id, bool, String)> = db
        .read(|c| {
            let mut st = c.prepare(
                "SELECT id, kind = 'folder', l0 FROM nodes
                 WHERE deleted_at IS NULL AND l0 IS NOT NULL",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
        .await?;
    for (id, folder, l0) in l0s {
        if !scan_secrets(&l0).is_empty() {
            if folder {
                d.folders.insert(id);
            } else {
                d.notes.insert(id);
            }
        }
    }
    // имена и пути папок без живых заметок внутри (задача 3 M7.1): у папки с живой заметкой они
    // входят в `context_prefix` её чанков (скан выше), у пустой — никуда
    let bare: Vec<(Id, String, String)> = db
        .read(|c| {
            let mut st = c.prepare(BARE_FOLDERS_SQL)?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
        .await?;
    for (id, path, name) in bare {
        if !scan_secrets(&path).is_empty() || !scan_secrets(&name).is_empty() {
            d.folders.insert(id);
        }
    }
    // сигнатуры карты кода
    let mut after = 0i64;
    loop {
        let rows: Vec<(i64, String)> = db
            .read(move |c| {
                let mut st = c.prepare_cached(
                    "SELECT id, sig FROM code_symbols
                     WHERE id > ?1 AND sig IS NOT NULL ORDER BY id LIMIT ?2",
                )?;
                let rows = st
                    .query_map(params![after, READ_BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await?;
        let Some(last) = rows.last().map(|r| r.0) else {
            break;
        };
        let n = rows.len() as i64;
        for (id, sig) in rows {
            let m = mask_text(&sig);
            if m != sig {
                d.sigs.push((id, sig, m));
            }
        }
        if n < READ_BATCH {
            break;
        }
        after = last;
    }
    Ok(d)
}

/// Переписывает найденное [`scan`] (транзакции писателя пакетами по [`WRITE_BATCH`] целей).
pub(crate) async fn apply(db: &Db, d: Delta) -> Result<DeltaStats, StoreError> {
    let mut stats = DeltaStats::default();
    let chunked = |ids: &BTreeSet<Id>| -> Vec<Vec<Id>> {
        ids.iter()
            .copied()
            .collect::<Vec<_>>()
            .chunks(WRITE_BATCH)
            .map(<[Id]>::to_vec)
            .collect()
    };
    for batch in chunked(&d.notes) {
        stats.notes += db
            .write(move |tx| {
                let now = fmt_ts(crate::time::now());
                let mut n = 0;
                for id in batch {
                    n += u64::from(crate::nodes::remask_note(tx, id, PRIORITY_REINDEX, &now)?);
                }
                Ok(n)
            })
            .await?;
    }
    for batch in chunked(&d.entries) {
        stats.entries += db
            .write(move |tx| {
                let now = fmt_ts(crate::time::now());
                let mut n = 0;
                for id in batch {
                    n += u64::from(crate::entries::remask_entry(
                        tx,
                        id,
                        PRIORITY_REINDEX,
                        &now,
                    )?);
                }
                Ok(n)
            })
            .await?;
    }
    for batch in chunked(&d.docs) {
        stats.docs += db
            .write(move |tx| {
                let now = fmt_ts(crate::time::now());
                let mut n = 0;
                for id in batch {
                    n += u64::from(crate::attachments::reextract_doc(tx, id, &now)?);
                }
                Ok(n)
            })
            .await?;
    }
    for batch in chunked(&d.folders) {
        stats.folders += db
            .write(move |tx| {
                let now = fmt_ts(crate::time::now());
                let mut n = 0;
                for id in batch {
                    // папки нет или она в корзине — `None`; L0 может быть пустым (находка в имени)
                    let row: Option<Option<String>> = tx
                        .prepare_cached(
                            "SELECT l0 FROM nodes WHERE id = ?1 AND deleted_at IS NULL",
                        )?
                        .query_row([id], |r| r.get(0))
                        .optional()?;
                    let Some(l0) = row else { continue };
                    if let Some(l0) = l0 {
                        let m = mask_text(&l0);
                        if m != l0 {
                            tx.prepare_cached("UPDATE nodes SET l0 = ?2 WHERE id = ?1")?
                                .execute(params![id, m])?;
                        }
                    }
                    // точка папки — свёрткой: её запускает `upsert` живой заметки внутри. У папки
                    // без живых заметок (находка в имени или пути) точку с L0 пересвернёт сверка
                    // точек папок воркера при запуске: ключ точки считается по входу эмбеддинга,
                    // замаскированному текущими правилами (`folder_embed_input`); имя и путь
                    // хранятся исходником — это адреса (спецификация M7 §4.3)
                    let child: Option<(Id, i64, String)> = tx
                        .prepare_cached(
                            "SELECT d.id, d.current_rev, d.index_state
                             FROM nodes f JOIN nodes d ON d.space_id = f.space_id
                             WHERE f.id = ?1 AND d.kind = 'note' AND d.deleted_at IS NULL
                               AND substr(d.path, 1, length(f.path) + 1) = f.path || '/'
                             ORDER BY d.path LIMIT 1",
                        )?
                        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .optional()?;
                    if let Some((c, rev, state)) = child {
                        crate::nodes::enqueue_for_note(
                            tx,
                            c,
                            rev,
                            Some(&state),
                            PRIORITY_REINDEX,
                            &now,
                        )?;
                    }
                    n += 1;
                }
                Ok(n)
            })
            .await?;
    }
    for batch in d.sigs.chunks(WRITE_BATCH).map(<[_]>::to_vec) {
        stats.sigs += db
            .write(move |tx| {
                let mut n = 0;
                for (id, old, new) in batch {
                    // сигнатура могла смениться после чтения: перезапись только прежней
                    n += tx
                        .prepare_cached(
                            "UPDATE code_symbols SET sig = ?3 WHERE id = ?1 AND sig = ?2",
                        )?
                        .execute(params![id, old, new])? as u64;
                }
                Ok(n)
            })
            .await?;
    }
    Ok(stats)
}

/// Скан и перезапись: [`scan`] + [`apply`], время скана — в `scan_ms`.
pub(crate) async fn run(db: &Db) -> Result<DeltaStats, StoreError> {
    let t0 = std::time::Instant::now();
    let d = scan(db).await?;
    let scan_ms = t0.elapsed().as_millis() as u64;
    let mut stats = if d.is_empty() {
        DeltaStats::default()
    } else {
        apply(db, d).await?
    };
    stats.scan_ms = scan_ms;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attribution, AuthorKind, NewNote, NodePath, SpaceKind, UserDbs, list_spaces};

    /// Ревью 15b, N2: находка в последней строке второго пакета чтения (пакет — 3 строки) и
    /// цели во втором пакете записи (пакет — 2) доходят до `apply`.
    #[tokio::test]
    async fn delta_hit_on_batch_boundary() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-tmp/zam-store-secrets-delta");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = UserDbs::new(dir).get(Id::new()).await.unwrap();
        let k = list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.kind == SpaceKind::Knowledge)
            .unwrap()
            .id;
        let mut ids = Vec::new();
        for i in 0..7 {
            let n = crate::create_note(
                &db,
                NewNote {
                    space_id: k,
                    path: NodePath::parse(&format!("n/note{i}")).unwrap(),
                    content: format!("# Заметка {i}\n\nобычный текст {i}\n"),
                    l0: None,
                    attrib: Attribution::new(AuthorKind::Agent),
                },
            )
            .await
            .unwrap();
            ids.push(n.id);
        }
        // строки чанков по порядку; новая утечка — в шестой (последней второго пакета) и седьмой
        let pw = format!("Sup3r{}", "S3cretValue");
        let seqs: Vec<(i64, Id)> = db
            .read(|c| {
                let mut st = c.prepare("SELECT seq, node_id FROM chunks ORDER BY seq")?;
                let rows = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await
            .unwrap();
        assert!(seqs.len() >= 7, "{seqs:?}");
        let hit: Vec<(i64, Id)> = vec![seqs[5], seqs[seqs.len() - 1]];
        let marks: Vec<i64> = hit.iter().map(|h| h.0).collect();
        db.write(move |tx| {
            for seq in marks {
                tx.execute(
                    "UPDATE chunks SET text = text || ?2 WHERE seq = ?1",
                    params![seq, format!(" password=`{pw}`")],
                )?;
            }
            tx.execute("DELETE FROM index_jobs", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let d = scan(&db).await.unwrap();
        let want: BTreeSet<Id> = hit.iter().map(|h| h.1).collect();
        assert_eq!(d.notes, want, "находки на границе пакетов");
        let st = apply(&db, d).await.unwrap();
        assert_eq!(st.notes, want.len() as u64);
        assert_eq!(
            crate::job_stats(&db).await.unwrap().queued,
            want.len() as i64
        );
    }

    /// Задача 3 M7.1 («Передать в M7» задачи 15b M6): папка с секретом в имени, у которой нет
    /// живых заметок (пустая или только с заметками в корзине), попадает в находки перепроверки —
    /// её имя и путь не входят ни в один `context_prefix` живых чанков. Папка с живой заметкой
    /// покрыта префиксом чанков заметки и отдельно не берётся; чистая пустая папка — тоже.
    #[tokio::test]
    async fn secrets_delta_folder_without_live_notes() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-tmp/zam-store-secrets-delta-folder");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = UserDbs::new(dir).get(Id::new()).await.unwrap();
        let k = list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.kind == SpaceKind::Knowledge)
            .unwrap()
            .id;
        let pw = format!("Sup3r{}", "S3cretValue1");
        let path = |p: &str| NodePath::parse(p).unwrap();
        let empty = crate::create_folder(&db, k, path(&format!("ops/password={pw}")))
            .await
            .unwrap()
            .id;
        // все заметки папки — в корзине
        let trashed_dir = crate::create_folder(&db, k, path(&format!("old/token={pw}")))
            .await
            .unwrap()
            .id;
        let gone = crate::create_note(
            &db,
            NewNote {
                space_id: k,
                path: path(&format!("old/token={pw}/n")),
                content: "# N\n\nтекст\n".into(),
                l0: None,
                attrib: Attribution::new(AuthorKind::Agent),
            },
        )
        .await
        .unwrap()
        .id;
        crate::delete_node(&db, gone, AuthorKind::Agent)
            .await
            .unwrap();
        let clean = crate::create_folder(&db, k, path("docs/пусто"))
            .await
            .unwrap()
            .id;
        // папка с живой заметкой: её имя — в `context_prefix` чанков заметки (там замаскировано)
        crate::create_note(
            &db,
            NewNote {
                space_id: k,
                path: path(&format!("live/secret={pw}/n")),
                content: "# N\n\nтекст\n".into(),
                l0: None,
                attrib: Attribution::new(AuthorKind::Agent),
            },
        )
        .await
        .unwrap();
        db.write(|tx| {
            tx.execute("DELETE FROM index_jobs", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let d = scan(&db).await.unwrap();
        assert!(d.folders.contains(&empty), "пустая папка: {d:?}");
        assert!(
            d.folders.contains(&trashed_dir),
            "папка с заметкой в корзине: {d:?}"
        );
        assert!(!d.folders.contains(&clean), "{d:?}");
        // родители `ops`, `old` и `live` — без секрета в имени; `live/secret=…` — с живой заметкой
        assert_eq!(d.folders.len(), 2, "{d:?}");
        assert!(
            d.notes.is_empty(),
            "префикс чанков живой заметки замаскирован: {d:?}"
        );
        let st = apply(&db, d).await.unwrap();
        assert_eq!(st.folders, 2, "находки в счётчике перепроверки");
        assert_eq!(
            crate::job_stats(&db).await.unwrap().queued,
            0,
            "живых заметок нет"
        );
    }
}
