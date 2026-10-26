//! `zam vault` (§7.6, §13.2): секреты пользователя.
//!
//! - `zam vault list [--q Q] [--global] [--solution GUID]` — метаданные без значений;
//! - `zam vault get <имя> [--global] [--raw]` — в stdout только значение (`--raw` — без
//!   перевода строки в конце); раскрытие пишется в аудит сервера;
//! - `zam vault set <имя> --type T [--username U] [--link URL] [--description D] [--global]` —
//!   значение **только** из stdin (один завершающий перевод строки отбрасывается) или
//!   скрытого ввода с терминала, никогда из аргументов. Есть запись с этим именем — новая
//!   ревизия (`If-Match`), открытые поля, не указанные флагами, сохраняются. Адрес ресурса —
//!   `--link` (`data.url`): `--url` у всех команд — адрес сервера **[решение исполнителя]**.
//!
//! Область: `--global` — личный Vault (`knowledge`), иначе Vault решения из `solution.toml`
//! (или `--solution`). Vault решения лежит в базе пользователя токена и другим участникам не
//! виден. Успешный `set` печатает `<id>@<rev>`.
use std::io::{IsTerminal, Read};

use serde_json::{Map, Value, json};

use super::{Ctx, emit, emit_json, solution_guid};
use crate::client::{CliError, EXIT_NOT_FOUND, read_json};
use crate::target::Loc;

/// Где искать запись: Knowledge или решение.
pub struct Area<'a> {
    pub global: bool,
    pub solution: Option<&'a str>,
}

/// Поля `zam vault set`.
pub struct Set<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub username: Option<&'a str>,
    pub url: Option<&'a str>,
    pub description: Option<&'a str>,
}

/// `{space}` Vault: `knowledge` или GUID решения.
fn space(ctx: &Ctx, a: &Area) -> Result<String, CliError> {
    if a.global {
        Ok("knowledge".into())
    } else {
        solution_guid(ctx, a.solution)
    }
}

/// Записи области (метаданные); `q` — подстрока открытых полей.
fn items(ctx: &Ctx, space: &str, q: Option<&str>) -> Result<Vec<Value>, CliError> {
    let query: Vec<(&str, &str)> = q.map(|q| vec![("q", q)]).unwrap_or_default();
    let v = read_json(ctx.client.get(&format!("/spaces/{space}/vault"), &query)?)?;
    Ok(v["items"].as_array().cloned().unwrap_or_default())
}

/// Запись с именем ровно `name`.
fn find(ctx: &Ctx, space: &str, name: &str) -> Result<Option<Value>, CliError> {
    let name = name.trim();
    Ok(items(ctx, space, Some(name))?
        .into_iter()
        .find(|i| i["title"].as_str() == Some(name)))
}

fn not_found(name: &str, space: &str) -> CliError {
    CliError::new(
        EXIT_NOT_FOUND,
        format!("в Vault ({space}) нет записи «{}»", name.trim()),
    )
}

/// Значение без одного завершающего перевода строки (`\n` или `\r\n`).
pub fn strip_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// Значение из stdin (не терминал) или скрытого ввода с терминала.
fn read_secret(name: &str) -> Result<String, CliError> {
    let raw = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Значение «{name}» (ввод скрыт): "))
            .map_err(|e| CliError::input(format!("скрытый ввод: {e}")))?
    } else {
        let mut b = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut b)
            .map_err(|e| CliError::input(format!("чтение stdin: {e}")))?;
        let s = String::from_utf8(b).map_err(|_| CliError::input("значение — не UTF-8"))?;
        strip_newline(s)
    };
    if raw.is_empty() {
        return Err(CliError::input(
            "пустое значение: передайте его через stdin или введите в терминале",
        ));
    }
    Ok(raw)
}

/// `data` записи: прежние открытые поля и флаги поверх.
pub fn merge_data(old: Option<&Value>, s: &Set) -> Value {
    let mut d: Map<String, Value> = old.and_then(Value::as_object).cloned().unwrap_or_default();
    d.insert("type".into(), json!(s.kind));
    for (k, v) in [
        ("username", s.username),
        ("url", s.url),
        ("description", s.description),
    ] {
        if let Some(v) = v {
            d.insert(k.into(), json!(v));
        }
    }
    Value::Object(d)
}

