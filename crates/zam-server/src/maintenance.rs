//! Воркер обслуживания (§6.3): периодические задачи, не связанные с индексацией.
//!
//! Сейчас — срок хранения сессий агентов: раз в [`PERIOD`] (первый проход — через
//! [`FIRST_RUN`] после старта) в каждой базе пользователя удаляются сессии без обращений
//! дольше [`SESSION_TTL`] (30 дней от `last_seen_at`) вместе с выданным им (`session_seen`);
//! `usage` хранится бессрочно; срок жизни кэша выжимок (§9.4): выжимки без обращений дольше
//! `cache.ttl_days` и сверх `cache.max_entries` на базу (LRU) удаляются, превращённые в записи
//! — нет; снимки сессий (M6 §8а) старше `snapshot.keep_days` от `taken_at` удаляются
//! ([`gc_snapshots`]); журнал отправок подключённой модели (`work_calls`, M7.3) — 180 суток от
//! `ts` ([`gc_work_calls`]); прогоны ночной проверки памяти (`check_runs` с их `check_results`,
//! M7.3) — 365 суток от `started_at` ([`gc_check_runs`]); закрытые внутренние работы (`works`,
//! M7.3) — 365 суток от `finished_at` ([`gc_works`]: столько же итоги работ читает отсев
//! кандидатов). Работает и без модели (в отличие от воркера индексации).
//!
//! Автокопии (M6 §12б.4): раз в [`BACKUP_TICK`] — решение [`crate::backup::schedule::tick`]
//! по `[backup]`; копию снимает тот же код, что `zam-server backup` (в блокирующем потоке),
//! копии сверх `keep` удаляются, начиная с самых старых. С M7.2 (§5.6) шаг идёт под замком
//! [`crate::backup::runner::BackupRunner`], общим со страницей «Копии» WebUI: замок занят —
//! шаг пропускается целиком до следующей минуты (ни копии, ни удаления, ни отметки попытки).
//!
//! Сборщик мусора блобов (M7.4 §7.4): раз в [`PERIOD`], первый проход — через
//! [`BLOB_GC_FIRST_RUN`] после старта, при `limits.blob_gc_days > 0` — [`gc_blobs`] по всем
//! пользователям ([`zam_store::blob_gc::sweep`]). Проход пропускается целиком, пока активен
//! признак копии или занят замок копий [`BackupRunner`]; пользователь, чьи блобы сейчас пишутся
//! (замок блобов занят), — до следующего прохода. Итог — строка INFO `blob gc: user=… rows=…
//! files=… bytes=…` на пользователя и аудит `blobs.gc {users, rows, files, bytes}` при ненулевом
//! итоге.
//!
//! Задача держит только `system.db` и кэш баз пользователей (открытые соединения SQLite);
//! останавливается [`Maintenance::stop`] (с ожиданием: после него соединений **этой** задачи
//! нет) или с последней копией состояния приложения. Другие держатели `Db` при остановке —
//! воркер индексации и фоновая задача открытия базы — перечислены у `AppState::shutdown`.
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::task::JoinHandle;
use zam_store::{SystemStore, UserDbs};

use crate::backup::runner::{BackupRunner, RunBy};
use crate::backup::schedule::{self, Action};
use crate::config::{BackupSchedule, CacheSection, ServerConfig};

use crate::app::AppState;

/// Срок хранения сессии без обращений.
pub const SESSION_TTL: SignedDuration = SignedDuration::from_hours(24 * 30);
/// Срок хранения строки журнала отправок модели от `ts` (M7.3 §6.2 п. 8).
pub const WORK_CALLS_TTL: SignedDuration = SignedDuration::from_hours(24 * 180);
/// Срок хранения прогона ночной проверки памяти от `started_at` (M7.3 §6.5 п. 7).
pub const CHECK_RUNS_TTL: SignedDuration = SignedDuration::from_hours(24 * 365);
/// Срок хранения закрытой внутренней работы от `finished_at` (M7.3 §6.3 п. 1; финальное ревью
/// M7.3, Minor 4): горизонт отсева кандидатов по итогам работ
/// ([`zam_store::works::RETENTION_DAYS`]).
pub const WORKS_TTL: SignedDuration =
    SignedDuration::from_hours(24 * zam_store::works::RETENTION_DAYS);
