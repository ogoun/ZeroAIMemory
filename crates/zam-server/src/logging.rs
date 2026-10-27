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

/// Цели, которые нельзя поднять выше `warn` (§13.1):
/// - `rmcp` на debug/trace пишет JSON-RPC целиком (тексты заметок, значения Vault, промпты);
/// - `memory_serve` на info пишет строку на каждый ассет WebUI при старте (~160 строк в
///   journald, стенд Debian 13) — шум без пользы для эксплуатации.
const CAPPED: [&str; 2] = ["rmcp", "memory_serve"];

/// Цель директивы (`rmcp::service=debug` → `rmcp::service`), если она относится к одной из
/// [`CAPPED`] или её подцели, — эта цель.
fn capped_target(directive: &str) -> Option<&'static str> {
    let target = directive.split(['=', '[']).next().unwrap_or("").trim();
    CAPPED.into_iter().find(|c| {
        target == *c
            || target
                .strip_prefix(c)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

/// Фильтр журнала: `log.level` без директив целей [`CAPPED`] и сверху `<цель>=<уровень>` для
/// каждой — `warn`, а при общем `error`/`off` — он же (принудительная директива не повышает
/// подробность).
pub(crate) fn filter(level: &str) -> anyhow::Result<EnvFilter> {
    let rest: Vec<&str> = level
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty() && capped_target(d).is_none())
        .collect();
    let global = rest
        .iter()
        .rev()
        .find(|d| !d.contains('=') && !d.contains('['))
        .map(|d| d.to_ascii_lowercase());
    let cap = match global.as_deref() {
        Some("off") => "off",
        Some("error") => "error",
        _ => "warn",
    };
    let mut f = EnvFilter::try_new(rest.join(","))?;
    for t in CAPPED {
        f = f.add_directive(format!("{t}={cap}").parse()?);
    }
    Ok(f)
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
        for level in [
            "trace",
            "debug",
            "info,rmcp=trace",
            "rmcp=debug",
            "info,rmcp::service=debug,rmcp::transport=trace",
            "debug,rmcp::service[req]=trace",
        ] {
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

    /// `memory_serve` (ассеты WebUI) — тоже не выше `warn`; свои цели не задеты. Директива
    /// пользователя снимается только для самой цели и её подцелей (`memory_server=…` — чужая);
    /// сам `EnvFilter` сравнивает цели по префиксу строки, поэтому принудительный `warn`
    /// задел бы и цель `memory_server…` — такой зависимости нет.
    #[test]
    fn memory_serve_capped_at_warn() {
        assert_eq!(
            super::capped_target("memory_serve=info"),
            Some("memory_serve")
        );
        assert_eq!(
            super::capped_target("memory_serve::x=debug"),
            Some("memory_serve")
        );
        assert_eq!(
            super::capped_target("rmcp::service[req]=trace"),
            Some("rmcp")
        );
        assert_eq!(super::capped_target("memory_server=info"), None);
        assert_eq!(super::capped_target("rmcpx=debug"), None);
        assert_eq!(super::capped_target("info"), None);
        for level in [
            "info",
            "debug",
            "info,memory_serve=trace",
            "memory_serve::assets=info",
        ] {
            let buf = Buf::default();
            let w = buf.clone();
            let sub = tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_writer(move || w.clone()))
                .with(super::filter(level).unwrap());
            tracing::subscriber::with_default(sub, || {
                tracing::info!(target: "memory_serve", "ассет-info");
                tracing::debug!(target: "memory_serve::assets", "ассет-debug");
                tracing::warn!(target: "memory_serve", "ассет-warn-виден");
                tracing::info!(target: "zam_server::web", "свой-info");
            });
            let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            assert!(
                !out.contains("ассет-info") && !out.contains("ассет-debug"),
                "{level}: {out}"
            );
            assert!(out.contains("ассет-warn-виден"), "{level}: {out}");
            assert_eq!(
                out.contains("свой-info"),
                !level.starts_with("memory_serve::"),
                "{level}: {out}"
            );
        }
    }

    /// При общем `error`/`off` принудительная директива не добавляет warn-событий `rmcp`.
    #[test]
    fn rmcp_cap_does_not_raise_quiet_levels() {
        for level in ["error", "off", "off,rmcp=debug"] {
            let buf = Buf::default();
            let w = buf.clone();
            let sub = tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_writer(move || w.clone()))
                .with(super::filter(level).unwrap());
            tracing::subscriber::with_default(sub, || {
                tracing::warn!(target: "rmcp::service", "rmcp-warn");
                tracing::warn!(target: "memory_serve", "rmcp-warn-ассет");
            });
            let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            assert!(!out.contains("rmcp-warn"), "{level}: {out}");
        }
    }
}
