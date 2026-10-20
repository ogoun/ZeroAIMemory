//! Супервизор нативного Qdrant (§4.3, исследование 05 §6–7).
//!
//! Режим `managed`:
//! 1. [`Supervisor::start`] берёт лок `<data_dir>/qdrant/.zam-qdrant.lock`, завершает
//!    осиротевший процесс из pid-файла, сверяет метку хранилища (несовместимое — стирается,
//!    [`Supervisor::storage_rebuilt`]), пишет `config/zam.yaml`, генерирует API-ключ и
//!    запускает поток «qdrant-supervisor» со своим `current_thread` runtime. Готовности
//!    не ждёт: состояние публикуется в [`Supervisor::state`].
//! 2. Поток запускает Qdrant (Job Object / PDEATHSIG, см. `process.rs`), пересылает его
//!    журнал в `tracing` (`component="qdrant"`) и ждёт готовности: `/readyz` → `GET /`
//!    (версия обязана совпасть с манифестом, иначе `Broken`) → gRPC health с ключом.
//! 3. Падение → `Restarting` с задержкой 1→60 с и jitter; 5 падений за 10 минут →
//!    `Broken` (circuit breaker), сервер продолжает работу в деградированном режиме.
//!    panic несовместимого хранилища → стирание `storage/` и один немедленный перезапуск.
//! 4. [`Supervisor::shutdown`]: Linux — SIGTERM, 10 с, SIGKILL; Windows — `TerminateProcess`.
//!
//! Режим `external`: процесса нет, состояние всегда `External`, доступность проверяет
//! `VectorIndex::health`.
//!
//! API-ключ нигде не логируется и не пишется на диск: только `QDRANT__SERVICE__API_KEY`.

mod config;
mod http;
mod process;
mod stamp;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use rand::Rng as _;
use tokio::sync::watch;

pub use config::{
    ExternalConfig, ManagedConfig, QDRANT_VERSION, RestartPolicy, SupervisorConfig, default_binary,
    qdrant_exe_name,
};
pub use process::{is_qdrant_process, pid_alive, rss_bytes};

use crate::{IndexError, QdrantIndex, VectorIndex};

/// Состояние Qdrant для `/index/status`, деградации поиска и воркера индексации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QdrantState {
    /// Процесс запущен, готовность ещё не подтверждена.
    Starting,
    /// `/readyz`, версия и gRPC health в порядке. `rebuilt` — хранилище стёрто после
    /// последнего [`Supervisor::ack_storage_rebuilt`]: индекс пуст, нужна пересборка из
    /// `embeddings` (воркер реагирует на переход в это состояние).
    Ready { version: String, rebuilt: bool },
    /// Процесс упал; следующий запуск через `next_in`. `attempt` — номер падения в окне.
    Restarting { attempt: u32, next_in: Duration },
    /// Перезапусков больше не будет (circuit breaker, чужая версия, нет бинарника).
    Broken { reason: String },
    /// Внешний Qdrant (`mode = "external"`).
    External,
}

/// Общие с потоком супервизора поля.
struct Shared {
    qdir: PathBuf,
    /// Издатель состояния: у потока и у [`Supervisor::ack_storage_rebuilt`].
    state: watch::Sender<QdrantState>,
    storage_rebuilt: AtomicBool,
    /// pid текущего процесса; 0 — процесса нет.
    pid: AtomicU32,
    /// Перезапусков после падения с момента старта супервизора (`/index/status`).
    restarts: AtomicU32,
    /// Точка остановки тестов перед перезапуском после падения (фича `test-hooks`).
    #[cfg(feature = "test-hooks")]
    restart_hold: crate::test_hooks::Hold,
}

