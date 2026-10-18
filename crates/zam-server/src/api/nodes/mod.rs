//! Узлы пространств (§12): чтение ([`read`]), запись и корзина ([`write`]),
//! ревизии, дифф и откат ([`history`]).
//!
//! Все обработчики проверяют доступ по пространству узла (`scope`): узел Human
//! по токену не виден ни по id, ни по пути. В логи и аудит текст заметок не попадает;
//! `WriteError` выводится только через `Display`.
pub mod history;
pub mod read;
pub mod write;

use serde_json::{Map, Value, json};
use zam_core::Id;
use zam_markdown::PatchError;
use zam_store::{Db, WriteError, node_content, unified_diff};

use crate::app::AppState;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

/// Актор аудита: `admin` (сессия WebUI) или id токена.
fn actor(p: &Principal) -> String {
    match p {
        Principal::Token(t) => t.token_id.to_string(),
        Principal::Admin { .. } => "admin".into(),
    }
}

/// Запись аудита операции над узлом; `details` — без текста заметки.
async fn audit_node(
    st: &AppState,
    p: &Principal,
    client: &Client,
    user: Id,
    action: &str,
    node: Id,
    details: Value,
) -> Result<(), ApiError> {
    st.system
        .audit(
            &actor(p),
            Some(user),
            action,
            Some(&node.to_string()),
            Some(&client.ip.to_string()),
            details,
        )
        .await?;
    Ok(())
}

/// Ошибка записи заметки → problem. `RevMismatch` дополняется диффом
/// `base_rev → current_rev` (base не найдена — дифф от пустого текста).
async fn write_error(db: &Db, id: Id, base_rev: i64, e: WriteError) -> ApiError {
    match e {
        WriteError::Store(e) => e.into(),
        WriteError::RevMismatch {
            current_rev,
            current,
        } => {
            let base = match node_content(db, id, Some(base_rev)).await {
                Ok((_, t)) => t,
                Err(_) => String::new(),
            };
            let diff = unified_diff(
                &base,
                &current,
                &format!("rev {base_rev}"),
                &format!("rev {current_rev}"),
            );
            let mut extra = Map::new();
            extra.insert("current_rev".into(), json!(current_rev));
            extra.insert("diff".into(), json!(diff));
            ApiError::Conflict {
                detail: format!("ревизия устарела: текущая ревизия {current_rev}"),
                extra,
            }
        }
        WriteError::SectionChanged {
            section,
            current_rev,
        } => {
            let mut extra = Map::new();
            extra.insert("section".into(), json!(section));
            extra.insert("current_rev".into(), json!(current_rev));
            ApiError::Conflict {
                detail: format!(
                    "секция «{section}» изменена после базовой ревизии: текущая ревизия {current_rev}"
                ),
                extra,
            }
        }
        WriteError::Patch(p) => patch_error(p),
        e @ WriteError::TooLarge { .. } => ApiError::PayloadTooLarge(e.to_string()),
    }
}

fn patch_error(e: PatchError) -> ApiError {
    match e {
        PatchError::SectionNotFound(_) | PatchError::Ambiguous(_) | PatchError::Broken(_) => {
            ApiError::Unprocessable(e.to_string())
        }
    }
}
