//! Кандидаты в Rules (M7.3, спецификация §6.7 «Проверка»): спонтанный кандидат агента
//! (`POST /proposals`, подсказка в 403 записи в Rules, пределы и повторы, токен `ro`), ночной вид
//! работы `rules` (источники и отказ исполнителя, проверка элемента, маска секрета, отсев
//! отклонённых и пропущенных, второй скан, записи участников), решение человека (принять с
//! правкой пути и текста, путь занят, токену — 403, отозвать), маска для токена, строки
//! неизвестного типа.
//!
//! Подставная модель — `common::mock_model` на `127.0.0.1` с `[works] allow_loopback = true`; ни
//! одного обращения к настоящей модели и сети. Почти-дубль по dense-косинусу требует модели и
//! Qdrant — здесь проверяется заголовком (векторный канал — `insights::dedup`, тот же путь).
//!
//! Тесты файла идут **по одному** (`SERIAL`): тестовые точки ночи и исполнения общие для процесса.
mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, ok, problem};
use common::mock_model::{MockModel, MockReply};
use jiff::SignedDuration;
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_server::config::ServerConfig;
use zam_server::works::package::MASK_LITERAL_REASON;
use zam_server::works::{NightCtx, queue};
use zam_store::works::{self as store, NewWork, Origin, WorkItem, WorkKind, WorkMode, WorkState};

const PROPOSALS: &str = "/api/v1/proposals";
const COUNTS: &str = "/api/v1/proposals/counts";
const RECALL: &str = "/api/v1/recall";
const WORKS: &str = "/api/v1/works";
const S1: &str = "rules-session-1";
const G1: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e41";
const REASON: &str = "Правило поведения агента во всех проектах, а не факт одного проекта";

/// Тесты файла — по одному (см. модуль).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Фиктивный ключ (склейка — push protection) и ключ поставщика для записи Vault.
fn secret() -> String {
    format!("gh{}_{}", "p", "zY8xW6vU4tS2rQ0pO8nM6lK4jI2hG0fE8dC6")
}

fn model_key() -> String {
    format!("{}{}", "sk-", "mockKeyNotReal9876543210")
}

/// Окно ночи вокруг «сейчас» (ночные обращения к модели идут).
fn night_now(c: &mut ServerConfig) {
    let start =
        (jiff::Timestamp::now() - SignedDuration::from_mins(10)).to_zoned(jiff::tz::TimeZone::UTC);
    c.works.at = format!("{:02}:{:02}", start.hour(), start.minute());
    c.works.tz = "UTC".into();
    c.works.window_min = 720;
}

/// Сервер теста: тик ночи — 1 ч (ночь его тиком не запускается), короткие сроки исполнения,
/// петля разрешена; `tweak` — правка конфигурации.
async fn server(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    zam_server::works::night::test_hooks::set_tick(Duration::from_secs(3600));
    zam_server::works::exec::test_hooks::set_call_timeout_ms(20_000);
    zam_server::works::exec::test_hooks::set_retry_delays_ms(10, 20);
    zam_server::works::exec::test_hooks::set_rate_unit_ms(1);
    api(&format!("rules-{name}"), |c| {
        c.works.allow_loopback = true;
        tweak(c);
    })
    .await
}

async fn db(a: &Api) -> zam_store::Db {
    a.st.users.get(a.st.admin_id).await.unwrap()
}

async fn space_id(a: &Api, kind: zam_store::SpaceKind) -> Id {
    zam_store::list_spaces(&db(a).await)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap()
        .id
}

/// Режим исполнителя администратора.
async fn set_mode(a: &Api, mode: &str) {
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/auth/settings",
            Some(json!({"works": {"mode": mode}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
}

/// Спонтанный кандидат токеном `tok` в сессии [`S1`].
async fn propose(a: &Api, tok: &str, path: &str, content: &str, reason: &str) -> Resp {
    a.tok_h(
        tok,
        Method::POST,
        PROPOSALS,
        Some(json!({"type": "rule", "path": path, "content": content, "reason": reason})),
        &[("x-zam-session", S1)],
    )
    .await
}

/// Кандидат принят (201): ссылка `o:<id>`.
async fn proposed(a: &Api, path: &str, content: &str) -> String {
    let r = propose(a, &a.token, path, content, REASON).await;
    ok(&r, StatusCode::CREATED);
    let o = r.json()["proposal"].as_str().unwrap().to_string();
    assert!(o.starts_with("o:"), "{o}");
    o
}

/// Заметка Rules от человека (WebUI); узел.
async fn rule_note(a: &Api, path: &str, content: &str) -> Value {
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "rules", "path": path, "kind": "note", "content": content})),
            None,
        )
        .await;
    ok(&r, StatusCode::CREATED);
    r.json()
}

/// Insight первым токеном в пространстве `space`: вид `kind`, достоверность `certainty`; ссылка.
async fn insight(a: &Api, space: &str, kind: &str, certainty: Option<&str>, title: &str) -> String {
    insight_by(a, &a.token, space, kind, certainty, title, &[]).await
}

