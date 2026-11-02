//! Ограничитель больших тел запросов (M5.5, ревью задачи 2). Отчёт MCP приходит одним JSON до
//! [`super::entries_json_max`] (≈ 32 МБ при умолчаниях); такие тела держатся в памяти целиком, а
//! `/mcp` (rmcp) ещё и строит из них `Map<String, Value>`. Тела не больше
//! [`super::note_body_max`] идут как раньше; «большое» тело — заявленное `Content-Length` или
//! фактически прочитанное больше этого порога — занимает одно из [`BIG_BODY_PERMITS`] разрешений
//! общего семафора `POST /spaces/{space}/entries` и `/mcp` на время обработки запроса. Разрешение
//! не получено за [`BIG_BODY_WAIT`] — 503 `/problems/busy` с `Retry-After`. Так в памяти не
//! больше двух больших тел одновременно, сколько бы токенов их ни слали.
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::app::AppState;
use crate::error::ApiError;

/// Больших тел в обработке одновременно.
pub const BIG_BODY_PERMITS: usize = 2;
/// Ожидание разрешения, прежде чем ответить 503.
pub const BIG_BODY_WAIT: Duration = Duration::from_millis(500);
/// `Retry-After` ответа 503, секунд.
const RETRY_AFTER_S: u64 = 2;

/// Семафор больших тел и порог «большого» тела, байт.
#[derive(Clone)]
pub struct BigBodies {
    sem: Arc<Semaphore>,
    threshold: usize,
}

impl BigBodies {
    pub fn new(permits: usize, threshold: usize) -> BigBodies {
        BigBodies {
            sem: Arc::new(Semaphore::new(permits)),
            threshold,
        }
    }

    /// Порог «большого» тела, байт.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Разрешение на большое тело: ждёт не дольше [`BIG_BODY_WAIT`], иначе 503 `busy`.
    pub async fn admit(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        match tokio::time::timeout(BIG_BODY_WAIT, self.sem.clone().acquire_owned()).await {
            Ok(Ok(p)) => Ok(p),
            _ => Err(ApiError::Busy {
                retry_after_s: RETRY_AFTER_S,
            }),
        }
    }

    /// Тело до `limit` байт (больше — 413). `declared` — `Content-Length`: больше `limit` — 413
    /// без чтения, больше порога — разрешение до чтения; без заголовка разрешение берётся, как
    /// только прочитано больше порога. Разрешение держит вызывающий до конца обработки.
    pub async fn read(
        &self,
        body: Body,
        limit: usize,
        declared: Option<u64>,
    ) -> Result<(Bytes, Option<OwnedSemaphorePermit>), ApiError> {
        let too_large = || ApiError::PayloadTooLarge("тело запроса превышает предел".into());
        if declared.is_some_and(|n| n > limit as u64) {
            return Err(too_large());
        }
        let mut permit = None;
        if declared.is_some_and(|n| n > self.threshold as u64) {
            permit = Some(self.admit().await?);
        }
        let mut s = body.into_data_stream();
        let mut buf = Vec::new();
        while let Some(chunk) = s.next().await {
            let chunk =
                chunk.map_err(|e| ApiError::BadRequest(format!("некорректное тело: {e}")))?;
            if buf.len() + chunk.len() > limit {
                return Err(too_large());
            }
            buf.extend_from_slice(&chunk);
            if permit.is_none() && buf.len() > self.threshold {
                permit = Some(self.admit().await?);
            }
        }
        Ok((Bytes::from(buf), permit))
    }
}

/// `Content-Length` запроса (нет или не число — `None`).
pub fn content_length(h: &axum::http::HeaderMap) -> Option<u64> {
    h.get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

/// Слой `/mcp`: тело читается здесь с пределом [`super::entries_json_max`] (он же предел rmcp)
/// и с разрешением для большого тела; дальше запрос идёт с прочитанным телом.
pub async fn limit_mcp(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let limit = super::entries_json_max(&st.cfg);
    let declared = content_length(&parts.headers);
    let (bytes, permit) = match st.big_bodies.read(body, limit, declared).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let resp = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    drop(permit);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permits_busy_and_threshold() {
        let b = BigBodies::new(2, 10);
        let p1 = b.admit().await.unwrap();
        let _p2 = b.admit().await.unwrap();
        let e = b.admit().await.unwrap_err();
        assert!(matches!(e, ApiError::Busy { .. }), "{e:?}");
        // малое тело — без разрешения, даже когда все заняты
        let (bytes, p) = b.read(Body::from("0123456789"), 100, None).await.unwrap();
        assert_eq!((bytes.len(), p.is_some()), (10, false));
        // большое без Content-Length — 503, пока разрешений нет
        let e = b
            .read(Body::from("x".repeat(11)), 100, None)
            .await
            .unwrap_err();
        assert!(matches!(e, ApiError::Busy { .. }), "{e:?}");
        let e = b.read(Body::from("x"), 100, Some(50)).await.unwrap_err();
        assert!(matches!(e, ApiError::Busy { .. }), "{e:?}");
        drop(p1);
        let (_, p) = b.read(Body::from("x".repeat(11)), 100, None).await.unwrap();
        assert!(p.is_some());
        // предел: заявленный и фактический
        let e = b.read(Body::from("x"), 100, Some(101)).await.unwrap_err();
        assert!(matches!(e, ApiError::PayloadTooLarge(_)), "{e:?}");
        drop(p);
        let e = b
            .read(Body::from("x".repeat(101)), 100, None)
            .await
            .unwrap_err();
        assert!(matches!(e, ApiError::PayloadTooLarge(_)), "{e:?}");
    }
}
