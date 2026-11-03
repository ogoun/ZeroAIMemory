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
//! - **Ссылка на символ** (M5.6 §8): `symbol` — квалифицированное имя (`Store::open`, `foo#2`),
//!   `symbol_hash` — `body_hash` тела при записи. Ключ ссылки владельца — `(путь, символ)`: файл
//!   целиком и его символы — разные ссылки. Строка `путь#qual` (frontmatter `refs:`,
//!   `data.refs.files` changelog) разбирается [`zam_code::split_ref`]. Устарелость символьной
//!   ссылки — по телу символа, если в сообщении есть символы файла ([`files_changed`]).
//! - Ссылки удалённых владельцев остаются (восстановление их возвращает), но в индекс,
//!   фильтр поиска и граф не попадают; в индекс не попадают и замещённые/отозванные записи.
use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use zam_code::{MISSING_HASH, SymbolHash};
use zam_core::{Id, TargetKind, TargetRef};

use crate::entries::EntryKind;
use crate::nodes::Attribution;
use crate::time::fmt_ts;
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Предел ссылок одного владельца.
pub const REFS_MAX: usize = 64;
/// Предел длины пути ссылки, символов (общий с разбором `путь#qual`, [`zam_code::REF_MAX_BYTES`]).
pub use zam_code::PATH_MAX_CHARS;
/// Предел файлов в одном сообщении `POST /files/changed` и `/files/verified`.
pub const FILES_MAX: usize = 500;
/// Записей на путь в индексе [`refs_index`].
pub const INDEX_ITEMS_MAX: usize = 5;
/// Предел длины коммита ссылки и `verified_at_commit`, символов.
pub const COMMIT_MAX_CHARS: usize = 64;
/// Причина устарелости: файл изменился.
pub const SOURCE_CHANGED: &str = "source_changed";
/// Причина устарелости символьной ссылки: тело символа изменилось (M5.6 §8).
pub const SYMBOL_CHANGED: &str = "symbol_changed";
/// Причина устарелости символьной ссылки: символа (или файла) больше нет (M5.6 §8).
pub const SYMBOL_REMOVED: &str = "symbol_removed";
/// Ключ `meta`: ссылки и `verified_at` ещё не дозаполнены (миграция 0016).
pub(crate) const REFS_BACKFILL: &str = "refs_backfill";
/// Заметок или записей в одном пакете дозаполнения.
const BACKFILL_BATCH: i64 = 200;

/// Ссылка на файл или его символ: нормализованный путь, квалифицированное имя символа,
/// blake3 содержимого (64 hex), `body_hash` символа (16 hex) и коммит, если известны.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRef {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl FileRef {
    /// Ссылка на файл без хэша и коммита.
    pub fn path_only(path: String) -> FileRef {
        FileRef {
            path,
            symbol: None,
            hash: None,
            symbol_hash: None,
            commit: None,
        }
    }

    /// Ссылка из строки `путь` или `путь#qual` ([`zam_code::split_ref`]) без хэшей; путь не
    /// нормализуется.
    pub fn parse(raw: &str) -> FileRef {
        let (path, symbol) = zam_code::split_ref(raw.trim());
        FileRef {
            symbol: symbol.map(str::to_string),
            ..FileRef::path_only(path.to_string())
        }
    }

    /// Ключ ссылки владельца: `(путь, символ или "")`.
    fn key(&self) -> (&str, &str) {
        (&self.path, self.symbol.as_deref().unwrap_or(""))
    }

    /// Строка `путь` или `путь#qual`.
    pub fn display(&self) -> String {
        match &self.symbol {
            Some(s) => format!("{}#{s}", self.path),
            None => self.path.clone(),
        }
    }
}

/// Файл сообщения об изменении (`POST /files/changed`, `PUT /code/files`): путь (нормализован
/// вызывающим), blake3 содержимого (или [`MISSING_HASH`] — файл удалён) и, если клиент
/// разобрал файл, хэши тел его символов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub hash: String,
    pub symbols: Option<Vec<SymbolHash>>,
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

