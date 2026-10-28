//! Состояние приложения и сборка роутера.
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{DefaultPredicate, Predicate};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use zam_core::Id;
use zam_store::{DataDir, SystemStore, UserDbs};

use crate::auth::AuthState;
use crate::config::{AdminSecret, ServerConfig};
use crate::error::not_found;
use crate::index_state::IndexRuntime;
use crate::maintenance::Maintenance;
use crate::rerank::RerankSlot;
use crate::vault::VaultKeys;
use crate::{api, bootstrap, ml, web};

/// Git-хэш сборки (см. build.rs).
pub const GIT_SHA: &str = env!("ZAM_GIT_SHA");

/// Общее состояние; дёшево клонируется.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<ServerConfig>,
    /// Владелец лока каталога данных: живёт, пока жив любой клон состояния.
    pub data_dir: Arc<DataDir>,
    pub admin_hash: Arc<String>,
    /// Пользователь администратора (неизменен после первого запуска).
    pub admin_id: Id,
    pub auth: Arc<AuthState>,
    pub system: SystemStore,
    pub users: UserDbs,
    pub started_at: Instant,
    pub version: &'static str,
    /// Кэш HTML заметок (см. [`api::render::RenderCache`]).
    pub render_cache: api::render::RenderCache,
    /// Поколения ссылок для ключа кэша рендера.
    pub render_gen: Arc<api::render::RenderGenerations>,
    /// Модель, Qdrant и воркер индексации.
    pub index: Arc<IndexRuntime>,
    /// Реранкер `rerank=on` (загружается при первом таком запросе).
    pub rerank: Arc<RerankSlot>,
    /// Ключи Vault (§7.6): мастер-ключ, ключи пользователей, состояние блокировки.
    pub vault: Arc<VaultKeys>,
    /// Воркер обслуживания (срок хранения сессий); снимается с последней копией состояния.
    pub maintenance: Arc<Maintenance>,
}

impl AppState {
    /// Штатная остановка фоновых частей. Фоновые задачи, держащие `Db` (соединения SQLite), и как
    /// каждая останавливается (финальное ревью M4, M-5):
    /// - воркер обслуживания ([`Maintenance::stop`]) — с ожиданием конца задачи;
    /// - воркер индексации (`IndexRuntime::shutdown` → `Worker::shutdown`) — с ожиданием
    ///   текущей пачки;
    /// - фоновая задача открытия базы (перенарезка, дозаполнение рёбер, пересканирование
    ///   секретов; `zam_store::UserDbs`) — ждём перенарезку не дольше 10 с, затем выходим без
    ///   неё: задача может держать `Db` до выхода процесса и дорабатывает возобновляемо при
    ///   следующем открытии базы (идемпотентна, отметка в `meta`);
    /// - затем Qdrant (ожидание подтверждения WAL и остановка процесса).
    ///
    /// Поэтому «после `shutdown` соединений нет» верно для обслуживания и воркера индексации,
    /// но не для незаконченной задачи открытия базы.
    pub async fn shutdown(&self) {
        self.maintenance.stop().await;
        self.index.shutdown(&self.system, &self.users).await;
    }
}

/// Откуда пришла команда остановки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSource {
    /// Ctrl+C, SIGINT, SIGTERM, (Windows) Ctrl+Break — консоль, `systemctl stop`, NSSM.
    Signal,
    /// `Stop`/`Shutdown`/`Preshutdown` диспетчера служб Windows.
    Service,
}

impl std::fmt::Display for StopSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Signal => "сигнал",
            Self::Service => "менеджер служб",
        })
    }
}

/// Общий сигнал остановки сервера (§4.3): сигналы ОС и SCM ведут в один и тот же путь —
/// штатное завершение HTTP, затем [`AppState::shutdown`] (воркер, перечанковка, ожидание WAL
/// и остановка Qdrant). Срабатывает один раз; источник — первый.
#[derive(Clone, Default)]
pub struct ShutdownSignal {
    token: tokio_util::sync::CancellationToken,
    source: Arc<std::sync::OnceLock<StopSource>>,
}

