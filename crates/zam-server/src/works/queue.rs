//! Очередь внутренних работ (M7.3 §6.3): постановка работ ночи, снятие открытых работ при смене
//! режима вида, возврат работ модели в очередь при старте.
//!
//! В задаче 1 плана — точки входа без тела (`Ok(())`): их уже зовут ночной исполнитель
//! ([`super::night::NightRunner`]: ночь и шаги старта в фоне) и `PATCH` настроек; тело — задача 3.

use jiff::{Timestamp, civil};
use zam_core::Id;
use zam_store::StoreError;
use zam_store::works::WorkKind;

use super::night::NightCtx;

/// Постановка и выполнение работ ночи `night` до конца окна `until` по всем пользователям
/// (§6.3 п. 2) — после проверки всех пользователей ([`crate::check::night`]).
pub async fn night(ctx: &NightCtx, night: civil::Date, until: Timestamp) -> Result<(), StoreError> {
    let _ = (ctx, night, until);
    Ok(())
}

/// Действующий режим видов `kinds` пользователя `user` сменился (`PATCH` настроек, §6.1 п. 2):
/// их открытые работы (`queued`, `running`) снимаются (`cancelled`, причина `mode_changed`).
pub async fn on_mode_change(
    ctx: &NightCtx,
    user: Id,
    kinds: &[WorkKind],
) -> Result<(), StoreError> {
    #[cfg(feature = "test-hooks")]
    test_hooks::record(user, kinds);
    let _ = (ctx, user, kinds);
    Ok(())
}

/// Один раз после сборки состояния, при любом `[works] enabled`, в фоне задачи раннера (§6.1 п. 5;
/// сборку не задерживает): работы модели `running`, оборванные остановкой, — снова `queued`.
pub async fn on_start(ctx: &NightCtx) -> Result<(), StoreError> {
    let _ = ctx;
    Ok(())
}

/// Тестовые точки очереди (фича `test-hooks`): журнал вызовов [`on_mode_change`].
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use zam_core::Id;
    use zam_store::works::WorkKind;

    static MODE_CHANGES: LazyLock<Mutex<HashMap<Id, Vec<Vec<WorkKind>>>>> =
        LazyLock::new(Default::default);

    pub(super) fn record(user: Id, kinds: &[WorkKind]) {
        MODE_CHANGES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(user)
            .or_default()
            .push(kinds.to_vec());
    }

    /// Вызовы [`super::on_mode_change`] для пользователя `user` по порядку (списки видов).
    pub fn mode_changes(user: Id) -> Vec<Vec<WorkKind>> {
        MODE_CHANGES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&user)
            .cloned()
            .unwrap_or_default()
    }
}
