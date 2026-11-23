//! Копии сервера из WebUI (M7 §5.6 п. 1, решение координатора 24): «Администрирование → Копии».
//! Только сессия администратора WebUI (`AdminOnly`); токену и пользователю — 403.
//!
//! - `GET /admin/backups` → `{dir, schedule, at, keep, warn_days, last_at, warning, running,
//!   last_run, items}`: `dir` — каталог копий (путь — только здесь и только администратору; в
//!   `/index/status` путей нет), `last_at`/`warning` — из [`crate::backup::status_json`],
//!   `running {started_at, by: "webui" | "schedule"} | null`, `last_run {finished_at, by, ok,
//!   error, report} | null` (`report` — итог без пути: имя копии и числа), `items` — готовые
//!   копии каталога копий по описи, новые первыми: `{name, created_at, server_version, bytes,
//!   files, dbs, blobs, drift}` (`bytes` — сумма размеров описи, без обхода диска). Опрос раз в
//!   3 с лёгкий: опись читается один раз на копию ([`BackupRunner::summaries`]).
//! - `POST /admin/backups` → 202 `{started_at}`: копия в процессе сервера тем же
//!   [`crate::backup::backup`] в блокирующем потоке под замком, общим с автокопией
//!   ([`crate::backup::runner`]); занято — 409 `/problems/backup-running` с `running`; сервер
//!   останавливается — 503 `/problems/busy`. После успеха при `schedule = "daily"` — ротация
//!   `keep`, при `off` — без ротации. Аудит `server.backup {by, ok, name, bytes, ms, error}`.
//! - `DELETE /admin/backups/{name}` → 204: только готовая копия каталога копий по имени-шаблону
//!   с описью ([`crate::backup::delete_copy`]); иначе — 404, путей из запроса нет. Удаления копий
//!   и ротация `keep` идут по одному ([`BackupRunner::delete`]): копия, которую уже удалили, —
//!   404, а не сбой. Аудит `server.backup.delete {name}`.
//!
//! **Восстановления и скачивания копии из WebUI нет:** `restore` — только на остановленном
//! сервере (ТЗ §16.4), а скачивание отдало бы через браузер хэши токенов и шифротексты Vault;
//! перенос — `zam-server backup --zip` на машине сервера.
//!
//! [`BackupRunner::summaries`]: crate::backup::runner::BackupRunner::summaries
//! [`BackupRunner::delete`]: crate::backup::runner::BackupRunner::delete

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};
use zam_store::time::fmt_ts;

use crate::app::AppState;
use crate::auth::{AdminOnly, Client};
use crate::backup::BackupError;
use crate::backup::runner::{Busy, RunBy};
use crate::error::ApiError;

/// `Retry-After` отказа копии во время остановки сервера, с.
const STOPPING_RETRY_S: u64 = 5;

/// `GET /admin/backups`.
pub async fn list(_: AdminOnly, State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    // состояние — до чтения каталога: итог копии пишется после переименования её каталога, так
    // что ответ с итогом всегда несёт и эту копию в `items` (страница перестаёт опрашивать по
    // `running = null` и иначе показала бы список без неё)
    let run = st.backups.state();
    let cfg = st.cfg.clone();
    let runner = st.backups.clone();
    // каталог копий — на диске (HDD): чтение — в блокирующем потоке
    let (items, status) = tokio::task::spawn_blocking(move || {
        let dir = cfg.backup.dir(&cfg.data_dir);
        let items = runner.summaries(&dir);
        (
            items,
            crate::backup::status_json(&cfg, zam_store::time::now()),
        )
    })
    .await
    .map_err(|e| ApiError::aborted("список копий", &e))?;
    let b = &st.cfg.backup;
    Ok(Json(json!({
        "dir": b.dir(&st.cfg.data_dir).display().to_string(),
        "schedule": b.schedule,
        "at": b.at,
        "keep": b.keep,
        "warn_days": b.warn_days,
        "last_at": status["last_at"],
        "warning": status["warning"],
        "running": run.running,
        "last_run": run.last,
        "items": items,
    })))
}

/// `POST /admin/backups`.
pub async fn start(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if st.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ApiError::ShuttingDown {
            retry_after_s: STOPPING_RETRY_S,
        });
    }
    let guard = st
        .backups
        .try_start(RunBy::WebUi)
        .map_err(|Busy(r)| ApiError::BackupRunning(r))?;
    let started_at = fmt_ts(guard.running().started_at);
    let cfg = (*st.cfg).clone();
    let system = st.system.clone();
    let ip = client.ip.to_string();
    tracing::info!(ip = %ip, "копия из WebUI начата");
    tokio::spawn(async move {
        let run = guard.run(&cfg, &system, Some(&ip)).await;
        if let Ok(rep) = &run.result {
            tracing::info!(pruned = run.pruned, "копия из WebUI: {}", rep.line());
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "started_at": started_at })),
    ))
}

/// `DELETE /admin/backups/{name}`. Имя, которое не разобрать (не UTF-8), — тоже 404.
pub async fn remove(
    _: AdminOnly,
    State(st): State<AppState>,
    client: Client,
    name: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Ok(Path(name)) = name else {
        return Err(ApiError::NotFound);
    };
    let dir = st.cfg.backup.dir(&st.cfg.data_dir);
    let n = name.clone();
    let runner = st.backups.clone();
    let r = tokio::task::spawn_blocking(move || runner.delete(&dir, &n))
        .await
        .map_err(|e| ApiError::aborted("удаление копии", &e))?;
    match r {
        Ok(()) => {}
        Err(BackupError::NotACopy(_)) => return Err(ApiError::NotFound),
        Err(e) => {
            return Err(ApiError::Internal(format!("копия {name} не удалена: {e}")));
        }
    }
    st.system
        .audit(
            "admin",
            None,
            "server.backup.delete",
            Some(&name),
            Some(&client.ip.to_string()),
            json!({ "name": name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
