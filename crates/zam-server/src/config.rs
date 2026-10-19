//! Конфигурация сервера. Слои: умолчания, TOML-файл, `ZAM_*`, CLI.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, SaltString, rand_core::OsRng};
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml::{Table, Value};

/// Переменные `ZAM_*`, которые не являются слоем конфигурации:
/// секреты и адрес клиента, а также сборочные (build.rs, xtask), попадающие в окружение CI.
const RESERVED_ENV: [&str; 8] = [
    "ZAM_ADMIN_PASSWORD",
    "ZAM_URL",
    "ZAM_TOKEN",
    "ZAM_MASTER_KEY",
    "ZAM_GIT_SHA",
    "ZAM_WEB_REBUILD",
    "ZAM_WEB_INDEX",
    "ZAM_VCRUNTIME_DLL",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub server: ServerSection,
    pub admin: AdminSection,
    pub data_dir: PathBuf,
    pub log: LogSection,
    pub qdrant: QdrantSection,
    pub limits: LimitsSection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub bind: SocketAddr,
    pub public_url: Option<String>,
    /// "" или "/memory": с ведущим "/", без завершающего.
    pub base_path: String,
    #[serde(serialize_with = "ser_nets", deserialize_with = "de_nets")]
    pub trusted_proxies: Vec<IpNet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSection {
    pub login: String,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    pub level: String,
    pub format: LogFormat,
    pub max_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QdrantMode {
    Managed,
    External,
    Edge,
}

/// Секция `[qdrant]` (§4.3). Супервизор строится из неё через [`QdrantSection::supervisor_config`].
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QdrantSection {
    pub mode: QdrantMode,
    /// gRPC-адрес внешнего Qdrant (`mode = "external"`), например `http://127.0.0.1:6334`.
    pub url: Option<String>,
    pub http_port: u16,
    pub grpc_port: u16,
    /// Бинарник управляемого Qdrant; по умолчанию `<exe_dir>/qdrant/<ver>/qdrant[.exe]`.
    pub binary: Option<PathBuf>,
    /// `storage.performance.max_search_threads` (0 — авто): не отбирать CPU у инференса.
    pub max_search_threads: u32,
    /// `storage.performance.optimizer_cpu_budget` (0 — авто).
    pub optimizer_cpu_budget: u32,
    /// API-ключ внешнего Qdrant. Для `managed` не задаётся: ключ случайный при каждом старте.
    pub api_key: Option<String>,
}

// Вручную: ключ внешнего Qdrant не должен попадать в отладочный вывод.
impl std::fmt::Debug for QdrantSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QdrantSection")
            .field("mode", &self.mode)
            .field("url", &self.url)
            .field("http_port", &self.http_port)
            .field("grpc_port", &self.grpc_port)
            .field("binary", &self.binary)
            .field("max_search_threads", &self.max_search_threads)
            .field("optimizer_cpu_budget", &self.optimizer_cpu_budget)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

impl QdrantSection {
    /// Бинарник управляемого Qdrant: из конфигурации или из комплекта рядом с `exe_dir`.
    pub fn binary_path(&self, exe_dir: &Path) -> PathBuf {
        self.binary
            .clone()
            .unwrap_or_else(|| zam_index::supervisor::default_binary(exe_dir))
    }

    /// Конфигурация супервизора; `None` — режим `edge` (экспериментальный, без процесса).
    pub fn supervisor_config(
        &self,
        exe_dir: &Path,
    ) -> Result<Option<zam_index::supervisor::SupervisorConfig>, ConfigError> {
        use zam_index::supervisor::{ManagedConfig, SupervisorConfig};
        let invalid = |key: &str, message: &str| ConfigError::Invalid {
            key: key.into(),
            source_name: "итог".into(),
            message: message.into(),
        };
        match self.mode {
            QdrantMode::Managed => {
                if self.api_key.is_some() {
                    return Err(invalid(
                        "qdrant.api_key",
                        "задаётся только для mode = \"external\": управляемый Qdrant получает случайный ключ при каждом старте",
                    ));
                }
                if self.http_port == self.grpc_port {
                    return Err(invalid("qdrant.grpc_port", "совпадает с http_port"));
                }
                let mut m = ManagedConfig::new(self.binary_path(exe_dir));
                m.http_port = self.http_port;
                m.grpc_port = self.grpc_port;
                m.max_search_threads = self.max_search_threads;
                m.optimizer_cpu_budget = self.optimizer_cpu_budget;
                Ok(Some(SupervisorConfig::Managed(m)))
            }
            QdrantMode::External => {
                let url = self
                    .url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .ok_or_else(|| {
                        invalid(
                            "qdrant.url",
                            "обязателен для mode = \"external\" (gRPC-адрес)",
                        )
                    })?;
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(invalid("qdrant.url", "ожидается http:// или https://"));
                }
                Ok(Some(SupervisorConfig::external(
                    url,
                    self.api_key.clone().filter(|k| !k.is_empty()),
                )))
            }
            QdrantMode::Edge => Ok(None),
        }
    }
}

/// Пределы размеров (§16.2): заметка и вложение, байт.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    pub note_max_bytes: usize,
    pub blob_max_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server: ServerSection {
                bind: SocketAddr::from(([0, 0, 0, 0], 7700)),
                public_url: None,
                base_path: String::new(),
                trusted_proxies: Vec::new(),
            },
            admin: AdminSection {
                login: "admin".into(),
                password_hash: None,
            },
            data_dir: PathBuf::from("./data"),
            log: LogSection {
                level: "info".into(),
                format: LogFormat::Json,
                max_files: 14,
            },
            qdrant: QdrantSection {
                mode: QdrantMode::Managed,
                url: None,
                http_port: 6333,
                grpc_port: 6334,
                binary: None,
                max_search_threads: 2,
                optimizer_cpu_budget: 2,
                api_key: None,
            },
            limits: LimitsSection {
                note_max_bytes: 5_242_880,
                blob_max_bytes: 209_715_200,
            },
        }
    }
}

