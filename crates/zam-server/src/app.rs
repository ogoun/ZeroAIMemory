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
use crate::rerank::RerankSlot;
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
}

impl AppState {
    /// Штатная остановка фоновых частей: воркер индексации, перечанковка R1, Qdrant.
    pub async fn shutdown(&self) {
        self.index.shutdown(&self.system, &self.users).await;
    }
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
    let admin_id = system
        .admin_user()
        .await?
        .ok_or_else(|| anyhow::anyhow!("администратор не найден после первого запуска"))?
        .id;
    // Фиктивный хэш — до приёма запросов: без оракула первого вызова.
    let real = admin_hash.clone();
    let auth = tokio::task::spawn_blocking(move || AuthState::new(&real))
        .await?
        .map_err(|e| anyhow::anyhow!("не удалось подготовить проверку пароля: {e}"))?;
    let users = UserDbs::new(data_dir.users_dir());
    // реестр решений — до индексации и приёма запросов (§11.6)
    bootstrap::register_solutions(&system, &users).await;
    let index = Arc::new(IndexRuntime::start(&cfg, ml, early_sup, &system, &users).await);
    let rerank = Arc::new(RerankSlot::new(&cfg));
    let state = AppState {
        users,
        index,
        rerank,
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
    let api = api::router(&state.cfg.limits)
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