/// Супервизор процесса Qdrant (или описание внешнего Qdrant).
pub struct Supervisor {
    state: watch::Receiver<QdrantState>,
    /// Только для `External`: держит канал состояния открытым.
    _state_tx: Option<watch::Sender<QdrantState>>,
    grpc_url: String,
    api_key: Option<String>,
    shared: Option<Arc<Shared>>,
    stop: Option<watch::Sender<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ключ наружу не выводим.
        f.debug_struct("Supervisor")
            .field("grpc_url", &self.grpc_url)
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Запускает супервизор и возвращается, не дожидаясь готовности Qdrant.
    /// Ошибка — только если каталог занят другим супервизором ([`IndexError::Locked`])
    /// или недоступен на запись; отсутствие бинарника и падения отражаются в состоянии.
    pub async fn start(cfg: SupervisorConfig, data_dir: &Path) -> Result<Supervisor, IndexError> {
        let m = match cfg {
            SupervisorConfig::External(e) => {
                let (tx, rx) = watch::channel(QdrantState::External);
                return Ok(Supervisor {
                    state: rx,
                    _state_tx: Some(tx),
                    grpc_url: e.grpc_url,
                    api_key: e.api_key,
                    shared: None,
                    stop: None,
                    thread: None,
                });
            }
            SupervisorConfig::Managed(m) => m,
        };
        let qdir = data_dir.join("qdrant");
        let io = |what: &str, e: std::io::Error| IndexError::Process(format!("{what}: {e}"));
        std::fs::create_dir_all(qdir.join("config"))
            .map_err(|e| io(&format!("каталог {}", qdir.display()), e))?;
        let lock = process::acquire_lock(&qdir)?;

        // Сирота и метка — блокирующие операции (до 10 с ожидания SIGTERM).
        let (m2, q2) = (m.clone(), qdir.clone());
        let rebuilt = tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
            process::cleanup_orphan(
                &q2,
                &m2.binary,
                &[m2.http_port, m2.grpc_port],
                m2.stop_timeout,
            );
            let rebuilt = stamp::check_before_start(&q2, &m2.expected_version)?;
            std::fs::write(
                q2.join("config").join("zam.yaml"),
                config::render_yaml(&m2, &q2)?,
            )?;
            Ok(rebuilt)
        })
        .await
        .map_err(|e| IndexError::Process(format!("подготовка: {e}")))?
        .map_err(|e| io("подготовка каталога Qdrant", e))?;

        let mut rng = rand::rng();
        let api_key = format!("{:032x}{:032x}", rng.random::<u128>(), rng.random::<u128>());
        let grpc_url = format!("http://127.0.0.1:{}", m.grpc_port);
        let (state_tx, state_rx) = watch::channel(QdrantState::Starting);
        let shared = Arc::new(Shared {
            qdir,
            state: state_tx,
            storage_rebuilt: AtomicBool::new(rebuilt),
            pid: AtomicU32::new(0),
            restarts: AtomicU32::new(0),
            #[cfg(feature = "test-hooks")]
            restart_hold: crate::test_hooks::Hold::default(),
        });
        let (stop_tx, stop_rx) = watch::channel(false);
        let ctx = Ctx {
            cfg: m,
            shared: shared.clone(),
            api_key: Arc::from(api_key.as_str()),
            grpc_url: grpc_url.clone(),
            stop: stop_rx,
        };
        // Долгоживущий поток: на Linux PDEATHSIG привязан к потоку, сделавшему fork.
        let thread = std::thread::Builder::new()
            .name("qdrant-supervisor".into())
            .spawn(move || run_thread(ctx, lock))
            .map_err(|e| io("поток qdrant-supervisor", e))?;
        Ok(Supervisor {
            state: state_rx,
            _state_tx: None,
            grpc_url,
            api_key: Some(api_key),
            shared: Some(shared),
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    /// Подписка на состояние.
    pub fn state(&self) -> watch::Receiver<QdrantState> {
        self.state.clone()
    }

    /// gRPC-адрес для [`QdrantIndex::connect`].
    pub fn grpc_url(&self) -> String {
        self.grpc_url.clone()
    }

    /// API-ключ: managed — случайный ключ этого запуска; external — из конфигурации.
    pub fn api_key(&self) -> Option<String> {
        self.api_key.clone()
    }

    /// pid текущего процесса Qdrant (managed, процесс запущен).
    pub fn pid(&self) -> Option<u32> {
        let pid = self.shared.as_ref()?.pid.load(Ordering::SeqCst);
        (pid != 0).then_some(pid)
    }

    /// Процесс Qdrant жив: managed — pid есть и процесс не завершён (проверка ОС, без
    /// обращения к Qdrant); external — всегда `true`. Поиск сверяется с ним при состоянии
    /// `Ready`: между смертью процесса и сменой состояния супервизором (ожидание журнала —
    /// до 2 с) запрос иначе ушёл бы в закрытый порт, а на Windows отказ соединения с
    /// localhost приходит только через ~2 с на попытку.
    pub fn process_alive(&self) -> bool {
        match &self.shared {
            None => true,
            Some(_) => self.pid().is_some_and(pid_alive),
        }
    }

    /// Перезапусков Qdrant после падения с момента старта (external — 0).
    pub fn restarts(&self) -> u32 {
        self.shared
            .as_ref()
            .map_or(0, |s| s.restarts.load(Ordering::SeqCst))
    }

    /// Резидентная память процесса Qdrant из ОС, байт (метрика `zam_qdrant_rss_bytes`,
    /// §17): Windows — `GetProcessMemoryInfo`, Linux — `/proc/<pid>/statm`. Нет процесса
    /// (external, ещё не запущен) — `None`.
    pub fn rss_bytes(&self) -> Option<u64> {
        rss_bytes(self.pid()?)
    }

    /// Хранилище стёрто (метка версии, panic хранилища или `qdrant reset`) и пересборка
    /// индекса из `embeddings` ещё не подтверждена через [`Self::ack_storage_rebuilt`].
    pub fn storage_rebuilt(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|s| s.storage_rebuilt.load(Ordering::SeqCst))
    }

    /// Подтверждает, что пересборка поставлена в очередь (задания reindex записаны в SQLite):
    /// снимает флаг `.zam-reindex`.
    pub fn ack_storage_rebuilt(&self) -> Result<(), IndexError> {
        let Some(s) = &self.shared else {
            return Ok(());
        };
        stamp::clear_reindex(&s.qdir)
            .map_err(|e| IndexError::Process(format!("флаг пересборки: {e}")))?;
        s.storage_rebuilt.store(false, Ordering::SeqCst);
        s.state.send_if_modified(|st| match st {
            QdrantState::Ready { rebuilt, .. } if *rebuilt => {
                *rebuilt = false;
                true
            }
            _ => false,
        });
        Ok(())
    }

    /// Останавливает Qdrant и ждёт завершения потока супервизора.
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(true);
        }
        if let Some(t) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || t.join()).await;
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // Без shutdown: поток остановит Qdrant сам, но ждать его здесь нельзя.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(true);
        }
    }
}

