//! Ночная проверка памяти (M7.3 §6.4–§6.5): пробы, прогоны тем же путём, что у агента, вердикт.
//!
//! В задаче 1 плана — точки входа без тела (`Ok(())`): их уже зовут ночной исполнитель
//! ([`crate::works::NightRunner`]) и сборка состояния; тело — задача 5.

use jiff::{Timestamp, civil};
use zam_store::StoreError;

use crate::works::NightCtx;

/// Ночной прогон проверки ночи `night` до конца окна `until` — для **всех** пользователей, до
/// работ ночи (§6.5 п. 1); `[works] check = "nightly"` проверяет вызывающий, `works.check`
/// пользователя — эта функция.
pub async fn night(ctx: &NightCtx, night: civil::Date, until: Timestamp) -> Result<(), StoreError> {
    let _ = (ctx, night, until);
    Ok(())
}

/// Один раз после сборки состояния, при любом `[works] enabled` (§6.5 п. 1): прогоны `running`,
/// оборванные остановкой, — `failed` с `error = "interrupted"`.
pub async fn on_start(ctx: &NightCtx) -> Result<(), StoreError> {
    let _ = ctx;
    Ok(())
}