/// `file_refs.owner_kind` владельца. У вложения ссылок на файлы нет (CHECK `file_refs`) —
/// `BadInput`, а не строка, которую таблица не примет.
fn owner_kind(k: TargetKind) -> Result<&'static str, StoreError> {
    match k {
        TargetKind::Node => Ok("node"),
        TargetKind::Entry => Ok("entry"),
        TargetKind::Attachment => Err(bad("у вложения нет ссылок на файлы")),
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
    let kind = owner_kind(owner.kind)?;
    let keep: Vec<serde_json::Value> = refs
        .iter()
        .map(|r| {
            let (p, s) = r.key();
            serde_json::json!([p, s])
        })
        .collect();
    tx.prepare_cached(
        "DELETE FROM file_refs WHERE owner_kind = ?1 AND owner_id = ?2
           AND json_array(path, coalesce(symbol, '')) NOT IN (SELECT value FROM json_each(?3))",
    )?
    .execute(params![
        kind,
        owner.id,
        serde_json::Value::from(keep).to_string()
    ])?;
    let mut st = tx.prepare_cached(
        "INSERT INTO file_refs (owner_kind, owner_id, solution_guid, path, symbol, content_hash,
                                symbol_hash, git_commit, recorded_at, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'ok')
         ON CONFLICT (owner_kind, owner_id, path, COALESCE(symbol, '')) DO UPDATE SET
           solution_guid = excluded.solution_guid, content_hash = excluded.content_hash,
           symbol_hash = excluded.symbol_hash, git_commit = excluded.git_commit,
           recorded_at = excluded.recorded_at, state = 'ok', stale_at = NULL,
           stale_reason = NULL",
    )?;
    for r in &refs {
        st.execute(params![
            kind,
            owner.id,
            solution,
            r.path,
            r.symbol,
            r.hash,
            r.symbol_hash,
            r.commit,
            now
        ])?;
    }
    Ok(())
}

