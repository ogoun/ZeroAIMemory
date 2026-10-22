//! `zam-server reindex [--user ID] [--space REF] [--full]` — пересборка векторного индекса
//! на остановленном сервере (§18 ТЗ: «Qdrant пересобирается из `embeddings` без инференса»).
//!
//! Каталог данных открывается под локом (`DataDir`): если сервер запущен, команда
//! отказывает с подсказкой про `POST /api/v1/admin/reindex`. Модель загружается так же, как
//! при старте сервера (её `model_id` — ключ кэша и метка коллекции), Qdrant запускается
//! своим супервизором (или подключается внешний). Дальше — тот же
//! [`zam_index::reindex`], что выполняет воркер: без `--full` точки области удаляются и
//! пишутся из кэша, заметки без эмбеддингов получают задания; с `--full` — перечанковка и
//! задания. Задания выполнит сервер после запуска. Пересборка всех баз снимает флаг
//! стёртого хранилища `.zam-reindex`: повторная пересборка при старте сервера не нужна.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use zam_index::supervisor::{QdrantState, Supervisor};
use zam_index::{QdrantIndex, ReindexCtx, ReindexReport, ReindexRequest, VectorIndex, reindex};
use zam_store::{DataDir, StoreError, SystemStore, UserDbs};

use crate::api::index::reindex_scope;
use crate::config::ServerConfig;
use crate::error::ApiError;

/// Предел ожидания готовности Qdrant (создание коллекции на HDD — секунды, запуск — до минуты).
const READY_TIMEOUT: Duration = Duration::from_secs(300);

/// Аргументы команды.
#[derive(Debug, Clone, Default)]
pub struct ReindexArgs {
    pub user: Option<String>,
    pub space: Option<String>,
    pub full: bool,
}

fn api_err(e: ApiError) -> anyhow::Error {
    match e {
        ApiError::NotFound => anyhow!("пользователь или пространство не найдены"),
        other => anyhow!("{other}"),
    }
}

/// Выполняет команду и возвращает отчёт для вывода.
pub async fn run(cfg: ServerConfig, args: ReindexArgs) -> anyhow::Result<String> {
    let dd = match DataDir::open(&cfg.data_dir) {
        Ok(d) => d,
        Err(StoreError::DataDirLocked(p)) => bail!(
            "каталог данных {} занят: zam-server запущен. Остановите сервер или выполните \
             reindex через API: POST /api/v1/admin/reindex (администратор)",
            p.display()
        ),
        Err(e) => return Err(e).context("каталог данных"),
    };
    // модель — как при старте сервера: её model_id — ключ кэша и метка коллекции
    let ml_cfg = cfg.clone();
    let ml = tokio::task::spawn_blocking(move || crate::ml::load(&ml_cfg))
        .await
        .context("загрузка модели")?;
    let Some(embedder) = ml.embedder else {
        let (reason, err) = ml
            .error
            .unwrap_or((crate::index_state::DegradedReason::NoModel, String::new()));
        bail!("модель не загружена: {err}; {}", reason.hint());
    };
    crate::ml::setup_chunker(&cfg, Some(&embedder));
    let system = SystemStore::open(&dd)?;
    let users = UserDbs::new(dd.users_dir());
    // база, указанная явно, должна открыться; при обходе всех повреждённые пропускаются
    // (reindex отметит их исключёнными в system.db, отчёт их перечислит)
    let scope = reindex_scope(&system, &users, args.user.as_deref(), args.space.as_deref())
        .await
        .map_err(api_err)?;
    let (user, space, notes) = (scope.user, scope.space, scope.job_count);

    let exe_dir = crate::qdrant::exe_dir()?;
    let Some(sc) = cfg.qdrant.supervisor_config(&exe_dir)? else {
        bail!("qdrant.mode = \"edge\" пока не поддерживается");
    };
    let sup = Supervisor::start(sc, &cfg.data_dir).await?;
    let res = async {
        let vector = QdrantIndex::connect(&sup.grpc_url(), sup.api_key().as_deref())?;
        wait_ready(&sup, &vector).await?;
        let req = ReindexRequest {
            user,
            space,
            full: args.full,
        };
        let ctx = ReindexCtx {
            system: &system,
            users: &users,
            vector: &vector,
            model_id: embedder.model_id(),
            dim: embedder.dim(),
            // L0 папок, которых нет в кэше, — моделью сразу (воркера здесь нет)
            embedder: Some(Arc::new(crate::ml::EmbedderAdapter(embedder.clone()))),
        };
        let rep = reindex(&ctx, &req).await?;
        // все базы пересобраны (неоткрывшиеся отмечены исключёнными и получат reindex,
        // когда откроются): стёртое хранилище восстановлено, флаг не нужен
        if may_ack_storage(sup.storage_rebuilt(), user.is_none(), &rep) {
            sup.ack_storage_rebuilt()?;
        }
        system
            .audit(
                "cli",
                user,
                "index.reindex",
                space.map(|s| s.to_string()).as_deref(),
                None,
                serde_json::json!({
                    "full": args.full, "job_count": notes, "users": rep.users,
                    "nodes": rep.nodes, "entries": rep.entries, "points": rep.points, "misses": rep.misses,
                    "jobs": rep.jobs, "gc_deleted": rep.gc_deleted,
                    "recreated": rep.recreated, "failed_users": rep.failed_users.len(),
                    "elapsed_ms": rep.elapsed_ms,
                }),
            )
            .await?;
        anyhow::Ok(rep)
    }
    .await;
    // перечанковка R1 баз, открытых впервые после обновления, — до выхода
    if let Ok(list) = system.list_users().await {
        for u in &list {
            users.wait_rechunk(u.id).await;
        }
    }
    sup.shutdown().await;
    Ok(report(&res?, embedder.model_id()))
}

