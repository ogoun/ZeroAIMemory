//! Разбор команды оболочки без зависимостей (M7.1 §4.6): слова с учётом кавычек ([`words`],
//! перенесён из журнала сессии хуков `zam-cli`), простая ли команда ([`is_simple`]) и слова
//! путей её аргументов ([`path_words`]). Их читают хук `zam hook post-tool` и прогон подсказок
//! `xtask eval --hints` — одно правило для клиента и замера.

/// Слова команды с учётом кавычек `"…"` и `'…'` (и `\\` вне одинарных) — грубый разбор
/// shell, достаточный для первого слова и аргументов.
pub fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            // в двойных кавычках `\` экранирует только `"`, `\`, `$` и `` ` `` (POSIX):
            // `"C:\Tools\zam.exe"` остаётся путём Windows
            (Some('"'), '\\') => match chars.clone().next() {
                Some(n @ ('"' | '\\' | '$' | '`')) => {
                    chars.next();
                    cur.push(n);
                }
                _ => cur.push('\\'),
            },
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Присваивание окружения перед командой: `ИМЯ=…` (имя — `[A-Za-z_][A-Za-z0-9_]*`).
pub fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut cs = name.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Простая команда (M7.1 §4.6 п. 1): вне кавычек нет `|`, `;`, `&` и перевода строки — ни
/// конвейера, ни списка команд (`&&`, `||`, `;`), ни фона, ни второй строки скрипта. Кавычки — как
/// у [`words`]; незакрытая кавычка — не простая.
pub fn is_simple(command: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                chars.next();
            }
            (Some(_), _) => {}
            (None, '\\') => {
                chars.next();
            }
            (None, '"' | '\'') => quote = Some(c),
            (None, '|' | ';' | '&' | '\n' | '\r') => return false,
            (None, _) => {}
        }
    }
    quote.is_none()
}

/// Предел слов путей команды ([`path_words`]).
pub const PATH_WORDS_MAX: usize = 32;
/// Короче — не слово пути (как слова ошибки в покрытии подсказок `post_tool`).
const PATH_WORD_MIN_CHARS: usize = 3;

/// Слова путей из аргументов команды (M7.1 §4.6 п. 3: имена каталогов и файлов, набранные в
/// команде, — не предмет её ошибки): аргументы [`words`] с `/` или `\` (у `--флаг=путь` —
/// значение после `=`) и аргументы без разделителя, для которых `is_dir` (существующий каталог;
/// программа и флаги `-…` не проверяются). Адреса (`://`) и аргументы с `@` (`user:pass@host`)
/// пропускаются. Слово — буквы и цифры от 3 символов в нижнем регистре, без повторов, до
/// [`PATH_WORDS_MAX`].
pub fn path_words(command: &str, is_dir: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut program = true;
    for w in words(command) {
        if program && is_assignment(&w) {
            continue;
        }
        let first = std::mem::take(&mut program);
        if w.contains("://") || w.contains('@') {
            continue;
        }
        let value = w.rsplit_once('=').map_or(w.as_str(), |(_, v)| v);
        let pathlike = value.contains(['/', '\\']);
        if !pathlike && (first || value.starts_with('-') || value.is_empty() || !is_dir(value)) {
            continue;
        }
        for part in value
            .split(|c: char| !c.is_alphanumeric())
            .filter(|p| p.chars().count() >= PATH_WORD_MIN_CHARS)
        {
            let part = part.to_lowercase();
            if !out.contains(&part) {
                out.push(part);
                if out.len() == PATH_WORDS_MAX {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_quotes_and_windows_paths() {
        assert_eq!(
            words(r#"FOO="a b" "C:\Tools\zam.exe" 'x y' "q\"t""#),
            ["FOO=a b", r"C:\Tools\zam.exe", "x y", r#"q"t"#]
        );
        assert!(words("   ").is_empty());
        assert!(is_assignment("A_1=x") && !is_assignment("1A=x") && !is_assignment("--a=b"));
    }

    /// Простая команда: операторы — только вне кавычек; незакрытая кавычка — не простая.
    #[test]
    fn simple_command_operators_outside_quotes() {
        for s in [
            "grep -c x f",
            "grep 'a|b' f",
            "grep \"a;b&c\" f",
            r"grep a\|b f",
            "diff a b > out.txt",
            "test -f x",
            "",
        ] {
            assert!(is_simple(s), "{s:?}");
        }
        for s in [
            "grep x f | wc -l",
            "grep x f; echo",
            "grep x f && echo",
            "grep x f || true",
            "grep x f &",
            "cd a\ngrep x f",
            "grep 'x f",
        ] {
            assert!(!is_simple(s), "{s:?}");
        }
    }

    /// Слова путей: аргументы с разделителем (и значение `--флаг=путь`), каталоги без него — по
    /// `is_dir`; программа, флаги, адреса и `user@host` — нет.
    #[test]
    fn path_words_of_arguments() {
        let none = |_: &str| false;
        assert_eq!(
            path_words("ls web/ web/e2e …; ls web/playwright.config.*", none),
            ["web", "e2e", "playwright", "config"]
        );
        assert_eq!(
            path_words(
                r#"cargo build --manifest-path=crates/zam-cli/Cargo.toml -p zam-cli"#,
                none
            ),
            ["crates", "zam", "cli", "cargo", "toml"]
        );
        assert_eq!(
            path_words(r"type C:\work\Отчёт.txt", none),
            ["work", "отчёт", "txt"]
        );
        // адрес, учётные данные, флаг и программа-путь
        assert_eq!(
            path_words(
                "curl https://example.org/a/b ssh://u:p@h/x u@h:/srv -o out/file ./run.sh",
                none
            ),
            ["out", "file", "run"]
        );
        assert_eq!(
            path_words("A=b/c ./scripts/x.sh", none),
            ["scripts"],
            "присваивание — не аргумент"
        );
        // каталог без разделителя — по `is_dir`; программа и флаги не проверяются
        let dir = |a: &str| matches!(a, "web" | "ls" | "-la");
        assert_eq!(path_words("ls -la web src", dir), ["web"]);
        // предел слов
        let many: String = (0..40).map(|i| format!("d{i:03}/ ")).collect();
        assert_eq!(
            path_words(&format!("ls {many}"), none).len(),
            PATH_WORDS_MAX
        );
    }
}
