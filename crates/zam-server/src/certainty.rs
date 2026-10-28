//! Эпистемический статус `certainty` на стороне сервера (§7.5 ТЗ).
//!
//! Матрица прав «агент / человек / system» и её проверка живут в хранилище
//! ([`zam_store::certainty`]): каждая операция записи (создание и правка заметок и записей,
//! PATCH, откат, перенос, удаление, восстановление, замещение, отзыв, триггеры, вложения)
//! проверяет её внутри своей транзакции — по текущему состоянию цели, без окна гонки и без
//! риска забыть проверку в новом обработчике **[решение исполнителя]**. Сервер отвечает за:
//!
//! - **автора**: [`crate::attrib::attribution`] — сессия WebUI даёт `author_kind = human`,
//!   любой токен — `agent` (человеческие токены M3 упразднены — решение владельца 2026-10-26,
//!   §11.4; колонка `tokens.human` не читается);
//! - **ответ**: [`CertaintyError`] → RFC 9457 `422 /problems/certainty` с полем `code`
//!   (`evidence-required`, `reason-required`); 403 по правилам `certainty` с M4 нет: `axiom`
//!   агент ставит и меняет по прямому указанию пользователя — правило инструкции агента
//!   (§14.2), сервер его не проверяет.
pub use zam_store::certainty::{
    CertaintyChange, CertaintyError, Evidence, check_certainty, default_certainty,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use axum::response::IntoResponse;
    use zam_store::StoreError;

    #[tokio::test]
    async fn problem_carries_status_and_code() {
        for (e, code) in [
            (CertaintyError::EvidenceRequired, "evidence-required"),
            (CertaintyError::ReasonRequired, "reason-required"),
        ] {
            let resp = ApiError::from(StoreError::Certainty(e)).into_response();
            assert_eq!(resp.status().as_u16(), 422);
            let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["type"], "/problems/certainty");
            assert_eq!(v["title"], "Unprocessable Content");
            assert_eq!(v["code"], code);
        }
    }
}
