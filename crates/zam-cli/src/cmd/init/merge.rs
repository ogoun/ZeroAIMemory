//! Слияние файлов проекта для `zam init` (§14.1): блок между маркерами в CLAUDE.md/AGENTS.md,
//! хуки в `.claude/settings.json` / `.codex/hooks.json`, сервер в `.mcp.json`, строки
//! `.zeroaimemory/.gitignore` и `info/exclude`, строка адреса сервера (вырезать из блока,
//! записать в `CLAUDE.local.md` — M6.6 §10.1). Всё чистые функции над текстом: повторный запуск
//! даёт тот же результат (идемпотентность), чужое содержимое не меняется.
use super::ojson::J;
use crate::locate::{URL_KEY, md_setting};

const BEGIN: &str = "<!-- zeroaimemory:begin";
const END: &str = "<!-- zeroaimemory:end";
/// Метка порядка байтов UTF-8 в начале файла (Блокнот Windows и др.).
pub const BOM: &str = "\u{feff}";
/// Имя сервера ZeroAIMemory в `.mcp.json`.
pub const MCP_NAME: &str = "zeroaimemory";

/// Вставляет или заменяет блок (`block` — с маркерами и `\n` в конце). Текст вне блока
/// сохраняется байт в байт; у файла с CRLF блок пишется с CRLF. Нет файла — только блок; нет
/// блока — он дописывается в конец через пустую строку. Начало без конца — ошибка.
pub fn upsert_block(existing: Option<&str>, block: &str) -> Result<String, String> {
    // BOM снимается на время правки и возвращается (fix round 1, I-1)
    if let Some(rest) = existing.and_then(|t| t.strip_prefix(BOM)) {
        return Ok(format!("{BOM}{}", upsert_block(Some(rest), block)?));
    }
    let Some(text) = existing.filter(|t| !t.is_empty()) else {
        return Ok(block.to_string());
    };
    let crlf = text.contains("\r\n");
    let block = if crlf {
        block.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        block.to_string()
    };
    let Some((b, end)) = block_span(text)? else {
        let nl = if crlf { "\r\n" } else { "\n" };
        let mut out = text.to_string();
        if !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(nl);
        out.push_str(&block);
        return Ok(out);
    };
    Ok(format!("{}{block}{}", &text[..b], &text[end..]))
}

/// Байты блока в тексте: от начала строки маркера начала до конца строки маркера конца
/// (с переводом строки). Нет блока — `None`; начало без конца — ошибка.
fn block_span(text: &str) -> Result<Option<(usize, usize)>, String> {
    // начала строк
    let mut starts = vec![0usize];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    starts.retain(|&i| i < text.len());
    let line_at = |i: usize| {
        let end = text[i..].find('\n').map_or(text.len(), |n| i + n + 1);
        (&text[i..end], end)
    };
    let begin = starts
        .iter()
        .copied()
        .find(|&i| line_at(i).0.trim_start().starts_with(BEGIN));
    let Some(b) = begin else {
        return Ok(None);
    };
    let end = starts
        .iter()
        .copied()
        .filter(|&i| i > b)
        .find(|&i| line_at(i).0.trim_start().starts_with(END))
        .map(|i| line_at(i).1)
        .ok_or_else(|| "начало блока ZeroAIMemory без <!-- zeroaimemory:end -->".to_string())?;
    Ok(Some((b, end)))
}

/// Блок ZeroAIMemory в тексте файла (с маркерами); нет блока — `None`, начало без конца —
/// ошибка, как у [`upsert_block`].
pub fn find_block(text: &str) -> Result<Option<&str>, String> {
    Ok(block_span(text)?.map(|(b, e)| &text[b..e]))
}

/// Вырезает из блока строки адреса сервера (ключ `ZeroAIMemoryUrl` — по правилу
/// [`md_setting`], как их находит `zam`): блок без них (остальное байт в байт) и непустые
/// значения вырезанных строк (M6.6 §10.1).
pub fn cut_url_lines(block: &str) -> (String, Vec<String>) {
    let mut urls = Vec::new();
    let rest = block
        .split_inclusive('\n')
        .filter(|l| match md_setting(l) {
            Some((URL_KEY, v)) => {
                if !v.is_empty() {
                    urls.push(v.to_string());
                }
                false
            }
            _ => true,
        })
        .collect();
    (rest, urls)
}

