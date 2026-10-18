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
    }
}
