//! Служба Windows (SCM, §13.3, §16.2).
//!
//! `install` регистрирует службу (`OwnProcess`, автозапуск, LocalSystem,
//! `binPath = "<exe>" service run --config <абсолютный путь>`, описание, восстановление —
//! перезапуск через 10 с, три раза; таймаут `Preshutdown` — [`super::stop_timeout`]).
//! `run` — точка входа SCM: `service_dispatcher::start`, команды `Stop`/`Shutdown`/`Preshutdown`
//! ведут в общий сигнал остановки сервера; пока идёт остановка — `StopPending` с `wait_hint`
//! 10 с и `checkpoint`, растущим каждые 2 с до выхода (R4).
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_dispatcher;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use super::{ServiceError, validate_name};
use crate::config::CliOverrides;
use crate::{RunMode, RunOptions, ShutdownSignal, StopSource};

/// `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: i32 = 5;
/// `ERROR_SERVICE_DOES_NOT_EXIST`.
const DOES_NOT_EXIST: i32 = 1060;
/// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`: процесс запущен не диспетчером служб.
const NOT_FROM_SCM: i32 = 1063;
/// `wait_hint` состояний `StartPending`/`StopPending` (R4).
const WAIT_HINT: Duration = Duration::from_secs(10);
/// Период роста `checkpoint` (R4).
const CHECKPOINT_EVERY: Duration = Duration::from_secs(2);
/// Восстановление: перезапуск через 10 с, три раза; счётчик сбоев сбрасывается через сутки.
const RESTART_DELAY: Duration = Duration::from_secs(10);
const RESTARTS: usize = 3;
const RESET_PERIOD: Duration = Duration::from_secs(86_400);
/// Описание службы в оснастке «Службы».
const DESCRIPTION: &str = "Сервер ZeroAIMemory: память для ИИ-агентов (REST, MCP, WebUI)";
/// Файл ошибки старта службы до журнала сервера (рядом с exe или в `data_dir/logs`).
pub const START_ERROR_FILE: &str = "zam-server-service-error.log";

fn os_err(e: &windows_service::Error) -> Option<i32> {
    match e {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

/// Ошибка вызова SCM: нет прав — код 3, нет службы — код 1 с понятным текстом.
fn scm_err(what: &str, name: &str, e: windows_service::Error) -> ServiceError {
    match os_err(&e) {
        Some(ACCESS_DENIED) => ServiceError::denied(format!(
            "{what}: нужны права администратора — запустите PowerShell или cmd «от имени администратора»"
        )),
        Some(DOES_NOT_EXIST) => ServiceError::failed(format!("служба «{name}» не установлена")),
        _ => {
            let detail = match &e {
                windows_service::Error::Winapi(io) => io.to_string(),
                other => other.to_string(),
            };
            ServiceError::failed(format!("{what}: {detail}"))
        }
    }
}

fn manager(
    access: ServiceManagerAccess,
    what: &str,
    name: &str,
) -> Result<ServiceManager, ServiceError> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|e| scm_err(what, name, e))
}

/// Параметры регистрации службы.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub name: String,
    pub exe: PathBuf,
    pub config: PathBuf,
    pub timeout: Duration,
    pub logs_dir: PathBuf,
}

impl InstallPlan {
    fn launch_arguments(&self) -> Vec<OsString> {
        vec![
            "service".into(),
            "run".into(),
            "--config".into(),
            self.config.clone().into_os_string(),
        ]
    }

    /// Описание для `--print`: что будет зарегистрировано.
    pub fn describe(&self) -> String {
        format!(
            "служба: {name} ({display})\n\
             binPath: \"{exe}\" service run --config \"{cfg}\"\n\
             запуск: автоматически, учётная запись LocalSystem, тип OwnProcess\n\
             восстановление: перезапуск через {rd} с, {n} раза (счётчик сбоев сбрасывается через сутки)\n\
             таймаут остановки (Preshutdown): {t} с; StopPending: wait_hint {wh} с, checkpoint каждые {cp} с\n\
             журнал: {logs}\n",
            name = self.name,
            display = display_name(&self.name),
            exe = self.exe.display(),
            cfg = self.config.display(),
            rd = RESTART_DELAY.as_secs(),
            n = RESTARTS,
            t = self.timeout.as_secs(),
            wh = WAIT_HINT.as_secs(),
            cp = CHECKPOINT_EVERY.as_secs(),
            logs = self.logs_dir.display(),
        )
    }
}

