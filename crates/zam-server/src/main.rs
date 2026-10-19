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
    /// Управляемый Qdrant: загрузка бинарника, состояние, сброс хранилища
    Qdrant {
        #[command(subcommand)]
        cmd: QdrantCmd,
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
    Status(Opts),
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

fn load(o: Opts) -> anyhow::Result<ServerConfig> {
    let env: Vec<(String, String)> = std::env::vars().collect();
    let cli = CliOverrides {
        bind: o.bind,
        data_dir: o.data_dir,
    };
    Ok(config::load(o.config.as_deref(), &env, &cli)?)
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
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
        Cmd::Qdrant { cmd } => qdrant_cmd(cmd).await,
    }
}

async fn qdrant_cmd(cmd: QdrantCmd) -> anyhow::Result<()> {
    use zam_server::qdrant;
    let exe_dir = qdrant::exe_dir()?;
    match cmd {
        QdrantCmd::Fetch { opts, manifest } => {
            let cfg = load(opts)?;
            let m = qdrant::load_manifest(manifest.as_deref())?;
            let binary = cfg.qdrant.binary_path(&exe_dir);
            // ureq блокирующий: загрузка вне потоков runtime.
            let report = tokio::task::spawn_blocking(move || qdrant::fetch(&m, &binary)).await??;
            println!("{report}");
            Ok(())
        }
        QdrantCmd::Status(opts) => {
            println!("{}", qdrant::status(&load(opts)?, &exe_dir).await?);
            Ok(())
        }
        QdrantCmd::Reset { opts, yes } => {
            println!("{}", qdrant::reset(&load(opts)?, &exe_dir, yes)?);
            Ok(())
        }
    }
}
