//! Очередь внутренних работ (M7.3 §6.3): постановка ночи и ручного запуска, выполнение работ
//! моделью, общая проверка итога, снятие открытых работ при смене режима, возврат работ модели в
//! очередь при старте, строка дайджеста.
//!
//! - **Ночь** ([`night`], §6.3 п. 2): после проверки всех пользователей — по пользователям и видам
//!   в порядке [`WorkKind::ALL`] (пробы, затем кандидаты в Rules). Действующий режим вида
//!   `agent` — сборщик вида ставит до [`PACKAGES_PER_RUN`] работ на пользователя за ночь, по
//!   одной на пространство (пространства — с давно не обслуженного, `max(finished_at)` вида);
//!   работа ждёт агента. Режим `model` — работы выполняются сразу, по одной, до конца окна или
//!   стопа: сначала возвращённые в очередь работы модели (остановка, стоп прошлого запуска), затем
//!   новые, пространства — по кругу; предел — [`PACKAGES_PER_RUN`] пакетов вида на пользователя
//!   за ночь (`origin = nightly`). Работы вида, чей исполнитель не совпал с действующим режимом
//!   (снятие при смене режима могло не пройти, `model_access` сменил администратор), снимаются
//!   (`mode_changed`); на паузе исполнителя работы модели ждут в очереди.
//! - **Ручной запуск** (`POST /works/run`): та же постановка с `origin = manual` и своим пределом
//!   — [`PACKAGES_PER_RUN`] пакетов вида за сутки UTC; режим `model` — выполнение сразу, вне окна
//!   ([`manual_model`], учтённым запуском раннера), с теми же пределами токенов, паузой и журналом.
//! - **Выполнение моделью** ([`run_model`]): пакет [`super::package::render_numbered`] →
//!   [`super::exec::run_payload`] → разбор JSON → **та же** проверка итога, что у агента
//!   ([`accept`]) → те же записи; автор — `model:<имя модели>`. Действующий режим вида
//!   сверяется перед каждой работой (и `run_payload` — перед каждым обращением): не `model` —
//!   работа `cancelled` (`mode_changed`); пауза, стоп до следующей ночи (`quota`, `limit`,
//!   `rate`, `unavailable`) и остановка (`aborted`, в том числе сбой хранилища) — работа снова
//!   `queued`, запуск вида заканчивается; прочие классы — работа `failed`, запуск идёт дальше.
//! - **Итог** ([`accept`], §6.3 п. 4): элемент — номер `n` или ссылка `target` элемента пакета
//!   (оба — должны совпасть); маска секрета в любой строке элемента — отказ; `{n, skip}` — явный
//!   отказ исполнителя (`result.skip`); проверка вида ([`super::kinds::accept_item`]; повтор
//!   номера — только у вида с несколькими элементами на запись, [`super::kinds::repeats`]);
//!   элементы пакета без итога — `skipped`; элементы, выброшенные вторым сканом, — отклонены с
//!   `secret_in_payload`. `skip` без элементов — работа `cancelled`: сборщик поставит её снова
//!   следующей ночью.
//! - **Предел записей вида** ([`super::kinds::room`]; у `probes` — остаток пределов проб):
//!   постановка берёт кандидатов не больше остатка, исчерпан — запуск вида кончен.
//!
//! Правила [`NightCtx`] (задача 1): признак остановки — только `ctx.stopping()`; клон контекста в
//! `tokio::spawn` не уносится — ручной запуск модели идёт через `NightRunner::spawn_tracked`.

use std::collections::HashSet;

use jiff::{SignedDuration, Timestamp, civil};
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::works::{
    self as store, NewWork, Origin, Work, WorkItem, WorkKind, WorkMode, WorkState,
};
use zam_store::{Db, StoreError};

use super::exec::{self, RunKey};
use super::kinds::{self, Author, Candidate, ItemCtx, Reject};
use super::model::CallClass;
use super::night::NightCtx;
use super::package;
use super::settings::{self, OFF_PAUSED, UserWorks};
use crate::agent_setup::Lang;
use crate::config::WorksMode;

/// Аренда захвата работы агентом (§6.3 п. 1).
pub const LEASE: SignedDuration = SignedDuration::from_hours(2);
/// Пакетов вида на пользователя за ночь (`origin = nightly`) и за сутки UTC ручных запусков
/// (`origin = manual`; §6.3 п. 2, решение координатора 44).
pub const PACKAGES_PER_RUN: u32 = 4;
/// Строка дайджеста (§6.3 п. 5): `{list}` — «пробы 1, правила 1».
pub const WORKS_DIGEST_LINE_RU: &str =
    "Внутренние работы ждут: {list} — memory_recall event=work, когда закончишь текущий шаг.";