/// Контекст потока супервизора.
struct Ctx {
    cfg: ManagedConfig,
    shared: Arc<Shared>,
    api_key: Arc<str>,
    grpc_url: String,
    stop: watch::Receiver<bool>,
}

impl Ctx {
    fn set(&self, s: QdrantState) {
        self.shared.state.send_replace(s);
    }

    fn broken(&self, reason: String) {
        publish_broken(&self.shared.state, reason);
    }
}

fn publish_broken(state: &watch::Sender<QdrantState>, reason: String) {
    tracing::error!(component = "qdrant", reason = %reason, "Qdrant выключен (Broken)");
    state.send_replace(QdrantState::Broken { reason });
}

/// Причина `Broken` при панике потока супервизора.
const PANIC_REASON: &str =
    "supervisor thread panicked: поток супервизора Qdrant завершился паникой";

/// Выполняет `f`; паника публикует `Broken`, а не оставляет устаревший `Ready`.
/// Процесс Qdrant при раскрутке стека уничтожается (`KillOnDrop`).
fn guard_panic(state: &watch::Sender<QdrantState>, f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        publish_broken(state, PANIC_REASON.into());
    }
}

/// Тело потока: свой runtime, цикл запусков, лок до полной остановки процесса.
fn run_thread(mut ctx: Ctx, lock: std::fs::File) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            ctx.broken(format!("runtime супервизора: {e}"));
            return;
        }
    };
    let state = ctx.shared.state.clone();
    guard_panic(&state, || rt.block_on(supervise(&mut ctx)));
    process::remove_pid_file(&ctx.shared.qdir);
    ctx.shared.pid.store(0, Ordering::SeqCst);
    drop(lock);
    rt.shutdown_timeout(Duration::from_secs(1));
}

