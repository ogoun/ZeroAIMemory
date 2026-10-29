//! `zam hook pre-tool` (`PreToolUse`, matcher `Write|Edit|Bash`; ≤ 5 мс сопоставление, ≤ 50 мс
//! процесс; §10.3) — **только локально, без сети**:
//!
//! - `file_glob` — по `tool_input.file_path` (у NotebookEdit — `notebook_path`) относительно
//!   корня (`/` как разделитель, `.` и `..` нормализуются; вне корня — не сопоставляется); на
//!   Windows без учёта регистра; `command_regex` — по `tool_input.command`;
//! - триггер с `tools` — только для этих инструментов (без учёта регистра);
//! - Codex правит файлы инструментом `apply_patch` (`tool_input.command` — текст патча): пути для
//!   `file_glob` — из заголовков `*** Add File:`, `*** Update File:`, `*** Delete File:` (и
//!   `*** Move to:`), `command_regex` к патчу не применяется (fix round 1, I-3; ⚠️ формат
//!   входа на живом Codex не проверен);
//! - знание о файле (M4.5 §6.3, ключ `file_context`) у правки — первыми строками, до
//!   триггеров `file_glob`: те же строки, что у `post-tool` на `Read`, раз за сессию на файл;
//! - запись показывается не больше раза за сессию (`shown-<сессия>.json`); правка
//!   Write/Edit/MultiEdit/NotebookEdit/`apply_patch` — маркер `edits-<сессия>` для `checkpoint`
//!   (R1). Маркер пишется и при выключенном `pre_tool`, если включён `checkpoint`
//!   **[решение исполнителя]**.
//!
//! Подсказка: `ZeroAIMemory: для <маска>: <title> [<вид>, <certainty>] — i:<id>`, для команды —
//! без «для …». Показ `pre_tool` в `usage` сервера не пишется: сети здесь нет.
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use regex::RegexBuilder;

use super::cache::{self, Trigger};
use super::input::Input;
use super::output::Output;
use super::{Env, Flag, Kind, Settings};

/// Предел размера скомпилированного regex — как у проверки триггеров на сервере.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Предел команды для сопоставления, байт.
const COMMAND_SCAN_MAX: usize = 64 * 1024;

/// Инструменты правки файлов (факт правок для `checkpoint`).
pub fn is_edit_tool(name: &str) -> bool {
    matches!(
        name,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch"
    )
}

