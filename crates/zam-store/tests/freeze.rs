//! Замороженные часы `zam_store::time::freeze_now` (фича `test-hooks`, M6 §4.4). Отдельный
//! двоичный файл: часы общие для процесса, рядом с ними не идут тесты, читающие настоящее время.
//! Запуск: `cargo test -p zam-store --features test-hooks --test freeze`.
use std::sync::mpsc;
use std::time::Duration;

use jiff::Timestamp;
use zam_store::test_hooks::hold_rechunk;
use zam_store::time::{freeze_now, now, utc_today};

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

#[test]
fn freeze_now_applies() {
    let t = ts("2026-10-10T14:03:22.123456Z");
    {
        let _g = freeze_now(t);
        assert_eq!(now(), t);
        assert_eq!(now(), t, "время не идёт");
        assert_eq!(utc_today().to_string(), "2026-10-10");
    }
    // после снятия — настоящие часы (блокировка тестовых точек без заморозки: параллельные
    // тесты файла часы сейчас не заморозят)
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let hold = rt.block_on(hold_rechunk());
    let real = Timestamp::now();
    assert!((now().as_second() - real.as_second()).abs() <= 1);
    drop(hold);
    // повторная заморозка после снятия — без взаимной блокировки
    let t2 = ts("2030-01-01T00:00:00Z");
    let _g = freeze_now(t2);
    assert_eq!(now(), t2);
}

/// Повторная заморозка из того же потока при живом guard — паника, а не вечное ожидание.
#[test]
#[should_panic(expected = "freeze_now: блокировка тестовых точек уже у этого потока")]
fn freeze_twice_same_thread_panics() {
    let _a = freeze_now(ts("2026-10-10T00:00:00Z"));
    let _b = freeze_now(ts("2026-10-11T00:00:00Z"));
}

/// `hold_rechunk` и затем `freeze_now` в том же потоке — тоже паника.
#[test]
#[should_panic(expected = "freeze_now: блокировка тестовых точек уже у этого потока")]
fn freeze_after_hold_rechunk_same_thread_panics() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _hold = rt.block_on(hold_rechunk());
    let _g = freeze_now(ts("2026-10-10T00:00:00Z"));
}

/// Guard другого потока — ожидание до его снятия, затем заморозка.
#[test]
fn freeze_waits_for_other_thread() {
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let a = std::thread::spawn(move || {
        let _g = freeze_now(ts("2026-10-10T00:00:00Z"));
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    held_rx.recv().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let b = std::thread::spawn(move || {
        let t2 = ts("2026-10-12T00:00:00Z");
        let _g = freeze_now(t2);
        done_tx.send(now()).unwrap();
    });
    // пока guard потока A жив, B ждёт
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
    release_tx.send(()).unwrap();
    assert_eq!(done_rx.recv().unwrap(), ts("2026-10-12T00:00:00Z"));
    a.join().unwrap();
    b.join().unwrap();
}
