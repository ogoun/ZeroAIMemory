//! Открытие базы без изменений и сверка схемы с эталоном (M7.4 §7.1 пп. 5–6). Потребители —
//! `zam-server rollback` (проверка копий `bak-v<N>` до первого изменения, §7.2) и `zam-server
//! doctor` (§7.5).
//!
//! **Только чтение.** [`open_ro`] не применяет миграций, не досбирает схему и не пишет в файл:
//! закрытая база (рядом нет `-wal` и `-shm`) читается `immutable=1` — без блокировок и без
//! создания файлов рядом; база с `-wal`/`-shm` (сервер запущен или остановлен аварийно) —
//! соединением `READ_ONLY`, которое видит кадры WAL (SQLite при этом может обновить индекс
//! `-shm`, сам файл базы — нет). У `immutable=1` есть цена: базу, которую сервер откроет
//! посреди чтения, такое соединение видит несогласованной — для запущенного сервера читайте
//! базы, которые он уже открыл (у них есть `-wal`/`-shm`).
//!
//! **Сверка схемы** ([`RoDb::schema_diff`]): эталон — `:memory:` с миграциями
//! `1..=user_version` программы (без копий), сравниваются столбцы (`pragma_table_xinfo`: имя,
//! тип, `notnull`, умолчание, `pk`, скрытость), определение виртуальных таблиц, индексы
//! (`pragma_index_list`: уникальность, происхождение, частичность; `pragma_index_xinfo`; SQL),
//! триггеры и представления — имя и SQL без пробельных различий. Объекты `sqlite_%`
//! (служебные и автоиндексы) и таблица мигратора `zam_migrations` не сравниваются. Итог — до
//! [`DIFF_MAX`] строк.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::StoreError;
use crate::migrate::{self, Migration};

/// Сколько различий схемы возвращает [`RoDb::schema_diff`] (последняя строка — «… и ещё N»).
pub const DIFF_MAX: usize = 20;

/// Сколько строк проблем возвращают `quick_check` и `integrity_check`.
const CHECK_ROWS: u32 = 50;

/// Состояние сумм миграций базы ([`RoDb::checksum_status`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecksumStatus {
    /// Суммы всех применённых версий записаны и совпадают с программой.
    Ok,
    /// База до 0.0.17 (таблицы сумм нет или версии без строк): суммы примет первое открытие
    /// сервером ([`crate::migrate`], `adopted`).
    AdoptedPending,
    /// Записанная сумма этих версий расходится с программой: сервер базу не откроет.
    Changed(Vec<u32>),
}

/// База, открытая только для чтения ([`open_ro`]).
#[derive(Debug)]
pub struct RoDb {
    conn: Connection,
    path: PathBuf,
    /// Открыта `immutable=1` (рядом не было `-wal` и `-shm`).
    immutable: bool,
}

/// Путь `<база><suffix>` (`-wal`, `-shm`).
fn side(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Открывает файл базы `path` только для чтения (см. документацию модуля). Файла нет —
/// ошибка, файл не создаётся; не база SQLite — ошибка здесь же.
pub fn open_ro(path: &Path) -> Result<RoDb, StoreError> {
    if !path.is_file() {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("нет файла базы {}", path.display()),
        )));
    }
    let closed = !side(path, "-wal").exists() && !side(path, "-shm").exists();
    let conn = if closed {
        Connection::open_with_flags(
            crate::backup::immutable_uri(path),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?
    } else {
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?
    };
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    // заголовок и схема читаются сразу: «не база» и повреждённый `sqlite_master` — здесь
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(RoDb {
        conn,
        path: path.to_path_buf(),
        immutable: closed,
    })
}

/// Строки `PRAGMA <pragma>` кроме единственного `ok`.
fn check_rows(c: &Connection, sql: &str) -> Result<Vec<String>, StoreError> {
    let mut st = c.prepare(sql)?;
    let rows: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(if rows.len() == 1 && rows[0] == "ok" {
        Vec::new()
    } else {
        rows
    })
}

