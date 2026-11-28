//! Сторож переменных `ZAM_*` клиента (M7.4 §7.6 п. 7): каждая строка `"ZAM_…"` в исходниках
//! `zam` зарегистрирована — в [`zam_core::CLIENT_ENV_VARS`] (сервер не принимает её за ключ
//! конфигурации), в списке сервера `RESERVED_ENV` (адрес, секреты, сборочные) или в явном списке
//! этого теста с причиной. Иначе такая переменная в окружении пользователя — неизвестный ключ
//! конфигурации, и `zam-server`, запущенный из той же консоли, не стартует.

use std::path::{Path, PathBuf};

/// Литералы `ZAM_*` вне обоих списков — с причиной, почему серверу они не мешают.
const EXPLICIT: &[(&str, &str)] = &[
    (
        "ZAM_DEBUG_CODE_ABORT",
        "крючок debug-сборки `zam code sync` (обрыв процесса): задают только тесты",
    ),
    (
        "ZAM_TEST_",
        "тестовые точки `zam` (задержки и сбои хуков): задают только тесты, в окружении \
         службы и пользователя их нет",
    ),
];

/// Файлы `.rs` каталога `dir` рекурсивно.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Имена `ZAM_[A-Z0-9_]+` после каждой кавычки `"ZAM_` строки.
fn literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("\"ZAM_") {
        let tail = &rest[i + 1..];
        let end = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(tail.len());
        out.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    out
}

#[test]
fn cli_env_literals_registered() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 20, "{files:?}");
    let registered = |name: &str| {
        zam_core::CLIENT_ENV_VARS.contains(&name)
            || zam_server::config::RESERVED_ENV.contains(&name)
            || EXPLICIT
                .iter()
                .any(|(n, _)| name == *n || (n.ends_with('_') && name.starts_with(n)))
    };
    let mut seen = 0;
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for name in literals(&text) {
            seen += 1;
            if !registered(&name) {
                bad.push(format!("{}: {name}", f.display()));
            }
        }
    }
    // проверка не пустая: `zam` читает ZAM_URL, ZAM_TOKEN, ZAM_SESSION и тестовые точки
    assert!(seen >= 20, "литералов `ZAM_*` — {seen}");
    assert!(
        bad.is_empty(),
        "переменные `ZAM_*` клиента вне CLIENT_ENV_VARS, RESERVED_ENV и списка теста — \
         добавьте в `zam_core::CLIENT_ENV_VARS` (или в список теста с причиной):\n{}",
        bad.join("\n")
    );
    // разбор: имя кончается на первом знаке не из `[A-Z0-9_]`
    assert_eq!(
        literals(r#"x("ZAM_URL") y["ZAM_TEST_A_MS", "ZAM_"]"#),
        ["ZAM_URL", "ZAM_TEST_A_MS", "ZAM_"]
    );
    // ловит незарегистрированное имя
    assert!(!registered("ZAM_NEW_CLIENT_KNOB"));
}
