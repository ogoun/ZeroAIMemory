//! Словарь домена (спецификация M4.5 §2): чтение терминов и предупреждения об использовании
//! псевдонимов.
//!
//! - `GET /spaces/{space}/glossary` → `{version, items: [{id, title, aliases, definition}]}` —
//!   действующие термины пространства по имени; `version` — версия словаря
//!   (`meta.glossary_version`, растёт при любой записи `term`);
//! - `GET /spaces/{space}/glossary?term=<имя или псевдоним>` → карточка (запись `Entry`:
//!   `data {aliases, definition, not_to_confuse, code}`, `body`) или 404.
//!
//! Запись терминов — обычный `POST /spaces/{space}/entries` с `kind: "term"`
//! ([`super::insights::create`]). Предупреждения [`alias_warnings`] идут в ответе записи
//! заметки и записи журнала (кроме `term`): `warnings: ["term_alias: «summary» →
//! «Выжимка»"]`; запись не отклоняется.
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{Db, active_terms, find_alias_uses, find_term, get_entry, glossary_version};

use super::insights::entry_json;
use super::query;
use super::scope::{Mode, space_scope};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Предупреждения `term_alias: «<псевдоним>» → «<имя>»` для текста (`title` + `body`) записи
/// в пространстве `space` базы `db` (до 5). Пустой словарь — без запроса текста.
pub async fn alias_warnings(
    db: &Db,
    space: Id,
    title: &str,
    body: &str,
) -> Result<Vec<String>, ApiError> {
    let terms = active_terms(db, &[space]).await?;
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let text = format!("{title}\n{body}");
    Ok(find_alias_uses(&text, &terms)
        .into_iter()
        .map(|(alias, title)| format!("term_alias: «{alias}» → «{title}»"))
        .collect())
}

/// Дописывает предупреждения `w` в `warnings` ответа `v` (поле появляется, только если есть
/// что показать).
pub fn attach_warnings(v: &mut Value, w: Vec<String>) {
    if w.is_empty() {
        return;
    }
    let Value::Object(o) = v else {
        return;
    };
    match o.get_mut("warnings").and_then(Value::as_array_mut) {
        Some(a) => a.extend(w.into_iter().map(Value::from)),
        None => {
            o.insert("warnings".into(), json!(w));
        }
    }
}

#[derive(Deserialize)]
pub struct GlossaryQuery {
    term: Option<String>,
}

/// `GET /spaces/{space}/glossary[?term=…]`.
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(space): Path<String>,
    q: Result<Query<GlossaryQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let sc = space_scope(&st, &p, &headers, &space, Mode::Read).await?;
    let terms = active_terms(&sc.db, &[sc.space.id]).await?;
    if let Some(name) = q.term.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let t = find_term(&terms, name).ok_or(ApiError::NotFound)?;
        let e = get_entry(&sc.db, t.id).await?;
        return Ok(Json(entry_json(&sc.db, &e).await?));
    }
    let mut items = Vec::with_capacity(terms.len());
    for t in &terms {
        items.push(json!({
            "id": t.id,
            "title": t.title,
            "aliases": t.aliases,
            "definition": t.definition,
        }));
    }
    Ok(Json(json!({
        "version": glossary_version(&sc.db).await?,
        "items": items,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_attached_or_merged() {
        let mut v = json!({"id": 1});
        attach_warnings(&mut v, vec![]);
        assert!(v.get("warnings").is_none());
        attach_warnings(&mut v, vec!["a".into()]);
        attach_warnings(&mut v, vec!["b".into()]);
        assert_eq!(v["warnings"], json!(["a", "b"]));
    }
}
