//! Воркер обслуживания (§6.3): периодические задачи, не связанные с индексацией.
//!
//! Сейчас — срок хранения сессий агентов: раз в [`PERIOD`] (первый проход — через
//! [`FIRST_RUN`] после старта) в каждой базе пользователя удаляются сессии без обращений
//! дольше [`SESSION_TTL`] (30 дней от `last_seen_at`) вместе с выданным им (`session_seen`);
//! `usage` хранится бессрочно; срок жизни кэша выжимок (§9.4): выжимки без обращений дольше
//! `cache.ttl_days` и сверх `cache.max_entries` на базу (LRU) удаляются, превращённые в записи
//! — нет; снимки сессий (M6 §8а) старше `snapshot.keep_days` от `taken_at` удаляются
//! ([`gc_snapshots`]). Работает и без модели (в отличие от воркера индексации).
//!
//! Автокопии (M6 §12б.4): раз в [`BACKUP_TICK`] — решение [`crate::backup::schedule::tick`]
//! по `[backup]`; копию снимает тот же код, что `zam-server backup` (в блокирующем потоке),
//! копии сверх `keep` удаляются, начиная с самых старых.
//! Задача держит только `system.db` и кэш баз пользователей (открытые соединения SQLite);
//! останавливается [`Maintenance::stop`] (с ожиданием: после него соединений **этой** задачи
//! нет) или с последней копией состояния приложения. Другие держатели `Db` при остановке —
//! воркер индексации и фоновая задача открытия базы — перечислены у `AppState::shutdown`.
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::task::JoinHandle;
use zam_store::{SystemStore, UserDbs};

use crate::backup::schedule::{self, Action};
use crate::config::{BackupSchedule, CacheSection, ServerConfig};

use crate::app::AppState;

/// Срок хранения сессии без обращений.
pub const SESSION_TTL: SignedDuration = SignedDuration::from_hours(24 * 30);
/// Период прохода обслуживания.
pub const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// Задержка первого прохода после старта: не мешать прогреву и перенарезке при открытии,
/// но и не откладывать на сутки (сервер могут перезапускать чаще).
pub const FIRST_RUN: Duration = Duration::from_secs(10 * 60);

/// Период проверки расписания автокопий.
pub const BACKUP_TICK: Duration = Duration::from_secs(60);

/// Фоновая задача обслуживания; снимается при уничтожении.
#[derive(Debug)]
pub struct Maintenance {
    handle: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Maintenance {
    /// Запускает задачу в текущем рантайме tokio; `cfg` — для автокопий (`[backup]`).
    pub fn start(
        system: SystemStore,
        users: UserDbs,
        cache: CacheSection,
        cfg: ServerConfig,
    ) -> Maintenance {
        let mut handles = Vec::new();
        let keep_days = cfg.snapshot.keep_days;
        if cfg.backup.schedule != BackupSchedule::Off {
            handles.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(BACKUP_TICK);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut state = BackupState::default();
                loop {
                    tick.tick().await;
                    backup_tick(&cfg, zam_store::time::now(), &mut state).await;
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
/// удаляются только после успешной копии (или когда копия не нужна), по фактическому списку.
pub async fn backup_tick(
    cfg: &ServerConfig,
    now: Timestamp,
    state: &mut BackupState,
) -> TickOutcome {
    let dir = cfg.backup.dir(&cfg.data_dir);
    let mut out = TickOutcome::default();
    for round in 0..2 {
        let existing = crate::backup::list_copies(&dir);
        let last = crate::backup::last_copy_at(&dir).max(state.last_attempt);
        let mut again = false;
        for a in schedule::tick(now, last, &existing, &cfg.backup) {
            match a {
                Action::Skip => {}
                Action::Backup if round == 0 => {
                    state.last_attempt = Some(now);
                    let c = cfg.clone();
                    let r = tokio::task::spawn_blocking(move || {
                        crate::backup::backup(&c, &crate::backup::BackupOpts::default())
                    })
                    .await;
                    let ok = matches!(r, Ok(Ok(_)));
                    match r {
                        Ok(Ok(rep)) => {
                            state.failures = 0;
                            tracing::info!("автокопия: {}", rep.line());
                        }
                        Ok(Err(e)) => {
                            state.failures += 1;
                            tracing::warn!(
                                error = %e,
                                failures = state.failures,
                                "автокопия не снята: повтор в следующий срок backup.at"
                            );
                        }
                        Err(e) => {
                            state.failures += 1;
                            tracing::warn!(
                                error = %e,
                                "автокопия завершилась аварийно: повтор в следующий срок backup.at"
                            );
                        }
                    }
                    out.backup = Some(ok);
                    // после успешной копии — ротация по новому списку
                    again = ok;
                }
                Action::Backup => {}
                Action::Prune(list) => {
                    out.pruned += list.len();
                    tokio::task::spawn_blocking(move || crate::backup::prune(&list))
                        .await
                        .ok();
                }
            }
        }
        if !again {
            break;
        }
    }
    out
}