/// Ждёт запроса остановки (или закрытия канала — супервизор уничтожен).
async fn wait_stop(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// Исход одного запуска процесса.
enum Outcome {
    /// Остановлен по запросу.
    Stopped,
    /// Перезапуск бесполезен.
    Fatal(String),
    /// Процесс упал или не стал готов.
    Crashed { reason: String, storage_panic: bool },
}

async fn supervise(ctx: &mut Ctx) {
    if !ctx.cfg.binary.is_file() {
        ctx.broken(format!(
            "нет бинарника Qdrant {}: выполните `zam-server qdrant fetch`",
            ctx.cfg.binary.display()
        ));
        wait_stop(&mut ctx.stop).await;
        return;
    }
    let policy = ctx.cfg.restart.clone();
    let mut crashes: VecDeque<Instant> = VecDeque::new();
    let mut panic_wipe_used = false;
    loop {
        if *ctx.stop.borrow() {
            return;
        }
        ctx.set(QdrantState::Starting);
        match run_once(ctx).await {
            Outcome::Stopped => return,
            Outcome::Fatal(reason) => {
                ctx.broken(reason);
                wait_stop(&mut ctx.stop).await;
                return;
            }
            Outcome::Crashed {
                reason,
                storage_panic,
            } => {
                if storage_panic && !panic_wipe_used {
                    panic_wipe_used = true;
                    tracing::warn!(
                        component = "qdrant",
                        reason = %reason,
                        "Qdrant не открыл хранилище (panic версии): стираю и перезапускаю"
                    );
                    let q = ctx.shared.qdir.clone();
                    let wiped = tokio::task::spawn_blocking(move || {
                        stamp::wipe_storage(&q, "panic несовместимого хранилища")
                    })
                    .await;
                    match wiped {
                        Ok(Ok(())) => {
                            ctx.shared.storage_rebuilt.store(true, Ordering::SeqCst);
                            ctx.shared.restarts.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                        Ok(Err(e)) => {
                            ctx.broken(format!("не удалось стереть хранилище: {e}"));
                            wait_stop(&mut ctx.stop).await;
                            return;
                        }
                        Err(e) => {
                            ctx.broken(format!("не удалось стереть хранилище: {e}"));
                            wait_stop(&mut ctx.stop).await;
                            return;
                        }
                    }
                }
                let now = Instant::now();
                crashes.push_back(now);
                while crashes
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > policy.window)
                {
                    crashes.pop_front();
                }
                let n = crashes.len() as u32;
                if n >= policy.max_crashes {
                    ctx.broken(format!(
                        "{n} падений за {} мин, перезапуски остановлены; последнее: {reason}",
                        policy.window.as_secs().div_ceil(60)
                    ));
                    wait_stop(&mut ctx.stop).await;
                    return;
                }
                let next_in = policy.delay(n, rand::rng().random_range(0.8..=1.2));
                tracing::warn!(
                    component = "qdrant",
                    attempt = n,
                    next_in_ms = next_in.as_millis() as u64,
                    reason = %reason,
                    "Qdrant упал, перезапуск"
                );
                ctx.set(QdrantState::Restarting {
                    attempt: n,
                    next_in,
                });
                ctx.shared.restarts.fetch_add(1, Ordering::SeqCst);
                // тесты: Qdrant лежит, пока тест не отпустит (состояние уже Restarting)
                #[cfg(feature = "test-hooks")]
                tokio::select! {
                    () = ctx.shared.restart_hold.park() => {}
                    () = wait_stop(&mut ctx.stop) => return,
                }
                tokio::select! {
                    () = tokio::time::sleep(next_in) => {}
                    () = wait_stop(&mut ctx.stop) => return,
                }
            }
        }
    }
}

/// Готовность запущенного процесса.
enum Readiness {
    Ready(String),
    WrongVersion(String),
}

/// Опрос готовности: `/readyz` → `GET /` (версия) → gRPC health с ключом.
async fn wait_ready(ctx: &Ctx) -> Readiness {
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut client: Option<QdrantIndex> = None;
    loop {
        tick.tick().await;
        if !http::readyz(ctx.cfg.http_port).await {
            continue;
        }
        let Some(version) = http::version(ctx.cfg.http_port).await else {
            continue;
        };
        if version != ctx.cfg.expected_version {
            return Readiness::WrongVersion(version);
        }
        if client.is_none() {
            client = QdrantIndex::connect(&ctx.grpc_url, Some(&ctx.api_key)).ok();
        }
        if let Some(c) = &client
            && c.health().await.is_ok()
        {
            return Readiness::Ready(version);
        }
    }
}

/// Дожидается конца пересылки журнала (EOF после выхода процесса), чтобы текст panic
/// успел попасть в детектор.
async fn drain(tasks: Vec<tokio::task::JoinHandle<()>>) {
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        for t in tasks {
            let _ = t.await;
        }
    })
    .await;
}

