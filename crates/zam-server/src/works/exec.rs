//! Исполнение пакета работы подключённой моделью (M7.3, спецификация §6.2 пп. 4–8; §6.3 п. 3):
//! пакет → второй скан → вызов → разбор JSON с одним повтором.
//!
//! - **Пакет** ([`Payload`]) — инструкция и схема итога (системное сообщение) и элементы
//!   (пользовательское); элементы собирает построитель пакета вида работы (маска агента, границы
//!   `<record>` — задача 3).
//! - **Второй скан** ([`second_scan`]): тело запроса проходит `zam_secrets::scan_secrets`;
//!   элемент с находкой выбрасывается, остальное уходит (решение координатора п. 39); находка в
//!   постоянном тексте инструкции — ошибка кода (`secret_in_payload`).
//! - **JSON** ([`extract_json`], [`json_step`]): первый блок кода `json` или первый
//!   сбалансированный объект текста; не разобрался — **один** повтор той же беседы с сообщением
//!   «ответ не JSON: <ошибка разбора>; верни только JSON по схеме», второй сбой — `bad_json`.
//! - **Выполнение** ([`run_payload`]): окно ночи, «стоп до следующей ночи» по строкам журнала
//!   **своего** запуска ([`RunKey`], столбец `work_calls.run`; решение координатора 77), второй
//!   скан, подключение ([`super::conn::prepare`]), затем на **каждое** обращение: действующий
//!   режим вида и пауза (иначе `blocked` без запроса), пределы суток и месяца, слот сервера
//!   (`max_parallel`; у пользователя — одно обращение, замок на весь пакет), аудит
//!   `works.model.key_use` раз на запуск, вызов в отдельном потоке, строка журнала; повторы
//!   `rate` и `unavailable`, пауза `auth`/`not_found` ([`super::pause`]).
//! - **Остановка** не ждёт висящий вызов: по `ctx.stopped()` обращение бросается (строка
//!   `aborted` без длительности), а сам вызов идёт в **отвязанном** потоке `std::thread` — он не
//!   держит ни пул блокирующих задач рантайма, ни выход процесса (Review Focus 5).
//!
//! Поток вызова получает только данные запроса (подключение, маршрут, ключ, беседу) — ни `Db`, ни
//! `AppState` (решение координатора 56); остановка — `ctx.stopping()`/`ctx.stopped()`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp, civil};
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::work_calls::{self, CallRow};
use zam_store::works::WorkKind;

use super::conn::{self, Prepared};
use super::model::{self, CallClass, CallError, ChatMessage, ModelConn, ModelReply};
use super::night::NightCtx;
use super::pause;
use super::settings::{self, UserWorks};
use crate::config::{WorksMode, WorksSection};

/// Разделитель элементов пользовательского сообщения.
const ITEM_SEPARATOR: &str = "\n\n";
/// Предел текста ошибки разбора в сообщении повтора, символов.
const REPAIR_ERROR_MAX_CHARS: usize = 200;

/// Пакет работы для модели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// Инструкция вида (постоянный текст).
    pub instruction: String,
    /// Схема итога (постоянный текст).
    pub schema: String,
    pub items: Vec<PayloadItem>,
}

/// Элемент пакета: номер `n` (как в `<record n="…">`) и готовый текст элемента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadItem {
    pub n: usize,
    pub text: String,
}

impl Payload {
    /// Системное сообщение: инструкция и схема.
    pub fn system(&self) -> String {
        format!("{}{ITEM_SEPARATOR}{}", self.instruction, self.schema)
    }

    /// Пользовательское сообщение: элементы через пустую строку.
    pub fn user(&self) -> String {
        assemble(&self.items).0
    }
}

/// Пакет после второго скана: что уходит и номера выброшенных элементов (по возрастанию).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub payload: Payload,
    pub dropped: Vec<usize>,
}

/// Пользовательское сообщение и байтовые диапазоны элементов в нём.
fn assemble(items: &[PayloadItem]) -> (String, Vec<std::ops::Range<usize>>) {
    let mut out = String::new();
    let mut spans = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(ITEM_SEPARATOR);
        }
        let start = out.len();
        out.push_str(&it.text);
        spans.push(start..out.len());
    }
    (out, spans)
}

