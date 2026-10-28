//! Запуск: создание администратора и его начального токена, регистрация решений M1–M2
//! в реестре (§11.6).
use serde_json::json;
use zam_core::mask_token;
use zam_store::{
    Access, RegistryReport, StoreError, SystemStore, UserDbs, register_user_solutions,
};

/// Регистрирует решения баз всех пользователей в реестре `system.db` — по порядку создания
/// пользователей (`list_users`: UUIDv7 id), последовательно; первый зарегистрировавший GUID —
/// владелец. Вызывается до индексации и обслуживания запросов (`app::finish`) и из
/// `zam-server migrate`. Ошибка базы пользователя или пространства не валит старт: WARN и
/// счётчик `failed`. Итог — одной строкой INFO.
pub async fn register_solutions(system: &SystemStore, users: &UserDbs) -> RegistryReport {
    let mut total = RegistryReport::default();
    match system.list_users().await {
        Ok(list) => {
            for u in list {
                let db = match users.get(u.id).await {
                    Ok(db) => db,
                    Err(e) => {
                        tracing::warn!(user = %u.id, error = %e, "база пользователя не открылась: его решения не зарегистрированы");
                        total.failed += 1;
                        continue;
                    }
                };
                match register_user_solutions(system, &db, u.id).await {
                    Ok(r) => total.absorb(r),
                    Err(e) => {
                        tracing::warn!(user = %u.id, error = %e, "решения пользователя не зарегистрированы");
                        total.failed += 1;
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "список пользователей не прочитан: решения не зарегистрированы");
            total.failed += 1;
        }
    }
    tracing::info!(
        registered = total.registered,
        already = total.already,
        renamed = total.renamed.len(),
        failed = total.failed,
        "реестр решений сверен с базами пользователей"
    );
    total
}

/// Строка предупреждения о неотозванных токенах, выпущенных человеческими в M3
/// (`tokens.human = 1`): с M4 флаг не читается, запись по ним — агентская (§11.4, решение
/// владельца 2026-10-26). Нет таких — `None`.
pub fn legacy_human_tokens_warning(prefixes: &[String]) -> Option<String> {
    (!prefixes.is_empty()).then(|| {
        format!(
            "токены {} были человеческими, теперь агентские",
            prefixes.join(", ")
        )
    })
}

/// Аудит при старте: одна строка WARN, если есть неотозванные человеческие токены M3
/// ([`legacy_human_tokens_warning`]). Ошибка чтения — тоже WARN, старт не прерывается.
pub async fn warn_legacy_human_tokens(system: &SystemStore) {
    match system.legacy_human_token_prefixes().await {
        Ok(p) => {
            if let Some(w) = legacy_human_tokens_warning(&p) {
                tracing::warn!("{w}");
            }
        }
        Err(e) => tracing::warn!(error = %e, "человеческие токены M3 не проверены"),
    }
}

/// Если администратора нет, создаёт `login` (is_admin) и токен `initial` (rw).
/// Если администратор есть, но у него нет ни одного токена, даже отозванного
/// (сбой между созданием и выпуском), выпускает новый `initial`. Отозванные
/// токены — действие администратора, новый токен в этом случае не выпускается.
/// Возвращает открытый текст токена только при выпуске; в аудит пишется маска.
pub async fn ensure_admin_user(
    system: &SystemStore,
    login: &str,
) -> Result<Option<String>, StoreError> {
    let (user, action) = match system.admin_user().await? {
        Some(u) => {
            let tokens = system.list_tokens(u.id).await?;
            if !tokens.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_human_tokens_one_line() {
        assert_eq!(legacy_human_tokens_warning(&[]), None);
        assert_eq!(
            legacy_human_tokens_warning(&["zam_ab12".into(), "zam_cd34".into()]).as_deref(),
            Some("токены zam_ab12, zam_cd34 были человеческими, теперь агентские")
        );
    }
}
