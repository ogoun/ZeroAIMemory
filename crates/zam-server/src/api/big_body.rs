//! Ограничитель больших тел запросов (M5.5, ревью задачи 2). Отчёт MCP приходит одним JSON до
//! [`super::entries_json_max`] (≈ 32 МБ при умолчаниях); такие тела держатся в памяти целиком, а
//! `/mcp` (rmcp) ещё и строит из них `Map<String, Value>`. Тела не больше
//! [`super::note_body_max`] идут как раньше; «большое» тело — прочитанное больше этого порога —
//! занимает одно из [`BIG_BODY_PERMITS`] разрешений общего семафора `POST /spaces/{space}/entries`
//! и `/mcp` на время обработки запроса. Разрешение не получено за [`BIG_BODY_WAIT`] — 503
//! `/problems/busy` с `Retry-After`. Так в памяти не больше двух больших тел одновременно.
//!
//! - **Порог читается без разрешения:** первые байты до порога (в памяти не больше обычного
//!   тела) позволяют отказать 413 раньше ожидания разрешения (`early_reject`, например не-отчёт
//!   с `kind` в начале тела).
//! - **Срок чтения:** с разрешением тело дочитывается не дольше [`BIG_BODY_READ_TIMEOUT`] (для
//!   тестов — [`BigBodies::set_read_timeout`]); дольше — 408 `/problems/timeout`, разрешение
//!   освобождается: медленный клиент не держит его бесконечно.
//! - **Внутренние вызовы:** инструменты MCP зовут REST через внутренний роутер (`oneshot`) с
//!   расширением [`InnerCall`]. Если тело внешнего `/mcp` заняло разрешение ([`limit_mcp`]
//!   ставит [`OuterPermit`]), `InnerCall { outer_permit: true }` — второе не берётся (иначе два
//!   больших MCP-отчёта заняли бы оба разрешения и их внутренние POST получили бы 503). Малое
//!   внешнее тело (`outer_permit: false`), из которого инструмент собрал большое внутреннее, —
//!   разрешение берётся как у внешнего запроса. Расширения запроса из сети не приходят — hyper
//!   создаёт их пустыми, метки ставят только [`limit_mcp`] и [`crate::mcp`].
//! - **Подсказка `zam report`:** 413 `/mcp` и 408 срока чтения называют путь для больших
//!   отчётов — CLI шлёт multipart потоком без base64 и без предела `reports.mcp_max_bytes`.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
/// Срок дочитывания большого тела с разрешением.
pub const BIG_BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// `Retry-After` ответа 503, секунд.
const RETRY_AFTER_S: u64 = 2;

/// Подсказка в 413 `/mcp` и 408: большой отчёт — через CLI.
const CLI_HINT: &str = "большой отчёт — используйте `zam report <файл.md>` (multipart потоком)";

/// Метка внутреннего запроса MCP → REST; `outer_permit` — внешний `/mcp` держит разрешение.
#[derive(Debug, Clone, Copy)]
pub struct InnerCall {
    pub outer_permit: bool,
}

/// Разрешение тела запроса `/mcp` (ставит [`limit_mcp`]): обработчик инструмента держит копию
/// `Arc` до конца вызова — разрешение живо, пока жив хоть один владелец (обрыв клиента не
/// освобождает его раньше внутреннего вызова REST).
#[derive(Debug, Clone)]
pub struct OuterPermit(pub Arc<OwnedSemaphorePermit>);

/// Семафор больших тел, порог «большого» тела (байт) и срок чтения.
#[derive(Clone)]
pub struct BigBodies {
    sem: Arc<Semaphore>,
    threshold: usize,
    read_timeout_ms: Arc<AtomicU64>,
}

impl BigBodies {
    pub fn new(permits: usize, threshold: usize) -> BigBodies {
        BigBodies {
            sem: Arc::new(Semaphore::new(permits)),
            threshold,
            read_timeout_ms: Arc::new(AtomicU64::new(BIG_BODY_READ_TIMEOUT.as_millis() as u64)),
        }
    }

    /// Порог «большого» тела, байт.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Свободных разрешений сейчас (тесты и диагностика).
    pub fn available(&self) -> usize {
        self.sem.available_permits()
    }

    /// Срок дочитывания большого тела (действует на все копии; по умолчанию
    /// [`BIG_BODY_READ_TIMEOUT`]).
    pub fn set_read_timeout(&self, d: Duration) {
        self.read_timeout_ms
            .store(d.as_millis() as u64, Ordering::Relaxed);
    }