/// Второй скан тела запроса (§6.2 п. 6): находка в системном сообщении — `secret_in_payload`
/// (ошибка кода: постоянный текст); элементы, которых касается находка в собранном
/// пользовательском сообщении, выбрасываются, и скан повторяется по новому телу, пока находок
/// нет — уходит ровно то, что сканер признал чистым.
pub fn second_scan(mut payload: Payload) -> Result<Scanned, CallError> {
    if !zam_secrets::scan_secrets(&payload.system()).is_empty() {
        return Err(CallError::new(CallClass::SecretInPayload));
    }
    let mut dropped = Vec::new();
    loop {
        let (user, spans) = assemble(&payload.items);
        let hits = zam_secrets::scan_secrets(&user);
        if hits.is_empty() {
            break;
        }
        let touched: BTreeSet<usize> = hits
            .iter()
            .flat_map(|h| {
                spans
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| h.range.start < s.end && s.start < h.range.end)
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>()
            })
            .collect();
        // находка только в разделителях невозможна (постоянный текст); на всякий случай — как
        // находка в инструкции
        if touched.is_empty() {
            return Err(CallError::new(CallClass::SecretInPayload));
        }
        for i in touched.iter().rev() {
            dropped.push(payload.items.remove(*i).n);
        }
    }
    dropped.sort_unstable();
    Ok(Scanned { payload, dropped })
}

/// Рассуждение `<think>…</think>` в начале ответа (локальные серверы без разбора рассуждений)
/// ответом не считается.
fn strip_think(text: &str) -> &str {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<think>")
        && let Some((_, after)) = rest.split_once("</think>")
    {
        return after;
    }
    text
}

/// Содержимое первого блока кода с языком `json`.
fn first_json_fence(t: &str) -> Option<&str> {
    let mut rest = t;
    while let Some(i) = rest.find("```") {
        let (info, body) = rest[i + 3..].split_once('\n')?;
        let end = body.find("```")?;
        if info.trim().eq_ignore_ascii_case("json") {
            return Some(&body[..end]);
        }
        rest = &body[end + 3..];
    }
    None
}

