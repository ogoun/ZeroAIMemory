//! Разбор команды оболочки без зависимостей (M7.1 §4.6): слова с учётом кавычек ([`words`],
//! перенесён из журнала сессии хуков `zam-cli`), простая ли команда ([`is_simple`]), простые
//! команды строки ([`segments`]) и имена каталогов её аргументов ([`path_words`]). Их читают хук
//! `zam hook post-tool`, сервер (предел [`PATH_WORDS_MAX`]) и прогон подсказок `xtask eval
//! --hints` — одно правило для клиента, сервера и замера.
//!
//! **Обратная косая.** [`words`] вне кавычек держит `\` буквально: `C:\Git\bin\grep.exe` без
//! кавычек остаётся путём Windows (так программу и называет агент). [`is_simple`] и [`segments`]
//! вне кавычек считают `\` экранированием следующего символа, как POSIX: `grep a\|b f` — не
//! конвейер; перед буквой пути Windows экранирование ничего не меняет. В двойных кавычках у всех
//! трёх `\` экранирует только `"`, `\`, `$` и `` ` `` (POSIX).

/// Слова команды с учётом кавычек `"…"` и `'…'`; `\` экранирует только в двойных кавычках (см.
/// модуль) — грубый разбор shell, достаточный для первого слова и аргументов.
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

/// Что встретил разбор строки команды ([`scan`]).
enum Mark {
    /// Разделитель простых команд: `|`, `||`, `;`, `&&`, фон `&`, перевод строки.
    Separator,
    /// Подстановка: `$(…)`, `` `…` `` (и в двойных кавычках), `<(…)`, `>(…)`.
    Substitution,
}

/// Проход строки команды с кавычками и экранированием (см. модуль): `on(смещение начала,
/// длина, что)` на каждый разделитель и подстановку вне одинарных кавычек; перенаправления
/// (`2>&1`, `>&2`, `<&3`, `&>`, `&>>`) разделителем не считаются. `false` — незакрытая кавычка.
fn scan(command: &str, mut on: impl FnMut(usize, usize, Mark)) -> bool {
    let mut quote: Option<char> = None;
    let mut prev: Option<char> = None;
    let mut it = command.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let next = it.peek().map(|&(_, n)| n);
        match (quote, c) {
            (Some('\''), '\'') => quote = None,
            (Some('\''), _) => {}
            (Some(_), '"') => quote = None,
            (Some(_), '\\') => {
                if matches!(next, Some('"' | '\\' | '$' | '`')) {
                    it.next();
                }
            }
            (Some(_), '`') => on(i, 1, Mark::Substitution),
            (Some(_), '$') if next == Some('(') => on(i, 2, Mark::Substitution),
            (Some(_), _) => {}
            (None, '\\') => {
                it.next();
            }
            (None, '"' | '\'') => quote = Some(c),
            (None, '`') => on(i, 1, Mark::Substitution),
            (None, '$' | '<' | '>') if next == Some('(') => on(i, 2, Mark::Substitution),
            (None, '&') if matches!(prev, Some('>' | '<')) || next == Some('>') => {}
            (None, '&' | '|') if next == Some(c) => {
                it.next();
                on(i, 2, Mark::Separator);
            }
            (None, '&' | '|' | ';' | '\n' | '\r') => on(i, 1, Mark::Separator),
            (None, _) => {}
        }
        prev = Some(c);
    }
    quote.is_none()
}

/// Простая команда (M7.1 §4.6 п. 1): вне кавычек нет `|`, `;`, `&&`, `||`, фона `&` и перевода
/// строки, и нигде нет подстановок `$(…)`, `` `…` `` (вне одинарных кавычек), `<(…)`, `>(…)`:
/// сбой вложенной программы прячется в коде внешней (`diff <(sort a) <(sort b)` без `a`).
/// Перенаправления `2>&1`, `&>`, `&>>` команду составной не делают. Незакрытая кавычка — не
/// простая.
pub fn is_simple(command: &str) -> bool {
    let mut simple = true;
    let closed = scan(command, |_, _, _| simple = false);
    simple && closed
}