async fn insight_by(
    a: &Api,
    tok: &str,
    space: &str,
    kind: &str,
    certainty: Option<&str>,
    title: &str,
    h: &[(&str, &str)],
) -> String {
    let mut body = json!({"kind": "insight", "title": title,
                          "body": format!("{title}: подробности и следствия."),
                          "data": {"kind": kind}, "force": true});
    if let Some(c) = certainty {
        body["certainty"] = json!(c);
    }
    let r = a
        .tok_h(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
            h,
        )
        .await;
    ok(&r, StatusCode::CREATED);
    format!("i:{}", r.json()["id"].as_str().unwrap())
}

fn entry_id(target: &str) -> Id {
    target.split_once(':').unwrap().1.parse().unwrap()
}

/// «Полезно» записи `n` раз (события `usage`).
async fn useful(a: &Api, target: &str, n: usize) {
    let db = db(a).await;
    for i in 0..n {
        zam_store::record_usage(
            &db,
            &zam_store::UsageEvent {
                user_id: a.st.admin_id,
                session_id: Some(format!("useful-{i}")),
                target: TargetRef {
                    kind: TargetKind::Entry,
                    id: entry_id(target),
                },
                event: zam_store::UsageKind::Useful,
                source: None,
            },
        )
        .await
        .unwrap();
    }
}

/// Открытия записи `n` сессиями (`usage_opened`).
async fn opened(a: &Api, target: &str, n: usize) {
    let db = db(a).await;
    for i in 0..n {
        zam_store::record_opened(
            &db,
            &zam_store::OpenedEvent {
                ts: zam_store::time::now(),
                user_id: a.st.admin_id,
                session_id: format!("opened-{i}"),
                target: TargetRef {
                    kind: TargetKind::Entry,
                    id: entry_id(target),
                },
                source: zam_store::UsageSource::Prompt,
            },
        )
        .await
        .unwrap();
    }
}

/// Ночь даты «сегодня + `days`» всех пользователей (режим агента окно не проверяет).
async fn night(a: &Api, days: i64) {
    let d = zam_store::time::utc_today()
        .checked_add(jiff::Span::new().days(days))
        .unwrap();
    let until = zam_store::time::now() + SignedDuration::from_hours(1);
    queue::night(&NightCtx::new(a.st.clone()), d, until)
        .await
        .unwrap();
}

/// Работы вида `rules` администратора, новые первыми.
async fn rules_works(a: &Api) -> Vec<store::Work> {
    store::list(&db(a).await, None, Some(WorkKind::Rules), 200)
        .await
        .unwrap()
}

/// Ссылки кандидатов последней работы `rules`.
async fn last_targets(a: &Api) -> Vec<String> {
    rules_works(a)
        .await
        .first()
        .map(|w| w.items.iter().map(|i| i.target.clone()).collect())
        .unwrap_or_default()
}

/// Пакет работы `rules` сессии [`S1`] и итог `items` той же сессией.
async fn do_work(a: &Api, items: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "work", "kind": "rules"})),
            &[("x-zam-session", S1)],
        )
        .await;
    ok(&r, StatusCode::OK);
    let work = r.json()["work"].as_str().unwrap().to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("{WORKS}/{work}/result"),
            Some(json!({"items": items})),
            &[("x-zam-session", S1)],
        )
        .await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// Строки `proposals` типа `rule` со статусом `status`.
async fn rule_rows(a: &Api, status: &str) -> Vec<(String, Value, String)> {
    let status = status.to_string();
    db(a)
        .await
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT targets, payload, coalesce(author, '') FROM proposals
                 WHERE type = 'rule' AND status = ?1 ORDER BY created_at, id",
            )?;
            let rows = st.query_map([status], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap(),
                    r.get::<_, String>(2)?,
                ))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

async fn counts(a: &Api) -> Value {
    let r = a.t(Method::GET, COUNTS, None).await;
    ok(&r, StatusCode::OK);
    r.json()
}

fn rule_item(n: usize, path: &str, content: &str) -> Value {
    json!({"n": n, "rule": {"path": path, "title": format!("Правило {path}"), "content": content},
           "reason": REASON})
}

/// §6.7 п. 2: `POST /proposals {type: rule, path, content, reason}` токеном `rw` → 201
/// `{proposal: "o:<id>"}`; автор — сессия агента; своё — `GET /proposals?author=me`; счёт
/// `GET /proposals/counts` → `{rules: 1}`; человеку — карточка с суммой Rules и приростом.
#[tokio::test]
async fn candidate_rule_via_write() {
    let _g = SERIAL.lock().await;
    let a = server("via-write", |_| {}).await;
    rule_note(
        &a,
        "process/commits",
        "# Коммиты\n\nКоммить только явными путями.",
    )
    .await;
    let o = proposed(
        &a,
        "process/tests",
        "Перед коммитом запускай тесты затронутых крейтов.",
    )
    .await;
    let r = a
        .t(Method::GET, &format!("{PROPOSALS}?author=me"), None)
        .await;
    ok(&r, StatusCode::OK);
    let list = r.json()["proposals"].as_array().unwrap().clone();
    assert_eq!(list.len(), 1, "{}", r.text());
    assert_eq!(list[0]["id"], o.as_str());
    assert_eq!(list[0]["type"], "rule");
    assert_eq!(list[0]["status"], "proposed");
    assert_eq!(list[0]["path"], "process/tests");
    assert_eq!(list[0]["author_kind"], "agent");
    assert_eq!(list[0]["session"], S1);
    assert_eq!(counts(&a).await, json!({"rules": 1}));
    let id = o.trim_start_matches("o:");
    let r = a
        .adm(Method::GET, &format!("{PROPOSALS}/{id}"), None, None)
        .await;
    ok(&r, StatusCode::OK);
    let card = r.json();
    assert_eq!(card["reason"], REASON);
    let (now, after) = (
        card["rules_tokens"]["now"].as_u64().unwrap(),
        card["rules_tokens"]["after"].as_u64().unwrap(),
    );
    assert!(now > 0 && after > now, "{card}");
    // по `o:` тоже; модели нет — автор не модель
    let r = a.t(Method::GET, &format!("{PROPOSALS}/{o}"), None).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["author"], S1);
}