/// Один запуск процесса: от spawn до выхода или остановки.
async fn run_once(ctx: &Ctx) -> Outcome {
    let crashed = |reason: String| Outcome::Crashed {
        reason,
        storage_panic: false,
    };
    for port in [ctx.cfg.http_port, ctx.cfg.grpc_port] {
        if !process::port_free(port) {
            return crashed(format!("порт 127.0.0.1:{port} занят другим процессом"));
        }
    }
    let started = Instant::now();
    let mut child = match process::spawn(&ctx.cfg, &ctx.shared.qdir, &ctx.api_key) {
        Ok(c) => c,
        Err(e) => return crashed(format!("запуск {}: {e}", ctx.cfg.binary.display())),
    };
    let pid = child.id().unwrap_or(0);
    ctx.shared.pid.store(pid, Ordering::SeqCst);
    if let Err(e) = process::write_pid_file(&ctx.shared.qdir, pid) {
        tracing::warn!(component = "qdrant", error = %e, "не удалось записать pid-файл");
    }
    tracing::info!(component = "qdrant", pid, "Qdrant запущен");

    let panic = Arc::new(AtomicBool::new(false));
    let mut logs = Vec::new();
    if let Some(out) = child.stdout().take() {
        logs.push(tokio::spawn(process::forward(
            out,
            "stdout",
            ctx.api_key.clone(),
            panic.clone(),
        )));
    }
    if let Some(err) = child.stderr().take() {
        logs.push(tokio::spawn(process::forward(
            err,
            "stderr",
            ctx.api_key.clone(),
            panic.clone(),
        )));
    }

    enum Startup {
        Ready(String),
        WrongVersion(String),
        Exited(String),
        Stop,
        Timeout,
    }
    let startup = {
        let ready = wait_ready(ctx);
        let mut stop = ctx.stop.clone();
        tokio::select! {
            r = ready => match r {
                Readiness::Ready(v) => Startup::Ready(v),
                Readiness::WrongVersion(v) => Startup::WrongVersion(v),
            },
            st = child.wait() => Startup::Exited(match st {
                Ok(s) => s.to_string(),
                Err(e) => e.to_string(),
            }),
            () = wait_stop(&mut stop) => Startup::Stop,
            () = tokio::time::sleep(ctx.cfg.ready_timeout) => Startup::Timeout,
        }
    };
    let grace = ctx.cfg.stop_timeout;
    let version = match startup {
        Startup::Ready(v) => v,
        Startup::WrongVersion(v) => {
            process::stop_child(&mut child, grace).await;
            ctx.shared.pid.store(0, Ordering::SeqCst);
            return Outcome::Fatal(format!(
                "версия Qdrant {v} не совпадает с закреплённой {} (deploy/qdrant.toml)",
                ctx.cfg.expected_version
            ));
        }
        Startup::Exited(st) => {
            ctx.shared.pid.store(0, Ordering::SeqCst);
            drain(logs).await;
            return Outcome::Crashed {
                reason: format!("процесс завершился при старте: {st}"),
                storage_panic: panic.load(Ordering::SeqCst),
            };
        }
        Startup::Stop => {
            process::stop_child(&mut child, grace).await;
            ctx.shared.pid.store(0, Ordering::SeqCst);
            return Outcome::Stopped;
        }
        Startup::Timeout => {
            process::stop_child(&mut child, grace).await;
            ctx.shared.pid.store(0, Ordering::SeqCst);
            drain(logs).await;
            return Outcome::Crashed {
                reason: format!("не готов за {} с", ctx.cfg.ready_timeout.as_secs()),
                storage_panic: panic.load(Ordering::SeqCst),
            };
        }
    };

    if let Err(e) = stamp::write_stamp(&ctx.shared.qdir, &version) {
        tracing::warn!(component = "qdrant", error = %e, "не удалось записать метку хранилища");
    }
    tracing::info!(
        component = "qdrant",
        pid,
        version = %version,
        ready_ms = started.elapsed().as_millis() as u64,
        "Qdrant готов"
    );
    ctx.set(QdrantState::Ready {
        version,
        rebuilt: ctx.shared.storage_rebuilt.load(Ordering::SeqCst),
    });

    let mut stop = ctx.stop.clone();
    let exited = tokio::select! {
        st = child.wait() => Some(match st {
            Ok(s) => s.to_string(),
            Err(e) => e.to_string(),
        }),
        () = wait_stop(&mut stop) => None,
    };
    ctx.shared.pid.store(0, Ordering::SeqCst);
    match exited {
        Some(st) => {
            drain(logs).await;
            Outcome::Crashed {
                reason: format!("процесс завершился: {st}"),
                storage_panic: panic.load(Ordering::SeqCst),
            }
        }
        None => {
            process::stop_child(&mut child, grace).await;
            tracing::info!(component = "qdrant", pid, "Qdrant остановлен");
            Outcome::Stopped
        }
    }
}

