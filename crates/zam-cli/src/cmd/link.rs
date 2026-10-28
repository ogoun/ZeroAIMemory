//! Граф связей (§8.5, §13): `zam link <src> <type> <dst>`, `zam link rm <edge-id>`,
//! `zam links <id>`.
//!
//! Цель — `n:<id>` (узел), `e:<id>` / `i:|c:|r:|t:|k:|q:<id>` (запись журнала, как в
//! карточках поиска), `[space:]path` заметки; у `dst` ещё `[[Имя]]` (заметка в пространстве
//! источника). Тип — из §8.5 (`supersedes` ставит только замещение insight).
//!
//! - `link` печатает id созданного ребра (`--json` — ребро);
//! - `link rm` — ничего (код 0), выведенные из текста и системные связи не удаляются (код 6);
//! - `links` — строки `<→|←><TAB>type<TAB>ref<TAB>путь или заголовок<TAB>origin<TAB>edge-id`:
//!   `→` — исходящие (у неразрешённой ссылки вместо ref — `-`, вместо пути — ключ ссылки),
//!   `←` — обратные ссылки (`--json` — `{links, backlinks}`).
use serde_json::{Value, json};

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, read_json};

/// Ссылка на цель для API: `n:<id>` | `e:<id>`; путь заметки разрешается через сервер.
/// `wiki` — разрешить `[[Имя]]` (только у `dst`).
pub fn target_ref(ctx: &Ctx, raw: &str, wiki: bool) -> Result<String, CliError> {
    let t = raw.trim();
    if t.starts_with("[[") && t.ends_with("]]") {
        return if wiki {
            Ok(t.to_string())
        } else {
            Err(CliError::input(
                "источник связи — n:<id>, e:<id> или путь заметки",
            ))
        };
    }
    if let Some(r) = short_ref(t) {
        return Ok(r);
    }
    let loc = ctx.locate(t)?;
    Ok(format!("n:{}", ctx.node_id(&loc)?))
}

/// `n:`/`node:` → `n:<id>`; `e:`/`entry:`/`i:|c:|r:|t:|k:|q:` → `e:<id>`; иначе `None`.
fn short_ref(t: &str) -> Option<String> {
    let (k, id) = t.split_once(':')?;
    let kind = match k {
        "n" | "node" => "n",
        "entry" => "e",
        k if crate::target::ENTRY_REF_LETTERS.contains(&k) => "e",
        _ => return None,
    };
    // id — 26 символов base32 (или UUID); проверку формата делает сервер
    (!id.is_empty() && !id.contains('/')).then(|| format!("{kind}:{id}"))
}

/// `zam link <src> <type> <dst>`.
pub fn create(ctx: &Ctx, src: &str, kind: &str, dst: &str) -> Result<(), CliError> {
    let src = target_ref(ctx, src, false)?;
    let dst = target_ref(ctx, dst, true)?;
    let e = read_json(
        ctx.client
            .post_json("/edges", &json!({"src": src, "type": kind, "dst": dst}))?,
    )?;
    if ctx.json {
        return emit_json(&e);
    }
    emit(format!("{}\n", e["id"].as_str().unwrap_or("?")).as_bytes())
}

/// `zam link rm <edge-id>`.
pub fn remove(ctx: &Ctx, id: &str) -> Result<(), CliError> {
    ctx.client.delete(&format!("/edges/{}", id.trim()))?;
    if ctx.json {
        return emit_json(&json!({ "deleted": id.trim() }));
    }
    Ok(())
}

/// `zam links <id>`: исходящие и обратные связи узла или записи.
pub fn list(ctx: &Ctx, raw: &str) -> Result<(), CliError> {
    let r = target_ref(ctx, raw, false)?;
    let (kind, id) = r.split_once(':').expect("ссылка n:/e:");
    let (out, back) = if kind == "n" {
        let out = read_json(ctx.client.get(&format!("/nodes/{id}/links"), &[])?)?;
        let back = read_json(ctx.client.get(&format!("/nodes/{id}/backlinks"), &[])?)?;
        (out, back)
    } else {
        // у записи нет /links: исходящие — рёбра подграфа на 1 шаг с источником-записью
        let g = read_json(ctx.client.get("/graph", &[("entry", id), ("depth", "1")])?)?;
        let items: Vec<Value> = g["edges"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|e| e["src"] == r.as_str())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let out = json!({ "items": items, "nodes": g["nodes"] });
        let back = read_json(ctx.client.get(&format!("/entries/{id}/backlinks"), &[])?)?;
        (out, back)
    };
    if ctx.json {
        return emit_json(&json!({ "links": out["items"], "backlinks": back["items"] }));
    }
    let mut s = render(&out, "→", "dst");
    s.push_str(&render(&back, "←", "src"));
    emit(s.as_bytes())
}

/// Путь заметки или заголовок записи по `ref` из `nodes` ответа.
fn label(nodes: &Value, r: &str) -> String {
    let n = nodes
        .as_array()
        .and_then(|a| a.iter().find(|n| n["ref"] == r));
    match n {
        Some(n) => n["path"]
            .as_str()
            .or(n["title"].as_str())
            .unwrap_or("-")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        None => "-".into(),
    }
}

/// Строки связей ответа `{items, nodes}`; `end` — какой конец показывать (`dst` | `src`).
fn render(v: &Value, dir: &str, end: &str) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .map(|e| {
            let (r, what) = match e[end].as_str() {
                Some(r) => (r.to_string(), label(&v["nodes"], r)),
                None => ("-".into(), e["dst_raw"].as_str().unwrap_or("-").to_string()),
            };
            format!(
                "{dir}\t{}\t{r}\t{what}\t{}\t{}\n",
                e["type"].as_str().unwrap_or("?"),
                e["origin"].as_str().unwrap_or("?"),
                e["id"].as_str().unwrap_or("?"),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_refs() {
        assert_eq!(short_ref("n:ABC").as_deref(), Some("n:ABC"));
        assert_eq!(short_ref("node:ABC").as_deref(), Some("n:ABC"));
        for p in ["i:X", "c:X", "r:X", "e:X", "entry:X"] {
            assert_eq!(short_ref(p).as_deref(), Some("e:X"), "{p}");
        }
        for p in ["knowledge:a/b", "n:", "n:a/b", "abc"] {
            assert!(short_ref(p).is_none(), "{p}");
        }
    }

    #[test]
    fn render_lines() {
        let v = json!({
            "items": [
                {"id": "E1", "type": "links_to", "origin": "wikilink", "dst": "n:B", "dst_raw": "[[b]]"},
                {"id": "E2", "type": "links_to", "origin": "wikilink", "dst": null, "dst_raw": "[[Нет]]"},
            ],
            "nodes": [{"ref": "n:B", "path": "notes/b", "title": "b"}],
        });
        assert_eq!(
            render(&v, "→", "dst"),
            "→\tlinks_to\tn:B\tnotes/b\twikilink\tE1\n→\tlinks_to\t-\t[[Нет]]\twikilink\tE2\n"
        );
    }
}