/// §6.7 п. 2 (решение 52): запись в Rules по токену без `rules.agent_write` — 403, в `detail` —
/// подсказка «предложите правило: memory_write … candidate».
#[tokio::test]
async fn rules_write_403_hints_candidate() {
    let _g = SERIAL.lock().await;
    let a = server("hint-403", |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "rules", "path": "x", "kind": "note", "content": "текст"})),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN);
    let d = r.json()["detail"].as_str().unwrap().to_string();
    assert!(
        d.contains("предложите правило") && d.contains("memory_write") && d.contains("candidate"),
        "{d}"
    );
}

/// §6.7 п. 2: открытых кандидатов — не больше 50 (51-й — 409 `/problems/proposals-full`); тот же
/// путь и текст — прежнее предложение (200, тот же `o:`); `reason` — 20..=2000 символов;
/// решение человека освобождает место.
#[tokio::test]
async fn candidate_limits_and_dedup() {
    let _g = SERIAL.lock().await;
    let a = server("limits", |_| {}).await;
    let first = proposed(&a, "r/0", "Правило номер 0 для всех проектов.").await;
    let r = propose(
        &a,
        &a.token,
        "r/0",
        "Правило номер 0 для всех проектов.",
        REASON,
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["proposal"], first.as_str());
    assert_eq!(r.json()["existing"], true);
    for (reason, why) in [("коротко", "короче 20"), (&"я".repeat(2001)[..], "длиннее")]
    {
        let r = propose(&a, &a.token, "r/x", "Правило x для всех проектов.", reason).await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            r.json()["detail"].as_str().unwrap().contains("reason"),
            "{why}: {}",
            r.text()
        );
    }
    for i in 1..50 {
        proposed(
            &a,
            &format!("r/{i}"),
            &format!("Правило номер {i} для всех проектов."),
        )
        .await;
    }
    assert_eq!(counts(&a).await, json!({"rules": 50}));
    let r = propose(&a, &a.token, "r/50", "Правило номер 50 для всех.", REASON).await;
    problem(&r, StatusCode::CONFLICT, "proposals-full");
    // повтор того же пути и текста и при полном списке — прежнее предложение
    let r = propose(
        &a,
        &a.token,
        "r/0",
        "Правило номер 0 для всех проектов.",
        REASON,
    )
    .await;
    ok(&r, StatusCode::OK);
    let id = first.trim_start_matches("o:");
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{id}/decline"),
            Some(json!({"note": "не нужно"})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "declined");
    assert_eq!(counts(&a).await, json!({"rules": 49}));
    proposed(&a, "r/50", "Правило номер 50 для всех.").await;
    // отклонённый путь и текст — всё ещё прежнее предложение (решение человека)
    let r = propose(
        &a,
        &a.token,
        "r/0",
        "Правило номер 0 для всех проектов.",
        REASON,
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "declined");
}

/// §6.7 п. 2: кандидат — только токеном `rw`: токен `ro` — 403, ничего не записано.
#[tokio::test]
async fn candidate_token_ro_403() {
    let _g = SERIAL.lock().await;
    let a = server("ro", |_| {}).await;
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = propose(&a, &ro, "r/ro", "Правило для всех проектов.", REASON).await;
    ok(&r, StatusCode::FORBIDDEN);
    assert!(
        r.json()["detail"].as_str().unwrap().contains("rw"),
        "{}",
        r.text()
    );
    assert_eq!(counts(&a).await, json!({"rules": 0}));
    // чтение `ro` — можно
    ok(
        &a.tok(&ro, Method::GET, PROPOSALS, None).await,
        StatusCode::OK,
    );
}

