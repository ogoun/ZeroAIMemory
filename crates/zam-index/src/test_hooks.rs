//! Точки остановки для тестов (фича `test-hooks`). Фичу включает только dev-зависимость
//! тестов `zam-server` (напрямую и через его фичу `test-hooks`, которой [`Hold`] нужен для
//! точки поиска); в сборке сервера и в релизе этого кода нет, а места вызова компилируются
//! в пустоту — накладных расходов на рабочем пути нет.
//!
//! Тест «взводит» точку, код доходит до неё и стоит, тест делает своё (убивает Qdrant)
//! и отпускает. Так события, которые в жизни разделяет случайный тайминг, идут в
//! заданном порядке на любой машине.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, watch};

/// Одноразовая точка остановки: срабатывает на первом проходе после взвода.
pub struct Hold {
    armed: AtomicBool,
    parked: watch::Sender<bool>,
    release: Notify,
}

impl Default for Hold {
    fn default() -> Self {
        Hold {
            armed: AtomicBool::new(false),
            parked: watch::channel(false).0,
            release: Notify::new(),
        }
    }
}

impl Hold {
    /// Место остановки: взведена — стоять до [`Hold::release`].
    pub async fn park(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        self.parked.send_replace(true);
        self.release.notified().await;
        self.parked.send_replace(false);
    }

    /// Взвести: остановится следующий проход.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// Ждёт остановки на точке; `false` — не дошли за `limit`.
    pub async fn wait_parked(&self, limit: Duration) -> bool {
        let mut rx = self.parked.subscribe();
        tokio::time::timeout(limit, rx.wait_for(|p| *p))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Отпустить и снять взвод (в том числе при панике теста). Разрешение `Notify`
    /// сохраняется, даже если код ещё не дошёл до ожидания, — гонки «отпустили раньше,
    /// чем встал» нет.
    pub fn release(&self) {
        self.armed.store(false, Ordering::SeqCst);
        self.release.notify_one();
    }
}
