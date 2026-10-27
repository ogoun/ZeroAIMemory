//! CLI сервера: разбор аргументов и вызов `zam_server`.
use std::io::{IsTerminal, Read};
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use zam_server::config::{self, CliOverrides, ServerConfig};

#[derive(Parser)]
#[command(name = "zam-server", version, about = "Сервер ZeroAIMemory")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Запустить сервер
    Serve(Opts),
    /// Прочитать пароль (stdin) и напечатать argon2id-хэш
    HashPassword,
    /// Открыть все базы (с миграциями) и выйти
    Migrate(Opts),
    /// Пересобрать векторный индекс на остановленном сервере: из кэша эмбеддингов без
    /// инференса (по умолчанию) или полностью (--full)
    Reindex {
        #[command(flatten)]
        opts: Opts,
        /// Только этот пользователь (id); по умолчанию — все
        #[arg(long)]
        user: Option<String>,
        /// Только это пространство пользователя: rules | knowledge | GUID решения | id
        #[arg(long, requires = "user")]
        space: Option<String>,
        /// Перечанковать и поставить задания (инференс для заметок без кэша) вместо
        /// пересборки из кэша
        #[arg(long)]
        full: bool,
    },
    /// Управляемый Qdrant: загрузка бинарника, состояние, сброс хранилища
    Qdrant {
        #[command(subcommand)]
        cmd: QdrantCmd,
    },
    /// Модели эмбеддингов и реранкера: загрузка по манифесту (SHA-256), состояние
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// Библиотеки NVIDIA (CUDA 13, cuDNN 9) для GPU-комплекта
    Cuda {
        #[command(subcommand)]
        cmd: CudaCmd,
    },
    /// Режим службы: служба Windows (SCM) или юнит systemd
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Зарегистрировать службу (Windows) или записать юнит systemd (Linux) с таймаутом
    /// остановки по конфигу; повторный вызов обновляет параметры
    Install {
        /// Имя службы
        #[arg(long, default_value = zam_server::service::DEFAULT_NAME)]
        name: String,
        /// Конфиг (по умолчанию: <каталог exe>\config.toml на Windows,
        /// /etc/zeroaimemory/config.toml на Linux)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Пользователь службы (Linux; по умолчанию zam)
        #[arg(long)]
        user: Option<String>,
        /// Вывести юнит (Linux) или параметры службы (Windows), ничего не устанавливая
        #[arg(long)]
        print: bool,
    },
    /// Остановить и удалить службу
    Uninstall {
        #[arg(long, default_value = zam_server::service::DEFAULT_NAME)]
        name: String,
    },
    /// Запустить службу
    Start {
        #[arg(long, default_value = zam_server::service::DEFAULT_NAME)]
        name: String,
    },
    /// Остановить службу (дожидается штатного выхода сервера)
    Stop {
        #[arg(long, default_value = zam_server::service::DEFAULT_NAME)]
        name: String,
    },
    /// Состояние службы
    Status {
        #[arg(long, default_value = zam_server::service::DEFAULT_NAME)]
        name: String,
    },
    /// Точка входа менеджера служб Windows (вручную не вызывается)
    #[command(hide = true)]
    Run(Opts),
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// Скачать модели устройства в ml.models_dir (по умолчанию <data_dir>/models)
    Fetch {
        #[command(flatten)]
        opts: Opts,
        /// cpu | gpu; по умолчанию — по ml.device и комплекту (CUDA EP рядом с ORT — gpu)
        #[arg(long, value_enum)]
        device: Option<zam_server::models::FetchDevice>,
        /// Также реранкер устройства (ml.rerank_model_cpu | ml.rerank_model_gpu)
        #[arg(long)]
        reranker: bool,
        /// Модель по id вместо набора устройства (можно повторять)
        #[arg(long = "id", value_name = "ID")]
        ids: Vec<String>,
        /// Согласие с лицензией CC-BY-NC-4.0 (только некоммерческое использование) для
        /// реранкера jina-reranker-v2
        #[arg(long)]
        accept_nc_license: bool,
        /// Манифест вместо встроенного при сборке (формат deploy/models.toml)
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Показать модели манифеста и что установлено (ничего не скачивает)
    Status {
        #[command(flatten)]
        opts: Opts,
        /// Сверить SHA-256 файлов (долго для гигабайтных весов)
        #[arg(long)]
        verify: bool,
        /// Манифест вместо встроенного при сборке
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CudaCmd {
    /// Скачать библиотеки NVIDIA по манифесту (размер и SHA-256) в <каталог ORT>/cuda/
    Fetch {
        #[command(flatten)]
        opts: Opts,
        /// Манифест вместо встроенного при сборке (формат deploy/cuda.toml)
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Итог в JSON в stdout (`dir`, `files`, `downloaded`, `installed`), прогресс — в stderr
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum QdrantCmd {
    /// Скачать бинарник Qdrant по манифесту (SHA-256), если его нет
    Fetch {
        #[command(flatten)]
        opts: Opts,
        /// Манифест вместо встроенного при сборке (формат deploy/qdrant.toml)
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Показать состояние: режим, версия, порты, pid, хранилище (ничего не запускает)
    Status {
        #[command(flatten)]
        opts: Opts,
        /// Вывод в JSON (для скриптов)
        #[arg(long)]
        json: bool,
    },
    /// Стереть хранилище Qdrant (сервер остановлен); при старте — пересборка индекса
    Reset {
        #[command(flatten)]
        opts: Opts,
        /// Подтверждение
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::Args)]
struct Opts {
    /// Путь к файлу конфигурации (TOML)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Адрес прослушивания
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Каталог данных
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

/// Конфигурация команды, которой нужен каталог данных: на Windows слишком длинный путь —
/// ошибка сразу, а не после долгой загрузки моделей или открытия баз.
fn load(o: Opts) -> anyhow::Result<ServerConfig> {
    let cfg = load_config(o)?;
    zam_store::check_data_dir_len(&cfg.data_dir)?;
    Ok(cfg)
}

/// Конфигурация без проверки длины каталога данных: для команд, которые его не трогают
/// (`cuda fetch`, `qdrant fetch`, `models` при заданном `ml.models_dir`).
fn load_config(o: Opts) -> anyhow::Result<ServerConfig> {
    // `vars()` паникует на не-Unicode значении: такие переменные пропускаются
    let env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    let cli = CliOverrides {
        bind: o.bind,
        data_dir: o.data_dir,
    };
    Ok(config::load(o.config.as_deref(), &env, &cli)?)
}

/// Конфигурация команд `models`: модели лежат в `<data_dir>/models`, если `ml.models_dir`
/// не задан, — тогда длина каталога данных проверяется.
fn load_for_models(o: Opts) -> anyhow::Result<ServerConfig> {
    let cfg = load_config(o)?;
    if cfg.ml.models_dir.is_none() {
        zam_store::check_data_dir_len(&cfg.data_dir)?;
    }
    Ok(cfg)
}

fn read_password() -> anyhow::Result<String> {
    if std::io::stdin().is_terminal() {
        // Приглашение в stderr: в stdout только хэш.
        eprint!("Пароль: ");
        return Ok(rpassword::read_password()?);
    }
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s.lines().next().unwrap_or("").to_string())
}

fn main() -> anyhow::Result<()> {
    let cmd = Cli::parse().cmd;
    // служба — без runtime: диспетчер SCM занимает главный поток, systemctl — синхронно
    if let Cmd::Service { cmd } = cmd {
        if let Err(e) = service_cmd(cmd) {
            eprintln!("ошибка: {e}");
            std::process::exit(i32::from(e.code));
        }
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cmd))
}

fn service_cmd(cmd: ServiceCmd) -> Result<(), zam_server::service::ServiceError> {
    use zam_server::service::{self, Control};
    match cmd {
        ServiceCmd::Install {
            name,
            config,
            user,
            print,
        } => service::install(service::InstallArgs {
            name,
            config,
            user,
            print,
        }),
        ServiceCmd::Uninstall { name } => service::control(Control::Uninstall, &name),
        ServiceCmd::Start { name } => service::control(Control::Start, &name),
        ServiceCmd::Stop { name } => service::control(Control::Stop, &name),
        ServiceCmd::Status { name } => service::control(Control::Status, &name),
        ServiceCmd::Run(o) => service::run(
            o.config,
            CliOverrides {
                bind: o.bind,
                data_dir: o.data_dir,
            },
        ),
    }
}

async fn async_main(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Serve(o) => {
            let cfg = load(o)?;
            let pw = std::env::var("ZAM_ADMIN_PASSWORD").ok();
            zam_server::run(cfg, pw).await
        }
        Cmd::HashPassword => {
            let pw = read_password()?;
            anyhow::ensure!(!pw.is_empty(), "пустой пароль");
            println!("{}", config::hash_password(&pw)?);
            Ok(())
        }
        Cmd::Migrate(o) => {
            let n = zam_server::migrate(load(o)?).await?;
            eprintln!("миграции применены: system.db и баз пользователей: {n}");
            Ok(())
        }
        Cmd::Reindex {
            opts,
            user,
            space,
            full,
        } => {
            let args = zam_server::reindex::ReindexArgs { user, space, full };
            println!("{}", zam_server::reindex::run(load(opts)?, args).await?);
            Ok(())
        }
        Cmd::Qdrant { cmd } => qdrant_cmd(cmd).await,
        Cmd::Models { cmd } => models_cmd(cmd).await,
        Cmd::Cuda { cmd } => cuda_cmd(cmd).await,
        Cmd::Service { .. } => unreachable!("служба обрабатывается до runtime"),
    }
}

async fn models_cmd(cmd: ModelsCmd) -> anyhow::Result<()> {
    use zam_server::models;
    match cmd {
        ModelsCmd::Fetch {
            opts,
            device,
            reranker,
            ids,
            accept_nc_license,
            manifest,
        } => {
            let cfg = load_for_models(opts)?;
            let m = models::load_manifest(manifest.as_deref())?;
            let device = device.unwrap_or_else(|| models::default_device(&cfg));
            let dir = cfg.ml.models_dir(&cfg.data_dir);
            // ureq блокирующий: загрузка вне потоков runtime
            tokio::task::spawn_blocking(move || {
                let list = models::plan(&m, &cfg, device, reranker, &ids)?;
                models::fetch(&m, &list, &dir, accept_nc_license, &mut |l| println!("{l}"))?;
                if ids.is_empty() && device == models::FetchDevice::Gpu {
                    println!(
                        "без CUDA сервер работает на CPU с {}: zam-server models fetch --device cpu",
                        zam_server::ml::MODEL_CPU
                    );
                }
                println!("перезапустите zam-server, чтобы подключить модели");
                anyhow::Ok(())
            })
            .await?
        }
        ModelsCmd::Status {
            opts,
            verify,
            manifest,
        } => {
            let cfg = load_for_models(opts)?;
            let m = models::load_manifest(manifest.as_deref())?;
            let out = tokio::task::spawn_blocking(move || models::status(&m, &cfg, verify)).await?;
            println!("{out}");
            Ok(())
        }
    }
}

async fn cuda_cmd(cmd: CudaCmd) -> anyhow::Result<()> {
    match cmd {
        CudaCmd::Fetch {
            opts,
            manifest,
            json,
        } => {
            let cfg = load_config(opts)?;
            let m = zam_server::cuda::load_manifest(manifest.as_deref())?;
            let report = tokio::task::spawn_blocking(move || {
                // с --json прогресс идёт в stderr: в stdout — только итог
                zam_server::cuda::fetch(&cfg, &m, &mut |l| {
                    if json {
                        eprintln!("{l}")
                    } else {
                        println!("{l}")
                    }
                })
            })
            .await??;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("{}", report.message());
            }
            Ok(())
        }
    }
}

async fn qdrant_cmd(cmd: QdrantCmd) -> anyhow::Result<()> {
    use zam_server::qdrant;
    let exe_dir = qdrant::exe_dir()?;
    match cmd {
        QdrantCmd::Fetch { opts, manifest } => {
            let cfg = load_config(opts)?;
            let m = qdrant::load_manifest(manifest.as_deref())?;
            let binary = cfg.qdrant.binary_path(&exe_dir);
            // ureq блокирующий: загрузка вне потоков runtime.
            let report = tokio::task::spawn_blocking(move || qdrant::fetch(&m, &binary)).await??;
            println!("{report}");
            Ok(())
        }
        QdrantCmd::Status { opts, json } => {
            let cfg = load(opts)?;
            if json {
                println!("{}", qdrant::status_json(&cfg, &exe_dir).await?);
            } else {
                println!("{}", qdrant::status(&cfg, &exe_dir).await?);
            }
            Ok(())
        }
        QdrantCmd::Reset { opts, yes } => {
            println!("{}", qdrant::reset(&load(opts)?, &exe_dir, yes)?);
            Ok(())
        }
    }
}