/// Простые команды строки — части между разделителями `|`, `||`, `;`, `&&`, `&` и переводами
/// строк вне кавычек (подстановки разделителями не считаются), без пустых и краевых пробелов.
pub fn segments(command: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    scan(command, |i, len, m| {
        if let Mark::Separator = m {
            out.push(&command[from..i]);
            from = i + len;
        }
    });
    out.push(&command[from..]);
    out.into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Предел имён каталогов команды ([`path_words`]) — и у клиента, и у сервера (поле
/// `path_words` запроса `/recall`).
pub const PATH_WORDS_MAX: usize = 32;
/// Короче — не имя каталога (как слова ошибки в покрытии подсказок `post_tool`).
const PATH_WORD_MIN_CHARS: usize = 3;
/// Начало команды, из которого клиент берёт [`path_words`], байт (16 КиБ; финальное ревью M7.1,
/// I-4): у длинного `python - <<EOF …` (400 КБ) скан всей команды и stat на каждое слово выходили
/// за бюджет хука `post-tool`.
pub const PATH_WORDS_SCAN_MAX: usize = 16 * 1024;
/// Вызовов `is_dir` у [`path_words`] на команду не больше (stat — около 6 мкс на Windows); дальше
/// аргумент без разделителя не проверяется, у пути с разделителем последний сегмент — каталог,
/// только если кончается разделителем.
pub const PATH_WORDS_STAT_MAX: usize = 64;

/// Имена каталогов из аргументов команды (M7.1 §4.6 п. 3: каталоги, набранные в команде, — не
/// предмет её ошибки). Аргумент [`words`] с `/` или `\` (у `--флаг=путь` — значение после `=`) —
/// его каталоги: все сегменты, кроме последнего, а последний — если аргумент кончается
/// разделителем или `is_dir` (существующий каталог); имя файла и расширение — нет. Аргумент без
/// разделителя — если `is_dir` (программа и флаги `-…` не проверяются). Адреса (`://`) и
/// аргументы с `@` (`user:pass@host`) пропускаются. Слово — буквы и цифры от 3 символов в нижнем
/// регистре, без повторов, до [`PATH_WORDS_MAX`]. `is_dir` — не больше [`PATH_WORDS_STAT_MAX`]
/// раз; длину команды ограничивает вызывающий ([`PATH_WORDS_SCAN_MAX`]).
pub fn path_words(command: &str, is_dir: impl Fn(&str) -> bool) -> Vec<String> {
    let stats = std::cell::Cell::new(0usize);
    let is_dir = |a: &str| {
        if stats.get() >= PATH_WORDS_STAT_MAX {
            return false;
        }
        stats.set(stats.get() + 1);
        is_dir(a)
    };
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
        let dirs: &str = if value.contains(['/', '\\']) {
            if value.ends_with(['/', '\\']) || is_dir(value) {
                value
            } else {
                value.rsplit_once(['/', '\\']).map_or("", |(d, _)| d)
            }
        } else if first || value.starts_with('-') || value.is_empty() || !is_dir(value) {
            continue;
        } else {
            value
        };
        for part in dirs
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
        assert_eq!(
            words(r"C:\Git\bin\grep.exe x"),
            [r"C:\Git\bin\grep.exe", "x"]
        );
        assert!(words("   ").is_empty());
        assert!(is_assignment("A_1=x") && !is_assignment("1A=x") && !is_assignment("--a=b"));
    }

    /// Простая команда: операторы — только вне кавычек; подстановки (и в двойных кавычках) —
    /// нет; перенаправления — да; незакрытая кавычка — не простая.
    #[test]
    fn simple_command_operators_outside_quotes() {
        for s in [
            "grep -c x f",
            "grep 'a|b' f",
            "grep \"a;b&c\" f",
            r"grep a\|b f",
            "diff a b > out.txt",
            "test -f x",
            "grep x f 2>&1",
            "grep x f >log 2>&1",
            "diff a b &>diff.txt",
            "diff a b &>>diff.txt",
            "cmp a b 0<&3",
            "grep '$(x)' f",
            "grep '`x`' f",
            r#"grep "\$(x)" f"#,
            r"C:\Git\bin\grep.exe x f",
            "",
        ] {
            assert!(is_simple(s), "{s:?}");
        }
        for s in [
            "grep x f | wc -l",
            "grep x f |& cat",
            "grep x f; echo",
            "grep x f && echo",
            "grep x f || true",
            "grep x f &",
            "cd a\ngrep x f",
            "grep 'x f",
            "diff <(sort a) <(sort b)",
            "diff a >(tee log)",
            "grep -n foo $(find src -name '*.rs')",
            "grep -n foo `find src`",
            "test -n \"$(python find.py)\"",
            "test -n \"`cat x`\"",
        ] {
            assert!(!is_simple(s), "{s:?}");
        }
    }

    /// Простые команды строки: разделители вне кавычек; перенаправления и подстановки — нет.
    #[test]
    fn segments_split_on_separators() {
        assert_eq!(
            segments("cd crates && cargo test 2>&1 | tail -n 5; echo 'a;b' || true &"),
            [
                "cd crates",
                "cargo test 2>&1",
                "tail -n 5",
                "echo 'a;b'",
                "true"
            ]
        );
        // скобки подстановки не отслеживаются: `|` внутри `$(…)` — тоже разделитель (для поиска
        // программы этого хватает: такая команда и так не простая)
        assert_eq!(
            segments("grep x $(find . | head)"),
            ["grep x $(find .", "head)"]
        );
        assert_eq!(segments("  cargo test  "), ["cargo test"]);
        assert!(segments(" ; ").is_empty());
    }

    /// Имена каталогов: каталоги путей аргументов (последний сегмент — у пути с разделителем на
    /// конце или по `is_dir`), каталоги без разделителя — по `is_dir`; имя файла, программа,
    /// флаги, адреса и `user@host` — нет.
    #[test]
    fn path_words_of_arguments() {
        let none = |_: &str| false;
        assert_eq!(
            path_words("ls web/ web/e2e …; ls web/playwright.config.*", none),
            ["web"]
        );
        let e2e = |a: &str| a == "web/e2e";
        assert_eq!(
            path_words("ls web/ web/e2e …; ls web/playwright.config.*", e2e),
            ["web", "e2e"]
        );
        assert_eq!(
            path_words(
                r#"cargo build --manifest-path=crates/zam-cli/Cargo.toml -p zam-cli"#,
                none
            ),
            ["crates", "zam", "cli"]
        );
        assert_eq!(path_words(r"type C:\work\Отчёт.txt", none), ["work"]);
        // адрес, учётные данные, флаг и программа-путь
        assert_eq!(
            path_words(
                "curl https://example.org/a/b ssh://u:p@h/x u@h:/srv -o out/dir/f.txt ./run.sh",
                none
            ),
            ["out", "dir"]
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

    /// Цена (финальное ревью M7.1, I-4): у длинной команды `is_dir` — не больше
    /// [`PATH_WORDS_STAT_MAX`] раз; каталоги начала команды — как прежде, пути с разделителем на
    /// конце — и после исчерпания предела.
    #[test]
    fn path_words_stat_limit() {
        let calls = std::cell::Cell::new(0usize);
        let dir = |a: &str| {
            calls.set(calls.get() + 1);
            a == "src"
        };
        let mut cmd = String::from("ls src && python3 - <<'EOF'\n");
        while cmd.len() < 400 * 1024 {
            cmd.push_str("alpha beta gamma delta epsilon zeta eta theta\n");
        }
        cmd.push_str("print(1) out/dir/\nEOF\n");
        assert_eq!(path_words(&cmd, dir), ["src", "out", "dir"]);
        assert_eq!(calls.get(), PATH_WORDS_STAT_MAX);
        calls.set(0);
        assert_eq!(path_words("ls web/ web/e2e src", dir), ["web", "src"]);
        assert_eq!(calls.get(), 2, "обычная команда — stat на каждый аргумент");
    }
}
