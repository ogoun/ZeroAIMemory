//! Неверный токен не попадает в журнал: только маска (отдельный бинарь — глобальный subscriber).
mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

/// Общий буфер, куда пишет subscriber.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[tokio::test]
async fn bad_token_not_in_logs() {
    let buf = Buf::default();
    tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .init();
    let cfg = common::cfg_in(common::tmp("auth-logs"));
    let (app, ..) = zam_server::build(cfg, Some("pw".into())).await.unwrap();
    let token = format!("zam_{}{}", "A".repeat(8), "B".repeat(32));
    let req = Request::get("/api/v1/spaces?x=1")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let r = app.oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains("BBBBBBBB"), "{log}");
    assert!(log.contains("zam_AAAAAAAA…"), "{log}");
    // событие о неудаче есть и содержит ip
    assert!(log.contains("WARN") && log.contains("ip=0.0.0.0"), "{log}");
}