/// Пути файлов из текста патча `apply_patch` (Codex): строки `*** Add File: <путь>`,
/// `*** Update File: <путь>`, `*** Delete File: <путь>`, `*** Move to: <путь>`.
pub fn patch_files(patch: &str) -> Vec<String> {
    const HEADS: [&str; 4] = [
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    let mut out: Vec<String> = Vec::new();
    for line in patch.lines() {
        // заголовок — только с начала строки; снимаются `\r` и хвостовые пробелы
        let l = line.trim_end();
        if let Some(p) = HEADS.iter().find_map(|h| l.strip_prefix(h)) {
            let p = p.trim();
            if !p.is_empty() && !out.iter().any(|x| x == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    let tool = inp.str("tool_name").unwrap_or_default();
    let hints_on = s.on(Kind::PreTool);
    let session = inp.session_id.as_deref();
    if is_edit_tool(tool)
        && s.on(Kind::Checkpoint)
        && let Some(id) = session
    {
        cache::mark_edits(env, id);
    }
    if !hints_on {
        return Output::Empty;
    }
    let command = command_of(inp);
    let (files, command) = if tool == "apply_patch" {
        (patch_files(command.as_deref().unwrap_or_default()), None)
    } else {
        let f = inp
            .tool_input("file_path")
            .or_else(|| inp.tool_input("notebook_path"));
        (f.map(str::to_string).into_iter().collect(), command)
    };
    let mut shown = session.map(|id| cache::read_shown(env, id));
    let mut dirty = false;
    let mut lines: Vec<String> = Vec::new();
    // знание о файле (M4.5 §6.3) — перед триггерами `file_glob`, в общем пределе текста
    if is_edit_tool(tool) && s.flag_on(Flag::FileContext) {
        let rels: Vec<String> = files
            .iter()
            .filter_map(|f| crate::cmd::refs::ref_path(f, &env.cwd, &env.root).ok())
            .collect();
        let before = shown.as_ref().map_or(0, |sh| sh.shown.len());
        lines.extend(super::post_tool::file_context_lines(
            env,
            shown.as_mut(),
            &rels,
        ));
        dirty |= shown.as_ref().map_or(0, |sh| sh.shown.len()) != before;
    }
    let server = env.server_url();
    if let Some(c) = cache::usable_triggers(env, server.as_deref()) {
        let found = matches(&c.triggers, tool, &files, command.as_deref(), env);
        let mut taken: Vec<&str> = Vec::new();
        for (t, lead) in found {
            if taken.contains(&t.entry_id.as_str()) {
                continue;
            }
            taken.push(&t.entry_id);
            if let Some(sh) = shown.as_mut() {
                if sh.shown.contains(&t.entry_id) {
                    continue;
                }
                sh.shown.push(t.entry_id.clone());
                dirty = true;
            }
            lines.push(t.hint(&lead));
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

/// Лексическая нормализация пути: `.` убирается, `..` сворачивает предыдущий компонент
/// (выше корня файловой системы — остаётся корень); файловая система не читается.
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Путь файла для масок (fix round 2): нормализованный (`.`, `..`) путь относительно корня с
/// `/`; путь вне корня (в том числе выше него через `..`) не сопоставляется — `None`.
pub fn glob_path(file: &str, cwd: &Path, root: &Path) -> Option<String> {
    let p = Path::new(file);
    let abs = normalize(&if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    });
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let (a, r) = (norm(&abs), norm(&normalize(root)));
    let r = r.trim_end_matches('/');
    let rel = if cfg!(windows) {
        strip_prefix_ci(&a, r)
    } else {
        a.strip_prefix(r)
    };
    rel.filter(|rest| rest.starts_with('/'))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .filter(|rest| !rest.is_empty())
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
    files: &[String],
    command: Option<&str>,
    env: &Env,
) -> Vec<(&'a Trigger, String)> {
    let tool = (!tool.is_empty()).then_some(tool);
    let rels: Vec<String> = files
        .iter()
        .filter_map(|f| glob_path(f, &env.cwd, &env.root))
        .collect();
    let command = command.map(|c| super::head_bytes(c, COMMAND_SCAN_MAX));
    let mut out = Vec::new();
    for t in triggers.iter().filter(|t| t.for_tool(tool)) {
        match (t.kind.as_str(), command) {
            ("file_glob", _) if !rels.is_empty() => {
                let g = GlobBuilder::new(&t.pattern)
                    .case_insensitive(cfg!(windows))
                    .build();
                match g {
                    Ok(g) => {
                        let m = g.compile_matcher();
                        if rels.iter().any(|p| m.is_match(p)) {
                            out.push((t, format!("для {}: ", t.pattern)));
                        }
                    }
                    Err(_) => env.log(&format!("триггер {}: неверная маска", t.id)),
                }
            }
            ("command_regex", Some(cmd)) => {
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
    fn patch_headers() {
        let p = "*** Begin Patch\n*** Add File: scripts/new.sh\r\n+echo\n*** Update File: src/a.rs  \n\
                 @@\n-x\n+y\n*** Move to: src/b.rs\n*** Delete File: old.txt\n\
                 *** Update File: src/a.rs\n  *** Add File: indented.sh\n+ *** Add File: added-line.sh\n\
                 *** End Patch\n";
        assert_eq!(
            patch_files(p),
            ["scripts/new.sh", "src/a.rs", "src/b.rs", "old.txt"]
        );
        assert!(patch_files("ls -la").is_empty());
    }

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
        let g = |f: &str, cwd: &Path| glob_path(f, cwd, root);
        assert_eq!(g(f, root).as_deref(), Some("scripts/deploy.sh"));
        assert_eq!(g("a/b.sh", &root.join("x")).as_deref(), Some("x/a/b.sh"));
        // нормализация: «.» и «..» внутри корня
        assert_eq!(g("./a/./b/../c.sh", root).as_deref(), Some("a/c.sh"));
        assert_eq!(g("../y/z.sh", &root.join("x")).as_deref(), Some("y/z.sh"));
        // выше корня и вне его — не сопоставляется
        assert_eq!(g("../a.sh", root), None);
        assert_eq!(g("x/../../a.sh", root), None);
        let outside = if cfg!(windows) {
            "D:\\o\\a.sh"
        } else {
            "/o/a.sh"
        };
        assert_eq!(g(outside, root), None);
        // префикс имени — не корень
        let sibling = if cfg!(windows) {
            "C:\\proj2\\a.sh"
        } else {
            "/proj2/a.sh"
        };
        assert_eq!(g(sibling, root), None);
        // ..-обход в абсолютном пути
        let tricky = if cfg!(windows) {
            "C:\\proj\\..\\etc\\a.sh"
        } else {
            "/proj/../etc/a.sh"
        };
        assert_eq!(g(tricky, root), None);
    }
}
