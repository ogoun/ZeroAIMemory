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
//! «Выжимка»"]` — псевдоним в прозе, не в коде и не в идентификаторе, аббревиатура — в своём
//! регистре (M6.6 §6). Ответ записи и правки `term` несёт [`broad_alias_warnings`]:
//! `["term_alias_broad: «ok» — меньше 3 букв или цифр"]`. Запись не отклоняется.
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{
    ALIAS_USES_MAX, BroadAlias, Db, GlossaryEntry, active_terms, broad_alias, find_alias_uses,
    find_term, get_entry, glossary_version,
};

use super::insights::entry_json;
use super::query;
use super::scope::{Mode, space_scope};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Предупреждения `term_alias: «<псевдоним>» → «<имя>»` для текста (`title` + `body`) записи
/// в пространстве `space` базы `db` (до 5). Словарь — из кэша `st.glossary`; пустой — без
/// разбора текста.
pub async fn alias_warnings(
    st: &AppState,
    db: &Db,
    space: Id,
    title: &str,
    body: &str,
) -> Result<Vec<String>, ApiError> {
    // словарь — из кэша процесса (по версии `meta.glossary_version`), как у канала `alias`
    let index = st.glossary.index(db).await?;
    let spaces = [space];
    let terms: Vec<&GlossaryEntry> = index.terms_in(&spaces).collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let text = format!("{title}\n{body}");
    Ok(find_alias_uses(&text, terms)
        .into_iter()
        .map(|(alias, title)| format!("term_alias: «{alias}» → «{title}»"))
        .collect())
}

/// Предупреждения `term_alias_broad: «<псевдоним>» — <причина>` о слишком общих псевдонимах
/// `data.aliases` термина (M6.6 §6.5, [`zam_store::broad_alias`]): такой псевдоним находится в
/// посторонних текстах. До [`ALIAS_USES_MAX`], в порядке псевдонимов; запись не отклоняется.
pub fn broad_alias_warnings(data: &Value) -> Vec<String> {
    data.get("aliases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|a| {
            let why = match broad_alias(a)? {
                BroadAlias::Short => "меньше 3 букв или цифр",
                BroadAlias::StopWord => "служебное слово",
            };
            Some(format!("term_alias_broad: «{a}» — {why}"))
        })
        .take(ALIAS_USES_MAX)
        .collect()
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
        return Ok(Json(entry_json(&p, &sc.db, &e, sc.role).await?));
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
    // агенту по токену — с маской секретов (задача 5 M7.1)
    let mut items = Value::Array(items);
    super::agent_view::view_json(&p, &mut items, &["title", "aliases", "definition"]);
    Ok(Json(json!({
        "version": glossary_version(&sc.db).await?,
        "items": items,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_alias_warnings_capped() {
        assert!(broad_alias_warnings(&json!({"definition": "d"})).is_empty());
        let many: Vec<String> = (0..8).map(|i| format!("a{i}")).collect();
        let w = broad_alias_warnings(&json!({"aliases": many}));
        assert_eq!(w.len(), ALIAS_USES_MAX);
        assert_eq!(w[0], "term_alias_broad: «a0» — меньше 3 букв или цифр");
    }

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
