//! Точки остановки для тестов (фича `test-hooks`). Фичу включает только dev-зависимость
//! тестов `zam-server`; в сборке сервера и в релизе этого кода нет, а место вызова
//! компилируется в пустоту.
//!
//! [`hold_rechunk`] задерживает фоновую перенарезку при открытии базы: уведомление уже
//! создано (WARN, `rechunk_progress`), а перенарезка не началась — тест видит ход
//! «0 из N» на любой машине, а не только если перенарезка медленнее запроса.

use std::sync::{Arc, LazyLock};

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