/// Строка дайджеста, en.
pub const WORKS_DIGEST_LINE_EN: &str =
    "Internal works are waiting: {list} — memory_recall event=work once the current step is done.";

/// Пространства вида `kind` в базе пользователя по порядку обхода: с давно не обслуженного
/// (`max(finished_at)` вида; не обслуженные — первыми), затем по id; `only` — одно пространство.
pub async fn kind_spaces(db: &Db, kind: WorkKind, only: Option<Id>) -> Result<Vec<Id>, StoreError> {
    let allowed = kinds::space_kinds(kind);
    let mut spaces: Vec<Id> = zam_store::list_spaces(db)
        .await?
        .into_iter()
        .filter(|s| allowed.contains(&s.kind))
        .map(|s| s.id)
        .filter(|id| only.is_none_or(|o| o == *id))
        .collect();
    let served = store::served_at(db, kind).await?;
    spaces.sort_by_key(|s| (served.get(s).copied(), *s));
    Ok(spaces)
}

/// Кандидаты — элементы работы с номерами 1, 2, ….
fn numbered(cands: Vec<Candidate>) -> Vec<WorkItem> {
    cands
        .into_iter()
        .enumerate()
        .map(|(i, c)| c.into_item(i + 1))
        .collect()
}

/// Незакрытые работы вида, чей исполнитель не совпал с действующим режимом `mode` (причина
/// `why`), — `cancelled` (`mode_changed`); на паузе исполнителя (выбран `model`) ждут только
/// работы модели, работы агента снимаются.
async fn drop_stale(
    db: &Db,
    kind: WorkKind,
    mode: WorksMode,
    why: Option<&str>,
) -> Result<u64, StoreError> {
    let other = match mode {
        WorksMode::Agent => Some(WorkMode::Model),
        WorksMode::Model => Some(WorkMode::Agent),
        WorksMode::Off if why == Some(OFF_PAUSED) => Some(WorkMode::Agent),
        WorksMode::Off => None,
    };
    store::cancel_kind(db, kind, store::MODE_CHANGED, other).await
}

/// Постановка и выполнение работ ночи `night` до конца окна `until` по всем пользователям
/// (§6.3 п. 2) — после проверки всех пользователей ([`crate::check::night`]). Сбой пользователя —
/// WARN, ночь идёт дальше.
pub async fn night(ctx: &NightCtx, night: civil::Date, until: Timestamp) -> Result<(), StoreError> {
    let users = ctx.st().system.list_users().await?;
    for u in users {
        if ctx.stopping() || zam_store::time::now() >= until {
            break;
        }
        if let Err(e) = night_user(ctx, u.id, night, until).await {
            tracing::warn!(user = %u.id, %night, error = %e, "внутренние работы: ночь пользователя не завершилась");
        }
    }
    Ok(())
}

/// Ночь одного пользователя: сроки работ агента, затем виды по порядку.
async fn night_user(
    ctx: &NightCtx,
    user: Id,
    night: civil::Date,
    until: Timestamp,
) -> Result<(), StoreError> {
    let st = ctx.st();
    let cfg = &st.cfg.works;
    let works = settings::load(&st.system, user).await?;
    let db = st.users.get(user).await?;
    store::expire(&db, zam_store::time::now()).await?;
    let since = crate::schedule::slot(night, cfg.at_hm(), &cfg.time_zone());
    for &kind in WorkKind::ALL {
        if ctx.stopping() {
            break;
        }
        let (mode, why) = settings::effective_mode(kind, &works, cfg);
        drop_stale(&db, kind, mode, why).await?;
        let plan = Plan {
            user,
            kind,
            origin: Origin::Nightly,
            night: Some(night),
            since,
            only: None,
            run: RunKey::Night(night),
            until: Some(until),
        };
        match mode {
            WorksMode::Off => {}
            WorksMode::Agent => {
                agent_batch(ctx, &db, &plan).await?;
            }
            WorksMode::Model => run_model(ctx, &db, &plan).await?,
        }
    }
    Ok(())
}

