//! Секция `[code]` клиента (спецификация M5.6 §12): `~/.zeroaimemory/config.toml` и
//! `<корень>/.zeroaimemory/config.toml`, проект сильнее домашнего по каждому ключу. Неизвестный
//! ключ, значение вне диапазона, неизвестный язык или неверная маска — ошибка конфигурации
//! (код 1) с именем ключа и файла: молча выключенная карта хуже явной ошибки.
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use zam_code::Lang;

use crate::client::CliError;

/// Предел файла настроек, байт (как у `[hooks]`).
const CONFIG_MAX: u64 = 1024 * 1024;

/// Настройки карты кода клиента.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeConfig {
    pub enabled: bool,
    /// Фоновая синхронизация из хуков (задача 6).
    pub auto: bool,
    pub auto_interval_min: u32,
    /// `None` — все собранные языки.
    pub languages: Option<Vec<Lang>>,
    /// Маски `globset` относительно корня (`/`).
    pub exclude: Vec<String>,
    pub max_file_bytes: u64,
    pub max_files: u32,
    pub parse_timeout_ms: u64,
    pub send_signatures: bool,
}

impl Default for CodeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto: true,
            auto_interval_min: 30,
            languages: None,
            exclude: Vec::new(),
            max_file_bytes: 1_048_576,
            max_files: 50_000,
            parse_timeout_ms: 200,
            send_signatures: true,
        }
    }
}

impl CodeConfig {
    /// Язык включён `languages`.
    pub fn lang_on(&self, l: Lang) -> bool {
        self.languages.as_ref().is_none_or(|v| v.contains(&l))
    }

    /// Набор масок `exclude` (проверены при загрузке).
    pub fn exclude_set(&self) -> GlobSet {
        globs(&self.exclude).unwrap_or_else(|_| GlobSet::empty())
    }
}

/// Ключи секции из одного файла; нет ключа — `None`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    enabled: Option<bool>,
    auto: Option<bool>,
    auto_interval_min: Option<i64>,
    languages: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    max_file_bytes: Option<i64>,
    max_files: Option<i64>,
    parse_timeout_ms: Option<i64>,
    send_signatures: Option<bool>,
}

impl Section {
    /// `self` сильнее `weaker`.
    fn over(self, weaker: Section) -> Section {
        Section {
            enabled: self.enabled.or(weaker.enabled),
            auto: self.auto.or(weaker.auto),
            auto_interval_min: self.auto_interval_min.or(weaker.auto_interval_min),
            languages: self.languages.or(weaker.languages),
            exclude: self.exclude.or(weaker.exclude),
            max_file_bytes: self.max_file_bytes.or(weaker.max_file_bytes),
            max_files: self.max_files.or(weaker.max_files),
            parse_timeout_ms: self.parse_timeout_ms.or(weaker.parse_timeout_ms),
            send_signatures: self.send_signatures.or(weaker.send_signatures),
        }
    }
}

/// Файл целиком: прочие секции (`[hooks]`, адрес, токен) здесь не разбираются.
#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    code: Option<toml::Value>,
}

