//! Просмотр карты кода (спецификация M5.6 §9 CLI): `status`, `map`, `sym`, `refs`, `impact`.
//! Вывод — Markdown по ответу сервера; `--json` — ответ сервера как есть (у `impact` — слияние
//! ответов пачек).
use std::path::Path;

use serde_json::{Value, json};
use zam_code::{Limits, lang_of, split_ref};

use super::config::CodeConfig;
use super::scan;
use super::state::{CodeState, SyncMark};
use crate::client::{CliError, EXIT_NOT_FOUND, read_json};
use crate::cmd::{Ctx, agent_time, emit, emit_json, refs};

/// Предел бюджета карты (как у сервера `code.map_tokens`).
pub const BUDGET_MAX: usize = 20_000;
/// Предел `focus.text`, символов.
pub const FOCUS_TEXT_MAX: usize = 2000;
/// Предел путей и имён фокуса (как у сервера).
pub const FOCUS_ITEMS_MAX: usize = 100;

/// Percent-кодирование сегмента пути URL (`путь#qual` в `/code/symbols/{…}`).
fn enc_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn warnings(v: &Value) {
    for w in v["warnings"].as_array().into_iter().flatten() {
        if let Some(w) = w.as_str() {
            eprintln!("предупреждение сервера: {w}");
        }
    }
}

/// `zam code status`.
pub fn status(ctx: &Ctx, guid: &str, root: &Path) -> Result<(), CliError> {
    let v = read_json(ctx.client.get("/code/status", &[("solution", guid)])?)?;
    if ctx.json {
        return emit_json(&v);
    }
    warnings(&v);
    if v["exists"] != true {
        return emit("карты кода нет: zam code sync\n".as_bytes());
    }
    let mut s = format!(
        "карта кода: файлов {}, символов {}; снимок {}",
        v["files"],
        v["symbols"],
        agent_time(v["updated_at"].as_str().unwrap_or("?"))
    );
    let snap: Vec<String> = ["commit", "branch", "machine"]
        .iter()
        .filter_map(|k| v[*k].as_str())
        .map(short_commit)
        .collect();
    if !snap.is_empty() {
        s.push_str(&format!(" ({})", snap.join(", ")));
    }
    s.push_str(&format!("; ранги: {}\n", text(&v["rank_state"])));
    let langs: Vec<String> = v["by_lang"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            format!(
                "{} {} ({} симв.)",
                text(&l["lang"]),
                l["files"],
                l["symbols"]
            )
        })
        .collect();
    if !langs.is_empty() {
        s.push_str(&format!("языки: {}\n", langs.join(", ")));
    }
    let total = v["skipped_total"].as_u64().unwrap_or(0);
    if total > 0 {
        s.push_str(&format!("пропущено {total}:\n"));
        for k in v["skipped"].as_array().into_iter().flatten() {
            s.push_str(&format!(
                "  {} — {}\n",
                text(&k["path"]),
                text(&k["reason"])
            ));
        }
    }
    let central: Vec<String> = v["central"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| format!("{} ({}:{})", text(&c["qual"]), text(&c["path"]), c["line"]))
        .collect();
    if !central.is_empty() {
        s.push_str(&format!("центральные: {}\n", central.join(", ")));
    }
    if let Some(m) = SyncMark::load(root).filter(|m| m.solution == guid) {
        s.push_str(&format!(
            "синхронизация с этой машины: {}\n",
            agent_time(&m.last_ok_at)
        ));
    }
    emit(s.as_bytes())
}

/// Коммит (40 или 64 hex) — первые 12 символов; прочее (ветка, машина) — как есть.
fn short_commit(x: &str) -> String {
    if matches!(x.len(), 40 | 64) && x.bytes().all(|b| b.is_ascii_hexdigit()) {
        x[..12].to_string()
    } else {
        x.to_string()
    }
}

/// Аргумент `--focus`: путь (есть `/` или расширение кода) или идентификатор.
fn is_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || lang_of(s).is_some()
}

/// Путь фокуса относительно корня (как пути ссылок), иначе как введён.
fn focus_path(raw: &str, cwd: &Path, root: &Path) -> String {
    refs::ref_path(raw, cwd, root).unwrap_or_else(|_| raw.replace('\\', "/"))
}

