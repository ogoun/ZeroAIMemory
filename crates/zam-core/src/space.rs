//! Префикс пространства в адресе `[<пространство>:]<путь>` — общее правило CLI (`zam tree
//! rules:`, `zam write knowledge:a/b`, `zam read`) и MCP (`memory_tree`, `memory_write`; M6.6 §8,
//! замечание 26).
//!
//! - Префикс распознаётся, только если часть до первого `:` — ровно `rules`, `knowledge`,
//!   `human`, `solution` или GUID решения (UUID с дефисами, любой регистр); иначе вся строка —
//!   путь (`a:b`, `Knowledge:x` остаются путями).
//! - Косая после двоеточия необязательна: `rules:`, `rules:/` — корень Rules, `rules:a/b` и
//!   `rules:/a/b` — путь `a/b`. Ведущие и замыкающие `/` пути отбрасываются.
//! - Что значат `solution` и отсутствие префикса, решает потребитель: CLI — решение из
//!   `solution.toml`, MCP — из заголовка `X-Zam-Solution`.
//! - Исключение MCP **[решение координатора, задача 7]**: форму `<слово>:/…`, слово которой
//!   правило не знает ([`SpaceSel::Default`]), MCP не считает путём — 26-символьный id
//!   пространства ([`crate::Id`]) адресует это пространство (так `memory_write` принимал его и
//!   раньше), прочее слово — ошибка аргумента. Форма `слово:x` без косой и в MCP — путь, как в CLI.

/// Пространство адреса до разрешения решения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSel {
    /// Без префикса: пространство по умолчанию потребителя.
    Default,
    /// `rules`, `knowledge` или `human`.
    Fixed(&'static str),
    /// `solution:` — решение потребителя (`solution.toml`, `X-Zam-Solution`).
    Solution,
    /// GUID решения (нижний регистр).
    Guid(String),
}

/// UUID строго в форме с дефисами (8-4-4-4-12), любой регистр.
pub fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// `[<пространство>:]<путь>` → пространство и путь без `/` по краям (правило — в описании
/// модуля).
pub fn split_space(raw: &str) -> (SpaceSel, &str) {
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
    (space, rest.trim_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11";

    /// Таблица правила (CLI `zam tree|read|write`, MCP `memory_tree|memory_write`): вход →
    /// пространство и путь.
    #[test]
    fn prefix_table() {
        let upper = format!("{}:/a/b", G.to_uppercase());
        let cases: Vec<(&str, SpaceSel, &str)> = vec![
            ("rules:", SpaceSel::Fixed("rules"), ""),
            ("rules:/", SpaceSel::Fixed("rules"), ""),
            ("rules:/a/b", SpaceSel::Fixed("rules"), "a/b"),
            ("rules:a/b", SpaceSel::Fixed("rules"), "a/b"),
            ("rules:x", SpaceSel::Fixed("rules"), "x"),
            ("knowledge:/a/", SpaceSel::Fixed("knowledge"), "a"),
            (
                "knowledge:Инструменты/Bash скрипты",
                SpaceSel::Fixed("knowledge"),
                "Инструменты/Bash скрипты",
            ),
            ("human:", SpaceSel::Fixed("human"), ""),
            ("solution:x", SpaceSel::Solution, "x"),
            ("solution:", SpaceSel::Solution, ""),
            (&upper, SpaceSel::Guid(G.into()), "a/b"),
            // неизвестный префикс и иной регистр — часть пути
            ("a:b", SpaceSel::Default, "a:b"),
            ("Knowledge:x", SpaceSel::Default, "Knowledge:x"),
            ("foo:/x", SpaceSel::Default, "foo:/x"),
            ("a/b", SpaceSel::Default, "a/b"),
            ("/a/b/", SpaceSel::Default, "a/b"),
            ("", SpaceSel::Default, ""),
        ];
        for (raw, space, path) in cases {
            assert_eq!(split_space(raw), (space, path), "{raw}");
        }
    }

    #[test]
    fn guid_form() {
        assert!(is_guid(G));
        assert!(is_guid(&G.to_uppercase()));
        assert!(!is_guid("0192f1c47a1e7c3b9a552b1f6f0d9e11"));
        assert!(!is_guid("0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e1z"));
        assert!(!is_guid(""));
    }
}