/// §6.7 п. 3: сборщик `rules` — аксиомы-решения и ловушки/приёмы с `useful ≥ 2` или
/// `opened ≥ 3`; в пакете — заголовки заметок Rules; итог: принятый элемент — кандидат (автор —
/// сессия), `skip` — строка `skipped`, отклонённый проверкой — с причиной; следующая ночь не
/// берёт источник с предложением и пропущенный в той же ревизии, а отклонённый проверкой и без
/// итога — берёт.
#[tokio::test]
async fn rules_work_sources_and_skip() {
    let _g = SERIAL.lock().await;
    let a = server("sources", |_| {}).await;
    set_mode(&a, "agent").await;
    rule_note(
        &a,
        "process/commits",
        "# Коммиты явными путями\n\nТолько явные пути.",
    )
    .await;
    let axiom = insight(
        &a,
        "knowledge",
        "decision",
        Some("axiom"),
        "Решение владельца",
    )
    .await;
    let gotcha = insight(&a, "knowledge", "gotcha", None, "Полезная ловушка").await;
    useful(&a, &gotcha, 2).await;
    let pattern = insight(&a, "knowledge", "pattern", None, "Открываемый приём").await;
    opened(&a, &pattern, 3).await;
    let quiet = insight(&a, "knowledge", "gotcha", None, "Тихая ловушка").await;
    useful(&a, &quiet, 1).await;
    opened(&a, &quiet, 2).await;
    insight(&a, "knowledge", "decision", Some("hypothesis"), "Гипотеза").await;
    insight(&a, "knowledge", "discovery", None, "Находка").await;
    night(&a, 0).await;
    let ws = rules_works(&a).await;
    assert_eq!(ws.len(), 1, "{ws:?}");
    assert_eq!(
        ws[0].space_id,
        space_id(&a, zam_store::SpaceKind::Rules).await
    );
    assert_eq!(
        last_targets(&a).await,
        [axiom.clone(), gotcha.clone(), pattern.clone()]
    );
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "work", "kind": "rules"})),
            &[("x-zam-session", S1)],
        )
        .await;
    ok(&r, StatusCode::OK);
    let text = r.json()["text"].as_str().unwrap().to_string();
    assert!(text.contains("· rules"), "{text}");
    assert!(text.contains("process/commits"), "заголовки Rules: {text}");
    assert!(
        text.contains("useful: 2") && text.contains("opened: 3"),
        "{text}"
    );
    assert!(text.contains("для всех проектов"), "{text}");
    let work = r.json()["work"].as_str().unwrap().to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("{WORKS}/{work}/result"),
            Some(json!({"items": [
                rule_item(1, "process/owner", "Следуй решениям владельца без оговорок."),
                {"n": 2, "skip": "факт одного проекта"},
                rule_item(3, "process/url", "Смотри https://example.com перед правкой."),
            ]})),
            &[("x-zam-session", S1)],
        )
        .await;
    ok(&r, StatusCode::OK);
    let v = r.json();
    assert_eq!(v["accepted"], json!([1]), "{v}");
    assert_eq!(v["rejected"][0]["n"], 3, "{v}");
    assert!(
        v["rejected"][0]["reason"].as_str().unwrap().contains("URL"),
        "{v}"
    );
    let open = rule_rows(&a, "proposed").await;
    assert_eq!(open.len(), 1);
    assert!(open[0].0.contains(&axiom), "{open:?}");
    assert_eq!(open[0].1["path"], "process/owner");
    assert!(open[0].1["content"].as_str().unwrap().contains("Следуй"));
    assert_eq!(open[0].2, S1);
    let skipped = rule_rows(&a, "skipped").await;
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0].0.contains(&gotcha), "{skipped:?}");
    // человеку `skipped` не показывается
    let r = a.adm(Method::GET, PROPOSALS, None, None).await;
    assert_eq!(
        r.json()["proposals"].as_array().unwrap().len(),
        1,
        "{}",
        r.text()
    );
    night(&a, 1).await;
    assert_eq!(rules_works(&a).await.len(), 2);
    assert_eq!(last_targets(&a).await, [pattern]);
}

/// §6.7 п. 3: проверка кандидата — `content` до 200 токенов (201 — отказ); без URL и блоков
/// кода (огороженный и отступом — отказ, вложенный список с четырьмя пробелами — принят); путь
/// Rules свободный (занят — 409 с советом) и допустимый; не почти-дубль заметки Rules (тот же
/// нормализованный заголовок — 409 со ссылкой).
#[tokio::test]
async fn rules_validator_cases() {
    let _g = SERIAL.lock().await;
    let a = server("validator", |_| {}).await;
    let note = rule_note(&a, "process/review", "# Ревью кода\n\nСмотри дифф целиком.").await;
    // ровно 200 и 201 токен счётчиком выдачи
    let mut text = String::from("Правило");
    while zam_store::count_tokens(&text) < 200 {
        text.push_str(" слово");
    }
    while zam_store::count_tokens(&text) > 200 {
        text.pop();
    }
    assert_eq!(zam_store::count_tokens(&text), 200, "{text}");
    let mut long = text.clone();
    while zam_store::count_tokens(&long) <= 200 {
        long.push_str(" ещё");
    }
    let r = propose(&a, &a.token, "len/201", &long, REASON).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        r.json()["detail"].as_str().unwrap().contains("200"),
        "{}",
        r.text()
    );
    proposed(&a, "len/200", &text).await;
    for (path, content, why) in [
        (
            "u/1",
            "Сверяйся с https://example.com/docs перед выпуском.",
            "URL",
        ),
        ("u/2", "Читай www.example.com перед выпуском.", "URL"),
        ("c/1", "Перед выпуском:\n\n```sh\ncargo test\n```\n", "блок"),
        (
            "c/2",
            "Перед выпуском запусти:\n\n    cargo test --all\n",
            "блок",
        ),
        ("p/1", "Путь", "путь"),
    ] {
        let path = if why == "путь" {
            "knowledge:/x"
        } else {
            path
        };
        let r = propose(&a, &a.token, path, content, REASON).await;
        let want = if why == "путь" {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        ok(&r, want);
        assert!(
            r.json()["detail"].as_str().unwrap().contains(why),
            "{why}: {}",
            r.text()
        );
    }
    proposed(
        &a,
        "list/nested",
        "Перед выпуском:\n- проверь журнал\n    - вложенный пункт с отступом\n- запусти `cargo test`",
    )
    .await;
    let r = propose(
        &a,
        &a.token,
        "process/review",
        "Новое правило о ревью.",
        REASON,
    )
    .await;
    ok(&r, StatusCode::CONFLICT);
    let v = r.json();
    assert_eq!(v["code"], "path_taken", "{v}");
    assert!(v["detail"].as_str().unwrap().contains("другой путь"), "{v}");
    let r = propose(
        &a,
        &a.token,
        "other/review",
        "# ревью  КОДА\n\nСмотри только изменённые строки.",
        REASON,
    )
    .await;
    ok(&r, StatusCode::CONFLICT);
    let v = r.json();
    assert_eq!(v["code"], "rule_duplicate", "{v}");
    assert_eq!(
        v["similar"][0]["ref"],
        format!("n:{}", note["id"].as_str().unwrap())
    );
}