/// Первый сбалансированный объект `{…}` текста (строки JSON с экранированием учитываются).
fn first_balanced_object(t: &str) -> Option<&str> {
    let start = t.find('{')?;
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (i, &c) in t.as_bytes().iter().enumerate().skip(start) {
        if in_str {
            match c {
                _ if esc => esc = false,
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&t[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_object(s: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(s.trim()) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err("ответ — не JSON-объект".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// JSON-объект ответа (§6.2 п. 4): первый блок кода `json`, иначе первый сбалансированный объект
/// текста; ошибка — текст для сообщения повтора.
pub fn extract_json(text: &str) -> Result<Value, String> {
    let t = strip_think(text);
    if let Some(block) = first_json_fence(t) {
        return parse_object(block);
    }
    match first_balanced_object(t) {
        Some(obj) => parse_object(obj),
        None => Err("в ответе нет JSON-объекта".into()),
    }
}

/// Сообщение повтора (§6.2 п. 4).
pub fn repair_prompt(err: &str) -> String {
    let err: String = err.chars().take(REPAIR_ERROR_MAX_CHARS).collect();
    format!("ответ не JSON: {err}; верни только JSON по схеме")
}

/// Что делать после ответа модели.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonStep {
    /// Итог разобран.
    Done(Value),
    /// Повтор той же беседы: ответ модели и просьба вернуть JSON.
    Retry(Vec<ChatMessage>),
    /// Второй сбой — `bad_json` (с учётом токенов ответа).
    Failed(CallError),
}

/// Шаг разбора JSON с одним повтором: `convo` — отправленная беседа, `reply` — ответ на неё,
/// `retried` — это уже ответ на повтор.
pub fn json_step(convo: &[ChatMessage], reply: &ModelReply, retried: bool) -> JsonStep {
    match extract_json(&reply.text) {
        Ok(v) => JsonStep::Done(v),
        Err(e) if !retried => {
            let mut next = convo.to_vec();
            next.push(ChatMessage::assistant(reply.text.clone()));
            next.push(ChatMessage::user(repair_prompt(&e)));
            JsonStep::Retry(next)
        }
        Err(_) => JsonStep::Failed(CallError {
            usage: Some(reply.usage()),
            ..CallError::new(CallClass::BadJson)
        }),
    }
}

// ---------- выполнение: запуск, пределы, журнал, повторы (§6.2 пп. 5, 7, 8) ----------

/// Ожидание перед повторами `unavailable` (§6.2 п. 5): через 10 и через 60 с.
pub const UNAVAILABLE_RETRY: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(60)];
/// `rate`: `retry-after` не дольше стольких секунд — ожидание и один повтор, иначе стоп.
pub const RATE_WAIT_MAX_S: u64 = 60;
/// Столько `unavailable` за запуск (ночь, ручной запуск) — стоп до следующей ночи.
pub const UNAVAILABLE_STOP: usize = 3;

/// Запуск, по строкам журнала которого считаются «стоп до следующей ночи» и аудит `key_use`
/// «раз за ночь» (решение координатора 77): ночь (дата слота) или ручной запуск — его id
/// (один на весь ручной запуск, создаётся в его начале). Строка журнала несёт запуск в столбце
/// `run` ([`RunKey::run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKey {
    Night(civil::Date),
    Manual(Id),
}

impl RunKey {
    /// Значение столбца `work_calls.run`: `night:<дата>` или `manual:<id>`.
    pub fn run(&self) -> String {
        match self {
            RunKey::Night(d) => format!("night:{d}"),
            RunKey::Manual(id) => format!("manual:{id}"),
        }
    }

    /// Окно ночи `[slot(d), slot(d) + window_min)` по `[works] at`, `tz`, `window_min`; у
    /// ручного запуска окна нет.
    pub fn night_window(&self, s: &WorksSection) -> Option<(Timestamp, Timestamp)> {
        let RunKey::Night(d) = self else {
            return None;
        };
        let start = crate::schedule::slot(*d, s.at_hm(), &s.time_zone());
        let len = SignedDuration::from_mins(i64::from(s.window_min));
        Some((start, start.checked_add(len).unwrap_or(start)))
    }
}

/// Итог пакета: JSON ответа модели (`null` — после второго скана отправлять было нечего) и
/// номера элементов, выброшенных вторым сканом (в итоге работы — `secret_in_payload`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelOutcome {
    pub json: Value,
    pub dropped: Vec<usize>,
}

/// Слоты обращений к модели (§6.2 п. 7): не больше `max_parallel` на сервер, у пользователя —
/// одно (замок держится всё выполнение пакета или проверку подключения).
pub struct CallSlots {
    server: Arc<tokio::sync::Semaphore>,
    users: Mutex<HashMap<Id, Arc<tokio::sync::Mutex<()>>>>,
}

impl CallSlots {
    pub fn new(max_parallel: u32) -> CallSlots {
        let n = usize::try_from(max_parallel.max(1)).unwrap_or(1);
        CallSlots {
            server: Arc::new(tokio::sync::Semaphore::new(n)),
            users: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn user(&self, user: Id) -> Arc<tokio::sync::Mutex<()>> {
        self.users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(user)
            .or_default()
            .clone()
    }
}

impl std::fmt::Debug for CallSlots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallSlots")
            .field("server_free", &self.server.available_permits())
            .finish_non_exhaustive()
    }
}

/// Срок одного обращения: `[works] call_timeout_s` (тестовая точка — в миллисекундах).
pub fn call_timeout(s: &WorksSection) -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = test_hooks::call_timeout() {
        return d;
    }
    Duration::from_secs(u64::from(s.call_timeout_s))
}

fn unavailable_wait(i: usize) -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = test_hooks::retry_delay(i) {
        return d;
    }
    UNAVAILABLE_RETRY[i.min(UNAVAILABLE_RETRY.len() - 1)]
}

fn rate_wait(secs: u64) -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(unit) = test_hooks::rate_unit() {
        return unit.saturating_mul(u32::try_from(secs).unwrap_or(u32::MAX));
    }
    Duration::from_secs(secs)
}

/// «Стоп до следующей ночи» по строкам запуска (без проверок подключения): строка `quota` или
/// `limit` — стоп; последняя строка — `rate` (повтор не помог или `retry-after` дольше 60 с) —
/// стоп; [`UNAVAILABLE_STOP`] строк `unavailable` — стоп.
pub fn stopped(rows: &[CallRow]) -> Option<CallClass> {
    let rows: Vec<&CallRow> = rows
        .iter()
        .filter(|r| r.kind != work_calls::KIND_TEST)
        .collect();
    let has = |c: CallClass| rows.iter().any(|r| r.outcome == c.as_str());
    if has(CallClass::Quota) {
        return Some(CallClass::Quota);
    }
    if has(CallClass::Limit) {
        return Some(CallClass::Limit);
    }
    if rows
        .last()
        .is_some_and(|r| r.outcome == CallClass::Rate.as_str())
    {
        return Some(CallClass::Rate);
    }
    let unavailable = rows
        .iter()
        .filter(|r| r.outcome == CallClass::Unavailable.as_str())
        .count();
    (unavailable >= UNAVAILABLE_STOP).then_some(CallClass::Unavailable)
}