/// Снимать ли флаг стёртого хранилища `.zam-reindex`: пересобраны все базы (команда без
/// `--user` или коллекция создана заново), а каждая необработанная база отмечена исключённой
/// в `system.db` — иначе она не восстановится сама, и флаг оставляется: сервер повторит
/// пересборку при старте.
fn may_ack_storage(storage_rebuilt: bool, all_users: bool, rep: &ReindexReport) -> bool {
    storage_rebuilt && (all_users || rep.recreated) && rep.skip_unrecorded == 0
}

/// Ждёт готовности Qdrant (managed — `Ready`, external — ответ на запрос с ключом).
async fn wait_ready(sup: &Supervisor, vector: &QdrantIndex) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut state = sup.state();
    loop {
        let st = state.borrow_and_update().clone();
        match st {
            QdrantState::Ready { .. } | QdrantState::External => {
                if vector.health().await.is_ok() {
                    return Ok(());
                }
            }
            QdrantState::Broken { reason } => bail!("Qdrant не запустился: {reason}"),
            _ => {}
        }
        if started.elapsed() > READY_TIMEOUT {
            bail!("Qdrant не готов за {} с", READY_TIMEOUT.as_secs());
        }
        let _ = tokio::time::timeout(Duration::from_millis(500), state.changed()).await;
    }
}

/// Отчёт команды: счётчики без текстов.
fn report(r: &ReindexReport, model: &str) -> String {
    let mut out = format!(
        "reindex ({}, модель {model}): баз {}, заметок из кэша {}, записей журналов {}, \
         точек {} (+{} папок), удалено строк кэша {}, {} мс",
        if r.full {
            "полный"
        } else {
            "из кэша"
        },
        r.users,
        r.nodes,
        r.entries,
        r.points,
        r.folders,
        r.gc_deleted,
        r.elapsed_ms
    );
    if r.recreated {
        out.push_str("\nколлекция Qdrant создана заново: пересобраны все базы");
    }
    if r.jobs > 0 {
        out.push_str(&format!(
            "\nпоставлено заданий индексации: {} (без эмбеддингов в кэше: {}); выполнятся после запуска сервера",
            r.jobs, r.misses
        ));
    }
    if r.skip_unrecorded > 0 {
        out.push_str(&format!(
            "\nотметки исключённых баз не записаны ({}): флаг пересборки хранилища оставлен, \
             сервер повторит пересборку при старте",
            r.skip_unrecorded
        ));
    }
    for (u, e) in &r.failed_users {
        out.push_str(&format!(
            "\nбаза пользователя {u} не обработана и исключена из пересборки ({e}); \
             после её восстановления сервер поставит reindex сам"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_flag_kept_when_skip_marks_unrecorded() {
        let mut rep = ReindexReport::default();
        assert!(may_ack_storage(true, true, &rep));
        assert!(
            !may_ack_storage(false, true, &rep),
            "хранилище не стиралось"
        );
        assert!(!may_ack_storage(true, false, &rep), "одна база — не все");
        rep.recreated = true;
        assert!(
            may_ack_storage(true, false, &rep),
            "новая коллекция: пересобраны все"
        );
        rep.failed_users
            .push((zam_core::Id::new(), "повреждена".into()));
        assert!(
            may_ack_storage(true, true, &rep),
            "исключённая база отмечена"
        );
        rep.skip_unrecorded = 1;
        assert!(
            !may_ack_storage(true, true, &rep),
            "отметка не записана — флаг остаётся"
        );
        assert!(report(&rep, "m").contains("флаг пересборки хранилища оставлен"));
    }
}