impl RoDb {
    /// Путь файла базы.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Открыта `immutable=1` (база была закрыта). Если за время чтения её открыл сервер (рядом
    /// появились `-wal`/`-shm`), прочитанное может быть несогласованным — проверку стоит
    /// повторить новым [`open_ro`] (`zam-server doctor`).
    pub fn immutable(&self) -> bool {
        self.immutable
    }

    /// Рядом с файлом базы появились `-wal` или `-shm` после открытия `immutable=1`.
    pub fn opened_meanwhile(&self) -> bool {
        self.immutable && (side(&self.path, "-wal").exists() || side(&self.path, "-shm").exists())
    }

    /// Соединение только для чтения (`query_only`): запросы потребителей (`doctor`, отбор
    /// сборщика мусора [`crate::blob_gc::plan`]).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// `PRAGMA user_version`.
    pub fn user_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Суммы миграций базы против списка программы `migrations`.
    pub fn checksum_status(&self, migrations: &[Migration]) -> Result<ChecksumStatus, StoreError> {
        let version = self.user_version()?;
        let rows = migrate::recorded(&self.conn)?;
        let changed: Vec<u32> = migrate::changed(&rows, migrations, version)
            .into_iter()
            .map(|(v, _, _)| v)
            .collect();
        if !changed.is_empty() {
            return Ok(ChecksumStatus::Changed(changed));
        }
        let have: BTreeSet<u32> = rows.iter().map(|(v, _)| *v).collect();
        let all_recorded = migrations
            .iter()
            .map(|m| m.version)
            .filter(|v| *v <= version)
            .all(|v| have.contains(&v));
        Ok(if all_recorded {
            ChecksumStatus::Ok
        } else {
            ChecksumStatus::AdoptedPending
        })
    }

    /// Различия схемы базы и эталона её версии из `migrations` (до [`DIFF_MAX`] строк; пусто —
    /// совпадает). Версия новее списка — [`StoreError::SchemaTooNew`].
    pub fn schema_diff(&self, migrations: &[Migration]) -> Result<Vec<String>, StoreError> {
        let version = self.user_version()?;
        let supported = migrations.len() as u32;
        if version > supported {
            return Err(StoreError::SchemaTooNew {
                found: version,
                supported,
            });
        }
        let mut reference = Connection::open_in_memory()?;
        reference.pragma_update(None, "foreign_keys", true)?;
        migrate::apply(
            &mut reference,
            Path::new(":memory:"),
            &migrations[..version as usize],
        )?;
        let want = Schema::read(&reference)?;
        let have = Schema::read(&self.conn)?;
        let mut diff = want.diff(&have);
        if diff.len() > DIFF_MAX {
            let rest = diff.len() - (DIFF_MAX - 1);
            diff.truncate(DIFF_MAX - 1);
            diff.push(format!("… и ещё {rest} различий"));
        }
        Ok(diff)
    }

    /// `PRAGMA quick_check`: строки проблем (пусто — `ok`).
    pub fn quick_check(&self) -> Result<Vec<String>, StoreError> {
        check_rows(&self.conn, &format!("PRAGMA quick_check({CHECK_ROWS})"))
    }

    /// `PRAGMA integrity_check` (с SQLite 3.44 — и виртуальных таблиц FTS5): строки проблем
    /// (пусто — `ok`).
    pub fn integrity_check(&self) -> Result<Vec<String>, StoreError> {
        check_rows(&self.conn, &format!("PRAGMA integrity_check({CHECK_ROWS})"))
    }

    /// Проверка каждой таблицы FTS5 (`PRAGMA integrity_check(<таблица>)` — её `xIntegrity`,
    /// только чтение): строки «таблица: проблема» (пусто — всё в порядке).
    pub fn fts_check(&self) -> Result<Vec<String>, StoreError> {
        let names: Vec<String> = {
            let mut st = self.conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                   AND lower(sql) LIKE 'create virtual table%using fts5%' ORDER BY name",
            )?;
            st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        let mut out = Vec::new();
        for n in names {
            let sql = format!("PRAGMA integrity_check(\"{}\")", n.replace('"', "\"\""));
            out.extend(
                check_rows(&self.conn, &sql)?
                    .into_iter()
                    .map(|p| format!("{n}: {p}")),
            );
        }
        Ok(out)
    }
}

