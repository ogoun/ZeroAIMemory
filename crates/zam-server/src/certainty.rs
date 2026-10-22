//! Эпистемический статус `certainty` на стороне сервера (§7.5 ТЗ).
//!
//! Матрица прав «агент / человек» и её проверка живут в хранилище
//! ([`zam_store::certainty`]): каждая операция записи (создание и правка заметок и записей,
//! PATCH, откат, перенос, удаление, восстановление, замещение, отзыв, триггеры, вложения)
//! проверяет её внутри своей транзакции — по текущему состоянию цели, без окна гонки и без
//! риска забыть проверку в новом обработчике **[решение исполнителя]**. Сервер отвечает за:
//!
//! - **автора**: [`crate::attrib::attribution`] — сессия WebUI администратора и токен с флагом
//!   `human` дают `author_kind = human`, остальные токены — `agent`;
//! - **ответ**: [`CertaintyError`] → RFC 9457 `/problems/certainty` с полем `code`
//!   (`axiom-requires-human` — 403, `evidence-required`, `reason-required`,
//!   `invalid-transition` — 422);
//! - **человеческий токен**: выпускает только администратор (`POST /admin/users/{id}/tokens
//!   {human: true}`), аудит `token_issue` с `human: true`; признак — в `/auth/me` и
//!   `GET /spaces` (`human: bool`) **[решение исполнителя]**.
pub use zam_store::certainty::{
    CertaintyChange, CertaintyError, Evidence, check_certainty, default_certainty,
};

use axum::http::StatusCode;

/// HTTP-статус нарушения матрицы: `axiom` не от человека — 403, остальное — 422.
pub fn status_of(e: CertaintyError) -> StatusCode {
    match e {
        CertaintyError::AxiomRequiresHuman => StatusCode::FORBIDDEN,
        CertaintyError::EvidenceRequired
        | CertaintyError::ReasonRequired
        | CertaintyError::InvalidTransition => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use axum::response::IntoResponse;
    use zam_store::StoreError;

    #[tokio::test]
    async fn problem_carries_status_and_code() {
        for (e, status, code) in [
            (
                CertaintyError::AxiomRequiresHuman,
                403,
                "axiom-requires-human",
            ),
            (CertaintyError::EvidenceRequired, 422, "evidence-required"),
            (CertaintyError::ReasonRequired, 422, "reason-required"),
            (CertaintyError::InvalidTransition, 422, "invalid-transition"),
        ] {
            let resp = ApiError::from(StoreError::Certainty(e)).into_response();
            assert_eq!(resp.status().as_u16(), status);
            let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["type"], "/problems/certainty");
            assert_eq!(v["code"], code);
        }
    }
}
