//! Инвалидация выжимок (§9.4): по обратному индексу `cache_deps_target` в транзакции писателя
//! источника ([`invalidate_target`], [`invalidate_revised`]) и по новым релевантным
//! источникам после индексации ([`mark_new_sources`]).
use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::{Id, TargetKind, TargetRef};

use super::deps::text_hash;
use super::{StaleKind, StaleSource, cos, kind_str, mark_stale, section_opt};
use crate::embeddings::decode_dense;
use crate::time::parse_ts;
use crate::{Db, StoreError};

/// Живые выжимки с источником `target`: (выжимка, секция, хэш).
fn deps_of(c: &Connection, target: TargetRef) -> Result<Vec<(Id, String, String)>, StoreError> {
    let mut st = c.prepare_cached(
        "SELECT d.cache_id, d.section, d.text_hash FROM cache_deps d
         JOIN answer_cache a ON a.id = d.cache_id
         WHERE d.target_kind = ?1 AND d.target_id = ?2 AND a.deleted_at IS NULL",
    )?;
    let rows = st.query_map(params![kind_str(target.kind), target.id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Помечает недействительными все выжимки с источником `target` (причина `reason`) — в
/// транзакции записи источника: удаление и перенос в корзину, перенос, замещение, отзыв,
/// новое вложение. Число выжимок, получивших новую причину.
pub fn invalidate_target(
    tx: &Connection,
    target: TargetRef,
    reason: StaleKind,
) -> Result<u32, StoreError> {
    let mut by: HashMap<Id, Vec<StaleSource>> = HashMap::new();
    for (cache, section, _) in deps_of(tx, target)? {
        by.entry(cache).or_default().push(StaleSource {
            target,
            section: section_opt(section),
            reason,
        });
    }
    mark_stale(tx, by)
}

/// Новая ревизия `rev` источника с текстом `text` (правка заметки, PATCH, откат, правка и
/// смена статуса записи): при `reason = changed` источник, текст которого (целиком или
/// секции) не изменился, получает новую ревизию и остаётся действующим; остальные выжимки —
/// недействительны с причиной `reason`.
pub(crate) fn invalidate_revised(
    tx: &Connection,
    target: TargetRef,
    rev: i64,
    text: &str,
    reason: StaleKind,
) -> Result<u32, StoreError> {
    let mut by: HashMap<Id, Vec<StaleSource>> = HashMap::new();
    let mut bump = tx.prepare_cached(
        "UPDATE cache_deps SET rev = ?5
         WHERE cache_id = ?1 AND target_kind = ?2 AND target_id = ?3 AND section = ?4",
    )?;
    for (cache, section, hash) in deps_of(tx, target)? {
        let sec = (!section.is_empty()).then_some(section.as_str());
        let same = reason == StaleKind::Changed && text_hash(text, sec).as_deref() == Some(&hash);
        if same {
            bump.execute(params![
                cache,
                kind_str(target.kind),
                target.id,
                section,
                rev
            ])?;
            continue;
        }
        by.entry(cache).or_default().push(StaleSource {
            target,
            section: section_opt(section),
            reason,
        });
    }
    mark_stale(tx, by)
}

/// Время изменения цели (`updated_at`); нет — `None`.
fn updated_at(c: &Connection, t: TargetRef) -> Result<Option<Timestamp>, StoreError> {
    let sql = match t.kind {
        TargetKind::Node => "SELECT updated_at FROM nodes WHERE id = ?1 AND deleted_at IS NULL",
        TargetKind::Entry => "SELECT updated_at FROM entries WHERE id = ?1 AND deleted_at IS NULL",
    };
    let s: Option<String> = c
        .prepare_cached(sql)?
        .query_row([t.id], |r| r.get(0))
        .optional()?;
    s.map(|s| parse_ts(&s)).transpose()
}

/// Новые источники (§9.4): воркер индексации после upsert чанков сравнивает их dense-векторы
/// (`vecs`: цель → вектор чанка; у цели может быть несколько) с `query_vec` действующих
/// выжимок той же области — пространство `space` (выжимка в нём или с источником в нём) и
/// область `area` (у выжимки и цели обе заданы — должны совпасть). Цель считается новой для
/// выжимки, если она не среди её источников и изменена после сохранения выжимки (иначе
/// переиндексация без изменений портила бы кэш). Косинус ≥ `threshold` → `stale:
/// new_sources` с этой целью. Число выжимок, получивших новую причину.
pub async fn mark_new_sources(
    db: &Db,
    space: Id,
    area: Option<&str>,
    vecs: &[(TargetRef, Vec<f32>)],
    threshold: f32,
) -> Result<u32, StoreError> {
    if vecs.is_empty() {
        return Ok(0);
    }
    let (area, vecs) = (area.map(str::to_string), vecs.to_vec());
    db.write(move |tx| {
        let mut st = tx.prepare_cached(
            "SELECT a.id, a.query_vec, a.created_at FROM answer_cache a
             WHERE a.deleted_at IS NULL AND a.state = 'valid' AND a.query_vec IS NOT NULL
               AND (?2 IS NULL OR a.area IS NULL OR a.area = ?2)
               AND (a.space_id = ?1 OR EXISTS (
                     SELECT 1 FROM cache_deps d
                     LEFT JOIN nodes n ON d.target_kind = 'node' AND n.id = d.target_id
                     LEFT JOIN entries e ON d.target_kind = 'entry' AND e.id = d.target_id
                     WHERE d.cache_id = a.id AND coalesce(n.space_id, e.space_id) = ?1))",
        )?;
        let caches: Vec<(Id, Vec<u8>, String)> = st
            .query_map(params![space, area], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        if caches.is_empty() {
            return Ok(0);
        }
        let mut changed: HashMap<TargetRef, Option<Timestamp>> = HashMap::new();
        let mut is_dep = tx.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM cache_deps
                            WHERE cache_id = ?1 AND target_kind = ?2 AND target_id = ?3)",
        )?;
        let mut by: HashMap<Id, Vec<StaleSource>> = HashMap::new();
        for (cache, qv, created) in caches {
            let qv = decode_dense(&qv)?;
            let created = parse_ts(&created)?;
            for (target, v) in &vecs {
                if by
                    .get(&cache)
                    .is_some_and(|l| l.iter().any(|s| s.target == *target))
                {
                    continue;
                }
                if cos(&qv, v).is_none_or(|s| s < threshold) {
                    continue;
                }
                let upd = match changed.get(target) {
                    Some(u) => *u,
                    None => {
                        let u = updated_at(tx, *target)?;
                        changed.insert(*target, u);
                        u
                    }
                };
                if upd.is_none_or(|u| u <= created) {
                    continue;
                }
                let dep: bool = is_dep
                    .query_row(params![cache, kind_str(target.kind), target.id], |r| {
                        r.get(0)
                    })?;
                if dep {
                    continue;
                }
                by.entry(cache).or_default().push(StaleSource {
                    target: *target,
                    section: None,
                    reason: StaleKind::NewSources,
                });
            }
        }
        mark_stale(tx, by)
    })
    .await
}