/// Период прохода обслуживания.
pub const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// Задержка первого прохода после старта: не мешать прогреву и перенарезке при открытии,
/// но и не откладывать на сутки (сервер могут перезапускать чаще).
pub const FIRST_RUN: Duration = Duration::from_secs(10 * 60);

/// Период проверки расписания автокопий.
pub const BACKUP_TICK: Duration = Duration::from_secs(60);

/// Задержка первого прохода сборщика мусора блобов после старта (M7.4 §7.4 п. 6).
pub const BLOB_GC_FIRST_RUN: Duration = Duration::from_secs(60 * 60);

/// Фоновая задача обслуживания; снимается при уничтожении.
#[derive(Debug)]
pub struct Maintenance {
    handle: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Maintenance {
    /// Запускает задачу в текущем рантайме tokio; `cfg` — для автокопий (`[backup]`), `backups`
    /// — замок копий, общий со страницей «Копии» (`AppState::backups`).
    pub fn start(
        system: SystemStore,
        users: UserDbs,
        cache: CacheSection,
        cfg: ServerConfig,
        backups: BackupRunner,
    ) -> Maintenance {
        let mut handles = Vec::new();
        let keep_days = cfg.snapshot.keep_days;
        if cfg.limits.blob_gc_days > 0 {
            let (system, users, cfg, backups) =
                (system.clone(), users.clone(), cfg.clone(), backups.clone());
            handles.push(tokio::spawn(async move {
                let start = tokio::time::Instant::now() + BLOB_GC_FIRST_RUN;
                let mut tick = tokio::time::interval_at(start, PERIOD);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    gc_blobs_in(&system, &users, &cfg, &backups, zam_store::time::now()).await;
                }
            }));
        }
        if cfg.backup.schedule != BackupSchedule::Off {
            let system = system.clone();
            handles.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(BACKUP_TICK);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut state = BackupState::default();
                loop {
                    tick.tick().await;
                    backup_tick(&cfg, zam_store::time::now(), &mut state, &backups, &system).await;
                }
            }));
        }
        let handle = tokio::spawn(async move {
            let start = tokio::time::Instant::now() + FIRST_RUN;
            let mut tick = tokio::time::interval_at(start, PERIOD);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                gc_sessions_in(&system, &users, zam_store::time::now()).await;
                gc_work_calls_in(&system, &users, zam_store::time::now()).await;
                gc_check_runs_in(&system, &users, zam_store::time::now()).await;
                gc_works_in(&system, &users, zam_store::time::now()).await;
                gc_cache_in(&system, &users, &cache, zam_store::time::now()).await;
                gc_snapshots_in(&system, &users, keep_days, zam_store::time::now()).await;
            }
        });
        handles.push(handle);
        Maintenance {
            handle: std::sync::Mutex::new(handles),
        }
    }

    /// Останавливает задачу и ждёт её завершения (проход, идущий сейчас, прерывается на
    /// ближайшем ожидании; каждая транзакция удаления атомарна). `abort` без ожидания
    /// оставлял копию `UserDbs` живой после остановки: соединения закрывались позже, уже
    /// после [`crate::AppState::shutdown`].
    pub async fn stop(&self) {
        let handles = std::mem::take(&mut *self.handle.lock().unwrap_or_else(|e| e.into_inner()));
        for h in handles {
            h.abort();
            let _ = h.await;
        }
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        let handles = std::mem::take(&mut *self.handle.lock().unwrap_or_else(|e| e.into_inner()));
        for h in handles {
            h.abort();
        }
    }
}

/// Один проход срока хранения сессий на момент `now` по всем базам; число удалённых сессий.
pub async fn gc_sessions(st: &AppState, now: Timestamp) -> u64 {
    gc_sessions_in(&st.system, &st.users, now).await
}

