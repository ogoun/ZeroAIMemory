//! Подключение к модели одного обращения и проверка подключения (M7.3, спецификация §6.2 пп. 1,
//! 6, 9).
//!
//! - [`prepare`] — перед каждым пакетом и проверкой: запись личного Vault подключения (удалена,
//!   перенесена, Vault заблокирован, нет значения, ключ не годится для заголовка — пауза
//!   `vault`), сверка её адреса с закреплённым (`pinned`; расхождение — пауза `vault_changed` и
//!   аудит `{from, to}`: токен, поправивший `data.url`, не уведёт пакеты и ключ), маршрут по
//!   политике `[works]` (запрет — пауза `blocked`; имя не разрешилось — `unavailable` без
//!   паузы), ключ.
//! - [`test_connection`] — `POST /works/model/test`: фиксированный запрос без данных памяти,
//!   строка журнала `kind = test`, размышления распознаются, успешная проверка снимает паузу
//!   (кроме `vault_changed`).

use std::time::Duration;

use serde_json::{Value, json};
use zam_core::Id;
use zam_store::work_calls;
use zeroize::Zeroizing;

use super::egress::{self, Policy, Route, RouteError};
use super::exec::{Meta, call, call_timeout, estimate, extract_json, fits_limits, journal, slot};
use super::model::{CallClass, CallError, ChatMessage, ModelConn};
use super::night::NightCtx;
use super::pause;
use super::settings::{self, ModelAuth, UserWorks};

/// Срок проверки подключения — не дольше этого (и не дольше `call_timeout_s`).
pub const TEST_TIMEOUT_MAX: Duration = Duration::from_secs(120);

/// Отказ до отправки: класс и пояснение человеку (причина запрета, «был → стал», что с записью
/// Vault).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub error: CallError,
    pub detail: Option<String>,
    /// У `vault_changed` — хосты «был → стал».
    pub change: Option<(String, String)>,
}

impl Refusal {
    pub(crate) fn new(class: CallClass, detail: impl Into<String>) -> Refusal {
        Refusal {
            error: CallError::new(class),
            detail: Some(detail.into()),
            change: None,
        }
    }
}

/// Внутренний сбой (хранилище, задача): запрос не отправлен, класс для вызывающего — `aborted`,
/// подробности — в журнал сервера.
pub(crate) fn internal(what: &str, e: impl std::fmt::Display) -> Refusal {
    tracing::warn!(error = %e, "внутренние работы: {what}");
    Refusal {
        error: CallError::new(CallClass::Aborted),
        detail: None,
        change: None,
    }
}

/// Подключение, готовое к обращениям: настройки, проверенный маршрут, ключ (у `auth = "key"`).
pub struct Prepared {
    pub conn: ModelConn,
    pub route: Route,
    /// Хост для журнала и аудита (без пути).
    pub host: String,
    key: Option<Zeroizing<String>>,
}

impl Prepared {
    /// У обращения есть ключ (`auth = "key"`).
    pub(crate) fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// Копия ключа для потока вызова (стирается при уничтожении).
    pub(crate) fn key_clone(&self) -> Option<Zeroizing<String>> {
        self.key.clone()
    }
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("host", &self.host)
            .field("external", &self.route.external)
            .field("key", &self.key.as_ref().map(|_| "‹secret›"))
            .finish_non_exhaustive()
    }
}

async fn refuse_paused(
    ctx: &NightCtx,
    user: Id,
    class: CallClass,
    detail: String,
    change: Option<(String, String)>,
    who: (&str, Option<&str>),
) -> Refusal {
    pause::set_paused(ctx.st(), user, class, change.clone(), who.0, who.1).await;
    Refusal {
        error: CallError::new(class),
        detail: Some(detail),
        change,
    }
}

/// Ключ для заголовка `Authorization: Bearer <ключ>`: без пробелов по краям; перевод строки,
/// управляющий символ или не-ASCII внутри — `None` (ревью r1 Minor 1: ошибка видна до отправки,
/// а не в каждом вызове).
fn header_key(plain: &str) -> Option<String> {
    let k = plain.trim();
    let ok = !k.is_empty()
        && ureq::http::HeaderValue::from_str(&format!("Bearer {k}")).is_ok()
        && k.bytes().all(|b| b.is_ascii_graphic());
    ok.then(|| k.to_string())
}

