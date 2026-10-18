//! Цель команды: `[space:]path` или id узла; файл решения `solution.toml` (§11.1).
//!
//! - Префикс `space:` распознаётся, только если часть до первого `:` — ровно
//!   `rules`, `knowledge`, `human`, `solution` или GUID (UUID с дефисами); иначе
//!   вся строка — путь (`a:b` остаётся путём).
//! - Без префикса — решение из ближайшего `solution.toml`, иначе `knowledge`.
//! - `solution:` без `solution.toml` — ошибка конфигурации.
//! - 26 символов в канонической форме [`zam_core::Id`] — id узла.
//! - Ведущие и замыкающие `/` пути отбрасываются.
use std::path::Path;

use crate::client::CliError;

/// Решение из `solution.toml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Solution {
    pub id: String,
    pub name: String,
}

/// Предел размера `solution.toml`, байт.
const SOLUTION_MAX: u64 = 64 * 1024;

/// Читает `solution.toml`: поля `id` (GUID) и `name`.
pub fn load_solution(path: &Path) -> Result<Solution, CliError> {
    let fail = |m: String| CliError::config(format!("{}: {m}", path.display()));
    let meta = std::fs::metadata(path).map_err(|e| fail(e.to_string()))?;
    if meta.len() > SOLUTION_MAX {
        return Err(fail(format!("файл больше {SOLUTION_MAX} байт")));
    }
    let text = std::fs::read_to_string(path).map_err(|e| fail(e.to_string()))?;
    let s: Solution = toml::from_str(&text).map_err(|e| fail(e.message().to_string()))?;
    if !is_guid(s.id.trim()) {
        return Err(fail("id — UUID с дефисами".into()));
    }
    if s.name.trim().is_empty() {
        return Err(fail("name не задан".into()));
    }
    Ok(Solution {
        id: s.id.trim().to_ascii_lowercase(),
        name: s.name.trim().to_string(),
    })
}

/// UUID строго в форме с дефисами (8-4-4-4-12).
pub fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Канонический id узла (26 символов Crockford, как печатает сервер).
fn is_node_id(s: &str) -> bool {
    s.len() == 26
        && s.parse::<zam_core::Id>()
            .is_ok_and(|id| id.to_string() == s)
}

/// Пространство цели до разрешения решения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSel {
    /// Без префикса: решение, если есть `solution.toml`, иначе knowledge.
    Default,
    Fixed(&'static str),
    /// `solution:` — решение из `solution.toml`.
    Solution,
    /// GUID решения (нижний регистр).
    Guid(String),
}

/// Разобранная цель.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Node(String),
    Path { space: SpaceSel, path: String },
}

/// Чистый разбор строки цели (без файловой системы).
pub fn parse(raw: &str) -> Target {
    if is_node_id(raw) {
        return Target::Node(raw.to_string());
    }
    let (space, rest) = match raw.split_once(':') {
        Some((p, rest)) => match p {
            "rules" => (SpaceSel::Fixed("rules"), rest),
            "knowledge" => (SpaceSel::Fixed("knowledge"), rest),
            "human" => (SpaceSel::Fixed("human"), rest),
            "solution" => (SpaceSel::Solution, rest),
            g if is_guid(g) => (SpaceSel::Guid(g.to_ascii_lowercase()), rest),
            _ => (SpaceSel::Default, raw),
        },
        None => (SpaceSel::Default, raw),
    };
    Target::Path {
        space,
        path: rest.trim_matches('/').to_string(),
    }
}

/// Цель для запросов API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loc {
    Node(String),
    Path {
        /// Ссылка на пространство для API: имя или GUID.
        space: String,
        path: String,
        /// Пространство — решение из `solution.toml` (его надо регистрировать перед записью).
        own_solution: bool,
    },
}