/// Запуск вида: ночь или ручной запуск.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub user: Id,
    pub kind: WorkKind,
    pub origin: Origin,
    /// Ночь ночной постановки.
    pub night: Option<civil::Date>,
    /// Начало счёта предела [`PACKAGES_PER_RUN`]: начало окна ночи или суток UTC.
    pub since: Timestamp,
    /// Только это пространство (ручной запуск с `space`).
    pub only: Option<Id>,
    /// Запуск строк журнала отправок (стоп и аудит `key_use` — по нему).
    pub run: RunKey,
    /// Конец окна ночи (у ручного запуска — нет).
    pub until: Option<Timestamp>,
}

impl Plan {
    /// Ручной запуск вида `kind` пользователя `user` (id запуска `run` — один на весь запуск).
    pub fn manual(user: Id, kind: WorkKind, only: Option<Id>, run: Id) -> Plan {
        Plan {
            user,
            kind,
            origin: Origin::Manual,
            night: None,
            since: zam_store::time::day_start_utc(zam_store::time::now()),
            only,
            run: RunKey::Manual(run),
            until: None,
        }
    }
}

/// Сколько пакетов вида ещё можно поставить в запуске `plan` (предел [`PACKAGES_PER_RUN`]).
pub async fn packages_left(db: &Db, plan: &Plan) -> Result<u32, StoreError> {
    let done = store::count_packages(db, plan.kind, plan.origin, plan.since).await?;
    Ok(PACKAGES_PER_RUN.saturating_sub(done))
}

/// Постановка работ агента запуска `plan`: по одной на пространство (у ночи — если в этом
/// пространстве ночной постановки этой ночи ещё не было), до предела пакетов; пустой сборщик —
/// работы нет. Поставленные работы.
pub async fn agent_batch(ctx: &NightCtx, db: &Db, plan: &Plan) -> Result<Vec<Work>, StoreError> {
    let mut left = packages_left(db, plan).await?;
    let done_spaces = match plan.night {
        Some(n) if plan.origin == Origin::Nightly => {
            store::nightly_spaces(db, plan.kind, n).await?
        }
        _ => HashSet::new(),
    };
    let batch = ctx.st().cfg.works.batch_records as usize;
    let mut out = Vec::new();
    for space in kind_spaces(db, plan.kind, plan.only).await? {
        if left == 0 || ctx.stopping() {
            break;
        }
        if done_spaces.contains(&space) {
            continue;
        }
        // предел записей вида (у `probes` — остаток пределов проб): исчерпан — постановка кончена
        let take = match kinds::room(plan.kind, ctx, db, plan.origin).await? {
            Some(0) => break,
            Some(r) => r.min(batch),
            None => batch,
        };
        let cands = kinds::collect(plan.kind, ctx, plan.user, space, take).await?;
        if cands.is_empty() {
            continue;
        }
        let w = store::enqueue(
            db,
            NewWork {
                kind: plan.kind,
                space,
                mode: WorkMode::Agent,
                origin: plan.origin,
                night: plan.night,
                items: numbered(cands),
            },
        )
        .await?;
        if let Some(w) = w {
            out.push(w);
            left -= 1;
        }
    }
    Ok(out)
}

/// Ход запуска модели после работы.
enum Step {
    /// Дальше — следующая работа.
    Next,
    /// Пространство в этом запуске больше не обходится (отказ `skip`).
    DropSpace,
    /// Запуск вида закончен (пауза, стоп, остановка, смена режима).
    Stop,
}

