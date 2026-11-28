//! `zam-server doctor [--full] [--fix] [--json]` (M7.4 §7.5, решение 107): проверка каталога
//! данных для администратора.
//!
//! **Режимы.** Без `--fix` — только чтение: базы открываются [`zam_store::inspect::open_ro`] (без
//! миграций, досборки и записи), лок каталога не берётся — работает и при запущенном сервере.
//! База, которую сервер открыл посреди чтения (`immutable=1` видит её несогласованной),
//! проверяется второй раз обычным соединением: ложной тревоги из-за этого нет. `--fix` — только на
//! остановленном сервере (лок каталога данных; занят — [`DoctorError::Busy`]) и только безопасное:
//! остатки `.restore-staging`, брошенный признак копии мёртвого процесса, `.incoming/*.part`
//! старше суток, FTS5 `rebuild` у таблиц с ошибкой, файлы `.gc/` — строкам, проход сборщика
//! мусора блобов ([`zam_store::blob_gc::sweep`]). Схему, суммы миграций и битые базы `--fix` не
//! правит — проверка называет `restore` или `rollback`. `--full` — `integrity_check` вместо
//! `quick_check` и SHA-256 моделей.
//!
//! **Итог** — строки `ok | info | warn | error <код>: <текст>` ([`Report::text`]) или JSON
//! `{checks: [{code, level, text, data}], errors, warnings}`; код выхода — 0 без `error`, 1 с
//! `error` (и отказ `--fix`), 2 — конфигурация не прочитана (`main.rs`).
//!
//! **Проверки:** `data_dir` (каталог, лок, признаки незавершённого восстановления и отката,
//! копии, остатки сборки), `disk` (свободное место тома), `db:system` и `db:<user>` (открытие,
//! `quick_check`, версия, суммы, сверка схемы, FTS5, `-wal`/`-shm` без базы, копии `bak-v*`),
//! `vault_key`, `qdrant`, `models`, `backup` (условие `no_recent_backup`), по пользователю —
//! `blobs:<user>`, `links:<user>`, `check:<user>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::inspect::{ChecksumStatus, RoDb, open_ro};
use zam_store::{LockMark, Migration, SYSTEM_MIGRATIONS, StoreError, USER_MIGRATIONS};

use crate::config::{CheckMode, ServerConfig, WorksSection};

/// Свободного места меньше — `error`.
pub const DISK_ERROR_BYTES: u64 = 512 * 1024 * 1024;
/// Свободного места меньше — `warn`.
pub const DISK_WARN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Сколько примеров показывает строка проверки (различия схемы, рёбра, блобы).
const FIRST: usize = 5;
/// Ночная проверка включена, а законченного ночного прогона нет столько — `warn`.
const CHECK_STALE: SignedDuration = SignedDuration::from_hours(48);

/// Уровень строки проверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// Строка проверки.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub code: String,
    pub level: Level,
    pub text: String,
    pub data: Value,
}

/// Итог `doctor`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
    pub errors: usize,
    pub warnings: usize,
    /// Время проверки, мс (только в тексте).
    #[serde(skip)]
    pub ms: u128,
}

impl Report {
    /// Строки для человека и итог.
    pub fn text(&self) -> String {
        let mut out: Vec<String> = self
            .checks
            .iter()
            .map(|c| format!("{} {}: {}", c.level.as_str(), c.code, c.text))
            .collect();
        out.push(format!(
            "итого: ошибок {}, предупреждений {} ({} мс)",
            self.errors, self.warnings, self.ms
        ));
        out.join("\n")
    }

    /// Код выхода: 0 — без `error`, 1 — есть `error`.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.errors > 0)
    }

    /// Строки с кодом `code`.
    pub fn of<'a>(&'a self, code: &'a str) -> impl Iterator<Item = &'a Check> + 'a {
        self.checks.iter().filter(move |c| c.code == code)
    }
}

/// Режимы `doctor`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoctorOpts {
    /// `integrity_check` вместо `quick_check`, SHA-256 моделей.
    pub full: bool,
    /// Безопасные исправления на остановленном сервере.
    pub fix: bool,
}

/// Отказ `doctor`.
#[derive(Debug, thiserror::Error)]
pub enum DoctorError {
    /// `--fix` при занятом каталоге данных.
    #[error(
        "каталог данных занят: zam-server запущен — остановите сервер (doctor --fix работает \
         только на остановленном сервере; без --fix doctor только читает)"
    )]
    Busy,
}

/// Накопитель строк.
#[derive(Default)]
struct Out {
    checks: Vec<Check>,
}

impl Out {
    fn push(&mut self, level: Level, code: &str, text: impl Into<String>, data: Value) {
        self.checks.push(Check {
            code: code.to_string(),
            level,
            text: text.into(),
            data,
        });
    }
}

fn mib(b: u64) -> String {
    format!("{:.1} МиБ", b as f64 / (1024.0 * 1024.0))
}

