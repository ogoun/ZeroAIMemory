//! `zam vault` (§7.6, §13.2): секреты пользователя.
//!
//! - `zam vault list [--q Q] [--global] [--solution GUID]` — метаданные без значений; при
//!   заблокированном Vault (§7.6) список печатается, а в stderr — строка «Vault заблокирован:
//!   <причина>» (код выхода 0; `get` и `set` получают 503);
//! - `zam vault get <имя> [--global] [--raw]` — в stdout только значение (`--raw` — без
//!   перевода строки в конце); раскрытие пишется в аудит сервера;
//! - `zam vault set <имя|--title Т> --type T [--username U] [--link URL] [--description D]
//!   [--purpose P] [--address A] [--limits L] [--notes N] [--secret-stdin] [--global]` (видимый
//!   синоним — `add`, M6.6 §7) — у `host`/`environment` (окружение, M4.5 §8) значение
//!   необязательно: читается из stdin только с `--secret-stdin`. Значение **только** из stdin
//!   (один завершающий перевод строки отбрасывается) или скрытого ввода с терминала, никогда из
//!   аргументов. Есть запись с этим именем — новая ревизия (`If-Match`), открытые поля, не
//!   указанные флагами, сохраняются. Адрес ресурса — `--link` (`data.url`): `--url` у всех команд
//!   — адрес сервера **[решение исполнителя]**. Новое окружение без `--global` и `--solution` —
//!   строка в stderr о Vault решения и `--global` ([`global_hint`]; при новой ревизии — нет);
//! - `zam vault rm <имя>` (синоним `delete`) — мягкое удаление (`DELETE /vault/{id}`): ревизии и
//!   аудит остаются, имя освобождается; печатает `<id> удалена`;
//! - `zam vault rename <имя> <новое>` — новое имя, новая ревизия с `If-Match` прочитанной;
//!   занятое имя или запись изменилась — 409, код 4;
//! - `zam vault mv <имя> --to global|solution|<GUID>` — перенос в другую область
//!   (`POST /vault/{id}/move`, M6.6 §7): новая ревизия, id и история те же; та же область — без
//!   ревизии (строка в stderr). Сервер без флага `vault_move` — код 1 с [`OLD_SERVER_MOVE`].
//!
//! Область (где искать запись): `--global` — личный Vault (`knowledge`), иначе Vault решения из
//! `solution.toml` (или `--solution`). Vault решения лежит в базе пользователя токена и другим
//! участникам не виден. Успешные `set`, `rename` и `mv` печатают `<id>@<rev>`.
use std::io::{IsTerminal, Read};

use serde_json::{Map, Value, json};

use super::{Ctx, emit, emit_json, old_server, solution_guid};
use crate::client::{CliError, EXIT_NOT_FOUND, read_json};
use crate::target::{Loc, is_guid};

/// Сообщение для сервера без переноса записей Vault (нет флага `vault_move` в `GET /health`
/// `features`, M6.6 §7).
pub const OLD_SERVER_MOVE: &str =
    "сервер не поддерживает перенос записей Vault: нужен zam-server 0.0.13-m6.6 или новее";

/// Флаг переноса записей Vault в `GET /health` `features` (`docs/api-compat.md`).
const FEATURE_MOVE: &str = "vault_move";

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
    pub purpose: Option<&'a str>,
    pub address: Option<&'a str>,
    pub limits: Option<&'a str>,
    pub notes: Option<&'a str>,
    /// Читать значение из stdin даже у типов, где оно необязательно.
    pub secret_stdin: bool,
}

impl Set<'_> {
    /// Читается ли значение: у типов окружения — только с `--secret-stdin`.
    fn wants_secret(&self) -> bool {
        self.secret_stdin || !matches!(self.kind, "host" | "environment")
    }
}

/// `{space}` Vault: `knowledge` или GUID решения.
fn space(ctx: &Ctx, a: &Area) -> Result<String, CliError> {
    if a.global {
        Ok("knowledge".into())
    } else {
        solution_guid(ctx, a.solution)
    }
}

/// `{space}` цели `zam vault mv --to`: `global` (или `knowledge`) — личный Vault, `solution` —
/// решение из `solution.toml`, GUID — решение по GUID; иначе код 6 до запроса.
fn target_space(ctx: &Ctx, to: &str) -> Result<String, CliError> {
    match to.trim() {
        "global" | "knowledge" => Ok("knowledge".into()),
        "solution" => solution_guid(ctx, None),
        g if is_guid(g) => Ok(g.to_ascii_lowercase()),
        _ => Err(CliError::input(format!(
            "--to «{to}»: global, solution или GUID решения"
        ))),
    }
}

/// Vault решения `space`: своё решение регистрируется (идемпотентно), иначе Vault решения —
/// 404; личный Vault — ничего.
fn ensure_space(ctx: &Ctx, space: &str) -> Result<(), CliError> {
    if space == "knowledge" {
        return Ok(());
    }
    let own = ctx.solution.as_ref().is_some_and(|x| x.id == space);
    ctx.ensure_solution(&Loc::Path {
        space: space.to_string(),
        path: String::new(),
        own_solution: own,
    })
}