/// SQL без пробельных различий.
fn norm(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Столбцы таблицы по порядку: `(имя, описание)`.
type Columns = Vec<(String, String)>;

/// Таблица схемы: столбцы и SQL виртуальной таблицы (у обычной — `None`).
type TableShape = (Columns, Option<String>);

/// Объекты схемы, которые сверяет [`RoDb::schema_diff`].
#[derive(Debug, Default)]
struct Schema {
    /// Таблица → столбцы и SQL виртуальной таблицы.
    tables: BTreeMap<String, TableShape>,
    /// Индекс → (таблица, описание).
    indexes: BTreeMap<String, (String, String)>,
    triggers: BTreeMap<String, String>,
    views: BTreeMap<String, String>,
}

impl Schema {
    fn read(c: &Connection) -> Result<Schema, StoreError> {
        let objects: Vec<(String, String, String, String)> = {
            let mut st = c.prepare(
                "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
                   AND name <> ?1 AND tbl_name <> ?1",
            )?;
            st.query_map([migrate::TABLE], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?
        };
        let index_sql: BTreeMap<&str, &str> = objects
            .iter()
            .filter(|o| o.0 == "index")
            .map(|o| (o.1.as_str(), o.3.as_str()))
            .collect();
        let mut s = Schema::default();
        for (kind, name, _, sql) in &objects {
            match kind.as_str() {
                "table" => {
                    let vsql = sql
                        .trim_start()
                        .to_ascii_uppercase()
                        .starts_with("CREATE VIRTUAL TABLE")
                        .then(|| norm(sql));
                    s.tables.insert(name.clone(), (columns(c, name)?, vsql));
                    for (iname, desc) in indexes(c, name, &index_sql)? {
                        s.indexes.insert(iname, (name.clone(), desc));
                    }
                }
                "trigger" => {
                    s.triggers.insert(name.clone(), norm(sql));
                }
                "view" => {
                    s.views.insert(name.clone(), norm(sql));
                }
                _ => {}
            }
        }
        Ok(s)
    }

    /// Различия `have` (база) от `self` (эталон) текстом.
    fn diff(&self, have: &Schema) -> Vec<String> {
        let mut out = Vec::new();
        for (t, (want_cols, want_v)) in &self.tables {
            let Some((have_cols, have_v)) = have.tables.get(t) else {
                out.push(format!("нет таблицы {t}"));
                continue;
            };
            let hmap: BTreeMap<&str, &str> = have_cols
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_str()))
                .collect();
            let wmap: BTreeMap<&str, &str> = want_cols
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_str()))
                .collect();
            for (n, d) in want_cols {
                match hmap.get(n.as_str()) {
                    None => out.push(format!("таблица {t}: нет столбца {n}")),
                    Some(h) if h != d => {
                        out.push(format!("таблица {t}, столбец {n}: «{h}» вместо «{d}»"))
                    }
                    Some(_) => {}
                }
            }
            for (n, _) in have_cols {
                if !wmap.contains_key(n.as_str()) {
                    out.push(format!("таблица {t}: лишний столбец {n}"));
                }
            }
            let order = |cols: &[(String, String)], other: &BTreeMap<&str, &str>| -> Vec<String> {
                cols.iter()
                    .filter(|(n, _)| other.contains_key(n.as_str()))
                    .map(|(n, _)| n.clone())
                    .collect()
            };
            if order(want_cols, &hmap) != order(have_cols, &wmap) {
                out.push(format!("таблица {t}: порядок столбцов отличается"));
            }
            if want_v != have_v {
                out.push(format!(
                    "таблица {t}: определение виртуальной таблицы отличается"
                ));
            }
        }
        for t in have.tables.keys() {
            if !self.tables.contains_key(t) {
                out.push(format!("лишняя таблица {t}"));
            }
        }
        for (i, (t, d)) in &self.indexes {
            match have.indexes.get(i) {
                None => out.push(format!("нет индекса {i} (таблица {t})")),
                Some((ht, hd)) if ht != t || hd != d => {
                    out.push(format!("индекс {i}: определение отличается"))
                }
                Some(_) => {}
            }
        }
        for (i, (t, _)) in &have.indexes {
            if !self.indexes.contains_key(i) {
                out.push(format!("лишний индекс {i} (таблица {t})"));
            }
        }
        named_diff(
            &mut out,
            &self.triggers,
            &have.triggers,
            "триггер",
            "триггера",
        );
        named_diff(
            &mut out,
            &self.views,
            &have.views,
            "представление",
            "представления",
        );
        out
    }
}