/// Подключение пользователя к обращению (§6.2 пп. 1, 6) — перед каждым пакетом и проверкой
/// (см. модуль). Пауза ставится здесь: запрос не отправляется. `who` — кто действует (аудит
/// паузы).
pub async fn prepare(
    ctx: &NightCtx,
    user: Id,
    works: &UserWorks,
    actor: &str,
    ip: Option<&str>,
) -> Result<Prepared, Refusal> {
    let st = ctx.st();
    let who = (actor, ip);
    let Some(conn) = works.model.clone() else {
        return Err(Refusal::new(
            CallClass::Blocked,
            "нет подключения works.model",
        ));
    };
    let db = st
        .users
        .get(user)
        .await
        .map_err(|e| internal("база пользователя", e))?;
    let (meta, blob) = match zam_store::vault_get(&db, conn.vault).await {
        Ok((m, b)) if m.scope == zam_store::VaultScope::Knowledge => (m, b),
        Ok(_) => {
            return Err(refuse_paused(
                ctx,
                user,
                CallClass::Vault,
                "запись Vault подключения перенесена из личного Vault".into(),
                None,
                who,
            )
            .await);
        }
        Err(zam_store::StoreError::NotFound) => {
            return Err(refuse_paused(
                ctx,
                user,
                CallClass::Vault,
                "записи Vault подключения нет (удалена)".into(),
                None,
                who,
            )
            .await);
        }
        Err(e) => return Err(internal("запись Vault", e)),
    };
    let current = egress::vault_address(&meta.data)
        .ok_or_else(|| "нет адреса".to_string())
        .and_then(egress::parse_base_url);
    let base = match (current, &conn.pinned) {
        (Ok(base), Some(pinned)) if base == *pinned => base,
        (current, pinned) => {
            let from = pinned
                .as_ref()
                .map_or_else(|| "не закреплён".to_string(), egress::address_label);
            let to = current
                .as_ref()
                .map_or_else(|_| "нет адреса".to_string(), egress::address_label);
            return Err(refuse_paused(
                ctx,
                user,
                CallClass::VaultChanged,
                format!(
                    "адрес подключения изменён: был {from} → стал {to} — подтвердите, сохранив \
                     подключение с confirm_url"
                ),
                Some((from, to)),
                who,
            )
            .await);
        }
    };
    let policy = Policy::of(&st.cfg.works);
    let b = base.clone();
    let route = tokio::task::spawn_blocking(move || egress::route(&b, policy))
        .await
        .map_err(|e| internal("разрешение имени", e))?;
    let route = match route {
        Ok(r) => r,
        Err(RouteError::Unresolved) => {
            return Err(Refusal::new(
                CallClass::Unavailable,
                format!("имя {} не разрешилось", base.host),
            ));
        }
        Err(RouteError::Blocked(reason)) => {
            return Err(
                refuse_paused(ctx, user, CallClass::Blocked, reason.message(), None, who).await,
            );
        }
    };
    let key = match conn.auth {
        ModelAuth::None => None,
        ModelAuth::Key if blob.is_empty() => {
            return Err(refuse_paused(
                ctx,
                user,
                CallClass::Vault,
                "у записи Vault подключения нет значения (ключа)".into(),
                None,
                who,
            )
            .await);
        }
        ModelAuth::Key => match st.vault.decrypt(user, meta.id, meta.rev, &blob).await {
            Ok(plain) => {
                let key = std::str::from_utf8(&plain).ok().and_then(header_key);
                match key {
                    Some(k) => Some(Zeroizing::new(k)),
                    None => {
                        return Err(refuse_paused(
                            ctx,
                            user,
                            CallClass::Vault,
                            "значение записи Vault не годится для заголовка Authorization: \
                             перевод строки, управляющий символ или не-ASCII"
                                .into(),
                            None,
                            who,
                        )
                        .await);
                    }
                }
            }
            Err(e) => {
                let detail = match e {
                    crate::error::ApiError::VaultLocked(_) => "Vault заблокирован",
                    _ => "значение записи Vault не расшифровалось",
                };
                return Err(
                    refuse_paused(ctx, user, CallClass::Vault, detail.into(), None, who).await,
                );
            }
        },
    };
    let host = egress::host_label(&route.base);
    Ok(Prepared {
        conn,
        route,
        host,
        key,
    })
}