impl ShutdownSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Запросить остановку (повторный вызов ничего не меняет).
    pub fn trigger(&self, source: StopSource) {
        let _ = self.source.set(source);
        self.token.cancel();
    }

    pub fn is_triggered(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Дождаться остановки; источник — первый [`Self::trigger`].
    pub async fn wait(&self) -> StopSource {
        self.token.cancelled().await;
        self.source.get().copied().unwrap_or(StopSource::Signal)
    }

    /// Перевести завершение `os` (сигналы ОС) в этот сигнал.
    pub fn watch(&self, os: impl std::future::Future<Output = ()> + Send + 'static) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = os => me.trigger(StopSource::Signal),
                () = me.token.cancelled() => {}
            }
        });
    }
}

/// Сигналы ОС штатной остановки: Ctrl+C/SIGINT, SIGTERM (unix), Ctrl+Break (Windows). Сбой
/// подписки на сигнал — не остановка: такой путь просто не срабатывает.
pub async fn os_stop_signals() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    // Windows: Ctrl+Break — штатная остановка процесса без окна консоли (его группе процессов
    // шлёт `GenerateConsoleCtrlEvent`: `cargo xtask bench --scenario first-start`, менеджеры
    // служб); Ctrl+C такой группе не доставляется.
    #[cfg(windows)]
    let term = async {
        match tokio::signal::windows::ctrl_break() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(any(unix, windows)))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

// Вручную: хэш пароля и пути не должны попадать в отладочный вывод.
impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// Результат первого запуска.
#[derive(Default)]
pub struct BootReport {
    /// Открытый текст первого токена администратора; `Some` только при создании.
    pub initial_token: Option<String>,
}