// ---------------------------------------------------------------- CLI: status / reset

/// Состояние каталога Qdrant для `zam-server qdrant status`; ничего не запускает.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantStatus {
    pub qdir: PathBuf,
    /// Лок каталога удерживается (супервизор работает).
    pub locked: bool,
    /// pid из pid-файла.
    pub pid: Option<u32>,
    /// Процесс из pid-файла жив и это Qdrant.
    pub pid_alive: bool,
    /// Версия из метки хранилища.
    pub stamp_version: Option<String>,
    pub storage_present: bool,
    /// Хранилище стёрто, пересборка не подтверждена.
    pub reindex_pending: bool,
    /// Версия из `GET /`, если на порту отвечает Qdrant.
    pub running_version: Option<String>,
    /// `/readyz` отвечает 200.
    pub ready: bool,
}

/// Читает pid-файл, лок, метку и, если процесс отвечает, `/readyz` и `GET /`.
pub async fn inspect(data_dir: &Path, http_port: u16) -> QdrantStatus {
    let qdir = data_dir.join("qdrant");
    let pid = process::read_pid_file(&qdir);
    let ready = http::readyz(http_port).await;
    let running_version = http::version(http_port).await;
    QdrantStatus {
        locked: process::lock_held(&qdir),
        pid,
        pid_alive: pid.is_some_and(|p| pid_alive(p) && is_qdrant_process(p)),
        stamp_version: stamp::read_stamp(&qdir).ok().flatten(),
        storage_present: qdir.join(stamp::STORAGE_DIR).exists(),
        reindex_pending: stamp::reindex_pending(&qdir).unwrap_or(false),
        running_version,
        ready,
        qdir,
    }
}

