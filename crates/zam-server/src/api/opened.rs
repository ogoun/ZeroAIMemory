//! «Агент открыл подсказку» (спецификация M7 `docs/specs/2026-11-21-m7-polish.md` §6.6 пп. 1–2,
//! решение координатора 50; признак `usage_opened`).
//!
//! Чтение цели — узла или записи журнала — **по токену** явной сессией S (`POST /read` — любой
//! уровень, секция, фокус; `GET /entries/{id}`; `GET /nodes/{id}` и `GET /nodes/by-path`) после
//! показа этой цели подсказкой `/recall` той же сессии **в той же эпохе** (`session_seen` с частью
//! `RECALL_PART` в базе сессии, [`zam_store::hint_shown`]) пишет строку `usage_opened` в базе цели
//! ([`zam_store::record_opened`]): `source` — событие последнего показа этой сессии из `usage`
//! ([`zam_store::open_source`]), пользователь — читавший. Одна строка на сессию и цель (первичный
//! ключ); уже записанное открытие проверяется чтением, повтор запись базы не берёт.
//!
//! Без сессии, с неявной сессией MCP, из сессии WebUI (человек), без показа и при показе в прежней
//! эпохе — ничего. Служебные `GET` самого MCP (поиск id заметки для `memory_write`) — не чтение
//! агентом: у них есть [`McpOrigin`]; чтение MCP — `memory_read` → `POST /read`. Чтения CLI в
//! сессии агента считаются все, в том числе служебные (ревизия для `zam write`, `zam insight
//! triggers|revisit|certify|append|supersede`): `zam` шлёт `X-Zam-Session` в каждом запросе, а
//! правка записи после её подсказки — тоже отклик на подсказку. Подсказка — и пакет разбора идей
//! (`idea_review`, `source = agent`): идея из очереди, прочитанная агентом, — открытие. Запись — вне
//! цепочки поиска и подсказок: ночная проверка (`Accounting::Off`) читает только ими и открытий не
//! пишет. Сбой учёта — WARN без id сессии и текста, ответ чтения не меняется.
use std::collections::HashSet;

use axum::http::HeaderMap;
use zam_core::{Id, TargetRef};
use zam_store::{Db, OpenedEvent, hint_shown, open_source, record_opened};

use super::sessions::{McpOrigin, RequestSession, seen_session};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

/// Прочитанная цель: владелец её базы, база и ссылка (узел или запись журнала).
pub struct ReadTarget {
    pub owner: Id,
    pub db: Db,
    pub target: TargetRef,
}

/// Открытия целей `targets`, прочитанных по токену `p` сессией `s` (явной, с текущей эпохой —
/// [`seen_session`]): цели, показанные сессии подсказкой в этой эпохе, получают строку
/// `usage_opened` в своей базе. Повтор цели в пачке — одна цель.
pub async fn record(p: &Principal, s: &RequestSession, targets: Vec<ReadTarget>) {
    let Some(user) = p.bound_user().filter(|_| !p.is_human()) else {
        return;
    };
    let mut uniq = HashSet::new();
    let targets: Vec<ReadTarget> = targets
        .into_iter()
        .filter(|t| uniq.insert((t.owner, t.target)))
        .collect();
    if targets.is_empty() {
        return;
    }
    let keys: Vec<(Id, TargetRef)> = targets.iter().map(|t| (t.owner, t.target)).collect();
    let shown: HashSet<(Id, TargetRef)> =
        match hint_shown(&s.db, &s.touch.id, s.touch.epoch, &keys).await {
            Ok(v) => v.into_iter().collect(),
            Err(e) => {
                tracing::warn!(error = %e, "открытие подсказки: показанное сессии не прочитано");
                return;
            }
        };
    for t in targets
        .iter()
        .filter(|t| shown.contains(&(t.owner, t.target)))
    {
        let source = match open_source(&t.db, &s.touch.id, t.target).await {
            Ok(Some(source)) => source,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "открытие подсказки: показ цели не прочитан");
                continue;
            }
        };
        let ev = OpenedEvent {
            ts: zam_store::time::now(),
            user_id: user,
            session_id: s.touch.id.clone(),
            target: t.target,
            source,
        };
        if let Err(e) = record_opened(&t.db, &ev).await {
            tracing::warn!(error = %e, "открытие подсказки не записано");
        }
    }
}

/// Открытие цели `GET /entries/{id}`, `GET /nodes/{id}` и `GET /nodes/by-path`: сессия —
/// `X-Zam-Session` запроса ([`seen_session`]); сессия WebUI и служебный запрос MCP (`origin`) —
/// ничего. Некорректный id сессии ответ чтения не меняет (раньше этот заголовок здесь не
/// читался) — DEBUG; сбой базы сессии — WARN.
pub async fn record_get(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    origin: Option<&McpOrigin>,
    t: ReadTarget,
) {
    if p.is_human() || origin.is_some() {
        return;
    }
    let s = match seen_session(st, p, headers, None, None).await {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e @ (ApiError::BadRequest(_) | ApiError::NotFound)) => {
            tracing::debug!(error = %e, "открытие подсказки: сессия запроса не принята");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "открытие подсказки: база сессии недоступна");
            return;
        }
    };
    record(p, &s, vec![t]).await;
}