/// Секция `[code]` файла `path`; нет файла — пусто.
fn read_section(path: &Path) -> Result<Section, CliError> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(Section::default());
    };
    let bad = |e: String| CliError::config(format!("{}: {e}", path.display()));
    if !meta.is_file() {
        return Ok(Section::default());
    }
    if meta.len() > CONFIG_MAX {
        return Err(bad(format!("больше {CONFIG_MAX} байт")));
    }
    let text = std::fs::read_to_string(path).map_err(|e| bad(e.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let f: File = toml::from_str(text).map_err(|e| bad(e.message().to_string()))?;
    let Some(v) = f.code else {
        return Ok(Section::default());
    };
    v.try_into::<Section>()
        .map_err(|e| bad(format!("[code]: {}", e.message())))
}

fn globs(masks: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for m in masks {
        b.add(Glob::new(m).map_err(|e| format!("«{m}»: {e}"))?);
    }
    b.build().map_err(|e| e.to_string())
}

/// Значение в диапазоне `lo..=hi`, иначе код 1 с именем ключа.
fn ranged(v: Option<i64>, default: u64, lo: u64, hi: u64, key: &str) -> Result<u64, CliError> {
    let Some(v) = v else {
        return Ok(default);
    };
    match u64::try_from(v) {
        Ok(u) if (lo..=hi).contains(&u) => Ok(u),
        _ => Err(CliError::config(format!(
            "code.{key}: {v} вне диапазона {lo}..={hi}"
        ))),
    }
}

/// Файл настроек проекта и домашний.
pub fn paths(home: &Path, root: &Path) -> (PathBuf, PathBuf) {
    (
        root.join(".zeroaimemory").join("config.toml"),
        home.join(".zeroaimemory").join("config.toml"),
    )
}

/// `[code]` проекта поверх домашнего; проверка диапазонов §12.
pub fn load(home: &Path, root: &Path) -> Result<CodeConfig, CliError> {
    let (project, home_file) = paths(home, root);
    let home_sec = if home_file == project {
        Section::default()
    } else {
        read_section(&home_file)?
    };
    let s = read_section(&project)?.over(home_sec);
    let d = CodeConfig::default();
    let languages = match s.languages {
        None => None,
        Some(names) => {
            let mut out = Vec::with_capacity(names.len());
            for n in &names {
                let l = Lang::parse(n.trim()).ok_or_else(|| {
                    let all: Vec<&str> = Lang::ALL.iter().map(|l| l.as_str()).collect();
                    CliError::config(format!(
                        "code.languages: «{n}» — не язык; допустимы {}",
                        all.join(", ")
                    ))
                })?;
                if !out.contains(&l) {
                    out.push(l);
                }
            }
            Some(out)
        }
    };
    let exclude = s.exclude.unwrap_or_default();
    globs(&exclude).map_err(|e| CliError::config(format!("code.exclude: {e}")))?;
    Ok(CodeConfig {
        enabled: s.enabled.unwrap_or(d.enabled),
        auto: s.auto.unwrap_or(d.auto),
        auto_interval_min: ranged(s.auto_interval_min, 30, 1, 10_080, "auto_interval_min")? as u32,
        languages,
        exclude,
        max_file_bytes: ranged(
            s.max_file_bytes,
            d.max_file_bytes,
            1024,
            16 * 1024 * 1024,
            "max_file_bytes",
        )?,
        max_files: ranged(s.max_files, 50_000, 1, 500_000, "max_files")? as u32,
        parse_timeout_ms: ranged(s.parse_timeout_ms, 200, 10, 10_000, "parse_timeout_ms")?,
        send_signatures: s.send_signatures.unwrap_or(d.send_signatures),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(name: &str) -> (PathBuf, PathBuf) {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/zam-cli-code-config")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let (home, root) = (base.join("home"), base.join("root"));
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::create_dir_all(root.join(".zeroaimemory")).unwrap();
        (home, root)
    }

    #[test]
    fn defaults_without_files() {
        let (home, root) = dirs("defaults");
        assert_eq!(load(&home, &root).unwrap(), CodeConfig::default());
    }

    #[test]
    fn project_over_home_per_key_and_ranges() {
        let (home, root) = dirs("over");
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "url = \"http://x\"\n[hooks]\nprompt = false\n[code]\nmax_files = 7\nauto = false\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".zeroaimemory/config.toml"),
            "\u{feff}[code]\nmax_files = 9\nlanguages = [\"rust\", \"rust\", \"go\"]\n",
        )
        .unwrap();
        let c = load(&home, &root).unwrap();
        assert_eq!(c.max_files, 9);
        assert!(!c.auto);
        assert_eq!(c.languages, Some(vec![Lang::Rust, Lang::Go]));
        assert!(c.lang_on(Lang::Go) && !c.lang_on(Lang::Python));
        for (bad, key) in [
            ("max_file_bytes = 1023", "max_file_bytes"),
            ("parse_timeout_ms = 10001", "parse_timeout_ms"),
            ("max_files = -1", "max_files"),
            ("auto_interval_min = 0", "auto_interval_min"),
        ] {
            std::fs::write(
                root.join(".zeroaimemory/config.toml"),
                format!("[code]\n{bad}\n"),
            )
            .unwrap();
            let e = load(&home, &root).unwrap_err();
            assert_eq!(e.code, 1);
            assert!(e.msg.contains(key), "{}", e.msg);
        }
    }
}
