//! `zam hook pre-tool` (`PreToolUse`, matcher `Write|Edit|Bash`; ≤ 5 мс сопоставление, ≤ 50 мс
//! процесс; §10.3) — **только локально, без сети**:
//!
//! - `file_glob` — по `tool_input.file_path` (у NotebookEdit — `notebook_path`) относительно
//!   корня решения (`/` как разделитель; вне корня — полный путь); на Windows без учёта
//!   регистра; `command_regex` — по `tool_input.command`;
//! - триггер с `tools` — только для этих инструментов (без учёта регистра);
//! - запись показывается не больше раза за сессию (`shown-<сессия>.json`); туда же — факт
//!   правок Write/Edit/MultiEdit/NotebookEdit для `checkpoint` (R1). Факт пишется и при
//!   выключенном `pre_tool`, если включён `checkpoint` **[решение исполнителя]**.
//!
//! Подсказка: `ZeroAIMemory: для <маска>: <title> [<вид>, <certainty>] — i:<id>`, для команды —
//! без «для …». Показ `pre_tool` в `usage` сервера не пишется: сети здесь нет.
use std::path::Path;

use globset::GlobBuilder;
use regex::RegexBuilder;

use super::cache::{self, Trigger};
use super::input::Input;
use super::output::Output;
use super::{Env, Kind, Settings};

/// Предел размера скомпилированного regex — как у проверки триггеров на сервере.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Предел команды для сопоставления, байт.
const COMMAND_SCAN_MAX: usize = 64 * 1024;

/// Инструменты правки файлов (факт правок для `checkpoint`).
fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    let tool = inp.str("tool_name").unwrap_or_default();
    let edit = is_edit_tool(tool);
    let hints_on = s.on(Kind::PreTool);
    let track = edit && s.on(Kind::Checkpoint);
    if !hints_on && !track {
        return Output::Empty;
    }
    let session = inp.session_id.as_deref();
    let mut shown = session.map(|id| cache::read_shown(env, id));
    let mut dirty = false;
    if track && let Some(sh) = shown.as_mut().filter(|sh| !sh.edits) {
        sh.edits = true;
        dirty = true;
    }
    let mut lines = Vec::new();
    if hints_on {
        let server = env.server_url();
        if let Some(c) = cache::usable_triggers(env, server.as_deref()) {
            let file = inp
                .tool_input("file_path")
                .or_else(|| inp.tool_input("notebook_path"));
            let command = command_of(inp);
            for (t, lead) in matches(&c.triggers, tool, file, command.as_deref(), env) {
                if let Some(sh) = shown.as_mut() {
                    if sh.shown.contains(&t.entry_id) {
                        continue;
                    }
                    sh.shown.push(t.entry_id.clone());
                    dirty = true;
                } else if lines.iter().any(|l: &String| l.ends_with(&t.entry_id)) {
                    continue;
                }
                lines.push(t.hint(&lead));
            }
        }
    }
    if dirty && let (Some(id), Some(sh)) = (session, shown.as_ref()) {
        cache::write_shown(env, id, sh);
    }
    Output::context(&inp.event, &lines.join("\n"))
}

/// `tool_input.command`: строка или массив аргументов (через пробел).
fn command_of(inp: &Input) -> Option<String> {
    let v = inp.v.get("tool_input")?.get("command")?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = v.as_array()?.iter().filter_map(|x| x.as_str()).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Путь файла для масок: относительно корня с `/`, вне корня — полный с `/`.
pub fn glob_path(file: &str, cwd: &Path, root: &Path) -> String {
    let p = Path::new(file);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let (a, r) = (norm(&abs), norm(root));
    let r = r.trim_end_matches('/');
    let rel = if cfg!(windows) {
        strip_prefix_ci(&a, r)
    } else {
        a.strip_prefix(r)
    };
    match rel.filter(|rest| rest.starts_with('/')) {
        Some(rest) => rest.trim_start_matches('/').to_string(),
        None => a,
    }
}

/// `strip_prefix` без учёта регистра (пути Windows).
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let mut it = s.char_indices();
    for pc in prefix.chars() {
        let (_, c) = it.next()?;
        if !c.to_lowercase().eq(pc.to_lowercase()) {
            return None;
        }
    }
    Some(it.next().map_or("", |(i, _)| &s[i..]))
}

/// Совпавшие триггеры и начало строки подсказки (`для <маска>: ` у `file_glob`).
fn matches<'a>(
    triggers: &'a [Trigger],
    tool: &str,
    file: Option<&str>,
    command: Option<&str>,
    env: &Env,
) -> Vec<(&'a Trigger, String)> {
    let tool = (!tool.is_empty()).then_some(tool);
    let rel = file.map(|f| glob_path(f, &env.cwd, &env.root));
    let command = command.map(|c| super::head_bytes(c, COMMAND_SCAN_MAX));
    let mut out = Vec::new();
    for t in triggers.iter().filter(|t| t.for_tool(tool)) {
        match (t.kind.as_str(), &rel, command) {
            ("file_glob", Some(path), _) => {
                let g = GlobBuilder::new(&t.pattern)
                    .case_insensitive(cfg!(windows))
                    .build();
                match g {
                    Ok(g) if g.compile_matcher().is_match(path) => {
                        out.push((t, format!("для {}: ", t.pattern)));
                    }
                    Ok(_) => {}
                    Err(_) => env.log(&format!("триггер {}: неверная маска", t.id)),
                }
            }
            ("command_regex", _, Some(cmd)) => {
                let r = RegexBuilder::new(&t.pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .dfa_size_limit(REGEX_SIZE_LIMIT)
                    .build();
                match r {
                    Ok(r) if r.is_match(cmd) => out.push((t, String::new())),
                    Ok(_) => {}
                    Err(_) => env.log(&format!("триггер {}: неверный regex", t.id)),
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_paths_relative_to_root() {
        let root = if cfg!(windows) {
            Path::new("C:\\proj")
        } else {
            Path::new("/proj")
        };
        let f = if cfg!(windows) {
            "c:\\Proj\\scripts\\deploy.sh"
        } else {
            "/proj/scripts/deploy.sh"
        };
        assert_eq!(glob_path(f, root, root), "scripts/deploy.sh");
        assert_eq!(glob_path("a/b.sh", &root.join("x"), root), "x/a/b.sh");
        let outside = if cfg!(windows) {
            "D:\\o\\a.sh"
        } else {
            "/o/a.sh"
        };
        assert!(glob_path(outside, root, root).ends_with("o/a.sh"));
        // префикс имени — не корень
        let sibling = if cfg!(windows) {
            "C:\\proj2\\a.sh"
        } else {
            "/proj2/a.sh"
        };
        assert!(glob_path(sibling, root, root).contains("proj2"));
    }
}