/// Что пишется в строку журнала одного обращения.
pub(crate) struct Meta<'a> {
    pub(crate) kind: &'a str,
    pub(crate) work: Option<Id>,
    pub(crate) items: Option<u32>,
    pub(crate) run: Option<String>,
}

pub(crate) fn row_of(
    p: &Prepared,
    m: &Meta<'_>,
    outcome: &str,
    usage: Option<model::Usage>,
    ms: Option<u64>,
    http_status: Option<u16>,
) -> CallRow {
    let u = usage.unwrap_or_default();
    CallRow {
        ts: zam_store::time::now(),
        work_id: m.work,
        kind: m.kind.to_string(),
        host: p.host.clone(),
        model: p.conn.model.clone(),
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        estimated: u.estimated,
        ms,
        outcome: outcome.to_string(),
        http_status,
        external: p.route.external,
        items: m.items,
        run: m.run.clone(),
    }
}

pub(crate) async fn journal(db: &zam_store::Db, row: CallRow) {
    if let Err(e) = work_calls::record_call(db, &row).await {
        tracing::warn!(
            outcome = %row.outcome,
            error = %e,
            "внутренние работы: строка журнала отправок не записана"
        );
    }
}

/// Пределы токенов (§6.2 п. 7): оценка «вход (счётчик выдачи) + `max_tokens`» помещается в
/// остаток суток и месяца UTC пользователя (`works.limits`, не выше потолков `[works]`); расход
/// — сумма по журналу отправок.
pub(crate) async fn fits_limits(
    db: &zam_store::Db,
    works: &UserWorks,
    s: &WorksSection,
    now: Timestamp,
    estimate: u64,
) -> Result<bool, zam_store::StoreError> {
    let (mut daily, mut monthly) = (s.max_daily_tokens, s.max_monthly_tokens);
    if let Some(l) = works.limits {
        daily = daily.min(l.daily_tokens);
        monthly = monthly.min(l.monthly_tokens);
    }
    if daily == 0 || monthly == 0 {
        return Ok(false);
    }
    let day = zam_store::time::day_start_utc(now);
    let month = month_start_utc(now);
    let spent_day = work_calls::tokens_since(db, day).await?;
    if spent_day.saturating_add(estimate) > daily {
        return Ok(false);
    }
    let spent_month = work_calls::tokens_since(db, month).await?;
    Ok(spent_month.saturating_add(estimate) <= monthly)
}

/// Начало месяца UTC момента `t`.
fn month_start_utc(t: Timestamp) -> Timestamp {
    let d = t.to_zoned(jiff::tz::TimeZone::UTC).date().first_of_month();
    d.to_zoned(jiff::tz::TimeZone::UTC)
        .map_or(t, |z| z.timestamp())
}

/// Оценка расхода обращения: вход счётчиком выдачи + предел ответа.
pub(crate) fn estimate(conn: &ModelConn, convo: &[ChatMessage]) -> u64 {
    let joined: Vec<&str> = convo.iter().map(|m| m.content.as_str()).collect();
    (zam_store::count_tokens(&joined.join("\n")) as u64).saturating_add(u64::from(conn.max_tokens))
}

/// Слот сервера (`max_parallel`); остановка сервера раньше — `None`.
pub(crate) async fn slot(ctx: &NightCtx) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let slots = ctx.st().model_calls.server.clone();
    tokio::select! {
        p = slots.acquire_owned() => p.ok(),
        () = ctx.stopped() => None,
    }
}

/// Итог одного обращения: ответ или ошибка, длительность (есть — ответ получен) и «сбой сервера
/// без отправки» (поток вызова не запустился или кончился без ответа; ревью r2 Minor 3).
pub(crate) struct Called {
    pub(crate) result: Result<ModelReply, CallError>,
    pub(crate) ms: Option<u64>,
    internal: bool,
}

impl Called {
    fn new(result: Result<ModelReply, CallError>, ms: Option<u64>, internal: bool) -> Called {
        Called {
            result,
            ms,
            internal,
        }
    }