async fn gc_sessions_in(system: &SystemStore, users: &UserDbs, now: Timestamp) -> u64 {
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return 0;
        }
    };
    let cutoff = now - SESSION_TTL;
    let mut total = 0;
    for u in list {
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::gc_sessions(&db, cutoff).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(user = %u.id, deleted = n, "обслуживание: удалены старые сессии");
                total += n;
            }
            Err(e) => tracing::warn!(user = %u.id, error = %e, "обслуживание: сессии не очищены"),
        }
    }
    total
}

/// Один проход срока хранения журнала отправок модели ([`WORK_CALLS_TTL`] от `ts`, M7.3 §6.2
/// п. 8) на момент `now` по всем базам; число удалённых строк.
pub async fn gc_work_calls(st: &AppState, now: Timestamp) -> u64 {
    gc_work_calls_in(&st.system, &st.users, now).await
}

async fn gc_work_calls_in(system: &SystemStore, users: &UserDbs, now: Timestamp) -> u64 {
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return 0;
        }
    };
    let cutoff = now - WORK_CALLS_TTL;
    let mut total = 0;
    for u in list {
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::work_calls::gc(&db, cutoff).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(user = %u.id, deleted = n, "обслуживание: удалены старые строки журнала отправок");
                total += n;
            }
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "обслуживание: журнал отправок не очищен")
            }
        }
    }
    total
}

/// Один проход срока хранения прогонов ночной проверки памяти ([`CHECK_RUNS_TTL`] от
/// `started_at`, M7.3 §6.5 п. 7) на момент `now` по всем базам — вместе с их результатами; число
/// удалённых прогонов.
pub async fn gc_check_runs(st: &AppState, now: Timestamp) -> u64 {
    gc_check_runs_in(&st.system, &st.users, now).await
}

async fn gc_check_runs_in(system: &SystemStore, users: &UserDbs, now: Timestamp) -> u64 {
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return 0;
        }
    };
    let cutoff = now - CHECK_RUNS_TTL;
    let mut total = 0;
    for u in list {
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::check::gc(&db, cutoff).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(user = %u.id, deleted = n, "обслуживание: удалены старые прогоны проверки памяти");
                total += n;
            }
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "обслуживание: прогоны проверки памяти не очищены")
            }
        }
    }
    total
}

/// Один проход срока хранения закрытых внутренних работ ([`WORKS_TTL`] от `finished_at`, M7.3)
/// на момент `now` по всем базам; число удалённых работ.
pub async fn gc_works(st: &AppState, now: Timestamp) -> u64 {
    gc_works_in(&st.system, &st.users, now).await
}

async fn gc_works_in(system: &SystemStore, users: &UserDbs, now: Timestamp) -> u64 {
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return 0;
        }
    };
    let cutoff = now - WORKS_TTL;
    let mut total = 0;
    for u in list {
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::works::gc(&db, cutoff).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(user = %u.id, deleted = n, "обслуживание: удалены старые внутренние работы");
                total += n;
            }
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "обслуживание: внутренние работы не очищены")
            }
        }
    }
    total
}

/// Один проход срока хранения снимков сессий (`snapshot.keep_days`) на момент `now` по всем
/// базам; число удалённых снимков.
pub async fn gc_snapshots(st: &AppState, now: Timestamp) -> u64 {
    gc_snapshots_in(&st.system, &st.users, st.cfg.snapshot.keep_days, now).await
}

async fn gc_snapshots_in(
    system: &SystemStore,
    users: &UserDbs,
    keep_days: u32,
    now: Timestamp,
) -> u64 {
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return 0;
        }
    };
    let cutoff = now - SignedDuration::from_hours(24 * i64::from(keep_days));
    let mut total = 0;
    for u in list {
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::gc_snapshots(&db, cutoff).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(user = %u.id, deleted = n, "обслуживание: удалены старые снимки сессий");
                total += n;
            }
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "обслуживание: снимки сессий не очищены")
            }
        }
    }
    total
}

