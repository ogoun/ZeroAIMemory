//! Ошибки API в формате RFC 9457 (`application/problem+json`).
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use zam_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("требуется аутентификация")]
    Unauthorized,
    #[error("доступ запрещён")]
    Forbidden,
    #[error("ресурс не найден")]
    NotFound,
    /// `extra` — расширения RFC 9457: поля попадают в тело problem рядом с `detail`.
    #[error("{detail}")]
    Conflict {
        detail: String,
        extra: Map<String, Value>,
    },
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("требуется заголовок If-Match с ревизией")]
    PreconditionRequired,
    #[error("{0}")]
    Unprocessable(String),
    #[error("слишком много запросов")]
    RateLimited { retry_after_s: u64 },
    #[error("{0}")]
    Unavailable(String),
    /// Подробности только в лог; клиенту уходит общий текст.
    #[error("внутренняя ошибка")]
    Internal(String),
}

impl ApiError {
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
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "about:blank", "Forbidden"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "about:blank", "Not Found"),
            ApiError::Conflict { .. } => (StatusCode::CONFLICT, "/problems/conflict", "Conflict"),
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

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::NotFound,
            StoreError::Conflict(m) => ApiError::conflict(m),
            StoreError::BadInput(m) => ApiError::BadRequest(m),
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
        if let (ApiError::Conflict { extra, .. }, Value::Object(o)) = (&self, &mut body) {
            for (k, v) in extra {
                // стандартные поля problem не перекрываются
                o.entry(k.clone()).or_insert_with(|| v.clone());
            }
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
