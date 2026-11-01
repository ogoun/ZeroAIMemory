//! Замер памяти `/read` вложения (M5, задача 7, раунд правок 1, M-4): лист на 60 000 строк,
//! 20 элементов с `locator` на весь лист в одной пачке. Чтение берёт из базы только чанки в
//! пределах бюджета, поэтому пик памяти запроса — от бюджета, а не от размера листа. Счётчик
//! аллокаций — на весь бинарник теста (разбор идёт в рабочем процессе и сюда не входит). Замер
//! на Windows (debug): до правки — 136,9 МиБ, после — 0,5 МиБ.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use axum::http::{Method, StatusCode};
use common::api::api;
use common::attach::{upload, wait_done, xlsx};
use serde_json::json;

struct Counter;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize) {
    let c = CUR.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
}

// SAFETY: делегирует системному аллокатору, только считает байты
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CUR.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            if n > l.size() {
                add(n - l.size());
            } else {
                CUR.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static GLOBAL: Counter = Counter;

const MIB: f64 = 1024.0 * 1024.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_whole_sheet_peak_follows_budget() {
    let a = api("read-budget-mem", |_| {}).await;
    let user = a.st.admin_id;
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "p/склад", "kind": "note",
                        "content": "# Склад\n\nОстатки.\n"}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let node = r.json()["id"].as_str().unwrap().to_string();
    let rows: Vec<[String; 2]> = (0..60_000)
        .map(|i| [format!("item-{i:05}"), format!("остаток {i} штук на полке")])
        .collect();
    let mut sheet: Vec<&[&str]> = vec![&["Item", "Qty"]];
    let cells: Vec<[&str; 2]> = rows
        .iter()
        .map(|r| [r[0].as_str(), r[1].as_str()])
        .collect();
    sheet.extend(cells.iter().map(|c| c.as_slice()));
    let book = xlsx(&[("Data", &sheet)]);
    upload(&a, &a.token, &node, "склад.xlsx", &book).await;
    let d = wait_done(&a, user, &node, "склад.xlsx").await;
    let item = json!({"id": format!("b:{}", d.blob_sha256), "locator": "sheet:Data!A1:B60001"});
    let body = json!({"items": vec![item; 20]});
    // прогрев (пулы соединений, кэши запросов)
    let r = a.t(Method::POST, "/api/v1/read", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    drop(r);
    let base = CUR.load(Relaxed);
    PEAK.store(base, Relaxed);
    let r = a.t(Method::POST, "/api/v1/read", Some(body)).await;
    let peak = PEAK.load(Relaxed).saturating_sub(base);
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let first = &j["items"][0];
    assert_eq!(first["truncated"], true, "{first}");
    assert!(first["more_tokens"].as_u64().unwrap() > 100_000, "{first}");
    eprintln!(
        "M-4: лист 60 000 строк, 20 элементов: пик запроса {:.1} МиБ, ответ {} байт",
        peak as f64 / MIB,
        r.body.len()
    );
    assert!(peak < 16 * 1024 * 1024, "пик {:.1} МиБ", peak as f64 / MIB);
    a.st.shutdown().await;
}