/// Выполнение работ модели запуска `plan` (ночь или ручной запуск; §6.3 пп. 2, 7): по одной, до
/// конца окна, предела пакетов или стопа. Сбой хранилища — ошибка (работа, если начата, при
/// следующем старте снова `queued`).
pub async fn run_model(ctx: &NightCtx, db: &Db, plan: &Plan) -> Result<(), StoreError> {
    let st = ctx.st();
    let batch = st.cfg.works.batch_records as usize;
    let mut dropped: HashSet<Id> = HashSet::new();
    loop {
        if ctx.stopping() || plan.until.is_some_and(|u| zam_store::time::now() >= u) {
            return Ok(());
        }
        // действующий режим — перед каждой работой (run_payload сверяет и перед обращением)
        let works = settings::load(&st.system, plan.user).await?;
        if settings::effective_mode(plan.kind, &works, &st.cfg.works).0 != WorksMode::Model {
            return Ok(());
        }
        let spaces: Vec<Id> = kind_spaces(db, plan.kind, plan.only)
            .await?
            .into_iter()
            .filter(|s| !dropped.contains(s))
            .collect();
        let Some(&first) = spaces.first() else {
            return Ok(());
        };
        let w = match store::next_model_queued(db, plan.kind, &spaces).await? {
            Some(w) => w,
            None => {
                if packages_left(db, plan).await? == 0 {
                    return Ok(());
                }
                // предел записей вида (у `probes` — остаток пределов проб): исчерпан — запуск
                // вида кончен, обращений к модели больше нет
                let take = match kinds::room(plan.kind, ctx, db, plan.origin).await? {
                    Some(0) => return Ok(()),
                    Some(r) => r.min(batch),
                    None => batch,
                };
                let cands = kinds::collect(plan.kind, ctx, plan.user, first, take).await?;
                if cands.is_empty() {
                    dropped.insert(first);
                    continue;
                }
                let new = NewWork {
                    kind: plan.kind,
                    space: first,
                    mode: WorkMode::Model,
                    origin: plan.origin,
                    night: plan.night,
                    items: numbered(cands),
                };
                match store::enqueue(db, new).await? {
                    Some(w) => w,
                    None => {
                        dropped.insert(first);
                        continue;
                    }
                }
            }
        };
        let space = w.space_id;
        match execute(ctx, db, plan, &works, w).await? {
            Step::Next => {}
            Step::DropSpace => {
                dropped.insert(space);
            }
            Step::Stop => return Ok(()),
        }
    }
}

/// Пакет работы к выдаче (агенту или модели) — одна подготовка для обоих: снимок кандидатов
/// сверяется с базой (удалённые и исправленные записи — `zam_store::works::live_refs` — не
/// идут), пакет строится в бюджете `budget`, хвост сверх него и устаревшие элементы снимаются с
/// работы (`set_items`). Живых кандидатов нет — `None`: вызывающий снимает работу
/// ([`store::STALE`]).
pub struct Prepared {
    /// Работа с кандидатами, вошедшими в пакет.
    pub work: Work,
    /// Markdown пакета агенту.
    pub text: String,
    /// Запрос модели.
    pub payload: super::exec::Payload,
}

/// [`Prepared`] работы `w`.
pub async fn prepare(db: &Db, mut w: Work, budget: usize) -> Result<Option<Prepared>, StoreError> {
    let refs: Vec<(String, i64)> = w.items.iter().map(|i| (i.target.clone(), i.rev)).collect();
    let live = store::live_refs(db, &refs).await?;
    let before = w.items.len();
    w.items
        .retain(|i| live.contains(&(i.target.clone(), i.rev)));
    if w.items.is_empty() {
        return Ok(None);
    }
    let cands: Vec<(usize, Candidate)> = w
        .items
        .iter()
        .map(|i| (i.n, Candidate::from_item(i)))
        .collect();
    let (text, payload, rest) = package::render_numbered(&w, &cands, w.kind, Lang::Ru, budget);
    if !rest.is_empty() {
        // хвост сверх бюджета — в следующую работу
        let keep: HashSet<usize> = payload.items.iter().map(|i| i.n).collect();
        w.items.retain(|i| keep.contains(&i.n));
    }
    if w.items.len() != before {
        store::set_items(db, w.id, w.items.clone()).await?;
    }
    Ok(Some(Prepared {
        work: w,
        text,
        payload,
    }))
}