    /// Строка журнала обращения: `ok`, класс ошибки, `aborted` (остановка после отправки, без
    /// `ms`) или `internal` (сбой без отправки, без `ms`; отправкой не считается).
    pub(crate) fn row(&self, p: &Prepared, m: &Meta<'_>) -> CallRow {
        match &self.result {
            Ok(reply) => row_of(
                p,
                m,
                work_calls::OUTCOME_OK,
                Some(reply.usage()),
                self.ms,
                Some(200),
            ),
            Err(_) if self.internal => row_of(p, m, work_calls::OUTCOME_INTERNAL, None, None, None),
            Err(e) => row_of(p, m, e.class.as_str(), e.usage, self.ms, e.http_status),
        }
    }
}

/// Одно обращение в **отвязанном** потоке `std::thread` (ни пул блокирующих задач рантайма, ни
/// выход процесса он не держит); поток получает только данные запроса. Остановка сервера
/// (`ctx.stopped()`) — `aborted` без длительности, вызов брошен (поток доживёт до срока `ureq` и
/// ответ выбросит); поток не запустился или кончился без ответа — `aborted` вызывающему, в
/// журнал — `internal`.
pub(crate) async fn call(
    ctx: &NightCtx,
    p: &Prepared,
    convo: &[ChatMessage],
    timeout: Duration,
) -> Called {
    let (conn, route, key) = (p.conn.clone(), p.route.clone(), p.key_clone());
    let convo = convo.to_vec();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let started = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("works-model-call".into())
        .spawn(move || {
            let r = model::call_chat_blocking(
                &conn,
                &route,
                key.as_ref().map(|k| k.as_str()),
                &convo,
                timeout,
            );
            let _ = tx.send(r);
        });
    let aborted = || Err(CallError::new(CallClass::Aborted));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "внутренние работы: поток вызова модели не запущен");
        return Called::new(aborted(), None, true);
    }
    tokio::select! {
        r = rx => match r {
            Ok(r) => {
                let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                Called::new(r, Some(ms), false)
            }
            Err(_) => {
                tracing::warn!("внутренние работы: поток вызова модели завершился без ответа");
                Called::new(aborted(), None, true)
            }
        },
        () = ctx.stopped() => Called::new(aborted(), None, false),
    }
}

/// Ожидание с остановкой: `false` — сервер останавливается.
async fn wait(ctx: &NightCtx, d: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(d) => true,
        () = ctx.stopped() => false,
    }
}

/// Состояние запуска внутри одного [`run_payload`].
struct Run<'a> {
    ctx: &'a NightCtx,
    user: Id,
    kind: WorkKind,
    db: &'a zam_store::Db,
    p: &'a Prepared,
    meta: Meta<'a>,
    /// Уже было обращение с ключом в этом запуске (аудит `key_use` — раз на запуск).
    key_used: bool,
    /// Строк `unavailable` запуска.
    unavailable: usize,
}

