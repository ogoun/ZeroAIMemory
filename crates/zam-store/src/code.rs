//! Карта кода решения (спецификация M5.6 §5–§9; миграция `0023_code_map.sql`): снимок, файлы,
//! символы с FTS, агрегированные ссылки и ранги — в базе владельца решения.
//!
//! - **Синхронизация** ([`code_sync_manifest`]): манифест `[(путь, хэш)]` → пути, которых у
//!   карты нет или чей хэш другой (`need`), файлы вне манифеста удаляются (каскадом — символы,
//!   ссылки, строки FTS).
//! - **Загрузка** ([`code_put_files`]): пофайлово «последний побеждает» — upsert строки файла по
//!   `(solution_guid, path)` (id сохраняется), замена символов и ссылок, атрибуция загрузки; затем
//!   правило устарелости ссылок памяти по загруженным символам
//!   ([`crate::file_refs::apply_changes`] — та же функция, что у `POST /files/changed`, но
//!   только для ссылок на символы: файловые ссылки судят хуки рабочей копии). Удаление файла
//!   синхронизацией — `symbol_removed` у ссылок на его символы.
//! - **Ранги** считаются лениво ([`code_rank_inputs`] → `zam_code::rank` → [`code_store_ranks`]);
//!   любое изменение карты увеличивает `gen` снимка и ставит `rank_state = pending`, ранги по
//!   устаревшим входам не сохраняются. Хранятся ранг файла и доли его символов
//!   (`code_files.shares`, f32); ранг символа = ранг файла × доля — при чтении.
//! - **Невидимость:** функции модуля не трогают `chunks`, `index_jobs` и Qdrant; поиск памяти
//!   (`search.rs`) таблиц карты не читает.
//!
//! Пути и значения проверяет вызывающий (сервер, M5.6 §12–§13); сигнатуры приходят уже
//! маскированными.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use zam_code::{
    FileUpload, Lang, ManifestItem, ParseState, RankFile, Ranked, RefAgg, RefKind, SymbolHash,
    SymbolKind, fts_terms, split_ident,
};
use zam_core::Id;

use crate::file_refs::{Change, ChangeSource, apply_changes};
use crate::time::{fmt_ts, parse_ts, ser_ts};
use crate::{Db, StoreError};

/// Символов в выдаче поиска не больше (§9: `limit` ≤ 100).
pub const SYMBOLS_LIMIT_MAX: usize = 100;
/// Пропущенных файлов в [`CodeStatus::skipped`] не больше (полное число — `skipped_total`).
pub const STATUS_SKIPPED_MAX: usize = 500;
/// Файлов-потребителей в карточке символа не больше.
const CALLERS_MAX: i64 = 50;
/// Ссылок файла в карточке символа не больше.
const USES_MAX: i64 = 100;
/// Кандидатов FTS на один проход поиска символов: без сортировки по bm25 по всему совпадению
/// префиксный запрос на 10^6 символов стоил секунды (ревью задачи 3, M5).
const FTS_CANDIDATES_MAX: i64 = 2000;
/// Слово запроса короче — точное слово FTS, не префикс.
const FTS_PREFIX_MIN_CHARS: usize = 2;
/// Относительное изменение ранга файла, ниже которого строка не переписывается.
const RANK_REL_EPS: f64 = 1e-4;
/// Центральных символов из одного файла не больше (§7).
const CENTRAL_PER_FILE: usize = 3;
/// Изменение доли символа, ниже которого доли файла не переписываются.
const SHARE_EPS: f32 = 1e-4;

/// Атрибуция загрузки: коммит, ветка и машина клиента, пользователь запроса, момент.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadMeta {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub machine: Option<String>,
    pub user_id: Id,
    pub now: Timestamp,
}

/// Итог `POST /code/sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncOutcome {
    /// Пути манифеста без строки карты или с другим хэшем — в порядке манифеста, без повторов.
    pub need: Vec<String>,
    /// Удалено файлов карты, которых нет в манифесте.
    pub removed: u64,
    pub etag: String,
    /// Ссылок на символы удалённых файлов признано устаревшими (`symbol_removed`).
    pub stale_refs: u32,
}

/// Итог `PUT /code/files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PutOutcome {
    /// Принято файлов (повтор пути в одной загрузке — каждый раз).
    pub accepted: u32,
    /// Ссылок памяти на загруженные пути после правила устарелости — `stale`.
    pub stale_refs: u32,
}

/// Состояние рангов снимка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RankState {
    Ok,
    Pending,
}

impl RankState {
    pub fn as_str(self) -> &'static str {
        match self {
            RankState::Ok => "ok",
            RankState::Pending => "pending",
        }
    }
}

impl ToSql for RankState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for RankState {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "ok" => Ok(RankState::Ok),
            "pending" => Ok(RankState::Pending),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестное состояние рангов: {other}").into(),
            )),
        }
    }
}

/// Снимок карты решения; `commit`, `branch`, `machine` — преобладающие среди файлов.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeSnapshot {
    pub solution: String,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub machine: Option<String>,
    #[serde(serialize_with = "ser_ts")]
    pub updated_at: Timestamp,
    pub files: u64,
    pub symbols: u64,
    pub etag: String,
    pub rank_state: RankState,
    #[serde(serialize_with = "crate::time::ser_ts_opt")]
    pub ranked_at: Option<Timestamp>,
}

/// Файлы и символы одного языка.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LangCount {
    pub lang: Lang,
    pub files: u64,
    pub symbols: u64,
}

/// Состояние карты (`GET /code/status`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeStatus {
    pub snapshot: CodeSnapshot,
    /// Языки по числу файлов (затем символов), убыванию.
    pub by_lang: Vec<LangCount>,
    /// `(путь, причина)` пропущенных и неполно разобранных файлов по алфавиту пути, до
    /// [`STATUS_SKIPPED_MAX`].
    pub skipped: Vec<(String, String)>,
    pub skipped_total: u64,
    /// Доля файлов не из преобладающего снимка `(commit, branch, machine)`, 0..=1.
    pub other_share: f64,
}

/// Входы ранжирования и поколение карты, по которому они прочитаны ([`code_store_ranks`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RankInputs {
    /// Файлы по `id`; `defs` — символы файла по `id`, `refs` — строки `code_refs`
    /// `(name, kind, count, recv_count)` (порядок не задан).
    pub files: Vec<RankFile>,
    pub generation: i64,
}

impl RankInputs {
    /// Оценка памяти входов, байт: ёмкости векторов и строк и накладные распределителя
    /// [`ALLOC_OVERHEAD`] на каждый блок кучи (строка, вектор) — предел кэша входов сервера
    /// (ревью задачи 4, O2; задачи 9A, M5).
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        let def = size_of::<(String, SymbolKind, Option<bool>)>();
        let rf = size_of::<(String, zam_code::RefKind, u32, u32)>();
        let s = |x: &String| x.capacity() + if x.capacity() > 0 { ALLOC_OVERHEAD } else { 0 };
        let v = |cap: usize, item: usize| cap * item + if cap > 0 { ALLOC_OVERHEAD } else { 0 };
        v(self.files.capacity(), size_of::<RankFile>())
            + self
                .files
                .iter()
                .map(|f| {
                    s(&f.path)
                        + v(f.defs.capacity(), def)
                        + f.defs.iter().map(|d| s(&d.0)).sum::<usize>()
                        + v(f.refs.capacity(), rf)
                        + f.refs.iter().map(|r| s(&r.0)).sum::<usize>()
                })
                .sum::<usize>()
    }
}

