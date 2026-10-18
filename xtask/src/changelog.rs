//! Извлечение раздела версии из CHANGELOG.md для текста релиза.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Тело раздела `## [X.Y.Z]` (без заголовка); `v`-префикс версии отбрасывается.
pub fn section(text: &str, version: &str) -> Result<String> {
    let v = version.strip_prefix('v').unwrap_or(version);
    let head = format!("## [{v}]");
    let mut body: Vec<&str> = Vec::new();
    let mut found = false;
    for line in text.lines() {
        if found {
            if line.starts_with("## [") {
                break;
            }
            body.push(line);
        } else if line == head || line.starts_with(&format!("{head} ")) {
            found = true;
        }
    }
    if !found {
        bail!("в CHANGELOG.md нет раздела '## [{v}]'");
    }
    let body = body.join("\n").trim().to_string();
    if body.is_empty() {
        bail!("раздел '## [{v}]' в CHANGELOG.md пуст");
    }
    Ok(body)
}

pub fn run(changelog: &Path, version: &str) -> Result<()> {
    let text =
        fs::read_to_string(changelog).with_context(|| format!("чтение {}", changelog.display()))?;
    println!("{}", section(&text, version)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Журнал\n\n## [Unreleased]\n\n- wip\n\n## [0.0.1-m0] - 2026-10-18\n\n### Added\n- a\n- b\n\n## [0.0.0] - 2026-01-01\n\n- old\n";

    #[test]
    fn extracts_section_body_with_and_without_v() {
        let want = "### Added\n- a\n- b";
        assert_eq!(section(LOG, "v0.0.1-m0").unwrap(), want);
        assert_eq!(section(LOG, "0.0.1-m0").unwrap(), want);
        assert_eq!(section(LOG, "0.0.0").unwrap(), "- old");
    }

    #[test]
    fn missing_or_empty_section_is_error() {
        assert!(section(LOG, "9.9.9").is_err());
        assert!(
            section(LOG, "0.0.1").is_err(),
            "префикс версии не совпадение"
        );
        assert!(section("## [1.0.0] - x\n\n## [0.9.0]\n- y\n", "1.0.0").is_err());
    }
}