/// Путь `<база><suffix>`.
fn side(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn has_side(p: &Path) -> bool {
    side(p, "-wal").exists() || side(p, "-shm").exists()
}

/// Начало sha256 для текста.
fn short(sha: &str) -> &str {
    sha.get(..16).unwrap_or(sha)
}

/// Проверка (и с `--fix` — исправления) каталога данных конфигурации `cfg`. `env_key` —
/// значение `ZAM_MASTER_KEY` (`None` — переменной нет).
pub async fn run(
    cfg: &ServerConfig,
    opts: DoctorOpts,
    env_key: Option<&str>,
) -> Result<Report, DoctorError> {
    let t0 = Instant::now();
    let now = zam_store::time::now();
    let root = cfg.data_dir.as_path();
    let mut out = Out::default();
    let mut fixes = Out::default();
    let lock = if opts.fix && root.is_dir() {
        Some(zam_store::try_lock_dir(root).ok_or(DoctorError::Busy)?)
    } else {
        None
    };
    if lock.is_some() {
        fix(cfg, now, &mut fixes).await;
    }
    if data_dir(cfg, lock.is_some(), &mut out) {
        disk(root, &mut out);
        let sys = root.join("system.db");
        let users = user_dbs(root);
        let sys_ro = if sys.is_file() || !users.is_empty() {
            db_check("db:system", &sys, SYSTEM_MIGRATIONS, opts.full, &mut out)
        } else {
            out.push(
                Level::Info,
                "db:system",
                "баз ещё нет: сервер с этим каталогом не запускался",
                json!({ "path": sys }),
            );
            None
        };
        side_files(root, &mut out);
        let works = works_effective(cfg, sys_ro.as_ref());
        vault_key(cfg, sys_ro.as_ref(), env_key, &mut out);
        qdrant(cfg, sys_ro.as_ref(), &mut out).await;
        models(cfg, opts.full, &mut out);
        backup(cfg, now, &mut out);
        for (user, path) in &users {
            let code = format!("db:{user}");
            let view = db_check(&code, path, USER_MIGRATIONS, opts.full, &mut out);
            if let Some(ro) = &view {
                blobs(cfg, user, ro, now, &mut out);
                links(user, ro, &mut out);
                check(&works, user, ro, now, &mut out);
            }
        }
    }
    drop(lock);
    out.checks.extend(fixes.checks);
    let errors = out
        .checks
        .iter()
        .filter(|c| c.level == Level::Error)
        .count();
    let warnings = out.checks.iter().filter(|c| c.level == Level::Warn).count();
    Ok(Report {
        checks: out.checks,
        errors,
        warnings,
        ms: t0.elapsed().as_millis(),
    })
}

// ---------- каталог и диск ----------

/// `data_dir`: `false` — каталога нет, дальше проверять нечего.
fn data_dir(cfg: &ServerConfig, own_lock: bool, out: &mut Out) -> bool {
    let root = cfg.data_dir.as_path();
    let code = "data_dir";
    if !root.is_dir() {
        out.push(
            Level::Error,
            code,
            format!("каталога данных нет: {}", root.display()),
            json!({ "path": root }),
        );
        return false;
    }
    let pid = std::fs::read_to_string(root.join(".zam.lock"))
        .ok()
        .and_then(|t| t.split_whitespace().last()?.parse::<u32>().ok());
    let (level, state, running) = if own_lock {
        (
            Level::Ok,
            "сервер остановлен (лок у doctor --fix)".to_string(),
            false,
        )
    } else {
        match zam_store::lock_mark(root) {
            None => (Level::Ok, "сервер остановлен".to_string(), false),
            Some(mark) => {
                let what = match mark {
                    LockMark::Serve => "сервер запущен",
                    LockMark::Restore => "идёт zam-server restore",
                    LockMark::Rollback => "идёт zam-server rollback",
                };
                let pid_text = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
                (
                    Level::Info,
                    format!("{what}{pid_text}: проверка только читает"),
                    true,
                )
            }
        }
    };
    out.push(
        level,
        code,
        format!("{}; {state}", root.display()),
        json!({ "path": root, "running": running, "pid": pid }),
    );
    if zam_store::restore_incomplete(root) {
        let (text, cmd) = if zam_store::rollback_incomplete(root) {
            (
                StoreError::RollbackIncomplete(root.to_path_buf()).to_string(),
                "rollback",
            )
        } else {
            (
                StoreError::RestoreIncomplete(root.to_path_buf()).to_string(),
                "restore",
            )
        };
        let flag = std::fs::read_to_string(root.join(zam_store::RESTORE_INCOMPLETE_FILE))
            .unwrap_or_default();
        out.push(
            Level::Error,
            code,
            format!("{text}; сервер не запустится, пока признак есть"),
            json!({ "command": cmd, "flag": flag.trim() }),
        );
    }
    match zam_store::backup::backup_flag_state(root, cfg.backup.flag_max_age()) {
        zam_store::backup::FlagState::Absent => {}
        zam_store::backup::FlagState::Active => out.push(
            Level::Info,
            code,
            "идёт копия (признак .zam-backup держит её процесс)",
            json!({ "backup_flag": "active" }),
        ),
        zam_store::backup::FlagState::Stale(why) => out.push(
            Level::Warn,
            code,
            format!("брошенный признак копии .zam-backup ({why}) — уберёт doctor --fix"),
            json!({ "backup_flag": "stale", "reason": why }),
        ),
    }
    let staging = root.join(crate::backup::restore::STAGING);
    if staging.exists() {
        out.push(
            Level::Warn,
            code,
            format!(
                "остатки сборки восстановления {} — удалит doctor --fix",
                crate::backup::restore::STAGING
            ),
            json!({ "path": staging }),
        );
    }
    true
}

/// `disk`: свободное место тома каталога данных.
fn disk(root: &Path, out: &mut Out) {
    match fs4::available_space(root) {
        Ok(free) => {
            let level = if free < DISK_ERROR_BYTES {
                Level::Error
            } else if free < DISK_WARN_BYTES {
                Level::Warn
            } else {
                Level::Ok
            };
            out.push(
                level,
                "disk",
                format!("свободно {} на томе каталога данных", mib(free)),
                json!({ "free_bytes": free }),
            );
        }
        Err(e) => out.push(
            Level::Warn,
            "disk",
            format!("свободное место не узнать: {e}"),
            Value::Null,
        ),
    }
}

// ---------- базы ----------

/// Базы пользователей `users/<id>.db`: `(id, путь)` по id.
fn user_dbs(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(root.join("users")) else {
        return Vec::new();
    };
    let mut v: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".db")?;
            id.parse::<Id>().ok()?;
            e.path().is_file().then(|| (id.to_string(), e.path()))
        })
        .collect();
    v.sort();
    v
}