/// Текст `CLAUDE.local.md` со строкой `ZeroAIMemoryUrl=<url>`: прежние строки адреса (все — у
/// `zam` побеждает последняя) заменяются ею, нет ни одной — она дописывается в конец. Прочий
/// текст, BOM и CRLF файла сохраняются (M6.6 §10.1).
pub fn set_url_line(existing: Option<&str>, url: &str) -> String {
    let text = existing.unwrap_or("");
    let (bom, body) = match text.strip_prefix(BOM) {
        Some(rest) => (BOM, rest),
        None => ("", text),
    };
    let line = format!("{URL_KEY}={url}");
    let mut found = false;
    let mut out: String = body
        .split_inclusive('\n')
        .map(|l| match md_setting(l) {
            Some((URL_KEY, _)) => {
                found = true;
                let eol = &l[l.trim_end_matches(['\r', '\n']).len()..];
                format!("{line}{eol}")
            }
            _ => l.to_string(),
        })
        .collect();
    if !found {
        let nl = if body.contains("\r\n") { "\r\n" } else { "\n" };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&line);
        out.push_str(nl);
    }
    format!("{bom}{out}")
}

/// Обработчик хука — наш: exec form `command == "zam"` и `args[0] == "hook"` (Claude Code)
/// или строка `zam hook …` (Codex; `zam.exe` и путь к `zam` — тоже).
pub fn is_ours(handler: &J) -> bool {
    let Some(cmd) = handler.get("command").and_then(J::as_str) else {
        return false;
    };
    let is_zam = |s: &str| {
        let base = s.rsplit(['/', '\\']).next().unwrap_or(s);
        base == "zam" || base.eq_ignore_ascii_case("zam.exe")
    };
    if is_zam(cmd.trim()) {
        return matches!(handler.get("args"), Some(J::Arr(a)) if a.first().and_then(J::as_str) == Some("hook"));
    }
    let mut words = cmd.split_whitespace();
    matches!((words.next(), words.next()), (Some(w), Some("hook")) if is_zam(w.trim_matches('"')))
}

/// Сливает хуки `ours` (`{"hooks": {событие: [группы]}}`) в настройки агента `root`: наши
/// прежние обработчики удаляются (группа без оставшихся обработчиков — тоже, событие без
/// групп — тоже), новые группы дописываются в конец массива события. Чужие ключи, события,
/// группы и обработчики остаются на своих местах.
pub fn merge_hooks(root: &mut J, ours: &J) -> Result<(), String> {
    if !matches!(root, J::Obj(_)) {
        return Err("верхний уровень — не объект JSON".into());
    }
    let hooks = root.entry_obj("hooks").expect("объект");
    let J::Obj(events) = hooks else {
        return Err("поле hooks — не объект".into());
    };
    let mut touched: Vec<String> = Vec::new();
    for (event, groups) in events.iter_mut() {
        let J::Arr(groups) = groups else { continue };
        let before = groups.len();
        let mut removed = false;
        groups.retain_mut(|g| {
            let Some(J::Arr(hs)) = g.get_mut("hooks") else {
                return true;
            };
            let n = hs.len();
            hs.retain(|h| !is_ours(h));
            if hs.len() == n {
                return true;
            }
            removed = true;
            !hs.is_empty()
        });
        if removed || groups.len() != before {
            touched.push(event.clone());
        }
    }
    let J::Obj(new) = ours.get("hooks").ok_or("во фрагменте хуков нет hooks")?
    else {
        return Err("фрагмент хуков: hooks — не объект".into());
    };
    for (event, add) in new {
        let J::Arr(add) = add else { continue };
        match events.iter_mut().find(|(k, _)| k == event) {
            Some((_, J::Arr(groups))) => groups.extend(add.iter().cloned()),
            Some(_) => return Err(format!("hooks.{event} — не массив")),
            None => events.push((event.clone(), J::Arr(add.clone()))),
        }
    }
    events.retain(|(k, v)| !(touched.contains(k) && matches!(v, J::Arr(a) if a.is_empty())));
    Ok(())
}

/// Ставит сервер ZeroAIMemory из фрагмента `.mcp.json` в `root` (замена на месте или
/// добавление в конец `mcpServers`); другие серверы и ключи не трогаются.
pub fn merge_mcp(root: &mut J, fragment: &J) -> Result<(), String> {
    if !matches!(root, J::Obj(_)) {
        return Err("верхний уровень — не объект JSON".into());
    }
    let server = fragment
        .get("mcpServers")
        .and_then(|s| s.get(MCP_NAME))
        .ok_or("во фрагменте .mcp.json нет mcpServers.zeroaimemory")?
        .clone();
    let servers = root.entry_obj("mcpServers").expect("объект");
    if !matches!(servers, J::Obj(_)) {
        return Err("поле mcpServers — не объект".into());
    }
    servers.set(MCP_NAME, server);
    Ok(())
}

/// `.zeroaimemory/.gitignore`: дописывает недостающие `cache/` и `logs/`.
pub fn gitignore(existing: Option<&str>) -> String {
    append_lines(existing, &["cache/", "logs/"])
}

