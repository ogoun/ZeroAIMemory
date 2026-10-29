//! Тест-сторож (fix round 1 задачи 8 M4.5, инцидент полного прогона): `zam`, запущенный с
//! окружением агента из каталога без разметки проекта (корень проекта — корень git, то есть
//! сам репозиторий ZeroAIMemory), не оставляет в корне репозитория `.zeroaimemory/`, CLAUDE.md,
//! AGENTS.md, `.mcp.json`, `.codex/`, `.claude/settings.json`. Проверяется только появление
//! нового: то, что было до теста (другие наборы идут параллельно), не учитывается.
mod common;

#[test]
fn cli_leaves_repo_root_clean() {
    let root = common::repo_root();
    let present = || -> Vec<&str> {
        common::REPO_LITTER
            .iter()
            .copied()
            .filter(|p| root.join(p).exists())
            .collect()
    };
    let before = present();
    if !before.is_empty() {
        eprintln!("в корне репозитория уже есть: {before:?}");
    }
    let (url, _) = common::stub(201, r#"{"id":"01X","rev":1}"#, std::time::Duration::ZERO);
    // без маркера проекта: корень проекта — корень git репозитория
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli-repo-clean");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sol = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e63";
    for args in [
        &[
            "log",
            "changelog",
            "--kind",
            "fix",
            "--title",
            "x",
            "--solution",
            sol,
        ][..],
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "y",
            "--solution",
            sol,
        ][..],
    ] {
        let _ = common::zam_cmd()
            .env("ZAM_URL", &url)
            .env("ZAM_TOKEN", "zam_x")
            .env("HOME", &dir)
            .env("USERPROFILE", &dir)
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_SESSION_ID", "repo-clean-session")
            .current_dir(&dir)
            .args(args)
            .write_stdin(b"text\n".to_vec())
            .output()
            .unwrap();
    }
    let new: Vec<&str> = present()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        new.is_empty(),
        "тесты оставили в корне репозитория: {new:?}"
    );
}
