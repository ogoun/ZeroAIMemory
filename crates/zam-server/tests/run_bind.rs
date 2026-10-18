//! Отдельный бинарь: run() ставит глобальный subscriber.
mod common;

use common::{cfg_in, tmp};

#[tokio::test]
async fn busy_port_issues_no_token() {
    let busy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let d = tmp("run1");
    let mut cfg = cfg_in(d.clone());
    cfg.server.bind = busy.local_addr().unwrap();
    let r = zam_server::run(cfg, Some("pw".into())).await;
    assert!(r.is_err(), "bind на занятый порт должен упасть");
    // run() вернул управление и отпустил лок; админа и токенов быть не должно
    let dd = zam_store::DataDir::open(&d).unwrap();
    let st = zam_store::SystemStore::open(&dd).unwrap();
    assert!(st.admin_user().await.unwrap().is_none());
}