impl Run<'_> {
    /// Одно обращение с повторами (§6.2 пп. 5, 7). Перед **каждой** попыткой: действующий режим
    /// вида и пауза (выключил человек, пока шло ожидание повтора, — `blocked` без запроса; ревью
    /// r1 Important 2), предел (не помещается — строка `limit`, запроса нет), слот сервера, аудит
    /// `key_use` перед первым обращением запуска (не записался — обращения нет); строка журнала
    /// на каждое обращение; `rate` с `retry-after` ≤ 60 с — ожидание и один повтор; `unavailable`
    /// — до двух повторов (через 10 и 60 с), пока строк запуска меньше трёх; `auth`, `not_found`
    /// — пауза.
    async fn send(&mut self, convo: &[ChatMessage]) -> Result<ModelReply, CallError> {
        let st = self.ctx.st();
        let cfg = &st.cfg.works;
        let timeout = call_timeout(cfg);
        let (mut rate_retried, mut retries) = (false, 0usize);
        loop {
            if self.ctx.stopping() {
                return Err(CallError::new(CallClass::Aborted));
            }
            let works = settings::load(&st.system, self.user)
                .await
                .map_err(|e| conn::internal("настройки исполнителя", e).error)?;
            if settings::effective_mode(self.kind, &works, cfg).0 != WorksMode::Model {
                return Err(CallError::new(CallClass::Blocked));
            }
            let now = zam_store::time::now();
            let need = estimate(&self.p.conn, convo);
            match fits_limits(self.db, &works, cfg, now, need).await {
                Ok(true) => {}
                Ok(false) => {
                    journal(
                        self.db,
                        row_of(
                            self.p,
                            &self.meta,
                            CallClass::Limit.as_str(),
                            None,
                            None,
                            None,
                        ),
                    )
                    .await;
                    return Err(CallError::new(CallClass::Limit));
                }
                Err(e) => return Err(conn::internal("расход токенов", e).error),
            }
            let Some(_permit) = slot(self.ctx).await else {
                return Err(CallError::new(CallClass::Aborted));
            };
            if self.p.has_key() && !self.key_used {
                let logged = pause::audit(
                    st,
                    pause::SYSTEM,
                    None,
                    self.user,
                    "works.model.key_use",
                    json!({"vault": format!("v:{}", self.p.conn.vault), "host": self.p.host}),
                )
                .await;
                // ключ без следа в аудите наружу не уходит
                if !logged {
                    return Err(CallError::new(CallClass::Aborted));
                }
                self.key_used = true;
            }
            let called = call(self.ctx, self.p, convo, timeout).await;
            journal(self.db, called.row(self.p, &self.meta)).await;
            let e = match called.result {
                Ok(reply) => return Ok(reply),
                Err(e) => e,
            };
            match e.class {
                CallClass::Auth | CallClass::NotFound => {
                    pause::set_paused(st, self.user, e.class, None, pause::SYSTEM, None).await;
                    return Err(e);
                }
                CallClass::Rate
                    if !rate_retried && e.retry_after.is_some_and(|s| s <= RATE_WAIT_MAX_S) =>
                {
                    rate_retried = true;
                    if !wait(self.ctx, rate_wait(e.retry_after.unwrap_or(0))).await {
                        return Err(CallError::new(CallClass::Aborted));
                    }
                }
                CallClass::Unavailable => {
                    self.unavailable += 1;
                    if self.unavailable >= UNAVAILABLE_STOP || retries >= UNAVAILABLE_RETRY.len() {
                        return Err(e);
                    }
                    if !wait(self.ctx, unavailable_wait(retries)).await {
                        return Err(CallError::new(CallClass::Aborted));
                    }
                    retries += 1;
                }
                _ => return Err(e),
            }
        }
    }
}

/// Выполняет пакет работы подключённой моделью (§6.2, §6.3 п. 7). Запроса нет вовсе (класс
/// `blocked`, без паузы), если действующий режим вида не `model` (`[works] enabled = false`,
/// режим `off`/`agent`, нет подключения, `model_access = "off"`, пауза) — и перед каждым
/// обращением, а не только в начале; у ночи — вне её окна (`aborted`); после «стопа до следующей
/// ночи» этого запуска (класс стопа); нулевой потолок и предел — `limit`. Второй скан
/// выбрасывает элементы с находкой (`dropped`); после него пусто — запроса нет, `json = null`.
/// Ответ не JSON — один повтор беседы, затем `bad_json`. `work` — работа `j:<id>` (строки
/// журнала). Паузу (`auth`, `not_found`, `blocked`, `vault`, `vault_changed`) ставит сам.
pub async fn run_payload(
    ctx: &NightCtx,
    user: Id,
    run: RunKey,
    payload: Payload,
    kind: WorkKind,
    work: Option<Id>,
) -> Result<ModelOutcome, CallError> {
    if ctx.stopping() {
        return Err(CallError::new(CallClass::Aborted));
    }
    let st = ctx.st();
    let cfg = &st.cfg.works;
    let lock = st.model_calls.user(user);
    let _guard = tokio::select! {
        g = lock.lock_owned() => g,
        () = ctx.stopped() => return Err(CallError::new(CallClass::Aborted)),
    };
    let works = settings::load(&st.system, user)
        .await
        .map_err(|e| conn::internal("настройки исполнителя", e).error)?;
    if settings::effective_mode(kind, &works, cfg).0 != WorksMode::Model {
        return Err(CallError::new(CallClass::Blocked));
    }
    let now = zam_store::time::now();
    if let Some((from, until)) = run.night_window(cfg)
        && !(from <= now && now < until)
    {
        return Err(CallError::new(CallClass::Aborted));
    }
    let db = st
        .users
        .get(user)
        .await
        .map_err(|e| conn::internal("база пользователя", e).error)?;
    let rows: Vec<CallRow> = work_calls::calls_of_run(&db, &run.run())
        .await
        .map_err(|e| conn::internal("журнал отправок", e).error)?;
    if let Some(class) = stopped(&rows) {
        return Err(CallError::new(class));
    }
    let Scanned { payload, dropped } = second_scan(payload)?;
    // только контекст вида (`n = 0`, заметки Rules) без элементов работы — запроса нет
    if payload.items.iter().all(|i| i.n == 0) {
        return Ok(ModelOutcome {
            json: Value::Null,
            dropped,
        });
    }
    let items = u32::try_from(payload.items.len()).ok();
    let p = match conn::prepare(ctx, user, &works, pause::SYSTEM, None).await {
        Ok(p) => p,
        Err(r) => {
            // имя не разрешилось — строка без отправки: три `unavailable` за запуск — стоп
            if r.error.class == CallClass::Unavailable
                && let Some(c) = &works.model
            {
                let host = c.pinned.as_ref();
                journal(
                    &db,
                    CallRow {
                        ts: zam_store::time::now(),
                        work_id: work,
                        kind: kind.as_str().to_string(),
                        host: host.map(super::egress::host_label).unwrap_or_default(),
                        model: c.model.clone(),
                        input_tokens: 0,
                        output_tokens: 0,
                        estimated: false,
                        ms: None,
                        outcome: CallClass::Unavailable.as_str().to_string(),
                        http_status: None,
                        external: host.is_none_or(|b| super::egress::assumed_external(&b.host)),
                        items,
                        run: Some(run.run()),
                    },
                )
                .await;
            }
            return Err(r.error);
        }
    };
    let mut call_run = Run {
        ctx,
        user,
        kind,
        db: &db,
        p: &p,
        meta: Meta {
            kind: kind.as_str(),
            work,
            items,
            run: Some(run.run()),
        },
        key_used: rows.iter().any(CallRow::sent),
        unavailable: rows
            .iter()
            .filter(|r| r.outcome == CallClass::Unavailable.as_str())
            .count(),
    };
    let mut convo = vec![
        ChatMessage::system(payload.system()),
        ChatMessage::user(payload.user()),
    ];
    let mut retried = false;
    loop {
        let reply = call_run.send(&convo).await?;
        match json_step(&convo, &reply, retried) {
            JsonStep::Done(json) => return Ok(ModelOutcome { json, dropped }),
            JsonStep::Retry(next) => {
                convo = next;
                retried = true;
            }
            JsonStep::Failed(e) => return Err(e),
        }
    }
}

