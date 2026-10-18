//! Первый запуск: создание администратора и его начального токена.
use serde_json::json;
use zam_core::mask_token;
use zam_store::{Access, StoreError, SystemStore};

/// Если администратора нет, создаёт `login` (is_admin) и токен `initial` (rw).
/// Возвращает открытый текст токена только при создании; в аудит пишется маска.
pub async fn ensure_admin_user(
    system: &SystemStore,
    login: &str,
) -> Result<Option<String>, StoreError> {
    if system.admin_user().await?.is_some() {
        return Ok(None);
    }
    let user = system.create_user(login, true).await?;
    let (_, plain) = system.issue_token(user.id, "initial", Access::Rw).await?;
    system
        .audit(
            "system",
            Some(user.id),
            "bootstrap",
            Some(login),
            None,
            json!({ "token": mask_token(&plain) }),
        )
        .await?;
    Ok(Some(plain))
}