fn display_name(name: &str) -> String {
    if name == super::DEFAULT_NAME {
        "ZeroAIMemory".into()
    } else {
        format!("ZeroAIMemory ({name})")
    }
}

/// Таймаут `Preshutdown` службы (мс).
fn set_preshutdown(service: &Service, timeout: Duration) -> Result<(), ServiceError> {
    use windows::Win32::System::Services::{
        ChangeServiceConfig2W, SC_HANDLE, SERVICE_CONFIG_PRESHUTDOWN_INFO, SERVICE_PRESHUTDOWN_INFO,
    };
    let info = SERVICE_PRESHUTDOWN_INFO {
        dwPreshutdownTimeout: u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX),
    };
    // SAFETY: дескриптор службы жив, пока жив `service`; структура живёт до конца вызова.
    unsafe {
        ChangeServiceConfig2W(
            SC_HANDLE(service.raw_handle() as _),
            SERVICE_CONFIG_PRESHUTDOWN_INFO,
            Some(std::ptr::from_ref(&info).cast()),
        )
    }
    .map_err(|e| ServiceError::failed(format!("таймаут Preshutdown: {e}")))
}

/// Таймаут `Preshutdown` открытой службы.
fn query_preshutdown(service: &Service) -> Result<Duration, String> {
    use windows::Win32::System::Services::{
        QueryServiceConfig2W, SC_HANDLE, SERVICE_CONFIG_PRESHUTDOWN_INFO, SERVICE_PRESHUTDOWN_INFO,
    };
    let mut buf = [0u8; std::mem::size_of::<SERVICE_PRESHUTDOWN_INFO>()];
    let mut needed = 0u32;
    // SAFETY: буфер нужного размера, дескриптор жив.
    unsafe {
        QueryServiceConfig2W(
            SC_HANDLE(service.raw_handle() as _),
            SERVICE_CONFIG_PRESHUTDOWN_INFO,
            Some(&mut buf),
            &mut needed,
        )
    }
    .map_err(|e| format!("QueryServiceConfig2W: {e}"))?;
    Ok(Duration::from_millis(u64::from(u32::from_ne_bytes(buf))))
}

/// Таймаут остановки собственной службы — для проверки при старте.
pub(super) fn preshutdown_timeout(name: &str) -> Result<Duration, String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("SCM: {e:?}"))?;
    let s = m
        .open_service(name, ServiceAccess::QUERY_CONFIG)
        .map_err(|e| format!("служба {name}: {e:?}"))?;
    query_preshutdown(&s)
}

/// Регистрирует службу или обновляет параметры уже зарегистрированной (повторный `install`
/// исправляет таймаут после смены `qdrant.shutdown_flush_wait_ms`).
pub fn install(plan: &InstallPlan) -> Result<(), ServiceError> {
    validate_name(&plan.name)?;
    let what = "регистрация службы";
    let m = manager(
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        what,
        &plan.name,
    )?;
    let info = ServiceInfo {
        name: plan.name.clone().into(),
        display_name: display_name(&plan.name).into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: plan.exe.clone(),
        launch_arguments: plan.launch_arguments(),
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    // START — для действия восстановления «перезапуск»
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::QUERY_CONFIG
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::START;
    let (service, updated) = match m.open_service(&plan.name, access) {
        Ok(s) => {
            s.change_config(&info)
                .map_err(|e| scm_err(what, &plan.name, e))?;
            (s, true)
        }
        Err(e) if os_err(&e) == Some(DOES_NOT_EXIST) => (
            m.create_service(&info, access)
                .map_err(|e| scm_err(what, &plan.name, e))?,
            false,
        ),
        Err(e) => return Err(scm_err(what, &plan.name, e)),
    };
    service
        .set_description(DESCRIPTION)
        .map_err(|e| scm_err(what, &plan.name, e))?;
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(RESET_PERIOD),
            reboot_msg: None,
            command: None,
            actions: Some(vec![
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: RESTART_DELAY,
                };
                RESTARTS
            ]),
        })
        .map_err(|e| scm_err(what, &plan.name, e))?;
    // выход с ненулевым кодом (сбой старта) — тоже сбой для восстановления
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| scm_err(what, &plan.name, e))?;
    set_preshutdown(&service, plan.timeout)?;
    println!(
        "{} служба «{}»",
        if updated {
            "обновлена"
        } else {
            "зарегистрирована"
        },
        plan.name
    );
    print!("{}", plan.describe());
    if updated {
        println!(
            "параметры применятся при следующем запуске: zam-server service stop, затем start"
        );
    } else {
        println!("запуск: zam-server service start --name {}", plan.name);
    }
    Ok(())
}