/// Один проход срока жизни кэша выжимок на момент `now` по всем базам; число удалённых.
pub async fn gc_cache(st: &AppState, now: Timestamp) -> u64 {
    gc_cache_in(&st.system, &st.users, &st.cfg.cache, now).await
}

async fn gc_cache_in(
    system: &SystemStore,
    users: &UserDbs,
    cfg: &CacheSection,
    now: Timestamp,
) -> u64 {
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return 0;
        }
    };
    let cutoff = now - SignedDuration::from_hours(24 * i64::from(cfg.ttl_days));
    let mut total = 0;
    for u in list {
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::gc_cache(&db, cutoff, cfg.max_entries).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(g) if g.expired + g.evicted == 0 => {}
            Ok(g) => {
                tracing::info!(
                    user = %u.id,
                    expired = g.expired,
                    evicted = g.evicted,
                    "обслуживание: очищен кэш выжимок"
                );
                total += g.expired + g.evicted;
            }
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "обслуживание: кэш выжимок не очищен")
            }
        }
    }
    total
}

/// Итог прохода сборщика мусора блобов по всем пользователям ([`gc_blobs`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct BlobGcPass {
    /// Проход пропущен целиком: `disabled` (`blob_gc_days = 0`) или `backup` (идёт копия).
    pub skipped: Option<&'static str>,
    /// Пользователей с удалениями.
    pub users: u64,
    pub rows: u64,
    pub files: u64,
    pub bytes: u64,
    /// Пользователей, пропущенных до следующего прохода: замок блобов занят (идёт запись).
    pub busy: u64,
}

/// Идёт ли копия каталога данных: признак копии или замок копий процесса.
fn backup_running(cfg: &ServerConfig, backups: &BackupRunner) -> bool {
    backups.state().running.is_some()
        || zam_store::backup_flag_active(&cfg.data_dir, cfg.backup.flag_max_age())
}

/// Один проход сборщика мусора блобов (M7.4 §7.4) на момент `now` по всем пользователям.
pub async fn gc_blobs(st: &AppState, now: Timestamp) -> BlobGcPass {
    gc_blobs_in(&st.system, &st.users, &st.cfg, &st.backups, now).await
}

async fn gc_blobs_in(
    system: &SystemStore,
    users: &UserDbs,
    cfg: &ServerConfig,
    backups: &BackupRunner,
    now: Timestamp,
) -> BlobGcPass {
    let mut pass = BlobGcPass::default();
    let days = cfg.limits.blob_gc_days;
    if days == 0 {
        pass.skipped = Some("disabled");
        return pass;
    }
    if backup_running(cfg, backups) {
        tracing::info!("blob gc: идёт копия — проход пропущен до следующих суток");
        pass.skipped = Some("backup");
        return pass;
    }
    let list = match system.list_users().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "обслуживание: список пользователей недоступен");
            return pass;
        }
    };
    let blobs_dir = cfg.data_dir.join("blobs");
    for u in list {
        // копия могла начаться посреди прохода: остаток — следующим проходом
        if backup_running(cfg, backups) {
            tracing::info!("blob gc: началась копия — проход прерван до следующих суток");
            break;
        }
        let store = zam_store::BlobStore::new(&blobs_dir, u.id);
        let r = match users.get(u.id).await {
            Ok(db) => zam_store::blob_gc::sweep(&db, store.root(), now, days).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(r) if r.busy => {
                tracing::debug!(user = %u.id, "blob gc: идёт запись блобов — пользователь пропущен");
                pass.busy += 1;
            }
            Ok(r) if r.is_empty() => {}
            Ok(r) => {
                tracing::info!(
                    "blob gc: user={} rows={} files={} bytes={}",
                    u.id,
                    r.rows,
                    r.files,
                    r.bytes
                );
                pass.users += 1;
                pass.rows += r.rows;
                pass.files += r.files;
                pass.bytes += r.bytes;
            }
            Err(e) => tracing::warn!(user = %u.id, error = %e, "blob gc: проход не выполнен"),
        }
    }
    if pass.rows + pass.files > 0 {
        let details = serde_json::json!({
            "users": pass.users,
            "rows": pass.rows,
            "files": pass.files,
            "bytes": pass.bytes,
        });
        if let Err(e) = system
            .audit("system", None, "blobs.gc", None, None, details)
            .await
        {
            tracing::warn!(error = %e, "аудит blobs.gc не записан");
        }
    }
    pass
}

