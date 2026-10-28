//! Источники выжимки: закрепление при сохранении ([`pin`]) и проверка при чтении
//! ([`check`]) — одним SQL-запросом по всем источникам выбранных выжимок (§9.4).
//!
//! Хэш текста — sha256 hex: у заметки целиком — `nodes.content_hash` текущей ревизии, у
//! записи журнала — тела (`body`, полный текст L2 `/read`), у секции — байт секции
//! ([`section_text`]: заголовок с подсекциями, путь однозначен). Секцию источника [`pin`]
//! разрешает, как чтение (`zam_markdown::resolve_section`: полный путь или однозначный хвост), и
//! закрепляет её **полный** путь — с ним выжимка проверяется и показывается дальше
//! (проверка на стенде Debian 13, доработка по решению контроллера).
use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use zam_core::{Id, TargetKind, TargetRef};

use super::{CacheDep, CacheError, StaleKind, StaleSource, kind_of, section_opt};
use crate::StoreError;
use crate::nodes::{revision_text, sha256_hex};

/// Текст секции `path` (заголовок с подсекциями); нет такой или путь неоднозначен — `None`.
pub fn section_text<'a>(text: &'a str, path: &str) -> Option<&'a str> {
    zam_markdown::section_bytes(text, path)
}

/// Хэш текста цели или её секции; `None` — секции нет.
pub(crate) fn text_hash(text: &str, section: Option<&str>) -> Option<String> {
    match section {
        Some(s) => section_text(text, s).map(|t| sha256_hex(t.as_bytes())),
        None => Some(sha256_hex(text.as_bytes())),
    }
}

/// Цель как источник: пространство, вид пространства, ревизия, текст, удалена ли, папка ли.
struct Target {
    space_kind: String,
    rev: i64,
    deleted: bool,
    folder: bool,
    /// Хэш целиком (`content_hash` узла) — без чтения текста.
    whole_hash: Option<String>,
    /// Статус insight (`superseded`, `retracted`, …); у узла — `None`.
    status: Option<String>,
}

fn load_target(c: &Connection, t: TargetRef) -> Result<Option<Target>, StoreError> {
    Ok(match t.kind {
        TargetKind::Node => c
            .prepare_cached(
                "SELECT s.kind, n.current_rev, n.deleted_at IS NOT NULL, n.kind = 'folder',
                        n.content_hash
                 FROM nodes n JOIN spaces s ON s.id = n.space_id WHERE n.id = ?1",
            )?
            .query_row([t.id], |r| {
                Ok(Target {
                    space_kind: r.get(0)?,
                    rev: r.get(1)?,
                    deleted: r.get(2)?,
                    folder: r.get(3)?,
                    whole_hash: r.get(4)?,
                    status: None,
                })
            })
            .optional()?,
        TargetKind::Entry => c
            .prepare_cached(
                "SELECT s.kind, e.current_rev, e.deleted_at IS NOT NULL, e.status
                 FROM entries e JOIN spaces s ON s.id = e.space_id WHERE e.id = ?1",
            )?
            .query_row([t.id], |r| {
                Ok(Target {
                    space_kind: r.get(0)?,
                    rev: r.get(1)?,
                    deleted: r.get(2)?,
                    folder: false,
                    whole_hash: None,
                    status: r.get(3)?,
                })
            })
            .optional()?,
    })
}

/// Текст цели текущей ревизии: заметка — текст ревизии, запись — тело.
pub(crate) fn current_text(c: &Connection, t: TargetRef, rev: i64) -> Result<String, StoreError> {
    match t.kind {
        TargetKind::Node => revision_text(c, t.id, rev),
        TargetKind::Entry => Ok(c
            .prepare_cached("SELECT body FROM entries WHERE id = ?1")?
            .query_row([t.id], |r| r.get(0))?),
    }
}

