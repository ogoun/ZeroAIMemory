//! Ошибки API в формате RFC 9457 (`application/problem+json`).
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("требуется аутентификация")]
    Unauthorized,
    #[error("доступ запрещён")]
    Forbidden,
    #[error("ресурс не найден")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("слишком много запросов")]
    RateLimited { retry_after_s: u64 },
    #[error("{0}")]
    Unavailable(String),
    /// Подробности только в лог; клиенту уходит общий текст.
    #[error("внутренняя ошибка")]
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "about:blank", "Unauthorized"),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "about:blank", "Forbidden"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "about:blank", "Not Found"),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "/problems/conflict", "Conflict"),
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
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "about:blank",
                "Internal Server Error",
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind, title) = self.parts();
        if let ApiError::Internal(d) = &self {
            tracing::error!(detail = %d, "внутренняя ошибка");
        }
        let body = json!({
            "type": kind,
            "title": title,
            "status": status.as_u16(),
            "detail": self.to_string(),
        });
        let mut resp = (status, Json(body)).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let ApiError::RateLimited { retry_after_s } = self {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_s));
        }
        resp
    }
}

/// Обработчик неизвестных маршрутов.
pub async fn not_found() -> ApiError {
    ApiError::NotFound
}
