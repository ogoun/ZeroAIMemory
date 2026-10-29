//! Ссылки знаний на файлы решения (спецификация M4.5 §6.1–6.3): таблица `file_refs`,
//! устарелость по хэшу содержимого и индекс «путь → записи» для хуков.
//!
//! - **Владелец** ссылки — заметка (frontmatter `refs:` при каждой ревизии, без хэша) или
//!   запись журнала (поле запроса `refs`, у changelog — ещё строки `data.refs.files` без хэша).
//!   Набор ссылок владельца заменяется целиком ([`set_refs`]): пути из нового набора получают
//!   новые хэш, коммит, `recorded_at` и `ok`, прочие удаляются. До [`REFS_MAX`] ссылок.
//! - **Путь** — относительно корня решения, `/`, лексически нормализован
//!   ([`normalize_ref_path`]): абсолютный, выше корня, пустой, длиннее [`PATH_MAX_CHARS`]
//!   символов, с управляющими или невидимыми (bidi, нулевой ширины) символами, с сегментом
//!   `C:` в любом месте — ошибка.
//! - **Решение** ссылки — GUID пространства решения владельца, иначе `X-Zam-Solution` запроса
//!   ([`crate::Attribution::solution`]), иначе NULL (ссылка справочная). У Human — всегда NULL.
//! - **Устарелость** ([`files_changed`]): хэш сообщения ≠ записанного → `stale`
//!   (`stale_reason = source_changed`), совпал → `ok`; у ссылки без хэша первый присланный
//!   хэш становится ориентиром, она остаётся `ok`. [`files_verified`] возвращает ссылки
//!   пути в `ok`. Обе операции касаются только владельцев из переданных пространств.
//! - Ссылки удалённых владельцев остаются (восстановление их возвращает), но в индекс,
//!   фильтр поиска и граф не попадают; в индекс не попадают и замещённые/отозванные записи.
use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use zam_core::{Id, TargetKind, TargetRef};

use crate::entries::EntryKind;
use crate::nodes::Attribution;
use crate::time::fmt_ts;
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Предел ссылок одного владельца.
pub const REFS_MAX: usize = 64;
/// Предел длины пути ссылки, символов.
pub const PATH_MAX_CHARS: usize = 1000;
/// Предел файлов в одном сообщении `POST /files/changed` и `/files/verified`.
pub const FILES_MAX: usize = 500;
/// Записей на путь в индексе [`refs_index`].
pub const INDEX_ITEMS_MAX: usize = 5;
/// Предел длины коммита ссылки и `verified_at_commit`, символов.
pub const COMMIT_MAX_CHARS: usize = 64;
/// Причина устарелости: файл изменился.
pub const SOURCE_CHANGED: &str = "source_changed";
/// Ключ `meta`: ссылки и `verified_at` ещё не дозаполнены (миграция 0016).
pub(crate) const REFS_BACKFILL: &str = "refs_backfill";
/// Заметок или записей в одном пакете дозаполнения.
const BACKFILL_BATCH: i64 = 200;

/// Ссылка на файл: нормализованный путь, blake3 содержимого (64 hex) и коммит, если известны.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRef {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl FileRef {
    /// Ссылка без хэша и коммита.
    pub fn path_only(path: String) -> FileRef {
        FileRef {
            path,
            hash: None,
            commit: None,
        }
    }
}

/// Состояние ссылки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RefState {
    Ok,
    Stale,
}

impl RefState {
    pub fn as_str(self) -> &'static str {
        match self {
            RefState::Ok => "ok",
            RefState::Stale => "stale",
        }
    }
}

impl ToSql for RefState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for RefState {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "ok" => Ok(RefState::Ok),
            "stale" => Ok(RefState::Stale),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестное состояние ссылки: {other}").into(),
            )),
        }
    }
}

/// Путь ссылки не принят.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("пустой путь")]
    Empty,
    #[error("путь длиннее {PATH_MAX_CHARS} символов")]
    TooLong,
    #[error("управляющие или невидимые символы в пути")]
    Control,
    #[error("абсолютный путь: нужен путь относительно корня решения")]
    Absolute,
    #[error("путь выходит выше корня решения")]
    AboveRoot,
}