/// Накладные распределителя на блок кучи, байт (заголовок и выравнивание malloc/HeapAlloc —
/// 16–32 байта).
pub const ALLOC_OVERHEAD: usize = 24;

/// Символ строки карты; `share` и `rank` — из сохранённых долей файла.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapSymRow {
    pub id: i64,
    pub name: String,
    pub qual: String,
    pub kind: SymbolKind,
    pub sig: Option<String>,
    pub line: u32,
    /// Доля ранга файла (`zam_code::Ranked::symbol_share`); долей нет — поровну.
    pub share: f64,
    /// Ранг файла × доля.
    pub rank: f64,
}

/// Файл карты; `symbols` — по рангу символа ([`code_map_symbols`]; [`code_map_files`] их не
/// заполняет).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapFileRow {
    pub id: i64,
    pub path: String,
    pub lang: Lang,
    pub rank: f64,
    pub symbol_count: u32,
    pub symbols: Vec<MapSymRow>,
}

/// Найденный символ.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SymbolHit {
    pub id: i64,
    pub file_id: i64,
    pub path: String,
    pub name: String,
    pub qual: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub end_line: u32,
    pub sig: Option<String>,
    pub rank: f64,
}

/// Файл, ссылающийся на имя символа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Caller {
    pub file_id: i64,
    pub path: String,
    pub count: u32,
}

/// Карточка символа (`GET /code/symbols/{id}`); знания добавляет сервер (`refs_to_path`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SymbolCard {
    pub symbol: SymbolHit,
    pub body_hash: String,
    pub exported: Option<bool>,
    /// Другие файлы со ссылками на имя символа — по числу ссылок, до 50.
    pub callers: Vec<Caller>,
    /// Ссылки файла символа (агрегированы по файлу, не по символу) — по числу, до 100.
    pub uses: Vec<RefAgg>,
}

/// Символ в ответе влияния.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactSym {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub qual: String,
    pub kind: SymbolKind,
    pub line: u32,
}

/// Файл-потребитель изменённых или удалённых символов (1 шаг по `code_refs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Consumer {
    pub file_id: i64,
    pub path: String,
    /// Имена изменённых символов, на которые он ссылается, по алфавиту.
    pub names: Vec<String>,
    pub count: u32,
}

/// Итог `POST /code/impact` (без знаний — их добавляет сервер).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactRows {
    /// Символы карты с другим `body_hash`.
    pub changed: Vec<ImpactSym>,
    /// Символы карты, которых нет среди присланных.
    pub removed: Vec<ImpactSym>,
    /// Другие файлы со ссылками на имена изменённых и удалённых — по числу ссылок.
    pub consumers: Vec<Consumer>,
    /// Пути без присланных символов — знания по путям.
    pub paths_only: Vec<String>,
}

/// Доли символов файла → BLOB f32 little-endian (порядок `code_symbols.id`).
fn shares_blob(shares: &[f64]) -> Vec<u8> {
    shares
        .iter()
        .flat_map(|&v| (if v.is_finite() { v as f32 } else { 0.0 }).to_le_bytes())
        .collect()
}

