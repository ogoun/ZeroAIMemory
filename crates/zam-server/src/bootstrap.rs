//! Первый запуск: создание администратора и его начального токена.
use serde_json::json;
use zam_core::mask_token;
use zam_store::{Access, StoreError, SystemStore};

/// Если администратора нет, создаёт `login` (is_admin) и токен `initial` (rw).
/// Если администратор есть, но у него нет действующих токенов (сбой между
/// созданием и выпуском), выпускает новый `initial`.
/// Возвращает открытый текст токена только при выпуске; в аудит пишется маска.
pub async fn ensure_admin_user(
    system: &SystemStore,
    login: &str,
) -> Result<Option<String>, StoreError> {
    let (user, action) = match system.admin_user().await? {
        Some(u) => {
            let tokens = system.list_tokens(u.id).await?;
            if tokens.iter().any(|t| t.revoked_at.is_none()) {
                return Ok(None);
            }
            (u, "bootstrap_recover")
        }
        None => {
            if system.list_users().await?.iter().any(|u| u.name == login) {
                return Err(StoreError::Conflict(format!(
                    "пользователь '{login}' уже существует и не является администратором; \
                     задайте другой [admin].login"
                )));
            }
            (system.create_user(login, true).await?, "bootstrap")
        }
    };
    let (_, plain) = system.issue_token(user.id, "initial", Access::Rw).await?;
    system
        .audit(
            "system",
            Some(user.id),
            action,
            Some(&user.name),
            None,
            json!({ "token": mask_token(&plain) }),
        )
        .await?;
    Ok(Some(plain))
}