/// Лексическая нормализация пути относительно корня решения: края без пробелов, `\` → `/`,
/// пустые сегменты и `.` убираются, `..` сворачивается. Абсолютный путь (`/x`, `\\server`,
/// `C:\x`, `C:x`), выход выше корня, пустой итог, длина больше [`PATH_MAX_CHARS`] символов и
/// управляющие символы (в том числе NUL) — ошибка.
pub fn normalize_ref_path(p: &str) -> Result<String, PathError> {
    let p = p.trim();
    if p.is_empty() {
        return Err(PathError::Empty);
    }
    if p.chars().count() > PATH_MAX_CHARS {
        return Err(PathError::TooLong);
    }
    if p.chars().any(|c| c.is_control() || is_invisible(c)) {
        return Err(PathError::Control);
    }
    let p = p.replace('\\', "/");
    if p.starts_with('/') {
        return Err(PathError::Absolute);
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    return Err(PathError::AboveRoot);
                }
            }
            // `C:` в любом сегменте: после сворачивания `..` он стал бы началом пути
            s if is_drive(s) => return Err(PathError::Absolute),
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return Err(PathError::Empty);
    }
    let norm = out.join("/");
    if norm.starts_with('/') || is_drive(&norm) {
        return Err(PathError::Absolute);
    }
    Ok(norm)
}

/// Сегмент начинается с буквы диска: `C:`, `c:x`.
fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Невидимые символы форматирования (Unicode `Cf`, `Zl`, `Zp`): bidi-управляющие и нулевой
/// ширины — путь с ними выглядит как другой.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// Хэш содержимого: 64 шестнадцатеричных символа (регистр приводится к нижнему).
pub fn check_hash(h: &str) -> Option<String> {
    let h = h.trim();
    (h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
}

/// Коммит git: от 4 до [`COMMIT_MAX_CHARS`] шестнадцатеричных символов (нижний регистр).
pub fn check_commit(c: &str) -> Option<String> {
    let c = c.trim();
    ((4..=COMMIT_MAX_CHARS).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| c.to_ascii_lowercase())
}

fn owner_kind(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Node => "node",
        TargetKind::Entry => "entry",
    }
}

fn bad(m: impl Into<String>) -> StoreError {
    StoreError::BadInput(m.into())
}

/// Решение ссылок владельца в пространстве `space`: GUID пространства решения; у Human —
/// NULL; у остальных — `X-Zam-Solution` запроса (`a.solution`).
pub(crate) fn owner_solution(
    c: &Connection,
    space: Id,
    a: &Attribution,
) -> Result<Option<String>, StoreError> {
    let (kind, guid): (SpaceKind, Option<String>) = c
        .prepare_cached("SELECT kind, solution_guid FROM spaces WHERE id = ?1")?
        .query_row([space], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or(StoreError::NotFound)?;
    Ok(match kind {
        SpaceKind::Solution => guid,
        SpaceKind::Human => None,
        SpaceKind::Rules | SpaceKind::Knowledge => a.solution.clone(),
    })
}

/// Заменяет набор ссылок владельца (см. модуль). Больше [`REFS_MAX`] ссылок или
/// ненормализуемый путь → `BadInput`; повтор пути — первая ссылка.
pub fn set_refs(
    tx: &Connection,
    owner: TargetRef,
    solution: Option<&str>,
    refs: &[FileRef],
    now: &str,
) -> Result<(), StoreError> {
    let refs = dedup(refs)?;
    if refs.len() > REFS_MAX {
        return Err(bad(format!("refs: не больше {REFS_MAX} ссылок")));
    }
    let kind = owner_kind(owner.kind);
    let keep: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
    tx.prepare_cached(
        "DELETE FROM file_refs WHERE owner_kind = ?1 AND owner_id = ?2
           AND path NOT IN (SELECT value FROM json_each(?3))",
    )?
    .execute(params![
        kind,
        owner.id,
        serde_json::Value::from(keep).to_string()
    ])?;
    let mut st = tx.prepare_cached(
        "INSERT INTO file_refs (owner_kind, owner_id, solution_guid, path, content_hash,
                                git_commit, recorded_at, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'ok')
         ON CONFLICT (owner_kind, owner_id, path) DO UPDATE SET
           solution_guid = excluded.solution_guid, content_hash = excluded.content_hash,
           git_commit = excluded.git_commit, recorded_at = excluded.recorded_at,
           state = 'ok', stale_at = NULL, stale_reason = NULL",
    )?;
    for r in &refs {
        st.execute(params![
            kind, owner.id, solution, r.path, r.hash, r.commit, now
        ])?;
    }
    Ok(())
}