/// `zam-server qdrant reset`: стирает хранилище остановленного Qdrant; при следующем
/// старте [`Supervisor::storage_rebuilt`] — индекс пересобирается из `embeddings`.
/// Работающий супервизор — [`IndexError::Locked`]; осиротевший процесс (без лока)
/// завершается той же проверкой, что и при старте. Если после этого жив процесс Qdrant
/// (из pid-файла или отвечающий на нашем HTTP-порту) и наш порт занят, хранилище не
/// стирается ([`IndexError::Process`]): из-под живого процесса файлы не удаляются.
pub fn reset_storage(data_dir: &Path, cfg: &ManagedConfig) -> Result<(), IndexError> {
    let qdir = data_dir.join("qdrant");
    if !qdir
        .try_exists()
        .map_err(|e| IndexError::Process(format!("каталог {}: {e}", qdir.display())))?
    {
        return Ok(());
    }
    let _lock = process::acquire_lock(&qdir)?;
    let pid = process::read_pid_file(&qdir);
    let ports = [cfg.http_port, cfg.grpc_port];
    process::cleanup_orphan(&qdir, &cfg.binary, &ports, cfg.stop_timeout);
    if let Some(port) = ports.iter().copied().find(|p| !process::port_free(*p)) {
        let live_pid = pid.filter(|p| pid_alive(*p) && is_qdrant_process(*p));
        // процесс мог только открыть порт: несколько попыток до ответа `GET /`
        let answers = (0..10).find_map(|i| {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(300));
            }
            http::version_blocking(cfg.http_port)
        });
        if live_pid.is_some() || answers.is_some() {
            let who = match (live_pid, answers) {
                (Some(p), _) => format!("процесс Qdrant pid {p}"),
                (None, Some(v)) => format!("Qdrant {v} на 127.0.0.1:{}", cfg.http_port),
                (None, None) => unreachable!("проверено выше"),
            };
            return Err(IndexError::Process(format!(
                "{who} работает, порт 127.0.0.1:{port} занят: остановите его, хранилище не стёрто"
            )));
        }
    }
    stamp::wipe_storage(&qdir, "zam-server qdrant reset")
        .map_err(|e| IndexError::Process(format!("стирание хранилища: {e}")))
}

/// Точка остановки перед перезапуском упавшего Qdrant (фича `test-hooks`, см.
/// [`crate::test_hooks`]): состояние уже `Restarting`, процесса нет.
#[cfg(feature = "test-hooks")]
impl Supervisor {
    /// Взвести: следующий перезапуск после падения ждёт [`Self::release_restart`].
    #[doc(hidden)]
    pub fn hold_next_restart(&self) {
        if let Some(s) = &self.shared {
            s.restart_hold.arm();
        }
    }

    /// Ждёт, пока супервизор встанет на точке; `false` — не встал за `limit`.
    #[doc(hidden)]
    pub async fn wait_restart_held(&self, limit: Duration) -> bool {
        match &self.shared {
            Some(s) => s.restart_hold.wait_parked(limit).await,
            None => false,
        }
    }

    /// Отпустить перезапуск (и снять взвод — на случай паники теста).
    #[doc(hidden)]
    pub fn release_restart(&self) {
        if let Some(s) = &self.shared {
            s.restart_hold.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_panic_publishes_broken() {
        let (tx, rx) = watch::channel(QdrantState::Ready {
            version: "1.19.2".into(),
            rebuilt: false,
        });
        guard_panic(&tx, || panic!("тестовая паника"));
        assert_eq!(
            *rx.borrow(),
            QdrantState::Broken {
                reason: PANIC_REASON.into()
            }
        );
        // Без паники состояние не трогается.
        tx.send_replace(QdrantState::Starting);
        guard_panic(&tx, || {});
        assert_eq!(*rx.borrow(), QdrantState::Starting);
    }
}
