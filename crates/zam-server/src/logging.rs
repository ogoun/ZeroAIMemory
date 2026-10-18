//! Журнал: stderr + файл `zam-server.log` с суточной ротацией.
use std::io::IsTerminal;
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

use crate::config::{LogFormat, LogSection};

type BoxLayer = Box<dyn Layer<Registry> + Send + Sync>;

fn fmt_layer<W>(format: LogFormat, w: W, ansi: bool) -> BoxLayer
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    let l = tracing_subscriber::fmt::layer()
        .with_writer(w)
        .with_ansi(ansi);
    match format {
        LogFormat::Json => l.json().boxed(),
        LogFormat::Text => l.boxed(),
    }
}

/// Ставит глобальный subscriber. Guard нужно держать до конца процесса.
/// Повторный вызов (уже есть subscriber) не ошибка — остаётся прежний.
pub fn init(cfg: &LogSection, logs_dir: &Path) -> anyhow::Result<WorkerGuard> {
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("zam-server.log")
        .max_log_files(cfg.max_files.max(1))
        .build(logs_dir)?;
    let (file, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_new(&cfg.level)?;
    let ansi = std::io::stderr().is_terminal();
    let layers = vec![
        fmt_layer(cfg.format, std::io::stderr, ansi),
        fmt_layer(cfg.format, file, false),
    ];
    let _ = tracing_subscriber::registry()
        .with(layers)
        .with(filter)
        .try_init();
    Ok(guard)
}
