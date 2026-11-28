//! `zam-server rollback --to v<N> [--system v<M>] [--dry-run] [--yes] [--json]` (M7.4 §7.2).
//!
//! Откат баз к копиям перед миграцией (`bak-v<N>`, их делает мигратор при обновлении схемы) на
//! остановленном сервере; **ничего не удаляется** (решение 102):
//! 1. **Что откатывается.** `--to v<N>` — базы пользователей `users/<id>.db` по копиям
//!    `users/<id>.db.bak-v<N>`; `system.db` — только с `--system v<M>` (номера схем у баз разные,
//!    решение 101). База уже на версии N — пропуск строкой; ниже N или копии нет — отказ.
//! 2. **Остановленный сервер.** Лок каталога данных с меткой `rollback`
//!    ([`zam_store::LockMark::Rollback`]); занят — [`BackupError::Locked`]; идёт копия, идёт или
//!    прервано восстановление — отказ.
//! 3. **Все копии проверяются до первого изменения** ([`zam_store::inspect::open_ro`]):
//!    `integrity_check` — `ok`, версия схемы — N (M), суммы `zam_migrations` копии совпадают с
//!    программой, сверка схемы с эталоном — без различий; любой сбой —
//!    [`BackupError::RollbackRefused`] со всеми причинами, каталог данных не тронут.
//! 4. **План** (и `--dry-run` — только он): базы, версии, время (`mtime`) и размер копии,
//!    размер текущей базы, строка «всё записанное после <время копии> теряется». Без `--yes` —
//!    план без изменений (команда — код 1 с подсказкой).
//! 5. **Применение.** Признак незавершённости [`zam_store::RESTORE_INCOMPLETE_FILE`] с текстом
//!    `rollback …` и именем каталога прежних баз (сервер и `backup` отказывают, пока он есть);
//!    текущие базы с `-wal`/`-shm` — переименованием в `backups/pre-rollback-<время>/`
//!    (раскладка `system.db`, `users/`, как у `restore --replace`); копия `bak-v<N>`
//!    **копируется** во временный файл рядом, `fsync`, переименование на место (копия остаётся
//!    для повтора); отметка «пересобрать векторы из кэша» (`meta.reindex_pending`) во всех базах
//!    пользователей и `.zam-reindex` в `qdrant/`; аудит `server.rollback {to, system, dbs}` в
//!    `system.db`; признак снимается последним. Базы пишутся без мигратора этой сборки
//!    (`set_reindex_pending_closed`, `audit_closed`) — он снова обновил бы откаченную схему.
//! 6. **Повтор после сбоя** — та же команда: каталог прежних баз — из признака; база, уже
//!    перенесённая туда, второй раз не переносится (кладётся недостающая копия), база, уже
//!    положенная прошлой попыткой, не трогается.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use zam_store::inspect::{ChecksumStatus, open_ro};
use zam_store::{DataDir, LockMark, Migration, SYSTEM_MIGRATIONS, StoreError, USER_MIGRATIONS};

use super::BackupError;
use crate::config::ServerConfig;

/// Префикс каталога прежних баз отката в `backups/`.
const PRE_ROLLBACK: &str = "pre-rollback-";

/// Параметры отката.
#[derive(Debug, Clone)]
pub struct RollbackOpts {
    /// Схема баз пользователей (`--to v<N>`).
    pub to: u32,
    /// Схема `system.db` (`--system v<M>`); `None` — `system.db` не трогается.
    pub system: Option<u32>,
    /// Только план.
    pub dry_run: bool,
    /// Подтверждение (`--yes`).
    pub yes: bool,
}

/// `v<N>` или `N` — номер схемы (от 1).
pub fn parse_version(s: &str) -> Result<u32, String> {
    let digits = s.strip_prefix('v').unwrap_or(s);
    match digits.parse::<u32>() {
        Ok(n) if n >= 1 && !digits.starts_with('+') => Ok(n),
        _ => Err(format!(
            "«{s}» — не номер схемы: укажите v<N> или N, например v26"
        )),
    }
}

/// Что делать с базой.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Перенести текущую базу и положить копию.
    Move,
    /// База уже перенесена прошлой попыткой — положить копию.
    Place,
    /// Копия уже положена прошлой попыткой.
    Done,
}

