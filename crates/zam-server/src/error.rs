//! Ошибки API в формате RFC 9457 (`application/problem+json`).
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use zam_core::SolutionGuid;
use zam_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("требуется аутентификация")]
    Unauthorized,
    #[error("доступ запрещён")]
    Forbidden,
    /// 403 с пояснением (например, запись участником с режимом `ro`).
    #[error("{0}")]
    Denied(String),
    /// Нет доступа к общему решению (§11.6): запрос ждёт, отклонён или отозван.
    #[error("нет доступа к решению {guid}: {}", access_state_text(state))]
    SolutionAccess {
        guid: SolutionGuid,
        /// `pending` | `rejected` | `revoked`.
        state: &'static str,
    },
    #[error("ресурс не найден")]
    NotFound,
    /// `extra` — расширения RFC 9457: поля попадают в тело problem рядом с `detail`.
    #[error("{detail}")]
    Conflict {
        detail: String,
        extra: Map<String, Value>,
    },
    /// 409 `/problems/possible-duplicate` (§7.5): похожий insight уже есть; `extra` —
    /// `candidates` и `dedup`.
    #[error("{detail}")]
    PossibleDuplicate {
        detail: String,
        extra: Map<String, Value>,
    },
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("требуется заголовок If-Match с ревизией")]
    PreconditionRequired,
    #[error("{0}")]
    Unprocessable(String),
    /// 422 `/problems/idempotency-mismatch`: ключ повторён с другим запросом.
    #[error("{0}")]
    IdempotencyMismatch(String),
    /// `/problems/certainty` (§7.5): нарушение матрицы прав `certainty` — 403 (`axiom` не от
    /// человека) или 422 (нет доказательства или причины, недопустимый переход); `code` —
    /// машинный код ([`zam_store::CertaintyError::code`]).
    #[error("{0}")]
    Certainty(zam_store::CertaintyError),
    #[error("слишком много запросов")]
    RateLimited { retry_after_s: u64 },
    #[error("{0}")]
    Unavailable(String),
    /// 503 `/problems/vault-locked` (§7.6): мастер-ключ не подходит или файла ключа нет при
    /// существующих ключах; в `detail` — причина (без путей и ключей).
    #[error("Vault заблокирован: {0}")]
    VaultLocked(String),
    /// Подробности только в лог; клиенту уходит общий текст.
    #[error("внутренняя ошибка")]
    Internal(String),
}

impl ApiError {
    /// 500 для прерванной задачи `spawn_blocking`: в журнал — только вид сбоя. Текст паники
    /// (`JoinError` через `Display`) не выводится: при панике на срезе строки в нём бывает
    /// до 256 символов незамаскированного исходника заметки.
    pub fn aborted(what: &str, e: &tokio::task::JoinError) -> ApiError {
        ApiError::Internal(format!(
            "{what} прерван (panic = {}, cancelled = {})",
            e.is_panic(),
            e.is_cancelled()
        ))
    }

    /// Конфликт без расширений.
    pub fn conflict(detail: impl Into<String>) -> ApiError {
        ApiError::Conflict {
            detail: detail.into(),
            extra: Map::new(),
        }
    }

    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "about:blank", "Bad Request"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "about:blank", "Unauthorized"),
            ApiError::Forbidden | ApiError::Denied(_) => {
                (StatusCode::FORBIDDEN, "about:blank", "Forbidden")
            }
            ApiError::SolutionAccess { .. } => (
                StatusCode::FORBIDDEN,
                "/problems/solution-access",
                "Forbidden",
            ),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "about:blank", "Not Found"),
            ApiError::Conflict { .. } => (StatusCode::CONFLICT, "/problems/conflict", "Conflict"),
            ApiError::PossibleDuplicate { .. } => (
                StatusCode::CONFLICT,
                "/problems/possible-duplicate",
                "Conflict",
            ),
            ApiError::PayloadTooLarge(_) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "/problems/too-large",
                "Payload Too Large",
            ),
            ApiError::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "about:blank",
                "Precondition Required",
            ),
            ApiError::Unprocessable(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/unprocessable",
                "Unprocessable Content",
            ),
            ApiError::IdempotencyMismatch(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/idempotency-mismatch",
                "Unprocessable Content",
            ),
            ApiError::Certainty(e) => {
                let status = crate::certainty::status_of(*e);
                let title = if status == StatusCode::FORBIDDEN {
                    "Forbidden"
                } else {
                    "Unprocessable Content"
                };
                (status, "/problems/certainty", title)
            }
            ApiError::RateLimited { .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                "/problems/rate-limited",
                "Too Many Requests",
            ),
            ApiError::Unavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "about:blank",
                "Service Unavailable",
            ),
            ApiError::VaultLocked(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "/problems/vault-locked",
                "Service Unavailable",
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "about:blank",
                "Internal Server Error",
            ),
        }
    }
}

/// Пояснение состояния запроса доступа для `detail`.
fn access_state_text(state: &str) -> &'static str {
    match state {
        "pending" => "запрос ждёт подтверждения владельца",
        "rejected" => "запрос отклонён владельцем",
        "revoked" => "доступ отозван владельцем",
        _ => "доступа нет",
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::NotFound,
            StoreError::Conflict(m) => ApiError::conflict(m),
            StoreError::BadInput(m) => ApiError::BadRequest(m),
            StoreError::IdempotencyMismatch(m) => ApiError::IdempotencyMismatch(m),
            StoreError::Certainty(c) => ApiError::Certainty(c),
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind, title) = self.parts();
        if let ApiError::Internal(d) = &self {
            tracing::error!(detail = %d, "внутренняя ошибка");
        }
        let mut body = json!({
            "type": kind,
            "title": title,
            "status": status.as_u16(),
            "detail": self.to_string(),
        });
        if let (
            ApiError::Conflict { extra, .. } | ApiError::PossibleDuplicate { extra, .. },
            Value::Object(o),
        ) = (&self, &mut body)
        {
            for (k, v) in extra {
                // стандартные поля problem не перекрываются
                o.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        if let (ApiError::Certainty(c), Value::Object(o)) = (&self, &mut body) {
            o.insert("code".into(), json!(c.code()));
        }
        if let (ApiError::SolutionAccess { guid, state }, Value::Object(o)) = (&self, &mut body) {
            o.insert("state".into(), json!(state));
            o.insert("agent_solution_id".into(), json!(guid));
        }
        let mut resp = (status, Json(body)).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        match self {
            ApiError::RateLimited { retry_after_s } => {
                h.insert(header::RETRY_AFTER, HeaderValue::from(retry_after_s));
            }
            // RFC 9110 §15.5.2: 401 обязан нести схему.
            ApiError::Unauthorized => {
                h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            _ => {}
        }
        resp
    }
}

/// Обработчик неизвестных маршрутов.
pub async fn not_found() -> ApiError {
    ApiError::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Текст паники (с фрагментом заметки) не попадает ни в подробности 500, ни в ответ.
    #[tokio::test]
    async fn aborted_hides_panic_message() {
        let e = tokio::task::spawn_blocking(|| panic!("SECRET-MARKER ghp_abc"))
            .await
            .unwrap_err();
        let err = ApiError::aborted("рендер", &e);
        let ApiError::Internal(d) = &err else {
            panic!("ожидалась Internal");
        };
        assert!(!d.contains("SECRET-MARKER"), "{d}");
        assert!(d.contains("panic = true"), "{d}");
        assert!(!err.to_string().contains("SECRET-MARKER"));
    }
}
