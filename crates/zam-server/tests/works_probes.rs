//! Вид внутренней работы `probes` (M7.3, спецификация §6.4 п. 2 «Проверка», задача 4): сборщик
//! по слоям (детерминированно, каждую ночь иначе; задачи, вопросы, идеи, changelog, отчёты и
//! Human — нет), проверка пробного вопроса (слова, длина, путь, ссылка, дата, URL, обратные
//! кавычки, пересказ заголовка, повтор, маска), две пробы у длинной записи, пределы на
//! пользователя (ночь и ручные запуски — свои), записи участников общего решения, автор пробы
//! модели, отсев `secret_in_payload` и явного `skip` до новой ревизии; ночь — сначала проверка
//! всех пользователей, затем работы (§6.3 «Проверка»).
//!
//! Подставная модель — `common::mock_model` на `127.0.0.1` с `[works] allow_loopback = true`; ни
//! одного обращения к настоящей модели и сети. Тесты файла идут по одному (`SERIAL`): часы,
//! тестовые точки ночи и исполнения общие для процесса; тик ночного исполнителя — 1 ч (кроме
//! теста порядка ночи — там ночь идёт тиком раннера после перезапуска).
mod common;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, api_with, id, ok};
use common::check::{Reset, SERIAL, space_of};
use common::mock_model::{MockModel, MockReply};
use jiff::{SignedDuration, Timestamp, civil};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::{CheckMode, ServerConfig};
use zam_server::works::package::MASK_LITERAL_REASON;
use zam_server::works::probes::{
    self, INSTRUCTION_RU, PROBES_PER_DAY_MANUAL, PROBES_PER_NIGHT, REASON_DUPLICATE,
    REASON_ONE_PER_RECORD, REASON_RETELL, REASON_TWO_PER_RECORD, SCHEMA, SIGNAL_QUESTIONS,
};
use zam_server::works::{NightCtx, kinds, queue};
use zam_store::SpaceKind;
use zam_store::probes::{
    InsertOutcome, NewProbe, Probe, ProbeAuthor, ProbeFilter, ProbeKind, ProbeTarget,
};
use zam_store::works::{self as store, NewWork, Origin, WorkItem, WorkKind, WorkMode, WorkState};

const SETTINGS: &str = "/api/v1/auth/settings";
const RECALL: &str = "/api/v1/recall";
const WORKS: &str = "/api/v1/works";
const RUN: &str = "/api/v1/works/run";
const S1: &str = "probes-session-1";
const G1: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e41";
const G2: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e42";

/// Фиктивный ключ (склейка — push protection) и ключ поставщика для записи Vault.
fn secret() -> String {
    format!("gh{}_{}", "p", "zY3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ7")
}

fn model_key() -> String {
    format!("{}{}", "sk-", "mockKeyNotReal0123456789")
}

/// Окно ночи вокруг «сейчас» (началось 10 минут назад, 12 ч): ночные обращения к модели идут.
fn night_now(c: &mut ServerConfig) {
    let start =
        (Timestamp::now() - SignedDuration::from_mins(10)).to_zoned(jiff::tz::TimeZone::UTC);
    c.works.at = format!("{:02}:{:02}", start.hour(), start.minute());
    c.works.tz = "UTC".into();
    c.works.window_min = 720;
}

/// Окно ночи через 6 ч (сейчас — вне окна).
fn night_later(c: &mut ServerConfig) {
    let start =
        (Timestamp::now() + SignedDuration::from_hours(6)).to_zoned(jiff::tz::TimeZone::UTC);
    c.works.at = format!("{:02}:{:02}", start.hour(), start.minute());
    c.works.tz = "UTC".into();
    c.works.window_min = 60;
}

/// Сервер теста: тик ночи — 1 ч, короткие сроки исполнения, петля разрешена, окно ночи — через
/// 6 ч; `tweak` — правка конфигурации.
async fn server(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    zam_server::works::night::test_hooks::set_tick(Duration::from_secs(3600));
    zam_server::works::exec::test_hooks::set_call_timeout_ms(20_000);
    zam_server::works::exec::test_hooks::set_retry_delays_ms(10, 20);
    zam_server::works::exec::test_hooks::set_rate_unit_ms(1);
    api(&format!("probes-{name}"), |c| {
        night_later(c);
        c.works.allow_loopback = true;
        tweak(c);
    })
    .await
}

async fn patch(a: &Api, body: Value) -> Resp {
    a.adm(Method::PATCH, SETTINGS, Some(body), None).await
}

/// Режим агента только у вида `probes` (прочие виды — `off`).
async fn agent_mode(a: &Api) {
    ok(
        &patch(
            a,
            json!({"works": {"mode": "off", "kinds": {"probes": "agent"}}}),
        )
        .await,
        StatusCode::OK,
    );
}