/// «Был → стал» подключения сейчас (полные адреса, ревью r2 Minor 1, 4): закреплённый адрес и
/// текущий адрес записи Vault; запись не читается (удалена, сбой) — `None`.
pub(crate) async fn observed_change(
    st: &crate::app::AppState,
    user: Id,
    conn: &ModelConn,
) -> Option<(String, String)> {
    let db = st.users.get(user).await.ok()?;
    let (meta, _) = zam_store::vault_get(&db, conn.vault).await.ok()?;
    let now = egress::vault_address(&meta.data)
        .and_then(|a| egress::parse_base_url(a).ok())
        .map_or_else(|| "нет адреса".to_string(), |b| egress::address_label(&b));
    let was = conn
        .pinned
        .as_ref()
        .map_or_else(|| "не закреплён".to_string(), egress::address_label);
    Some((was, now))
}

/// Итог проверки подключения (§6.2 п. 9): `{ok, ms, class?, hint?, thinking?}`.
#[derive(Debug, Clone, PartialEq)]
pub struct TestOutcome {
    pub ok: bool,
    pub ms: Option<u64>,
    pub class: Option<CallClass>,
    pub hint: Option<String>,
    /// В ответе было рассуждение или предел съели размышления.
    pub thinking: bool,
}

impl TestOutcome {
    pub fn to_json(&self) -> Value {
        let mut v = json!({"ok": self.ok, "ms": self.ms});
        if let Some(c) = self.class {
            v["class"] = json!(c.as_str());
        }
        if let Some(h) = &self.hint {
            v["hint"] = json!(h);
        }
        if self.thinking {
            v["thinking"] = json!(true);
        }
        v
    }

    fn refused(r: Refusal) -> TestOutcome {
        TestOutcome {
            ok: false,
            ms: None,
            class: Some(r.error.class),
            hint: r
                .detail
                .or_else(|| r.error.class.hint().map(str::to_string)),
            thinking: false,
        }
    }
}

/// Почему проверка подключения не идёт (ответ не 200).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestRefusal {
    /// `[works] enabled = false` или `model_access = "off"` — 409.
    Disabled(&'static str),
    /// Нет подключения `works.model` — 422.
    NoConnection,
    /// Пауза `vault_changed` (или адрес изменён сейчас) — 409 «был → стал».
    VaultChanged { was: String, now: String },
    /// У пользователя идёт обращение к модели — 409.
    Busy,
}

/// Системное и пользовательское сообщения проверки подключения: без данных памяти.
pub const TEST_SYSTEM: &str = "Ты — проверка подключения. Ответь одним JSON-объектом.";
pub const TEST_USER: &str = "ответь JSON {\"ok\": true}";