/// Одна работа модели: пакет, обращение, та же проверка итога, что у агента; записи принятого —
/// одной транзакцией с закрытием работы.
async fn execute(
    ctx: &NightCtx,
    db: &Db,
    plan: &Plan,
    works: &UserWorks,
    w: Work,
) -> Result<Step, StoreError> {
    let st = ctx.st();
    if !store::start_model(db, w.id).await? {
        return Ok(Step::Next);
    }
    let id = w.id;
    let Some(Prepared {
        work: w, payload, ..
    }) = prepare(db, w, package::PACKAGE_BUDGET).await?
    else {
        // все кандидаты удалены или исправлены — сборщик предложит новые ревизии
        store::finish(
            db,
            id,
            WorkState::Cancelled,
            Value::Null,
            Some(store::STALE),
        )
        .await?;
        return Ok(Step::Next);
    };
    let model_name = works
        .model
        .as_ref()
        .map(|m| m.model.clone())
        .unwrap_or_default();
    match exec::run_payload(ctx, plan.user, plan.run, payload, plan.kind, Some(w.id)).await {
        Ok(out) => {
            let author = Author::Model(model_name);
            let ictx = ItemCtx {
                st,
                user: plan.user,
                db,
                work: &w,
                author: &author,
            };
            let data = if out.json.is_null() {
                json!({"items": []})
            } else {
                out.json
            };
            let a = match accept(&ictx, &data, &out.dropped).await {
                Ok(a) => a,
                // ответ — JSON, но не по схеме итога
                Err(e) => {
                    let class = CallClass::BadJson.as_str();
                    let closed = store::finish(
                        db,
                        w.id,
                        WorkState::Failed,
                        json!({"error": e}),
                        Some(class),
                    )
                    .await?;
                    if !closed {
                        tracing::warn!(work = %w.id, "внутренние работы: работа модели уже закрыта другим путём");
                    }
                    return Ok(Step::Next);
                }
            };
            let state = a.state;
            match store::finish_claimed(db, w.id, None, state, a.result, None, a.writes).await? {
                store::Closed::Closed => {
                    tracing::debug!(work = %w.id, state = state.as_str(), "внутренние работы: работа модели закрыта");
                }
                // работу сняли или вернули, пока шло обращение: итог и записи не приняты
                store::Closed::Replay(_) | store::Closed::NotClaimed(_) => {
                    tracing::warn!(work = %w.id, "внутренние работы: итог модели не принят — работа уже не выполняется");
                    return Ok(Step::Next);
                }
            }
            Ok(if state == WorkState::Cancelled {
                Step::DropSpace
            } else {
                Step::Next
            })
        }
        Err(e) => {
            tracing::info!(work = %w.id, class = e.class.as_str(), "внутренние работы: обращение к модели не дало итога");
            failed_call(st, db, plan, &w, e.class).await
        }
    }
}

/// Итог неудачного обращения (§6.2 п. 5): режим не `model` без паузы — работа `cancelled`
/// (`mode_changed`); пауза, стоп до следующей ночи, остановка и сбой хранилища (`aborted`) —
/// снова `queued`, запуск вида закончен; находка в постоянном тексте пакета — `failed`, запуск
/// закончен; прочие (`bad_request`, `thinking`, `max_tokens`, `refused`, `bad_response`,
/// `bad_json`) — `failed`, запуск идёт дальше.
async fn failed_call(
    st: &crate::app::AppState,
    db: &Db,
    plan: &Plan,
    w: &Work,
    class: CallClass,
) -> Result<Step, StoreError> {
    match class {
        CallClass::Blocked => {
            let paused = settings::load(&st.system, plan.user).await?.pause.is_some();
            if paused {
                store::release(db, w.id).await?;
            } else {
                store::finish(
                    db,
                    w.id,
                    WorkState::Cancelled,
                    Value::Null,
                    Some(store::MODE_CHANGED),
                )
                .await?;
            }
            Ok(Step::Stop)
        }
        CallClass::Aborted
        | CallClass::Quota
        | CallClass::Limit
        | CallClass::Rate
        | CallClass::Unavailable
        | CallClass::Auth
        | CallClass::NotFound
        | CallClass::Vault
        | CallClass::VaultChanged => {
            store::release(db, w.id).await?;
            Ok(Step::Stop)
        }
        CallClass::SecretInPayload => {
            store::finish(
                db,
                w.id,
                WorkState::Failed,
                Value::Null,
                Some(class.as_str()),
            )
            .await?;
            Ok(Step::Stop)
        }
        _ => {
            store::finish(
                db,
                w.id,
                WorkState::Failed,
                Value::Null,
                Some(class.as_str()),
            )
            .await?;
            Ok(Step::Next)
        }
    }
}

/// Ручной запуск в режиме агента: постановка сразу (работы ждут агента).
pub async fn manual_agent(ctx: &NightCtx, plan: &Plan) -> Result<Vec<Work>, StoreError> {
    let db = ctx.st().users.get(plan.user).await?;
    store::expire(&db, zam_store::time::now()).await?;
    agent_batch(ctx, &db, plan).await
}