/// Состояние автокопий между шагами: последняя попытка (удачная или нет) и подряд неудач.
#[derive(Debug, Clone, Default)]
pub struct BackupState {
    pub last_attempt: Option<Timestamp>,
    pub failures: u32,
}

/// Что сделал шаг автокопий.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// Копия запускалась; `Some(true)` — успешно.
    pub backup: Option<bool>,
    /// Удалено копий сверх `keep`.
    pub pruned: usize,
}

/// Один шаг расписания автокопий на момент `now`. Неудачная копия занимает слот (повтор — в
/// следующий срок `backup.at`, а не каждую минуту), WARN — один на неудачу; лишние копии
/// удаляются только после успешной копии (ротация `keep` в той же копии,
/// [`crate::backup::runner::RunGuard::run`]) или когда копия не нужна, по фактическому списку.
///
/// Шаг с делом (копия или удаление) идёт под замком `backups`, общим со страницей «Копии»
/// (M7 §5.6): занят — шаг пропущен целиком (ни копии, ни удаления, ни отметки попытки), следующая
/// проверка — через минуту. Решение принимается заново под замком: копия из WebUI могла только
/// что закончиться. Итог копии — аудит `server.backup {by: "schedule"}` в `system`.
pub async fn backup_tick(
    cfg: &ServerConfig,
    now: Timestamp,
    state: &mut BackupState,
    backups: &BackupRunner,
    system: &SystemStore,
) -> TickOutcome {
    let dir = cfg.backup.dir(&cfg.data_dir);
    let plan = |state: &BackupState| {
        let existing = crate::backup::list_copies(&dir);
        let last = crate::backup::last_copy_at(&dir).max(state.last_attempt);
        schedule::tick(now, last, &existing, &cfg.backup)
    };
    let mut out = TickOutcome::default();
    if plan(state).iter().all(|a| *a == Action::Skip) {
        return out;
    }
    let guard = match backups.try_start(RunBy::Schedule) {
        Ok(g) => g,
        Err(busy) => {
            tracing::debug!(
                by = busy.0.by.as_str(),
                "автокопия: идёт другая копия — шаг расписания отложен до следующей минуты"
            );
            return out;
        }
    };
    let mut guard = Some(guard);
    for a in plan(state) {
        match a {
            Action::Skip => {}
            Action::Backup => {
                let Some(g) = guard.take() else { continue };
                state.last_attempt = Some(now);
                let run = g.run(cfg, system, None).await;
                match &run.result {
                    Ok(rep) => {
                        state.failures = 0;
                        tracing::info!("автокопия: {}", rep.line());
                    }
                    Err(_) => {
                        // причина — строкой выше (`RunGuard::run`, с путями)
                        state.failures += 1;
                        tracing::warn!(
                            failures = state.failures,
                            "автокопия не снята: повтор в следующий срок backup.at"
                        );
                    }
                }
                out.backup = Some(run.result.is_ok());
                out.pruned += run.pruned;
            }
            // список удаления — заново под замком удаления, удалённые — по факту (ТЗ §16.4)
            Action::Prune(_) => {
                let (b, d, keep) = (backups.clone(), dir.clone(), cfg.backup.keep);
                out.pruned += tokio::task::spawn_blocking(move || b.rotate(&d, keep))
                    .await
                    .unwrap_or(0);
            }
        }
    }
    if let Some(g) = guard {
        g.release();
    }
    out
}