    fn read_timeout(&self) -> Duration {
        Duration::from_millis(self.read_timeout_ms.load(Ordering::Relaxed))
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

    /// Тело до `limit` байт (больше — 413; заявленное `declared` больше — 413 без чтения). Тело
    /// больше порога: прочитанный префикс до порога проверяет `early_reject` (`true` — 413 без
    /// ожидания разрешения), затем берётся разрешение (кроме `held` — его уже держит внешний
    /// запрос) и тело дочитывается не дольше срока (408). Разрешение держит вызывающий до конца
    /// обработки.
    pub async fn read(
        &self,
        body: Body,
        limit: usize,
        declared: Option<u64>,
        held: bool,
        early_reject: impl Fn(&[u8]) -> bool,
    ) -> Result<(Bytes, Option<OwnedSemaphorePermit>), ApiError> {
        let too_large = || ApiError::PayloadTooLarge("тело запроса превышает предел".into());
        if declared.is_some_and(|n| n > limit as u64) {
            return Err(too_large());
        }
        let mut s = body.into_data_stream();
        let mut buf = Vec::new();
        let mut permit = None;
        let mut deadline: Option<tokio::time::Instant> = None;
        loop {
            let next = match deadline {
                Some(d) => match tokio::time::timeout_at(d, s.next()).await {
                    Ok(n) => n,
                    Err(_) => {
                        return Err(ApiError::RequestTimeout(format!(
                            "большое тело запроса не получено за {} с: {CLI_HINT}",
                            self.read_timeout().as_secs_f32()
                        )));
                    }
                },
                None => s.next().await,
            };
            let Some(chunk) = next else {
                break;
            };
            let chunk =
                chunk.map_err(|e| ApiError::BadRequest(format!("некорректное тело: {e}")))?;
            if buf.len() + chunk.len() > limit {
                return Err(too_large());
            }
            buf.extend_from_slice(&chunk);
            if deadline.is_none() && buf.len() > self.threshold {
                if early_reject(&buf[..self.threshold]) {
                    return Err(too_large());
                }
                if !held {
                    permit = Some(self.admit().await?);
                }
                deadline = Some(tokio::time::Instant::now() + self.read_timeout());
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

/// Разрешение большого тела уже держит внешний запрос: внутренний вызов MCP → REST из `/mcp`
/// с большим телом ([`InnerCall`] с `outer_permit`).
pub fn permit_held(ext: &axum::http::Extensions) -> bool {
    ext.get::<InnerCall>().is_some_and(|c| c.outer_permit)
}

/// Слой `/mcp`: тело читается здесь с пределом [`super::entries_json_max`] (он же предел rmcp)
/// и с разрешением для большого тела; дальше запрос идёт с прочитанным телом и меткой
/// [`OuterPermit`], если разрешение занято.
pub async fn limit_mcp(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let limit = super::entries_json_max(&st.cfg);
    let declared = content_length(&parts.headers);
    let read = st
        .big_bodies
        .read(body, limit, declared, false, |_| false)
        .await;
    let (bytes, permit) = match read {
        Ok(v) => v,
        Err(ApiError::PayloadTooLarge(m)) => {
            return ApiError::PayloadTooLarge(format!("{m}: {CLI_HINT}")).into_response();
        }
        Err(e) => return e.into_response(),
    };
    // разрешение — общее с обработчиком: rmcp выполняет инструмент в своей задаче, и при обрыве
    // клиента этот future уничтожается раньше, чем инструмент закончит (ревью задачи 5, I1)
    let permit = permit.map(Arc::new);
    if let Some(p) = &permit {
        parts.extensions.insert(OuterPermit(p.clone()));
    }
    let resp = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    drop(permit);
    resp
}

/// Вид записи по началу JSON-тела: `Some(true)` — верхнеуровневый `"kind": "report"`,
/// `Some(false)` — `kind` другой (или не строка, или объект кончился без него), `None` —
/// не понять по префиксу (обрыв, экранирование в значении `kind`, не объект).
pub fn kind_prefix(b: &[u8]) -> Option<bool> {
    let mut i = ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        i = ws(b, i);
        match b.get(i)? {
            b'}' => return Some(false),
            b'"' => {}
            _ => return None,
        }
        let (key, after) = string(b, i)?;
        i = ws(b, after);
        if b.get(i) != Some(&b':') {
            return None;
        }
        i = ws(b, i + 1);
        if key == b"kind" {
            return match b.get(i)? {
                b'"' => {
                    let (v, _) = string(b, i)?;
                    if v.contains(&b'\\') {
                        None
                    } else {
                        Some(v == b"report")
                    }
                }
                _ => Some(false),
            };
        }
        i = ws(b, skip_value(b, i)?);
        match b.get(i)? {
            b',' => i += 1,
            b'}' => return Some(false),
            _ => return None,
        }
    }
}

fn ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// Строка JSON с `b[i] == '"'`: содержимое (с экранированием как есть) и позиция после `"`.
fn string(b: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let start = i + 1;
    let mut k = start;
    while k < b.len() {
        match b[k] {
            b'\\' => k += 2,
            b'"' => return Some((&b[start..k], k + 1)),
            _ => k += 1,
        }
    }
    None
}

/// Позиция после значения JSON с `b[i]` (строки, вложенные объекты и массивы, литералы).
fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => string(b, i).map(|(_, e)| e),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut k = i;
            while k < b.len() {
                match b[k] {
                    b'"' => {
                        k = string(b, k)?.1;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(k + 1);
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            None
        }
        _ => {
            let mut k = i;
            while k < b.len() && !matches!(b[k], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                k += 1;
            }
            (k < b.len()).then_some(k)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read(b: &BigBodies, body: Body, declared: Option<u64>) -> Result<bool, ApiError> {
        b.read(body, 100, declared, false, |_| false)
            .await
            .map(|(_, p)| p.is_some())
    }

    #[tokio::test]
    async fn permits_busy_and_threshold() {
        let b = BigBodies::new(2, 10);
        let p1 = b.admit().await.unwrap();
        let _p2 = b.admit().await.unwrap();
        let e = b.admit().await.unwrap_err();
        assert!(matches!(e, ApiError::Busy { .. }), "{e:?}");
        // малое тело — без разрешения, даже когда все заняты
        assert!(!read(&b, Body::from("0123456789"), None).await.unwrap());
        // большое — 503, пока разрешений нет (заявленная длина без чтения не решает)
        let e = read(&b, Body::from("x".repeat(11)), None)
            .await
            .unwrap_err();
        assert!(matches!(e, ApiError::Busy { .. }), "{e:?}");
        let e = read(&b, Body::from("x".repeat(11)), Some(11))
            .await
            .unwrap_err();
        assert!(matches!(e, ApiError::Busy { .. }), "{e:?}");
        // разрешение уже держит внешний запрос (внутренний вызов MCP → REST) — второе не берётся
        let (bytes, p) = b
            .read(Body::from("x".repeat(50)), 100, None, true, |_| false)
            .await
            .unwrap();
        assert_eq!((bytes.len(), p.is_some()), (50, false));
        // ранний отказ по префиксу — 413, а не 503
        let e = b
            .read(Body::from("x".repeat(50)), 100, Some(50), false, |_| true)
            .await
            .unwrap_err();
        assert!(matches!(e, ApiError::PayloadTooLarge(_)), "{e:?}");
        drop(p1);
        assert!(read(&b, Body::from("x".repeat(11)), None).await.unwrap());
        // предел: заявленный и фактический
        let e = read(&b, Body::from("x"), Some(101)).await.unwrap_err();
        assert!(matches!(e, ApiError::PayloadTooLarge(_)), "{e:?}");
        let e = read(&b, Body::from("x".repeat(101)), None)
            .await
            .unwrap_err();
        assert!(matches!(e, ApiError::PayloadTooLarge(_)), "{e:?}");
    }

    /// Метка внутреннего вызова сама по себе разрешения не заменяет: только `outer_permit`.
    #[test]
    fn permit_held_only_with_outer_permit() {
        let mut ext = axum::http::Extensions::new();
        assert!(!permit_held(&ext));
        ext.insert(InnerCall {
            outer_permit: false,
        });
        assert!(!permit_held(&ext));
        ext.insert(InnerCall { outer_permit: true });
        assert!(permit_held(&ext));
    }

    #[tokio::test]
    async fn slow_body_times_out_and_frees_permit() {
        let b = BigBodies::new(1, 10);
        b.set_read_timeout(Duration::from_millis(100));
        let first: Result<Bytes, std::io::Error> = Ok(Bytes::from("x".repeat(20)));
        let slow = futures_util::stream::iter([first]).chain(futures_util::stream::pending());
        let e = b
            .read(Body::from_stream(slow), 100, None, false, |_| false)
            .await
            .unwrap_err();
        assert!(
            matches!(&e, ApiError::RequestTimeout(m) if m.contains("zam report")),
            "{e:?}"
        );
        // разрешение свободно
        let _p = b.admit().await.unwrap();
    }

    #[test]
    fn kind_by_prefix() {
        assert_eq!(kind_prefix(br#"{"kind": "report", "x": "#), Some(true));
        assert_eq!(
            kind_prefix(
                br#" { "title": "a\"b", "data": {"k": [1, {"z": "}"}]}, "kind":"changelog""#
            ),
            Some(false)
        );
        assert_eq!(kind_prefix(br#"{"kind": 5"#), Some(false));
        assert_eq!(kind_prefix(br#"{"title": "t"}"#), Some(false));
        assert_eq!(kind_prefix(br#"{"images": [{"data": "AAAA"#), None);
        // экранирование в значении kind — по префиксу не решаем
        assert_eq!(kind_prefix(b"{\"kind\": \"rep\\u006frt\"}"), None);
        assert_eq!(kind_prefix(b"[1, 2]"), None);
        assert_eq!(kind_prefix(br#"{"n": 12"#), None);
    }
}