fn ser_nets<S: Serializer>(nets: &[IpNet], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(nets.iter().map(|n| n.to_string()))
}

/// CIDR или одиночный IP (как /32 или /128).
fn de_nets<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<IpNet>, D::Error> {
    let raw = Vec::<String>::deserialize(d)?;
    raw.iter()
        .map(|s| {
            s.parse::<IpNet>()
                .or_else(|_| s.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| serde::de::Error::custom(format!("неверный CIDR или IP: `{s}`")))
        })
        .collect()
}

/// Переопределения из командной строки (верхний слой).
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub bind: Option<SocketAddr>,
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminSecret {
    Hash(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать файл конфигурации {path}: {message}")]
    Read { path: String, message: String },
    #[error("ошибка синтаксиса в файле конфигурации {path}: {message}")]
    Syntax { path: String, message: String },
    #[error("неверное значение ключа `{key}` ({source_name}): {message}")]
    Invalid {
        key: String,
        source_name: String,
        message: String,
    },
    #[error("задайте [admin].password_hash (zam-server hash-password) или ZAM_ADMIN_PASSWORD")]
    AdminPasswordMissing,
    #[error("[admin].password_hash не является корректным PHC-хэшем: {0}")]
    AdminHashInvalid(String),
    #[error("не удалось захэшировать пароль администратора: {0}")]
    Hash(String),
}

/// Собирает конфигурацию: умолчания, файл, окружение, CLI.
pub fn load(
    file: Option<&Path>,
    env: &[(String, String)],
    cli: &CliOverrides,
) -> Result<ServerConfig, ConfigError> {
    let mut merged = Value::try_from(ServerConfig::default()).expect("умолчания сериализуются");
    // (путь ключа, источник, значение) в порядке применения.
    let mut leaves: Vec<(Vec<String>, String, Value, Option<String>)> = Vec::new();

    if let Some(path) = file {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: shown.clone(),
            message: e.to_string(),
        })?;
        let table: Table = toml::from_str(&text).map_err(|e| {
            // Без цитаты исходника: в строке может быть секрет.
            let place = e.span().map_or(String::new(), |sp| {
                let before = &text.as_bytes()[..sp.start.min(text.len())];
                let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
                let col = before.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
                format!(" (строка {line}, столбец {col})")
            });
            ConfigError::Syntax {
                path: shown.clone(),
                message: format!("{}{place}", e.message()),
            }
        })?;
        flatten(&mut Vec::new(), &Value::Table(table), &mut |p, v| {
            leaves.push((p, format!("file {shown}"), v, None));
        });
    }

    for (name, raw) in env {
        let upper = name.to_ascii_uppercase();
        let Some(rest) = upper.strip_prefix("ZAM_") else {
            continue;
        };
        if RESERVED_ENV.contains(&upper.as_str()) || rest.is_empty() {
            continue;
        }
        let path: Vec<String> = rest
            .to_ascii_lowercase()
            .split("__")
            .map(String::from)
            .collect();
        leaves.push((
            path,
            format!("env {name}"),
            parse_scalar(raw),
            Some(raw.clone()),
        ));
    }

    if let Some(bind) = cli.bind {
        leaves.push((
            vec!["server".into(), "bind".into()],
            "cli".into(),
            Value::String(bind.to_string()),
            None,
        ));
    }
    if let Some(dir) = &cli.data_dir {
        leaves.push((
            vec!["data_dir".into()],
            "cli".into(),
            Value::String(dir.to_string_lossy().into_owned()),
            None,
        ));
    }

    let defaults = merged.clone();
    for (path, source, mut value, raw) in leaves {
        // Пустая таблица известной секции ничего не меняет.
        if path.is_empty()
            || (is_empty_table(&value) && get_path(&defaults, &path).is_some_and(Value::is_table))
        {
            continue;
        }
        // Проверка значения в изоляции, чтобы указать ключ и источник.
        let probe_with = |v: &Value| {
            let mut probe = defaults.clone();
            set_path(&mut probe, &path, v.clone());
            probe.try_into::<ServerConfig>().map(|_| ())
        };
        if let Err(e) = probe_with(&value) {
            // Значение env может быть строкой, похожей на число/булево/дату.
            let as_string = raw.map(Value::String).filter(|v| probe_with(v).is_ok());
            match as_string {
                Some(v) => value = v,
                None => {
                    return Err(ConfigError::Invalid {
                        key: path.join("."),
                        source_name: source,
                        message: e.message().to_string(),
                    });
                }
            }
        }
        set_path(&mut merged, &path, value);
    }

    let mut cfg: ServerConfig =
        merged
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Invalid {
                key: "?".into(),
                source_name: "итог".into(),
                message: e.message().to_string(),
            })?;
    cfg.server.base_path = normalize_base_path(&cfg.server.base_path);
    Ok(cfg)
}