/// Подключение пользователя к подставной модели (запись его личного Vault по токену `tok`) и
/// режим `model` только у вида `probes`; `user` — чужой пользователь (настройки — через
/// администратора).
async fn model_mode(a: &Api, m: &MockModel, tok: &str, user: Option<&str>) {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/spaces/knowledge/vault",
            Some(
                json!({"title": "llm", "data": {"type": "token", "url": m.base_url()},
                        "secret": model_key()}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let body = json!({"works": {"mode": "off", "kinds": {"probes": "model"},
        "model": {"protocol": "openai", "vault": "llm", "model": "mock-model",
                  "max_tokens": 4000}}});
    let r = match user {
        None => patch(a, body).await,
        Some(u) => {
            a.adm(
                Method::PATCH,
                &format!("/api/v1/admin/users/{u}/settings"),
                Some(body),
                None,
            )
            .await
        }
    };
    ok(&r, StatusCode::OK);
}

/// Запись журнала в пространстве `space` токеном `tok`; ссылка по букве вида.
async fn entry_by(a: &Api, tok: &str, space: &str, body: Value) -> String {
    let kind = body["kind"].as_str().unwrap().to_string();
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let letter = zam_store::EntryKind::parse(&kind).unwrap().ref_letter();
    format!("{letter}:{}", r.json()["id"].as_str().unwrap())
}

/// Insight вида `kind` первым токеном.
async fn insight_k(a: &Api, space: &str, title: &str, body: &str, kind: &str) -> String {
    entry_by(
        a,
        &a.token,
        space,
        json!({"kind": "insight", "title": title, "body": body, "data": {"kind": kind}}),
    )
    .await
}

async fn insight(a: &Api, space: &str, title: &str, body: &str) -> String {
    insight_k(a, space, title, body, "gotcha").await
}

/// Термин первым токеном.
async fn term(a: &Api, space: &str, title: &str) -> String {
    entry_by(
        a,
        &a.token,
        space,
        json!({"kind": "term", "title": title,
               "data": {"definition": format!("Определение понятия {title}")}}),
    )
    .await
}

/// Заметка токеном `tok` (`None` — человек, сессия WebUI); ссылка `n:<id>`.
async fn note_by(a: &Api, tok: Option<&str>, space: &str, path: &str, content: &str) -> String {
    let body = json!({"space": space, "path": path, "kind": "note", "content": content});
    let r = match tok {
        Some(t) => a.tok(t, Method::POST, "/api/v1/nodes", Some(body)).await,
        None => a.adm(Method::POST, "/api/v1/nodes", Some(body), None).await,
    };
    ok(&r, StatusCode::CREATED);
    format!("n:{}", id(&r.json()))
}

/// Новая ревизия записи журнала `target` (`i:<id>`) токеном `tok`.
async fn revise(a: &Api, tok: &str, target: &str, body: &str) {
    let eid = target.split_once(':').unwrap().1;
    let r = a
        .tok_rev(
            tok,
            Method::PATCH,
            &format!("/api/v1/entries/{eid}"),
            Some(json!({"body": body})),
        )
        .await;
    ok(&r, StatusCode::OK);
}

/// Регистрация решения `g` токеном `tok`.
async fn register(a: &Api, tok: &str, g: &str) -> Resp {
    a.tok(
        tok,
        Method::POST,
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": g, "name": format!("Решение {g}")})),
    )
    .await
}

fn ctx(a: &Api) -> NightCtx {
    NightCtx::new(a.st.clone())
}

async fn db(a: &Api) -> zam_store::Db {
    a.st.users.get(a.st.admin_id).await.unwrap()
}

/// Пространство вида `kind` базы администратора.
async fn space(a: &Api, kind: SpaceKind) -> Id {
    space_of(a, a.st.admin_id, kind).await
}

/// Пространство решения `g` базы администратора.
async fn solution(a: &Api, g: &str) -> Id {
    zam_store::list_spaces(&db(a).await)
        .await
        .unwrap()
        .into_iter()
        .find(|s| {
            s.solution_guid
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(g))
        })
        .unwrap()
        .id
}

/// Ночь `d` (окно — до «сейчас + 1 ч») всех пользователей (режим агента окна не проверяет).
async fn night_of(a: &Api, d: civil::Date) {
    let until = zam_store::time::now() + SignedDuration::from_hours(1);
    queue::night(&ctx(a), d, until).await.unwrap();
}

/// Ночь, в окне которой «сейчас» (`night_now`): у модели — ночные обращения идут.
async fn night_in_window(a: &Api) {
    let w = &a.st.cfg.works;
    let (d, _, until) = zam_server::schedule::night_window(
        zam_store::time::now(),
        w.at_hm(),
        &w.time_zone(),
        w.window_min,
    )
    .expect("сейчас — в окне ночи");
    queue::night(&ctx(a), d, until).await.unwrap();
}

/// Пустой отсев «уже предложено в этом запуске» (сборщик вне постановки).
fn none() -> HashSet<(String, i64)> {
    HashSet::new()
}

fn day(n: i8) -> civil::Date {
    civil::date(2026, 11, n)
}

/// Работы вида `probes` администратора, новые первыми.
async fn probe_works(a: &Api) -> Vec<store::Work> {
    store::list(&db(a).await, None, Some(WorkKind::Probes), 200)
        .await
        .unwrap()
}

/// Активные пробы вида `probe` базы, новые первыми.
async fn probes_of(db: &zam_store::Db) -> Vec<Probe> {
    zam_store::probes::list_probes(
        db,
        &ProbeFilter {
            kind: Some(ProbeKind::Probe),
            active: Some(true),
            split: None,
            limit: 1000,
        },
    )
    .await
    .unwrap()
}

/// Пакет вида `probes` первым токеном в сессии `session` (пространства сессии — Knowledge и
/// Rules).
async fn package(a: &Api, session: &str) -> Resp {
    package_in(a, session, None).await
}

/// Пакет вида `probes` в сессии `session` решения `solution` (`X-Zam-Solution`).
async fn package_in(a: &Api, session: &str, solution: Option<&str>) -> Resp {
    let mut h = vec![("x-zam-session", session)];
    if let Some(g) = solution {
        h.push(("x-zam-solution", g));
    }
    a.tok_h(
        &a.token,
        Method::POST,
        RECALL,
        Some(json!({"event": "work", "kind": "probes", "budget_tokens": 20000})),
        &h,
    )
    .await
}

/// Итог работы `work` (`j:<id>`) сессии `session`.
async fn put_result(a: &Api, session: &str, work: &str, body: Value) -> Resp {
    a.tok_h(
        &a.token,
        Method::POST,
        &format!("{WORKS}/{work}/result"),
        Some(body),
        &[("x-zam-session", session)],
    )
    .await
}

/// Итог работы `work` сессии `session` с `Idempotency-Key` `key`.
async fn put_result_key(a: &Api, session: &str, work: &str, body: Value, key: &str) -> Resp {
    a.tok_h(
        &a.token,
        Method::POST,
        &format!("{WORKS}/{work}/result"),
        Some(body),
        &[("x-zam-session", session), ("idempotency-key", key)],
    )
    .await
}

/// Захват пакета и итог `items` одним вызовом: ответ итога.
async fn answer(a: &Api, session: &str, items: Value) -> Value {
    answer_in(a, session, None, items).await
}

/// [`answer`] в сессии решения `solution`.
async fn answer_in(a: &Api, session: &str, solution: Option<&str>, items: Value) -> Value {
    let r = package_in(a, session, solution).await;
    ok(&r, StatusCode::OK);
    let work = r.json()["work"]
        .as_str()
        .unwrap_or_else(|| panic!("нет работы: {}", r.text()))
        .to_string();
    let r = put_result(a, session, &work, json!({"items": items})).await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// Причины отказов ответа итога по ссылке цели.
fn reasons(resp: &Value) -> HashMap<String, String> {
    resp["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["target"].as_str().unwrap_or("").to_string(),
                r["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Ждёт условия не дольше `limit`.
async fn until<F, Fut>(what: &str, limit: Duration, cond: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let t0 = Instant::now();
    while !cond().await {
        assert!(t0.elapsed() < limit, "не дождались: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Ответ подставной модели: элементы `{n, question}` с вопросами `q(n)`.
fn reply(ns: std::ops::RangeInclusive<usize>, q: impl Fn(usize) -> String) -> MockReply {
    let items: Vec<Value> = ns.map(|n| json!({"n": n, "question": q(n)})).collect();
    MockReply::chat(&json!({"items": items}).to_string())
}

/// Правильный пробный вопрос с номерами пакета `p` и элемента `n` (без слов заголовков тестов).
fn good_q(p: usize, n: usize) -> String {
    format!("Как настроить узел номер {n} в пакете {p} для ночной проверки памяти сервера?")
}

/// Готовые пробы вида `probe` работы постановки `origin` (закрытая работа без элементов в
/// пространстве `space`): `n` проб с вымышленными целями — счёт пределов.
async fn seed(a: &Api, space: Id, origin: Origin, n: usize) {
    let db = db(a).await;
    let w = store::enqueue(
        &db,
        NewWork {
            kind: WorkKind::Probes,
            space,
            mode: WorkMode::Agent,
            origin,
            night: None,
            items: Vec::new(),
        },
    )
    .await
    .unwrap()
    .expect("открытой работы вида в пространстве нет");
    store::finish(&db, w.id, WorkState::Done, json!({}), None)
        .await
        .unwrap();
    for i in 0..n {
        let target = format!("i:{}", Id::new());
        let out = zam_store::probes::insert_probe(
            &db,
            NewProbe {
                space: Some(space),
                question: format!(
                    "Готовая проба {} номер {i} для счёта пределов",
                    origin.as_str()
                ),
                kind: ProbeKind::Probe,
                targets: vec![ProbeTarget { target, rev: 1 }],
                split: zam_store::probes::Split::Dev,
                overlap: Some(0.0),
                author_kind: ProbeAuthor::Agent,
                author: None,
                work_id: Some(w.id),
            },
        )
        .await
        .unwrap();
        assert!(matches!(out, InsertOutcome::Inserted(_)), "{out:?}");
    }
}

/// Счёт новых проб постановки `origin` в её пределе (как у проверки итога).
async fn made(a: &Api, origin: Origin) -> u32 {
    let (since, _) = probes::quota(origin, zam_store::time::now(), &a.st.cfg.works);
    zam_store::probes::new_probes_since(&db(a).await, origin.as_str(), since)
        .await
        .unwrap()
}

// ---------------- сборщик ----------------

/// §6.4 п. 2: слои — insight по `data.kind`, заметки, термины (в Rules — заметки Rules), по
/// кругу слоёв; внутри слоя — порядок ночи: та же дата — тот же порядок, другая — другой; задачи,
/// вопросы, идеи, changelog, отчёты и Human не берутся; ночь ставит работы решения и Rules.
#[tokio::test]
async fn probes_sample_stratified_deterministic() {
    let _g = SERIAL.lock().await;
    let a = server("sample", |_| {}).await;
    ok(&register(&a, &a.token, G1).await, StatusCode::CREATED);
    let mut layer: HashMap<String, &str> = HashMap::new();
    for i in 0..5 {
        let r = insight_k(&a, G1, &format!("Ловушка {i}"), "текст ловушки", "gotcha").await;
        layer.insert(r, "insight:gotcha");
        let r = insight_k(&a, G1, &format!("Решение {i}"), "текст решения", "decision").await;
        layer.insert(r, "insight:decision");
        let r = note_by(
            &a,
            Some(&a.token),
            G1,
            &format!("docs/n{i}.md"),
            &format!("# Заметка {i}\n\nтекст заметки\n"),
        )
        .await;
        layer.insert(r, "note");
        let r = term(&a, G1, &format!("Понятие{i}")).await;
        layer.insert(r, "term");
    }
    // не берутся: задачи, вопросы, идеи, changelog, отчёты
    let mut other = HashSet::new();
    for body in [
        json!({"kind": "task", "title": "Задача", "data": {"next_step": "сделать"}}),
        json!({"kind": "question", "title": "Вопрос человеку?", "body": "к",
               "data": {"options": [{"key": "a", "text": "да"}, {"key": "b", "text": "нет"}]}}),
        json!({"kind": "idea", "title": "Идея", "body": "замысел", "data": {"origin": "agent"}}),
        json!({"kind": "changelog", "title": "Правка", "body": "что изменилось",
               "data": {"kind": "fix"}}),
        json!({"kind": "report", "title": "Отчёт", "body": "итог"}),
    ] {
        other.insert(entry_by(&a, &a.token, G1, body).await);
    }
    let rules: HashSet<String> = [
        note_by(&a, None, "rules", "r1.md", "# Правило 1\n\nделай так\n").await,
        note_by(&a, None, "rules", "r2.md", "# Правило 2\n\nделай этак\n").await,
    ]
    .into();
    note_by(&a, None, "human", "h.md", "# Человек\n\nличное\n").await;
    let user = a.st.admin_id;
    let g1 = solution(&a, G1).await;
    let c = ctx(&a);
    let first = probes::collect_on(&c, user, g1, day(23), 100, &none())
        .await
        .unwrap();
    assert_eq!(first.len(), 20, "{first:?}");
    // круг слоёв: первые четыре — все четыре слоя (порядок слоёв — ночи), дальше круг повторяется
    let round: Vec<&str> = first[..4].iter().map(|c| layer[&c.target]).collect();
    assert_eq!(
        round.iter().copied().collect::<HashSet<_>>(),
        HashSet::from(["insight:decision", "insight:gotcha", "note", "term"]),
        "{round:?}"
    );
    for (i, cand) in first.iter().enumerate() {
        assert!(!other.contains(&cand.target), "{cand:?}");
        assert_eq!(layer[&cand.target], round[i % 4], "{i}: по кругу слоёв");
    }
    let targets = |v: &[kinds::Candidate]| v.iter().map(|c| c.target.clone()).collect::<Vec<_>>();
    let again = probes::collect_on(&c, user, g1, day(23), 100, &none())
        .await
        .unwrap();
    assert_eq!(
        targets(&again),
        targets(&first),
        "та же ночь — тот же порядок"
    );
    let next = probes::collect_on(&c, user, g1, day(24), 100, &none())
        .await
        .unwrap();
    assert_ne!(
        targets(&next),
        targets(&first),
        "другая ночь — другой порядок"
    );
    let set = |v: &[kinds::Candidate]| targets(v).into_iter().collect::<HashSet<_>>();
    assert_eq!(set(&next), set(&first));
    let six = probes::collect_on(&c, user, g1, day(23), 6, &none())
        .await
        .unwrap();
    assert_eq!(targets(&six), targets(&first)[..6]);
    let r = probes::collect_on(
        &c,
        user,
        space(&a, SpaceKind::Rules).await,
        day(23),
        100,
        &none(),
    )
    .await
    .unwrap();
    assert_eq!(set(&r), rules);
    let h = probes::collect_on(
        &c,
        user,
        space(&a, SpaceKind::Human).await,
        day(23),
        100,
        &none(),
    )
    .await
    .unwrap();
    assert!(h.is_empty(), "{h:?}");
    // диспетчер вида и ночь: работы решения и Rules, Human — нет
    let d = kinds::collect(WorkKind::Probes, &c, user, g1, 100, &none())
        .await
        .unwrap();
    assert_eq!(set(&d), set(&first));
    agent_mode(&a).await;
    night_of(&a, day(23)).await;
    let ws = probe_works(&a).await;
    let spaces: HashSet<Id> = ws.iter().map(|w| w.space_id).collect();
    assert!(spaces.contains(&g1), "{ws:?}");
    assert!(
        spaces.contains(&space(&a, SpaceKind::Rules).await),
        "{ws:?}"
    );
    assert!(
        !spaces.contains(&space(&a, SpaceKind::Human).await),
        "{ws:?}"
    );
    let w = ws.iter().find(|w| w.space_id == g1).unwrap();
    assert_eq!(w.items.len(), 10, "batch_records по умолчанию");
}

/// Ревью r1, M2: начало круга слоёв — свой порядок слоёв каждой ночи: при `limit` меньше числа
/// слоёв (малый `batch_records`, хвост предела) поздние по имени слои не голодают — за несколько
/// ночей в выборку попадает каждый, и выборки ночей различаются.
#[tokio::test]
async fn probes_layer_round_start_varies() {
    let _g = SERIAL.lock().await;
    let a = server("layers", |_| {}).await;
    let mut layer: HashMap<String, &str> = HashMap::new();
    for i in 0..2 {
        let r = insight_k(&a, "knowledge", &format!("Ловушка {i}"), "текст", "gotcha").await;
        layer.insert(r, "insight:gotcha");
        let r = insight_k(
            &a,
            "knowledge",
            &format!("Решение {i}"),
            "текст",
            "decision",
        )
        .await;
        layer.insert(r, "insight:decision");
        let r = note_by(
            &a,
            Some(&a.token),
            "knowledge",
            &format!("kb/n{i}.md"),
            "# Заметка\n\nтекст\n",
        )
        .await;
        layer.insert(r, "note");
        let r = term(&a, "knowledge", &format!("Понятие{i}")).await;
        layer.insert(r, "term");
    }
    let kn = space(&a, SpaceKind::Knowledge).await;
    let c = ctx(&a);
    let mut seen: HashSet<&str> = HashSet::new();
    let mut nights: HashSet<Vec<&str>> = HashSet::new();
    for n in 1..=12 {
        let got = probes::collect_on(&c, a.st.admin_id, kn, day(n), 3, &none())
            .await
            .unwrap();
        assert_eq!(got.len(), 3);
        let mut layers: Vec<&str> = got.iter().map(|c| layer[&c.target]).collect();
        assert_eq!(
            layers.iter().collect::<HashSet<_>>().len(),
            3,
            "круг: три разных слоя"
        );
        seen.extend(layers.iter().copied());
        layers.sort_unstable();
        nights.insert(layers);
    }
    assert_eq!(seen.len(), 4, "каждый слой попадает в выборку: {seen:?}");
    assert!(nights.len() > 1, "выборки ночей различаются: {nights:?}");
}

// ---------------- проверка итога ----------------

/// §6.4 п. 2: проверка пробного вопроса — 7 и 26 слов, 301 символ, путь (и Windows), ссылка,
/// дата, URL (и `www.`), обратные кавычки — отказ; 25 слов и 300 символов — приняты; `overlap` 4
/// из 8 (0,5) — принят, 13 из 25 (0,52) — «пересказ заголовка»; повтор (у активной пробы
/// пространства и в том же итоге) — отказ; маска секрета (`‹secret›`, `‹secret-1›`) — отказ;
/// ссылка не из пакета — отказ с присланной ссылкой. Принятая проба агента — автор `agent`,
/// сессия, `overlap` пробы.
#[tokio::test]
async fn probes_validator_cases() {
    let _g = SERIAL.lock().await;
    let a = server("validator", |c| c.works.batch_records = 30).await;
    agent_mode(&a).await;
    let kn = space(&a, SpaceKind::Knowledge).await;
    let dup_db = "Какой порядок действий при восстановлении базы памяти из копии сервера?";
    let fake = format!("i:{}", Id::new());
    let db0 = db(&a).await;
    zam_store::probes::insert_probe(
        &db0,
        NewProbe {
            space: Some(kn),
            question: dup_db.into(),
            kind: ProbeKind::Probe,
            targets: vec![ProbeTarget {
                target: fake,
                rev: 1,
            }],
            split: zam_store::probes::Split::Dev,
            overlap: Some(0.0),
            author_kind: ProbeAuthor::Agent,
            author: None,
            work_id: None,
        },
    )
    .await
    .unwrap();
    let w25: Vec<String> = (0..25).map(|i| format!("k{i:02}abcd")).collect();
    let mut chars301 = vec!["ш".repeat(30); 9].join(" ");
    chars301.push(' ');
    chars301.push_str(&"щ".repeat(22));
    assert_eq!(chars301.chars().count(), 301);
    // границы, которые принимаются (ревью r1, M7): ровно 25 слов и ровно 300 символов
    let words25 = (0..25)
        .map(|i| format!("слово{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars300 = vec!["ш".repeat(30); 9].join(" ");
    chars300.push(' ');
    chars300.push_str(&"щ".repeat(21));
    assert_eq!(chars300.chars().count(), 300);
    let q1 = "Почему ночная проверка памяти пропускает базу без проб с целью?";
    // (заголовок записи, вопрос, ожидание: None — принят, Some(часть причины) — отказ)
    let cases: Vec<(String, String, Option<&str>)> = vec![
        (
            "Тема номер 1".into(),
            "Как настроить ночную проверку памяти на сервере".into(),
            Some("слов в вопросе: 7 "),
        ),
        (
            "Тема номер 2".into(),
            (0..26)
                .map(|i| format!("слово{i}"))
                .collect::<Vec<_>>()
                .join(" "),
            Some("слов в вопросе: 26 "),
        ),
        (
            "Тема номер 3".into(),
            chars301,
            Some("символов в вопросе: 301 "),
        ),
        (
            "Тема номер 4".into(),
            "Что именно запускает файл src/main.rs при старте сервера памяти?".into(),
            Some("путь"),
        ),
        (
            "Тема номер 5".into(),
            "Что говорит запись i:01M7Z8KVMZEMRB634C4VMPY7FQ о ночной проверке памяти?".into(),
            Some("ссылка"),
        ),
        (
            "Тема номер 6".into(),
            "Что изменилось в ночной проверке памяти после 2026-11-23 на сервере?".into(),
            Some("дата"),
        ),
        (
            "Тема номер 7".into(),
            "Где описана настройка подключения модели, например https://x для сервера памяти?"
                .into(),
            Some("URL"),
        ),
        (
            "Тема номер 8".into(),
            "Как работает команда `zam check run` при ручном прогоне проверки памяти?".into(),
            Some("обратные кавычки"),
        ),
        (
            "alpha bravo charlie delta".into(),
            "alpha bravo charlie delta echoes foxtrot golfing hotels".into(),
            None,
        ),
        (w25[..13].join(" "), w25.join(" "), Some(REASON_RETELL)),
        ("Тема номер 9".into(), dup_db.into(), Some(REASON_DUPLICATE)),
        ("Тема номер 10".into(), q1.into(), None),
        (
            "Тема номер 11".into(),
            "почему  ночная проверка памяти   пропускает базу без проб с целью?".into(),
            Some(REASON_DUPLICATE),
        ),
        (
            "Тема номер 12".into(),
            "Где хранится ‹secret› для подключения подставной модели к серверу памяти?".into(),
            Some(MASK_LITERAL_REASON),
        ),
        ("Тема номер 13".into(), words25, None),
        ("Тема номер 14".into(), chars300, None),
        (
            "Тема номер 15".into(),
            "Где хранится ‹secret-1› для подключения подставной модели к серверу памяти?".into(),
            Some(MASK_LITERAL_REASON),
        ),
        (
            "Тема номер 16".into(),
            "Что хранит файл C:\\data\\zam\\config.toml на сервере памяти сейчас?".into(),
            Some("путь"),
        ),
        (
            "Тема номер 17".into(),
            "Где описана настройка модели на сайте www.example.com для сервера памяти?".into(),
            Some("URL"),
        ),
    ];
    let mut refs = Vec::new();
    for (title, _, _) in &cases {
        refs.push(insight(&a, "knowledge", title, "текст записи для пробы").await);
    }
    night_of(&a, day(23)).await;
    let r = package(&a, S1).await;
    ok(&r, StatusCode::OK);
    let text = r.json()["text"].as_str().unwrap().to_string();
    assert!(text.contains(INSTRUCTION_RU), "{text}");
    assert!(text.contains(SCHEMA), "{text}");
    let work = r.json()["work"].as_str().unwrap().to_string();
    let mut items: Vec<Value> = cases
        .iter()
        .zip(&refs)
        .map(|((_, q, _), t)| json!({"target": t, "question": q}))
        .collect();
    // ссылка не из пакета (ревью r1, M6): в отказе — присланная строка
    let stranger = format!("i:{}", Id::new());
    items.push(json!({"target": stranger, "question": q1}));
    let resp = put_result(&a, S1, &work, json!({"items": items})).await;
    ok(&resp, StatusCode::OK);
    let resp = resp.json();
    let why = reasons(&resp);
    assert!(
        why.get(&stranger)
            .is_some_and(|r| r.contains("не запись пакета")),
        "{resp}"
    );
    let mut accepted = Vec::new();
    for ((title, _, want), t) in cases.iter().zip(&refs) {
        match want {
            None => {
                assert!(!why.contains_key(t), "{title}: {why:?}");
                accepted.push(t.clone());
            }
            Some(part) => {
                let reason = why
                    .get(t)
                    .unwrap_or_else(|| panic!("{title}: не отклонён {resp}"));
                assert!(reason.contains(part), "{title}: {reason}");
            }
        }
    }
    assert_eq!(resp["accepted"].as_array().unwrap().len(), 4, "{resp}");
    let ps = probes_of(&db(&a).await).await;
    let mine: Vec<&Probe> = ps.iter().filter(|p| p.work_id.is_some()).collect();
    assert_eq!(mine.len(), 4, "{ps:?}");
    for p in &mine {
        assert!(accepted.contains(&p.targets[0].target), "{p:?}");
        assert_eq!(p.author_kind, ProbeAuthor::Agent);
        assert_eq!(p.author.as_deref(), Some(S1));
        assert_eq!(p.space, Some(kn));
        assert_eq!(p.targets[0].rev, 1);
        assert_eq!(p.split, zam_search::eval::probe_split(&p.targets[0].target));
    }
    let half = mine
        .iter()
        .find(|p| p.question.starts_with("alpha"))
        .unwrap();
    assert!((half.overlap.unwrap() - 0.5).abs() < 1e-9, "{half:?}");
}

/// §6.4 п. 2: одна проба на запись, две — на запись длиннее 2000 символов (сигнал
/// `вопросов: 2`): 2001 символ — две пробы (третья — отказ), 1999 — вторая отказ.
#[tokio::test]
async fn probes_two_for_long_record() {
    let _g = SERIAL.lock().await;
    let a = server("long", |_| {}).await;
    agent_mode(&a).await;
    let long_body = format!("{}я", "я ".repeat(1000));
    let short_body = format!("{}я", "я ".repeat(999));
    assert_eq!(long_body.chars().count(), 2001);
    assert_eq!(short_body.chars().count(), 1999);
    let long = insight(&a, "knowledge", "Длинная запись", &long_body).await;
    let short = insight(&a, "knowledge", "Короткая запись", &short_body).await;
    night_of(&a, day(23)).await;
    let w = probe_works(&a).await.remove(0);
    let sig = |t: &str| {
        w.items
            .iter()
            .find(|i| i.target == t)
            .unwrap()
            .signals
            .clone()
    };
    assert_eq!(
        sig(&long),
        vec![(SIGNAL_QUESTIONS.to_string(), "2".to_string())]
    );
    assert!(sig(&short).is_empty());
    let q = |s: &str| format!("Как устроена {s} часть ночной проверки памяти на сервере?");
    let resp = answer(
        &a,
        S1,
        json!([
            {"target": long, "question": q("первая")},
            {"target": long, "question": q("вторая")},
            {"target": long, "question": q("третья")},
            {"target": short, "question": q("короткая")},
            {"target": short, "question": q("лишняя")},
        ]),
    )
    .await;
    let rej = resp["rejected"].as_array().unwrap();
    assert_eq!(rej.len(), 2, "{resp}");
    assert_eq!(rej[0]["target"], long.as_str());
    assert_eq!(rej[0]["reason"], REASON_TWO_PER_RECORD);
    assert_eq!(rej[1]["target"], short.as_str());
    assert_eq!(rej[1]["reason"], REASON_ONE_PER_RECORD);
    let ps = probes_of(&db(&a).await).await;
    let on = |t: &str| ps.iter().filter(|p| p.targets[0].target == t).count();
    assert_eq!((on(&long), on(&short)), (2, 1), "{ps:?}");
}

// ---------------- пределы ----------------

/// §6.3 п. 2, §6.4 п. 1: 40 новых проб за ночь — на пользователя, сколько бы ни было решений:
/// постановка агента — 25 + 15 записей (остаток предела за вычетом поставленного); итог сам
/// сверяет предел (проба другого итога пришла раньше — отказ «предел»); исчерпанный предел —
/// следующая ночь работ не ставит; у модели ночь останавливается на пределе (обращений — 2).
#[tokio::test]
async fn probes_limits_per_user() {
    let _g = SERIAL.lock().await;
    let a = server("limits", |c| {
        night_now(c);
        c.works.batch_records = 25;
    })
    .await;
    agent_mode(&a).await;
    for g in [G1, G2] {
        ok(&register(&a, &a.token, g).await, StatusCode::CREATED);
        for i in 0..25 {
            insight(&a, g, &format!("Запись {g} {i}"), "текст").await;
        }
    }
    night_in_window(&a).await;
    let mut ws = probe_works(&a).await;
    ws.reverse();
    assert_eq!(ws.len(), 2, "{ws:?}");
    assert!(ws.iter().all(|w| w.origin == Origin::Nightly));
    // постановка вычитает поставленное в этом же проходе (ревью r1, M3): 25 + 15, не 25 + 25
    assert_eq!(
        ws.iter().map(|w| w.items.len()).collect::<Vec<_>>(),
        vec![25, 15]
    );
    let g1 = solution(&a, G1).await;
    let guid = |w: &store::Work| if w.space_id == g1 { G1 } else { G2 };
    let all = |p: usize, k: usize| -> Value {
        json!(
            (1..=k)
                .map(|n| json!({"n": n, "question": good_q(p, n)}))
                .collect::<Vec<_>>()
        )
    };
    let first = answer_in(&a, "probes-limit-1", Some(guid(&ws[0])), all(0, 25)).await;
    assert_eq!(first["accepted"].as_array().unwrap().len(), 25, "{first}");
    // итог проверяет предел сам: проба другого итога пришла раньше — последняя из 15 — отказ
    seed(
        &a,
        space(&a, SpaceKind::Knowledge).await,
        Origin::Nightly,
        1,
    )
    .await;
    let second = answer_in(&a, "probes-limit-2", Some(guid(&ws[1])), all(1, 15)).await;
    assert_eq!(second["accepted"].as_array().unwrap().len(), 14, "{second}");
    let rej = second["rejected"].as_array().unwrap();
    assert_eq!(rej.len(), 1, "{second}");
    assert!(
        rej[0]["reason"]
            .as_str()
            .unwrap()
            .contains(&format!("предел новых проб за ночь — {PROBES_PER_NIGHT}")),
        "{second}"
    );
    assert_eq!(made(&a, Origin::Nightly).await, PROBES_PER_NIGHT);
    // предел исчерпан — ночь работ вида не ставит (другая дата: постановка пространства за эту
    // ночь — не повод; записей без проб — 11)
    let before = probe_works(&a).await.len();
    night_of(&a, day(25)).await;
    assert_eq!(probe_works(&a).await.len(), before);
    // режим модели: ночь останавливается на пределе — второй пакет только до него
    let m = MockModel::start();
    let b = server("limits-model", |c| {
        night_now(c);
        c.works.batch_records = 25;
    })
    .await;
    for g in [G1, G2] {
        ok(&register(&b, &b.token, g).await, StatusCode::CREATED);
        for i in 0..25 {
            insight(&b, g, &format!("Запись {g} {i}"), "текст").await;
        }
    }
    model_mode(&b, &m, &b.token, None).await;
    m.push(reply(1..=25, |n| good_q(1, n)));
    m.push(reply(1..=25, |n| good_q(2, n)));
    m.set_default(MockReply::chat("{\"items\": []}"));
    night_in_window(&b).await;
    let ws = probe_works(&b).await;
    assert_eq!(m.count(), 2, "{ws:?}");
    let mut sizes: Vec<usize> = ws.iter().map(|w| w.items.len()).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![15, 25]);
    assert_eq!(made(&b, Origin::Nightly).await, PROBES_PER_NIGHT);
    assert!(ws.iter().all(|w| w.state == WorkState::Done), "{ws:?}");
}

/// §6.3 п. 2, решение 44: пределы ночи и ручных запусков — свои: 39 ручных проб за сутки не
/// мешают ночной; 40 ночных не мешают ручному запуску, а 40-я ручная — последняя (вторая проба
/// длинной записи — отказ «предел ручных»); после 40 ночных ночь работ не ставит.
#[tokio::test]
async fn probes_manual_limit_separate() {
    let _g = SERIAL.lock().await;
    let a = server("manual-limit", |_| {}).await;
    agent_mode(&a).await;
    let kn = space(&a, SpaceKind::Knowledge).await;
    seed(&a, kn, Origin::Manual, 39).await;
    let r1 = insight(&a, "knowledge", "Первая запись", "текст").await;
    night_of(&a, day(23)).await;
    let resp = answer(&a, S1, json!([{"target": r1, "question": good_q(1, 1)}])).await;
    assert_eq!(resp["accepted"], json!([1]), "{resp}");
    assert_eq!(made(&a, Origin::Nightly).await, 1);
    seed(&a, kn, Origin::Nightly, 39).await;
    assert_eq!(made(&a, Origin::Nightly).await, PROBES_PER_NIGHT);
    let long = insight(
        &a,
        "knowledge",
        "Длинная запись",
        &format!("{}я", "я ".repeat(1000)),
    )
    .await;
    let r = a
        .adm(Method::POST, RUN, Some(json!({"kind": "probes"})), None)
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(
        r.json()["queued"].as_array().unwrap().len(),
        1,
        "{}",
        r.text()
    );
    let resp = answer(
        &a,
        "probes-manual-1",
        json!([
            {"target": long, "question": good_q(2, 1)},
            {"target": long, "question": good_q(2, 2)},
        ]),
    )
    .await;
    assert_eq!(resp["accepted"].as_array().unwrap().len(), 1, "{resp}");
    let reason = resp["rejected"][0]["reason"].as_str().unwrap().to_string();
    assert!(
        reason.contains(&format!("ручных запусков — {PROBES_PER_DAY_MANUAL}")),
        "{reason}"
    );
    assert_eq!(made(&a, Origin::Manual).await, PROBES_PER_DAY_MANUAL);
    assert_eq!(made(&a, Origin::Nightly).await, PROBES_PER_NIGHT);
    // ночь с исчерпанным пределом работ не ставит
    insight(&a, "knowledge", "Новая запись", "текст").await;
    let before = probe_works(&a).await.len();
    night_of(&a, day(24)).await;
    assert_eq!(probe_works(&a).await.len(), before);
}

// ---------------- что уходит наружу ----------------

/// §6 «Общее», решение 62: записи, у которых автор любой ревизии — участник общего решения, в
/// пакеты не берутся — и созданные участником, и смешанные (создал владелец, правил участник).
#[tokio::test]
async fn probes_skip_member_records() {
    let _g = SERIAL.lock().await;
    let a = server("member", |_| {}).await;
    agent_mode(&a).await;
    ok(&register(&a, &a.token, G1).await, StatusCode::CREATED);
    let (member, tok) = a.new_user("Участник").await;
    assert_eq!(register(&a, &tok, G1).await.status, StatusCode::ACCEPTED);
    a.set_access(&a.token, G1, &member, "rw").await;
    let own_i = insight(&a, G1, "Знание владельца", "текст владельца").await;
    let own_n = note_by(&a, Some(&a.token), G1, "own.md", "# Владелец\n\nтекст\n").await;
    let mixed_i = insight(&a, G1, "Правил участник", "текст владельца").await;
    let mixed_n = note_by(&a, Some(&a.token), G1, "mixed.md", "# Смешанная\n\nтекст\n").await;
    let member_i = entry_by(
        &a,
        &tok,
        G1,
        json!({"kind": "insight", "title": "Знание участника", "body": "текст участника",
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let member_n = note_by(&a, Some(&tok), G1, "member.md", "# Участник\n\nтекст\n").await;
    revise(&a, &tok, &mixed_i, "правка участника").await;
    let nid = mixed_n.split_once(':').unwrap().1;
    let r = a
        .tok_h(
            &tok,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content": "# Смешанная\n\nправка участника\n"})),
            &[("if-match", "\"1\"")],
        )
        .await;
    ok(&r, StatusCode::OK);
    let g1 = solution(&a, G1).await;
    let got: HashSet<String> =
        probes::collect_on(&ctx(&a), a.st.admin_id, g1, day(23), 100, &none())
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.target)
            .collect();
    assert_eq!(got, HashSet::from([own_i.clone(), own_n.clone()]));
    for t in [&mixed_i, &mixed_n, &member_i, &member_n] {
        assert!(!got.contains(t), "{t}");
    }
    night_of(&a, day(23)).await;
    let w = probe_works(&a)
        .await
        .into_iter()
        .find(|w| w.space_id == g1)
        .expect("работа решения");
    let items: HashSet<String> = w.items.iter().map(|i| i.target.clone()).collect();
    assert_eq!(items, HashSet::from([own_i, own_n]));
}

/// §6.3 п. 7: проба модели — `author_kind = model`, `author = "model:<имя>"`, работа, цель с
/// ревизией, часть по цели.
#[tokio::test]
async fn probes_written_by_model_author() {
    let _g = SERIAL.lock().await;
    let m = MockModel::start();
    let a = server("model-author", night_now).await;
    let target = insight(&a, "knowledge", "Запись для модели", "текст записи").await;
    model_mode(&a, &m, &a.token, None).await;
    m.push(reply(1..=1, |_| good_q(1, 1)));
    m.set_default(MockReply::chat("{\"items\": []}"));
    night_in_window(&a).await;
    assert_eq!(m.count(), 1);
    let w = probe_works(&a).await.remove(0);
    assert_eq!(w.state, WorkState::Done, "{w:?}");
    assert_eq!(w.mode, WorkMode::Model);
    let ps = probes_of(&db(&a).await).await;
    assert_eq!(ps.len(), 1, "{ps:?}");
    let p = &ps[0];
    assert_eq!(p.author_kind, ProbeAuthor::Model);
    assert_eq!(p.author.as_deref(), Some("model:mock-model"));
    assert_eq!(p.work_id, Some(w.id));
    assert_eq!(p.kind, ProbeKind::Probe);
    assert_eq!(p.question, good_q(1, 1));
    assert_eq!(
        p.targets,
        vec![ProbeTarget {
            target: target.clone(),
            rev: 1
        }]
    );
    assert_eq!(p.split, zam_search::eval::probe_split(&target));
    assert_eq!(p.space, Some(space(&a, SpaceKind::Knowledge).await));
    assert_eq!(w.result.unwrap()["author"], "model:mock-model");
}

/// §6.2 п. 6, решение 72, Review Focus 2: элемент, выброшенный вторым сканом
/// (`secret_in_payload`), следующей ночью в пакет не входит; после новой ревизии записи — входит.
#[tokio::test]
async fn probes_secret_dropped_not_reoffered_until_new_rev() {
    let _g = SERIAL.lock().await;
    let m = MockModel::start();
    let a = server("secret", night_now).await;
    let clean = insight(&a, "knowledge", "Чистая запись", "текст").await;
    let leaky = insight(&a, "knowledge", "Запись с ключом", "текст").await;
    model_mode(&a, &m, &a.token, None).await;
    let kn = space(&a, SpaceKind::Knowledge).await;
    // снимок с ключом в тексте — мимо маски сборщика: его выбросит второй скан тела
    let item = |n: usize, target: &str, text: String| WorkItem {
        n,
        target: target.to_string(),
        rev: 1,
        kind: "insight".into(),
        title: "Запись".into(),
        text,
        signals: Vec::new(),
    };
    let s = secret();
    store::enqueue(
        &db(&a).await,
        NewWork {
            kind: WorkKind::Probes,
            space: kn,
            mode: WorkMode::Model,
            origin: Origin::Nightly,
            night: Some(zam_store::time::utc_today()),
            items: vec![
                item(1, &clean, "текст".into()),
                item(2, &leaky, format!("токен деплоя {s} в CI")),
            ],
        },
    )
    .await
    .unwrap()
    .unwrap();
    m.push(reply(1..=1, |_| good_q(1, 1)));
    m.set_default(MockReply::chat("{\"items\": []}"));
    night_in_window(&a).await;
    let body = m.requests()[0].body.clone();
    assert!(!body.contains(&s), "{body}");
    let first = probe_works(&a).await.remove(0);
    assert_eq!(first.state, WorkState::Done, "{first:?}");
    let rej = first.result.as_ref().unwrap()["rejected"].clone();
    assert_eq!(rej[0]["target"], leaky.as_str(), "{rej}");
    assert_eq!(rej[0]["reason"], store::SECRET_IN_PAYLOAD);
    // следующая ночь: чистая запись опробована, запись с ключом — до новой ревизии вне пакетов
    let c = ctx(&a);
    let got = probes::collect_on(&c, a.st.admin_id, kn, day(24), 100, &none())
        .await
        .unwrap();
    assert!(got.is_empty(), "{got:?}");
    night_in_window(&a).await;
    let ws = probe_works(&a).await;
    assert_eq!(ws.len(), 1, "{ws:?}");
    assert_eq!(m.count(), 1);
    revise(&a, &a.token, &leaky, "новый текст без ключа").await;
    let got = probes::collect_on(&c, a.st.admin_id, kn, day(24), 100, &none())
        .await
        .unwrap();
    assert_eq!(
        got.iter()
            .map(|c| (c.target.clone(), c.rev))
            .collect::<Vec<_>>(),
        vec![(leaky.clone(), 2)]
    );
    m.push(reply(1..=1, |_| good_q(1, 2)));
    night_in_window(&a).await;
    let ws = probe_works(&a).await;
    assert_eq!(ws.len(), 2, "{ws:?}");
    assert_eq!(ws[0].items[0].target, leaky);
    assert_eq!(ws[0].items[0].rev, 2);
    assert_eq!(ws[0].state, WorkState::Done);
    assert_eq!(m.count(), 2);
}

/// Решение 72: запись, которую исполнитель отклонил явным `skip`, до новой ревизии в пакеты не
/// берётся; после неё — снова.
#[tokio::test]
async fn probes_skip_not_reoffered_until_new_rev() {
    let _g = SERIAL.lock().await;
    let a = server("skip", |_| {}).await;
    agent_mode(&a).await;
    let r1 = insight(&a, "knowledge", "Запись с вопросом", "текст").await;
    let r2 = insight(&a, "knowledge", "Запись без вопроса", "текст").await;
    night_of(&a, day(23)).await;
    let resp = answer(
        &a,
        S1,
        json!([
            {"target": r1, "question": good_q(1, 1)},
            {"target": r2, "skip": "о записи нельзя спросить"},
        ]),
    )
    .await;
    assert_eq!(resp["state"], "done", "{resp}");
    assert_eq!(resp["skip"][0]["target"], r2.as_str(), "{resp}");
    let excluded = store::excluded_refs(&db(&a).await, WorkKind::Probes)
        .await
        .unwrap();
    assert!(excluded.contains(&(r2.clone(), 1)), "{excluded:?}");
    night_of(&a, day(24)).await;
    assert_eq!(
        probe_works(&a).await.len(),
        1,
        "отказ держит запись вне пакетов"
    );
    revise(&a, &a.token, &r2, "дописано: теперь есть о чём спросить").await;
    night_of(&a, day(25)).await;
    let ws = probe_works(&a).await;
    assert_eq!(ws.len(), 2, "{ws:?}");
    assert_eq!(
        ws[0]
            .items
            .iter()
            .map(|i| (i.target.clone(), i.rev))
            .collect::<Vec<_>>(),
        vec![(r2, 2)]
    );
}

// ---------------- круг правок 1 (ревью r1) ----------------

/// Ревью r1, I1 (решение координатора): записи, уже отправленные в пакете этой ночью, — с любым
/// исходом (без итога, отклонённые проверкой) — той же ночью снова не предлагаются: вторая
/// постановка той же ночи — ни одного обращения к подставной модели; другой ночью — снова
/// кандидаты.
#[tokio::test]
async fn probes_offered_not_reoffered_same_night() {
    let _g = SERIAL.lock().await;
    let m = MockModel::start();
    m.set_default(MockReply::chat("{\"items\": []}"));
    let a = server("offered", night_now).await;
    let r1 = insight(&a, "knowledge", "Первая запись", "текст").await;
    let r2 = insight(&a, "knowledge", "Вторая запись", "текст").await;
    model_mode(&a, &m, &a.token, None).await;
    night_in_window(&a).await;
    assert_eq!(m.count(), 1);
    let w = probe_works(&a).await.remove(0);
    assert_eq!(w.state, WorkState::Done, "{w:?}");
    let skipped: HashSet<String> = w.result.as_ref().unwrap()["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["target"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(skipped, HashSet::from([r1.clone(), r2.clone()]));
    // та же ночь: молчание модели — не повод отправить те же записи снова
    night_in_window(&a).await;
    assert_eq!(m.count(), 1);
    assert_eq!(probe_works(&a).await.len(), 1);
    // отказ проверки (пересказ заголовка) — та же ночь без повторного пакета
    let r3 = insight(&a, "knowledge", "Резервная копия сервера памяти", "текст").await;
    m.push(reply(1..=1, |_| {
        "Где лежит резервная копия сервера памяти и как часто она делается?".into()
    }));
    night_in_window(&a).await;
    assert_eq!(m.count(), 2);
    let w = probe_works(&a).await.remove(0);
    assert_eq!(
        w.items.iter().map(|i| i.target.clone()).collect::<Vec<_>>(),
        vec![r3.clone()]
    );
    assert_eq!(
        w.result.as_ref().unwrap()["rejected"][0]["reason"],
        REASON_RETELL
    );
    night_in_window(&a).await;
    assert_eq!(
        m.count(),
        2,
        "отклонённая проверкой — той же ночью снова не идёт"
    );
    // отсев — по ночи: этой ночью предложены все три, другой ночью — снова кандидаты
    let wc = &a.st.cfg.works;
    let (d, start, _) = zam_server::schedule::night_window(
        zam_store::time::now(),
        wc.at_hm(),
        &wc.time_zone(),
        wc.window_min,
    )
    .unwrap();
    let dbx = db(&a).await;
    let run = |d: civil::Date| format!("night:{d}");
    let tonight = store::offered_refs(
        &dbx,
        WorkKind::Probes,
        Origin::Nightly,
        Some(d),
        start,
        Some(&run(d)),
    )
    .await
    .unwrap();
    let all: HashSet<(String, i64)> = [&r1, &r2, &r3].iter().map(|t| ((*t).clone(), 1)).collect();
    assert_eq!(tonight, all);
    let other = d.tomorrow().unwrap();
    let later = store::offered_refs(
        &dbx,
        WorkKind::Probes,
        Origin::Nightly,
        Some(other),
        start,
        Some(&run(other)),
    )
    .await
    .unwrap();
    assert!(later.is_empty(), "{later:?}");
    let kn = space(&a, SpaceKind::Knowledge).await;
    let c = ctx(&a);
    let got = kinds::collect(WorkKind::Probes, &c, a.st.admin_id, kn, 10, &later)
        .await
        .unwrap();
    assert_eq!(
        got.into_iter()
            .map(|c| (c.target, c.rev))
            .collect::<HashSet<_>>(),
        all
    );
    let none_left = kinds::collect(WorkKind::Probes, &c, a.st.admin_id, kn, 10, &tonight)
        .await
        .unwrap();
    assert!(none_left.is_empty(), "{none_left:?}");
}

/// Ревью r2, N2: «отправлено этой ночью» — и работа прошлой ночи, возвращённая в очередь и
/// выполненная моделью этой ночью (обращение с `work_calls.run` этой ночи): записи, о которых
/// модель промолчала, в ту же ночь второй раз не уходят.
#[tokio::test]
async fn probes_requeued_last_night_counts_as_offered() {
    let _g = SERIAL.lock().await;
    let m = MockModel::start();
    m.set_default(MockReply::chat("{\"items\": []}"));
    let a = server("requeued-offered", night_now).await;
    let r1 = insight(&a, "knowledge", "Первая запись", "текст").await;
    let r2 = insight(&a, "knowledge", "Вторая запись", "текст").await;
    model_mode(&a, &m, &a.token, None).await;
    let wc = &a.st.cfg.works;
    let (d, _, _) = zam_server::schedule::night_window(
        zam_store::time::now(),
        wc.at_hm(),
        &wc.time_zone(),
        wc.window_min,
    )
    .unwrap();
    let item = |n: usize, target: &str| WorkItem {
        n,
        target: target.to_string(),
        rev: 1,
        kind: "insight".into(),
        title: "Запись".into(),
        text: "текст".into(),
        signals: Vec::new(),
    };
    // работа прошлой ночи, оставшаяся в очереди (пауза, стоп): этой ночью она идёт первой
    let old = store::enqueue(
        &db(&a).await,
        NewWork {
            kind: WorkKind::Probes,
            space: space(&a, SpaceKind::Knowledge).await,
            mode: WorkMode::Model,
            origin: Origin::Nightly,
            night: Some(d.yesterday().unwrap()),
            items: vec![item(1, &r1), item(2, &r2)],
        },
    )
    .await
    .unwrap()
    .unwrap();
    night_in_window(&a).await;
    assert_eq!(m.count(), 1, "только работа прошлой ночи");
    let ws = probe_works(&a).await;
    assert_eq!(ws.len(), 1, "{ws:?}");
    assert_eq!(ws[0].id, old.id);
    assert_eq!(ws[0].state, WorkState::Done);
}

/// Ревью r1, M5: заголовок заметки для пакета и проверки — её первый H1 и путь: пересказ H1
/// отклоняется «пересказ заголовка». Ревью r2, N4: заголовок — разбором Markdown: строка кода с
/// отступом — не заголовок, setext-заголовок (`===`) — заголовок.
#[tokio::test]
async fn probes_note_title_includes_heading() {
    let _g = SERIAL.lock().await;
    let a = server("note-title", |_| {}).await;
    agent_mode(&a).await;
    let n = note_by(
        &a,
        Some(&a.token),
        "knowledge",
        "kb/backup.md",
        "---\ntags: [ops]\n---\n# Восстановление базы из резервной копии сервера\n\nШаги и проверка.\n",
    )
    .await;
    let setext = note_by(
        &a,
        Some(&a.token),
        "knowledge",
        "kb/restore.md",
        "    # строка кода с отступом\n\nВосстановление из архива\n===\n\nШаги.\n",
    )
    .await;
    night_of(&a, day(23)).await;
    let w = probe_works(&a).await.remove(0);
    let title = |t: &str| {
        w.items
            .iter()
            .find(|i| i.target == t)
            .unwrap()
            .title
            .clone()
    };
    assert_eq!(
        title(&n),
        "Восстановление базы из резервной копии сервера (kb/backup.md)"
    );
    assert_eq!(title(&setext), "Восстановление из архива (kb/restore.md)");
    let resp = answer(
        &a,
        S1,
        json!([{"target": n,
                "question": "Как выполнить восстановление базы из резервной копии сервера?"}]),
    )
    .await;
    assert_eq!(resp["rejected"][0]["reason"], REASON_RETELL, "{resp}");
}

/// ⚠️ ревью r1: граница «пересказа заголовка» на кириллице — нормализация как у слов подсказок
/// (§6.4 п. 2): словоформы по основам, регистр, `ё` = `е`, служебные слова не считаются; ровно
/// 0,5 (4 из 8) — принят, 5 из 9 — отказ.
#[test]
fn probes_overlap_cyrillic_boundary() {
    let title = "Резервная копия сервера: учёт памяти";
    let half = "Как ночная проверка РЕЗЕРВНОЙ копии серверу для учета занимает время?";
    let ov = zam_search::eval::overlap(half, title);
    assert!((ov - 0.5).abs() < 1e-9, "{ov}");
    let got = probes::check_question(half, title).unwrap();
    assert!((got - 0.5).abs() < 1e-9, "{got}");
    let above = "Как ночная проверка РЕЗЕРВНОЙ копии серверу для учёта ПАМЯТИ занимает время?";
    let ov = zam_search::eval::overlap(above, title);
    assert!((ov - 5.0 / 9.0).abs() < 1e-9, "{ov}");
    assert_eq!(
        probes::check_question(above, title).unwrap_err().reason,
        REASON_RETELL
    );
}

/// ⚠️ ревью r1, M1: запись принятых проб — в транзакции закрытия работы, у одного писателя базы:
/// тот же вопрос, записанный другим путём до закрытия, не дублируется; предел ночи, исчерпанный к
/// закрытию, — проба не пишется; запись, чья проба не записалась, следующей ночью — снова кандидат.
#[tokio::test]
async fn probes_write_rechecks_in_transaction() {
    let _g = SERIAL.lock().await;
    let a = server("recheck", |_| {}).await;
    let r = insight(&a, "knowledge", "Запись", "текст").await;
    let kn = space(&a, SpaceKind::Knowledge).await;
    let dbx = db(&a).await;
    let probe = |q: &str, target: &str| NewProbe {
        space: Some(kn),
        question: q.into(),
        kind: ProbeKind::Probe,
        targets: vec![ProbeTarget {
            target: target.into(),
            rev: 1,
        }],
        split: zam_search::eval::probe_split(target),
        overlap: Some(0.0),
        author_kind: ProbeAuthor::Model,
        author: Some("model:mock-model".into()),
        work_id: None,
    };
    let (since, cap) = probes::quota(Origin::Nightly, zam_store::time::now(), &a.st.cfg.works);
    let result = json!({"accepted": [{"n": 1, "target": r, "rev": 1}], "rejected": [],
                        "skip": [], "skipped": []});
    let start = |dbx: zam_store::Db, r: String| async move {
        let w = store::enqueue(
            &dbx,
            NewWork {
                kind: WorkKind::Probes,
                space: kn,
                mode: WorkMode::Model,
                origin: Origin::Nightly,
                night: Some(day(23)),
                items: vec![WorkItem {
                    n: 1,
                    target: r,
                    rev: 1,
                    kind: "insight".into(),
                    title: "Запись".into(),
                    text: "текст".into(),
                    signals: Vec::new(),
                }],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(store::start_model(&dbx, w.id).await.unwrap());
        w.id
    };
    // 1. тот же вопрос записан другим путём, пока шла работа: закрытие его не дублирует
    let q = good_q(1, 1);
    let w1 = start(dbx.clone(), r.clone()).await;
    let fake = format!("i:{}", Id::new());
    zam_store::probes::insert_probe(&dbx, probe(&q, &fake))
        .await
        .unwrap();
    let closed = store::finish_claimed(
        &dbx,
        w1,
        None,
        WorkState::Done,
        result.clone(),
        None,
        vec![store::ItemWrite::Probe {
            probe: probe(&q, &r),
            since,
            cap,
        }],
    )
    .await
    .unwrap();
    assert_eq!(closed, store::Closed::Closed);
    let same: Vec<Probe> = probes_of(&dbx)
        .await
        .into_iter()
        .filter(|p| p.question == q)
        .collect();
    assert_eq!(same.len(), 1, "{same:?}");
    assert_eq!(same[0].targets[0].target, fake);
    // 2. предел ночи исчерпан к закрытию: проба не пишется
    seed(&a, kn, Origin::Nightly, PROBES_PER_NIGHT as usize).await;
    let w2 = start(dbx.clone(), r.clone()).await;
    let q2 = good_q(2, 1);
    let closed = store::finish_claimed(
        &dbx,
        w2,
        None,
        WorkState::Done,
        result.clone(),
        None,
        vec![store::ItemWrite::Probe {
            probe: probe(&q2, &r),
            since,
            cap,
        }],
    )
    .await
    .unwrap();
    assert_eq!(closed, store::Closed::Closed);
    let ps = probes_of(&dbx).await;
    assert!(ps.iter().all(|p| p.question != q2), "{ps:?}");
    assert!(ps.iter().all(|p| p.targets[0].target != r), "{ps:?}");
    // 3. проба записи не записалась — следующей ночью запись снова кандидат (принятый элемент
    // итога её не держит)
    let got = probes::collect_on(&ctx(&a), a.st.admin_id, kn, day(24), 10, &none())
        .await
        .unwrap();
    assert_eq!(
        got.iter().map(|c| c.target.clone()).collect::<Vec<_>>(),
        vec![r]
    );
}

/// ⚠️ ревью r1: параллельный повтор итога с тем же `Idempotency-Key` — один итог и одна запись
/// проб (замок работы, один писатель); параллельные итоги двух работ у предела ночи — проб не
/// больше 40.
#[tokio::test]
async fn probes_parallel_results_once() {
    let _g = SERIAL.lock().await;
    let a = server("parallel", |c| c.works.batch_records = 25).await;
    agent_mode(&a).await;
    let r1 = insight(&a, "knowledge", "Первая запись", "текст").await;
    let r2 = insight(&a, "knowledge", "Вторая запись", "текст").await;
    night_of(&a, day(23)).await;
    let r = package(&a, S1).await;
    let work = r.json()["work"].as_str().unwrap().to_string();
    let body = json!({"items": [{"target": r1, "question": good_q(1, 1)},
                                {"target": r2, "question": good_q(1, 2)}]});
    let (x, y) = tokio::join!(
        put_result_key(&a, S1, &work, body.clone(), "probes-parallel"),
        put_result_key(&a, S1, &work, body.clone(), "probes-parallel"),
    );
    ok(&x, StatusCode::OK);
    ok(&y, StatusCode::OK);
    assert_eq!(x.json(), y.json());
    let mine = probes_of(&db(&a).await)
        .await
        .into_iter()
        .filter(|p| p.work_id.is_some())
        .count();
    assert_eq!(mine, 2);
    // две работы у предела: 37 готовых + 2 принятых = 39, по вопросу в каждой работе —
    // параллельно, записано ровно до 40
    for g in [G1, G2] {
        ok(&register(&a, &a.token, g).await, StatusCode::CREATED);
        insight(&a, g, &format!("Запись {g}"), "текст").await;
    }
    night_of(&a, day(24)).await;
    let open: Vec<store::Work> = probe_works(&a)
        .await
        .into_iter()
        .filter(|w| w.state == WorkState::Queued)
        .collect();
    assert_eq!(open.len(), 2, "{open:?}");
    seed(&a, space(&a, SpaceKind::Rules).await, Origin::Nightly, 37).await;
    assert_eq!(made(&a, Origin::Nightly).await, PROBES_PER_NIGHT - 1);
    let item = |p: usize| json!([{"n": 1, "question": good_q(p, 1)}]);
    let (x, y) = tokio::join!(
        answer_in(&a, "probes-parallel-1", Some(G1), item(3)),
        answer_in(&a, "probes-parallel-2", Some(G2), item(4)),
    );
    let took = |v: &Value| v["accepted"].as_array().unwrap().len();
    assert!(took(&x) + took(&y) >= 1, "{x} {y}");
    assert_eq!(made(&a, Origin::Nightly).await, PROBES_PER_NIGHT);
}

// ---------------- порядок ночи ----------------

/// §6.3 п. 2 «Проверка», §6.5 п. 1: ночь — сначала проверка памяти **всех** пользователей, затем
/// работы: у двух пользователей в режиме `model` с подставной моделью `finished_at` обоих
/// ночных прогонов раньше `created_at` первой ночной работы. Данные — до ночи (окно через 6 ч),
/// затем перезапуск с окном «сейчас», и ночь идёт тиком раннера (20 мс).
#[tokio::test]
async fn night_checks_all_users_before_works() {
    let _g = SERIAL.lock().await;
    let _reset = Reset;
    zam_server::check::test_hooks::set_pause(Duration::ZERO);
    let m = MockModel::start();
    m.set_default(reply(1..=1, |_| good_q(7, 1)));
    let a = server("night-order", |c| c.works.check = CheckMode::Nightly).await;
    let (bob, bob_tok) = a.new_user("bob").await;
    let bob_id: Id = bob.parse().unwrap();
    for (user, tok) in [(a.st.admin_id, a.token.clone()), (bob_id, bob_tok.clone())] {
        let kn = space_of(&a, user, SpaceKind::Knowledge).await;
        let r = a
            .tok(
                &tok,
                Method::POST,
                "/api/v1/nodes",
                Some(
                    json!({"space": "knowledge", "path": "kb/a.md", "kind": "note",
                            "content": "# Заметка\n\nzalpha zbravo — запись знаний.\n"}),
                ),
            )
            .await;
        ok(&r, StatusCode::CREATED);
        let udb = a.st.users.get(user).await.unwrap();
        zam_store::probes::insert_probe(
            &udb,
            NewProbe {
                space: Some(kn),
                question: "zalpha zbravo".into(),
                kind: ProbeKind::Probe,
                targets: vec![ProbeTarget {
                    target: format!("n:{}", id(&r.json())),
                    rev: 1,
                }],
                split: zam_store::probes::Split::Test,
                overlap: Some(0.0),
                author_kind: ProbeAuthor::Agent,
                author: None,
                work_id: None,
            },
        )
        .await
        .unwrap();
        entry_by(
            &a,
            &tok,
            "knowledge",
            json!({"kind": "insight", "title": "Для работы", "body": "текст",
                   "data": {"kind": "gotcha"}}),
        )
        .await;
    }
    model_mode(&a, &m, &a.token, None).await;
    model_mode(&a, &m, &bob_tok, Some(&bob)).await;
    let mut cfg = (*a.st.cfg).clone();
    night_now(&mut cfg);
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    zam_server::works::night::test_hooks::set_tick(Duration::from_millis(20));
    let mut a = api_with(cfg).await;
    a.token = token;
    let users = [a.st.admin_id, bob_id];
    let st = a.st.clone();
    until(
        "ночные работы обоих пользователей",
        Duration::from_secs(60),
        || {
            let st = st.clone();
            async move {
                for u in users {
                    let db = st.users.get(u).await.unwrap();
                    let ws = store::list(&db, None, Some(WorkKind::Probes), 10)
                        .await
                        .unwrap();
                    if !ws.iter().any(|w| w.state == WorkState::Done) {
                        return false;
                    }
                }
                true
            }
        },
    )
    .await;
    let mut checked = Vec::new();
    let mut created = Vec::new();
    for u in users {
        let db = a.st.users.get(u).await.unwrap();
        let runs = zam_store::check::runs(&db, 10).await.unwrap();
        let run = runs
            .iter()
            .find(|r| r.trigger.as_str() == "nightly")
            .unwrap_or_else(|| panic!("нет ночного прогона у {u}: {runs:?}"));
        checked.push(run.finished_at.expect("прогон закончен"));
        let ws = store::list(&db, None, Some(WorkKind::Probes), 10)
            .await
            .unwrap();
        created.extend(ws.iter().map(|w| w.created_at));
    }
    let last_check = checked.iter().max().unwrap();
    let first_work = created.iter().min().unwrap();
    assert!(
        last_check < first_work,
        "проверка {checked:?} — до работ {created:?}"
    );
    assert!(m.count() >= 2);
    a.st.shutdown().await;
}