/// §6.3 п. 4, §6.7 п. 3 (решение 59): текст с маской секрета — отказ: спонтанный кандидат —
/// 422, элемент работы — `rejected` с причиной, принятие с правкой — 422.
#[tokio::test]
async fn rule_candidate_with_mask_rejected() {
    let _g = SERIAL.lock().await;
    let a = server("mask", |_| {}).await;
    let r = propose(
        &a,
        &a.token,
        "m/1",
        "Ключ деплоя ‹secret› храни в Vault.",
        REASON,
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["detail"], MASK_LITERAL_REASON, "{}", r.text());
    set_mode(&a, "agent").await;
    insight(&a, "knowledge", "decision", Some("axiom"), "Аксиома").await;
    night(&a, 0).await;
    let v = do_work(
        &a,
        json!([rule_item(1, "m/2", "Ключ ‹secret-a1b2c3-1› не коммить.")]),
    )
    .await;
    assert_eq!(v["rejected"][0]["reason"], MASK_LITERAL_REASON, "{v}");
    let o = proposed(&a, "m/3", "Секреты держи в Vault, а не в тексте.").await;
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{}/accept", o.trim_start_matches("o:")),
            Some(json!({"content": "Секрет ‹secret› держи в Vault."})),
            None,
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["detail"], MASK_LITERAL_REASON, "{}", r.text());
}

/// §6.7 п. 3 (решение 64): отклонённый человеком источник больше не предлагается ни в какой
/// ревизии.
#[tokio::test]
async fn declined_source_never_again_any_rev() {
    let _g = SERIAL.lock().await;
    let a = server("declined", |_| {}).await;
    set_mode(&a, "agent").await;
    let axiom = insight(
        &a,
        "knowledge",
        "decision",
        Some("axiom"),
        "Аксиома владельца",
    )
    .await;
    night(&a, 0).await;
    do_work(
        &a,
        json!([rule_item(1, "d/1", "Держись решения владельца.")]),
    )
    .await;
    let r = a.adm(Method::GET, PROPOSALS, None, None).await;
    let o = r.json()["proposals"][0]["id"].as_str().unwrap().to_string();
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{o}/decline"),
            Some(json!({})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    // новая ревизия источника
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{}", entry_id(&axiom)),
            Some(json!({"body": "Аксиома владельца: новая редакция."})),
        )
        .await;
    ok(&r, StatusCode::OK);
    night(&a, 1).await;
    assert_eq!(
        rules_works(&a).await.len(),
        1,
        "отклонённый источник не предлагается"
    );
}

/// §6.7 п. 3 (решение 64): отказ исполнителя (`skipped`) — по ссылке и ревизии: та же ревизия не
/// предлагается, новая — снова.
#[tokio::test]
async fn skipped_source_back_on_new_rev() {
    let _g = SERIAL.lock().await;
    let a = server("skipped", |_| {}).await;
    set_mode(&a, "agent").await;
    let gotcha = insight(&a, "knowledge", "gotcha", None, "Ловушка").await;
    useful(&a, &gotcha, 2).await;
    night(&a, 0).await;
    do_work(&a, json!([{"n": 1, "skip": "факт одного проекта"}])).await;
    night(&a, 1).await;
    assert_eq!(
        rules_works(&a).await.len(),
        1,
        "та же ревизия не предлагается"
    );
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{}", entry_id(&gotcha)),
            Some(json!({"body": "Ловушка: общее правило для всех проектов."})),
        )
        .await;
    ok(&r, StatusCode::OK);
    night(&a, 2).await;
    assert_eq!(last_targets(&a).await, [gotcha]);
    let w = rules_works(&a).await.remove(0);
    assert_eq!(w.items[0].rev, 2);
}

