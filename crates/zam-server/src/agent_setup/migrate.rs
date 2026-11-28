//! Задание агенту на перенос проекта в память (M7.6 §9.1): `GET /agent-setup?migrate=true` →
//! поле `migration_prompt`, WebUI «Подключение агента» → «Подключить с переносом данных».
//!
//! - **Один источник текста — сервер** **[решение координатора 136]**: шаблоны
//!   `migrate.{ru,en}.md` (`include_str!`), синхронны по разделам, условным строкам и подстановкам
//!   (тесты ниже). Обобщают перенос AIIndicators 2026-11-28 (подключение, опись, раскладка по
//!   видам памяти, сессии, карта кода, проверка, отчёт, правила работы).
//! - **Подстановки** — один проход по строке ([`substitute`]): значение (имя решения) не
//!   разбирается повторно, `{url}` в имени остаётся текстом. `{solution}` — «имя» и GUID или
//!   указание на `zam init`; `{url}` — внешний адрес сервера; `{init}` — однострочник `zam init`
//!   без токена (POSIX-кавычки: имя прошло [`super::safe_cli_name`], адрес — без `'`);
//!   `{agent_file}` — `CLAUDE.md` или `AGENTS.md`.
//! - **Условные строки** — префикс `{?claude}`, `{?codex,agents}`: строка остаётся только для
//!   перечисленных агентов, префикс снимается.
//! - **Токена нет никогда** (§15.5): открытый токен — только в `install` и `init_cmd`.
use super::{AgentKind, Lang, SolutionRef};

/// Шаблоны по умолчанию.
pub const MIGRATE_RU: &str = include_str!("migrate.ru.md");
pub const MIGRATE_EN: &str = include_str!("migrate.en.md");

/// Имена подстановок шаблона.
pub const PLACEHOLDERS: [&str; 4] = ["solution", "url", "init", "agent_file"];

/// `{solution}` без выбранного решения.
const SOLUTION_AUTO_RU: &str = "новое — его создаст `zam init` (имя — имя каталога проекта)";
const SOLUTION_AUTO_EN: &str =
    "a new one — `zam init` creates it (named after the project directory)";

/// Шаблон языка.
pub fn template(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => MIGRATE_RU,
        Lang::En => MIGRATE_EN,
    }
}

/// Что подставить в задание.
pub struct MigrateCtx<'a> {
    pub url: &'a str,
    pub solution: Option<&'a SolutionRef>,
    pub agent: AgentKind,
    pub lang: Lang,
    /// Однострочник `zam init` без токена ([`super::init_sh`]).
    pub init: &'a str,
}

/// Условная строка: `{?a,b}текст` → (`"a,b"`, `"текст"`).
pub fn agent_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("{?")?;
    let (list, text) = rest.split_once('}')?;
    Some((list, text))
}