/// Ручной запуск в режиме модели — тело учтённого запуска раннера (`spawn_tracked`): постановка
/// и выполнение сразу, вне окна; сбой — WARN.
pub async fn manual_model(ctx: NightCtx, plan: Plan) {
    let run = async {
        let db = ctx.st().users.get(plan.user).await?;
        run_model(&ctx, &db, &plan).await
    };
    if let Err(e) = run.await {
        tracing::warn!(user = %plan.user, kind = plan.kind.as_str(), error = %e, "внутренние работы: ручной запуск не завершился");
    }
}

/// Итог работы после проверки: состояние (`done` или `cancelled`), JSON `result` (с ответом
/// исполнителю `result.response`), сам ответ и записи принятых элементов — их пишет закрытие
/// работы (`store::finish_claimed`) в той же транзакции.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub state: WorkState,
    pub result: Value,
    pub response: Value,
    pub writes: Vec<store::ItemWrite>,
}

/// Общая проверка итога агента и модели (§6.3 пп. 4, 7) для работы `ctx.work`: `data` —
/// `{items: [{n | target, …}], skip?}`; `dropped` — номера, выброшенные вторым сканом. `Err` —
/// итог не по схеме (не объект, `items` не массив, `skip` не строка). Ничего не пишет: записи
/// принятого — в [`Accepted::writes`].
pub async fn accept(
    ctx: &ItemCtx<'_>,
    data: &Value,
    dropped: &[usize],
) -> Result<Accepted, String> {
    let w = ctx.work;
    let Some(obj) = data.as_object() else {
        return Err("итог — JSON-объект {items, skip?}".into());
    };
    let items: &[Value] = match obj.get("items") {
        None | Some(Value::Null) => &[],
        Some(Value::Array(a)) => a,
        Some(_) => return Err("items — массив элементов {n, …}".into()),
    };
    let skip_reason = match obj.get("skip") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => return Err("skip — строка с причиной отказа".into()),
    };
    let reject = |n: Option<usize>, it: Option<&WorkItem>, reason: &str| {
        json!({
            "n": n,
            "target": it.map(|i| i.target.clone()),
            "rev": it.map(|i| i.rev),
            "reason": reason,
        })
    };
    let (mut accepted, mut rejected, mut skip) = (Vec::new(), Vec::new(), Vec::new());
    let mut writes: Vec<store::ItemWrite> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    for item in items {
        // элемент — номер `n` или ссылка `target` элемента пакета (задача 4: схема `probes`)
        let target = item
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let n = item
            .get("n")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .or_else(|| target.and_then(|t| w.items.iter().find(|i| i.target == t).map(|i| i.n)));
        let Some(n) = n else {
            let why = if target.is_some() {
                "target — не запись пакета: ref элемента"
            } else {
                "нет n или target: номер или ref элемента пакета"
            };
            rejected.push(reject(None, None, why));
            continue;
        };
        let Some(it) = w.item(n) else {
            rejected.push(reject(Some(n), None, "нет элемента с таким n в пакете"));
            continue;
        };
        if dropped.contains(&n) {
            // элемент не уходил исполнителю — его итог не принимается
            continue;
        }
        if target.is_some_and(|t| t != it.target) {
            rejected.push(reject(
                Some(n),
                Some(it),
                "target — не запись элемента n пакета",
            ));
            continue;
        }
        if !seen.insert(n) && !kinds::repeats(w.kind) {
            rejected.push(reject(Some(n), Some(it), "элемент n повторяется"));
            continue;
        }
        if let Err(r) = package::check_mask_literal_json(item) {
            rejected.push(reject(Some(n), Some(it), &r.reason));
            continue;
        }
        if let Some(why) = item.get("skip").filter(|v| !v.is_null()) {
            let why = why.as_str().unwrap_or("").trim();
            if why.is_empty() {
                rejected.push(reject(Some(n), Some(it), "skip — строка с причиной"));
            } else {
                skip.push(json!({"n": n, "target": it.target, "rev": it.rev, "why": why}));
            }
            continue;
        }
        match kinds::accept_item(w.kind, ctx, n, item, &writes).await {
            Ok(rows) => {
                writes.extend(rows);
                accepted.push(json!({"n": n, "target": it.target, "rev": it.rev, "item": item}));
            }
            Err(Reject { reason }) => rejected.push(reject(Some(n), Some(it), &reason)),
        }
    }
    for &n in dropped {
        if let Some(it) = w.item(n) {
            rejected.push(reject(Some(n), Some(it), store::SECRET_IN_PAYLOAD));
            seen.insert(n);
        }
    }
    let skipped: Vec<Value> = w
        .items
        .iter()
        .filter(|i| !seen.contains(&i.n))
        .map(|i| json!({"n": i.n, "target": i.target, "rev": i.rev}))
        .collect();
    let state = if items.is_empty() && skip_reason.is_some() {
        WorkState::Cancelled
    } else {
        WorkState::Done
    };
    let response = json!({
        "work": format!("j:{}", w.id),
        "state": state.as_str(),
        "accepted": accepted.iter().map(|a| a["n"].clone()).collect::<Vec<_>>(),
        "rejected": rejected
            .iter()
            .map(|r| json!({"n": r["n"], "target": r["target"], "reason": r["reason"]}))
            .collect::<Vec<_>>(),
        "skipped": skipped.iter().map(|s| s["n"].clone()).collect::<Vec<_>>(),
        "skip": skip.iter().map(|s| json!({"n": s["n"], "target": s["target"], "why": s["why"]})).collect::<Vec<_>>(),
        "skip_reason": skip_reason,
    });
    let result = json!({
        "accepted": accepted,
        "rejected": rejected,
        "skip": skip,
        "skipped": skipped,
        "skip_reason": skip_reason,
        "author_kind": ctx.author.kind(),
        "author": ctx.author.label(),
        "response": response,
    });
    Ok(Accepted {
        state,
        result,
        response,
        writes,
    })
}