/// База плана.
#[derive(Debug, Clone, Serialize)]
pub struct PlanItem {
    /// Путь в каталоге данных (`users/<id>.db`, `system.db`).
    pub db: String,
    /// Текущая версия схемы.
    pub from: u32,
    /// Версия копии.
    pub to: u32,
    /// Копия в каталоге данных (`users/<id>.db.bak-v<N>`).
    pub copy: String,
    /// Время копии (`mtime` файла).
    pub copy_time: Option<jiff::Timestamp>,
    /// Размер копии, байт.
    pub copy_bytes: u64,
    /// Размер текущей базы с `-wal`, байт (0 — уже перенесена).
    pub current_bytes: u64,
    pub step: Step,
}

/// База без изменений: уже на схеме отката.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub db: String,
    pub version: u32,
}

/// План отката.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RollbackPlan {
    pub items: Vec<PlanItem>,
    pub skipped: Vec<Skipped>,
    /// Продолжение прерванного отката (признак `rollback …`).
    pub resume: bool,
}

/// Время для человека.
fn when(t: Option<jiff::Timestamp>) -> String {
    t.map_or_else(|| "?".into(), |t| t.to_string())
}

impl RollbackPlan {
    /// Самая ранняя копия плана: всё записанное после неё теряется.
    pub fn lost_after(&self) -> Option<jiff::Timestamp> {
        self.items.iter().filter_map(|i| i.copy_time).min()
    }

    /// План для человека (несколько строк).
    pub fn text(&self) -> String {
        if self.items.is_empty() && self.skipped.is_empty() {
            return "откатывать нечего: баз нет".into();
        }
        let mut s = String::from("план отката:");
        for i in &self.items {
            let what = match i.step {
                Step::Move => format!("сейчас {} байт", i.current_bytes),
                Step::Place => "уже перенесена прошлой попыткой".into(),
                Step::Done => "копия уже положена прошлой попыткой".into(),
            };
            s.push_str(&format!(
                "\n  {}: v{} → v{} — копия {} от {}, {} байт ({what})",
                i.db,
                i.from,
                i.to,
                i.copy,
                when(i.copy_time),
                i.copy_bytes
            ));
        }
        for k in &self.skipped {
            s.push_str(&format!(
                "\n  {}: уже на схеме v{} — пропуск",
                k.db, k.version
            ));
        }
        if self.items.is_empty() {
            s.push_str("\nоткатывать нечего: все базы уже на схеме отката");
        } else {
            s.push_str(&format!(
                "\nвсё записанное в эти базы после {} теряется; прежние базы переносятся в \
                 backups/{PRE_ROLLBACK}<время>/ (ничего не удаляется), векторы всех баз \
                 пересоберутся из кэша при первом запуске",
                when(self.lost_after())
            ));
        }
        s
    }
}

/// Итог отката.
#[derive(Debug, Clone, Serialize)]
pub struct RollbackReport {
    pub to: u32,
    pub system: Option<u32>,
    pub dry_run: bool,
    pub plan: RollbackPlan,
    /// Откат выполнен (иначе — только план).
    pub applied: bool,
    /// Каталог прежних баз.
    pub moved_to: Option<PathBuf>,
}

impl RollbackReport {
    /// Нужно подтверждение `--yes`: план не пуст, а откат не выполнен и это не `--dry-run`.
    pub fn needs_yes(&self) -> bool {
        !self.applied && !self.dry_run && (!self.plan.items.is_empty() || self.plan.resume)
    }

    /// Итог для человека: план (не выполнен) или строки по базам и что делать дальше.
    pub fn text(&self) -> String {
        if !self.applied {
            return self.plan.text();
        }
        let mut s = String::from("откат выполнен:");
        for i in &self.plan.items {
            s.push_str(&format!("\n  {}: v{} → v{}", i.db, i.from, i.to));
        }
        for k in &self.plan.skipped {
            s.push_str(&format!(
                "\n  {}: уже на схеме v{} — без изменений",
                k.db, k.version
            ));
        }
        if let Some(p) = &self.moved_to {
            s.push_str(&format!("\nпрежние базы — в {}", p.display()));
        }
        s.push_str("\nвекторы всех баз пересоберутся из кэша при первом запуске");
        let sys = self
            .system
            .map(|m| format!(" (system.db — v{m})"))
            .unwrap_or_default();
        s.push_str(&format!(
            "\nзамените файлы комплекта на выпуск со схемой v{}{sys} до запуска: этот zam-server \
             при запуске снова обновит базы",
            self.to
        ));
        s
    }
}

