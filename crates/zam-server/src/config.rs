//! Конфигурация сервера. Слои: умолчания, TOML-файл, `ZAM_*`, CLI.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, SaltString, rand_core::OsRng};
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml::{Table, Value};

/// Переменные `ZAM_*`, которые не являются слоем конфигурации.
const RESERVED_ENV: [&str; 4] = [
    "ZAM_ADMIN_PASSWORD",
    "ZAM_URL",
    "ZAM_TOKEN",
    "ZAM_MASTER_KEY",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub server: ServerSection,
    pub admin: AdminSection,
    pub data_dir: PathBuf,
    pub log: LogSection,
    pub qdrant: QdrantSection,
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

/// В M0 только разбирается.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QdrantSection {
    pub mode: QdrantMode,
    pub url: Option<String>,
    pub http_port: u16,
    pub grpc_port: u16,
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
    let mut leaves: Vec<(Vec<String>, String, Value)> = Vec::new();

    if let Some(path) = file {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: shown.clone(),
            message: e.to_string(),
        })?;
        let table: Table = toml::from_str(&text).map_err(|e| ConfigError::Syntax {
            path: shown.clone(),
            message: e.to_string(),
        })?;
        flatten(&mut Vec::new(), &Value::Table(table), &mut |p, v| {
            leaves.push((p, format!("file {shown}"), v));
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
        leaves.push((path, format!("env {name}"), parse_scalar(raw)));
    }

    if let Some(bind) = cli.bind {
        leaves.push((
            vec!["server".into(), "bind".into()],
            "cli".into(),
            Value::String(bind.to_string()),
        ));
    }
    if let Some(dir) = &cli.data_dir {
        leaves.push((
            vec!["data_dir".into()],
            "cli".into(),
            Value::String(dir.to_string_lossy().into_owned()),
        ));
    }

    let defaults = merged.clone();
    for (path, source, value) in leaves {
        // Проверка значения в изоляции, чтобы указать ключ и источник.
        let mut probe = defaults.clone();
        set_path(&mut probe, &path, value.clone());
        if let Err(e) = probe.try_into::<ServerConfig>() {
            return Err(ConfigError::Invalid {
                key: path.join("."),
                source_name: source,
                message: e.message().to_string(),
            });
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
        Value::Table(t) => {
            for (k, child) in t {
                prefix.push(k.clone());
                flatten(prefix, child, f);
                prefix.pop();
            }
        }
        leaf => f(prefix.clone(), leaf.clone()),
    }
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
}