/// Запись с именем `name` в области `a`: `(space, запись)`; нет — код 5.
fn located(ctx: &Ctx, a: &Area, name: &str) -> Result<(String, Value), CliError> {
    let space = space(ctx, a)?;
    let item = find(ctx, &space, name)?.ok_or_else(|| not_found(name, &space))?;
    Ok((space, item))
}

/// Метаданные записанной записи: `<id>@<rev>` или JSON ответа.
fn written(ctx: &Ctx, m: &Value) -> Result<(), CliError> {
    if ctx.json {
        return emit_json(m);
    }
    emit(format!("{}@{}\n", m["id"].as_str().unwrap_or("?"), m["rev"]).as_bytes())
}

/// Совет после создания записи `set --type host|environment` без области (M6.6 §7): запись
/// легла в Vault решения `solution`, общий для проектов хост — в личный Vault. Имя записи — в
/// кавычках (в нём бывают пробелы); имени решения нет — без него, имя записи вместо него не
/// подставляется (финальное ревью M6.6, Minor 9).
pub fn global_hint(solution: Option<&str>, name: &str) -> String {
    let vault = match solution {
        Some(s) => format!("Vault решения «{s}»"),
        None => "Vault решения".to_string(),
    };
    format!(
        "записано в {vault}; общий для проектов — --global (перенести: zam vault mv \"{name}\" --to global)"
    )
}

/// Ответ списка области: метаданные записей; `q` — подстрока открытых полей.
fn listing(ctx: &Ctx, space: &str, q: Option<&str>) -> Result<Value, CliError> {
    let query: Vec<(&str, &str)> = q.map(|q| vec![("q", q)]).unwrap_or_default();
    read_json(ctx.client.get(&format!("/spaces/{space}/vault"), &query)?)
}