/// Значение из env: TOML-скаляр или массив, иначе строка.
fn parse_scalar(raw: &str) -> Value {
    toml::from_str::<Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

fn normalize_base_path(raw: &str) -> String {
    let t = raw.trim().trim_matches('/');
    if t.is_empty() {
        String::new()
    } else {
        format!("/{t}")
    }
}

/// Обходит листья (всё, кроме таблиц).
fn flatten(prefix: &mut Vec<String>, v: &Value, f: &mut impl FnMut(Vec<String>, Value)) {
    match v {
        Value::Table(t) if !t.is_empty() => {
            for (k, child) in t {
                prefix.push(k.clone());
                flatten(prefix, child, f);
                prefix.pop();
            }
        }
        leaf => f(prefix.clone(), leaf.clone()),
    }
}

fn is_empty_table(v: &Value) -> bool {
    v.as_table().is_some_and(Table::is_empty)
}

fn get_path<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(root, |v, k| v.as_table()?.get(k))
}

fn set_path(root: &mut Value, path: &[String], value: Value) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    if !root.is_table() {
        *root = Value::Table(Table::new());
    }
    let table = root.as_table_mut().expect("таблица");
    if tail.is_empty() {
        table.insert(head.clone(), value);
    } else {
        let child = table
            .entry(head.clone())
            .or_insert_with(|| Value::Table(Table::new()));
        set_path(child, tail, value);
    }
}

impl ServerConfig {
    /// Хэш пароля администратора: из конфига, иначе из `ZAM_ADMIN_PASSWORD` (argon2id в памяти).
    pub fn admin_secret(&self, env_password: Option<&str>) -> Result<AdminSecret, ConfigError> {
        if let Some(h) = self
            .admin
            .password_hash
            .as_deref()
            .filter(|h| !h.is_empty())
        {
            PasswordHash::new(h).map_err(|e| ConfigError::AdminHashInvalid(e.to_string()))?;
            return Ok(AdminSecret::Hash(h.to_string()));
        }
        match env_password.filter(|p| !p.is_empty()) {
            Some(p) => hash_password(p).map(AdminSecret::Hash),
            None => Err(ConfigError::AdminPasswordMissing),
        }
    }
}