/// Ждёт состояния `want`, пока служба в `pending`; `None` — истёк `limit`.
fn wait_state(
    s: &Service,
    pending: ServiceState,
    limit: Duration,
) -> Result<Option<ServiceStatus>, windows_service::Error> {
    let t0 = Instant::now();
    loop {
        let st = s.query_status()?;
        if st.current_state != pending {
            return Ok(Some(st));
        }
        if t0.elapsed() > limit {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Предел ожидания остановки из командной строки: наибольший таймаут R4 и запас.
const STOP_WAIT_LIMIT: Duration = Duration::from_secs(150);
/// Предел ожидания запуска (загрузка модели с HDD, CUDA).
const START_WAIT_LIMIT: Duration = Duration::from_secs(180);

fn stop_and_wait(s: &Service, name: &str) -> Result<(), ServiceError> {
    let st = s
        .query_status()
        .map_err(|e| scm_err("состояние службы", name, e))?;
    if st.current_state == ServiceState::Stopped {
        return Ok(());
    }
    if st.current_state != ServiceState::StopPending {
        s.stop().map_err(|e| scm_err("остановка службы", name, e))?;
    }
    println!("остановка «{name}»: сервер дожидается записи индекса и останавливает Qdrant…");
    match wait_state(s, ServiceState::StopPending, STOP_WAIT_LIMIT)
        .map_err(|e| scm_err("остановка службы", name, e))?
    {
        Some(st) if st.current_state == ServiceState::Stopped => Ok(()),
        Some(st) => Err(ServiceError::failed(format!(
            "служба «{name}» в состоянии {:?}",
            st.current_state
        ))),
        None => Err(ServiceError::failed(format!(
            "служба «{name}» не остановилась за {} с",
            STOP_WAIT_LIMIT.as_secs()
        ))),
    }
}

pub fn uninstall(name: &str) -> Result<(), ServiceError> {
    validate_name(name)?;
    let what = "удаление службы";
    let m = manager(ServiceManagerAccess::CONNECT, what, name)?;
    let s = m
        .open_service(
            name,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )
        .map_err(|e| scm_err(what, name, e))?;
    stop_and_wait(&s, name)?;
    s.delete().map_err(|e| scm_err(what, name, e))?;
    println!("служба «{name}» удалена");
    Ok(())
}

pub fn start(name: &str) -> Result<(), ServiceError> {
    validate_name(name)?;
    let what = "запуск службы";
    let m = manager(ServiceManagerAccess::CONNECT, what, name)?;
    let s = m
        .open_service(name, ServiceAccess::QUERY_STATUS | ServiceAccess::START)
        .map_err(|e| scm_err(what, name, e))?;
    let st = s.query_status().map_err(|e| scm_err(what, name, e))?;
    if st.current_state == ServiceState::Running {
        println!("служба «{name}» уже работает");
        return Ok(());
    }
    s.start::<&OsStr>(&[]).map_err(|e| scm_err(what, name, e))?;
    match wait_state(&s, ServiceState::StartPending, START_WAIT_LIMIT)
        .map_err(|e| scm_err(what, name, e))?
    {
        Some(st) if st.current_state == ServiceState::Running => {
            println!(
                "служба «{name}» запущена (pid {})",
                st.process_id.unwrap_or(0)
            );
            Ok(())
        }
        Some(st) => Err(ServiceError::failed(format!(
            "служба «{name}» не запустилась ({:?}, код {:?}): журнал — data_dir\\logs, \
             ошибка до журнала — {START_ERROR_FILE} рядом с zam-server.exe",
            st.current_state, st.exit_code
        ))),
        None => Err(ServiceError::failed(format!(
            "служба «{name}» не перешла в «работает» за {} с",
            START_WAIT_LIMIT.as_secs()
        ))),
    }
}

pub fn stop(name: &str) -> Result<(), ServiceError> {
    validate_name(name)?;
    let what = "остановка службы";
    let m = manager(ServiceManagerAccess::CONNECT, what, name)?;
    let s = m
        .open_service(name, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP)
        .map_err(|e| scm_err(what, name, e))?;
    stop_and_wait(&s, name)?;
    println!("служба «{name}» остановлена");
    Ok(())
}

pub fn status(name: &str) -> Result<(), ServiceError> {
    validate_name(name)?;
    let what = "состояние службы";
    let m = manager(ServiceManagerAccess::CONNECT, what, name)?;
    let s = m
        .open_service(
            name,
            ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
        )
        .map_err(|e| scm_err(what, name, e))?;
    let st = s.query_status().map_err(|e| scm_err(what, name, e))?;
    let state = match st.current_state {
        ServiceState::Running => "работает",
        ServiceState::Stopped => "остановлена",
        ServiceState::StartPending => "запускается",
        ServiceState::StopPending => "останавливается",
        _ => "приостановлена",
    };
    println!("служба: {name}");
    match st.process_id {
        Some(pid) if pid != 0 => println!("состояние: {state} (pid {pid})"),
        _ => println!("состояние: {state}"),
    }
    if let Ok(c) = s.query_config() {
        println!("binPath: {}", c.executable_path.display());
    }
    match query_preshutdown(&s) {
        Ok(t) => println!("таймаут остановки (Preshutdown): {} с", t.as_secs()),
        Err(e) => println!("таймаут остановки (Preshutdown): не определён ({e})"),
    }
    Ok(())
}

/// Параметры `service run`.
#[derive(Debug, Clone, Default)]
pub struct RunArgs {
    pub config: Option<PathBuf>,
    pub cli: CliOverrides,
}

static RUN_ARGS: OnceLock<RunArgs> = OnceLock::new();

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Точка входа SCM. Из консоли — ошибка «запускается только менеджером служб», код 2.
pub fn run(args: RunArgs) -> Result<(), ServiceError> {
    let _ = RUN_ARGS.set(args);
    // имя для OwnProcess диспетчер не сверяет; своё имя служба получает в аргументах main
    service_dispatcher::start(super::DEFAULT_NAME, ffi_service_main).map_err(|e| {
        if os_err(&e) == Some(NOT_FROM_SCM) {
            ServiceError::usage(
                "`zam-server service run` запускается только менеджером служб Windows: \
                 используйте `zam-server service start` или `zam-server serve`",
            )
        } else {
            ServiceError::failed(format!("диспетчер служб: {e:?}"))
        }
    })
}

/// Отчёт о состоянии в SCM: в состояниях ожидания `checkpoint` растёт каждые 2 с (отдельный
/// поток), пока процесс не выйдет.
struct Reporter {
    handle: ServiceStatusHandle,
    state: Mutex<(ServiceState, u32)>,
}

impl Reporter {
    fn send(&self, state: ServiceState, checkpoint: u32, exit: ServiceExitCode) {
        let pending = matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        );
        let _ = self.handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: exit,
            checkpoint: if pending { checkpoint } else { 0 },
            wait_hint: if pending { WAIT_HINT } else { Duration::ZERO },
            process_id: None,
        });
    }

    fn set(&self, state: ServiceState, exit: ServiceExitCode) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // из Stopped назад не возвращаемся; StopPending не сменяется на Running
        if g.0 == ServiceState::Stopped
            || (g.0 == ServiceState::StopPending && state == ServiceState::Running)
        {
            return;
        }
        *g = (state, 1);
        self.send(state, 1, exit);
    }

    fn pump(self: &Arc<Self>) {
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(CHECKPOINT_EVERY);
                let mut g = me.state.lock().unwrap_or_else(|e| e.into_inner());
                match g.0 {
                    ServiceState::Stopped => return,
                    ServiceState::StartPending | ServiceState::StopPending => {
                        g.1 = g.1.wrapping_add(1);
                        me.send(g.0, g.1, ServiceExitCode::NO_ERROR);
                    }
                    _ => {}
                }
            }
        });
    }
}

