//! Точки остановки для тестов (фича `test-hooks`). Фичу включает только dev-зависимость
//! тестов `zam-server`; в сборке сервера и в релизе этого кода нет, а место вызова
//! компилируется в пустоту.
//!
//! [`hold_rechunk`] задерживает фоновую перенарезку при открытии базы: уведомление уже
//! создано (WARN, `rechunk_progress`), а перенарезка не началась — тест видит ход
//! «0 из N» на любой машине, а не только если перенарезка медленнее запроса.
//!
//! [`crate::time::freeze_now`] замораживает часы хранилища и сервера (`time::now()`, M6 §4.4).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use jiff::Timestamp;

use tokio::sync::{OwnedRwLockWriteGuard, RwLock};

static GATE: LazyLock<Arc<RwLock<()>>> = LazyLock::new(Arc::default);

/// Пока жив результат, фоновые перенарезки при открытии баз процесса ждут перед началом.
pub async fn hold_rechunk() -> OwnedRwLockWriteGuard<()> {
    GATE.clone().write_owned().await
}

/// Ждёт, пока перенарезки не задержаны ([`hold_rechunk`]).
pub(crate) async fn rechunk_gate() {
    drop(GATE.read().await);
}

/// Замороженный момент [`crate::time::now`], микросекунды; [`NOT_FROZEN`] — часы идут.
static FROZEN_US: AtomicI64 = AtomicI64::new(NOT_FROZEN);
const NOT_FROZEN: i64 = i64::MIN;

/// Замороженные часы [`crate::time::freeze_now`]: пока жив, `time::now()` отдаёт заданный момент;
/// держит запись [`GATE`].
pub struct FreezeGuard {
    _gate: OwnedRwLockWriteGuard<()>,
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        // часы снимаются раньше блокировки: следующий держатель GATE их не видит
        FROZEN_US.store(NOT_FROZEN, Ordering::SeqCst);
    }
}

/// Момент заморозки или `None`.
pub(crate) fn frozen() -> Option<Timestamp> {
    match FROZEN_US.load(Ordering::SeqCst) {
        NOT_FROZEN => None,
        us => Timestamp::from_microsecond(us).ok(),
    }
}

pub(crate) fn freeze(t: Timestamp) -> FreezeGuard {
    let gate = block_on(GATE.clone().write_owned());
    FROZEN_US.store(t.as_microsecond(), Ordering::SeqCst);
    FreezeGuard { _gate: gate }
}

/// Синхронное ожидание будущего парковкой потока: блокировки `tokio::sync` не требуют рантайма,
/// а `blocking_*` tokio паникуют в контексте рантайма (`#[tokio::test]`).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}