/// §6.7 п. 4 (решение 60): «Принять» — заметка Rules тем же кодом, что заметка WebUI: автор —
/// человек, сообщение ревизии «из предложения o:<id> (сессия <id>)»; предложение `accepted` с
/// `result_ref: n:<id>`; аудит без текста; правило — в дайджесте следующей сессии.
#[tokio::test]
async fn accept_creates_rules_note_by_human() {
    let _g = SERIAL.lock().await;
    let a = server("accept", |_| {}).await;
    let content = "Пиши итог задачи в память перед ответом человеку.";
    let o = proposed(&a, "process/summary", content).await;
    let id = o.trim_start_matches("o:");
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{id}/accept"),
            Some(json!({})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let v = r.json();
    assert_eq!(v["proposal"]["status"], "accepted", "{v}");
    let node = v["note"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["proposal"]["result_ref"], format!("n:{node}"));
    assert_eq!(v["note"]["path"], "process/summary");
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/nodes/{node}/revisions"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let rev = r.json()[0].clone();
    assert_eq!(rev["author_kind"], "human", "{rev}");
    let msg = rev["message"].as_str().unwrap();
    assert!(
        msg.contains(&format!("из предложения {o}")) && msg.contains(S1),
        "{msg}"
    );
    assert_eq!(counts(&a).await, json!({"rules": 0}));
    let r = a.adm(Method::GET, "/api/v1/admin/audit", None, None).await;
    let row = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["action"] == "proposal.accept")
        .cloned()
        .expect("аудит proposal.accept");
    assert_eq!(row["details"]["id"], o.as_str());
    assert_eq!(row["details"]["result"], format!("n:{node}"));
    assert!(!row.to_string().contains("Пиши итог"), "{row}");
    // второе принятие — 409
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{id}/accept"),
            Some(json!({})),
            None,
        )
        .await;
    ok(&r, StatusCode::CONFLICT);
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "session_start"})),
            &[("x-zam-session", "rules-next-session")],
        )
        .await;
    ok(&r, StatusCode::OK);
    let digest = r.json()["text"].as_str().unwrap().to_string();
    assert!(
        digest.contains("process/summary") && digest.contains("Пиши итог задачи"),
        "{digest}"
    );
}

/// §6.7 п. 4: путь и текст правятся до принятия: заметка — по новому пути с новым текстом
/// (человек вправе дописать адрес — решение 58), в предложении — исходный текст и итог.
#[tokio::test]
async fn accept_edit_path_and_content() {
    let _g = SERIAL.lock().await;
    let a = server("accept-edit", |_| {}).await;
    let o = proposed(&a, "draft/rule", "Черновик правила о проверках.").await;
    let edited = "Перед выпуском сверяйся со списком https://example.com/checklist.";
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{}/accept", o.trim_start_matches("o:")),
            Some(json!({"path": "release/checklist", "content": edited})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let v = r.json();
    assert_eq!(v["note"]["path"], "release/checklist", "{v}");
    let node = v["note"]["id"].as_str().unwrap();
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/nodes/{node}/revisions/1"),
            None,
            None,
        )
        .await;
    assert_eq!(r.json()["content"], edited);
    assert_eq!(v["proposal"]["path"], "draft/rule");
    assert_eq!(v["proposal"]["status"], "accepted");
}

/// §6.7 п. 4: «Принять» и «Отклонить» — только сессия WebUI: токену — 403, предложение открыто.
#[tokio::test]
async fn accept_token_403() {
    let _g = SERIAL.lock().await;
    let a = server("accept-token", |_| {}).await;
    let o = proposed(&a, "t/rule", "Правило для проверки прав.").await;
    let id = o.trim_start_matches("o:");
    for act in ["accept", "decline"] {
        let r = a
            .t(
                Method::POST,
                &format!("{PROPOSALS}/{id}/{act}"),
                Some(json!({})),
            )
            .await;
        ok(&r, StatusCode::FORBIDDEN);
        assert!(
            r.json()["detail"].as_str().unwrap().contains("человек"),
            "{}",
            r.text()
        );
    }
    assert_eq!(counts(&a).await, json!({"rules": 1}));
}

/// §6.7 п. 4: путь занят к моменту принятия — 409 с подсказкой, предложение открыто.
#[tokio::test]
async fn accept_path_taken_409() {
    let _g = SERIAL.lock().await;
    let a = server("accept-taken", |_| {}).await;
    let o = proposed(&a, "taken/rule", "Правило, путь которого займут.").await;
    rule_note(&a, "taken/rule", "Человек успел первым.").await;
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{}/accept", o.trim_start_matches("o:")),
            Some(json!({})),
            None,
        )
        .await;
    ok(&r, StatusCode::CONFLICT);
    let v = r.json();
    assert_eq!(v["code"], "path_taken", "{v}");
    assert!(v["detail"].as_str().unwrap().contains("path"), "{v}");
    assert_eq!(counts(&a).await, json!({"rules": 1}));
}