/// Ссылки владельца (путь, хэш, коммит) по алфавиту пути.
pub(crate) fn owner_refs(tx: &Connection, owner: TargetRef) -> Result<Vec<FileRef>, StoreError> {
    let mut st = tx.prepare_cached(
        "SELECT path, content_hash, git_commit FROM file_refs
         WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY path",
    )?;
    let rows = st.query_map(params![owner_kind(owner.kind), owner.id], |r| {
        Ok(FileRef {
            path: r.get(0)?,
            hash: r.get(1)?,
            commit: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Замещение без `refs` (раунд 2 задачи 7, ревью): ссылки новой записи `new` на те же пути
/// получают от замещаемой `old` решение, `recorded_at` и состояние (`state`, `stale_at`,
/// `stale_reason`) — иначе ссылка Knowledge теряла бы `X-Zam-Solution` исходного запроса, а
/// `stale` молча становилась бы `ok`.
pub(crate) fn inherit_ref_state(tx: &Connection, old: Id, new: Id) -> Result<(), StoreError> {
    tx.prepare_cached(
        "UPDATE file_refs AS n SET solution_guid = o.solution_guid, recorded_at = o.recorded_at,
                state = o.state, stale_at = o.stale_at, stale_reason = o.stale_reason
         FROM file_refs AS o
         WHERE n.owner_kind = 'entry' AND n.owner_id = ?2
           AND o.owner_kind = 'entry' AND o.owner_id = ?1 AND o.path = n.path",
    )?
    .execute(params![old, new])?;
    Ok(())
}

/// Дописывает ссылки без хэша на пути, которых у владельца ещё нет (до [`REFS_MAX`] всего).
fn add_paths(
    tx: &Connection,
    owner: TargetRef,
    solution: Option<&str>,
    paths: &[String],
    now: &str,
) -> Result<(), StoreError> {
    let kind = owner_kind(owner.kind);
    let have: HashSet<String> = tx
        .prepare_cached("SELECT path FROM file_refs WHERE owner_kind = ?1 AND owner_id = ?2")?
        .query_map(params![kind, owner.id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut n = have.len();
    let mut st = tx.prepare_cached(
        "INSERT OR IGNORE INTO file_refs (owner_kind, owner_id, solution_guid, path,
                                          recorded_at, state)
         VALUES (?1, ?2, ?3, ?4, ?5, 'ok')",
    )?;
    for p in paths {
        if n >= REFS_MAX {
            break;
        }
        if !have.contains(p) {
            st.execute(params![kind, owner.id, solution, p, now])?;
            n += 1;
        }
    }
    Ok(())
}

/// Нормализованные ссылки без повторов пути (первая побеждает); путь не принят — `BadInput`.
fn dedup(refs: &[FileRef]) -> Result<Vec<FileRef>, StoreError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let path = normalize_ref_path(&r.path).map_err(|e| bad(format!("refs: {e}")))?;
        if seen.insert(path.clone()) {
            out.push(FileRef {
                path,
                hash: r.hash.clone(),
                commit: r.commit.clone(),
            });
        }
    }
    Ok(out)
}

/// Пути строк `data.refs.files` changelog, которые нормализуются (прочие пропускаются: это
/// свободные строки с M3).
pub fn changelog_paths(data: &serde_json::Value) -> Vec<String> {
    data.pointer("/refs/files")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(|s| normalize_ref_path(s).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Набор ссылок записи: ссылки запроса, затем пути `data.refs.files` changelog без хэша —
/// до [`REFS_MAX`] всего (строки сверх предела пропускаются).
fn entry_set(kind: EntryKind, refs: &[FileRef], data: &serde_json::Value) -> Vec<FileRef> {
    let mut out = refs.to_vec();
    if kind == EntryKind::Changelog {
        let have: HashSet<String> = refs
            .iter()
            .filter_map(|r| normalize_ref_path(&r.path).ok())
            .collect();
        for p in changelog_paths(data) {
            if out.len() >= REFS_MAX.max(refs.len()) {
                break;
            }
            if !have.contains(&p) && !out.iter().any(|r| r.path == p) {
                out.push(FileRef::path_only(p));
            }
        }
    }
    out
}

/// Ссылки новой записи (в транзакции её создания).
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_new_entry(
    tx: &Connection,
    id: Id,
    space: Id,
    kind: EntryKind,
    refs: &[FileRef],
    data: &serde_json::Value,
    a: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let set = entry_set(kind, refs, data);
    if set.is_empty() {
        return Ok(());
    }
    let solution = owner_solution(tx, space, a)?;
    let owner = TargetRef {
        kind: TargetKind::Entry,
        id,
    };
    set_refs(tx, owner, solution.as_deref(), &set, now)
}

/// Ссылки после правки записи: `refs` задан — набор заменяется (с путями `data.refs.files`
/// итоговой `data` changelog); иначе у changelog с новой `data` (`data_changed`) новые пути
/// строк дописываются без хэша.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_patched_entry(
    tx: &Connection,
    id: Id,
    space: Id,
    kind: EntryKind,
    refs: Option<&[FileRef]>,
    data: &serde_json::Value,
    data_changed: bool,
    a: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let owner = TargetRef {
        kind: TargetKind::Entry,
        id,
    };
    match refs {
        Some(refs) => {
            let set = entry_set(kind, refs, data);
            let solution = owner_solution(tx, space, a)?;
            set_refs(tx, owner, solution.as_deref(), &set, now)
        }
        None if kind == EntryKind::Changelog && data_changed => {
            let paths = changelog_paths(data);
            if paths.is_empty() {
                return Ok(());
            }
            let solution = owner_solution(tx, space, a)?;
            add_paths(tx, owner, solution.as_deref(), &paths, now)
        }
        None => Ok(()),
    }
}

/// Пути `refs:` из frontmatter заметки: нормализуемые, без повторов, до [`REFS_MAX`].
pub fn note_ref_paths(fm: Option<&zam_markdown::Frontmatter>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in fm.map(|f| f.refs.as_slice()).unwrap_or_default() {
        if out.len() >= REFS_MAX {
            break;
        }
        if let Ok(p) = normalize_ref_path(p)
            && !out.contains(&p)
        {
            out.push(p);
        }
    }
    out
}

/// Ревизия заметки: `verified_at`, `verified_at_commit` и ссылки из её frontmatter (в
/// транзакции записи ревизии). Ненормализуемые пути и сверх [`REFS_MAX`] пропускаются —
/// сервер отдаёт о них предупреждения.
pub(crate) fn sync_note(
    tx: &Connection,
    node: Id,
    fm: Option<&zam_markdown::Frontmatter>,
    a: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let space: Id = tx
        .prepare_cached("SELECT space_id FROM nodes WHERE id = ?1")?
        .query_row([node], |r| r.get(0))?;
    let (verified, commit) = verified_of(fm);
    tx.prepare_cached("UPDATE nodes SET verified_at = ?2, verified_at_commit = ?3 WHERE id = ?1")?
        .execute(params![node, verified, commit])?;
    let paths = note_ref_paths(fm);
    let owner = TargetRef {
        kind: TargetKind::Node,
        id: node,
    };
    if paths.is_empty() {
        // прежние ссылки снимаются (поле `refs:` убрали)
        tx.prepare_cached("DELETE FROM file_refs WHERE owner_kind = 'node' AND owner_id = ?1")?
            .execute([node])?;
        return Ok(());
    }
    let solution = owner_solution(tx, space, a)?;
    let set: Vec<FileRef> = paths.into_iter().map(FileRef::path_only).collect();
    set_refs(tx, owner, solution.as_deref(), &set, now)
}

/// `verified_at` и `verified_at_commit` (sha по [`check_commit`], иначе нет) из frontmatter.
fn verified_of(fm: Option<&zam_markdown::Frontmatter>) -> (Option<String>, Option<String>) {
    let Some(f) = fm else {
        return (None, None);
    };
    let commit = f.verified_at_commit.as_deref().and_then(check_commit);
    (f.verified_at.clone(), commit)
}

/// Ссылки решения `?1` на путь `?2` у владельцев из пространств `?3` (JSON-массив), в том
/// числе удалённых: участнику не видны ссылки базы владельца вне пространства решения.
const REFS_IN_SPACES: &str = "SELECT f.id, f.content_hash, f.state FROM file_refs f
     LEFT JOIN nodes n ON f.owner_kind = 'node' AND n.id = f.owner_id
     LEFT JOIN entries e ON f.owner_kind = 'entry' AND e.id = f.owner_id
     WHERE f.solution_guid = ?1 AND f.path = ?2
       AND coalesce(n.space_id, e.space_id) IN (SELECT value FROM json_each(?3))";

/// Сообщение об изменении файлов решения `solution` (`(путь, хэш)`, пути нормализованы
/// вызывающим) для ссылок владельцев из `spaces`: хэш другой — `stale`, совпал — `ok`;
/// у ссылки без хэша присланный хэш становится ориентиром, ссылка остаётся `ok` (решение
/// контроллера, раунд 1 задачи 7). Возвращает, сколько ссылок этих путей после этого `stale`.
pub async fn files_changed(
    db: &Db,
    solution: &str,
    spaces: &[Id],
    files: &[(String, String)],
    now: Timestamp,
) -> Result<u32, StoreError> {
    let solution = solution.to_string();
    let spaces = ids_json(spaces);
    let files = files.to_vec();
    db.write(move |tx| {
        let now = fmt_ts(now);
        let mut stale = 0u32;
        let mut sel = tx.prepare_cached(REFS_IN_SPACES)?;
        let mut to_stale = tx.prepare_cached(
            "UPDATE file_refs SET state = 'stale', stale_reason = ?2,
                    stale_at = CASE WHEN state = 'stale' THEN stale_at ELSE ?3 END
             WHERE id = ?1",
        )?;
        let mut to_ok = tx.prepare_cached(
            "UPDATE file_refs SET state = 'ok', stale_at = NULL, stale_reason = NULL
             WHERE id = ?1",
        )?;
        let mut adopt = tx.prepare_cached(
            "UPDATE file_refs SET content_hash = ?2, state = 'ok', stale_at = NULL,
                    stale_reason = NULL
             WHERE id = ?1",
        )?;
        for (path, hash) in &files {
            let rows: Vec<(i64, Option<String>, RefState)> = sel
                .query_map(params![solution, path, spaces], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<Result<_, _>>()?;
            for (id, recorded, state) in rows {
                match recorded.as_deref() {
                    Some(h) if h == hash => {
                        if state == RefState::Stale {
                            to_ok.execute([id])?;
                        }
                    }
                    Some(_) => {
                        to_stale.execute(params![id, SOURCE_CHANGED, now])?;
                        stale += 1;
                    }
                    // без хэша: первый присланный хэш — ориентир
                    None => {
                        adopt.execute(params![id, hash])?;
                    }
                }
            }
        }
        Ok(stale)
    })
    .await
}

/// «Проверено, знание актуально»: ссылки решения на эти пути у владельцев из `spaces` —
/// `ok`, `recorded_at` — сейчас, хэш — присланный (без него — NULL: ориентиром станет хэш
/// следующего сообщения). Возвращает число обновлённых ссылок.
pub async fn files_verified(
    db: &Db,
    solution: &str,
    spaces: &[Id],
    files: &[(String, Option<String>)],
    now: Timestamp,
) -> Result<u32, StoreError> {
    let solution = solution.to_string();
    let spaces = ids_json(spaces);
    let files = files.to_vec();
    db.write(move |tx| {
        let now = fmt_ts(now);
        let mut sel = tx.prepare_cached(REFS_IN_SPACES)?;
        let mut st = tx.prepare_cached(
            "UPDATE file_refs SET state = 'ok', stale_at = NULL, stale_reason = NULL,
                    recorded_at = ?2, content_hash = ?3
             WHERE id = ?1",
        )?;
        let mut n = 0u32;
        for (path, hash) in &files {
            let ids: Vec<i64> = sel
                .query_map(params![solution, path, spaces], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for id in ids {
                n += st.execute(params![id, now, hash])? as u32;
            }
        }
        Ok(n)
    })
    .await
}

/// Запись индекса для хуков: ссылка `n:`/`i:`/`c:`/… , вид, заголовок и состояние ссылки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: RefState,
}

/// Путь индекса: последний записанный хэш и до [`INDEX_ITEMS_MAX`] записей (новые первыми).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathRefs {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub items: Vec<RefItem>,
}

/// Условие живого владельца из пространств `?2` (JSON-массив) для `file_refs f`,
/// `LEFT JOIN nodes n` и `LEFT JOIN entries e`.
const LIVE_OWNER: &str = "((f.owner_kind = 'node' AND n.deleted_at IS NULL
        AND n.space_id IN (SELECT value FROM json_each(?2)))
     OR (f.owner_kind = 'entry' AND e.deleted_at IS NULL
        AND e.space_id IN (SELECT value FROM json_each(?2))
        AND (e.status IS NULL OR e.status NOT IN ('superseded', 'retracted'))))";

const OWNER_JOIN: &str = "LEFT JOIN nodes n ON f.owner_kind = 'node' AND n.id = f.owner_id
     LEFT JOIN entries e ON f.owner_kind = 'entry' AND e.id = f.owner_id";

fn ids_json(ids: &[Id]) -> String {
    serde_json::Value::from(ids.iter().map(Id::to_string).collect::<Vec<_>>()).to_string()
}

/// Ссылка выдачи владельца: `n:<id>` или буква вида записи.
fn owner_ref(kind: &str, id: &str, entry_kind: Option<&str>) -> (String, String) {
    match (kind, entry_kind.and_then(EntryKind::parse)) {
        ("entry", Some(k)) => (format!("{}:{id}", k.ref_letter()), k.as_str().to_string()),
        ("entry", None) => (format!("e:{id}"), "entry".to_string()),
        _ => (format!("n:{id}"), "note".to_string()),
    }
}

/// Индекс ссылок решения `solution` из пространств `spaces`: пути по алфавиту, у каждого —
/// последний записанный хэш и до [`INDEX_ITEMS_MAX`] живых владельцев.
pub async fn refs_index(
    db: &Db,
    solution: &str,
    spaces: &[Id],
) -> Result<Vec<PathRefs>, StoreError> {
    let solution = solution.to_string();
    let spaces = ids_json(spaces);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT f.path, f.content_hash, f.state, f.owner_kind, f.owner_id,
                    coalesce(n.name, e.title, ''), e.kind
             FROM file_refs f {OWNER_JOIN}
             WHERE f.solution_guid = ?1 AND {LIVE_OWNER}
             ORDER BY f.path, f.recorded_at DESC, f.id DESC"
        ))?;
        let rows = st.query_map(params![solution, spaces], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, RefState>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut out: Vec<PathRefs> = Vec::new();
        for row in rows {
            let (path, hash, state, kind, id, title, ekind) = row?;
            if out.last().is_none_or(|p| p.path != path) {
                out.push(PathRefs {
                    path,
                    hash: None,
                    items: Vec::new(),
                });
            }
            let cur = out.last_mut().expect("путь только что добавлен");
            if cur.hash.is_none() {
                cur.hash = hash;
            }
            if cur.items.len() < INDEX_ITEMS_MAX {
                let (id, kind) = owner_ref(&kind, &id, ekind.as_deref());
                cur.items.push(RefItem {
                    id,
                    kind,
                    title,
                    state,
                });
            }
        }
        Ok(out)
    })
    .await
}