/// Номер схемы в пределах программы.
fn check_target(n: u32, list: &[Migration], flag: &str) -> Result<(), BackupError> {
    if n == 0 || n as usize > list.len() {
        return Err(BackupError::RollbackRefused(format!(
            "{flag} v{n}: у этой программы схемы v1…v{}",
            list.len()
        )));
    }
    Ok(())
}

/// Базы `*.db` каталога `dir` (имена файлов по порядку).
fn list_dbs(dir: &Path) -> BTreeSet<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    rd.flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".db"))
        .collect()
}

/// Версия схемы файла базы — только чтением.
fn version_of(p: &Path) -> Result<u32, StoreError> {
    open_ro(p)?.user_version()
}

/// Проверка копии до первого изменения (§7.2 п. 3); `Err` — причина для человека.
fn verify_copy(copy: &Path, list: &[Migration], n: u32) -> Result<(), String> {
    let e = |e: StoreError| e.to_string();
    let ro = open_ro(copy).map_err(|e| format!("не открывается: {e}"))?;
    let v = ro.user_version().map_err(e)?;
    if v != n {
        return Err(format!("версия схемы копии v{v}, а нужна v{n}"));
    }
    let problems = ro.integrity_check().map_err(e)?;
    if !problems.is_empty() {
        return Err(format!("integrity_check: {}", problems.join("; ")));
    }
    if let ChecksumStatus::Changed(vs) = ro.checksum_status(list).map_err(e)? {
        return Err(format!(
            "суммы миграций {vs:?} в копии расходятся с этой программой"
        ));
    }
    let diff = ro.schema_diff(list).map_err(e)?;
    if !diff.is_empty() {
        return Err(format!(
            "схема расходится с эталоном v{n}: {}",
            diff.join("; ")
        ));
    }
    Ok(())
}

/// Размер файла и его `-wal`.
fn db_bytes(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len()) + zam_store::backup::wal_size(p)
}

/// План: каждая база проверяется только чтением; причины отказа копятся в `problems`.
struct Planner<'a> {
    root: &'a Path,
    prev: Option<&'a Path>,
    plan: RollbackPlan,
    problems: Vec<String>,
}

impl Planner<'_> {
    fn check(&mut self, rel: &str, list: &[Migration], n: u32) {
        let cur = self.root.join(rel);
        let old = self.prev.map(|p| p.join(rel)).filter(|p| p.is_file());
        let (from, step) = if cur.is_file() {
            match version_of(&cur) {
                Ok(v) if v == n && old.is_some() => {
                    // положена прошлой попыткой: версия — у перенесённой базы
                    let from = old.as_deref().map_or(Ok(n), version_of).unwrap_or(n);
                    (from, Step::Done)
                }
                Ok(v) if v == n => {
                    self.plan.skipped.push(Skipped {
                        db: rel.to_string(),
                        version: n,
                    });
                    return;
                }
                Ok(v) => (v, Step::Move),
                Err(e) => {
                    self.problems
                        .push(format!("{rel}: не открывается для чтения: {e}"));
                    return;
                }
            }
        } else if let Some(o) = &old {
            match version_of(o) {
                Ok(v) => (v, Step::Place),
                Err(e) => {
                    self.problems.push(format!(
                        "{rel} (перенесена в {}): не открывается для чтения: {e}",
                        o.display()
                    ));
                    return;
                }
            }
        } else {
            if rel == "system.db" {
                self.problems.push("system.db: нет файла".into());
            }
            return;
        };
        if from < n {
            self.problems.push(format!(
                "{rel}: схема v{from} ниже v{n} — откат только к более старой схеме"
            ));
            return;
        }
        let copy_rel = format!("{rel}.bak-v{n}");
        let copy = self.root.join(&copy_rel);
        if step != Step::Done {
            if !copy.is_file() {
                self.problems.push(format!(
                    "{rel}: нет копии {copy_rel} (её делает мигратор при обновлении со схемы v{n})"
                ));
                return;
            }
            if let Err(why) = verify_copy(&copy, list, n) {
                self.problems.push(format!("{copy_rel}: {why}"));
                return;
            }
        }
        let md = std::fs::metadata(&copy).ok();
        self.plan.items.push(PlanItem {
            db: rel.to_string(),
            from,
            to: n,
            copy: copy_rel,
            copy_time: md
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| jiff::Timestamp::try_from(t).ok()),
            copy_bytes: md.map_or(0, |m| m.len()),
            current_bytes: if step == Step::Move {
                db_bytes(&cur)
            } else {
                0
            },
            step,
        });
    }
}