/// Параметры `zam code map`.
pub struct MapOpts<'a> {
    pub focus: &'a [String],
    pub task: Option<&'a str>,
    pub tokens: Option<usize>,
}

/// JSON фокуса `{paths?, idents?, text?}`; пустой — `None`.
pub fn focus_json(o: &MapOpts, cwd: &Path, root: &Path) -> Result<Option<Value>, CliError> {
    let (mut paths, mut idents) = (Vec::new(), Vec::new());
    for f in o.focus.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        if is_path(f) {
            paths.push(focus_path(f, cwd, root));
        } else {
            idents.push(f.to_string());
        }
    }
    if paths.len() > FOCUS_ITEMS_MAX || idents.len() > FOCUS_ITEMS_MAX {
        return Err(CliError::input(format!(
            "--focus: не больше {FOCUS_ITEMS_MAX} путей и {FOCUS_ITEMS_MAX} имён"
        )));
    }
    let task = o.task.map(str::trim).filter(|t| !t.is_empty());
    if task.is_some_and(|t| t.chars().count() > FOCUS_TEXT_MAX) {
        return Err(CliError::input(format!(
            "--task: не длиннее {FOCUS_TEXT_MAX} символов"
        )));
    }
    if paths.is_empty() && idents.is_empty() && task.is_none() {
        return Ok(None);
    }
    let mut f = serde_json::Map::new();
    if !paths.is_empty() {
        f.insert("paths".into(), json!(paths));
    }
    if !idents.is_empty() {
        f.insert("idents".into(), json!(idents));
    }
    if let Some(t) = task {
        f.insert("text".into(), json!(t));
    }
    Ok(Some(Value::Object(f)))
}