/// argon2id с параметрами по умолчанию и случайной солью.
pub fn hash_password(password: &str) -> Result<String, ConfigError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ConfigError::Hash(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::write_in_target;

    fn env(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn defaults() {
        let c = load(None, &[], &Default::default()).unwrap();
        assert_eq!(c.server.bind.to_string(), "0.0.0.0:7700");
        assert_eq!(c.server.base_path, "");
        assert_eq!(c.admin.login, "admin");
        assert_eq!(c.data_dir, std::path::PathBuf::from("./data"));
        assert_eq!(c.log.level, "info");
        assert_eq!(c.log.format, LogFormat::Json);
        assert_eq!(c.log.max_files, 14);
        assert_eq!(c.qdrant.mode, QdrantMode::Managed);
        assert_eq!((c.qdrant.http_port, c.qdrant.grpc_port), (6333, 6334));
        assert_eq!(
            (c.qdrant.max_search_threads, c.qdrant.optimizer_cpu_budget),
            (2, 2)
        );
        assert_eq!((&c.qdrant.binary, &c.qdrant.api_key), (&None, &None));
        assert_eq!(c.limits.note_max_bytes, 5_242_880);
        assert_eq!(c.limits.blob_max_bytes, 209_715_200);
    }

    #[test]
    fn limits_from_file_and_env() {
        let f = write_in_target(
            "limits.toml",
            "[limits]
note_max_bytes = 1000
",
        );
        let e = vec![env("ZAM_LIMITS__BLOB_MAX_BYTES", "2000")];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        assert_eq!(
            (c.limits.note_max_bytes, c.limits.blob_max_bytes),
            (1000, 2000)
        );
        let bad = vec![env("ZAM_LIMITS__NOTE_MAX_BYTES", "-1")];
        let e = load(None, &bad, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("limits.note_max_bytes"), "{e}");
    }

    #[test]
    fn file_then_env_then_cli() {
        let f = write_in_target(
            "cfg.toml",
            "[server]\nbind = \"127.0.0.1:8000\"\n[log]\nlevel=\"debug\"",
        );
        let env = vec![env("ZAM_SERVER__BIND", "127.0.0.1:9000")];
        let c = load(Some(&f), &env, &Default::default()).unwrap();
        assert_eq!(c.server.bind.port(), 9000);
        assert_eq!(c.log.level, "debug");
        let cli = CliOverrides {
            bind: Some("127.0.0.1:9100".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(load(Some(&f), &env, &cli).unwrap().server.bind.port(), 9100);
    }

    #[test]
    fn env_top_level_and_case_insensitive() {
        let env = vec![
            env("ZAM_DATA_DIR", "/srv/zam"),
            env("zam_log__max_files", "3"),
            env("PATH", "/usr/bin"),
        ];
        let c = load(None, &env, &Default::default()).unwrap();
        assert_eq!(c.data_dir, std::path::PathBuf::from("/srv/zam"));
        assert_eq!(c.log.max_files, 3);
    }

    #[test]
    fn cli_data_dir() {
        let cli = CliOverrides {
            data_dir: Some("/x/y".into()),
            ..Default::default()
        };
        let c = load(None, &[], &cli).unwrap();
        assert_eq!(c.data_dir, std::path::PathBuf::from("/x/y"));
    }

    #[test]
    fn reserved_env_are_skipped() {
        let env = vec![
            env("ZAM_ADMIN_PASSWORD", "x"),
            env("ZAM_URL", "x"),
            env("ZAM_TOKEN", "x"),
            env("ZAM_MASTER_KEY", "x"),
        ];
        assert!(load(None, &env, &Default::default()).is_ok());
    }

    #[test]
    fn build_env_are_skipped() {
        // Сборочные переменные CI/разработчика не должны ломать запуск сервера.
        let env = vec![
            env("ZAM_GIT_SHA", "abc"),
            env("ZAM_WEB_REBUILD", "1"),
            env("ZAM_WEB_INDEX", "/x/index.html"),
            env("ZAM_VCRUNTIME_DLL", "C:/x/vcruntime140.dll"),
        ];
        assert_eq!(
            load(None, &env, &Default::default()).unwrap(),
            ServerConfig::default()
        );
    }

    #[test]
    fn unknown_env_key_is_error() {
        let env = vec![env("ZAM_SERVER__BINDD", "1")];
        let e = load(None, &env, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("ZAM_SERVER__BINDD"), "{e}");
    }

    #[test]
    fn unknown_file_key_is_error() {
        let f = write_in_target("unknown.toml", "[server]\nbnd = \"1.2.3.4:1\"\n");
        let e = load(Some(&f), &[], &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("server.bnd") && e.contains("file"), "{e}");
    }

    #[test]
    fn missing_file_is_error() {
        let p = std::path::Path::new("no/such/zam.toml");
        assert!(load(Some(p), &[], &Default::default()).is_err());
    }

    #[test]
    fn base_path_normalized() {
        for (raw, want) in [
            ("memory/", "/memory"),
            ("/memory/", "/memory"),
            ("/", ""),
            ("", ""),
        ] {
            let env = vec![env("ZAM_SERVER__BASE_PATH", raw)];
            let c = load(None, &env, &Default::default()).unwrap();
            assert_eq!(c.server.base_path, want, "raw={raw:?}");
        }
    }

    #[test]
    fn trusted_proxies_cidr_and_bare_ip() {
        let f = write_in_target(
            "proxies.toml",
            "[server]\ntrusted_proxies = [\"10.0.0.0/8\", \"127.0.0.1\", \"::1\"]\n",
        );
        let c = load(Some(&f), &[], &Default::default()).unwrap();
        let got: Vec<String> = c
            .server
            .trusted_proxies
            .iter()
            .map(|n| n.to_string())
            .collect();
        assert_eq!(got, ["10.0.0.0/8", "127.0.0.1/32", "::1/128"]);
    }

    #[test]
    fn trusted_proxies_from_env_array() {
        let env = vec![env("ZAM_SERVER__TRUSTED_PROXIES", "[\"192.168.0.0/16\"]")];
        let c = load(None, &env, &Default::default()).unwrap();
        assert_eq!(c.server.trusted_proxies[0].to_string(), "192.168.0.0/16");
    }

    #[test]
    fn enums_lowercase() {
        let env = vec![
            env("ZAM_LOG__FORMAT", "text"),
            env("ZAM_QDRANT__MODE", "external"),
        ];
        let c = load(None, &env, &Default::default()).unwrap();
        assert_eq!(c.log.format, LogFormat::Text);
        assert_eq!(c.qdrant.mode, QdrantMode::External);
    }

    #[test]
    fn qdrant_section_layers_and_supervisor_config() {
        use zam_index::supervisor::{SupervisorConfig, default_binary};
        let f = write_in_target(
            "qdrant.toml",
            "[qdrant]\nhttp_port = 7333\nmax_search_threads = 4\nbinary = \"/opt/q/qdrant\"\n",
        );
        let e = vec![
            env("ZAM_QDRANT__GRPC_PORT", "7334"),
            env("ZAM_QDRANT__OPTIMIZER_CPU_BUDGET", "1"),
        ];
        let c = load(Some(&f), &e, &Default::default()).unwrap();
        let exe = std::path::Path::new("inst");
        let Some(SupervisorConfig::Managed(m)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался managed");
        };
        assert_eq!((m.http_port, m.grpc_port), (7333, 7334));
        assert_eq!((m.max_search_threads, m.optimizer_cpu_budget), (4, 1));
        assert_eq!(m.binary, std::path::PathBuf::from("/opt/q/qdrant"));

        // По умолчанию — бинарник из комплекта.
        let d = load(None, &[], &Default::default()).unwrap();
        assert_eq!(d.qdrant.binary_path(exe), default_binary(exe));

        // Ключ — только для external; там же обязателен url.
        let e = vec![env("ZAM_QDRANT__API_KEY", "s3cret-key")];
        let c = load(None, &e, &Default::default()).unwrap();
        let err = c.qdrant.supervisor_config(exe).unwrap_err().to_string();
        assert!(err.contains("qdrant.api_key"), "{err}");
        assert!(!format!("{c:?}").contains("s3cret-key"), "ключ в Debug");
        let mut e = vec![
            env("ZAM_QDRANT__MODE", "external"),
            env("ZAM_QDRANT__API_KEY", "s3cret-key"),
        ];
        let c = load(None, &e, &Default::default()).unwrap();
        let err = c.qdrant.supervisor_config(exe).unwrap_err().to_string();
        assert!(err.contains("qdrant.url"), "{err}");
        e.push(env("ZAM_QDRANT__URL", "http://10.0.0.5:6334"));
        let c = load(None, &e, &Default::default()).unwrap();
        let Some(SupervisorConfig::External(x)) = c.qdrant.supervisor_config(exe).unwrap() else {
            panic!("ожидался external");
        };
        assert_eq!(x.grpc_url, "http://10.0.0.5:6334");
        assert_eq!(x.api_key.as_deref(), Some("s3cret-key"));

        let e = vec![env("ZAM_QDRANT__MODE", "edge")];
        let c = load(None, &e, &Default::default()).unwrap();
        assert!(c.qdrant.supervisor_config(exe).unwrap().is_none());
    }

    #[test]
    fn bad_value_names_key_and_source() {
        let env = vec![env("ZAM_SERVER__BIND", "nope")];
        let e = load(None, &env, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("server.bind") && e.contains("ZAM_SERVER__BIND"),
            "{e}"
        );
    }

    #[test]
    fn admin_password_missing_is_error() {
        let c = load(None, &[], &Default::default()).unwrap();
        let e = c.admin_secret(None).unwrap_err().to_string();
        assert!(e.contains("hash-password") && e.contains("ZAM_ADMIN_PASSWORD"));
    }

    #[test]
    fn admin_password_from_env_is_hashed() {
        let c = load(None, &[], &Default::default()).unwrap();
        let AdminSecret::Hash(h) = c.admin_secret(Some("s3cret")).unwrap();
        assert!(h.starts_with("$argon2id$") && !h.contains("s3cret"));
    }

    #[test]
    fn admin_hash_from_config_wins() {
        let AdminSecret::Hash(h) = load(None, &[], &Default::default())
            .unwrap()
            .admin_secret(Some("pw"))
            .unwrap();
        let env = vec![env("ZAM_ADMIN__PASSWORD_HASH", &h)];
        let c = load(None, &env, &Default::default()).unwrap();
        let AdminSecret::Hash(got) = c.admin_secret(Some("other")).unwrap();
        assert_eq!(got, h);
    }

    #[test]
    fn invalid_admin_hash_is_error() {
        let env = vec![env("ZAM_ADMIN__PASSWORD_HASH", "plain")];
        let c = load(None, &env, &Default::default()).unwrap();
        assert!(c.admin_secret(None).is_err());
    }

    #[test]
    fn example_config_parses_to_defaults() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/config.example.toml");
        let c = load(Some(&p), &[], &Default::default()).unwrap();
        assert_eq!(c, load(None, &[], &Default::default()).unwrap());
    }

    #[test]
    fn env_string_like_values_for_string_fields() {
        for (raw, want) in [("123", "123"), ("true", "true"), ("nan", "nan")] {
            let env = vec![env("ZAM_ADMIN__LOGIN", raw)];
            let c = load(None, &env, &Default::default()).unwrap();
            assert_eq!(c.admin.login, want);
        }
        let e2 = vec![
            env("ZAM_DATA_DIR", "2024-01-01"),
            env("ZAM_SERVER__BASE_PATH", "123"),
        ];
        let c = load(None, &e2, &Default::default()).unwrap();
        assert_eq!(c.data_dir, std::path::PathBuf::from("2024-01-01"));
        assert_eq!(c.server.base_path, "/123");
        let e3 = vec![env("ZAM_LOG__MAX_FILES", "7")];
        assert_eq!(
            load(None, &e3, &Default::default()).unwrap().log.max_files,
            7
        );
        let e4 = vec![env("ZAM_LOG__MAX_FILES", "abc")];
        let e = load(None, &e4, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("log.max_files") && e.contains("ZAM_LOG__MAX_FILES"),
            "{e}"
        );
    }

    #[test]
    fn empty_unknown_table_is_error() {
        let f = write_in_target("bogus.toml", "[bogus]\n");
        let e = load(Some(&f), &[], &Default::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("bogus"), "{e}");
        let f = write_in_target("empty_known.toml", "[server]\n[log]\n");
        assert!(load(Some(&f), &[], &Default::default()).is_ok());
    }

    #[test]
    fn syntax_error_does_not_quote_source() {
        let f = write_in_target(
            "broken.toml",
            "[admin]\npassword_hash = \"$argon2id$SECRET\n",
        );
        let e = load(Some(&f), &[], &Default::default())
            .unwrap_err()
            .to_string();
        assert!(!e.contains("SECRET"), "{e}");
        assert!(e.contains("строка 2"), "{e}");
    }
}
