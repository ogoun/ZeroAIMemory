//! CLI замера M6 (`cargo xtask eval --tokens --via mcp`): защита от «подкрутки» замера
//! (спецификация M6 §3.2, §3.5; Review Focus плана, п. 5) — xtask падает с понятной ошибкой до
//! запуска сервера.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn xtask(args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_xtask"));
    c.args(args);
    // окружение агента и сервера не наследуется
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("ZAM_") {
            c.env_remove(k);
        }
    }
    c.output().expect("запуск xtask")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Свежий каталог теста в `CARGO_TARGET_TMPDIR`.
fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("eval_cli")
        .join(name);
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Мини-набор ТС-1: две заметки, один сценарий с нагетами, без insight и документов.
fn mini_set(d: &Path) {
    let notes = d.join("corpus").join("ru").join("git");
    fs::create_dir_all(&notes).unwrap();
    fs::write(
        notes.join("revert.md"),
        "# Откат коммита\n\nКоротко о том, как откатить.\n\n## Подробно\n\n\
         Команда git revert создаёт новый коммит, обратный указанному, и историю не переписывает.\n",
    )
    .unwrap();
    fs::write(
        notes.join("reset.md"),
        "# Сброс\n\nКоротко о reset.\n\n## Подробно\n\n\
         Флаг --hard у reset выбрасывает незакоммиченные правки рабочего каталога безвозвратно.\n",
    )
    .unwrap();
    fs::create_dir_all(d.join("docs")).unwrap();
    fs::write(
        d.join("scenarios.toml"),
        r#"[[scenario]]
id = "t-01"
kind = "multi"
question = "как отменить последний коммит"
expect = ["ru/git/revert", "ru/git/reset"]
nuggets = [
  { source = "ru/git/revert", text = "создаёт новый коммит, обратный указанному, и историю не переписывает" },
  { source = "ru/git/reset", text = "выбрасывает незакоммиченные правки рабочего каталога безвозвратно" },
]
"#,
    )
    .unwrap();
}

fn set_args(d: &Path) -> Vec<String> {
    vec![
        "--scenarios".into(),
        s(&d.join("scenarios.toml")),
        "--corpus".into(),
        s(&d.join("corpus")),
        "--insights".into(),
        s(&d.join("no-insights.toml")),
        "--docs-dir".into(),
        s(&d.join("docs")),
    ]
}

fn run(base: &[&str], extra: &[String]) -> Output {
    let mut args: Vec<&str> = base.to_vec();
    args.extend(extra.iter().map(String::as_str));
    xtask(&args)
}

/// Значение `<name> = "<hex>"` из вывода `--check`.
fn printed(out: &str, name: &str) -> String {
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with(name))
        .unwrap_or_else(|| panic!("нет {name} в выводе:\n{out}"));
    line.split('"').nth(1).unwrap().to_string()
}

/// Сетка подбирается только на dev: `--grid` с `--split holdout` — ошибка до сборки сервера.
#[test]
fn grid_refuses_holdout() {
    let d = tmp("grid-holdout");
    let grid = d.join("grid.toml");
    fs::write(&grid, "[[point]]\nname = \"base\"\nenv = {}\n").unwrap();
    let o = xtask(&[
        "eval",
        "--tokens",
        "--via",
        "mcp",
        "--split",
        "holdout",
        "--grid",
        &s(&grid),
    ]);
    assert!(!o.status.success(), "{}", stdout(&o));
    let e = stderr(&o);
    assert!(e.contains("holdout") && e.contains("dev"), "{e}");
}

/// Нагеты или корпус изменены после заморозки (`nuggets.lock`) — `--check` и прогон падают.
#[test]
fn eval_rejects_fingerprint_mismatch() {
    let d = tmp("fp-mismatch");
    mini_set(&d);
    let set = set_args(&d);
    // без замка — проверка проходит и печатает оба отпечатка
    let o = run(&["eval", "--check"], &set);
    assert!(o.status.success(), "{}\n{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    let nf = printed(&out, "nuggets_fingerprint");
    let cf = printed(&out, "corpus_fingerprint");
    fs::write(
        d.join("nuggets.lock"),
        format!("nuggets_fingerprint = \"{nf}\"\ncorpus_fingerprint = \"{cf}\"\n"),
    )
    .unwrap();
    let o = run(&["eval", "--check"], &set);
    assert!(o.status.success(), "{}\n{}", stdout(&o), stderr(&o));
    // правка текста источника нагета меняет оба отпечатка
    let note = d.join("corpus/ru/git/reset.md");
    let t = fs::read_to_string(&note).unwrap();
    fs::write(&note, t.replace("Коротко о reset.", "Коротко о git reset.")).unwrap();
    let o = run(&["eval", "--check"], &set);
    assert!(!o.status.success(), "{}", stdout(&o));
    let e = stderr(&o);
    assert!(
        e.contains("nuggets_fingerprint") && e.contains("nuggets.lock"),
        "{e}"
    );
    // прогон падает до запуска сервера (сервера по этому пути нет)
    let mut run_set = set.clone();
    run_set.extend([
        "--server-bin".into(),
        s(&d.join("nope").join("zam-server.exe")),
    ]);
    let o = run(
        &["eval", "--tokens", "--via", "mcp", "--split", "dev"],
        &run_set,
    );
    assert!(!o.status.success());
    let e = stderr(&o);
    assert!(e.contains("nuggets.lock") && !e.contains("нет "), "{e}");
}

fn arm(path: &Path, corpus_fp: &str, tokens: [usize; 3]) {
    let sc: Vec<serde_json::Value> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            serde_json::json!({
                "id": format!("t-{i}"), "kind": "multi", "split": "dev",
                "nuggets_total": 2,
                "protocol": { "tokens": t, "found": 2, "complete": true, "steps": [] }
            })
        })
        .collect();
    let v = serde_json::json!({
        "info": { "corpus_fingerprint": corpus_fp, "nuggets_fingerprint": "n1", "split": "dev",
                  "server_version": "x" },
        "scenarios": sc,
    });
    fs::write(path, v.to_string()).unwrap();
}

/// Плечи сравниваются только на одном отпечатке корпуса.
#[test]
fn arms_must_share_corpus_fingerprint() {
    let d = tmp("arms-fp");
    let (a, b, c) = (d.join("m6.json"), d.join("base.json"), d.join("other.json"));
    arm(&a, "c1", [600, 700, 800]);
    arm(&b, "c1", [1000, 1000, 1000]);
    arm(&c, "c2", [1000, 1000, 1000]);
    let o = xtask(&[
        "eval",
        "--tokens",
        "--via",
        "mcp",
        "--arm",
        &s(&a),
        "--compare",
        &s(&b),
    ]);
    assert!(o.status.success(), "{}\n{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains("0.700"), "{}", stdout(&o));
    let o = xtask(&[
        "eval",
        "--tokens",
        "--via",
        "mcp",
        "--arm",
        &s(&a),
        "--compare",
        &s(&c),
    ]);
    assert!(!o.status.success(), "{}", stdout(&o));
    let e = stderr(&o);
    assert!(
        e.contains("corpus_fingerprint") && e.contains("c1") && e.contains("c2"),
        "{e}"
    );
}