/// Код проверки базы по имени её файла (`system.db` → `db:system`, `<id>.db` → `db:<id>`).
fn db_code(file: &str) -> String {
    match file.strip_suffix(".db") {
        Some("system") => "db:system".into(),
        Some(id) => format!("db:{id}"),
        None => format!("db:{file}"),
    }
}

/// `-wal`/`-shm` без базы (`warn`) и копии перед миграцией `bak-v*` (`info`).
fn side_files(root: &Path, out: &mut Out) {
    let mut baks: BTreeMap<String, Vec<(u32, u64)>> = BTreeMap::new();
    for dir in [root.to_path_buf(), root.join("users")] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<(String, u64)> = rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().into_string().ok()?;
                let len = e.metadata().ok()?.len();
                Some((n, len))
            })
            .collect();
        names.sort();
        for (n, len) in names {
            if let Some(base) = n.strip_suffix("-wal").or_else(|| n.strip_suffix("-shm")) {
                // журнал копии `bak-v<N>-journal` и прочее — не базы каталога
                if base.ends_with(".db") && !dir.join(base).exists() {
                    out.push(
                        Level::Warn,
                        &db_code(base),
                        format!("{n} без базы {base} (остаток после переноса или удаления)"),
                        json!({ "path": dir.join(&n) }),
                    );
                }
            } else if let Some((base, v)) = n.split_once(".bak-v")
                && let Ok(v) = v.parse::<u32>()
            {
                baks.entry(base.to_string()).or_default().push((v, len));
            }
        }
    }
    for (base, list) in baks {
        let text = list
            .iter()
            .map(|(v, len)| format!("v{v} ({})", mib(*len)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(
            Level::Info,
            &db_code(&base),
            format!("копии перед миграцией: {text} (zam-server rollback)"),
            json!({ "copies": list.iter().map(|(v, len)| json!({"version": v, "bytes": len})).collect::<Vec<_>>() }),
        );
    }
}

/// Проверка базы `path` (код `code`) против миграций программы. Базу, открытую `immutable=1`,
/// которую сервер открыл посреди чтения, с ошибкой — второй раз.
fn db_check(
    code: &str,
    path: &Path,
    migrations: &'static [Migration],
    full: bool,
    out: &mut Out,
) -> Option<RoDb> {
    let closed = !has_side(path);
    let mut first = Out::default();
    let view = db_once(code, path, migrations, full, &mut first);
    let failed = first.checks.iter().any(|c| c.level == Level::Error);
    if failed && closed && has_side(path) {
        let view = db_once(code, path, migrations, full, out);
        return view;
    }
    out.checks.extend(first.checks);
    view
}

fn db_once(
    code: &str,
    path: &Path,
    migrations: &'static [Migration],
    full: bool,
    out: &mut Out,
) -> Option<RoDb> {
    let start = out.checks.len();
    let data = json!({ "path": path });
    let ro = match open_ro(path) {
        Ok(ro) => ro,
        Err(e) => {
            out.push(
                Level::Error,
                code,
                format!(
                    "база не открывается: {e} — восстановите копию (zam-server restore) или \
                     откатите (zam-server rollback)"
                ),
                data,
            );
            return None;
        }
    };
    let checked = if full {
        ro.integrity_check()
    } else {
        ro.quick_check()
    };
    let what = if full {
        "integrity_check"
    } else {
        "quick_check"
    };
    match checked {
        Ok(rows) if rows.is_empty() => {}
        Ok(rows) => {
            let text = format!(
                "{what}: {} — база повреждена: восстановите копию (zam-server restore)",
                rows.iter()
                    .take(FIRST)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            out.push(
                Level::Error,
                code,
                text,
                json!({ "path": path, "check": what, "rows": rows }),
            );
        }
        Err(e) => {
            out.push(Level::Error, code, format!("{what}: {e}"), data.clone());
        }
    }
    let supported = migrations.len() as u32;
    let version = match ro.user_version() {
        Ok(v) => v,
        Err(e) => {
            out.push(Level::Error, code, format!("версия схемы: {e}"), data);
            return Some(ro);
        }
    };
    if version > supported {
        out.push(
            Level::Error,
            code,
            format!(
                "схема v{version} новее программы (v{supported}): верните выпуск, которым база \
                 обновлялась, или восстановите копию (zam-server restore)"
            ),
            json!({ "path": path, "version": version, "supported": supported }),
        );
        return Some(ro);
    }
    if version < supported {
        out.push(
            Level::Info,
            code,
            format!("схема v{version}, программа v{supported}: обновится при запуске сервера"),
            json!({ "path": path, "version": version, "supported": supported }),
        );
    }
    match ro.checksum_status(migrations) {
        Ok(ChecksumStatus::Ok) => {}
        Ok(ChecksumStatus::AdoptedPending) => out.push(
            Level::Info,
            code,
            "суммы миграций примет первое открытие сервером (база до 0.0.17)",
            json!({ "path": path, "checksums": "adopted_pending" }),
        ),
        Ok(ChecksumStatus::Changed(vs)) => {
            let list = vs.iter().map(|v| format!("v{v}")).collect::<Vec<_>>();
            out.push(
                Level::Error,
                code,
                format!(
                    "миграции {} изменились после применения к этой базе: верните выпуск, которым \
                     база обновлялась, откатите базы к копии bak-v<N> (zam-server rollback --to \
                     v<N>) или восстановите копию (zam-server restore)",
                    list.join(", ")
                ),
                json!({ "path": path, "checksums": "changed", "versions": vs }),
            );
        }
        Err(e) => {
            out.push(
                Level::Error,
                code,
                format!("суммы миграций: {e}"),
                data.clone(),
            );
        }
    }
    match ro.schema_diff(migrations) {
        Ok(d) if d.is_empty() => {}
        Ok(d) => {
            out.push(
                Level::Error,
                code,
                format!(
                    "схема расходится с эталоном v{version} ({} различий): {} — восстановите \
                     копию (zam-server restore) или откатите (zam-server rollback)",
                    d.len(),
                    d.iter().take(FIRST).cloned().collect::<Vec<_>>().join("; ")
                ),
                json!({ "path": path, "schema_diff": d }),
            );
        }
        Err(e) => {
            out.push(
                Level::Error,
                code,
                format!("сверка схемы: {e}"),
                data.clone(),
            );
        }
    }
    match ro.fts_check() {
        Ok(rows) if rows.is_empty() => {}
        Ok(rows) => out.push(
            Level::Error,
            code,
            format!(
                "FTS5: {} — исправит doctor --fix (rebuild)",
                rows.iter()
                    .take(FIRST)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            json!({ "path": path, "fts": rows, "tables": fts_tables(&rows) }),
        ),
        Err(e) => out.push(Level::Error, code, format!("FTS5: {e}"), data.clone()),
    }
    if !out.checks[start..].iter().any(|c| c.level >= Level::Warn) {
        out.push(
            Level::Ok,
            code,
            format!("v{version}, {what} ok, суммы и схема сверены"),
            json!({ "path": path, "version": version }),
        );
    }
    Some(ro)
}

/// Таблицы FTS5 из строк [`RoDb::fts_check`] (`таблица: проблема`).
fn fts_tables(rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| r.split_once(": ").map(|(t, _)| t.to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

// ---------- Vault, Qdrant, модели, копии ----------

fn vault_key(cfg: &ServerConfig, sys: Option<&RoDb>, env_key: Option<&str>, out: &mut Out) {
    let Some(sys) = sys else {
        return;
    };
    let code = "vault_key";
    use crate::vault::keys::VaultKeyState as K;
    match crate::vault::keys::key_state_in(cfg, sys.conn(), env_key) {
        Ok(K::NoKeys) => out.push(
            Level::Ok,
            code,
            "ключей Vault нет",
            json!({"state": "no_keys"}),
        ),
        Ok(K::Fits) => out.push(
            Level::Ok,
            code,
            "мастер-ключ подходит",
            json!({"state": "fits"}),
        ),
        Ok(s @ K::Missing) => out.push(
            Level::Error,
            code,
            s.warning(cfg).unwrap_or_default(),
            json!({ "state": "missing" }),
        ),
        Ok(s @ K::Mismatch) => out.push(
            Level::Error,
            code,
            s.warning(cfg).unwrap_or_default(),
            json!({ "state": "mismatch" }),
        ),
        Err(e) => out.push(Level::Error, code, format!("ключи Vault: {e}"), Value::Null),
    }
}

/// `qdrant`: режим и (управляемый) бинарник, хранилище, отметка пересборки; базы в
/// `index_skipped`.
async fn qdrant(cfg: &ServerConfig, sys: Option<&RoDb>, out: &mut Out) {
    use zam_index::supervisor::SupervisorConfig;
    let code = "qdrant";
    let exe_dir = crate::qdrant::exe_dir().unwrap_or_default();
    match cfg.qdrant.supervisor_config(&exe_dir) {
        Err(e) => out.push(Level::Warn, code, format!("конфигурация: {e}"), Value::Null),
        Ok(None) => out.push(
            Level::Info,
            code,
            "режим edge (процесс не запускается)",
            json!({ "mode": "edge" }),
        ),
        Ok(Some(SupervisorConfig::External(x))) => out.push(
            Level::Info,
            code,
            format!("внешний Qdrant {}", x.grpc_url),
            json!({ "mode": "external", "grpc_url": x.grpc_url }),
        ),
        Ok(Some(SupervisorConfig::Managed(m))) => {
            let st = zam_index::supervisor::inspect(&cfg.data_dir, m.http_port).await;
            let present = m.binary.is_file();
            out.push(
                if present { Level::Ok } else { Level::Warn },
                code,
                if present {
                    format!("управляемый, бинарник {}", m.binary.display())
                } else {
                    format!(
                        "управляемый, бинарника нет ({}) — zam-server qdrant fetch; без него \
                         поиск без векторов",
                        m.binary.display()
                    )
                },
                json!({
                    "mode": "managed",
                    "binary": m.binary,
                    "binary_present": present,
                    "storage_present": st.storage_present,
                    "reindex_pending": st.reindex_pending,
                }),
            );
            if st.reindex_pending {
                out.push(
                    Level::Info,
                    code,
                    "отметка .zam-reindex: векторный индекс пересоберётся при запуске",
                    Value::Null,
                );
            }
        }
    }
    let Some(sys) = sys else {
        return;
    };
    let skipped: Vec<(String, String)> = sys
        .conn()
        .prepare("SELECT user_id, error FROM index_skipped ORDER BY user_id")
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()
        })
        .unwrap_or_default();
    for (user, error) in skipped {
        out.push(
            Level::Warn,
            code,
            format!("база {user} исключена из пересборки векторов: {error}"),
            json!({ "user": user, "error": error }),
        );
    }
}

/// `models`: модель эмбеддингов набора сервера (как `models status`).
fn models(cfg: &ServerConfig, full: bool, out: &mut Out) {
    let code = "models";
    let m = match crate::models::load_manifest(None) {
        Ok(m) => m,
        Err(e) => {
            out.push(
                Level::Warn,
                code,
                format!("манифест моделей: {e}"),
                Value::Null,
            );
            return;
        }
    };
    let device = crate::models::default_device(cfg);
    let embed = match crate::models::plan(&m, cfg, device, false, &[]) {
        Ok(list) => list.into_iter().next(),
        Err(e) => {
            out.push(
                Level::Warn,
                code,
                format!("модель эмбеддингов: {e}"),
                Value::Null,
            );
            return;
        }
    };
    let Some(e) = embed else {
        return;
    };
    let dir = cfg.ml.models_dir(&cfg.data_dir);
    let (level, text) = match zam_fetch::model_state(e, &dir, full) {
        zam_fetch::ModelState::Installed => (
            Level::Ok,
            format!(
                "модель эмбеддингов {}: установлена{}",
                e.id,
                if full {
                    " (SHA-256 сверены)"
                } else {
                    ""
                }
            ),
        ),
        zam_fetch::ModelState::Missing => (
            Level::Warn,
            format!(
                "модели эмбеддингов {} нет в {} — поиск без векторов: zam-server models fetch",
                e.id,
                dir.display()
            ),
        ),
        zam_fetch::ModelState::Broken(why) => (
            Level::Warn,
            format!(
                "модель эмбеддингов {} неполная ({why}) — поиск без векторов: zam-server models \
                 fetch",
                e.id
            ),
        ),
        zam_fetch::ModelState::Stale(why) => (
            Level::Info,
            format!(
                "модель эмбеддингов {}: {why} — zam-server models fetch --id {}",
                e.id, e.id
            ),
        ),
    };
    out.push(level, code, text, json!({ "model": e.id, "dir": dir }));
}

/// `backup`: возраст последней копии против условия `no_recent_backup` (как `GET /index/status`).
fn backup(cfg: &ServerConfig, now: Timestamp, out: &mut Out) {
    let st = crate::backup::status_json(cfg, now);
    let last = st["last_at"].as_str().map(str::to_string);
    if st["warning"] == "no_recent_backup" {
        out.push(
            Level::Warn,
            "backup",
            match &last {
                Some(t) => format!(
                    "последняя копия {t} старше backup.warn_days = {} (no_recent_backup): \
                     zam-server backup",
                    cfg.backup.warn_days
                ),
                None => "копий нет (no_recent_backup): zam-server backup".to_string(),
            },
            st,
        );
    } else {
        out.push(
            Level::Ok,
            "backup",
            format!("последняя копия {}", last.unwrap_or_default()),
            st,
        );
    }
}

// ---------- по пользователю ----------

/// Корень блобов пользователя.
fn blob_root(cfg: &ServerConfig, user: &str) -> PathBuf {
    cfg.data_dir.join("blobs").join(user)
}

/// `blobs:<user>`: строки без файла, мусор к удалению, `.incoming`, остатки `.gc/`.
fn blobs(cfg: &ServerConfig, user: &str, ro: &RoDb, now: Timestamp, out: &mut Out) {
    let code = format!("blobs:{user}");
    let days = cfg.limits.blob_gc_days;
    let p = match zam_store::blob_gc::plan(ro.conn(), &blob_root(cfg, user), now, days) {
        Ok(p) => p,
        Err(e) => {
            out.push(
                Level::Error,
                &code,
                format!("блобы не сверены: {e}"),
                Value::Null,
            );
            return;
        }
    };
    let mut quiet = true;
    if !p.missing.is_empty() {
        quiet = false;
        let in_gc = p.missing.iter().filter(|m| m.in_gc).count();
        let first: Vec<&str> = p
            .missing
            .iter()
            .take(FIRST)
            .map(|m| short(&m.sha256))
            .collect();
        out.push(
            Level::Error,
            &code,
            format!(
                "строк блобов без файла: {} (первые: {}){} — вложения не отдаются; восстановите \
                 файлы из копии (zam-server restore)",
                p.missing.len(),
                first.join(", "),
                if in_gc > 0 {
                    format!("; {in_gc} из них в .gc/ — вернёт doctor --fix")
                } else {
                    String::new()
                }
            ),
            json!({ "missing": p.missing }),
        );
    }
    let leftovers = p.leftovers.len();
    if leftovers > 0 {
        quiet = false;
        out.push(
            Level::Info,
            &code,
            format!(
                ".gc/: {leftovers} файлов после сбоя посреди удаления — разберёт doctor --fix \
                 или следующий проход сборщика"
            ),
            json!({ "leftovers": p.leftovers }),
        );
    }
    if days == 0 {
        out.push(
            Level::Info,
            &code,
            "сборщик мусора выключен (limits.blob_gc_days = 0)",
            Value::Null,
        );
    } else if p.blobs() > 0 {
        quiet = false;
        out.push(
            Level::Info,
            &code,
            format!(
                "к удалению сборщиком: {} блобов ({}) старше {days} суток без ссылок — проход \
                 раз в сутки или doctor --fix",
                p.blobs(),
                mib(p.rows.iter().chain(&p.orphans).map(|i| i.bytes).sum())
            ),
            json!({
                "rows": p.rows.len(),
                "orphans": p.orphans.len(),
                "dangling": p.dangling.len(),
            }),
        );
    }
    if p.incoming_files > 0 {
        quiet = false;
        out.push(
            Level::Info,
            &code,
            format!(
                ".incoming: {} файлов ({}), из них старше суток: {} — удалит doctor --fix",
                p.incoming_files,
                mib(p.incoming_bytes),
                p.parts.len()
            ),
            json!({ "files": p.incoming_files, "bytes": p.incoming_bytes, "stale": p.parts.len() }),
        );
    }
    if quiet {
        let (n, bytes): (i64, i64) = ro
            .conn()
            .query_row(
                "SELECT count(*), coalesce(sum(size), 0) FROM blobs WHERE refcount > 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_default();
        out.push(
            Level::Ok,
            &code,
            format!(
                "блобов со ссылками: {n} ({}), файлы на месте",
                mib(bytes.max(0) as u64)
            ),
            json!({ "blobs": n, "bytes": bytes }),
        );
    }
}

/// `links:<user>`: явные и системные рёбра живых источников к удалённой или несуществующей цели
/// базы (`warn`), неразрешённые wikilink (`info`).
fn links(user: &str, ro: &RoDb, out: &mut Out) {
    let code = format!("links:{user}");
    const BROKEN: &str = "FROM edges e
         WHERE e.deleted_at IS NULL AND e.origin IN ('explicit', 'system') AND e.dst_id IS NOT NULL
           AND (e.dst_space_id IS NULL OR e.dst_space_id IN (SELECT id FROM spaces))
           AND CASE e.src_kind
                 WHEN 'node' THEN EXISTS (SELECT 1 FROM nodes n
                                          WHERE n.id = e.src_id AND n.deleted_at IS NULL)
                 ELSE EXISTS (SELECT 1 FROM entries x WHERE x.id = e.src_id AND x.deleted_at IS NULL)
               END
           AND NOT CASE e.dst_kind
                 WHEN 'node' THEN EXISTS (SELECT 1 FROM nodes n
                                          WHERE n.id = e.dst_id AND n.deleted_at IS NULL)
                 ELSE EXISTS (SELECT 1 FROM entries x WHERE x.id = e.dst_id AND x.deleted_at IS NULL)
               END";
    let c = ro.conn();
    let broken: Result<(i64, Vec<String>), rusqlite::Error> = (|| {
        let n: i64 = c.query_row(&format!("SELECT count(*) {BROKEN}"), [], |r| r.get(0))?;
        let mut st = c.prepare(&format!(
            "SELECT e.src_kind, e.src_id, e.type, e.dst_kind, e.dst_id {BROKEN}
             ORDER BY e.created_at, e.id LIMIT {FIRST}"
        ))?;
        let first = st
            .query_map([], |r| {
                Ok(format!(
                    "{} {} —{}→ {} {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    r.get::<_, String>(4)?
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok((n, first))
    })();
    let unresolved: i64 = c
        .query_row(
            "SELECT count(*) FROM edges WHERE deleted_at IS NULL AND dst_id IS NULL
               AND origin IN ('wikilink', 'frontmatter')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    match broken {
        Err(e) => out.push(
            Level::Warn,
            &code,
            format!("связи не сверены: {e}"),
            Value::Null,
        ),
        Ok((0, _)) => out.push(
            Level::Ok,
            &code,
            "явные и системные связи указывают на живые цели",
            json!({ "broken": 0 }),
        ),
        Ok((n, first)) => out.push(
            Level::Warn,
            &code,
            format!(
                "связей к удалённой или несуществующей цели: {n} (первые: {})",
                first.join("; ")
            ),
            json!({ "broken": n, "first": first }),
        ),
    }
    if unresolved > 0 {
        out.push(
            Level::Info,
            &code,
            format!("неразрешённых wikilink: {unresolved} (цели ещё нет)"),
            json!({ "unresolved": unresolved }),
        );
    }
}

/// Действующая секция `[works]`, как у сервера (M7.6 §9.9): файл конфигурации и значения из
/// WebUI администратора (строка `works.server` в `settings` `system.db`); строки нет или она не
/// читается — файл.
fn works_effective(cfg: &ServerConfig, sys: Option<&RoDb>) -> WorksSection {
    use crate::works::server_settings::{SETTING_KEY, Stored, effective};
    let row: Option<Value> = sys.and_then(|s| {
        s.conn()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [SETTING_KEY],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
    });
    effective(&cfg.works, &Stored::from_row(row.as_ref()).values).0
}

/// `check:<user>`: последний законченный ночной прогон — открытая регрессия (`warn`); при
/// `works.check = nightly` и активных пробах нет законченного прогона двое суток (`warn`).
fn check(works: &WorksSection, user: &str, ro: &RoDb, now: Timestamp, out: &mut Out) {
    let code = format!("check:{user}");
    let c = ro.conn();
    let has_table: bool = c
        .query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'check_runs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if !has_table {
        return;
    }
    let last = match zam_store::check::nightly_done_in(c, 1) {
        Ok(mut v) => v.pop(),
        Err(e) => {
            out.push(
                Level::Warn,
                &code,
                format!("прогоны не прочитаны: {e}"),
                Value::Null,
            );
            return;
        }
    };
    let probes: i64 = c
        .query_row(
            "SELECT count(*) FROM probes WHERE status = 'active'",
            [],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0);
    let mut said = false;
    if let Some(run) = &last
        && run.verdict_kind() == Some("regression")
        && run.accepted_at.is_none()
    {
        said = true;
        out.push(
            Level::Warn,
            &code,
            format!(
                "открытая регрессия ночной проверки (прогон {}, ночь {}): {} — WebUI «Проверка \
                 памяти»",
                run.id,
                run.night.map(|n| n.to_string()).unwrap_or_default(),
                run.verdict
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_default()
            ),
            json!({ "run": run.id, "verdict": run.verdict }),
        );
    }
    if works.check == CheckMode::Nightly
        && probes > 0
        && last
            .as_ref()
            .is_none_or(|r| now.duration_since(r.started_at) > CHECK_STALE)
    {
        said = true;
        out.push(
            Level::Warn,
            &code,
            format!(
                "ночная проверка включена, а законченного ночного прогона нет двое суток \
                 (последний: {})",
                last.as_ref()
                    .map(|r| r.started_at.to_string())
                    .unwrap_or_else(|| "нет".into())
            ),
            json!({ "last": last.as_ref().map(|r| r.started_at.to_string()) }),
        );
    }
    if !said && let Some(run) = &last {
        out.push(
            Level::Ok,
            &code,
            format!(
                "последний ночной прогон {}: {}",
                run.started_at,
                run.verdict_kind().unwrap_or("без вердикта")
            ),
            json!({ "run": run.id }),
        );
    }
}

// ---------- --fix ----------

/// Безопасные исправления на остановленном сервере (лок каталога у вызывающего).
async fn fix(cfg: &ServerConfig, now: Timestamp, out: &mut Out) {
    let root = cfg.data_dir.as_path();
    let staging = root.join(crate::backup::restore::STAGING);
    if staging.exists() {
        match std::fs::remove_dir_all(&staging) {
            Ok(()) => out.push(
                Level::Info,
                "fix:data_dir",
                format!("удалены остатки {}", crate::backup::restore::STAGING),
                Value::Null,
            ),
            Err(e) => out.push(
                Level::Error,
                "fix:data_dir",
                format!("{} не удалён: {e}", crate::backup::restore::STAGING),
                Value::Null,
            ),
        }
    }
    if let zam_store::backup::FlagState::Stale(_) =
        zam_store::backup::backup_flag_state(root, cfg.backup.flag_max_age())
    {
        let flag = zam_store::backup_flag_path(root);
        // только признак мёртвого процесса: лок признака свободен
        let free = std::fs::File::open(&flag)
            .ok()
            .is_some_and(|f| fs4::FileExt::try_lock(&f).is_ok());
        if free && std::fs::remove_file(&flag).is_ok() {
            out.push(
                Level::Info,
                "fix:data_dir",
                "удалён брошенный признак копии .zam-backup",
                Value::Null,
            );
        }
    }
    let days = cfg.limits.blob_gc_days;
    let mut total = (0u64, 0u64, 0u64, 0u64);
    for (user, path) in user_dbs(root) {
        let code = format!("fix:db:{user}");
        let broot = blob_root(cfg, &user);
        // (проверена начисто, таблицы FTS5 с ошибкой, есть ли дело сборщику)
        let (sound, tables, pending) = match open_ro(&path) {
            Ok(ro) => {
                let v = ro.user_version().ok();
                let sound = v == Some(USER_MIGRATIONS.len() as u32)
                    && matches!(
                        ro.checksum_status(USER_MIGRATIONS),
                        Ok(ChecksumStatus::Ok | ChecksumStatus::AdoptedPending)
                    )
                    && ro.schema_diff(USER_MIGRATIONS).is_ok_and(|d| d.is_empty())
                    && ro.quick_check().is_ok_and(|r| r.is_empty());
                let pending = zam_store::blob_gc::plan(ro.conn(), &broot, now, days)
                    .is_ok_and(|p| p.blobs() > 0 || !p.parts.is_empty() || !p.leftovers.is_empty());
                (
                    sound,
                    ro.fts_check().map(|r| fts_tables(&r)).unwrap_or_default(),
                    pending,
                )
            }
            Err(_) => (false, Vec::new(), false),
        };
        if !tables.is_empty() {
            match fts_rebuild(&path, &tables) {
                Ok(()) => out.push(
                    Level::Info,
                    &code,
                    format!("FTS5 rebuild: {}", tables.join(", ")),
                    json!({ "tables": tables }),
                ),
                Err(e) => out.push(
                    Level::Error,
                    &code,
                    format!("FTS5 rebuild не выполнен: {e}"),
                    Value::Null,
                ),
            }
        }
        let code = format!("fix:blobs:{user}");
        if !sound {
            // базу старше программы или с ошибкой проверки сборщик не трогает: молча, если ему и
            // делать нечего
            if pending {
                out.push(
                    Level::Info,
                    &code,
                    "база не проверена начисто (версия, суммы, схема или quick_check) — \
                     сборщик, .part и разбор .gc/ пропущены",
                    Value::Null,
                );
            }
            continue;
        }
        match fix_blobs(&path, &broot, now, days).await {
            Ok(r) if r.is_empty() => {}
            Ok(r) => {
                total.0 += 1;
                total.1 += r.rows;
                total.2 += r.files;
                total.3 += r.bytes;
                out.push(
                    Level::Info,
                    &code,
                    format!(
                        "сборщик: строк {}, файлов {} ({}), из них .part {}; возвращено из .gc/: \
                         {}",
                        r.rows,
                        r.files,
                        mib(r.bytes),
                        r.parts,
                        r.restored
                    ),
                    serde_json::to_value(&r).unwrap_or_default(),
                );
            }
            Err(e) => out.push(Level::Error, &code, format!("сборщик: {e}"), Value::Null),
        }
    }
    let sys = root.join("system.db");
    if total.1 + total.2 > 0 && sys.is_file() {
        let details = json!({
            "users": total.0, "rows": total.1, "files": total.2, "bytes": total.3, "by": "doctor",
        });
        if let Err(e) = zam_store::system::audit_closed(&sys, "cli", "blobs.gc", &details) {
            out.push(
                Level::Warn,
                "fix:blobs",
                format!("аудит blobs.gc не записан: {e}"),
                Value::Null,
            );
        }
    }
}

/// FTS5 `rebuild` таблиц `tables` базы `path` (соединение без мигратора).
fn fts_rebuild(path: &Path, tables: &[String]) -> Result<(), StoreError> {
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    for t in tables {
        let q = t.replace('"', "\"\"");
        c.execute(
            &format!("INSERT INTO \"{q}\"(\"{q}\") VALUES ('rebuild')"),
            [],
        )?;
    }
    Ok(())
}

/// `.gc/` — строкам, `.part` старше суток, проход сборщика (база проверена начисто: версия
/// программы, суммы, схема). `days = 0` — без прохода: только `.gc/` и `.part`.
async fn fix_blobs(
    path: &Path,
    root: &Path,
    now: Timestamp,
    days: u32,
) -> Result<zam_store::GcReport, StoreError> {
    if days > 0 {
        let db = zam_store::Db::open(path, USER_MIGRATIONS)?;
        let r = zam_store::blob_gc::sweep(&db, root, now, days).await;
        drop(db);
        return r;
    }
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    let (restored, removed, rbytes) = zam_store::blob_gc::recover(&c, root)?;
    let (parts, pbytes) = zam_store::blob_gc::remove_stale_parts(root, now)?;
    Ok(zam_store::GcReport {
        files: removed + parts,
        bytes: rbytes + pbytes,
        parts,
        restored,
        ..Default::default()
    })
}