/// План отката — только чтение.
fn make_plan(
    root: &Path,
    opts: &RollbackOpts,
    prev: Option<&Path>,
    resume: bool,
) -> Result<RollbackPlan, BackupError> {
    let mut names = list_dbs(&root.join("users"));
    if let Some(p) = prev {
        names.extend(list_dbs(&p.join("users")));
    }
    let mut pl = Planner {
        root,
        prev,
        plan: RollbackPlan {
            resume,
            ..RollbackPlan::default()
        },
        problems: Vec::new(),
    };
    for name in &names {
        pl.check(&format!("users/{name}"), USER_MIGRATIONS, opts.to);
    }
    if let Some(m) = opts.system {
        pl.check("system.db", SYSTEM_MIGRATIONS, m);
    }
    if !pl.problems.is_empty() {
        return Err(BackupError::RollbackRefused(pl.problems.join("\n")));
    }
    Ok(pl.plan)
}

/// Копирует `src` в `dst` с `fsync`.
fn copy_synced(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::copy(src, dst)?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(dst)?
        .sync_all()
}

/// Каталог `<rel>` без имени файла внутри `base` (`users/x.db` → `base/users`).
fn dir_of(base: &Path, rel: &str) -> PathBuf {
    match Path::new(rel).parent() {
        Some(p) if !p.as_os_str().is_empty() => base.join(p),
        _ => base.to_path_buf(),
    }
}

/// Шаг 5 (после признака): перенос, копии на место, отметки пересборки, аудит.
fn apply(
    root: &Path,
    plan: &RollbackPlan,
    pre: &Path,
    opts: &RollbackOpts,
) -> Result<(), BackupError> {
    std::fs::create_dir_all(pre)?;
    let remnants = pre.join(format!("rollback-remnants-{}", super::ts_name()));
    // 1. текущие базы с `-wal`/`-shm` — в каталог прежних баз; у перенесённых прошлой попыткой —
    // оставшиеся на месте `-wal`/`-shm` (чужой WAL рядом с копией испортил бы её)
    for it in plan.items.iter().filter(|i| i.step != Step::Done) {
        let cur = root.join(&it.db);
        let to_dir = dir_of(pre, &it.db);
        let rem_dir = dir_of(&remnants, &it.db);
        for s in ["", "-wal", "-shm"] {
            let p = super::side(&cur, s);
            if p.exists() {
                super::move_into(&p, &to_dir, &rem_dir)?;
            }
        }
        if to_dir.is_dir() {
            super::sync_dir(&to_dir)?;
        }
    }
    super::fail_point(root, "rollback_moved")?;
    // 2. копии `bak-v<N>` — копированием во временный файл рядом и переименованием на место
    let mut first = true;
    for it in plan.items.iter().filter(|i| i.step != Step::Done) {
        let cur = root.join(&it.db);
        let tmp = super::side(&cur, ".rollback-tmp");
        copy_synced(&root.join(&it.copy), &tmp)?;
        super::rename_retry(&tmp, &cur)?;
        super::sync_dir(&dir_of(root, &it.db))?;
        if first {
            super::fail_point(root, "rollback_placed")?;
            first = false;
        }
    }
    // 3. векторы всех баз — из кэша при первом запуске (точки Qdrant — от новых баз)
    let spec = serde_json::json!({ "full": false, "space": null });
    for name in list_dbs(&root.join("users")) {
        zam_store::jobs::set_reindex_pending_closed(&root.join("users").join(name), &spec)?;
    }
    zam_index::supervisor::mark_storage_reindex(&root.join("qdrant"), "zam-server rollback")?;
    // 4. аудит — в system.db после замены
    let dbs: Vec<&str> = plan.items.iter().map(|i| i.db.as_str()).collect();
    zam_store::system::audit_closed(
        &root.join("system.db"),
        "cli",
        "server.rollback",
        &serde_json::json!({
            "to": opts.to,
            "system": opts.system,
            "dbs": dbs,
            "moved_to": pre.file_name().and_then(|n| n.to_str()),
        }),
    )?;
    Ok(())
}

