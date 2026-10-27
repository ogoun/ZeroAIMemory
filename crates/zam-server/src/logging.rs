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

/// Директива поверх фильтра из конфигурации: `rmcp` на debug/trace пишет JSON-RPC целиком
/// (тексты заметок, значения Vault, промпты), поэтому выше `warn` её не поднять (§13.1).
const FORCED: &str = "rmcp=warn";

/// Фильтр журнала: `log.level` и принудительные директивы [`FORCED`].
pub(crate) fn filter(level: &str) -> anyhow::Result<EnvFilter> {
    Ok(EnvFilter::try_new(level)?.add_directive(FORCED.parse()?))
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
    let filter = filter(&cfg.level)?;
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt;

    /// Буфер вывода subscriber-а в тесте.
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `rmcp` выше `warn` не поднимается ни общим уровнем, ни своей директивой.
    #[test]
    fn rmcp_capped_at_warn() {
        for level in ["trace", "debug", "info,rmcp=trace", "rmcp=debug"] {
            let buf = Buf::default();
            let w = buf.clone();
            let sub = tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_writer(move || w.clone()))
                .with(super::filter(level).unwrap());
            tracing::subscriber::with_default(sub, || {
                tracing::debug!(target: "rmcp::service", "секрет-rmcp-debug");
                tracing::trace!(target: "rmcp::transport", "секрет-rmcp-trace");
                tracing::warn!(target: "rmcp::service", "rmcp-warn-виден");
                tracing::debug!(target: "zam_server::x", "свой-debug");
            });
            let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            assert!(!out.contains("секрет-rmcp"), "{level}: {out}");
            assert!(out.contains("rmcp-warn-виден"), "{level}: {out}");
            assert_eq!(
                out.contains("свой-debug"),
                level.starts_with("trace") || level.starts_with("debug"),
                "{level}: {out}"
            );
        }
    }
}