fn service_main(arguments: Vec<OsString>) {
    let name = arguments
        .first()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| super::DEFAULT_NAME.to_string());
    let args = RUN_ARGS.get().cloned().unwrap_or_default();
    let signal = ShutdownSignal::new();
    let handler_signal = signal.clone();
    let handle = match service_control_handler::register(&name, move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
            handler_signal.trigger(StopSource::Service);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) {
        Ok(h) => h,
        Err(e) => {
            write_start_error(None, &format!("регистрация обработчика SCM: {e:?}"));
            return;
        }
    };
    let reporter = Arc::new(Reporter {
        handle,
        state: Mutex::new((ServiceState::StartPending, 1)),
    });
    reporter.send(ServiceState::StartPending, 1, ServiceExitCode::NO_ERROR);
    reporter.pump();
    let code = match serve(&name, args, signal, &reporter) {
        Ok(()) => ServiceExitCode::NO_ERROR,
        Err(()) => ServiceExitCode::ServiceSpecific(1),
    };
    reporter.set(ServiceState::Stopped, code);
}

/// Конфиг, рабочий каталог (каталог exe — как `WorkingDirectory` юнита), сервер до общего
/// сигнала остановки. Ошибка — в журнал сервера или в [`START_ERROR_FILE`].
fn serve(
    name: &str,
    args: RunArgs,
    signal: ShutdownSignal,
    reporter: &Arc<Reporter>,
) -> Result<(), ()> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(d) = &exe_dir {
        let _ = std::env::set_current_dir(d);
    }
    let cfg = match super::load_config(args.config.as_deref(), &args.cli) {
        Ok(c) => c,
        Err(e) => {
            write_start_error(None, &e.message);
            return Err(());
        }
    };
    let data_dir = cfg.data_dir.clone();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            write_start_error(Some(&data_dir), &format!("runtime: {e}"));
            return Err(());
        }
    };
    let pw = std::env::var("ZAM_ADMIN_PASSWORD").ok();
    let ready = Arc::clone(reporter);
    let stopping = Arc::clone(reporter);
    let res = rt.block_on(async move {
        let s = signal.clone();
        tokio::spawn(async move {
            s.wait().await;
            stopping.set(ServiceState::StopPending, ServiceExitCode::NO_ERROR);
        });
        crate::run_with(
            cfg,
            pw,
            RunOptions {
                mode: RunMode::WindowsService {
                    name: name.to_string(),
                },
                signal,
                on_ready: Some(Box::new(move || {
                    ready.set(ServiceState::Running, ServiceExitCode::NO_ERROR)
                })),
            },
        )
        .await
    });
    // фоновые задачи после штатной остановки уже завершены; зависшие не держат выход
    rt.shutdown_timeout(Duration::from_secs(5));
    res.map_err(|e| {
        tracing::error!(error = %format!("{e:#}"), "служба остановлена с ошибкой");
        write_start_error(Some(&data_dir), &format!("{e:#}"));
    })
}