/// §6.7 п. 4: автор отзывает своё — токен того же пользователя; человек — 403 (он отклоняет);
/// второй отзыв — 409; предложение модели токен не отзывает.
#[tokio::test]
async fn withdraw_by_author() {
    let _g = SERIAL.lock().await;
    let a = server("withdraw", |_| {}).await;
    let o = proposed(&a, "w/rule", "Правило, которое отзовут.").await;
    let id = o.trim_start_matches("o:");
    let url = format!("{PROPOSALS}/{id}/withdraw");
    ok(
        &a.adm(Method::POST, &url, None, None).await,
        StatusCode::FORBIDDEN,
    );
    let r = a.t(Method::POST, &url, None).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "withdrawn");
    ok(&a.t(Method::POST, &url, None).await, StatusCode::CONFLICT);
    // предложение модели
    let rules = space_id(&a, zam_store::SpaceKind::Rules).await;
    let out = zam_store::proposals::insert(
        &db(&a).await,
        zam_store::proposals::NewProposal {
            space: rules,
            ptype: zam_store::proposals::ProposalType::Rule,
            status: zam_store::proposals::ProposalStatus::Proposed,
            targets: Vec::new(),
            payload: json!({"path": "model/rule", "content": "Правило модели."}),
            reason: Some(REASON.into()),
            basis: Vec::new(),
            dedup_key: None,
            author_kind: zam_store::proposals::ProposalAuthor::Model,
            author: Some("model:mock-model".into()),
            work_id: None,
            session_id: None,
        },
    )
    .await
    .unwrap();
    let zam_store::proposals::InsertOutcome::Inserted(mid) = out else {
        panic!("{out:?}")
    };
    ok(
        &a.t(Method::POST, &format!("{PROPOSALS}/{mid}/withdraw"), None)
            .await,
        StatusCode::FORBIDDEN,
    );
}

/// §6 «Общее» (маскировка): ответы `/proposals` токену — с маской `‹secret›`, человеку — исходник.
#[tokio::test]
async fn proposals_masked_for_token() {
    let _g = SERIAL.lock().await;
    let a = server("masked", |_| {}).await;
    let s = secret();
    let o = proposed(&a, "s/rule", &format!("Не публикуй токен {s} в журналах.")).await;
    for uri in [PROPOSALS.to_string(), format!("{PROPOSALS}/{o}")] {
        let r = a.t(Method::GET, &uri, None).await;
        ok(&r, StatusCode::OK);
        assert!(!r.text().contains(&s), "{}", r.text());
        assert!(r.text().contains("‹secret"), "{}", r.text());
        let r = a.adm(Method::GET, &uri, None, None).await;
        assert!(r.text().contains(&s), "{}", r.text());
    }
}

