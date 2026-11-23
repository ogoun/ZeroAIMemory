//! Пауза исполнителя внутренних работ и аудит подключённой модели (M7.3, спецификация §6.2 пп. 1,
//! 5, 6).
//!
//! - **Пауза** (`works.pause`, пишет только сервер через [`super::settings::set_pause`]):
//!   `auth`, `not_found` — после ответа модели; `blocked`, `vault`, `vault_changed` — запрос не
//!   отправляется. Снимают человек сохранением подключения и успешная проверка подключения; у
//!   `vault_changed` — только сохранение **с подтверждением** хоста, который видел человек
//!   (`PATCH works.model {confirm_host}`, ревью r1 задачи 2, Important 1).
//! - **Аудит**: `works.model.paused {class}`, `works.model.vault_changed {from, to}`,
//!   `works.model.pause_cleared {class, by, from?, to?}`, `works.model.key_use {vault, host}`.
//!   Отказ записи аудита не глотается: WARN с действием и пользователем ([`audit`]); там, где без
//!   записи действие идти не должно (`key_use` перед обращением с ключом), вызывающий не идёт
//!   дальше.
//! - `actor`/`ip` — кто действовал: человек (`PATCH`, проверка подключения) или `system` (ночь,
//!   ручной запуск очереди).

use serde_json::{Value, json};
use zam_core::Id;

use super::model::CallClass;
use super::settings::{self, Pause, UserWorks};
use crate::app::AppState;

/// Автор действий сервера (ночь, исполнение пакета).
pub const SYSTEM: &str = "system";

/// Запись аудита пользователя `user`; отказ — WARN с действием и пользователем, итог —
/// записалось ли.
pub async fn audit(
    st: &AppState,
    actor: &str,
    ip: Option<&str>,
    user: Id,
    action: &str,
    details: Value,
) -> bool {
    let target = user.to_string();
    match st
        .system
        .audit(actor, Some(user), action, Some(&target), ip, details)
        .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                user = %user,
                action,
                error = %e,
                "внутренние работы: строка аудита не записана"
            );
            false
        }
    }
}

/// Ставит паузу исполнителя (§6.2 п. 5) с аудитом `works.model.paused {class}` (у
/// `vault_changed` — и `works.model.vault_changed {from, to}`). Сбой записи — WARN.
pub async fn set_paused(
    st: &AppState,
    user: Id,
    class: CallClass,
    change: Option<(String, String)>,
    actor: &str,
    ip: Option<&str>,
) {
    let (from, to) = change.clone().unzip();
    let pause = Pause {
        class: class.as_str().to_string(),
        since: zam_store::time::now(),
        from,
        to,
    };
    if let Err(e) = settings::set_pause(st, user, Some(pause)).await {
        tracing::warn!(user = %user, class = class.as_str(), error = %e, "внутренние работы: пауза не записана");
    }
    audit(
        st,
        actor,
        ip,
        user,
        "works.model.paused",
        json!({"class": class.as_str()}),
    )
    .await;
    if let Some((from, to)) = change {
        audit(
            st,
            actor,
            ip,
            user,
            "works.model.vault_changed",
            json!({"from": from, "to": to}),
        )
        .await;
    }
}

/// Снимает паузу исполнителя (если есть) с аудитом `works.model.pause_cleared {class, by,
/// from?, to?}`; `by` — `save` (сохранение подключения) или `test` (успешная проверка).
pub async fn clear_pause(
    st: &AppState,
    user: Id,
    works: &UserWorks,
    by: &str,
    actor: &str,
    ip: Option<&str>,
) {
    let Some(p) = &works.pause else {
        return;
    };
    if let Err(e) = settings::set_pause(st, user, None).await {
        tracing::warn!(user = %user, error = %e, "внутренние работы: пауза не снята");
        return;
    }
    let mut details = json!({"class": p.class, "by": by});
    if let (Some(from), Some(to)) = (&p.from, &p.to) {
        details["from"] = json!(from);
        details["to"] = json!(to);
    }
    audit(st, actor, ip, user, "works.model.pause_cleared", details).await;
}