/// Ссылки владельца (путь, символ, хэши, коммит) по алфавиту пути, файловая — перед символьными.
pub(crate) fn owner_refs(tx: &Connection, owner: TargetRef) -> Result<Vec<FileRef>, StoreError> {
    let mut st = tx.prepare_cached(
        "SELECT path, symbol, content_hash, symbol_hash, git_commit FROM file_refs
         WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY path, coalesce(symbol, '')",
    )?;
    let rows = st.query_map(params![owner_kind(owner.kind)?, owner.id], |r| {
        Ok(FileRef {
            path: r.get(0)?,
            symbol: r.get(1)?,
            hash: r.get(2)?,
            symbol_hash: r.get(3)?,
            commit: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Замещение без `refs` (раунд 2 задачи 7, ревью): ссылки новой записи `new` на те же пути (и
/// символы)
/// получают от замещаемой `old` решение, `recorded_at` и состояние (`state`, `stale_at`,
/// `stale_reason`) — иначе ссылка Knowledge теряла бы `X-Zam-Solution` исходного запроса, а
/// `stale` молча становилась бы `ok`.
pub(crate) fn inherit_ref_state(tx: &Connection, old: Id, new: Id) -> Result<(), StoreError> {
    tx.prepare_cached(
        "UPDATE file_refs AS n SET solution_guid = o.solution_guid, recorded_at = o.recorded_at,
                state = o.state, stale_at = o.stale_at, stale_reason = o.stale_reason
         FROM file_refs AS o
         WHERE n.owner_kind = 'entry' AND n.owner_id = ?2
           AND o.owner_kind = 'entry' AND o.owner_id = ?1 AND o.path = n.path
           AND coalesce(o.symbol, '') = coalesce(n.symbol, '')",
    )?
    .execute(params![old, new])?;
    Ok(())
}

/// Дописывает ссылки без хэша на `(путь, символ)`, которых у владельца ещё нет (до
/// [`REFS_MAX`] всего).
fn add_refs(
    tx: &Connection,
    owner: TargetRef,
    solution: Option<&str>,
    refs: &[FileRef],
    now: &str,
) -> Result<(), StoreError> {
    let kind = owner_kind(owner.kind)?;
    let mut have: HashSet<(String, String)> = tx
        .prepare_cached(
            "SELECT path, coalesce(symbol, '') FROM file_refs
             WHERE owner_kind = ?1 AND owner_id = ?2",
        )?
        .query_map(params![kind, owner.id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut st = tx.prepare_cached(
        "INSERT OR IGNORE INTO file_refs (owner_kind, owner_id, solution_guid, path, symbol,
                                          recorded_at, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ok')",
    )?;
    for r in refs {
        if have.len() >= REFS_MAX {
            break;
        }
        let (p, s) = r.key();
        if have.insert((p.to_string(), s.to_string())) {
            st.execute(params![kind, owner.id, solution, r.path, r.symbol, now])?;
        }
    }
    Ok(())
}

/// Нормализованные ссылки без повторов `(путь, символ)` (первая побеждает); путь не принят
/// — `BadInput`.
fn dedup(refs: &[FileRef]) -> Result<Vec<FileRef>, StoreError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let path = normalize_ref_path(&r.path).map_err(|e| bad(format!("refs: {e}")))?;
        let r = FileRef { path, ..r.clone() };
        if seen.insert((r.path.clone(), r.symbol.clone())) {
            out.push(r);
        }
    }
    Ok(out)
}

/// Строка `путь` или `путь#qual` → ссылка с нормализованным путём; путь не нормализуется —
/// `None`.
fn ref_of_str(raw: &str) -> Option<FileRef> {
    let r = FileRef::parse(raw);
    let path = normalize_ref_path(&r.path).ok()?;
    Some(FileRef { path, ..r })
}

/// Ссылки строк `data.refs.files` changelog (`путь` или `путь#qual`), которые нормализуются
/// (прочие пропускаются: это свободные строки с M3).
pub fn changelog_refs(data: &serde_json::Value) -> Vec<FileRef> {
    data.pointer("/refs/files")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(ref_of_str)
                .collect()
        })
        .unwrap_or_default()
}

/// Набор ссылок записи: ссылки запроса, затем ссылки `data.refs.files` changelog без хэша —
/// до [`REFS_MAX`] всего (строки сверх предела пропускаются).
fn entry_set(kind: EntryKind, refs: &[FileRef], data: &serde_json::Value) -> Vec<FileRef> {
    let mut out = refs.to_vec();
    if kind == EntryKind::Changelog {
        let mut have: HashSet<(String, Option<String>)> = refs
            .iter()
            .filter_map(|r| Some((normalize_ref_path(&r.path).ok()?, r.symbol.clone())))
            .collect();
        for r in changelog_refs(data) {
            if out.len() >= REFS_MAX.max(refs.len()) {
                break;
            }
            if have.insert((r.path.clone(), r.symbol.clone())) {
                out.push(r);
            }
        }
    }
    out
}

/// Ссылки новой записи (в транзакции её создания). Отчёт строк не пишет: его `data.refs` —
/// только данные, следов отчёта у файлов и заметок нет (спецификация M5.5 §7).
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
    if kind == EntryKind::Report {
        return Ok(());
    }
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
/// строк дописываются без хэша. Отчёт неизменяем → `ReportImmutable`.
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
    if kind == EntryKind::Report {
        return Err(StoreError::ReportImmutable);
    }
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
            let refs = changelog_refs(data);
            if refs.is_empty() {
                return Ok(());
            }
            let solution = owner_solution(tx, space, a)?;
            add_refs(tx, owner, solution.as_deref(), &refs, now)
        }
        None => Ok(()),
    }
}

/// Ссылки `refs:` из frontmatter заметки (`путь` или `путь#qual`, без хэшей): нормализуемые,
/// без повторов `(путь, символ)`, до [`REFS_MAX`].
pub fn note_refs(fm: Option<&zam_markdown::Frontmatter>) -> Vec<FileRef> {
    let mut out: Vec<FileRef> = Vec::new();
    for raw in fm.map(|f| f.refs.as_slice()).unwrap_or_default() {
        if out.len() >= REFS_MAX {
            break;
        }
        if let Some(r) = ref_of_str(raw)
            && !out.iter().any(|o| o.key() == r.key())
        {
            out.push(r);
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
    let set = note_refs(fm);
    let owner = TargetRef {
        kind: TargetKind::Node,
        id: node,
    };
    if set.is_empty() {
        // прежние ссылки снимаются (поле `refs:` убрали)
        tx.prepare_cached("DELETE FROM file_refs WHERE owner_kind = 'node' AND owner_id = ?1")?
            .execute([node])?;
        return Ok(());
    }
    let solution = owner_solution(tx, space, a)?;
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
const REFS_IN_SPACES: &str = "SELECT f.id, f.content_hash, f.state, f.symbol, f.symbol_hash,
            f.stale_reason
     FROM file_refs f
     LEFT JOIN nodes n ON f.owner_kind = 'node' AND n.id = f.owner_id
     LEFT JOIN entries e ON f.owner_kind = 'entry' AND e.id = f.owner_id
     WHERE f.solution_guid = ?1 AND f.path = ?2
       AND coalesce(n.space_id, e.space_id) IN (SELECT value FROM json_each(?3))";

/// Ссылка, которую судит правило устарелости.
struct RefRow {
    id: i64,
    hash: Option<String>,
    state: RefState,
    symbol: Option<String>,
    symbol_hash: Option<String>,
    stale_reason: Option<String>,
}

/// Ссылки решения на путь у владельцев из пространств (`spaces` — JSON-массив id).
fn refs_in_spaces(
    tx: &Connection,
    solution: &str,
    path: &str,
    spaces: &str,
) -> Result<Vec<RefRow>, StoreError> {
    let mut sel = tx.prepare_cached(REFS_IN_SPACES)?;
    let rows = sel.query_map(params![solution, path, spaces], |r| {
        Ok(RefRow {
            id: r.get(0)?,
            hash: r.get(1)?,
            state: r.get(2)?,
            symbol: r.get(3)?,
            symbol_hash: r.get(4)?,
            stale_reason: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Откуда сведения об изменении файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeSource {
    /// Хуки рабочей копии (`POST /files/changed`): файловые и символьные ссылки.
    WorkingCopy,
    /// Карта кода (`PUT /code/files`, удаление синхронизацией): только символьные ссылки и только
    /// по символам — хэш файла другой машины (CRLF/LF, BOM) ни файловых ссылок, ни хэша файла
    /// у символьных не трогает (решение контроллера по ревью задачи 3, I2).
    Map,
}

/// Файл для правила устарелости: [`ChangedFile`], полнота присланных символов и источник.
/// Неполный разбор (`partial`, предел символов): найденный символ судится по телу, не
/// найденный — по хэшу файла у [`ChangeSource::WorkingCopy`] и не судится у
/// [`ChangeSource::Map`] (он мог быть обрезан пределом, а не удалён).
pub(crate) struct Change<'a> {
    pub path: &'a str,
    pub hash: &'a str,
    pub symbols: Option<&'a [SymbolHash]>,
    pub complete: bool,
    pub source: ChangeSource,
}

/// Вердикт правила для одной ссылки.
enum Verdict<'a> {
    /// `ok`; ссылка принимает хэш файла и (если не было) хэш символа.
    Ok {
        hash: &'a str,
        symbol_hash: Option<&'a str>,
    },
    /// `ok` без изменения хэшей (хэш совпал).
    Same,
    /// Карта подтвердила тело символа: хэш тела — если не было; `stale` по символу снимается,
    /// `stale` по хэшу файла (его судят хуки рабочей копии) — остаётся.
    SymbolOk {
        symbol_hash: &'a str,
    },
    Stale(&'static str),
    /// Ссылку этот источник не судит.
    Skip,
}

/// Правило устарелости (спецификация M4.5 §6.2, M5.6 §8) для ссылки `r` и файла `c`.
fn verdict<'a>(r: &RefRow, c: &Change<'a>) -> Verdict<'a> {
    let map = c.source == ChangeSource::Map;
    let by_file = || {
        if map {
            return Verdict::Skip;
        }
        match r.hash.as_deref() {
            Some(h) if h == c.hash => Verdict::Same,
            Some(_) => Verdict::Stale(SOURCE_CHANGED),
            // без хэша: первый присланный хэш — ориентир
            None => Verdict::Ok {
                hash: c.hash,
                symbol_hash: None,
            },
        }
    };
    let Some(qual) = r.symbol.as_deref() else {
        return by_file();
    };
    if c.hash == MISSING_HASH {
        return Verdict::Stale(SYMBOL_REMOVED);
    }
    let Some(symbols) = c.symbols else {
        return by_file();
    };
    match symbols.iter().find(|s| s.qual == qual) {
        Some(s) => match r.symbol_hash.as_deref() {
            Some(h) if h != s.body_hash => Verdict::Stale(SYMBOL_CHANGED),
            _ if map => Verdict::SymbolOk {
                symbol_hash: &s.body_hash,
            },
            // тело то же (или ориентира не было): ссылка принимает и новый хэш файла, иначе
            // следующее сообщение без символов сочло бы её устаревшей по старому хэшу файла
            _ => Verdict::Ok {
                hash: c.hash,
                symbol_hash: Some(&s.body_hash),
            },
        },
        None if c.complete => Verdict::Stale(SYMBOL_REMOVED),
        None => by_file(),
    }
}

/// Применяет правило устарелости к ссылкам решения `solution` на файлы `changes` у владельцев
/// из `spaces` (JSON-массив id) — общая часть [`files_changed`], `code_put_files` и
/// `code_sync_manifest`. Возвращает, сколько ссылок этот вызов признал устаревшими (в том
/// числе уже бывших `stale`).
pub(crate) fn apply_changes(
    tx: &Connection,
    solution: &str,
    spaces: &str,
    changes: &[Change<'_>],
    now: &str,
) -> Result<u32, StoreError> {
    let mut stale = 0u32;
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
        "UPDATE file_refs SET content_hash = ?2,
                symbol_hash = coalesce(?3, symbol_hash), state = 'ok', stale_at = NULL,
                stale_reason = NULL
         WHERE id = ?1",
    )?;
    let mut symbol_ok = tx.prepare_cached(
        "UPDATE file_refs SET symbol_hash = coalesce(symbol_hash, ?2), state = 'ok',
                stale_at = NULL, stale_reason = NULL
         WHERE id = ?1",
    )?;
    for c in changes {
        for r in refs_in_spaces(tx, solution, c.path, spaces)? {
            match verdict(&r, c) {
                Verdict::Same => {
                    if r.state == RefState::Stale {
                        to_ok.execute([r.id])?;
                    }
                }
                Verdict::Ok { hash, symbol_hash } => {
                    adopt.execute(params![r.id, hash, symbol_hash])?;
                }
                Verdict::SymbolOk { symbol_hash } => {
                    // `stale` по хэшу файла снимают только хуки рабочей копии
                    let by_file = r.state == RefState::Stale
                        && r.stale_reason.as_deref() == Some(SOURCE_CHANGED);
                    if !by_file && (r.state == RefState::Stale || r.symbol_hash.is_none()) {
                        symbol_ok.execute(params![r.id, symbol_hash])?;
                    }
                }
                Verdict::Stale(reason) => {
                    to_stale.execute(params![r.id, reason, now])?;
                    stale += 1;
                }
                Verdict::Skip => {}
            }
        }
    }
    Ok(stale)
}

/// Сообщение об изменении файлов решения `solution` (пути нормализованы вызывающим) для
/// ссылок владельцев из `spaces`. Ссылка на файл: хэш другой — `stale`/`source_changed`,
/// совпал — `ok`; у ссылки без хэша присланный хэш становится ориентиром, ссылка остаётся
/// `ok` (решение контроллера, раунд 1 задачи 7 M4.5). Ссылка на символ (M5.6 §8): файл удалён
/// ([`MISSING_HASH`]) — `symbol_removed`; символы присланы — символ найден с тем же телом (или
/// без записанного хэша тела) — `ok` с новыми хэшами, с другим — `symbol_changed`, не найден —
/// `symbol_removed`; символов нет — по хэшу файла. Возвращает, сколько ссылок этих путей
/// после этого `stale`.
pub async fn files_changed(
    db: &Db,
    solution: &str,
    spaces: &[Id],
    files: &[ChangedFile],
    now: Timestamp,
) -> Result<u32, StoreError> {
    let solution = solution.to_string();
    let spaces = ids_json(spaces);
    let files = files.to_vec();
    db.write(move |tx| {
        let changes: Vec<Change<'_>> = files
            .iter()
            .map(|f| Change {
                path: &f.path,
                hash: &f.hash,
                symbols: f.symbols.as_deref(),
                complete: true,
                source: ChangeSource::WorkingCopy,
            })
            .collect();
        apply_changes(tx, &solution, &spaces, &changes, &fmt_ts(now))
    })
    .await
}

/// Правило загрузки карты кода (`PUT /code/files`) для ссылок владельцев из `spaces` другой
/// базы (Knowledge участника): судятся только ссылки на символы и только по присланным
/// символам (`symbols: None` — файл пропущен или разобран не полностью — ссылки не судятся;
/// **при `partial` передавайте `None`**: присланный список считается полным, и не найденный в
/// нём символ стал бы `symbol_removed`, хотя его мог обрезать предел),
/// файловые ссылки и хэш файла не меняются — как у `code_put_files` в базе владельца
/// (решение контроллера по ревью задачи 3, I2). Возвращает, сколько ссылок признано
/// устаревшими.
pub async fn symbols_uploaded(
    db: &Db,
    solution: &str,
    spaces: &[Id],
    files: &[ChangedFile],
    now: Timestamp,
) -> Result<u32, StoreError> {
    let solution = solution.to_string();
    let spaces = ids_json(spaces);
    let files = files.to_vec();
    db.write(move |tx| {
        let changes: Vec<Change<'_>> = files
            .iter()
            .map(|f| Change {
                path: &f.path,
                hash: &f.hash,
                symbols: f.symbols.as_deref(),
                complete: true,
                source: ChangeSource::Map,
            })
            .collect();
        apply_changes(tx, &solution, &spaces, &changes, &fmt_ts(now))
    })
    .await
}

/// Файл `POST /files/verified`: путь (нормализован вызывающим), хэш содержимого и хэши тел
/// символов, если известны.
pub type VerifiedFile = (String, Option<String>, Option<Vec<SymbolHash>>);

/// «Проверено, знание актуально»: ссылки решения на эти пути у владельцев из `spaces` —
/// `ok`, `recorded_at` — сейчас, хэш — присланный (без него — NULL: ориентиром станет хэш
/// следующего сообщения). У символьной ссылки хэш тела — из присланных символов (символа среди
/// них нет — NULL); символов не прислали (файл не разобран, клиент без карты) — хэш тела
/// прежний: иначе подтверждение файла стёрло бы ориентир чужих ссылок на его символы, и правка
/// символа больше не давала бы `symbol_changed` (ревью задачи 6 M5.6, I3).
pub async fn files_verified(
    db: &Db,
    solution: &str,
    spaces: &[Id],
    files: &[VerifiedFile],
    now: Timestamp,
) -> Result<u32, StoreError> {
    let solution = solution.to_string();
    let spaces = ids_json(spaces);
    let files = files.to_vec();
    db.write(move |tx| {
        let now = fmt_ts(now);
        let mut st = tx.prepare_cached(
            "UPDATE file_refs SET state = 'ok', stale_at = NULL, stale_reason = NULL,
                    recorded_at = ?2, content_hash = ?3,
                    symbol_hash = CASE WHEN ?5 THEN ?4 ELSE symbol_hash END
             WHERE id = ?1",
        )?;
        let mut n = 0u32;
        for (path, hash, symbols) in &files {
            for r in refs_in_spaces(tx, &solution, path, &spaces)? {
                let body = r.symbol.as_deref().and_then(|q| {
                    symbols
                        .as_deref()?
                        .iter()
                        .find(|s| s.qual == q)
                        .map(|s| s.body_hash.as_str())
                });
                let judged = symbols.is_some();
                n += st.execute(params![r.id, now, hash, body, judged])? as u32;
            }
        }
        Ok(n)
    })
    .await
}

/// Запись индекса для хуков: ссылка `n:`/`i:`/`c:`/… , вид, заголовок, состояние ссылки и
/// символ, если ссылка на символ (M5.6 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: RefState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Пространство записи-владельца — у [`path_owners`] (карточка символа и влияние, ссылка
    /// WebUI на знание из Knowledge участника); в индексе хуков не отдаётся.
    #[serde(skip)]
    pub space: Option<zam_core::Id>,
}

/// Путь индекса: последний записанный хэш и до [`INDEX_ITEMS_MAX`] записей (ссылки на символы
/// первыми, затем новые первыми).
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

pub(crate) fn ids_json(ids: &[Id]) -> String {
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
/// последний записанный хэш и до [`INDEX_ITEMS_MAX`] живых владельцев (ссылки на символы
/// первыми).
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
                    coalesce(n.name, e.title, ''), e.kind, f.symbol, f.recorded_at
             FROM file_refs f {OWNER_JOIN}
             WHERE f.solution_guid = ?1 AND {LIVE_OWNER}
             ORDER BY f.path, f.symbol IS NULL, f.recorded_at DESC, f.id DESC"
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
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?;
        let mut out: Vec<PathRefs> = Vec::new();
        // время записи хэша пути: «последний записанный» — по `recorded_at`, а не по порядку
        // выдачи (символьные ссылки идут первыми)
        let mut hash_at = String::new();
        for row in rows {
            let (path, hash, state, kind, id, title, ekind, symbol, at) = row?;
            if out.last().is_none_or(|p| p.path != path) {
                out.push(PathRefs {
                    path,
                    hash: None,
                    items: Vec::new(),
                });
                hash_at.clear();
            }
            let cur = out.last_mut().expect("путь только что добавлен");
            if hash.is_some() && (cur.hash.is_none() || at > hash_at) {
                cur.hash = hash;
                hash_at = at;
            }
            if cur.items.len() < INDEX_ITEMS_MAX {
                let (id, kind) = owner_ref(&kind, &id, ekind.as_deref());
                cur.items.push(RefItem {
                    id,
                    kind,
                    title,
                    state,
                    symbol,
                    space: None,
                });
            }
        }
        Ok(out)
    })
    .await
}

/// Живые владельцы ссылок решения `solution` на путь `path` (нормализован вызывающим) в
/// пространствах `spaces` — без предела [`INDEX_ITEMS_MAX`] (карточка символа и влияние правки,
/// M5.6 §8–§9): ссылки на символы первыми, затем новые первыми; владелец с несколькими
/// ссылками пути (файловой и символьными) — по строке на ссылку.
pub async fn path_owners(
    db: &Db,
    solution: &str,
    path: &str,
    spaces: &[Id],
) -> Result<Vec<RefItem>, StoreError> {
    if spaces.is_empty() {
        return Ok(Vec::new());
    }
    let (solution, path) = (solution.to_string(), path.to_string());
    let spaces = ids_json(spaces);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT f.state, f.owner_kind, f.owner_id, coalesce(n.name, e.title, ''), e.kind,
                    f.symbol, coalesce(n.space_id, e.space_id)
             FROM file_refs f {OWNER_JOIN}
             WHERE f.solution_guid = ?1 AND f.path = ?3 AND {LIVE_OWNER}
             ORDER BY f.symbol IS NULL, f.recorded_at DESC, f.id DESC"
        ))?;
        let rows = st.query_map(params![solution, spaces, path], |r| {
            Ok((
                r.get::<_, RefState>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<zam_core::Id>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (state, kind, id, title, ekind, symbol, space) = row?;
            let (id, kind) = owner_ref(&kind, &id, ekind.as_deref());
            out.push(RefItem {
                id,
                kind,
                title,
                state,
                symbol,
                space,
            });
        }
        Ok(out)
    })
    .await
}

/// Живые владельцы ссылок на путь `path` (нормализован вызывающим) в пространствах `spaces`
/// — в любом решении: «что память знает об этом файле». Владельцы ссылок на символы первыми,
/// затем новые первыми. `symbol` задан (`путь#qual`) — только ссылки на этот символ (первыми)
/// и файловые; ссылки на другие символы файла — нет (ревью задачи 6 M5.6, M3).
pub async fn refs_to_path(
    db: &Db,
    path: &str,
    symbol: Option<&str>,
    spaces: &[Id],
) -> Result<Vec<TargetRef>, StoreError> {
    let path = path.to_string();
    let symbol = symbol.map(str::to_string);
    let spaces = ids_json(spaces);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT f.owner_kind, f.owner_id FROM file_refs f {OWNER_JOIN}
             WHERE f.path = ?1 AND {LIVE_OWNER}
               AND (?3 IS NULL OR f.symbol IS NULL OR f.symbol = ?3)
             ORDER BY f.symbol IS NULL, f.recorded_at DESC, f.id DESC"
        ))?;
        let rows = st.query_map(params![path, spaces, symbol], |r| {
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

/// Заметка с `verified_at:` (проверенная команда, M4.5 §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedNote {
    pub id: Id,
    pub path: String,
    pub verified_at: String,
}

/// Живые заметки пространства `space_id` с `verified_at`, новые проверки первыми (затем —
/// свежие правки); не больше `limit`.
pub async fn verified_notes(
    db: &Db,
    space_id: Id,
    limit: usize,
) -> Result<Vec<VerifiedNote>, StoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.read(move |c| {
        let mut st = c.prepare_cached(
            "SELECT id, path, verified_at FROM nodes
             WHERE space_id = ?1 AND kind = 'note' AND deleted_at IS NULL
               AND verified_at IS NOT NULL AND trim(verified_at) <> ''
             ORDER BY verified_at DESC, updated_at DESC, id LIMIT ?2",
        )?;
        let rows = st.query_map(params![space_id, limit], |r| {
            Ok(VerifiedNote {
                id: r.get(0)?,
                path: r.get(1)?,
                verified_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Ссылки `stale` у владельцев `owners` строками `путь` или `путь#qual` (по алфавиту);
/// владельцы без них в ответ не входят.
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
            "SELECT owner_kind, owner_id,
                    CASE WHEN symbol IS NULL THEN path ELSE path || '#' || symbol END
             FROM file_refs
             WHERE state = 'stale'
               AND ((owner_kind = 'node' AND owner_id IN (SELECT value FROM json_each(?1)))
                 OR (owner_kind = 'entry' AND owner_id IN (SELECT value FROM json_each(?2))))
             ORDER BY owner_kind, owner_id, path, coalesce(symbol, '')",
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
/// (и удалённых: восстановление их не пересчитывает) из frontmatter текущей ревизии, затем
/// ссылки changelog из `data.refs.files` (без хэша; решение — пространство решения, у
/// Knowledge — NULL). Пакетами по
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
                    let refs = changelog_refs(&data);
                    if refs.is_empty() {
                        continue;
                    }
                    let owner = TargetRef {
                        kind: TargetKind::Entry,
                        id: *id,
                    };
                    let solution = owner_solution(tx, *space, &a)?;
                    add_refs(tx, owner, solution.as_deref(), &refs, &now)?;
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
            hash: Some("0".repeat(64)),
            ..FileRef::path_only("b.rs".into())
        }];
        let set = entry_set(EntryKind::Changelog, &req, &data);
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].hash.as_deref(), Some("0".repeat(64).as_str()));
        assert_eq!(set[1], FileRef::path_only("a.rs".into()));
        // не changelog — строки data не читаются
        assert_eq!(entry_set(EntryKind::Insight, &req, &data).len(), 1);
    }
}
