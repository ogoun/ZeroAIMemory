//! Воркер обслуживания (§6.3): периодические задачи, не связанные с индексацией.
//!
//! Сейчас — срок хранения сессий агентов: раз в [`PERIOD`] (первый проход — через
//! [`FIRST_RUN`] после старта) в каждой базе пользователя удаляются сессии без обращений
//! дольше [`SESSION_TTL`] (30 дней от `last_seen_at`) вместе с выданным им (`session_seen`);
//! `usage` хранится бессрочно; срок жизни кэша выжимок (§9.4): выжимки без обращений дольше
//! `cache.ttl_days` и сверх `cache.max_entries` на базу (LRU) удаляются, превращённые в записи
//! — нет. Работает и без модели (в отличие от воркера индексации).
//! Задача держит только `system.db` и кэш баз пользователей; останавливается
//! [`Maintenance::stop`] или с последней копией состояния приложения.
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::task::JoinHandle;
use zam_store::{SystemStore, UserDbs};

use crate::config::CacheSection;

use crate::app::AppState;

/// Срок хранения сессии без обращений.
pub const SESSION_TTL: SignedDuration = SignedDuration::from_hours(24 * 30);
/// Период прохода обслуживания.
pub const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// Задержка первого прохода после старта: не мешать прогреву и перенарезке при открытии,
/// но и не откладывать на сутки (сервер могут перезапускать чаще).
pub const FIRST_RUN: Duration = Duration::from_secs(10 * 60);

/// Фоновая задача обслуживания; снимается при уничтожении.
#[derive(Debug)]
pub struct Maintenance {
    handle: JoinHandle<()>,
}

impl Maintenance {
    /// Запускает задачу в текущем рантайме tokio.
    pub fn start(system: SystemStore, users: UserDbs, cache: CacheSection) -> Maintenance {
        let handle = tokio::spawn(async move {
            let start = tokio::time::Instant::now() + FIRST_RUN;
            let mut tick = tokio::time::interval_at(start, PERIOD);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                gc_sessions_in(&system, &users, Timestamp::now()).await;
                gc_cache_in(&system, &users, &cache, Timestamp::now()).await;
            }
        });
        Maintenance { handle }
    }

    /// Останавливает задачу (проход, идущий сейчас, прерывается на ближайшем ожидании;
    /// каждая транзакция удаления атомарна).
    pub fn stop(&self) {
        self.handle.abort();
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.handle.abort();
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
