//! Точки остановки для тестов (фича `test-hooks`). Фичу включают только dev-зависимости:
//! тестов самого `zam-index` и тестов `zam-server` (напрямую и через его фичу `test-hooks`,
//! которой [`Hold`] нужен для точки поиска); в сборке сервера и в релизе этого кода нет, а места
//! вызова компилируются в пустоту — накладных расходов на рабочем пути нет.
//!
//! Тест «взводит» точку, код доходит до неё и стоит, тест делает своё (убивает Qdrant)
//! и отпускает. Так события, которые в жизни разделяет случайный тайминг, идут в
//! заданном порядке на любой машине.
//!
//! [`DeleteFaults`] — сбои `delete_collection` у [`crate::QdrantIndex`] (M7.1, задача 7): сбой
//! Qdrant на Windows («failed to rename file», `i:01M7Y3C3A6E069FQ3QJXGZZGTQ`) воспроизводится
//! на любой машине. Сбои привязаны к одному клиенту, а не к процессу: параллельные тесты
//! одного бинарника друг другу не мешают.

use std::sync::Mutex;
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

/// Сбои `delete_collection` одного [`crate::QdrantIndex`] ([`crate::QdrantIndex::with_delete_faults`]):
/// взведённые попытки отвечают ошибкой Qdrant `Internal` с заданным текстом, не обращаясь к
/// Qdrant; остальные идут в Qdrant. Журнал — число попыток удаления с последнего взвода, удачных
/// и нет.
#[derive(Debug, Default)]
pub struct DeleteFaults(Mutex<Faults>);

#[derive(Debug, Default)]
struct Faults {
    left: u32,
    message: String,
    attempts: u32,
}

impl DeleteFaults {
    fn state(&self) -> std::sync::MutexGuard<'_, Faults> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Взвести: `n` следующих попыток — сбой `Internal` с текстом `message`; журнал — с нуля.
    pub fn fail_next(&self, n: u32, message: &str) {
        let mut s = self.state();
        s.left = n;
        s.message = message.to_string();
        s.attempts = 0;
    }

    /// Попыток удаления с последнего взвода.
    pub fn attempts(&self) -> u32 {
        self.state().attempts
    }

    /// Попытка удаления: в журнал; взведённая — текст сбоя вместо запроса к Qdrant.
    pub(crate) fn attempt(&self) -> Option<String> {
        let mut s = self.state();
        s.attempts += 1;
        (s.left > 0).then(|| {
            s.left -= 1;
            s.message.clone()
        })
    }
}