/// Живые владельцы ссылок на путь `path` (нормализован вызывающим) в пространствах `spaces`
/// — в любом решении: «что память знает об этом файле». Новые первыми.
pub async fn refs_to_path(
    db: &Db,
    path: &str,
    spaces: &[Id],
) -> Result<Vec<TargetRef>, StoreError> {
    let path = path.to_string();
    let spaces = ids_json(spaces);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT f.owner_kind, f.owner_id FROM file_refs f {OWNER_JOIN}
             WHERE f.path = ?1 AND {LIVE_OWNER}
             ORDER BY f.recorded_at DESC, f.id DESC"
        ))?;
        let rows = st.query_map(params![path, spaces], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Id>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, id) = row?;
            let kind = if kind == "node" {
                TargetKind::Node
            } else {
                TargetKind::Entry
            };
            let r = TargetRef { kind, id };
            if !out.contains(&r) {
                out.push(r);
            }
        }
        Ok(out)
    })
    .await
}

/// Пути ссылок `stale` у владельцев `owners` (по алфавиту); владельцы без них в ответ не входят.
pub async fn stale_refs_of(
    db: &Db,
    owners: &[TargetRef],
) -> Result<HashMap<TargetRef, Vec<String>>, StoreError> {
    if owners.is_empty() {
        return Ok(HashMap::new());
    }
    let of = |k: TargetKind| {
        ids_json(
            &owners
                .iter()
                .filter(|o| o.kind == k)
                .map(|o| o.id)
                .collect::<Vec<_>>(),
        )
    };
    let (nodes, entries) = (of(TargetKind::Node), of(TargetKind::Entry));
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT owner_kind, owner_id, path FROM file_refs
             WHERE state = 'stale'
               AND ((owner_kind = 'node' AND owner_id IN (SELECT value FROM json_each(?1)))
                 OR (owner_kind = 'entry' AND owner_id IN (SELECT value FROM json_each(?2))))
             ORDER BY owner_kind, owner_id, path",
        )?;
        let rows = st.query_map(params![nodes, entries], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Id>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out: HashMap<TargetRef, Vec<String>> = HashMap::new();
        for row in rows {
            let (kind, id, path) = row?;
            let kind = if kind == "node" {
                TargetKind::Node
            } else {
                TargetKind::Entry
            };
            out.entry(TargetRef { kind, id }).or_default().push(path);
        }
        Ok(out)
    })
    .await
}