/// Проверяет источник и закрепляет его текущую ревизию и хэш (в транзакции сохранения).
pub(crate) fn pin(
    c: &Connection,
    t: TargetRef,
    rev: Option<i64>,
    section: Option<&str>,
) -> Result<CacheDep, CacheError> {
    let missing = |detail: &str| CacheError::SourceNotFound {
        target: t.to_string(),
        detail: detail.into(),
    };
    let Some(cur) = load_target(c, t)? else {
        return Err(missing("не найден"));
    };
    if cur.space_kind == "human" {
        return Err(CacheError::ForbiddenSource(t.to_string()));
    }
    if cur.deleted {
        return Err(missing("удалён"));
    }
    if cur.folder {
        return Err(missing("папка — не источник: у неё нет текста"));
    }
    // замещённый или отозванный insight — выжимка на нём была бы недействительна сразу
    if let Some(s @ ("superseded" | "retracted")) = cur.status.as_deref() {
        return Err(CacheError::SourceInactive {
            target: t.to_string(),
            status: s.to_string(),
        });
    }
    if let Some(expected) = rev
        && expected != cur.rev
    {
        return Err(CacheError::SourceChanged {
            target: t.to_string(),
            expected,
            current_rev: cur.rev,
        });
    }
    let (text_hash, section) = match (section, &cur.whole_hash) {
        (None, Some(h)) => (h.clone(), None),
        (None, None) => (sha256_hex(current_text(c, t, cur.rev)?.as_bytes()), None),
        (Some(path), _) => {
            let text = current_text(c, t, cur.rev)?;
            let full = match zam_markdown::resolve_section(&zam_markdown::parse(&text), path) {
                zam_markdown::SectionLookup::Found(s) => s.path.join(" > "),
                zam_markdown::SectionLookup::NotFound => return Err(missing("секция не найдена")),
                zam_markdown::SectionLookup::Ambiguous => {
                    return Err(missing(&format!("путь секции неоднозначен: {path}")));
                }
            };
            let hash = text_hash(&text, Some(&full)).ok_or_else(|| missing("секция не найдена"))?;
            (hash, Some(full))
        }
    };
    Ok(CacheDep {
        target: t,
        rev: cur.rev,
        text_hash,
        section,
    })
}

/// Цель жива (не удалена).
pub(crate) fn is_live(c: &Connection, t: TargetRef) -> Result<bool, StoreError> {
    Ok(load_target(c, t)?.is_some_and(|x| !x.deleted))
}

/// Итог проверки выжимки: расхождения и пространства источников.
pub(crate) type Checked = (Vec<StaleSource>, Vec<Id>);

/// Проверка источников выжимок `ids` одним запросом: выжимка → (расхождения, пространства
/// источников). Расхождение: цели нет или она удалена (`deleted`), insight замещён или
/// отозван (`superseded`, `retracted`), ревизия другая (`changed`; у секции — если изменился
/// её текст: тогда он читается из текущей ревизии).
pub(crate) fn check(c: &Connection, ids: &[Id]) -> Result<HashMap<Id, Checked>, StoreError> {
    let mut out: HashMap<Id, Checked> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let list = Value::from(ids.iter().map(|i| i.to_string()).collect::<Vec<_>>()).to_string();
    let mut st = c.prepare_cached(
        "SELECT d.cache_id, d.target_kind, d.target_id, d.rev, d.text_hash, d.section,
                coalesce(n.current_rev, e.current_rev),
                coalesce(n.deleted_at, e.deleted_at) IS NOT NULL,
                e.status,
                coalesce(n.space_id, e.space_id)
         FROM cache_deps d
         LEFT JOIN nodes n ON d.target_kind = 'node' AND n.id = d.target_id
         LEFT JOIN entries e ON d.target_kind = 'entry' AND e.id = d.target_id
         WHERE d.cache_id IN (SELECT value FROM json_each(?1))",
    )?;
    type Row = (
        Id,
        String,
        Id,
        i64,
        String,
        String,
        Option<i64>,
        bool,
        Option<String>,
        Option<Id>,
    );
    let rows: Vec<Row> = st
        .query_map(params![list], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    for (cache, kind, id, rev, hash, section, cur_rev, deleted, status, space) in rows {
        let kind =
            kind_of(&kind).ok_or_else(|| StoreError::Corrupt(format!("cache_deps: «{kind}»")))?;
        let target = TargetRef { kind, id };
        let slot = out.entry(cache).or_default();
        if let Some(s) = space
            && !slot.1.contains(&s)
        {
            slot.1.push(s);
        }
        let reason = match (cur_rev, status.as_deref()) {
            (None, _) => Some(StaleKind::Deleted),
            _ if deleted => Some(StaleKind::Deleted),
            (_, Some("superseded")) => Some(StaleKind::Superseded),
            (_, Some("retracted")) => Some(StaleKind::Retracted),
            (Some(r), _) if r != rev => {
                // секция могла не измениться: сравнивается её текст в текущей ревизии
                let same = !section.is_empty()
                    && text_hash(&current_text(c, target, r)?, Some(&section)).as_deref()
                        == Some(hash.as_str());
                (!same).then_some(StaleKind::Changed)
            }
            _ => None,
        };
        if let Some(reason) = reason {
            slot.0.push(StaleSource {
                target,
                section: section_opt(section),
                reason,
            });
        }
    }
    Ok(out)
}