/// Ошибка, которую некуда больше написать (консоли нет): `data_dir/logs/` или рядом с exe.
fn write_start_error(data_dir: Option<&Path>, msg: &str) {
    use std::io::Write;
    let dir = data_dir
        .map(|d| d.join("logs"))
        .filter(|d| std::fs::create_dir_all(d).is_ok())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
        });
    let Some(dir) = dir else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(START_ERROR_FILE))
    {
        let _ = writeln!(f, "{} {msg}", jiff::Timestamp::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_describes_registration() {
        let p = InstallPlan {
            name: "zam-server".into(),
            exe: PathBuf::from(r"C:\ZeroAIMemory\zam-server.exe"),
            config: PathBuf::from(r"C:\ZeroAIMemory\config.toml"),
            timeout: super::super::stop_timeout(14_000),
            logs_dir: PathBuf::from(r"C:\ZeroAIMemory\data\logs"),
        };
        let d = p.describe();
        assert!(
            d.contains(r#"binPath: "C:\ZeroAIMemory\zam-server.exe" service run --config "C:\ZeroAIMemory\config.toml""#),
            "{d}"
        );
        assert!(d.contains("Preshutdown): 94 с"), "{d}");
        assert!(d.contains("wait_hint 10 с, checkpoint каждые 2 с"), "{d}");
        assert_eq!(
            p.launch_arguments(),
            vec![
                OsString::from("service"),
                "run".into(),
                "--config".into(),
                r"C:\ZeroAIMemory\config.toml".into()
            ]
        );
        assert_eq!(display_name("zam-server"), "ZeroAIMemory");
        assert_eq!(display_name("zam2"), "ZeroAIMemory (zam2)");
    }
}