/// Дозаполняет после миграции 0016 `verified_at`, `verified_at_commit` и ссылки заметок
/// (и удалённых: восстановление их не пересчитывает) из frontmatter текущей ревизии, затем ссылки changelog из `data.refs.files`
/// (без хэша; решение — пространство решения, у Knowledge — NULL). Пакетами по
/// [`BACKFILL_BATCH`]; флаг `meta.refs_backfill` снимается в конце, прерванное
/// дозаполнение повторится при следующем открытии (оно идемпотентно). Возвращает число
/// владельцев со ссылками или `verified_at`.
pub(crate) async fn backfill_refs(db: &Db) -> Result<u64, StoreError> {
    let pending = db
        .read(|c| {
            Ok(
                c.query_row("SELECT 1 FROM meta WHERE key = ?1", [REFS_BACKFILL], |_| {
                    Ok(())
                })
                .optional()?
                .is_some(),
            )
        })
        .await?;
    if !pending {
        return Ok(0);
    }
    let system = Attribution::new(crate::nodes::AuthorKind::System);
    let (mut total, mut after) = (0u64, None::<Id>);
    // заметки
    loop {
        let cursor = after;
        let a = system.clone();
        let (n, found, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                // заметка без текущей ревизии (повреждённая база) пропускается
                let batch: Vec<(Id, String)> = tx
                    .prepare_cached(
                        "SELECT n.id, r.content FROM nodes n
                         JOIN revisions r ON r.node_id = n.id AND r.rev = n.current_rev
                         WHERE n.kind = 'note' AND (?1 IS NULL OR n.id > ?1)
                         ORDER BY n.id LIMIT ?2",
                    )?
                    .query_map(params![cursor, BACKFILL_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?;
                let mut found = 0u64;
                for (id, text) in &batch {
                    let doc = zam_markdown::parse(text);
                    let fm = doc.frontmatter.as_ref();
                    if fm.is_some_and(|f| !f.refs.is_empty() || f.verified_at.is_some()) {
                        sync_note(tx, *id, fm, &a, &now)?;
                        found += 1;
                    }
                }
                Ok((batch.len() as i64, found, batch.last().map(|b| b.0)))
            })
            .await?;
        total += found;
        match last {
            Some(l) if n == BACKFILL_BATCH => after = Some(l),
            _ => break,
        }
    }
    // changelog
    after = None;
    loop {
        let cursor = after;
        let a = system.clone();
        let (n, found, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, Id, String)> = tx
                    .prepare_cached(
                        "SELECT id, space_id, data FROM entries
                         WHERE kind = 'changelog' AND (?1 IS NULL OR id > ?1)
                         ORDER BY id LIMIT ?2",
                    )?
                    .query_map(params![cursor, BACKFILL_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<Result<_, _>>()?;
                let mut found = 0u64;
                for (id, space, data) in &batch {
                    let data: serde_json::Value =
                        serde_json::from_str(data).unwrap_or(serde_json::Value::Null);
                    let paths = changelog_paths(&data);
                    if paths.is_empty() {
                        continue;
                    }
                    let owner = TargetRef {
                        kind: TargetKind::Entry,
                        id: *id,
                    };
                    let solution = owner_solution(tx, *space, &a)?;
                    add_paths(tx, owner, solution.as_deref(), &paths, &now)?;
                    found += 1;
                }
                let last = batch.last().map(|b| b.0);
                if (batch.len() as i64) < BACKFILL_BATCH {
                    tx.execute("DELETE FROM meta WHERE key = ?1", [REFS_BACKFILL])?;
                }
                Ok((batch.len() as i64, found, last))
            })
            .await?;
        total += found;
        match last {
            Some(l) if n == BACKFILL_BATCH => after = Some(l),
            _ => break,
        }
    }
    if total > 0 {
        tracing::info!(owners = total, "ссылки на файлы дозаполнены (v16)");
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        let ok = |p: &str| normalize_ref_path(p).unwrap();
        assert_eq!(ok("a\\b.rs"), "a/b.rs");
        assert_eq!(ok("./a/b.rs"), "a/b.rs");
        assert_eq!(ok("a/../b.rs"), "b.rs");
        assert_eq!(ok("a//b/./c/"), "a/b/c");
        assert_eq!(ok("  src/main.rs "), "src/main.rs");
        // двоеточие не после первой буквы — обычный символ имени
        assert_eq!(ok("ab:c/d"), "ab:c/d");
        let err = |p: &str| normalize_ref_path(p).unwrap_err();
        assert_eq!(err("/abs"), PathError::Absolute);
        assert_eq!(err("C:\\x"), PathError::Absolute);
        assert_eq!(err("c:x"), PathError::Absolute);
        assert_eq!(err("\\\\server\\share\\f"), PathError::Absolute);
        // буква диска после сворачивания `.` и `..` — тоже абсолютный
        assert_eq!(err("./C:/x"), PathError::Absolute);
        assert_eq!(err("a/../C:/x"), PathError::Absolute);
        assert_eq!(err("a/../c:x"), PathError::Absolute);
        assert_eq!(err("a\\..\\D:\\x"), PathError::Absolute);
        assert_eq!(err("src/c:x"), PathError::Absolute);
        // bidi и нулевой ширины
        for c in [
            '\u{202E}', '\u{200B}', '\u{200F}', '\u{2066}', '\u{FEFF}', '\u{2028}', '\u{2029}',
        ] {
            assert_eq!(
                err(&format!("a{c}b.rs")),
                PathError::Control,
                "{:04X}",
                c as u32
            );
        }
        assert_eq!(err("../x"), PathError::AboveRoot);
        assert_eq!(err("a/../../x"), PathError::AboveRoot);
        assert_eq!(err(""), PathError::Empty);
        assert_eq!(err("   "), PathError::Empty);
        assert_eq!(err("./."), PathError::Empty);
        assert_eq!(err("a/.."), PathError::Empty);
        assert_eq!(err("a\0b"), PathError::Control);
        assert_eq!(err("a\nb"), PathError::Control);
        assert_eq!(err("a\u{7f}b"), PathError::Control);
        assert_eq!(err(&"я".repeat(PATH_MAX_CHARS + 1)), PathError::TooLong);
        assert_eq!(
            ok(&"я".repeat(PATH_MAX_CHARS)).chars().count(),
            PATH_MAX_CHARS
        );
    }

    #[test]
    fn hash_and_commit_checked() {
        let h = "A".repeat(64);
        assert_eq!(check_hash(&h), Some("a".repeat(64)));
        assert_eq!(check_hash(&"a".repeat(63)), None);
        assert_eq!(check_hash(&"g".repeat(64)), None);
        assert_eq!(check_commit("abc1"), Some("abc1".into()));
        assert_eq!(check_commit("abc"), None);
        assert_eq!(check_commit("xyz123"), None);
    }

    #[test]
    fn changelog_set_merges_strings_without_hash() {
        let data = serde_json::json!({"refs": {"files": ["a.rs", "../bad", "./b.rs", "a.rs"]}});
        let req = vec![FileRef {
            path: "b.rs".into(),
            hash: Some("0".repeat(64)),
            commit: None,
        }];
        let set = entry_set(EntryKind::Changelog, &req, &data);
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].hash.as_deref(), Some("0".repeat(64).as_str()));
        assert_eq!(set[1], FileRef::path_only("a.rs".into()));
        // не changelog — строки data не читаются
        assert_eq!(entry_set(EntryKind::Insight, &req, &data).len(), 1);
    }
}