/// Различия объектов «имя → SQL» (`what` — именительный, `of` — родительный падеж).
fn named_diff(
    out: &mut Vec<String>,
    want: &BTreeMap<String, String>,
    have: &BTreeMap<String, String>,
    what: &str,
    of: &str,
) {
    for (n, sql) in want {
        match have.get(n) {
            None => out.push(format!("нет {of} {n}")),
            Some(h) if h != sql => out.push(format!("{what} {n}: SQL отличается")),
            Some(_) => {}
        }
    }
    for n in have.keys() {
        if !want.contains_key(n) {
            out.push(format!("лишний {what} {n}"));
        }
    }
}

/// Столбцы таблицы `t` по порядку: `(имя, «тип notnull умолчание pk скрытость»)`.
fn columns(c: &Connection, t: &str) -> Result<Columns, StoreError> {
    let mut st = c.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1)
         ORDER BY cid",
    )?;
    let rows = st.query_map([t], |r| {
        let name: String = r.get(0)?;
        let ty: String = r.get(1)?;
        let notnull: i64 = r.get(2)?;
        let dflt: Option<String> = r.get(3)?;
        let pk: i64 = r.get(4)?;
        let hidden: i64 = r.get(5)?;
        Ok((
            name,
            format!(
                "{ty} notnull={notnull} default={} pk={pk} hidden={hidden}",
                dflt.as_deref().unwrap_or("NULL")
            ),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Индексы таблицы `t` (кроме автоиндексов `sqlite_%`): `(имя, описание)`.
fn indexes(
    c: &Connection,
    t: &str,
    sql: &BTreeMap<&str, &str>,
) -> Result<Vec<(String, String)>, StoreError> {
    let list: Vec<(String, i64, String, i64)> = {
        let mut st = c.prepare(
            "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1) ORDER BY name",
        )?;
        st.query_map([t], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?
    };
    let mut out = Vec::new();
    for (name, unique, origin, partial) in list {
        if name.starts_with("sqlite_") {
            continue;
        }
        let cols: Vec<String> = {
            let mut st = c.prepare(
                "SELECT cid, name, \"desc\", coll, key FROM pragma_index_xinfo(?1) ORDER BY seqno",
            )?;
            st.query_map([&name], |r| {
                let cid: i64 = r.get(0)?;
                let col: Option<String> = r.get(1)?;
                let desc: i64 = r.get(2)?;
                let coll: Option<String> = r.get(3)?;
                let key: i64 = r.get(4)?;
                let col = col.unwrap_or_else(|| {
                    if cid == -1 {
                        "<rowid>"
                    } else {
                        "<выражение>"
                    }
                    .to_string()
                });
                Ok(format!(
                    "{col}:{desc}:{}:{key}",
                    coll.as_deref().unwrap_or("")
                ))
            })?
            .collect::<Result<_, _>>()?
        };
        out.push((
            name.clone(),
            format!(
                "unique={unique} origin={origin} partial={partial} cols=[{}] sql={}",
                cols.join(", "),
                norm(sql.get(name.as_str()).copied().unwrap_or(""))
            ),
        ));
    }
    Ok(out)
}