/// Дописывает в конец файла правил git (`.gitignore`, `info/exclude`) недостающие строки
/// `lines` (строка уже есть — без изменений).
pub fn append_lines(existing: Option<&str>, lines: &[&str]) -> String {
    let mut out = existing.unwrap_or("").to_string();
    for &line in lines {
        if !out
            .strip_prefix(BOM)
            .unwrap_or(&out)
            .lines()
            .any(|l| l.trim() == line)
        {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const B2: &str = "<!-- zeroaimemory:begin v2 -->\nновый\n<!-- zeroaimemory:end -->\n";

    #[test]
    fn block_replaced_in_place_text_kept() {
        let old = "# Проект\n\nтекст\n<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryToken=zam_x\n<!-- zeroaimemory:end -->\nхвост\n";
        let out = upsert_block(Some(old), B2).unwrap();
        assert_eq!(out, format!("# Проект\n\nтекст\n{B2}хвост\n"));
        assert_eq!(upsert_block(Some(&out), B2).unwrap(), out);
    }

    #[test]
    fn block_appended_or_created() {
        assert_eq!(upsert_block(None, B2).unwrap(), B2);
        assert_eq!(upsert_block(Some(""), B2).unwrap(), B2);
        assert_eq!(upsert_block(Some("a"), B2).unwrap(), format!("a\n\n{B2}"));
        let out = upsert_block(Some("a\n"), B2).unwrap();
        assert_eq!(out, format!("a\n\n{B2}"));
        assert_eq!(upsert_block(Some(&out), B2).unwrap(), out);
    }

    #[test]
    fn block_with_bom_first_line() {
        let old = "\u{feff}<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryToken=zam_x\n<!-- zeroaimemory:end -->\nхвост\n";
        let out = upsert_block(Some(old), B2).unwrap();
        assert_eq!(out, format!("\u{feff}{B2}хвост\n"));
        assert_eq!(upsert_block(Some(&out), B2).unwrap(), out);
        let out = upsert_block(Some("\u{feff}текст\n"), B2).unwrap();
        assert_eq!(out, format!("\u{feff}текст\n\n{B2}"));
        assert_eq!(
            upsert_block(Some("\u{feff}"), B2).unwrap(),
            format!("\u{feff}{B2}")
        );
        assert_eq!(
            gitignore(Some("\u{feff}cache/\n")),
            "\u{feff}cache/\nlogs/\n"
        );
    }

    #[test]
    fn block_crlf_and_eof_without_newline() {
        let old = "a\r\n<!-- zeroaimemory:begin v1 -->\r\nx\r\n<!-- zeroaimemory:end -->";
        let out = upsert_block(Some(old), B2).unwrap();
        assert_eq!(
            out,
            "a\r\n<!-- zeroaimemory:begin v2 -->\r\nновый\r\n<!-- zeroaimemory:end -->\r\n"
        );
        assert!(upsert_block(Some("<!-- zeroaimemory:begin v1 -->\nx\n"), B2).is_err());
    }

    fn ours() -> J {
        J::parse(r#"{"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "zam", "args": ["hook", "checkpoint"], "timeout": 2}]}],
            "PreToolUse": [{"matcher": "Write|Edit|Bash", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "pre-tool"], "timeout": 1}]}]
        }}"#)
        .unwrap()
    }

    #[test]
    fn hooks_merge_keeps_foreign_and_is_idempotent() {
        let mut root = J::parse(r#"{"permissions": {"allow": ["Bash(ls)"]},
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "lint"}]},
                                     {"matcher": "Edit", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "pre-tool"]},
                                                                   {"type": "command", "command": "fmt"}]}],
                      "Stop": [{"hooks": [{"type": "command", "command": "zam hook checkpoint"}]}]},
            "model": "x"}"#)
        .unwrap();
        merge_hooks(&mut root, &ours()).unwrap();
        let once = root.pretty();
        merge_hooks(&mut root, &ours()).unwrap();
        assert_eq!(root.pretty(), once);
        let v: serde_json::Value = serde_json::from_str(&once).unwrap();
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 3, "{once}");
        assert_eq!(pre[0]["hooks"][0]["command"], "lint");
        assert_eq!(
            pre[1]["hooks"].as_array().unwrap().len(),
            1,
            "чужой fmt остался"
        );
        assert_eq!(pre[1]["hooks"][0]["command"], "fmt");
        assert_eq!(pre[2]["hooks"][0]["args"][1], "pre-tool");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1, "{once}");
        // порядок ключей верхнего уровня сохранён
        let keys: Vec<&str> = once
            .lines()
            .filter(|l| l.starts_with("  \""))
            .map(|l| l.trim().split('"').nth(1).unwrap())
            .collect();
        assert_eq!(keys, ["permissions", "hooks", "model"]);
    }

    #[test]
    fn ours_detection() {
        for (s, want) in [
            (r#"{"command": "zam", "args": ["hook", "prompt"]}"#, true),
            (r#"{"command": "C:\\bin\\zam.exe", "args": ["hook"]}"#, true),
            (r#"{"command": "zam hook pre-tool --agent codex"}"#, true),
            (r#"{"command": "/usr/local/bin/zam hook prompt"}"#, true),
            (r#"{"command": "zam", "args": ["status"]}"#, false),
            (r#"{"command": "zamboni hook"}"#, false),
            (r#"{"command": "echo zam hook"}"#, false),
        ] {
            assert_eq!(is_ours(&J::parse(s).unwrap()), want, "{s}");
        }
    }

    #[test]
    fn mcp_merge_replaces_only_ours() {
        let frag =
            J::parse(r#"{"mcpServers": {"zeroaimemory": {"type": "http", "url": "u2"}}}"#).unwrap();
        let mut root = J::parse(
            r#"{"mcpServers": {"other": {"command": "x"}, "zeroaimemory": {"url": "u1"}}, "z": 1}"#,
        )
        .unwrap();
        merge_mcp(&mut root, &frag).unwrap();
        assert_eq!(
            serde_json::to_string(&root).unwrap(),
            r#"{"mcpServers":{"other":{"command":"x"},"zeroaimemory":{"type":"http","url":"u2"}},"z":1}"#
        );
        assert!(merge_mcp(&mut J::parse("[]").unwrap(), &frag).is_err());
    }

    #[test]
    fn gitignore_lines() {
        assert_eq!(gitignore(None), "cache/\nlogs/\n");
        assert_eq!(gitignore(Some("x")), "x\ncache/\nlogs/\n");
        assert_eq!(gitignore(Some("logs/\ncache/\n")), "logs/\ncache/\n");
        assert_eq!(
            append_lines(Some("# свои\n"), &["/CLAUDE.local.md"]),
            "# свои\n/CLAUDE.local.md\n"
        );
        let once = append_lines(None, &["/CLAUDE.local.md"]);
        assert_eq!(append_lines(Some(&once), &["/CLAUDE.local.md"]), once);
    }

    /// M6.6 §10.1: строки адреса вырезаются из блока (ключ — как у `locate`: `- `, обратные
    /// кавычки, пробелы у `=`), остальное — байт в байт; повтор ничего не меняет.
    #[test]
    fn url_lines_cut_from_block() {
        let b = "<!-- zeroaimemory:begin v8 -->\n## Память\nZeroAIMemoryUrl=http://192.0.2.1:7700\n\
                 - `ZeroAIMemoryUrl = http://192.0.2.2`\nZeroAIMemoryUrlX=y\nZeroAIMemoryUrl=\n\
                 Решение: x.\n<!-- zeroaimemory:end -->\n";
        let (cut, urls) = cut_url_lines(b);
        assert_eq!(
            cut,
            "<!-- zeroaimemory:begin v8 -->\n## Память\nZeroAIMemoryUrlX=y\nРешение: x.\n\
             <!-- zeroaimemory:end -->\n"
        );
        assert_eq!(urls, ["http://192.0.2.1:7700", "http://192.0.2.2"]);
        assert_eq!(cut_url_lines(&cut), (cut.clone(), Vec::new()));
        // блок в тексте файла
        let text = format!("# Проект\r\n{b}хвост");
        assert_eq!(find_block(&text), Ok(Some(b)));
        assert_eq!(find_block("# только текст\n"), Ok(None));
        assert!(find_block("<!-- zeroaimemory:begin v1 -->\nx\n").is_err());
    }

    /// Строка адреса в `CLAUDE.local.md`: заменяет прежние (все), иначе дописывается в конец;
    /// BOM и CRLF файла сохраняются.
    #[test]
    fn url_line_set_in_local_file() {
        let u = "http://192.0.2.1:7700";
        let line = format!("ZeroAIMemoryUrl={u}");
        assert_eq!(set_url_line(None, u), format!("{line}\n"));
        assert_eq!(set_url_line(Some(""), u), format!("{line}\n"));
        assert_eq!(set_url_line(Some("a"), u), format!("a\n{line}\n"));
        let mine = set_url_line(Some("# мои\n"), u);
        assert_eq!(mine, format!("# мои\n{line}\n"));
        assert_eq!(set_url_line(Some(&mine), u), mine);
        assert_eq!(
            set_url_line(
                Some(
                    "\u{feff}a\r\nZeroAIMemoryUrl=http://198.51.100.1\r\nb\r\n- `ZeroAIMemoryUrl=`"
                ),
                u
            ),
            format!("\u{feff}a\r\n{line}\r\nb\r\n{line}")
        );
        assert_eq!(set_url_line(Some("a\r\n"), u), format!("a\r\n{line}\r\n"));
    }
}