impl std::fmt::Debug for BootReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootReport")
            .field("initial_token", &self.initial_token.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Собирает состояние и роутер. Глобальный subscriber не трогает.
pub async fn build(
    cfg: ServerConfig,
    env_password: Option<String>,
) -> anyhow::Result<(Router, AppState, BootReport)> {
    finish(prepare(cfg, env_password.as_deref())?).await
}

/// Фаза 1 запуска: пароль администратора и лок каталога данных.
pub(crate) struct Prepared {
    cfg: ServerConfig,
    admin_hash: String,
    pub(crate) data_dir: Arc<DataDir>,
}

/// Без побочных эффектов до проверки пароля; каталог создаётся только после неё.
pub(crate) fn prepare(cfg: ServerConfig, env_password: Option<&str>) -> anyhow::Result<Prepared> {
    let AdminSecret::Hash(admin_hash) = cfg.admin_secret(env_password)?;
    let data_dir = Arc::new(DataDir::open(&cfg.data_dir)?);
    Ok(Prepared {
        cfg,
        admin_hash,
        data_dir,
    })
}

/// Фаза 2: система, администратор, роутер.
pub(crate) async fn finish(p: Prepared) -> anyhow::Result<(Router, AppState, BootReport)> {
    let Prepared {
        cfg,
        admin_hash,
        data_dir,
    } = p;
    // Модель — до первого открытия базы пользователя: перечанковка R1 и запись идут
    // точным счётчиком токенов модели (без второй копии токенизатора).
    let ml_cfg = cfg.clone();
    let ml_task = tokio::task::spawn_blocking(move || ml::load(&ml_cfg));
    // Qdrant — параллельно с загрузкой модели (секунды с HDD, CUDA): к её концу он уже
    // готов (§17). Без установленной модели не запускается — векторный индекс не нужен.
    let early_sup = if ml::model_installed(&cfg) {
        Some(IndexRuntime::start_supervisor(&cfg).await)
    } else {
        None
    };
    // паника загрузки (ORT, модель) не валит старт: сервер работает в режиме FTS-only
    let ml = match ml_task.await {
        Ok(m) => m,
        Err(e) => ml::MlLoad::failed(
            crate::index_state::DegradedReason::NoModel,
            format!("загрузка модели завершилась аварийно: {e}"),
        ),
    };
    // без эмбеддера — токенизатор установленной модели (если есть): чанки сразу точные
    ml::setup_chunker(&cfg, ml.embedder.as_deref());
    let system = SystemStore::open(&data_dir)?;
    let initial_token = bootstrap::ensure_admin_user(&system, &cfg.admin.login).await?;
    bootstrap::warn_legacy_human_tokens(&system).await;
    let admin_id = system
        .admin_user()
        .await?
        .ok_or_else(|| anyhow::anyhow!("администратор не найден после первого запуска"))?
        .id;
    // Фиктивный хэш — до приёма запросов: без оракула первого вызова.
    let auth = tokio::task::spawn_blocking(AuthState::new)
        .await?
        .map_err(|e| anyhow::anyhow!("не удалось подготовить проверку пароля: {e}"))?;
    // Vault: блокировка (ключ подменён или потерян) — не ошибка старта
    let vault = Arc::new(VaultKeys::load(&cfg, &system).await?);
    let users = UserDbs::new(data_dir.users_dir());
    // реестр решений — до индексации и приёма запросов (§11.6)
    bootstrap::register_solutions(&system, &users).await;
    let index = Arc::new(IndexRuntime::start(&cfg, ml, early_sup, &system, &users).await);
    let rerank = Arc::new(RerankSlot::new(&cfg));
    let maintenance = Arc::new(Maintenance::start(
        system.clone(),
        users.clone(),
        cfg.cache.clone(),
    ));
    let state = AppState {
        maintenance,
        users,
        index,
        rerank,
        vault,
        data_dir,
        system,
        admin_hash: Arc::new(admin_hash),
        admin_id,
        auth: Arc::new(auth),
        started_at: Instant::now(),
        version: env!("CARGO_PKG_VERSION"),
        render_cache: api::render::new_cache(),
        render_gen: Arc::default(),
        cfg: Arc::new(cfg),
    };
    let router = router(state.clone());
    Ok((router, state, BootReport { initial_token }))
}

fn router(state: AppState) -> Router {
    let base = state.cfg.server.base_path.clone();
    let index = web::Index::new(&base);
    let root = index.clone();
    let api = api::router(&state)
        // MCP (§13.1) — под тем же `base_path`; его внутренний роутер — `api::router` без `/mcp`
        .merge(crate::mcp::router(&state))
        .merge(web::assets_router())
        .fallback(move |m, u| web::fallback(index.clone(), m, u))
        .with_state(state);
    let app = if base.is_empty() {
        api
    } else {
        // `nest` не отдаёт «{base}/» вложенному роутеру — корень WebUI добавляем явно.
        Router::new()
            .route(
                &format!("{base}/"),
                axum::routing::get(move || std::future::ready(root.response())),
            )
            .nest(&base, api)
            .fallback(not_found)
    };
    // Только метод, путь (без query: в ней может быть токен), статус и задержка.
    app.layer(
        TraceLayer::new_for_http()
            .make_span_with(|req: &axum::http::Request<_>| {
                tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
            })
            .on_request(())
            .on_response(DefaultOnResponse::new().level(Level::INFO)),
    )
    .layer(CompressionLayer::new().compress_when(compress_predicate()))
}

/// Сжатие по умолчанию, кроме ответов с меткой [`api::blobs::NoCompress`]
/// (блобы: `Range` и медиа).
fn compress_predicate() -> impl Predicate {
    DefaultPredicate::new().and(
        |_: axum::http::StatusCode,
         _: axum::http::Version,
         _: &axum::http::HeaderMap,
         ext: &axum::http::Extensions| ext.get::<api::blobs::NoCompress>().is_none(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ctrl+C/SIGTERM и SCM ведут в один сигнал: кто первый — тот источник, ожидающие
    /// просыпаются одинаково, повтор ничего не меняет.
    #[tokio::test]
    async fn shutdown_signal_shared_by_ctrl_c_and_service() {
        // путь SCM
        let s = ShutdownSignal::new();
        let waiter = tokio::spawn({
            let s = s.clone();
            async move { s.wait().await }
        });
        assert!(!s.is_triggered());
        s.trigger(StopSource::Service);
        s.trigger(StopSource::Signal);
        assert_eq!(waiter.await.unwrap(), StopSource::Service);
        // путь сигнала ОС (вместо Ctrl+C — готовое будущее) через тот же сигнал
        let s = ShutdownSignal::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        s.watch(async move {
            let _ = rx.await;
        });
        let waiter = tokio::spawn({
            let s = s.clone();
            async move { s.wait().await }
        });
        tx.send(()).unwrap();
        assert_eq!(waiter.await.unwrap(), StopSource::Signal);
        // после SCM сигнал ОС источник не подменяет
        let s = ShutdownSignal::new();
        s.trigger(StopSource::Service);
        s.watch(std::future::ready(()));
        tokio::task::yield_now().await;
        assert_eq!(s.wait().await, StopSource::Service);
    }
}