/// Строка списка: `<имя>  <тип>  <username|url>  <id>`.
fn line(i: &Value) -> String {
    let d = &i["data"];
    let hint = d["username"]
        .as_str()
        .or_else(|| d["url"].as_str())
        .unwrap_or("-");
    format!(
        "{}\t{}\t{}\t{}\n",
        i["title"].as_str().unwrap_or("?"),
        d["type"].as_str().unwrap_or("?"),
        hint,
        i["id"].as_str().unwrap_or("?")
    )
}

/// `zam vault list`.
pub fn list(ctx: &Ctx, a: &Area, q: Option<&str>) -> Result<(), CliError> {
    let space = space(ctx, a)?;
    let items = items(ctx, &space, q)?;
    if ctx.json {
        return emit_json(&json!({ "items": items }));
    }
    let out: String = items.iter().map(line).collect();
    emit(out.as_bytes())
}

/// `zam vault get`: в stdout только значение.
pub fn get(ctx: &Ctx, a: &Area, name: &str, raw: bool) -> Result<(), CliError> {
    let space = space(ctx, a)?;
    let item = find(ctx, &space, name)?.ok_or_else(|| not_found(name, &space))?;
    let id = item["id"].as_str().unwrap_or_default();
    let v = read_json(
        ctx.client
            .get(&format!("/vault/{id}"), &[("reveal", "1")])?,
    )?;
    if ctx.json {
        return emit_json(&v);
    }
    let secret = v["secret"]
        .as_str()
        .ok_or_else(|| CliError::new(crate::client::EXIT_UNREACHABLE, "в ответе нет secret"))?;
    if raw {
        emit(secret.as_bytes())
    } else {
        emit(format!("{secret}\n").as_bytes())
    }
}

/// `zam vault set`: создание или новая ревизия записи с именем `name`.
pub fn set(ctx: &Ctx, a: &Area, s: &Set) -> Result<(), CliError> {
    let space = space(ctx, a)?;
    if space != "knowledge" {
        // своё решение регистрируется (идемпотентно), иначе Vault решения — 404
        let own = ctx.solution.as_ref().is_some_and(|x| x.id == space);
        ctx.ensure_solution(&Loc::Path {
            space: space.clone(),
            path: String::new(),
            own_solution: own,
        })?;
    }
    let existing = find(ctx, &space, s.name)?;
    let secret = read_secret(s.name.trim())?;
    let r = match &existing {
        Some(item) => {
            let id = item["id"].as_str().unwrap_or_default();
            let body = json!({ "data": merge_data(Some(&item["data"]), s), "secret": secret });
            ctx.client.patch_json_if(
                &format!("/vault/{id}"),
                &body,
                item["rev"].as_i64().unwrap_or(0),
            )?
        }
        None => {
            let body = json!({ "title": s.name, "data": merge_data(None, s), "secret": secret });
            ctx.client
                .post_json(&format!("/spaces/{space}/vault"), &body)?
        }
    };
    let m = read_json(r)?;
    if ctx.json {
        return emit_json(&m);
    }
    emit(format!("{}@{}\n", m["id"].as_str().unwrap_or("?"), m["rev"]).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_stripped_once() {
        assert_eq!(strip_newline("abc\n".into()), "abc");
        assert_eq!(strip_newline("abc\r\n".into()), "abc");
        assert_eq!(strip_newline("abc\n\n".into()), "abc\n");
        assert_eq!(strip_newline("abc".into()), "abc");
    }

    #[test]
    fn data_merged_with_flags() {
        let s = Set {
            name: "n",
            kind: "token",
            username: None,
            url: Some("https://x"),
            description: None,
        };
        let old = json!({"type": "password", "username": "u", "url": "https://old"});
        assert_eq!(
            merge_data(Some(&old), &s),
            json!({"type": "token", "username": "u", "url": "https://x"})
        );
        assert_eq!(
            merge_data(None, &s),
            json!({"type": "token", "url": "https://x"})
        );
    }
}