/// `zam code map`.
pub fn map(ctx: &Ctx, guid: &str, root: &Path, o: &MapOpts) -> Result<(), CliError> {
    if let Some(t) = o.tokens
        && !(1..=BUDGET_MAX).contains(&t)
    {
        return Err(CliError::input(format!("--tokens: от 1 до {BUDGET_MAX}")));
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let focus = focus_json(o, &cwd, root)?.map(|v| v.to_string());
    let tokens = o.tokens.map(|t| t.to_string());
    let mut q: Vec<(&str, &str)> = vec![
        ("solution", guid),
        ("format", if ctx.json { "json" } else { "md" }),
    ];
    if let Some(t) = &tokens {
        q.push(("budget_tokens", t));
    }
    if let Some(f) = &focus {
        q.push(("focus", f));
    }
    let v = read_json(ctx.client.get("/code/map", &q)?)?;
    if ctx.json {
        return emit_json(&v);
    }
    warnings(&v);
    let mut t = text(&v["text"]).to_string();
    if !t.ends_with('\n') {
        t.push('\n');
    }
    emit(t.as_bytes())
}

/// Карточка символа по имени, `qual` или `путь#qual`; не найден — код 5.
fn card(ctx: &Ctx, guid: &str, q: &str) -> Result<Value, CliError> {
    let q = q.trim();
    if q.is_empty() {
        return Err(CliError::input("символ: имя, qual или путь#qual"));
    }
    let id = match split_ref(q) {
        (p, Some(qual)) => format!("{}#{qual}", p.replace('\\', "/")),
        _ => {
            let hits = read_json(ctx.client.get(
                "/code/symbols",
                &[("solution", guid), ("q", q), ("limit", "20")],
            )?)?;
            let list = hits["symbols"].as_array().cloned().unwrap_or_default();
            // точное совпадение qual, затем имени; иначе первый (сервер — по рангу)
            let pick = list
                .iter()
                .find(|s| s["qual"] == q)
                .or_else(|| list.iter().find(|s| s["name"] == q))
                .or_else(|| list.first())
                .ok_or_else(|| {
                    CliError::new(EXIT_NOT_FOUND, format!("символ «{q}» не найден в карте"))
                })?;
            pick["id"].to_string()
        }
    };
    read_json(ctx.client.get(
        &format!("/code/symbols/{}", enc_segment(&id)),
        &[("solution", guid)],
    )?)
}

fn knowledge_md(s: &mut String, v: &Value) {
    let k = v["knowledge"].as_array().cloned().unwrap_or_default();
    if k.is_empty() {
        return;
    }
    s.push_str("\nзнания:\n");
    for e in &k {
        let sym = e["symbol"]
            .as_str()
            .map(|x| format!(" `{x}`"))
            .unwrap_or_default();
        s.push_str(&format!(
            "  {} {}{sym}: {} [{}]\n",
            text(&e["id"]),
            text(&e["path"]),
            text(&e["title"]),
            text(&e["state"])
        ));
    }
}

/// `zam code sym`.
pub fn sym(ctx: &Ctx, guid: &str, q: &str) -> Result<(), CliError> {
    let v = card(ctx, guid, q)?;
    if ctx.json {
        return emit_json(&v);
    }
    let s0 = &v["symbol"];
    let mut s = format!(
        "{} {} — {}:{}-{}\n",
        text(&s0["kind"]),
        text(&s0["qual"]),
        text(&s0["path"]),
        s0["line"],
        s0["end_line"]
    );
    if let Some(sig) = s0["sig"].as_str() {
        s.push_str(&format!("  {sig}\n"));
    }
    s.push_str(&format!("ссылка: {}\n", text(&s0["ref"])));
    let callers = v["callers"].as_array().cloned().unwrap_or_default();
    if !callers.is_empty() {
        let c: Vec<String> = callers
            .iter()
            .map(|c| format!("{} ({})", text(&c["path"]), c["count"]))
            .collect();
        s.push_str(&format!("кто ссылается: {}\n", c.join(", ")));
    }
    let uses = v["uses"].as_array().cloned().unwrap_or_default();
    if !uses.is_empty() {
        let u: Vec<String> = uses
            .iter()
            .take(20)
            .map(|u| format!("{} {}", text(&u["kind"]), text(&u["name"])))
            .collect();
        s.push_str(&format!("файл ссылается на: {}\n", u.join(", ")));
    }
    knowledge_md(&mut s, &v);
    emit(s.as_bytes())
}

/// `zam code refs` — кто ссылается на символ.
pub fn refs_of(ctx: &Ctx, guid: &str, q: &str) -> Result<(), CliError> {
    let v = card(ctx, guid, q)?;
    if ctx.json {
        return emit_json(&v);
    }
    let s0 = &v["symbol"];
    let callers = v["callers"].as_array().cloned().unwrap_or_default();
    let mut s = format!(
        "{} ({}:{}): ",
        text(&s0["qual"]),
        text(&s0["path"]),
        s0["line"]
    );
    if callers.is_empty() {
        s.push_str("ссылок из других файлов нет\n");
    } else {
        s.push_str(&format!("файлов со ссылками {}\n", callers.len()));
        for c in &callers {
            s.push_str(&format!("  {} — {}\n", text(&c["path"]), c["count"]));
        }
    }
    emit(s.as_bytes())
}

/// Набор файлов `impact`: `--since` или отличия отпечатков от `code-state.json`.
fn impact_paths(
    root: &Path,
    cfg: &CodeConfig,
    since: Option<&str>,
) -> Result<Vec<String>, CliError> {
    if let Some(c) = since {
        return refs::git_changed_since(root, c);
    }
    let state = CodeState::load(root);
    let fps = scan::fingerprints(root, cfg)?;
    let mut out: Vec<String> = Vec::new();
    for fp in &fps {
        match state.files.get(&fp.path) {
            Some(s) if s.fp == fp.fp && s.size == fp.size => {}
            Some(s) => {
                // отпечаток другой, содержимое может совпасть (touch, checkout)
                if refs::hash_file(&root.join(&fp.path)).ok().as_deref() != Some(&s.hash) {
                    out.push(fp.path.clone());
                }
            }
            None => out.push(fp.path.clone()),
        }
    }
    let live: std::collections::HashSet<&str> = fps.iter().map(|f| f.path.as_str()).collect();
    for p in state.files.keys() {
        if !live.contains(p.as_str()) {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// Файл влияния: `{path, symbols?}`; удалённый — `symbols: []`, разбор не удался — без символов.
fn impact_file(root: &Path, rel: &str, cfg: &CodeConfig) -> Value {
    let Some(lang) = lang_of(rel) else {
        return json!({"path": rel});
    };
    let abs = root.join(rel);
    if !abs.exists() {
        return json!({"path": rel, "symbols": []});
    }
    let Ok(bytes) = std::fs::read(&abs) else {
        return json!({"path": rel});
    };
    let lim = Limits {
        max_file_bytes: cfg.max_file_bytes,
        parse_timeout: std::time::Duration::from_millis(cfg.parse_timeout_ms),
        ..Limits::default()
    };
    match zam_code::symbol_hashes(lang, &bytes, &lim) {
        Some(s) => json!({"path": rel, "symbols": s}),
        None => json!({"path": rel}),
    }
}

/// `zam code impact`.
pub fn impact(
    ctx: &Ctx,
    guid: &str,
    root: &Path,
    cfg: &CodeConfig,
    since: Option<&str>,
) -> Result<(), CliError> {
    let paths = impact_paths(root, cfg, since)?;
    let mut merged = json!({"changed": [], "removed": [], "consumers": [], "paths_only": [],
                            "knowledge": [], "warnings": [], "tokens": 0});
    let files: Vec<Value> = paths.iter().map(|p| impact_file(root, p, cfg)).collect();
    for chunk in files.chunks(zam_code::UPLOAD_FILES_MAX) {
        let v = read_json(
            ctx.client
                .post_json("/code/impact", &json!({"solution": guid, "files": chunk}))?,
        )?;
        for k in [
            "changed",
            "removed",
            "consumers",
            "paths_only",
            "knowledge",
            "warnings",
        ] {
            if let (Some(dst), Some(src)) = (merged[k].as_array_mut(), v[k].as_array()) {
                dst.extend(src.iter().cloned());
            }
        }
        merged["tokens"] =
            json!(merged["tokens"].as_u64().unwrap_or(0) + v["tokens"].as_u64().unwrap_or(0));
    }
    if ctx.json {
        return emit_json(&merged);
    }
    warnings(&merged);
    let list = |k: &str| merged[k].as_array().cloned().unwrap_or_default();
    let mut s = format!("изменённых файлов: {}\n", paths.len());
    for (k, title) in [("changed", "изменены"), ("removed", "удалены")] {
        let l = list(k);
        if !l.is_empty() {
            s.push_str(&format!("{title}:\n"));
            for x in &l {
                s.push_str(&format!(
                    "  {} {} ({}:{})\n",
                    text(&x["kind"]),
                    text(&x["qual"]),
                    text(&x["path"]),
                    x["line"]
                ));
            }
        }
    }
    let c = list("consumers");
    if !c.is_empty() {
        s.push_str("потребители:\n");
        for x in &c {
            let names: Vec<&str> = x["names"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            s.push_str(&format!("  {} — {}\n", text(&x["path"]), names.join(", ")));
        }
    }
    knowledge_md(&mut s, &merged);
    emit(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_shortened_only_if_hex() {
        assert_eq!(short_commit(&"a".repeat(40)), "aaaaaaaaaaaa");
        assert_eq!(
            short_commit("ветка-номер-сорок-символов-длиной-ровно-x"),
            "ветка-номер-сорок-символов-длиной-ровно-x"
        );
        let mach = "машина".repeat(4);
        assert_eq!(short_commit(&mach), mach);
    }

    #[test]
    fn segment_encoding() {
        assert_eq!(
            enc_segment("src/a.rs#Store::open"),
            "src%2Fa.rs%23Store::open"
        );
        assert_eq!(enc_segment("ж"), "%D0%B6");
    }

    #[test]
    fn focus_split_paths_and_idents() {
        let d = Path::new("/nonexistent-root");
        let focus = [
            "src/a.rs".to_string(),
            "Store".to_string(),
            "x.py".to_string(),
        ];
        let o = MapOpts {
            focus: &focus,
            task: Some(" задача "),
            tokens: None,
        };
        let v = focus_json(&o, d, d).unwrap().unwrap();
        assert_eq!(
            v,
            json!({"paths": ["src/a.rs", "x.py"], "idents": ["Store"], "text": "задача"})
        );
        let none = MapOpts {
            focus: &[],
            task: None,
            tokens: None,
        };
        assert_eq!(focus_json(&none, d, d).unwrap(), None);
    }
}