/// Подстановка `{имя}` из `vals` одним проходом: вставленное значение не разбирается снова;
/// прочие фигурные скобки (JSON в тексте задания) остаются как есть.
pub fn substitute(line: &str, vals: &[(&str, &str)], out: &mut String) {
    let mut rest = line;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let hit = vals.iter().find(|(k, _)| {
            tail.strip_prefix(k)
                .is_some_and(|after| after.starts_with('}'))
        });
        match hit {
            Some((k, v)) => {
                out.push_str(v);
                rest = &tail[k.len() + 1..];
            }
            None => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
}

/// Текст задания для агента `c.agent` на языке `c.lang`.
pub fn render(c: &MigrateCtx<'_>) -> String {
    let solution = match (c.solution, c.lang) {
        (Some(s), Lang::Ru) => format!("«{}» (`{}`)", s.name, s.guid),
        (Some(s), Lang::En) => format!("\"{}\" (`{}`)", s.name, s.guid),
        (None, Lang::Ru) => SOLUTION_AUTO_RU.into(),
        (None, Lang::En) => SOLUTION_AUTO_EN.into(),
    };
    let agent_file = match c.agent {
        AgentKind::Claude => "CLAUDE.md",
        AgentKind::Agents | AgentKind::Codex => "AGENTS.md",
    };
    let vals = [
        ("solution", solution.as_str()),
        ("url", c.url),
        ("init", c.init),
        ("agent_file", agent_file),
    ];
    let t = template(c.lang);
    let mut out = String::with_capacity(t.len() + 512);
    for line in t.lines() {
        let line = match agent_line(line) {
            Some((list, text)) if list.split(',').any(|a| a == c.agent.as_str()) => text,
            Some(_) => continue,
            None => line,
        };
        substitute(line, &vals, &mut out);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sol() -> SolutionRef {
        SolutionRef {
            guid: "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11".into(),
            name: "Trading X".into(),
        }
    }

    /// Заголовки `#`…: (уровень, текст).
    fn headings(t: &str) -> Vec<(usize, &str)> {
        t.lines()
            .filter_map(|l| {
                let level = l.chars().take_while(|&c| c == '#').count();
                (level > 0 && l[level..].starts_with(' ')).then(|| (level, l[level + 1..].trim()))
            })
            .collect()
    }

    /// Списки агентов условных строк по порядку.
    fn markers(t: &str) -> Vec<&str> {
        t.lines()
            .filter_map(|l| agent_line(l).map(|(a, _)| a))
            .collect()
    }

    /// Имена `{слово}` (подстановки) по порядку появления, без повторов.
    fn placeholders(t: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut rest = t;
        while let Some(i) = rest.find('{') {
            rest = &rest[i + 1..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            if !name.is_empty() && rest[name.len()..].starts_with('}') && !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// ru и en синхронны: заголовки (уровень, число, порядок), условные строки, подстановки;
    /// у условных строк — только известные агенты; строки — LF.
    #[test]
    fn templates_in_sync() {
        let (hr, he) = (headings(MIGRATE_RU), headings(MIGRATE_EN));
        assert_eq!(hr.len(), he.len(), "заголовки ru {hr:?}, en {he:?}");
        for (r, e) in hr.iter().zip(&he) {
            assert_eq!(r.0, e.0, "уровень: «{}» и «{}»", r.1, e.1);
        }
        assert_eq!(hr.len(), 9, "заголовок и 8 разделов: {hr:?}");
        assert_eq!(markers(MIGRATE_RU), markers(MIGRATE_EN));
        for list in markers(MIGRATE_RU) {
            for a in list.split(',') {
                assert!(AgentKind::parse(a).is_some(), "агент «{a}» в {{?{list}}}");
            }
        }
        for t in [MIGRATE_RU, MIGRATE_EN] {
            let p = placeholders(t);
            assert_eq!(p, placeholders(MIGRATE_RU), "подстановки");
            for name in &p {
                assert!(
                    PLACEHOLDERS.contains(&name.as_str()),
                    "неизвестная {{{name}}}"
                );
            }
            assert!(!t.contains('\r'), "CRLF в шаблоне");
            for a in AgentKind::ALL {
                assert!(
                    markers(t)
                        .iter()
                        .any(|l| l.split(',').any(|x| x == a.as_str())),
                    "нет строк агента {}",
                    a.as_str()
                );
            }
        }
    }

    #[test]
    fn render_substitutes_per_agent() {
        let s = sol();
        let init = "zam init --url 'http://h:1' --agent claude --id 0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11 --yes";
        for lang in Lang::ALL {
            for agent in AgentKind::ALL {
                let t = render(&MigrateCtx {
                    url: "http://h:1",
                    solution: Some(&s),
                    agent,
                    lang,
                    init,
                });
                for want in [
                    "Trading X",
                    s.guid.as_str(),
                    "`http://h:1`",
                    init,
                    "zam guide",
                ] {
                    assert!(t.contains(want), "{lang:?} {agent:?}: нет «{want}»\n{t}");
                }
                assert!(!t.contains("{?"), "{t}");
                for p in PLACEHOLDERS {
                    assert!(!t.contains(&format!("{{{p}}}")), "{{{p}}} осталась: {t}");
                }
                let file = if agent == AgentKind::Claude {
                    "CLAUDE.md"
                } else {
                    "AGENTS.md"
                };
                assert!(t.contains(&format!("`{file}`")), "{t}");
                let claude = t.contains("~/.claude/projects/");
                let codex = t.contains("~/.codex/sessions/");
                let cursor = t.contains(".cursor/rules/");
                assert_eq!(
                    (claude, codex, cursor),
                    (
                        agent == AgentKind::Claude,
                        agent == AgentKind::Codex,
                        agent == AgentKind::Agents
                    ),
                    "{agent:?}"
                );
                assert!(!t.contains("zam_"), "{t}");
            }
        }
    }

    #[test]
    fn render_without_solution() {
        for (lang, want) in [(Lang::Ru, SOLUTION_AUTO_RU), (Lang::En, SOLUTION_AUTO_EN)] {
            let t = render(&MigrateCtx {
                url: "http://h:1",
                solution: None,
                agent: AgentKind::Codex,
                lang,
                init: "zam init --url 'http://h:1' --agent codex --yes",
            });
            assert!(t.contains(want), "{t}");
        }
    }

    /// Значение не разбирается повторно: `{url}` в имени решения — текст.
    #[test]
    fn substitution_single_pass() {
        let mut out = String::new();
        substitute(
            "a {solution} b {url} {x} {\"k\":1} {",
            &[("solution", "{url}"), ("url", "U")],
            &mut out,
        );
        assert_eq!(out, "a {url} b U {x} {\"k\":1} {");
        assert_eq!(
            agent_line("{?claude,codex}- x"),
            Some(("claude,codex", "- x"))
        );
        assert_eq!(agent_line("- {?claude}"), None);
    }
}