/// BLOB долей → значения; длина не кратна 4 — пусто.
fn shares_of(blob: &[u8]) -> Vec<f32> {
    let (chunks, rest) = blob.as_chunks::<4>();
    if !rest.is_empty() {
        return Vec::new();
    }
    chunks.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// Доля символа `idx` из `n` символов файла: сохранённая, если долей ровно `n`, иначе поровну
/// (файл перезагружен после ранжирования).
fn share_at(blob: Option<&[u8]>, idx: usize, n: usize) -> f64 {
    let saved = blob.map(shares_of).unwrap_or_default();
    if saved.len() == n && idx < n {
        f64::from(saved[idx])
    } else if n > 0 {
        1.0 / n as f64
    } else {
        0.0
    }
}

fn corrupt(what: &str, v: &str) -> rusqlite::Error {
    crate::time::conv_err(0, StoreError::Corrupt(format!("{what}: «{v}»")))
}

fn lang_col(r: &Row<'_>, i: usize) -> rusqlite::Result<Lang> {
    let s: String = r.get(i)?;
    Lang::parse(&s).ok_or_else(|| corrupt("язык файла карты", &s))
}

fn kind_col(r: &Row<'_>, i: usize) -> rusqlite::Result<SymbolKind> {
    let s: String = r.get(i)?;
    SymbolKind::parse(&s).ok_or_else(|| corrupt("вид символа", &s))
}

fn ts_col(r: &Row<'_>, i: usize) -> rusqlite::Result<Timestamp> {
    let s: String = r.get(i)?;
    parse_ts(&s).map_err(|e| crate::time::conv_err(i, e))
}

/// Токен решения в `code_symbols_fts.sol`: `s` + 16 hex blake3 GUID — одно слово для
/// токенизатора `unicode61` при любом GUID.
fn sol_token(solution: &str) -> String {
    format!("s{}", &blake3::hash(solution.as_bytes()).to_hex()[..16])
}

/// `etag` снимка: blake3 `(files, symbols, updated_at, gen)`, 32 hex.
fn etag_of(files: i64, symbols: i64, updated_at: &str, generation: i64) -> String {
    let h = blake3::hash(format!("{files}\n{symbols}\n{updated_at}\n{generation}").as_bytes());
    h.to_hex()[..32].to_string()
}

/// Снимок решения есть (создаётся пустым, `pending`).
fn ensure_snapshot(tx: &Connection, solution: &str, now: &str) -> Result<(), StoreError> {
    tx.prepare_cached(
        "INSERT INTO code_snapshots (solution_guid, updated_at, etag) VALUES (?1, ?2, ?3)
         ON CONFLICT (solution_guid) DO NOTHING",
    )?
    .execute(params![solution, now, etag_of(0, 0, now, 0)])?;
    Ok(())
}

/// Преобладающий `(commit, branch, machine)` среди файлов решения и число его файлов.
type Dominant = (Option<String>, Option<String>, Option<String>, i64);

fn dominant(c: &Connection, solution: &str) -> Result<Option<Dominant>, StoreError> {
    Ok(c.prepare_cached(
        "SELECT git_commit, branch, machine, count(*) AS n FROM code_files
         WHERE solution_guid = ?1
         GROUP BY git_commit, branch, machine ORDER BY n DESC, max(updated_at) DESC LIMIT 1",
    )?
    .query_row([solution], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()?)
}

/// Карта изменилась: счётчики, преобладающий снимок, `gen + 1`, `etag`, `rank_state = pending`.
fn touch_snapshot(tx: &Connection, solution: &str, now: &str) -> Result<(), StoreError> {
    let (files, symbols): (i64, i64) = tx
        .prepare_cached(
            "SELECT count(*), coalesce(sum(symbols), 0) FROM code_files WHERE solution_guid = ?1",
        )?
        .query_row([solution], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (commit, branch, machine, _) = dominant(tx, solution)?.unwrap_or((None, None, None, 0));
    let generation: i64 = tx
        .prepare_cached("SELECT gen + 1 FROM code_snapshots WHERE solution_guid = ?1")?
        .query_row([solution], |r| r.get(0))?;
    tx.prepare_cached(
        "UPDATE code_snapshots SET git_commit = ?2, branch = ?3, machine = ?4, updated_at = ?5,
                files = ?6, symbols = ?7, gen = ?8, etag = ?9, rank_state = 'pending'
         WHERE solution_guid = ?1",
    )?
    .execute(params![
        solution,
        commit,
        branch,
        machine,
        now,
        files,
        symbols,
        generation,
        etag_of(files, symbols, now, generation)
    ])?;
    Ok(())
}

/// Синхронизация по манифесту (одна транзакция): удаляет файлы карты вне манифеста, отдаёт
/// пути без строки или с другим хэшем. Повтор пути в манифесте — первый (сервер отвечает 422
/// раньше, M5.6 Review Focus 5). Ссылки на символы удалённых файлов у владельцев пространств
/// `ref_spaces` этой базы — `stale`/`symbol_removed` (файл удалён, переименован или исключён
/// `code.exclude`); файловые ссылки не трогаются.
pub async fn code_sync_manifest(
    db: &Db,
    solution: &str,
    meta: &UploadMeta,
    manifest: &[ManifestItem],
    ref_spaces: &[Id],
) -> Result<SyncOutcome, StoreError> {
    let solution = solution.to_string();
    let now = fmt_ts(meta.now);
    let manifest = manifest.to_vec();
    let spaces = (!ref_spaces.is_empty()).then(|| crate::file_refs::ids_json(ref_spaces));
    db.write(move |tx| {
        ensure_snapshot(tx, &solution, &now)?;
        let have: HashMap<String, (i64, String)> = tx
            .prepare_cached(
                "SELECT path, id, content_hash FROM code_files WHERE solution_guid = ?1",
            )?
            .query_map([&solution], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?;
        let mut seen: HashSet<&str> = HashSet::with_capacity(manifest.len());
        let mut need = Vec::new();
        for m in &manifest {
            if !seen.insert(&m.path) {
                continue;
            }
            if have.get(&m.path).is_none_or(|(_, h)| *h != m.hash) {
                need.push(m.path.clone());
            }
        }
        let mut gone: Vec<&str> = Vec::new();
        {
            let mut del = tx.prepare_cached("DELETE FROM code_files WHERE id = ?1")?;
            for (path, (id, _)) in &have {
                if !seen.contains(path.as_str()) && del.execute([id])? > 0 {
                    gone.push(path);
                }
            }
        }
        let mut stale_refs = 0;
        if !gone.is_empty() {
            touch_snapshot(tx, &solution, &now)?;
            if let Some(spaces) = &spaces {
                let changes: Vec<Change<'_>> = gone
                    .iter()
                    .map(|p| Change {
                        path: p,
                        hash: zam_code::MISSING_HASH,
                        symbols: None,
                        complete: true,
                        source: ChangeSource::Map,
                    })
                    .collect();
                stale_refs = apply_changes(tx, &solution, spaces, &changes, &now)?;
            }
        }
        let etag: String = tx
            .prepare_cached("SELECT etag FROM code_snapshots WHERE solution_guid = ?1")?
            .query_row([&solution], |r| r.get(0))?;
        Ok(SyncOutcome {
            need,
            removed: gone.len() as u64,
            etag,
            stale_refs,
        })
    })
    .await
}

/// Строка файла: upsert по `(solution_guid, path)` с атрибуцией; символы, строки FTS и ссылки
/// файла заменяются. Возвращает id файла.
fn put_file(
    tx: &Connection,
    solution: &str,
    meta: &UploadMeta,
    now: &str,
    f: &FileUpload,
) -> Result<i64, StoreError> {
    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO code_files (solution_guid, path, lang, content_hash, git_oid, size,
                                     parse_state, skip_reason, symbols, git_commit, branch,
                                     machine, user_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (solution_guid, path) DO UPDATE SET
               lang = excluded.lang, content_hash = excluded.content_hash,
               git_oid = excluded.git_oid, size = excluded.size,
               parse_state = excluded.parse_state, skip_reason = excluded.skip_reason,
               symbols = excluded.symbols, git_commit = excluded.git_commit,
               branch = excluded.branch, machine = excluded.machine,
               user_id = excluded.user_id, updated_at = excluded.updated_at, shares = NULL
             RETURNING id",
        )?
        .query_row(
            params![
                solution,
                f.path,
                f.lang.as_str(),
                f.hash,
                f.git_oid,
                i64::try_from(f.size).unwrap_or(i64::MAX),
                f.parse_state.as_str(),
                f.skip_reason.map(|r| r.as_str()),
                f.symbols.len() as i64,
                meta.commit,
                meta.branch,
                meta.machine,
                meta.user_id,
                now
            ],
            |r| r.get(0),
        )?;
    // строки FTS символов удаляет триггер `code_symbols_fts_delete`
    tx.prepare_cached("DELETE FROM code_symbols WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM code_refs WHERE file_id = ?1")?
        .execute([id])?;
    let mut sym = tx.prepare_cached(
        "INSERT INTO code_symbols (file_id, name, qual, kind, line, end_line, sig, body_hash,
                                   exported)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING id",
    )?;
    let mut fts = tx.prepare_cached(
        "INSERT INTO code_symbols_fts (rowid, name, qual, terms, sol, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let sol = sol_token(solution);
    for s in &f.symbols {
        let sid: i64 = sym.query_row(
            params![
                id,
                s.name,
                s.qual,
                s.kind.as_str(),
                s.line,
                s.end_line,
                s.sig,
                s.body_hash,
                s.exported
            ],
            |r| r.get(0),
        )?;
        fts.execute(params![
            sid,
            s.name,
            s.qual,
            fts_terms(&s.name, &s.qual),
            sol,
            s.kind.as_str()
        ])?;
    }
    // повтор `(name, kind)` в загрузке складывается; `recv_count` (0025, M6 §12а) — сколько из
    // `count` вызовы по получателю: нет поля (клиент до M6) — 0, то есть все вызовы прямые
    let mut rf = tx.prepare_cached(
        "INSERT INTO code_refs (file_id, name, kind, count, first_line, recv_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (file_id, name, kind) DO UPDATE SET
           count = count + excluded.count, first_line = min(first_line, excluded.first_line),
           recv_count = recv_count + excluded.recv_count",
    )?;
    for r in &f.refs {
        let recv = r.recv_count.unwrap_or(0).min(r.count);
        rf.execute(params![
            id,
            r.name,
            r.kind.as_str(),
            r.count,
            r.first_line,
            recv
        ])?;
    }
    Ok(id)
}

/// Загрузка разобранных файлов (одна транзакция на вызов, до 500 файлов): строки файлов,
/// символы, ссылки, атрибуция; затем правило устарелости ссылок памяти на символы пространств
/// `ref_spaces` этой базы по загруженным символам. Файловые ссылки и хэш файла у ссылок не
/// меняются (их судят хуки рабочей копии: у другой машины другой перевод строк); пропущенный
/// файл (`skipped`) и не найденный символ неполного (`partial`) — ссылку не судят. Снимок:
/// счётчики, преобладающий коммит/ветка/машина, `rank_state = pending`.
pub async fn code_put_files(
    db: &Db,
    solution: &str,
    meta: &UploadMeta,
    files: Vec<FileUpload>,
    ref_spaces: &[Id],
) -> Result<PutOutcome, StoreError> {
    let solution = solution.to_string();
    let meta = meta.clone();
    let spaces = (!ref_spaces.is_empty()).then(|| crate::file_refs::ids_json(ref_spaces));
    db.write(move |tx| {
        let now = fmt_ts(meta.now);
        ensure_snapshot(tx, &solution, &now)?;
        for f in &files {
            put_file(tx, &solution, &meta, &now, f)?;
        }
        let mut stale_refs = 0;
        if let Some(spaces) = &spaces {
            // повтор пути в загрузке — последний (как и строка файла)
            let mut last: HashMap<&str, usize> = HashMap::new();
            for (i, f) in files.iter().enumerate() {
                last.insert(&f.path, i);
            }
            let hashes: Vec<Option<Vec<SymbolHash>>> = files
                .iter()
                .map(|f| {
                    (f.parse_state != ParseState::Skipped).then(|| {
                        f.symbols
                            .iter()
                            .map(|s| SymbolHash {
                                qual: s.qual.clone(),
                                body_hash: s.body_hash.clone(),
                            })
                            .collect()
                    })
                })
                .collect();
            let changes: Vec<Change<'_>> = files
                .iter()
                .enumerate()
                .filter(|(i, f)| last.get(f.path.as_str()) == Some(i))
                .map(|(i, f)| Change {
                    path: &f.path,
                    hash: &f.hash,
                    symbols: hashes[i].as_deref(),
                    complete: f.parse_state == ParseState::Ok,
                    source: ChangeSource::Map,
                })
                .collect();
            stale_refs = apply_changes(tx, &solution, spaces, &changes, &now)?;
        }
        touch_snapshot(tx, &solution, &now)?;
        Ok(PutOutcome {
            accepted: files.len() as u32,
            stale_refs,
        })
    })
    .await
}

fn snapshot_of(c: &Connection, solution: &str) -> Result<Option<CodeSnapshot>, StoreError> {
    Ok(c.prepare_cached(
        "SELECT solution_guid, git_commit, branch, machine, updated_at, files, symbols, etag,
                rank_state, ranked_at
         FROM code_snapshots WHERE solution_guid = ?1",
    )?
    .query_row([solution], |r| {
        Ok(CodeSnapshot {
            solution: r.get(0)?,
            commit: r.get(1)?,
            branch: r.get(2)?,
            machine: r.get(3)?,
            updated_at: ts_col(r, 4)?,
            files: r.get::<_, i64>(5)? as u64,
            symbols: r.get::<_, i64>(6)? as u64,
            etag: r.get(7)?,
            rank_state: r.get(8)?,
            ranked_at: r
                .get::<_, Option<String>>(9)?
                .map(|s| parse_ts(&s).map_err(|e| crate::time::conv_err(9, e)))
                .transpose()?,
        })
    })
    .optional()?)
}

/// Строка снимка решения без подсчётов по файлам (лёгкое чтение `rank_state`, `etag`); снимка
/// нет — `None` (очищенная карта снимок сохраняет: `files = 0`).
pub async fn code_snapshot(db: &Db, solution: &str) -> Result<Option<CodeSnapshot>, StoreError> {
    let solution = solution.to_string();
    db.read(move |c| snapshot_of(c, &solution)).await
}

/// Состояние карты решения; карты нет (или в ней нет файлов: очищена, ещё не загружена) —
/// `None`.
pub async fn code_status(db: &Db, solution: &str) -> Result<Option<CodeStatus>, StoreError> {
    let solution = solution.to_string();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let Some(snapshot) = snapshot_of(&tx, &solution)?.filter(|s| s.files > 0) else {
            return Ok(None);
        };
        let by_lang: Vec<LangCount> = tx
            .prepare_cached(
                "SELECT lang, count(*) AS n, coalesce(sum(symbols), 0) AS s FROM code_files
                 WHERE solution_guid = ?1 GROUP BY lang ORDER BY n DESC, s DESC, lang",
            )?
            .query_map([&solution], |r| {
                Ok(LangCount {
                    lang: lang_col(r, 0)?,
                    files: r.get::<_, i64>(1)? as u64,
                    symbols: r.get::<_, i64>(2)? as u64,
                })
            })?
            .collect::<Result<_, _>>()?;
        let skipped: Vec<(String, String)> = tx
            .prepare_cached(
                "SELECT path, skip_reason FROM code_files
                 WHERE solution_guid = ?1 AND parse_state <> 'ok' AND skip_reason IS NOT NULL
                 ORDER BY path LIMIT ?2",
            )?
            .query_map(params![solution, STATUS_SKIPPED_MAX as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let skipped_total: i64 = tx
            .prepare_cached(
                "SELECT count(*) FROM code_files
                 WHERE solution_guid = ?1 AND parse_state <> 'ok' AND skip_reason IS NOT NULL",
            )?
            .query_row([&solution], |r| r.get(0))?;
        let other_share = match (dominant(&tx, &solution)?, snapshot.files) {
            (Some((_, _, _, n)), files) if files > 0 => 1.0 - n as f64 / files as f64,
            _ => 0.0,
        };
        Ok(Some(CodeStatus {
            snapshot,
            by_lang,
            skipped,
            skipped_total: skipped_total as u64,
            other_share,
        }))
    })
    .await
}

/// Входы ранжирования (один снимок чтения) и поколение карты.
pub async fn code_rank_inputs(db: &Db, solution: &str) -> Result<RankInputs, StoreError> {
    let solution = solution.to_string();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let generation: i64 = tx
            .prepare_cached("SELECT gen FROM code_snapshots WHERE solution_guid = ?1")?
            .query_row([&solution], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        let mut files: Vec<RankFile> = tx
            .prepare_cached("SELECT id, path FROM code_files WHERE solution_guid = ?1 ORDER BY id")?
            .query_map([&solution], |r| {
                Ok(RankFile {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    defs: Vec::new(),
                    refs: Vec::new(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let at: HashMap<i64, usize> = files.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
        // без ORDER BY: временное B-дерево на 10^6 строк стоило ~0,2 с; порядок `id` внутри
        // файла восстанавливается ниже (обычно его уже даёт индекс `code_symbols_file`)
        let mut st = tx.prepare_cached(
            "SELECT s.file_id, s.id, s.name, s.kind, s.exported FROM code_files f
             JOIN code_symbols s ON s.file_id = f.id WHERE f.solution_guid = ?1",
        )?;
        let rows = st.query_map([&solution], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                kind_col(r, 3)?,
                r.get::<_, Option<bool>>(4)?,
            ))
        })?;
        let mut def_ids: Vec<Vec<i64>> = vec![Vec::new(); files.len()];
        for row in rows {
            let (fid, sid, name, kind, exported) = row?;
            if let Some(&i) = at.get(&fid) {
                files[i].defs.push((name, kind, exported));
                def_ids[i].push(sid);
            }
        }
        for (f, ids) in files.iter_mut().zip(&def_ids) {
            if !ids.is_sorted() {
                let mut pairs: Vec<_> = ids.iter().copied().zip(f.defs.drain(..)).collect();
                pairs.sort_by_key(|p| p.0);
                f.defs = pairs.into_iter().map(|p| p.1).collect();
            }
        }
        let mut st = tx.prepare_cached(
            "SELECT r.file_id, r.name, r.kind, r.count, r.recv_count FROM code_files f
             JOIN code_refs r ON r.file_id = f.id WHERE f.solution_guid = ?1",
        )?;
        let rows = st.query_map([&solution], |r| {
            let k: String = r.get(2)?;
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                RefKind::parse(&k).ok_or_else(|| corrupt("вид ссылки", &k))?,
                r.get::<_, u32>(3)?,
                r.get::<_, u32>(4)?,
            ))
        })?;
        for row in rows {
            let (fid, name, kind, n, recv) = row?;
            if let Some(&i) = at.get(&fid) {
                files[i].refs.push((name, kind, n, recv));
            }
        }
        Ok(RankInputs { files, generation })
    })
    .await
}

/// Сохраняет ранги файлов и доли символов (`code_files.rank`, `code_files.shares`),
/// `rank_state = ok`, `ranked_at = now`. Пишутся только файлы, у которых ранг изменился больше
/// чем на [`RANK_REL_EPS`] (относительно) или доля — больше чем на [`SHARE_EPS`]: повторное
/// ранжирование почти без изменений не переписывает таблицу под замком писателя. Карта
/// изменилась после [`code_rank_inputs`] (другое поколение) — ничего не пишет: `None` (ранги
/// посчитаются при следующем запросе); иначе `Some(файлов переписано)`. Единственное место,
/// где хранилище читает [`Ranked`].
pub async fn code_store_ranks(
    db: &Db,
    solution: &str,
    inputs: &RankInputs,
    r: &Ranked,
    now: Timestamp,
) -> Result<Option<u64>, StoreError> {
    if r.file_rank.len() != inputs.files.len() || r.symbol_share.len() != inputs.files.len() {
        return Err(StoreError::BadInput(
            "ранги не соответствуют входам ранжирования".into(),
        ));
    }
    let solution = solution.to_string();
    let generation = inputs.generation;
    let rows: Vec<(i64, f64, Vec<f64>)> = inputs
        .files
        .iter()
        .zip(&r.file_rank)
        .zip(&r.symbol_share)
        .map(|((f, &rank), share)| {
            (
                f.id,
                if rank.is_finite() { rank } else { 0.0 },
                share.clone(),
            )
        })
        .collect();
    db.write(move |tx| {
        let cur: Option<i64> = tx
            .prepare_cached("SELECT gen FROM code_snapshots WHERE solution_guid = ?1")?
            .query_row([&solution], |r| r.get(0))
            .optional()?;
        if cur != Some(generation) {
            return Ok(None);
        }
        let old: HashMap<i64, (f64, Option<Vec<u8>>)> = tx
            .prepare_cached("SELECT id, rank, shares FROM code_files WHERE solution_guid = ?1")?
            .query_map([&solution], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?;
        let mut upd =
            tx.prepare_cached("UPDATE code_files SET rank = ?2, shares = ?3 WHERE id = ?1")?;
        let mut written = 0u64;
        for (fid, rank, share) in &rows {
            let changed = match old.get(fid) {
                None => false, // файла уже нет — поколение это исключает, но не пишем
                Some((old_rank, old_shares)) => {
                    let rank_moved =
                        (rank - old_rank).abs() > RANK_REL_EPS * rank.abs().max(old_rank.abs());
                    let saved = old_shares.as_deref().map(shares_of);
                    let shares_moved = match saved {
                        Some(v) if v.len() == share.len() => v
                            .iter()
                            .zip(share)
                            .any(|(a, b)| (a - *b as f32).abs() > SHARE_EPS),
                        _ => true,
                    };
                    rank_moved || shares_moved
                }
            };
            if changed {
                upd.execute(params![fid, rank, shares_blob(share)])?;
                written += 1;
            }
        }
        tx.prepare_cached(
            "UPDATE code_snapshots SET rank_state = 'ok', ranked_at = ?2 WHERE solution_guid = ?1",
        )?
        .execute(params![solution, fmt_ts(now)])?;
        Ok(Some(written))
    })
    .await
}

/// Условие «путь `?2` — сам файл или каталог над ним» (`?2` NULL — без условия) для `f.path`.
const AREA: &str = "(?2 IS NULL OR f.path = ?2 OR substr(f.path, 1, length(?2) + 1) = ?2 || '/')";

/// `area` запроса: без краевых пробелов и `/`; пусто — нет.
fn area_of(area: Option<&str>) -> Option<String> {
    area.map(|a| a.trim().trim_end_matches('/').to_string())
        .filter(|a| !a.is_empty())
}

/// Файлы карты (с префиксом пути `area`: файл или каталог) по сохранённому рангу, без символов
/// (первая фаза карты: сервер отбирает файлы под бюджет, затем [`code_map_symbols`]).
pub async fn code_map_files(
    db: &Db,
    solution: &str,
    area: Option<&str>,
) -> Result<Vec<MapFileRow>, StoreError> {
    let solution = solution.to_string();
    let area = area_of(area);
    db.read(move |c| {
        Ok(c.prepare_cached(&format!(
            "SELECT f.id, f.path, f.lang, f.rank, f.symbols FROM code_files f
             WHERE f.solution_guid = ?1 AND {AREA} ORDER BY f.rank DESC, f.path"
        ))?
        .query_map(params![solution, area], |r| {
            Ok(MapFileRow {
                id: r.get(0)?,
                path: r.get(1)?,
                lang: lang_col(r, 2)?,
                rank: r.get(3)?,
                symbol_count: r.get(4)?,
                symbols: Vec::new(),
            })
        })?
        .collect::<Result<_, _>>()?)
    })
    .await
}

/// Страница таблицы файлов (`GET /code/files`): файлы языка `lang` (нет — все), путь которых
/// содержит `needle` без учёта регистра (Unicode, `needle` — уже в нижнем регистре), по рангу —
/// `(страница, всего подходящих)`. Язык, порядок и без `needle` страница с числом — в SQL; с
/// `needle` строки идут потоком, в памяти остаётся только страница (ревью задачи 4, O4:
/// `lower()` SQLite — только ASCII, а пути бывают кириллицей).
pub async fn code_files_page(
    db: &Db,
    solution: &str,
    lang: Option<Lang>,
    needle: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<(Vec<MapFileRow>, usize), StoreError> {
    let solution = solution.to_string();
    let lang = lang.map(|l| l.as_str().to_string());
    let needle = needle.map(str::to_string);
    db.read(move |c| {
        let row = |r: &Row<'_>| {
            Ok(MapFileRow {
                id: r.get(0)?,
                path: r.get(1)?,
                lang: lang_col(r, 2)?,
                rank: r.get(3)?,
                symbol_count: r.get(4)?,
                symbols: Vec::new(),
            })
        };
        const WHERE: &str = "f.solution_guid = ?1 AND (?2 IS NULL OR f.lang = ?2)";
        let Some(needle) = needle else {
            let total: i64 = c
                .prepare_cached(&format!("SELECT count(*) FROM code_files f WHERE {WHERE}"))?
                .query_row(params![solution, lang], |r| r.get(0))?;
            let page = c
                .prepare_cached(&format!(
                    "SELECT f.id, f.path, f.lang, f.rank, f.symbols FROM code_files f
                     WHERE {WHERE} ORDER BY f.rank DESC, f.path LIMIT ?3 OFFSET ?4"
                ))?
                .query_map(
                    params![
                        solution,
                        lang,
                        i64::try_from(limit).unwrap_or(i64::MAX),
                        i64::try_from(offset).unwrap_or(i64::MAX)
                    ],
                    row,
                )?
                .collect::<Result<_, _>>()?;
            return Ok((page, usize::try_from(total).unwrap_or(0)));
        };
        let mut st = c.prepare_cached(&format!(
            "SELECT f.id, f.path, f.lang, f.rank, f.symbols FROM code_files f
             WHERE {WHERE} ORDER BY f.rank DESC, f.path"
        ))?;
        let mut rows = st.query(params![solution, lang])?;
        let (mut page, mut total) = (Vec::new(), 0usize);
        while let Some(r) = rows.next()? {
            let path: String = r.get(1)?;
            if !path.to_lowercase().contains(&needle) {
                continue;
            }
            if total >= offset && page.len() < limit {
                page.push(row(r)?);
            }
            total += 1;
        }
        Ok((page, total))
    })
    .await
}

/// Символы файлов `file_ids` решения по рангу символа (затем строке) — в порядке `file_ids`;
/// файл другого решения или несуществующий — пропускается.
pub async fn code_map_symbols(
    db: &Db,
    solution: &str,
    file_ids: &[i64],
) -> Result<Vec<(i64, Vec<MapSymRow>)>, StoreError> {
    let solution = solution.to_string();
    let ids = file_ids.to_vec();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let mut file = tx.prepare_cached(
            "SELECT rank, shares FROM code_files WHERE id = ?1 AND solution_guid = ?2",
        )?;
        let mut syms = tx.prepare_cached(
            "SELECT id, name, qual, kind, sig, line FROM code_symbols WHERE file_id = ?1
             ORDER BY id",
        )?;
        let mut out = Vec::with_capacity(ids.len());
        for fid in ids {
            let Some((rank, shares)) = file
                .query_row(params![fid, solution], |r| {
                    Ok((r.get::<_, f64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
                })
                .optional()?
            else {
                continue;
            };
            let mut list: Vec<MapSymRow> = syms
                .query_map([fid], |r| {
                    Ok(MapSymRow {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        qual: r.get(2)?,
                        kind: kind_col(r, 3)?,
                        sig: r.get(4)?,
                        line: r.get(5)?,
                        share: 0.0,
                        rank: 0.0,
                    })
                })?
                .collect::<Result<_, _>>()?;
            let n = list.len();
            for (i, s) in list.iter_mut().enumerate() {
                s.share = share_at(shares.as_deref(), i, n);
                s.rank = rank * s.share;
            }
            list.sort_by(|a, b| {
                b.rank
                    .total_cmp(&a.rank)
                    .then(a.line.cmp(&b.line))
                    .then(a.id.cmp(&b.id))
            });
            out.push((fid, list));
        }
        Ok(out)
    })
    .await
}

/// Файлы карты с символами ([`code_map_files`] + [`code_map_symbols`] всех файлов) — для
/// небольших областей; полную карту сервер собирает в две фазы.
pub async fn code_map_rows(
    db: &Db,
    solution: &str,
    area: Option<&str>,
) -> Result<Vec<MapFileRow>, StoreError> {
    let mut files = code_map_files(db, solution, area).await?;
    let ids: Vec<i64> = files.iter().map(|f| f.id).collect();
    let mut syms: HashMap<i64, Vec<MapSymRow>> = code_map_symbols(db, solution, &ids)
        .await?
        .into_iter()
        .collect();
    for f in &mut files {
        f.symbols = syms.remove(&f.id).unwrap_or_default();
    }
    Ok(files)
}

/// Центральные символы решения (§7) без чтения всей карты: первые `n` по рангу символа
/// (ранг файла × доля), не больше трёх из одного файла; при равенстве — по порядку файла и
/// месту символа. Файлы перебираются по убыванию ранга, не больше `files_max`; перебор
/// останавливается, как только ранг файла не выше `n`-го найденного (ранг символа не выше
/// ранга его файла).
pub async fn code_central(
    db: &Db,
    solution: &str,
    n: usize,
    files_max: usize,
) -> Result<Vec<SymbolHit>, StoreError> {
    if n == 0 || files_max == 0 {
        return Ok(Vec::new());
    }
    let solution = solution.to_string();
    let files_max = i64::try_from(files_max).unwrap_or(i64::MAX);
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        // (ранг символа, порядок файла, индекс символа в файле, id файла)
        let mut best: Vec<(f64, usize, usize, i64)> = Vec::new();
        {
            let mut files = tx.prepare_cached(
                "SELECT id, rank, shares, symbols FROM code_files WHERE solution_guid = ?1
                 ORDER BY rank DESC, path LIMIT ?2",
            )?;
            let rows = files.query_map(params![solution, files_max], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for (order, row) in rows.enumerate() {
                let (fid, rank, shares, count) = row?;
                if best.len() >= n && rank <= best[n - 1].0 {
                    break;
                }
                let count = count.max(0) as usize;
                let mut own: Vec<(f64, usize)> = (0..count)
                    .map(|i| (rank * share_at(shares.as_deref(), i, count), i))
                    .collect();
                own.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
                best.extend(
                    own.into_iter()
                        .take(CENTRAL_PER_FILE)
                        .map(|(r, i)| (r, order, i, fid)),
                );
                best.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
                best.truncate(n);
            }
        }
        let mut sym = tx.prepare_cached(&format!(
            "SELECT {HIT_COLS} FROM code_symbols s JOIN code_files f ON f.id = s.file_id
             WHERE s.file_id = ?1 ORDER BY s.id LIMIT 1 OFFSET ?2"
        ))?;
        let mut out = Vec::with_capacity(best.len());
        for (_, _, idx, fid) in best {
            if let Some(h) = sym
                .query_row(params![fid, idx as i64], hit_row)
                .optional()?
            {
                out.push(h);
            }
        }
        Ok(out)
    })
    .await
}

/// Столбцы [`hit_row`]: ранг символа — из ранга файла, его долей, места символа среди
/// символов файла (по `id`, индекс `code_symbols_file`) и их числа.
const HIT_COLS: &str = "s.id, s.file_id, f.path, s.name, s.qual, s.kind, s.line, s.end_line,
    s.sig, f.rank, f.shares, f.symbols,
    (SELECT count(*) FROM code_symbols x WHERE x.file_id = s.file_id AND x.id < s.id)";
/// Число столбцов [`HIT_COLS`].
const HIT_COLS_N: usize = 13;

fn hit_row(r: &Row<'_>) -> rusqlite::Result<SymbolHit> {
    let file_rank: f64 = r.get(9)?;
    let shares: Option<Vec<u8>> = r.get(10)?;
    let n: i64 = r.get(11)?;
    let idx: i64 = r.get(12)?;
    let share = share_at(shares.as_deref(), idx as usize, n.max(0) as usize);
    Ok(SymbolHit {
        id: r.get(0)?,
        file_id: r.get(1)?,
        path: r.get(2)?,
        name: r.get(3)?,
        qual: r.get(4)?,
        kind: kind_col(r, 5)?,
        line: r.get(6)?,
        end_line: r.get(7)?,
        sig: r.get(8)?,
        rank: file_rank * share,
    })
}

/// Выдача по рангу символа (затем пути и строке), не больше `limit`.
fn sort_hits(hits: &mut Vec<SymbolHit>, limit: usize) {
    hits.sort_by(|a, b| {
        b.rank
            .total_cmp(&a.rank)
            .then_with(|| a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    hits.truncate(limit);
}

/// Запрос FTS5 из слов идентификатора: `prefix` — слова от [`FTS_PREFIX_MIN_CHARS`] символов
/// префиксом (`getUs` → `"get"* "us"*`), короче — точным словом; иначе все слова точные.
/// Слов нет — `None`. Слова — только буквы и цифры (`split_ident`): синтаксис FTS5 запроса не
/// действует.
fn symbol_fts(q: &str, prefix: bool) -> Option<String> {
    let w = split_ident(q);
    (!w.is_empty()).then(|| {
        w.iter()
            .map(|t| {
                if prefix && t.chars().count() >= FTS_PREFIX_MIN_CHARS {
                    format!("\"{t}\"*")
                } else {
                    format!("\"{t}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Поиск символа: сначала точное совпадение `qual` (затем имени), потом FTS по словам имени и
/// `qual` — точными словами, затем префиксами; внутри группы — по рангу символа. Фильтры — вид
/// и путь (файл или каталог). `limit` — 1..=100. FTS отдаёт не больше
/// [`FTS_CANDIDATES_MAX`] кандидатов на проход (без сортировки по bm25: на миллионе символов
/// она стоила секунды); решение и вид входят в сам запрос MATCH (столбцы `sol`, `kind`), так
/// что предел действует внутри решения, а путь фильтруется после — у очень частого слова в
/// узкой области выдача может быть неполна.
pub async fn code_symbols_search(
    db: &Db,
    solution: &str,
    q: &str,
    kind: Option<SymbolKind>,
    path: Option<&str>,
    limit: usize,
) -> Result<Vec<SymbolHit>, StoreError> {
    let limit = limit.clamp(1, SYMBOLS_LIMIT_MAX);
    let (solution, q) = (solution.to_string(), q.trim().to_string());
    let path = area_of(path);
    let kind = kind.map(SymbolKind::as_str);
    // решение и вид — в самом MATCH: предел кандидатов действует уже внутри решения (I6)
    let scope = match kind {
        Some(k) => format!("sol : \"{}\" AND kind : \"{k}\"", sol_token(&solution)),
        None => format!("sol : \"{}\"", sol_token(&solution)),
    };
    let passes: Vec<String> = [symbol_fts(&q, false), symbol_fts(&q, true)]
        .into_iter()
        .flatten()
        .fold(Vec::new(), |mut v, f| {
            if !v.contains(&f) {
                v.push(f);
            }
            v
        })
        .into_iter()
        .map(|f| format!("{scope} AND {{name qual terms}} : ({f})"))
        .collect();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let filters = "AND (?3 IS NULL OR s.kind = ?3)";
        let mut exact: Vec<(SymbolHit, bool)> = tx
            .prepare_cached(&format!(
                "SELECT {HIT_COLS}, s.qual = ?4 FROM code_symbols s
                 JOIN code_files f ON f.id = s.file_id
                 WHERE f.solution_guid = ?1 AND {AREA} {filters}
                   AND (s.qual = ?4 OR s.name = ?4)
                 ORDER BY s.qual = ?4 DESC, f.rank DESC LIMIT ?5"
            ))?
            .query_map(params![solution, path, kind, q, limit as i64], |r| {
                Ok((hit_row(r)?, r.get::<_, bool>(HIT_COLS_N)?))
            })?
            .collect::<Result<_, _>>()?;
        // совпадение `qual` — первым, внутри группы — по рангу символа
        exact.sort_by(|(a, qa), (b, qb)| {
            qb.cmp(qa)
                .then_with(|| b.rank.total_cmp(&a.rank))
                .then_with(|| a.path.cmp(&b.path))
                .then(a.line.cmp(&b.line))
        });
        let mut out: Vec<SymbolHit> = exact.into_iter().map(|(h, _)| h).collect();
        for fts in passes {
            if out.len() >= limit {
                break;
            }
            let mut more: Vec<SymbolHit> = tx
                .prepare_cached(&format!(
                    "SELECT {HIT_COLS} FROM
                       (SELECT rowid AS sid FROM code_symbols_fts
                        WHERE code_symbols_fts MATCH ?4 LIMIT ?5) m
                     JOIN code_symbols s ON s.id = m.sid
                     JOIN code_files f ON f.id = s.file_id
                     WHERE f.solution_guid = ?1 AND {AREA} {filters}"
                ))?
                .query_map(
                    params![solution, path, kind, fts, FTS_CANDIDATES_MAX],
                    hit_row,
                )?
                .collect::<Result<_, _>>()?;
            more.retain(|h| !out.iter().any(|o: &SymbolHit| o.id == h.id));
            sort_hits(&mut more, limit - out.len());
            out.extend(more);
        }
        Ok(out)
    })
    .await
}

/// Карточка символа решения `solution`; символа нет или он из другого решения — `None`.
pub async fn code_symbol(
    db: &Db,
    solution: &str,
    id: i64,
) -> Result<Option<SymbolCard>, StoreError> {
    let solution = solution.to_string();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let found = tx
            .prepare_cached(&format!(
                "SELECT {HIT_COLS}, s.body_hash, s.exported
                 FROM code_symbols s JOIN code_files f ON f.id = s.file_id
                 WHERE s.id = ?1 AND f.solution_guid = ?2"
            ))?
            .query_row(params![id, solution], |r| {
                Ok((
                    hit_row(r)?,
                    r.get::<_, String>(HIT_COLS_N)?,
                    r.get::<_, Option<bool>>(HIT_COLS_N + 1)?,
                ))
            })
            .optional()?;
        let Some((symbol, body_hash, exported)) = found else {
            return Ok(None);
        };
        let callers: Vec<Caller> = tx
            .prepare_cached(
                "SELECT f.id, f.path, sum(r.count) AS n FROM code_refs r
                 JOIN code_files f ON f.id = r.file_id
                 WHERE f.solution_guid = ?1 AND r.name = ?2 AND r.file_id <> ?3
                 GROUP BY f.id ORDER BY n DESC, f.path LIMIT ?4",
            )?
            .query_map(
                params![solution, symbol.name, symbol.file_id, CALLERS_MAX],
                |r| {
                    Ok(Caller {
                        file_id: r.get(0)?,
                        path: r.get(1)?,
                        count: u32::try_from(r.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                    })
                },
            )?
            .collect::<Result<_, _>>()?;
        let uses: Vec<RefAgg> = tx
            .prepare_cached(
                "SELECT name, kind, count, first_line, recv_count FROM code_refs
                 WHERE file_id = ?1 ORDER BY count DESC, name, kind LIMIT ?2",
            )?
            .query_map(params![symbol.file_id, USES_MAX], |r| {
                let k: String = r.get(1)?;
                let recv: u32 = r.get(4)?;
                Ok(RefAgg {
                    name: r.get(0)?,
                    kind: RefKind::parse(&k).ok_or_else(|| corrupt("вид ссылки", &k))?,
                    count: r.get(2)?,
                    first_line: r.get(3)?,
                    recv_count: (recv > 0).then_some(recv),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(Some(SymbolCard {
            symbol,
            body_hash,
            exported,
            callers,
            uses,
        }))
    })
    .await
}

/// Символы для пакета субагенту: с именем или `qual` из `names` (совпадение `qual` — первым,
/// затем по рангу), затем символы файлов `paths` по рангу; не больше `limit`.
pub async fn code_symbols_named(
    db: &Db,
    solution: &str,
    names: &[String],
    paths: &[String],
    limit: usize,
) -> Result<Vec<SymbolHit>, StoreError> {
    if limit == 0 || (names.is_empty() && paths.is_empty()) {
        return Ok(Vec::new());
    }
    let solution = solution.to_string();
    let names = serde_json::Value::from(names.to_vec()).to_string();
    let paths = serde_json::Value::from(paths.to_vec()).to_string();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let mut out: Vec<SymbolHit> = tx
            .prepare_cached(&format!(
                "SELECT {HIT_COLS} FROM code_symbols s JOIN code_files f ON f.id = s.file_id
                 WHERE f.solution_guid = ?1
                   AND (s.qual IN (SELECT value FROM json_each(?2))
                        OR s.name IN (SELECT value FROM json_each(?2)))
                 ORDER BY s.qual IN (SELECT value FROM json_each(?2)) DESC, f.rank DESC,
                          f.path, s.line
                 LIMIT ?3"
            ))?
            .query_map(params![solution, names, limit as i64], hit_row)?
            .collect::<Result<_, _>>()?;
        if out.len() < limit {
            // символы названных файлов — все (файлов в задаче единицы), затем по рангу символа
            let mut more: Vec<SymbolHit> = tx
                .prepare_cached(&format!(
                    "SELECT {HIT_COLS} FROM code_symbols s JOIN code_files f ON f.id = s.file_id
                     WHERE f.solution_guid = ?1 AND f.path IN (SELECT value FROM json_each(?2))"
                ))?
                .query_map(params![solution, paths], hit_row)?
                .collect::<Result<_, _>>()?;
            more.retain(|h| !out.iter().any(|o| o.id == h.id));
            sort_hits(&mut more, limit - out.len());
            out.extend(more);
        }
        Ok(out)
    })
    .await
}

/// Влияние правки: для файлов с присланными символами — символы карты с другим телом
/// (`changed`) и отсутствующие среди присланных (`removed`), файлы-потребители их имён на 1
/// шаг; файлы без символов — в `paths_only`. Путей нет в карте — пропускаются.
pub async fn code_impact(
    db: &Db,
    solution: &str,
    files: &[(String, Option<Vec<SymbolHash>>)],
) -> Result<ImpactRows, StoreError> {
    let solution = solution.to_string();
    let files = files.to_vec();
    db.read(move |c| {
        let tx = c.unchecked_transaction()?;
        let mut out = ImpactRows {
            changed: Vec::new(),
            removed: Vec::new(),
            consumers: Vec::new(),
            paths_only: Vec::new(),
        };
        let mut own: HashSet<i64> = HashSet::new();
        let mut st = tx.prepare_cached(
            "SELECT s.id, f.path, s.name, s.qual, s.kind, s.line, s.body_hash, f.id
             FROM code_symbols s JOIN code_files f ON f.id = s.file_id
             WHERE f.solution_guid = ?1 AND f.path = ?2 ORDER BY s.line, s.id",
        )?;
        for (path, symbols) in &files {
            let Some(symbols) = symbols else {
                if !out.paths_only.contains(path) {
                    out.paths_only.push(path.clone());
                }
                continue;
            };
            let given: HashMap<&str, &str> = symbols
                .iter()
                .map(|s| (s.qual.as_str(), s.body_hash.as_str()))
                .collect();
            let rows = st.query_map(params![solution, path], |r| {
                Ok((
                    ImpactSym {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        name: r.get(2)?,
                        qual: r.get(3)?,
                        kind: kind_col(r, 4)?,
                        line: r.get(5)?,
                    },
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (s, body, fid) = row?;
                own.insert(fid);
                match given.get(s.qual.as_str()) {
                    Some(b) if *b == body => {}
                    Some(_) => out.changed.push(s),
                    None => out.removed.push(s),
                }
            }
        }
        let names: Vec<String> = {
            let mut n: Vec<String> = out
                .changed
                .iter()
                .chain(&out.removed)
                .map(|s| s.name.clone())
                .collect();
            n.sort();
            n.dedup();
            n
        };
        if !names.is_empty() {
            let mut st = tx.prepare_cached(
                "SELECT f.id, f.path, r.name, r.count FROM code_refs r
                 JOIN code_files f ON f.id = r.file_id
                 WHERE f.solution_guid = ?1 AND r.name IN (SELECT value FROM json_each(?2))
                 ORDER BY f.path, r.name",
            )?;
            let rows = st.query_map(
                params![solution, serde_json::Value::from(names).to_string()],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )?;
            for row in rows {
                let (fid, path, name, n) = row?;
                if own.contains(&fid) {
                    continue;
                }
                let n = u32::try_from(n).unwrap_or(u32::MAX);
                match out.consumers.iter_mut().find(|c| c.file_id == fid) {
                    Some(c) => {
                        if !c.names.contains(&name) {
                            c.names.push(name);
                        }
                        c.count = c.count.saturating_add(n);
                    }
                    None => out.consumers.push(Consumer {
                        file_id: fid,
                        path,
                        names: vec![name],
                        count: n,
                    }),
                }
            }
            out.consumers
                .sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
        }
        Ok(out)
    })
    .await
}

/// Файлов в одной транзакции очистки: каскад символов, ссылок и строк FTS — около 1 мс на файл
/// на HDD, пачка держит писателя доли секунды (одна транзакция на 49 500 файлов — 48 с, ревью
/// задачи 3, M2).
const CLEAR_BATCH: i64 = 200;

/// Снимок есть — `gen + 1`, счётчики, `rank_state = pending`.
fn touch_if_snapshot(tx: &Connection, solution: &str) -> Result<(), StoreError> {
    if tx
        .prepare_cached("SELECT 1 FROM code_snapshots WHERE solution_guid = ?1")?
        .exists([solution])?
    {
        touch_snapshot(tx, solution, &fmt_ts(crate::time::now()))?;
    }
    Ok(())
}

/// Очищает карту решения (файлы, символы, FTS, ссылки — каскадом) пачками по [`CLEAR_BATCH`]
/// файлов, каждая — своей короткой транзакцией; возвращает число удалённых файлов. Строка
/// снимка остаётся с `gen + 1` после каждой пачки (а не удаляется): иначе поколение начиналось
/// бы заново и ранги, посчитанные до очистки, совпали бы по `gen` с новой картой (ABA), а ранги
/// частично очищенной карты не сохранятся. Загрузка, пришедшая во время очистки, может быть
/// удалена следующей пачкой — очистка означает «всё, что было до её конца».
pub async fn code_clear(db: &Db, solution: &str) -> Result<u64, StoreError> {
    let mut total = 0u64;
    loop {
        let sol = solution.to_string();
        let n = db
            .write(move |tx| {
                let n = tx
                    .prepare_cached(
                        "DELETE FROM code_files WHERE id IN
                           (SELECT id FROM code_files WHERE solution_guid = ?1 LIMIT ?2)",
                    )?
                    .execute(params![sol, CLEAR_BATCH])?;
                // пустая карта — поколение всё равно растёт (очистка — изменение карты)
                touch_if_snapshot(tx, &sol)?;
                Ok(n as u64)
            })
            .await?;
        total += n;
        if n < CLEAR_BATCH as u64 {
            break;
        }
    }
    Ok(total)
}