impl Target {
    pub fn resolve(self, sol: Option<&Solution>) -> Result<Loc, CliError> {
        let (space, path) = match self {
            Target::Node(id) => return Ok(Loc::Node(id)),
            Target::Path { space, path } => (space, path),
        };
        let (space, own_solution) = match (space, sol) {
            (SpaceSel::Fixed(n), _) => (n.to_string(), false),
            (SpaceSel::Guid(g), s) => {
                let own = s.is_some_and(|s| s.id == g);
                (g, own)
            }
            (SpaceSel::Solution | SpaceSel::Default, Some(s)) => (s.id.clone(), true),
            (SpaceSel::Default, None) => ("knowledge".to_string(), false),
            (SpaceSel::Solution, None) => {
                return Err(CliError::config(
                    "префикс solution: требует .zeroaimemory/solution.toml в текущем каталоге или выше",
                ));
            }
        };
        Ok(Loc::Path {
            space,
            path,
            own_solution,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11";

    fn p(space: SpaceSel, path: &str) -> Target {
        Target::Path {
            space,
            path: path.into(),
        }
    }

    #[test]
    fn parse_forms() {
        assert_eq!(
            parse("knowledge:a/b"),
            p(SpaceSel::Fixed("knowledge"), "a/b")
        );
        assert_eq!(parse("rules:x"), p(SpaceSel::Fixed("rules"), "x"));
        assert_eq!(parse("human:"), p(SpaceSel::Fixed("human"), ""));
        assert_eq!(parse("solution:x"), p(SpaceSel::Solution, "x"));
        assert_eq!(
            parse(&format!("{}:x", G.to_uppercase())),
            p(SpaceSel::Guid(G.into()), "x")
        );
        assert_eq!(parse("a/b"), p(SpaceSel::Default, "a/b"));
        // неизвестный префикс — часть пути
        assert_eq!(parse("a:b"), p(SpaceSel::Default, "a:b"));
        assert_eq!(parse("Knowledge:x"), p(SpaceSel::Default, "Knowledge:x"));
        assert_eq!(
            parse("knowledge:Инструменты/Bash скрипты"),
            p(SpaceSel::Fixed("knowledge"), "Инструменты/Bash скрипты")
        );
        assert_eq!(parse("knowledge:/a/"), p(SpaceSel::Fixed("knowledge"), "a"));
        assert_eq!(parse(""), p(SpaceSel::Default, ""));
        let id = zam_core::Id::new().to_string();
        assert_eq!(parse(&id), Target::Node(id.clone()));
        // некононическая форма id — путь
        assert_eq!(
            parse(&id.to_lowercase()),
            p(SpaceSel::Default, &id.to_lowercase())
        );
        assert_eq!(
            parse("abcdefghijklmnopqrstuvwxyz"),
            p(SpaceSel::Default, "abcdefghijklmnopqrstuvwxyz")
        );
    }

    #[test]
    fn resolve_spaces() {
        let sol = Solution {
            id: G.into(),
            name: "S".into(),
        };
        let loc = |t: Target, s: Option<&Solution>| t.resolve(s).unwrap();
        assert_eq!(
            loc(parse("x"), Some(&sol)),
            Loc::Path {
                space: G.into(),
                path: "x".into(),
                own_solution: true
            }
        );
        assert_eq!(
            loc(parse("x"), None),
            Loc::Path {
                space: "knowledge".into(),
                path: "x".into(),
                own_solution: false
            }
        );
        assert_eq!(
            loc(parse(&format!("{G}:x")), Some(&sol)),
            Loc::Path {
                space: G.into(),
                path: "x".into(),
                own_solution: true
            }
        );
        let e = parse("solution:x").resolve(None).unwrap_err();
        assert_eq!(e.code, crate::client::EXIT_CONFIG);
    }

    #[test]
    fn guid_check() {
        assert!(is_guid(G));
        assert!(is_guid(&G.to_uppercase()));
        assert!(!is_guid("0192f1c47a1e7c3b9a552b1f6f0d9e11"));
        assert!(!is_guid("0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e1z"));
    }

    #[test]
    fn solution_file() {
        let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli/solution-file");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("solution.toml");
        std::fs::write(
            &f,
            format!("id = \"{}\"\nname = \" T \"\n", G.to_uppercase()),
        )
        .unwrap();
        assert_eq!(
            load_solution(&f).unwrap(),
            Solution {
                id: G.into(),
                name: "T".into()
            }
        );
        for bad in ["id = [", "name = \"x\"", "id = \"nope\"\nname = \"x\""] {
            std::fs::write(&f, bad).unwrap();
            let e = load_solution(&f).unwrap_err();
            assert_eq!(e.code, crate::client::EXIT_CONFIG, "{bad}");
            assert!(e.msg.contains("solution.toml"), "{}", e.msg);
        }
    }
}