/// §6 «Общее» (решение 62): записи, к которым приложил руку участник общего решения, сборщик
/// `rules` не берёт; аксиома владельца в том же решении — берёт.
#[tokio::test]
async fn proposals_skip_member_records() {
    let _g = SERIAL.lock().await;
    let a = server("member", |_| {}).await;
    set_mode(&a, "agent").await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G1, "name": "Общее решение"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let owner = insight(&a, G1, "decision", Some("axiom"), "Аксиома владельца").await;
    let (member, tok) = a.new_user("Участник").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G1, "name": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    a.set_access(&a.token, G1, &member, "rw").await;
    let h = [("x-zam-solution", G1)];
    insight_by(
        &a,
        &tok,
        G1,
        "decision",
        Some("axiom"),
        "Аксиома участника",
        &h,
    )
    .await;
    // запись владельца, которую правил участник
    let touched = insight(&a, G1, "decision", Some("axiom"), "Тронутая участником").await;
    let r = a
        .tok_rev(
            &tok,
            Method::PATCH,
            &format!("/api/v1/entries/{}", entry_id(&touched)),
            Some(json!({"body": "правка участника"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    night(&a, 0).await;
    assert_eq!(last_targets(&a).await, [owner]);
}

/// Схема 0027 без CHECK типа (§6 «Общее»): строка `proposals.type = 'tidy'` (и тестового `echo`)
/// в выборке и счёте не участвует, по id — 404 (WARN).
#[tokio::test]
async fn unknown_proposal_type_skipped_on_read() {
    let _g = SERIAL.lock().await;
    let a = server("unknown-type", |_| {}).await;
    let rules = space_id(&a, zam_store::SpaceKind::Rules).await;
    let tidy = Id::new();
    db(&a)
        .await
        .write(move |t| {
            for (id, ty) in [(tidy, "tidy"), (Id::new(), "echo")] {
                t.execute(
                    "INSERT INTO proposals (id, space_id, type, status, payload, author_kind,
                                            created_at)
                     VALUES (?1, ?2, ?3, 'proposed', '{\"path\": \"x\"}', 'agent', ?4)",
                    rusqlite::params![
                        id,
                        rules,
                        ty,
                        zam_store::time::fmt_ts(zam_store::time::now())
                    ],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    proposed(&a, "k/rule", "Известное правило для всех.").await;
    let r = a.adm(Method::GET, PROPOSALS, None, None).await;
    ok(&r, StatusCode::OK);
    let list = r.json()["proposals"].as_array().unwrap().clone();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["type"], "rule");
    assert_eq!(counts(&a).await, json!({"rules": 1}));
    ok(
        &a.adm(Method::GET, &format!("{PROPOSALS}/{tidy}"), None, None)
            .await,
        StatusCode::NOT_FOUND,
    );
    ok(
        &a.adm(Method::GET, &format!("{PROPOSALS}?type=tidy"), None, None)
            .await,
        StatusCode::BAD_REQUEST,
    );
}

/// §6.2 п. 6, решение 72 (режим модели): источник, выброшенный вторым сканом
/// (`secret_in_payload`), до новой ревизии не предлагается; принятый элемент модели — кандидат с
/// автором `model:<имя>`.
#[tokio::test]
async fn rules_secret_dropped_not_reoffered() {
    let _g = SERIAL.lock().await;
    let m = MockModel::start();
    m.push(MockReply::chat(
        &json!({"items": [rule_item(1, "model/rule", "Проверяй журнал перед выпуском.")]})
            .to_string(),
    ));
    m.set_default(MockReply::chat("{\"items\": []}"));
    let a = server("secret-drop", night_now).await;
    let clean = insight(&a, "knowledge", "decision", Some("axiom"), "Чистая аксиома").await;
    let leaky = insight(
        &a,
        "knowledge",
        "decision",
        Some("axiom"),
        "Аксиома с ключом",
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/vault",
            Some(
                json!({"title": "llm", "data": {"type": "token", "url": m.base_url()},
                        "secret": model_key()}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/auth/settings",
            Some(
                json!({"works": {"mode": "model", "model": {"protocol": "openai",
                "vault": "llm", "model": "mock-model", "max_tokens": 4000}}}),
            ),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let rules = space_id(&a, zam_store::SpaceKind::Rules).await;
    let item = |n: usize, target: &str, text: String| WorkItem {
        n,
        target: target.to_string(),
        rev: 1,
        kind: "insight".into(),
        title: "т".into(),
        text,
        signals: Vec::new(),
    };
    let s = secret();
    store::enqueue(
        &db(&a).await,
        NewWork {
            kind: WorkKind::Rules,
            space: rules,
            mode: WorkMode::Model,
            origin: Origin::Nightly,
            night: Some(zam_store::time::utc_today()),
            items: vec![
                item(1, &clean, "решение".into()),
                item(2, &leaky, format!("токен деплоя {s} в CI")),
            ],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let w = &a.st.cfg.works;
    let (d, _, until) = zam_server::schedule::night_window(
        zam_store::time::now(),
        w.at_hm(),
        &w.time_zone(),
        w.window_min,
    )
    .expect("сейчас — в окне ночи");
    queue::night(&NightCtx::new(a.st.clone()), d, until)
        .await
        .unwrap();
    let body = m.requests()[0].body.clone();
    assert!(!body.contains(&s), "{body}");
    let ws = rules_works(&a).await;
    let first = ws.last().unwrap();
    assert_eq!(first.state, WorkState::Done, "{first:?}");
    let excluded = store::excluded_refs(&db(&a).await, WorkKind::Rules)
        .await
        .unwrap();
    assert!(excluded.contains(&(leaky.clone(), 1)), "{excluded:?}");
    let open = rule_rows(&a, "proposed").await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].2, "model:mock-model");
    // сборщик: чистый источник уже с предложением, источник с ключом — до новой ревизии нет
    let cands = zam_server::works::kinds::collect(
        WorkKind::Rules,
        &NightCtx::new(a.st.clone()),
        a.st.admin_id,
        rules,
        10,
    )
    .await
    .unwrap();
    assert!(cands.is_empty(), "{cands:?}");
    // принятие кандидата модели — сообщение ревизии с именем модели
    let r = a.adm(Method::GET, PROPOSALS, None, None).await;
    let o = r.json()["proposals"][0]["id"].as_str().unwrap().to_string();
    let r = a
        .adm(
            Method::POST,
            &format!("{PROPOSALS}/{o}/accept"),
            Some(json!({})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let node = r.json()["note"]["id"].as_str().unwrap().to_string();
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/nodes/{node}/revisions"),
            None,
            None,
        )
        .await;
    let msg = r.json()[0]["message"].as_str().unwrap().to_string();
    assert!(
        msg.contains(&format!("из предложения {o} (model:mock-model)")),
        "{msg}"
    );
}

/// Протокол `zam guide` (§6 «Общее»): раздел «Кандидаты в Rules» в ru и en — `candidate` у
/// `memory_write`, решает человек, `GET /proposals?author=me`, `zam rules candidate`.
#[tokio::test]
async fn guide_rules_candidates_section() {
    let _g = SERIAL.lock().await;
    let a = server("guide", |_| {}).await;
    for (q, head, human) in [
        ("", "## Кандидаты в Rules\n", "человек"),
        ("?lang=en", "## Rules candidates\n", "human"),
    ] {
        let r = a
            .t(Method::GET, &format!("/api/v1/agent-guide{q}"), None)
            .await;
        ok(&r, StatusCode::OK);
        let t = r.text();
        let sec = t
            .split(head)
            .nth(1)
            .unwrap_or_else(|| panic!("{head}: {t}"));
        let sec = sec.split("\n## ").next().unwrap();
        for want in [
            "candidate",
            "memory_write",
            "rules:/",
            "author=me",
            "zam rules candidate",
            "withdraw",
            "200",
            human,
        ] {
            assert!(sec.contains(want), "{q} {want}: {sec}");
        }
    }
}