/// Проверка подключения (§6.2 п. 9): фиксированный запрос без данных памяти; строка журнала
/// `kind = test`; на паузе `vault_changed` (и при изменённом адресе) — без обращения;
/// распознаёт размышления и советует `body_extra`; успешная — снимает паузу (кроме
/// `vault_changed`). Сбой с ответом (`auth`, …) — `ok: false` с классом и подсказкой (у `auth`,
/// `not_found` — и пауза). `actor`, `ip` — кто проверяет (аудит).
pub async fn test_connection(
    ctx: &NightCtx,
    user: Id,
    actor: &str,
    ip: Option<&str>,
) -> Result<TestOutcome, TestRefusal> {
    let st = ctx.st();
    let cfg = &st.cfg.works;
    if !cfg.enabled {
        return Err(TestRefusal::Disabled(
            "внутренние работы выключены ([works] enabled = false)",
        ));
    }
    if cfg.model_access == crate::config::ModelAccess::Off {
        return Err(TestRefusal::Disabled(
            "обращения к модели выключены ([works] model_access = \"off\")",
        ));
    }
    let lock = st.model_calls.user(user);
    let Ok(_guard) = lock.try_lock_owned() else {
        return Err(TestRefusal::Busy);
    };
    let works = match settings::load(&st.system, user).await {
        Ok(w) => w,
        Err(e) => return Ok(TestOutcome::refused(internal("настройки исполнителя", e))),
    };
    if works.model.is_none() {
        return Err(TestRefusal::NoConnection);
    }
    if let Some(p) = works
        .pause
        .as_ref()
        .filter(|p| p.class == CallClass::VaultChanged.as_str())
    {
        // «стал» — актуальный: адрес сменили ещё раз — пауза обновляется (ревью r2 Minor 1)
        let observed = match &works.model {
            Some(c) => observed_change(st, user, c).await,
            None => None,
        };
        let (was, now) = match observed {
            Some((was, now)) => {
                if p.to.as_deref() != Some(now.as_str()) {
                    pause::set_paused(
                        st,
                        user,
                        CallClass::VaultChanged,
                        Some((was.clone(), now.clone())),
                        actor,
                        ip,
                    )
                    .await;
                }
                (was, now)
            }
            None => (
                p.from.clone().unwrap_or_default(),
                p.to.clone().unwrap_or_default(),
            ),
        };
        return Err(TestRefusal::VaultChanged { was, now });
    }
    let p = match prepare(ctx, user, &works, actor, ip).await {
        Ok(p) => p,
        Err(Refusal {
            change: Some((was, now)),
            ..
        }) => return Err(TestRefusal::VaultChanged { was, now }),
        Err(r) => return Ok(TestOutcome::refused(r)),
    };
    let db = match st.users.get(user).await {
        Ok(db) => db,
        Err(e) => return Ok(TestOutcome::refused(internal("база пользователя", e))),
    };
    let convo = [
        ChatMessage::system(TEST_SYSTEM),
        ChatMessage::user(TEST_USER),
    ];
    let meta = Meta {
        kind: work_calls::KIND_TEST,
        work: None,
        items: None,
        run: None,
    };
    match fits_limits(
        &db,
        &works,
        cfg,
        zam_store::time::now(),
        estimate(&p.conn, &convo),
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            return Ok(TestOutcome::refused(Refusal::new(
                CallClass::Limit,
                "проверка не помещается в остаток токенов суток или месяца (works.limits, \
                 [works] max_daily_tokens, max_monthly_tokens)",
            )));
        }
        Err(e) => return Ok(TestOutcome::refused(internal("расход токенов", e))),
    }
    let Some(_permit) = slot(ctx).await else {
        return Ok(TestOutcome::refused(Refusal::new(
            CallClass::Aborted,
            "сервер останавливается",
        )));
    };
    if p.has_key()
        && !pause::audit(
            st,
            actor,
            ip,
            user,
            "works.model.key_use",
            json!({"vault": format!("v:{}", p.conn.vault), "host": p.host, "test": true}),
        )
        .await
    {
        // ключ без следа в аудите наружу не уходит
        return Ok(TestOutcome::refused(Refusal::new(
            CallClass::Aborted,
            "аудит обращения не записался — проверка не отправлена",
        )));
    }
    let timeout = call_timeout(cfg).min(TEST_TIMEOUT_MAX);
    let called = call(ctx, &p, &convo, timeout).await;
    journal(&db, called.row(&p, &meta)).await;
    let ms = called.ms;
    let out = match called.result {
        Ok(reply) => {
            let parsed = extract_json(&reply.text);
            match parsed {
                Ok(_) => {
                    pause::clear_pause(st, user, &works, "test", actor, ip).await;
                    TestOutcome {
                        ok: true,
                        ms,
                        class: None,
                        hint: reply.reasoning.then(|| {
                            "у модели включены размышления: для коротких работ выключите их в \
                             body_extra (например, {\"chat_template_kwargs\": {\"enable_thinking\": \
                             false}}) — меньше токенов и времени"
                                .to_string()
                        }),
                        thinking: reply.reasoning,
                    }
                }
                Err(_) => TestOutcome {
                    ok: false,
                    ms,
                    class: Some(CallClass::BadJson),
                    hint: Some(
                        "модель ответила не JSON: для работ задайте в body_extra \
                         {\"response_format\": {\"type\": \"json_object\"}}, если поставщик его \
                         понимает"
                            .into(),
                    ),
                    thinking: reply.reasoning,
                },
            }
        }
        Err(e) => {
            if matches!(e.class, CallClass::Auth | CallClass::NotFound) {
                pause::set_paused(st, user, e.class, None, actor, ip).await;
            }
            TestOutcome {
                ok: false,
                ms,
                class: Some(e.class),
                hint: e.class.hint().map(str::to_string),
                thinking: e.class == CallClass::Thinking,
            }
        }
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ключ для заголовка: края обрезаются, перевод строки и управляющие символы внутри — нет.
    #[test]
    fn header_key_cases() {
        let k = format!("{}{}", "sk-", "abc123");
        assert_eq!(header_key(&format!("  {k}\n")), Some(k.clone()));
        for bad in [
            format!("{}\n{}", "sk-a", "b"),
            format!("{}\t{}", "sk-a", "b"),
            format!("{}\u{7}", "sk-a"),
            "ключ".to_string(),
            "   ".to_string(),
            String::new(),
        ] {
            assert_eq!(header_key(&bad), None, "{bad:?}");
        }
    }
}