/// Откат каталога данных `cfg.data_dir` на остановленном сервере (блокирующая; вызывать вне
/// потоков runtime). Без `--yes` и с `--dry-run` — только план (`applied: false`).
pub fn rollback(cfg: &ServerConfig, opts: &RollbackOpts) -> Result<RollbackReport, BackupError> {
    check_target(opts.to, USER_MIGRATIONS, "--to")?;
    if let Some(m) = opts.system {
        check_target(m, SYSTEM_MIGRATIONS, "--system")?;
    }
    let root = cfg.data_dir.clone();
    if !root.join("system.db").is_file() && !zam_store::rollback_incomplete(&root) {
        return Err(BackupError::NoData(root));
    }
    let dd = match DataDir::open_for_rollback(&root) {
        Ok(d) => d,
        Err(StoreError::DataDirLocked(_)) => {
            return Err(match zam_store::lock_mark(&root) {
                Some(LockMark::Restore) => BackupError::RestoreInProgress,
                Some(LockMark::Rollback) => BackupError::RollbackInProgress,
                _ => BackupError::Locked,
            });
        }
        Err(e) => return Err(e.into()),
    };
    if zam_store::backup_flag_active(&root, cfg.backup.flag_max_age()) {
        return Err(BackupError::BackupInProgress);
    }
    let resume = zam_store::rollback_incomplete(&root);
    if zam_store::restore_incomplete(&root) && !resume {
        return Err(StoreError::RestoreIncomplete(root).into());
    }
    let prev = if resume {
        super::incomplete_moved_to(&root, PRE_ROLLBACK)
    } else {
        None
    };
    let plan = make_plan(&root, opts, prev.as_deref(), resume)?;
    let mut report = RollbackReport {
        to: opts.to,
        system: opts.system,
        dry_run: opts.dry_run,
        plan,
        applied: false,
        moved_to: None,
    };
    if opts.dry_run || !opts.yes || (report.plan.items.is_empty() && !resume) {
        return Ok(report);
    }
    // место под копии (текущие базы переносятся переименованием)
    let need = super::with_margin(
        report
            .plan
            .items
            .iter()
            .filter(|i| i.step != Step::Done)
            .map(|i| i.copy_bytes)
            .sum(),
    );
    let free = super::free_space(&root, &root)?;
    if need > free {
        return Err(BackupError::NoSpace { need, free });
    }
    let pre = prev.unwrap_or_else(|| super::new_pre_dir(&root, PRE_ROLLBACK));
    super::mark_incomplete(&root, "rollback", Some(&pre))?;
    let failed = |e: &dyn std::fmt::Display| BackupError::RollbackFailed {
        reason: e.to_string(),
        moved_to: pre.clone(),
    };
    apply(&root, &report.plan, &pre, opts).map_err(|e| failed(&e))?;
    // всё на месте: признак снимается последним
    std::fs::remove_file(root.join(zam_store::RESTORE_INCOMPLETE_FILE)).map_err(|e| failed(&e))?;
    super::sync_dir(&root).map_err(|e| failed(&e))?;
    drop(dd);
    report.applied = true;
    report.moved_to = Some(pre);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_forms() {
        assert_eq!(parse_version("v7"), Ok(7));
        assert_eq!(parse_version("27"), Ok(27));
        for bad in ["", "v", "v0", "0", "x1", "v+3", "-1", "v1.5"] {
            assert!(parse_version(bad).is_err(), "{bad}");
        }
    }
}
