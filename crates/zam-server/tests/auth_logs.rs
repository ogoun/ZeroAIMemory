//! Неверный и отозванный токены не попадают в журнал: только маска (отдельный бинарь — глобальный subscriber).
mod common;

use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
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
    let (app, st, _) = zam_server::build(cfg, Some("pw".into())).await.unwrap();
    let token = format!("zam_{}{}", "A".repeat(8), "B".repeat(32));
    let req = Request::get("/api/v1/spaces?x=1")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains("BBBBBBBB"), "{log}");
    assert!(log.contains("zam_AAAAAAAA…"), "{log}");
    // событие о неудаче есть и содержит ip
    assert!(log.contains("WARN") && log.contains("ip=0.0.0.0"), "{log}");
    // отозванный действующий токен: в журнале маска, хвоста нет
    let admin = st.system.admin_user().await.unwrap().unwrap();
    let (info, revoked) = st
        .system
        .issue_token(admin.id, "logs", zam_store::Access::Ro, false)
        .await
        .unwrap();
    st.system.revoke_token(info.id).await.unwrap();
    let req = Request::get("/api/v1/spaces")
        .header(header::AUTHORIZATION, format!("Bearer {revoked}"))
        .body(Body::empty())
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains(&revoked[12..]), "{log}");
    assert!(log.contains(&zam_core::mask_token(&revoked)), "{log}");
    // прокси вне trusted_proxies — одно предупреждение на процесс
    for _ in 0..2 {
        let mut req = Request::get("/api/v1/spaces")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 2, 2, 2], 1))));
        let r = app.clone().oneshot(req).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let n = log.matches("server.trusted_proxies").count();
    assert_eq!(n, 1, "{log}");
}