/// Записи области (метаданные).
fn items(ctx: &Ctx, space: &str, q: Option<&str>) -> Result<Vec<Value>, CliError> {
    Ok(listing(ctx, space, q)?["items"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// Строка о блокировке из ответа списка; `None`, если Vault работает.
pub fn locked_line(listing: &Value) -> Option<String> {
    if listing["locked"].as_bool() != Some(true) {
        return None;
    }
    let reason = listing["reason"].as_str().unwrap_or("причина не указана");
    Some(format!("Vault заблокирован: {reason}"))
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
        ("purpose", s.purpose),
        ("address", s.address),
        ("limits", s.limits),
        ("notes", s.notes),
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
        .or_else(|| d["address"].as_str())
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
    let listing = listing(ctx, &space, q)?;
    if ctx.json {
        return emit_json(&listing);
    }
    let items = listing["items"].as_array().cloned().unwrap_or_default();
    let out: String = items.iter().map(line).collect();
    emit(out.as_bytes())?;
    // stdout остаётся списком для скриптов; предупреждение — в stderr
    if let Some(l) = locked_line(&listing) {
        eprintln!("{l}");
    }
    Ok(())
}

/// `zam vault get`: в stdout только значение.
pub fn get(ctx: &Ctx, a: &Area, name: &str, raw: bool) -> Result<(), CliError> {
    let (_, item) = located(ctx, a, name)?;
    let id = item["id"].as_str().unwrap_or_default();
    let v = read_json(
        ctx.client
            .get(&format!("/vault/{id}"), &[("reveal", "1")])?,
    )?;
    if ctx.json {
        return emit_json(&v);
    }
    // запись окружения без значения: `secret: null`
    if v["secret"].is_null() && v.get("secret").is_some() {
        eprintln!("у записи нет значения");
        return Ok(());
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
    ensure_space(ctx, &space)?;
    let existing = find(ctx, &space, s.name)?;
    let secret = s
        .wants_secret()
        .then(|| read_secret(s.name.trim()))
        .transpose()?;
    let r = match &existing {
        Some(item) => {
            let id = item["id"].as_str().unwrap_or_default();
            let mut body = json!({ "data": merge_data(Some(&item["data"]), s) });
            if let Some(x) = &secret {
                body["secret"] = json!(x);
            }
            ctx.client.patch_json_if(
                &format!("/vault/{id}"),
                &body,
                item["rev"].as_i64().unwrap_or(0),
            )?
        }
        None => {
            let mut body = json!({ "title": s.name, "data": merge_data(None, s) });
            if let Some(x) = &secret {
                body["secret"] = json!(x);
            }
            ctx.client
                .post_json(&format!("/spaces/{space}/vault"), &body)?
        }
    };
    let m = read_json(r)?;
    // новое окружение без явной области легло в Vault решения (умолчание не меняется, M6.6 §7);
    // новая ревизия существующей записи совета не печатает: запись уже лежит там, куда её
    // положили (признак — запись не нашлась до запроса и создана `POST`, финальное ревью M6.6,
    // Minor 9); имя решения — из `solution.toml`, пустого нет — совет без имени
    if existing.is_none()
        && !a.global
        && a.solution.is_none()
        && matches!(s.kind, "host" | "environment")
    {
        let solution = ctx
            .solution
            .as_ref()
            .map(|x| x.name.trim())
            .filter(|n| !n.is_empty());
        eprintln!("{}", global_hint(solution, s.name.trim()));
    }
    written(ctx, &m)
}

/// `zam vault rm`: мягкое удаление записи `name` (ревизии и аудит `vault.delete` остаются, имя
/// освобождается) — `<id> удалена`.
pub fn remove(ctx: &Ctx, a: &Area, name: &str) -> Result<(), CliError> {
    let (_, item) = located(ctx, a, name)?;
    let id = item["id"].as_str().unwrap_or_default();
    ctx.client.delete(&format!("/vault/{id}"))?;
    if ctx.json {
        return emit_json(&json!({ "deleted": id }));
    }
    emit(format!("{id} удалена\n").as_bytes())
}

/// `zam vault rename`: новое имя `new_name` — новая ревизия с `If-Match` прочитанной; имя занято
/// в той же области или запись изменилась между чтением и правкой — 409, код 4. Прежнее имя —
/// сервер ревизию не создаёт: `<id>@<rev>` и строка в stderr, как у `mv` в ту же область.
pub fn rename(ctx: &Ctx, a: &Area, name: &str, new_name: &str) -> Result<(), CliError> {
    let (_, item) = located(ctx, a, name)?;
    let id = item["id"].as_str().unwrap_or_default();
    let r = ctx.client.patch_json_if(
        &format!("/vault/{id}"),
        &json!({ "title": new_name.trim() }),
        item["rev"].as_i64().unwrap_or(0),
    )?;
    let m = read_json(r)?;
    if m["rev"] == item["rev"] {
        eprintln!("запись «{}» уже называется так: без изменений", name.trim());
    }
    written(ctx, &m)
}

/// `zam vault mv`: перенос записи `name` из области `a` в `to` (`POST /vault/{id}/move`, M6.6
/// §7) — `<id>@<rev>`; та же область — без новой ревизии (строка в stderr). Цель — своё решение —
/// регистрируется, как у `set`. Сервер без флага `vault_move` (404 или 405 на маршрут) — код 1 с
/// [`OLD_SERVER_MOVE`].
pub fn mv(ctx: &Ctx, a: &Area, name: &str, to: &str) -> Result<(), CliError> {
    let to = target_space(ctx, to)?;
    let (_, item) = located(ctx, a, name)?;
    ensure_space(ctx, &to)?;
    let id = item["id"].as_str().unwrap_or_default();
    let m = ctx
        .client
        .post_json(&format!("/vault/{id}/move"), &json!({ "space": to }))
        .and_then(read_json)
        .map_err(|e| old_server(ctx, e, &[404, 405], FEATURE_MOVE, OLD_SERVER_MOVE))?;
    if m["rev"] == item["rev"] {
        eprintln!("запись «{}» уже в Vault {to}: без изменений", name.trim());
    }
    written(ctx, &m)
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
    fn locked_line_from_listing() {
        assert_eq!(locked_line(&json!({"items": [], "locked": false})), None);
        assert_eq!(locked_line(&json!({"items": []})), None);
        assert_eq!(
            locked_line(&json!({"items": [], "locked": true, "reason": "нет мастер-ключа"}))
                .as_deref(),
            Some("Vault заблокирован: нет мастер-ключа")
        );
    }

    /// Текст совета §7 и флаг переноса — дословно; имя записи — в кавычках, без имени решения —
    /// совет без него, а не с именем записи (финальное ревью M6.6, Minor 9).
    #[test]
    fn move_texts() {
        assert_eq!(
            global_hint(Some("Проект"), "stage"),
            "записано в Vault решения «Проект»; общий для проектов — --global (перенести: zam vault mv \"stage\" --to global)"
        );
        assert_eq!(
            global_hint(None, "lab box"),
            "записано в Vault решения; общий для проектов — --global (перенести: zam vault mv \"lab box\" --to global)"
        );
        assert_eq!(
            OLD_SERVER_MOVE,
            "сервер не поддерживает перенос записей Vault: нужен zam-server 0.0.13-m6.6 или новее"
        );
        assert!(super::super::has_feature(
            &json!({"features": ["m6", FEATURE_MOVE]}),
            "vault_move"
        ));
    }

    #[test]
    fn data_merged_with_flags() {
        let s = Set {
            name: "n",
            kind: "token",
            username: None,
            url: Some("https://x"),
            description: None,
            purpose: Some("стенд"),
            address: None,
            limits: None,
            notes: None,
            secret_stdin: false,
        };
        let old = json!({"type": "password", "username": "u", "url": "https://old"});
        assert_eq!(
            merge_data(Some(&old), &s),
            json!({"type": "token", "username": "u", "url": "https://x", "purpose": "стенд"})
        );
        assert_eq!(
            merge_data(None, &s),
            json!({"type": "token", "url": "https://x", "purpose": "стенд"})
        );
        assert!(s.wants_secret());
        let h = Set { kind: "host", ..s };
        assert!(!h.wants_secret());
        assert!(
            Set {
                secret_stdin: true,
                ..h
            }
            .wants_secret()
        );
    }
}
