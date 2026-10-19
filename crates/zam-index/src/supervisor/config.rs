//! Конфигурация супервизора и генерация `config/zam.yaml` для Qdrant (исследование 05 §6.2).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Закреплённая версия Qdrant из `deploy/qdrant.toml` (build.rs). Ей должен отвечать
/// `GET /` запущенного процесса, она же пишется в метку хранилища и входит в путь
/// бинарника по умолчанию.
pub const QDRANT_VERSION: &str = env!("ZAM_QDRANT_PINNED_VERSION");

/// Имя исполняемого файла Qdrant на текущей ОС.
pub fn qdrant_exe_name() -> &'static str {
    if cfg!(windows) {
        "qdrant.exe"
    } else {
        "qdrant"
    }
}

/// Бинарник по умолчанию в комплекте: `<exe_dir>/qdrant/<ver>/qdrant[.exe]`
/// (раскладка `cargo xtask package`).
pub fn default_binary(exe_dir: &Path) -> PathBuf {
    exe_dir
        .join("qdrant")
        .join(QDRANT_VERSION)
        .join(qdrant_exe_name())
}

/// Режим работы с Qdrant.
#[derive(Clone, Debug)]
pub enum SupervisorConfig {
    /// Процесс запускает и контролирует супервизор.
    Managed(ManagedConfig),
    /// Уже работающий Qdrant: без процесса, доступность проверяет `VectorIndex::health`.
    External(ExternalConfig),
}

impl SupervisorConfig {
    /// Внешний Qdrant по gRPC-адресу.
    pub fn external(grpc_url: impl Into<String>, api_key: Option<String>) -> Self {
        SupervisorConfig::External(ExternalConfig {
            grpc_url: grpc_url.into(),
            api_key,
        })
    }
}

/// Внешний Qdrant.
#[derive(Clone)]
pub struct ExternalConfig {
    /// gRPC-адрес, например `http://127.0.0.1:6334`.
    pub grpc_url: String,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ExternalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalConfig")
            .field("grpc_url", &self.grpc_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Управляемый процесс Qdrant.
#[derive(Clone, Debug)]
pub struct ManagedConfig {
    pub binary: PathBuf,
    /// Версия, которую обязан сообщить `GET /`; иначе `Broken`.
    pub expected_version: String,
    pub http_port: u16,
    pub grpc_port: u16,
    /// `storage.performance.max_search_threads` (0 — авто).
    pub max_search_threads: u32,
    /// `storage.performance.optimizer_cpu_budget` (0 — авто).
    pub optimizer_cpu_budget: u32,
    pub restart: RestartPolicy,
    /// Предел ожидания готовности одного запуска; большое хранилище на HDD грузится долго.
    pub ready_timeout: Duration,
    /// Linux: ожидание после SIGTERM до SIGKILL.
    pub stop_timeout: Duration,
}

impl ManagedConfig {
    /// Умолчания §4.3: порты 6333/6334, 2 потока поиска, бюджет оптимизатора 2.
    pub fn new(binary: PathBuf) -> Self {
        ManagedConfig {
            binary,
            expected_version: QDRANT_VERSION.to_string(),
            http_port: 6333,
            grpc_port: 6334,
            max_search_threads: 2,
            optimizer_cpu_budget: 2,
            restart: RestartPolicy::default(),
            ready_timeout: Duration::from_secs(180),
            stop_timeout: Duration::from_secs(10),
        }
    }
}

/// Перезапуск при падении: экспоненциальная задержка с jitter и circuit breaker.
#[derive(Clone, Debug)]
pub struct RestartPolicy {
    /// Задержка перед первым перезапуском (1 с).
    pub initial: Duration,
    /// Потолок задержки (60 с).
    pub max: Duration,
    /// Окно подсчёта падений (10 мин).
    pub window: Duration,
    /// Число падений в окне, после которого — `Broken` без перезапусков (5).
    pub max_crashes: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            window: Duration::from_secs(600),
            max_crashes: 5,
        }
    }
}

impl RestartPolicy {
    /// Задержка перед попыткой `attempt` (с 1): `initial · 2^(attempt−1)`, множитель
    /// jitter из [0.8, 1.2], не больше `max`.
    pub fn delay(&self, attempt: u32, jitter: f64) -> Duration {
        let exp = attempt.saturating_sub(1).min(20);
        let base = self.initial.saturating_mul(1u32 << exp).min(self.max);
        base.mul_f64(jitter.clamp(0.8, 1.2)).min(self.max)
    }
}

/// Текст `config/zam.yaml` для рабочего каталога `qdir` (`<data_dir>/qdrant/`).
/// Ключа здесь нет: он передаётся только через `QDRANT__SERVICE__API_KEY`.
pub(crate) fn render_yaml(c: &ManagedConfig, qdir: &Path) -> std::io::Result<String> {
    Ok(format!(
        "# Сгенерирован zam-server при каждом запуске Qdrant; правки будут перезаписаны.\n\
         log_level: INFO\n\
         logger:\n  format: json\n\
         telemetry_disabled: true\n\
         storage:\n  storage_path: {}\n  snapshots_path: {}\n  temp_path: {}\n  on_disk_payload: true\n  \
         performance:\n    max_search_threads: {}\n    optimizer_cpu_budget: {}\n\
         service:\n  host: 127.0.0.1\n  http_port: {}\n  grpc_port: {}\n  enable_cors: false\n  enable_static_content: false\n\
         cluster:\n  enabled: false\n",
        storage_path(qdir, "storage")?,
        storage_path(qdir, "snapshots")?,
        storage_path(qdir, "temp")?,
        c.max_search_threads,
        c.optimizer_cpu_budget,
        c.http_port,
        c.grpc_port
    ))
}