/// Тестовые точки исполнения (фича `test-hooks`): срок обращения и ожидания повторов в
/// миллисекундах — тесты идут секунды. Настройки общие для процесса.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone, Copy, Default)]
    struct Hooks {
        call_timeout: Option<Duration>,
        retry: Option<[Duration; 2]>,
        rate_unit: Option<Duration>,
    }

    static HOOKS: Mutex<Hooks> = Mutex::new(Hooks {
        call_timeout: None,
        retry: None,
        rate_unit: None,
    });

    fn get() -> Hooks {
        *HOOKS.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set(f: impl FnOnce(&mut Hooks)) {
        f(&mut HOOKS.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Срок одного обращения вместо `[works] call_timeout_s`.
    pub fn set_call_timeout_ms(ms: u64) {
        set(|h| h.call_timeout = Some(Duration::from_millis(ms)));
    }

    /// Ожидание перед первым и вторым повтором `unavailable` вместо 10 и 60 с.
    pub fn set_retry_delays_ms(first: u64, second: u64) {
        set(|h| h.retry = Some([Duration::from_millis(first), Duration::from_millis(second)]));
    }

    /// Единица `retry-after` (`rate`) вместо секунды.
    pub fn set_rate_unit_ms(ms: u64) {
        set(|h| h.rate_unit = Some(Duration::from_millis(ms)));
    }

    /// Снимает подмены.
    pub fn reset() {
        set(|h| *h = Hooks::default());
    }

    pub(crate) fn call_timeout() -> Option<Duration> {
        get().call_timeout
    }

    pub(super) fn retry_delay(i: usize) -> Option<Duration> {
        get().retry.map(|r| r[i.min(1)])
    }

    pub(super) fn rate_unit() -> Option<Duration> {
        get().rate_unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(n: usize, text: &str) -> PayloadItem {
        PayloadItem {
            n,
            text: text.into(),
        }
    }

    fn secret() -> String {
        format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5")
    }

    /// Блок `json`, объект в тексте, строки со скобками, `<think>`, не объект, нет объекта.
    #[test]
    fn extract_json_cases() {
        assert_eq!(extract_json("{\"a\": 1}").unwrap(), json!({"a": 1}));
        assert_eq!(
            extract_json("Итог:\n```json\n{\"a\": [1, 2]}\n```\nконец").unwrap(),
            json!({"a": [1, 2]})
        );
        // блок без языка пропускается, объект — из текста
        assert_eq!(
            extract_json("```\nкод\n```\nи {\"b\": true}").unwrap(),
            json!({"b": true})
        );
        // блок json сильнее объекта раньше него
        assert_eq!(
            extract_json("{\"x\": 0} потом ```JSON\n{\"y\": 1}\n```").unwrap(),
            json!({"y": 1})
        );
        assert_eq!(
            extract_json("Вот: {\"q\": \"скобка } и { и \\\" кавычка\"} — всё").unwrap(),
            json!({"q": "скобка } и { и \" кавычка"})
        );
        assert_eq!(
            extract_json("<think>думаю {не JSON}</think>\n{\"ok\": true}").unwrap(),
            json!({"ok": true})
        );
        assert!(extract_json("[1, 2]").is_err());
        assert!(
            extract_json("```json\n[1]\n```")
                .unwrap_err()
                .contains("не JSON-объект")
        );
        assert!(
            extract_json("просто текст")
                .unwrap_err()
                .contains("нет JSON-объекта")
        );
        assert!(extract_json("{\"a\": ").is_err());
        assert!(extract_json("{\"a\": 1,}").is_err());
    }

    /// Повтор — беседа плюс ответ и просьба; ошибка разбора усечена.
    #[test]
    fn json_step_retry_then_fail() {
        let convo = vec![ChatMessage::system("с"), ChatMessage::user("п")];
        let reply = ModelReply {
            text: "не JSON".into(),
            input_tokens: 5,
            output_tokens: 3,
            estimated: false,
            reasoning: false,
        };
        let JsonStep::Retry(next) = json_step(&convo, &reply, false) else {
            panic!("повтор");
        };
        assert_eq!(next.len(), 4);
        assert_eq!(next[2], ChatMessage::assistant("не JSON"));
        assert!(next[3].content.starts_with("ответ не JSON: "));
        let JsonStep::Failed(e) = json_step(&next, &reply, true) else {
            panic!("отказ");
        };
        assert_eq!(e.class, CallClass::BadJson);
        assert_eq!(e.usage.unwrap().output_tokens, 3);
        let long = repair_prompt(&"x".repeat(1000));
        assert!(long.chars().count() < 260, "{long}");
    }

    /// Второй скан: выброшены только элементы с находкой; находка в инструкции или схеме —
    /// `secret_in_payload`; чистый пакет не меняется.
    #[test]
    fn second_scan_drops_items() {
        let p = Payload {
            instruction: "инструкция".into(),
            schema: "{}".into(),
            items: vec![
                item(1, "чисто"),
                item(2, &format!("ключ {}", secret())),
                item(3, "тоже чисто"),
                item(4, &format!("и здесь {}", secret())),
            ],
        };
        let s = second_scan(p.clone()).unwrap();
        assert_eq!(s.dropped, vec![2, 4]);
        assert_eq!(s.payload.user(), "чисто\n\nтоже чисто");
        assert!(zam_secrets::scan_secrets(&s.payload.user()).is_empty());
        let clean = Payload {
            items: vec![item(1, "чисто")],
            ..p.clone()
        };
        assert_eq!(second_scan(clean.clone()).unwrap().payload, clean);
        let mut bad = p.clone();
        bad.schema = format!("{{\"k\": \"{}\"}}", secret());
        assert_eq!(
            second_scan(bad).unwrap_err().class,
            CallClass::SecretInPayload
        );
        assert_eq!(p.system(), "инструкция\n\n{}");
    }

    /// Начало месяца UTC — первое число, а не начало суток (ревью r1 Minor 4: граница месяца).
    #[test]
    fn month_start_is_first_of_month() {
        let t = |s: &str| s.parse::<Timestamp>().unwrap();
        assert_eq!(
            month_start_utc(t("2026-11-23T10:00:00Z")),
            t("2026-11-01T00:00:00Z")
        );
        assert_eq!(
            month_start_utc(t("2026-12-01T00:00:00Z")),
            t("2026-12-01T00:00:00Z")
        );
        assert_eq!(
            month_start_utc(t("2026-03-31T23:59:59Z")),
            t("2026-03-01T00:00:00Z")
        );
    }
}