/// Действующий режим вида сменился (`PATCH` настроек, §6.1 п. 2): открытые работы видов `kinds`,
/// чей исполнитель не совпал с новым режимом, снимаются (`cancelled`, `mode_changed`; на паузе
/// исполнителя ждут только работы модели).
pub async fn on_mode_change(
    ctx: &NightCtx,
    user: Id,
    kinds: &[WorkKind],
) -> Result<(), StoreError> {
    #[cfg(feature = "test-hooks")]
    test_hooks::record(user, kinds);
    if kinds.is_empty() {
        return Ok(());
    }
    let st = ctx.st();
    let works = settings::load(&st.system, user).await?;
    let db = st.users.get(user).await?;
    for &kind in kinds {
        let (mode, why) = settings::effective_mode(kind, &works, &st.cfg.works);
        drop_stale(&db, kind, mode, why).await?;
    }
    Ok(())
}

/// Один раз после сборки состояния, при любом `[works] enabled`, в фоне задачи раннера (§6.1 п. 5;
/// сборку не задерживает): работы модели `running`, оборванные остановкой, — снова `queued`. База
/// пользователя не открылась — WARN, остальные пользователи — дальше.
pub async fn on_start(ctx: &NightCtx) -> Result<(), StoreError> {
    let st = ctx.st();
    for u in st.system.list_users().await? {
        if ctx.stopping() {
            break;
        }
        let requeued = async {
            let db = st.users.get(u.id).await?;
            store::requeue_model_running(&db).await
        };
        match requeued.await {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(user = %u.id, works = n, "внутренние работы: работы модели возвращены в очередь");
            }
            Err(e) => {
                tracing::warn!(user = %u.id, error = %e, "внутренние работы: работы модели пользователя не возвращены в очередь");
            }
        }
    }
    Ok(())
}

/// Строка дайджеста (§6.3 п. 5): работы агента `queued` в пространствах `spaces` у видов с
/// действующим режимом `agent` и старейшая из них (цель отметки показа
/// [`store::WORKS_LINE_PART`]); нет — `None`.
pub async fn digest_line(
    st: &crate::app::AppState,
    user: Id,
    db: &Db,
    spaces: &[Id],
    lang: Lang,
) -> Result<Option<(String, Id)>, StoreError> {
    let works = settings::load(&st.system, user).await?;
    let counts: Vec<(WorkKind, u32, Id)> = store::queued_agent_counts(db, spaces)
        .await?
        .into_iter()
        .filter(|(k, _, _)| {
            settings::effective_mode(*k, &works, &st.cfg.works).0 == WorksMode::Agent
        })
        .collect();
    if counts.is_empty() {
        return Ok(None);
    }
    let list = counts
        .iter()
        .map(|(k, n, _)| format!("{} {n}", kinds::label(*k, lang)))
        .collect::<Vec<_>>()
        .join(", ");
    let line = match lang {
        Lang::Ru => WORKS_DIGEST_LINE_RU,
        Lang::En => WORKS_DIGEST_LINE_EN,
    };
    Ok(Some((line.replace("{list}", &list), counts[0].2)))
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