/// Путь подкаталога `name` рабочего каталога Qdrant для `zam.yaml`.
///
/// Linux — относительный (`./storage`). Windows — абсолютный с префиксом `\\?\`: файлы
/// payload-индексов лежат глубоко (`storage/collections/<коллекция>/0/segments/<uuid>/
/// payload_index/<id>-<поле>/…`), и при каталоге данных длиннее примерно 80 символов
/// относительный путь превышает MAX_PATH (260): Qdrant 1.19 отвечает на создание
/// payload-индекса ошибкой «failed to open mutable numeric index on gridstore» (os error 3),
/// а при следующем запуске падает на загрузке коллекции. Путь с `\\?\` ограничения не имеет.
fn storage_path(qdir: &Path, name: &str) -> std::io::Result<String> {
    if cfg!(windows) {
        let abs = std::path::absolute(qdir.join(name))?;
        Ok(yaml_quote(&verbatim(&abs.to_string_lossy())))
    } else {
        Ok(format!("./{name}"))
    }
}

/// Абсолютный путь Windows в форме `\\?\…` (UNC `\\сервер\ресурс` — `\\?\UNC\сервер\ресурс`).
fn verbatim(p: &str) -> String {
    if p.starts_with(r"\\?\") {
        p.to_string()
    } else if let Some(rest) = p.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{p}")
    }
}

/// Строка YAML в одинарных кавычках: обратные косые черты не экранируются, `'` удваивается.
fn yaml_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_default_binary() {
        assert_eq!(QDRANT_VERSION.split('.').count(), 3);
        let p = default_binary(Path::new("inst"));
        assert_eq!(
            p,
            Path::new("inst")
                .join("qdrant")
                .join(QDRANT_VERSION)
                .join(qdrant_exe_name())
        );
    }

    #[test]
    fn backoff_grows_to_cap_with_jitter() {
        let p = RestartPolicy::default();
        assert_eq!(p.delay(1, 1.0), Duration::from_secs(1));
        assert_eq!(p.delay(2, 1.0), Duration::from_secs(2));
        assert_eq!(p.delay(4, 1.0), Duration::from_secs(8));
        assert_eq!(p.delay(7, 1.0), Duration::from_secs(60));
        assert_eq!(p.delay(100, 1.2), Duration::from_secs(60));
        assert_eq!(p.delay(1, 0.8), Duration::from_millis(800));
        assert_eq!(p.delay(1, 5.0), Duration::from_millis(1200));
    }

    #[test]
    fn yaml_has_required_keys() {
        let mut c = ManagedConfig::new(PathBuf::from("q"));
        c.http_port = 7333;
        c.grpc_port = 7334;
        let qdir = Path::new("data").join("qdrant");
        let y = render_yaml(&c, &qdir).unwrap();
        for needle in [
            "log_level: INFO",
            "  format: json",
            "telemetry_disabled: true",
            "  on_disk_payload: true",
            "    max_search_threads: 2",
            "    optimizer_cpu_budget: 2",
            "  host: 127.0.0.1",
            "  http_port: 7333",
            "  grpc_port: 7334",
            "  enable_cors: false",
            "  enable_static_content: false",
            "cluster:\n  enabled: false",
        ] {
            assert!(y.contains(needle), "нет `{needle}` в\n{y}");
        }
        if cfg!(windows) {
            let abs = std::path::absolute(qdir.join("storage")).unwrap();
            let want = format!("  storage_path: '\\\\?\\{}'", abs.display());
            assert!(y.contains(&want), "нет `{want}` в\n{y}");
            assert!(y.contains("snapshots'\n") && y.contains("temp'\n"), "{y}");
        } else {
            for needle in [
                "  storage_path: ./storage",
                "  snapshots_path: ./snapshots",
                "  temp_path: ./temp",
            ] {
                assert!(y.contains(needle), "нет `{needle}` в\n{y}");
            }
        }
    }

    #[test]
    fn verbatim_paths_and_yaml_quoting() {
        assert_eq!(verbatim(r"C:\d\qdrant\storage"), r"\\?\C:\d\qdrant\storage");
        assert_eq!(verbatim(r"\\?\C:\d"), r"\\?\C:\d");
        assert_eq!(verbatim(r"\\srv\share\d"), r"\\?\UNC\srv\share\d");
        assert_eq!(yaml_quote(r"\\?\C:\O'Brien"), r"'\\?\C:\O''Brien'");
    }

    #[test]
    fn external_debug_hides_key() {
        let c = SupervisorConfig::external("http://h:1", Some("secret-key".into()));
        assert!(!format!("{c:?}").contains("secret-key"));
    }
}
