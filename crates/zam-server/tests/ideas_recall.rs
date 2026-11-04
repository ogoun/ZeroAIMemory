//! Проактивность идей (M5.7 §6, задача 3): раздел `### Идеи` дайджеста `session_start` (только
//! первый старт сессии, только пространство решения, до `ideas.digest_max`), подсказка идеи к
//! `prompt` отдельным лёгким поиском (основной поиск не меняется), выключатели `ideas.proactive`
//! сервера и пользователя, общие идеи Knowledge — только при связи с задачей `in_progress`
//! сессии (ответ владельца на вопрос 8 — А), совместный доступ. Сервер без моделей (поиск — FTS), тесты с
//! векторным каналом — под `ZAM_REQUIRE_ML`.
mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e5a";
const RECALL: &str = "/api/v1/recall";
const SECTION: &str = "### Идеи";
const HINT: &str = "Предложи человеку одну: memory_update_entry action=propose — вопрос «начать / \
                    отложить / отказаться»; без ответа «начать» не начинай.";
/// Добавка к строке-указанию, если в разделе есть идея со снятой задачей (ревью задачи 7, M3).
const HINT_REOPEN: &str =
    " Со снятой задачей — предложи вернуть: propose с text, почему пора; решает человек.";
const INDEXED: Duration = Duration::from_secs(240);

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

const KN: &str = "/api/v1/spaces/knowledge/entries";

/// Сервер с решением `G` администратора; `recall.timeout_ms` поднят (отладочная сборка на HDD).
async fn server(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let a = api(&format!("ideas-recall-{name}"), |c| {
        c.recall.timeout_ms = 30_000;
        tweak(c);
    })
    .await;
    register(&a, &a.token).await;
    a
}

async fn register(a: &Api, tok: &str) -> StatusCode {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Проект"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    r.status
}

/// Идея токеном `tok` в `path` (лента пространства) с `priority` и `origin`; id.
async fn idea_as(a: &Api, tok: &str, path: &str, title: &str, extra: Value) -> String {
    let mut data = json!({"origin": "human"});
    for (k, v) in extra.as_object().unwrap() {
        data[k] = v.clone();
    }
    let r = a
        .tok(
            tok,
            Method::POST,
            path,
            Some(json!({"kind": "idea", "title": title, "body": "Замысел.", "data": data})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn idea(a: &Api, path: &str, title: &str, extra: Value) -> String {
    idea_as(a, &a.token, path, title, extra).await
}

async fn recall_as(a: &Api, tok: &str, session: Option<&str>, body: Value) -> Resp {
    let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
    a.tok_h(tok, Method::POST, RECALL, Some(body), &h).await
}

async fn ok_as(a: &Api, tok: &str, session: Option<&str>, body: Value) -> Value {
    let r = recall_as(a, tok, session, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn ok(a: &Api, session: Option<&str>, body: Value) -> Value {
    ok_as(a, &a.token, session, body).await
}

/// `session_start` решения `G` в сессии `session`.
async fn start(a: &Api, session: &str) -> Value {
    ok(
        a,
        Some(session),
        json!({"event": "session_start", "solution": G}),
    )
    .await
}

async fn prompt(a: &Api, session: &str, text: &str) -> Value {
    ok(
        a,
        Some(session),
        json!({"event": "prompt", "solution": G, "prompt": text}),
    )
    .await
}

/// Ссылки `w:<id>` подсказок-идей ответа (`items[].reason == "idea"`) по порядку.
fn idea_items(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] == "idea")
        .map(|i| {
            assert_eq!(i["kind"], "idea", "{i}");
            i["id"].as_str().unwrap().to_string()
        })
        .collect()
}

fn text(j: &Value) -> &str {
    j["text"].as_str().unwrap()
}

/// Строки раздела `### Идеи` (без заголовка); нет раздела — `None`.
fn section_lines(j: &Value) -> Option<Vec<String>> {
    let t = text(j);
    let rest = t.split(&format!("{SECTION}\n")).nth(1)?;
    let body = rest.split("\n\n").next().unwrap();
    Some(body.lines().map(str::to_string).collect())
}

async fn post(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.t(
        Method::POST,
        &format!("/api/v1/entries/{id}/{op}"),
        Some(body),
    )
    .await
}

async fn set_user_proactive(a: &Api, on: bool) {
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"ideas": {"proactive": on}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Прямой SQL по базе администратора (наступивший `horizon`, статус `done`).
async fn sql(a: &Api, q: &'static str, id: &str) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let id = id.to_string();
    let n = db.write(move |tx| Ok(tx.execute(q, [id])?)).await.unwrap();
    assert_eq!(n, 1, "{q}");
}

fn yesterday() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_sub(jiff::Span::new().days(1))
        .unwrap()
        .to_string()
}

fn tomorrow() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(1))
        .unwrap()
        .to_string()
}

// ---- дайджест ----

/// 4 идеи решения и одна в Knowledge → 3 строки: `high`, `normal` новее, `normal`; без
/// Knowledge и `low`; заголовок и строка-указание на месте; `items[]` — `kind/reason idea`.
#[tokio::test]
async fn digest_ideas_section_top3_order() {
    let a = server("top3", |_| {}).await;
    let low = idea(&a, &sol(), "Низкая идея", json!({"priority": "low"})).await;
    let n_old = idea(&a, &sol(), "Обычная старая", json!({})).await;
    let n_new = idea(&a, &sol(), "Обычная новая", json!({"priority": "normal"})).await;
    let high = idea(&a, &sol(), "Важная идея", json!({"priority": "high"})).await;
    let kn = idea(&a, KN, "Общая идея", json!({"priority": "high"})).await;
    let j = start(&a, "s1").await;
    let lines = section_lines(&j).unwrap_or_else(|| panic!("нет раздела: {j}"));
    assert_eq!(
        lines,
        [
            HINT.to_string(),
            format!("ZeroAIMemory: идея «Важная идея» [high] — w:{high}"),
            format!("ZeroAIMemory: идея «Обычная новая» [normal] — w:{n_new}"),
            format!("ZeroAIMemory: идея «Обычная старая» [normal] — w:{n_old}"),
        ],
        "{j}"
    );
    assert_eq!(
        idea_items(&j),
        [
            format!("w:{high}"),
            format!("w:{n_new}"),
            format!("w:{n_old}")
        ]
    );
    assert!(!text(&j).contains(&low) && !text(&j).contains(&kn), "{j}");
}

/// Review Focus 3: раздел — только на первом `session_start` сессии. Повтор после `compact`
/// (пакет восстановления) и обычный повторный старт — без раздела и без четвёртой идеи;
/// новая сессия — снова первые три; `prompt` той же сессии показанную идею не повторяет.
#[tokio::test]
async fn digest_ideas_only_on_first_session_start() {
    let a = server("first-only", |_| {}).await;
    let mut ids = Vec::new();
    for (i, p) in ["high", "high", "normal", "low"].iter().enumerate() {
        ids.push(
            idea(
                &a,
                &sol(),
                &format!("Идея номер {i}"),
                json!({"priority": p}),
            )
            .await,
        );
    }
    let first = start(&a, "s1").await;
    let shown = idea_items(&first);
    assert_eq!(shown.len(), 3, "{first}");
    for body in [
        json!({"event": "session_start", "solution": G, "source": "compact"}),
        json!({"event": "session_start", "solution": G, "source": "resume"}),
        json!({"event": "session_start", "solution": G}),
        json!({"event": "recovery", "solution": G}),
    ] {
        let j = ok(&a, Some("s1"), body.clone()).await;
        assert!(section_lines(&j).is_none(), "{body}: {j}");
        assert!(idea_items(&j).is_empty(), "{body}: {j}");
        assert!(!text(&j).contains(&ids[3]), "{body}: {j}");
    }
    let j = start(&a, "s2").await;
    assert_eq!(idea_items(&j), shown, "{j}");
}

/// Раздел идей — сразу после строки открытых вопросов и раньше правил.
#[tokio::test]
async fn digest_ideas_after_open_questions_line() {
    let a = server("after-q", |_| {}).await;
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "question", "title": "Какой формат?"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    a.note("rules", "стиль", "# Стиль\n\nПиши коротко.\n").await;
    idea(&a, &sol(), "Тёмная тема", json!({})).await;
    let j = start(&a, "s1").await;
    let t = text(&j);
    let q = t.find("### Открытые вопросы\n").expect("вопросы");
    let i = t.find(&format!("{SECTION}\n{HINT}\n")).expect("идеи");
    let rules = t.find("### Правила").expect("правила");
    assert!(q < i && i < rules, "{t}");
    // между строкой вопросов и разделом идей ничего нет
    let between = &t[q..i];
    assert_eq!(between.lines().count(), 3, "{between}");
}

/// Review Focus 5: никогда не предлагаются идеи «от агента», `proposed`, `accepted`,
/// `declined`, `done` и `deferred` до `horizon`; `deferred` с наступившим `horizon` — да
/// (дайджест и подсказка).
#[tokio::test]
async fn never_proposed_kinds_matrix() {
    let a = server("matrix", |c| c.ideas.prompt_max = 3).await;
    let agent = idea(&a, &sol(), "Звездолёт агента", json!({"origin": "agent"})).await;
    let proposed = idea(&a, &sol(), "Звездолёт предложенный", json!({})).await;
    let r = post(&a, &proposed, "propose", json!({"text": "пора"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let accepted = idea(&a, &sol(), "Звездолёт принятый", json!({})).await;
    let r = post(
        &a,
        &accepted,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let declined = idea(&a, &sol(), "Звездолёт отклонённый", json!({})).await;
    let r = post(
        &a,
        &declined,
        "decide",
        json!({"decision": "decline", "by": "human"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let done = idea(&a, &sol(), "Звездолёт сделанный", json!({})).await;
    sql(
        &a,
        "UPDATE entries SET status = 'done' WHERE id = ?1",
        &done,
    )
    .await;
    let later = idea(&a, &sol(), "Звездолёт отложенный", json!({})).await;
    let r = post(
        &a,
        &later,
        "decide",
        json!({"decision": "defer", "by": "human", "horizon": tomorrow()}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let due = idea(&a, &sol(), "Звездолёт созревший", json!({})).await;
    let r = post(
        &a,
        &due,
        "decide",
        json!({"decision": "defer", "by": "human", "horizon": tomorrow()}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = "UPDATE entries SET data = json_set(data, '$.horizon', ?2) WHERE id = ?1";
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let (id, y) = (due.clone(), yesterday());
    db.write(move |tx| Ok(tx.execute(q, [id, y])?))
        .await
        .unwrap();
    let never = [&agent, &proposed, &accepted, &declined, &done, &later];
    let j = start(&a, "s1").await;
    assert_eq!(idea_items(&j), [format!("w:{due}")], "{j}");
    for id in never {
        assert!(!text(&j).contains(id.as_str()), "{id}: {j}");
    }
    // подсказка: текст совпадает с заголовками всех идей
    for (i, p) in ["звездолёт", "Звездолёт агента", "звездолёт отклонённый"]
        .iter()
        .enumerate()
    {
        let j = prompt(&a, &format!("p{i}"), p).await;
        for id in never {
            assert!(!text(&j).contains(id.as_str()), "{p}: {id}: {j}");
        }
        assert!(
            idea_items(&j).iter().all(|w| *w == format!("w:{due}")),
            "{p}: {j}"
        );
    }
    // положительная сторона: `deferred` с наступившим `horizon` к подсказке приходит
    let j = prompt(&a, "p-due", "звездолёт созревший").await;
    assert_eq!(idea_items(&j), [format!("w:{due}")], "{j}");
}

/// Ревью задачи 3, I1: сбой чтения идей (повреждённый JSON `data` — `json_extract` падает)
/// не роняет `session_start` и не снимает остальной дайджест — раздела идей просто нет.
#[tokio::test]
async fn digest_ideas_store_error_degrades() {
    let a = server("store-err", |_| {}).await;
    let r = a
        .t(
            Method::POST,
            &sol(),
            Some(json!({"kind": "insight", "title": "Пул течёт при панике",
                        "body": "Соединение не возвращается.", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let gotcha = r.json()["id"].as_str().unwrap().to_string();
    idea(&a, &sol(), "Целая идея", json!({})).await;
    let bad = idea(&a, &sol(), "Битая идея", json!({})).await;
    sql(&a, "UPDATE entries SET data = '{' WHERE id = ?1", &bad).await;
    let j = start(&a, "s1").await;
    assert!(section_lines(&j).is_none(), "{j}");
    assert!(text(&j).contains(&format!("i:{gotcha}")), "{j}");
}

/// Ревью задачи 3, M4 (буква §6.1): «первый `session_start`» — в сессии нет ни одной отметки
/// показа любой идеи. Показанные идеи отложены (из кандидатов ушли) — повторный старт той же
/// сессии раздела всё равно не даёт, хотя четвёртая идея — кандидат.
#[tokio::test]
async fn digest_no_section_after_shown_ideas_deferred() {
    let a = server("shown-deferred", |_| {}).await;
    for i in 0..4 {
        idea(&a, &sol(), &format!("Идея {i}"), json!({})).await;
    }
    let j = start(&a, "s1").await;
    let shown = idea_items(&j);
    assert_eq!(shown.len(), 3, "{j}");
    for w in &shown {
        let id = w.trim_start_matches("w:");
        let r = post(
            &a,
            id,
            "decide",
            json!({"decision": "defer", "by": "human", "horizon": tomorrow()}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let j = start(&a, "s1").await;
    assert!(section_lines(&j).is_none(), "{j}");
    assert!(idea_items(&j).is_empty(), "{j}");
    // новая сессия — четвёртая
    let j = start(&a, "s2").await;
    assert_eq!(idea_items(&j).len(), 1, "{j}");
}

/// Review Focus 5: выключенный `ideas.proactive` (пользователя и сервера) — дайджест побайтно
/// равен дайджесту той же базы без идей, показы не учитываются.
#[tokio::test]
async fn proactive_off_digest_byte_equal() {
    // пользователь
    let a = server("off-user", |_| {}).await;
    let base = start(&a, "s0").await;
    for i in 0..3 {
        idea(&a, &sol(), &format!("Идея {i}"), json!({})).await;
    }
    set_user_proactive(&a, false).await;
    let off = start(&a, "s1").await;
    assert_eq!(text(&off), text(&base), "{off}");
    assert!(idea_items(&off).is_empty(), "{off}");
    let j = prompt(&a, "s1", "идея").await;
    assert!(idea_items(&j).is_empty(), "{j}");
    // показов не было: после включения та же сессия получает раздел
    set_user_proactive(&a, true).await;
    let on = start(&a, "s1").await;
    assert_eq!(idea_items(&on).len(), 3, "{on}");
    // сервер
    let b = server("off-server", |c| c.ideas.proactive = false).await;
    let base = start(&b, "s0").await;
    for i in 0..3 {
        idea(&b, &sol(), &format!("Идея {i}"), json!({})).await;
    }
    let off = start(&b, "s1").await;
    assert_eq!(text(&off), text(&base), "{off}");
    let j = prompt(&b, "s2", "идея").await;
    assert!(idea_items(&j).is_empty(), "{j}");
}

#[tokio::test]
async fn digest_max_zero_no_section() {
    let a = server("max0", |c| c.ideas.digest_max = 0).await;
    idea(&a, &sol(), "Идея", json!({})).await;
    let j = start(&a, "s1").await;
    assert!(section_lines(&j).is_none(), "{j}");
    assert!(idea_items(&j).is_empty(), "{j}");
}

/// Пакеты восстановления и под задачу идей не содержат (§6.1).
#[tokio::test]
async fn recovery_and_brief_have_no_ideas() {
    let a = server("rec-brief", |_| {}).await;
    let id = idea(&a, &sol(), "Кэш миниатюр", json!({})).await;
    for body in [
        json!({"event": "recovery", "solution": G}),
        json!({"event": "session_start", "solution": G, "source": "compact"}),
        json!({"event": "brief", "solution": G}),
        json!({"event": "brief", "solution": G, "task": "Кэш миниатюр"}),
    ] {
        let j = ok(&a, Some("r1"), body.clone()).await;
        assert!(!text(&j).contains(&id), "{body}: {j}");
        assert!(!text(&j).contains(SECTION), "{body}: {j}");
    }
    // показов не было: первый обычный старт этой сессии раздел показывает
    let j = start(&a, "r1").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
}

// ---- подсказка к prompt ----

/// Близкий к идее решения запрос — одна строка идеи; повтор в той же сессии — без неё.
#[tokio::test]
async fn prompt_idea_hint_once() {
    let a = server("prompt-once", |_| {}).await;
    let id = idea(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let j = prompt(&a, "s1", "как сделать экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
    let line = format!(
        "ZeroAIMemory: к задаче подходит идея «Экспорт отчётов в PDF» — w:{id} (предложи: \
         action=propose; без ответа не начинай)"
    );
    assert!(text(&j).lines().any(|l| l == line), "{j}");
    let j = prompt(&a, "s1", "как сделать экспорт отчётов в PDF").await;
    assert!(idea_items(&j).is_empty(), "{j}");
    // другая сессия — снова
    let j = prompt(&a, "s2", "экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
    // `prompt_max = 0` — тишина по идеям
    let b = server("prompt-max0", |c| c.ideas.prompt_max = 0).await;
    idea(&b, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let j = prompt(&b, "s1", "экспорт отчётов в PDF").await;
    assert!(idea_items(&j).is_empty(), "{j}");
}

/// Review Focus 3: идея, показанная дайджестом, в `prompt` той же сессии не приходит.
#[tokio::test]
async fn prompt_skips_idea_seen_in_digest() {
    let a = server("prompt-seen", |_| {}).await;
    let id = idea(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let j = start(&a, "s1").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
    let j = prompt(&a, "s1", "экспорт отчётов в PDF").await;
    assert!(idea_items(&j).is_empty(), "{j}");
    let j = prompt(&a, "s2", "экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
}

/// Основной поиск подсказок `prompt` (то же тело, что у `prompt_hints` в `recall.rs`):
/// `(id, балл)` по порядку.
async fn main_search(a: &Api, q: &str) -> Vec<(String, f64)> {
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({
                "query": q,
                "scope": {"solution": G, "include_knowledge": true, "include_rules": true},
                "filters": {"kinds": ["insight", "note"]},
                "mode": "brief",
                "top_k": 20,
                "budget_tokens": 4000,
                "rerank": "off",
                "skip_seen": false,
                "no_cache": true,
                "format": "json",
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let id = i["entry_id"].as_str().or(i["node_id"].as_str()).unwrap();
            (id.to_string(), i["score"].as_f64().unwrap())
        })
        .collect()
}

/// Review Focus 5: набор, порядок, строки insight-подсказок и баллы основного поиска `prompt`
/// с идеями и без них одинаковы — идеи ищутся отдельным запросом.
#[tokio::test]
async fn prompt_insight_hints_unchanged_with_ideas() {
    let a = server("prompt-same", |_| {}).await;
    for i in 0..4 {
        let r = a
            .t(
                Method::POST,
                KN,
                Some(
                    json!({"kind": "insight", "title": format!("Экспорт отчётов, случай {i}"),
                            "body": format!("Экспорт отчётов в PDF падает на шрифтах, {i}."),
                            "data": {"kind": "gotcha"}, "force": true}),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let q = "экспорт отчётов в PDF";
    let base = prompt(&a, "s0", q).await;
    let base_scores = main_search(&a, q).await;
    assert!(!base_scores.is_empty());
    for i in 0..5 {
        idea(
            &a,
            &sol(),
            &format!("Экспорт отчётов в PDF, вариант {i}"),
            json!({}),
        )
        .await;
    }
    let with = prompt(&a, "s1", q).await;
    assert_eq!(idea_items(&with).len(), 1, "{with}");
    let strip = |j: &Value| -> (Vec<Value>, String) {
        let items = j["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["reason"] != "idea")
            .cloned()
            .collect();
        let t = text(j)
            .lines()
            .filter(|l| !l.contains("к задаче подходит идея"))
            .collect::<Vec<_>>()
            .join("\n");
        (items, t)
    };
    assert!(!base["items"].as_array().unwrap().is_empty(), "{base}");
    assert_eq!(strip(&with), strip(&base), "{with}\n{base}");
    // баллы основного поиска — побайтно те же (идей в нём нет)
    assert_eq!(main_search(&a, q).await, base_scores);
}

/// Отдельный поиск идей не уложился в срок — идей нет, основные подсказки есть, без
/// предупреждения о тайм-ауте (тестовая метка в заголовке идеи, фича `test-hooks`). Срок 8 с:
/// под параллельной нагрузкой набора (модели ML соседних тестов) основной поиск отладочной
/// сборки на HDD сам доходил до 3–4 с — `recall timeout` ставил он, а не идеи.
#[tokio::test]
async fn prompt_ideas_timeout_silent() {
    let a = api("ideas-recall-timeout", |c| c.recall.timeout_ms = 8000).await;
    register(&a, &a.token).await;
    let r = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "insight", "title": "Экспорт отчётов в PDF",
                        "body": "Экспорт отчётов в PDF падает на шрифтах.",
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let mark = zam_server::api::recall::TEST_SLOW_IDEA_MARK;
    idea(
        &a,
        &sol(),
        &format!("Экспорт отчётов в PDF {mark}"),
        json!({}),
    )
    .await;
    let j = prompt(&a, "s1", "экспорт отчётов в PDF").await;
    assert!(idea_items(&j).is_empty(), "{j}");
    assert!(
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["reason"] == "search"),
        "{j}"
    );
    assert!(
        !j["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("recall timeout")),
        "{j}"
    );
}

/// Выключенный тип подсказок `hooks.prompt` гасит и идеи.
#[tokio::test]
async fn hooks_prompt_off_silences_ideas() {
    let a = server("hooks-off", |_| {}).await;
    idea(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"prompt": false}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = prompt(&a, "s1", "экспорт отчётов в PDF").await;
    assert_eq!(text(&j), "", "{j}");
    assert!(idea_items(&j).is_empty(), "{j}");
}

/// Общая идея Knowledge (ответ владельца на вопрос 8 — вариант А): связь с текущей работой —
/// только тег или `area` задачи `in_progress`, захваченной сессией, либо ребро именно к такой
/// задаче. Почти дословный запрос без связи — нет (косинус не учитывается); ребро к insight
/// решения — нет; ребро к задаче чужой сессии или не `in_progress` — нет.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_knowledge_idea_needs_session_task_link() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("ideas-kn");
    let a = server("kn-link", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let post = |path: String, body: Value| {
        let a = &a;
        async move {
            let r = a.t(Method::POST, &path, Some(body)).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            r.json()["id"].as_str().unwrap().to_string()
        }
    };
    let kn_idea = |title: &'static str, tags: Value, area: Option<&'static str>| {
        let mut body = json!({"kind": "idea", "title": title, "body": "Something to build later.",
                              "tags": tags, "data": {"origin": "human"}});
        if let Some(ar) = area {
            body["area"] = json!(ar);
        }
        post(KN.to_string(), body)
    };
    let edge = |src: String, dst: String| {
        let a = &a;
        async move {
            let r = a
                .t(
                    Method::POST,
                    "/api/v1/edges",
                    Some(json!({"src": format!("e:{src}"), "type": "relates_to",
                                "dst": format!("e:{dst}")})),
                )
                .await;
            assert!(r.status.is_success(), "{}", r.text());
        }
    };
    // задача решения, захваченная сессией `session`; `in_progress` — по флагу
    let task = |title: &'static str, extra: Value, session: &'static str, progress: bool| {
        let a = &a;
        async move {
            let mut body = json!({"kind": "task", "title": title});
            for (k, v) in extra.as_object().unwrap() {
                body[k] = v.clone();
            }
            let r = a.t(Method::POST, &sol(), Some(body)).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            let id = r.json()["id"].as_str().unwrap().to_string();
            let r = a
                .tok_h(
                    &a.token,
                    Method::POST,
                    &format!("/api/v1/entries/{id}/claim"),
                    Some(json!({})),
                    &[("x-zam-session", session)],
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            if progress {
                let r = a
                    .tok_rev(
                        &a.token,
                        Method::PATCH,
                        &format!("/api/v1/entries/{id}"),
                        Some(json!({"status": "in_progress"})),
                    )
                    .await;
                assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            }
            id
        }
    };
    let by_tag = kn_idea(
        "Export reports to PDF with embedded fonts",
        json!(["pdf"]),
        None,
    )
    .await;
    let by_area = kn_idea(
        "Telemetry dashboard for nightly benchmarks",
        json!([]),
        Some("bench"),
    )
    .await;
    let by_sol_edge = kn_idea("Offline mode for the mobile client", json!([]), None).await;
    let by_task_edge = kn_idea("Search history export to CSV", json!([]), None).await;
    let by_open_edge = kn_idea("Keyboard shortcuts cheat sheet", json!([]), None).await;
    let insight = post(
        sol(),
        json!({"kind": "insight", "title": "Mobile client caches nothing",
               "body": "No offline storage yet.", "data": {"kind": "discovery"}}),
    )
    .await;
    edge(by_sol_edge.clone(), insight).await;
    for id in [
        &by_tag,
        &by_area,
        &by_sol_edge,
        &by_task_edge,
        &by_open_edge,
    ] {
        let eid: Id = id.parse().unwrap();
        a.st.index.worker.notify();
        until(
            "идея проиндексирована",
            INDEXED,
            async || {
                zam_store::get_entry(&db, eid).await.unwrap().index_state
                    == zam_store::IndexState::Indexed
            },
        )
        .await;
    }
    let w = |id: &str| format!("w:{id}");
    // почти дословный запрос без связи — нет (косинус не учитывается)
    let p = "export reports to PDF with embedded fonts";
    let j = prompt(&a, "s1", p).await;
    assert!(!idea_items(&j).contains(&w(&by_tag)), "без связи: {j}");
    // тег задачи `in_progress` сессии s2 — да; другая сессия — нет
    task("Отчёты", json!({"tags": ["pdf"]}), "s2", true).await;
    let j = prompt(&a, "s2", p).await;
    assert_eq!(idea_items(&j), [w(&by_tag)], "{j}");
    let j = prompt(&a, "s3", p).await;
    assert!(idea_items(&j).is_empty(), "{j}");
    // `area` задачи `in_progress` сессии s4 — да; без задачи — нет
    task("Бенчмарки", json!({"area": "bench"}), "s4", true).await;
    let pa = "telemetry dashboard for nightly benchmarks";
    let j = prompt(&a, "s5", pa).await;
    assert!(!idea_items(&j).contains(&w(&by_area)), "без связи: {j}");
    let j = prompt(&a, "s4", pa).await;
    assert_eq!(idea_items(&j), [w(&by_area)], "{j}");
    // ребро к insight решения — больше не связь (ни без задач, ни в сессии с задачей)
    let ps = "offline mode for the mobile client";
    for s in ["s6", "s2"] {
        let j = prompt(&a, s, ps).await;
        assert!(!idea_items(&j).contains(&w(&by_sol_edge)), "{s}: {j}");
    }
    // ребро к задаче `in_progress` сессии s7 — да; другая сессия — нет
    let t7 = task("История поиска", json!({}), "s7", true).await;
    edge(by_task_edge.clone(), t7).await;
    let pt = "search history export to CSV";
    let j = prompt(&a, "s7", pt).await;
    assert_eq!(idea_items(&j), [w(&by_task_edge)], "{j}");
    let j = prompt(&a, "s8", pt).await;
    assert!(idea_items(&j).is_empty(), "{j}");
    // ребро к задаче сессии s9, но не `in_progress` (`open`) — нет
    let t9 = task("Горячие клавиши", json!({}), "s9", false).await;
    edge(by_open_edge.clone(), t9).await;
    let j = prompt(&a, "s9", "keyboard shortcuts cheat sheet").await;
    assert!(idea_items(&j).is_empty(), "{j}");
}

// ---- совместный доступ ----

/// Идеи общего решения (база владельца) — в дайджесте и подсказке участника, пока доступ
/// действует; после отзыва — нет.
#[tokio::test]
async fn shared_solution_ideas_in_participant_digest_until_revoked() {
    let a = api("ideas-recall-shared", |c| c.recall.timeout_ms = 30_000).await;
    let (_, o_tok) = a.new_user("Анна").await;
    let (m_id, m_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &o_tok).await, StatusCode::CREATED);
    assert_eq!(register(&a, &m_tok).await, StatusCode::ACCEPTED);
    let access = |step: &'static str| {
        let (a, o_tok, m_id) = (&a, o_tok.clone(), m_id.clone());
        async move {
            let r = a
                .tok(
                    &o_tok,
                    Method::POST,
                    &format!("/api/v1/solutions/{G}/access/{m_id}/{step}"),
                    Some(json!({})),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{step}: {}", r.text());
        }
    };
    access("approve").await;
    let id = idea_as(&a, &o_tok, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let body = json!({"event": "session_start", "solution": G});
    let j = ok_as(&a, &m_tok, Some("m1"), body.clone()).await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
    let p = json!({"event": "prompt", "solution": G, "prompt": "экспорт отчётов в PDF"});
    let j = ok_as(&a, &m_tok, Some("m2"), p.clone()).await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
    access("revoke").await;
    let j = ok_as(&a, &m_tok, Some("m3"), body).await;
    assert!(idea_items(&j).is_empty(), "{j}");
    assert!(!text(&j).contains(&id), "{j}");
    let j = ok_as(&a, &m_tok, Some("m4"), p).await;
    assert!(idea_items(&j).is_empty(), "{j}");
}

/// Исследовательский замер (обоснование ответа владельца на вопрос 8 — вариант А: косинус не
/// учитывается; связанные пары 0,45–0,73, несвязанные 0,23–0,44): абсолютный косинус dense
/// bge-m3 запроса к связанным и несвязанным общим идеям. Запуск: `cargo test --test ideas_recall
/// measure_knowledge_idea_cosine_bge_m3 -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "замер для журнала: bge-m3 на CPU"]
async fn measure_knowledge_idea_cosine_bge_m3() {
    const MODEL: &str = "bge-m3-int8";
    if !have(MODEL) {
        return;
    }
    let q = TestQdrant::start("ideas-cos-bge");
    let a = server("cos-bge", |c| {
        ml(c, MODEL);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let ideas = [
        "Экспорт отчётов в PDF со встроенными шрифтами",
        "Тёмная тема интерфейса WebUI",
        "Синхронизация карты кода по событиям git",
        "Автоматическое резервное копирование базы на внешний диск",
        "Голосовой ввод заметок",
        "Поддержка языка Kotlin в карте кода",
    ];
    let prompts = [
        "сделай выгрузку отчёта в PDF, шрифты должны быть внутри файла",
        "почини вёрстку таблицы в PDF-отчёте",
        "добавь переключатель светлой и тёмной темы",
        "карта кода не обновляется после git pull",
        "настрой бэкап sqlite перед миграцией",
        "почему падает тест миграции 0024",
        "реализуй парсер tree-sitter для Kotlin",
        "напиши функцию сортировки списка задач",
    ];
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let mut ids: Vec<Id> = Vec::new();
    for t in ideas {
        let id = idea(&a, KN, t, json!({})).await;
        ids.push(id.parse().unwrap());
    }
    a.st.index.worker.notify();
    for id in &ids {
        let id = *id;
        until(
            "идея проиндексирована",
            INDEXED,
            async || {
                zam_store::get_entry(&db, id).await.unwrap().index_state
                    == zam_store::IndexState::Indexed
            },
        )
        .await;
    }
    let emb = a.st.index.embedder.clone().unwrap();
    let vector = a.st.index.vector.clone().unwrap();
    let kn = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == zam_store::SpaceKind::Knowledge)
        .unwrap()
        .id;
    let filter = zam_index::VectorFilter {
        kinds: vec!["idea".into()],
        ..zam_index::VectorFilter::scope(a.st.admin_id, vec![kn])
    };
    for p in prompts {
        let e = emb.embed_query(p).unwrap();
        let hits = vector.nearest_dense(&e.dense, &filter, 50).await.unwrap();
        let mut row = Vec::new();
        for (t, id) in ideas.iter().zip(&ids) {
            let best = hits
                .iter()
                .filter(|h| h.target.id == *id)
                .map(|h| h.score)
                .fold(f32::NAN, f32::max);
            row.push(format!("{best:.3} «{t}»"));
        }
        eprintln!("ZAM_COSINE «{p}»: {}", row.join("; "));
    }
}

// ---- задача 7: идея со снятой задачей (§16) ----

/// Принятая «начать» идея решения: `(идея, задача)`.
async fn accepted(a: &Api, title: &str, extra: Value) -> (String, String) {
    let id = idea(a, &sol(), title, extra).await;
    let r = post(
        a,
        &id,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let task = r.json()["task"]["id"].as_str().unwrap().to_string();
    (id, task)
}

/// Снять задачу (`dropped`) токеном.
async fn drop_task(a: &Api, task: &str) {
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{task}"),
            Some(json!({"status": "dropped", "reason": "не до того"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// §16, п. 1: в дайджесте идея со снятой (удалённой) задачей — после `new`, с пометкой; живая
/// задача — нет; раздел один раз за сессию; с открытым вопросом «вернуть» — нет.
#[tokio::test]
async fn digest_reopen_mark_once_per_session() {
    let a = server("reopen-digest", |_| {}).await;
    let (dropped, t1) = accepted(&a, "Снятая идея", json!({"priority": "high"})).await;
    drop_task(&a, &t1).await;
    let (deleted, t2) = accepted(&a, "Удалённая идея", json!({"priority": "normal"})).await;
    let r = a
        .tok_rev(
            &a.token,
            Method::DELETE,
            &format!("/api/v1/entries/{t2}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let (live, _) = accepted(&a, "Живая идея", json!({"priority": "high"})).await;
    let fresh = idea(&a, &sol(), "Новая идея", json!({"priority": "low"})).await;
    let j = start(&a, "s1").await;
    let lines = section_lines(&j).unwrap_or_else(|| panic!("нет раздела: {j}"));
    assert_eq!(
        lines,
        [
            format!("{HINT}{HINT_REOPEN}"),
            format!("ZeroAIMemory: идея «Новая идея» [low] — w:{fresh}"),
            format!(
                "ZeroAIMemory: идея «Снятая идея» [high] — задача снята, можно предложить \
                 вернуть — w:{dropped}"
            ),
            format!(
                "ZeroAIMemory: идея «Удалённая идея» [normal] — задача удалена, можно \
                 предложить вернуть — w:{deleted}"
            ),
        ],
        "{j}"
    );
    assert!(!text(&j).contains(&live), "{j}");
    // повторный старт той же сессии — без раздела
    let j = start(&a, "s1").await;
    assert!(section_lines(&j).is_none(), "{j}");
    // открытый вопрос «вернуть» — в новой сессии идеи нет
    let r = post(&a, &dropped, "propose", json!({"text": "кэш снова нужен"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = start(&a, "s2").await;
    assert!(!text(&j).contains(&dropped), "{j}");
    assert!(text(&j).contains(&deleted), "{j}");
}

/// §16, п. 1: подсказка к `prompt` по идее со снятой задачей — с пометкой и указанием
/// «предложи вернуть», один раз за сессию; живая задача — тишина.
#[tokio::test]
async fn prompt_reopen_hint_mark() {
    let a = server("reopen-prompt", |_| {}).await;
    let (id, task) = accepted(&a, "Экспорт отчётов в PDF", json!({})).await;
    let j = prompt(&a, "s0", "как сделать экспорт отчётов в PDF").await;
    assert!(idea_items(&j).is_empty(), "живая задача: {j}");
    drop_task(&a, &task).await;
    let j = prompt(&a, "s1", "как сделать экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
    let line = format!(
        "ZeroAIMemory: к задаче подходит идея «Экспорт отчётов в PDF» — задача снята, можно \
         предложить вернуть — w:{id} (предложи: action=propose с text — почему пора; решает \
         человек)"
    );
    assert!(text(&j).lines().any(|l| l == line), "{j}");
    let j = prompt(&a, "s1", "экспорт отчётов в PDF").await;
    assert!(idea_items(&j).is_empty(), "{j}");
}

/// Ревью задачи 7, I1: принятая идея с живой задачей, совпадающая с запросом лучше, не вытесняет
/// новую подходящую идею даже при `prompt_top_k = 1`: основной проход ищет только `new` и
/// `deferred`, `accepted` — вторым проходом и только при идеях со снятой задачей.
#[tokio::test]
async fn prompt_live_accepted_does_not_crowd_new_idea() {
    let a = server("reopen-crowd", |c| c.ideas.prompt_top_k = 1).await;
    let (_, _) = accepted(&a, "Экспорт отчётов в PDF", json!({})).await;
    let fresh = idea(&a, &sol(), "Экспорт отчётов в PDF с картинками", json!({})).await;
    let j = prompt(&a, "s1", "экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{fresh}")], "{j}");
    // и наоборот: снятая задача — после новой, но тоже находится (новая уже показана)
    let (rel, task) = accepted(&a, "Импорт заметок Obsidian", json!({})).await;
    drop_task(&a, &task).await;
    let j = prompt(&a, "s2", "импорт заметок Obsidian").await;
    assert_eq!(idea_items(&j), [format!("w:{rel}")], "{j}");
}

/// Ревью задачи 7, I1′ (а): первый проход уже дал `prompt_max` непоказанных — второй проход
/// (принятые со снятой задачей) не выполняется; когда новая идея показана, он идёт.
#[tokio::test]
async fn prompt_second_pass_skipped_when_first_full() {
    let a = server("second-skip", |_| {}).await;
    let (_, task) = accepted(&a, "Импорт заметок Obsidian", json!({})).await;
    drop_task(&a, &task).await;
    let fresh = idea(&a, &sol(), "Экспорт отчётов в PDF архивом", json!({})).await;
    let q = "экспорт отчётов в PDF архивом";
    let ran = || {
        zam_server::api::recall::TEST_SECOND_PASS_QUERIES
            .lock()
            .unwrap()
            .iter()
            .filter(|x| x.as_str() == q)
            .count()
    };
    let j = prompt(&a, "s1", q).await;
    assert_eq!(idea_items(&j), [format!("w:{fresh}")], "{j}");
    assert_eq!(ran(), 0, "второй проход не нужен");
    // новая показана — второй проход идёт (и ничего не находит: снятая идея не про это)
    let j = prompt(&a, "s1", q).await;
    assert!(idea_items(&j).is_empty(), "{j}");
    assert_eq!(ran(), 1);
}

/// Ревью задачи 7, I1′ (в): просрочка второго прохода не гасит новую идею первого.
#[tokio::test]
async fn prompt_second_pass_timeout_keeps_first() {
    let a = api("ideas-recall-second-timeout", |c| {
        c.recall.timeout_ms = 3000;
        c.ideas.prompt_max = 2;
    })
    .await;
    register(&a, &a.token).await;
    let mark = zam_server::api::recall::TEST_SLOW_IDEA_MARK;
    let fresh = idea(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let (_, task) = accepted(&a, &format!("Экспорт отчётов в PDF {mark}"), json!({})).await;
    drop_task(&a, &task).await;
    let j = prompt(&a, "s1", "экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{fresh}")], "{j}");
}

/// Финальное ревью M5.7, I-1 (ML: MiniLM + Qdrant): балл отдельного поиска только по идеям
/// нормирован, и ближайшая по вектору идея получала 1,0 на любой запрос. Идея решения теперь
/// требует лексического подтверждения: посторонний запрос — идей нет, связанный — есть.
#[tokio::test]
async fn prompt_solution_idea_needs_lexical_match() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("ideas-lexical");
    let a = server("lexical", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let id = idea(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let eid: Id = id.parse().unwrap();
    a.st.index.worker.notify();
    until(
        "идея проиндексирована",
        INDEXED,
        async || {
            zam_store::get_entry(&db, eid).await.unwrap().index_state
                == zam_store::IndexState::Indexed
        },
    )
    .await;
    // посторонние запросы, в том числе длинный — идей нет
    for (i, q) in [
        "настрой логирование сервера",
        "Почини падающий тест миграции базы данных и обнови документацию по развёртыванию на          стенде",
    ]
    .iter()
    .enumerate()
    {
        let j = prompt(&a, &format!("neg{i}"), q).await;
        assert!(idea_items(&j).is_empty(), "посторонний запрос «{q}»: {j}");
    }
    // связанные — в том числе другие формы слов и длинный запрос Claude Code (I-1′)
    for (i, q) in [
        "как сделать экспорт отчётов в PDF",
        "экспортировать отчёты в pdf",
        "Пользователи просят, чтобы отчёты можно было скачать в PDF; посмотри модуль reports и          предложи план, с чего начать",
    ]
    .iter()
    .enumerate()
    {
        let j = prompt(&a, &format!("pos{i}"), q).await;
        assert_eq!(idea_items(&j), [format!("w:{id}")], "«{q}»: {j}");
    }
}

/// Финальное ревью M5.7, Minor 3: токену `ro` идей не предлагают — ни раздела `### Идеи`, ни
/// подсказки к `prompt` (его `propose` получил бы 403).
#[tokio::test]
async fn ro_token_gets_no_ideas() {
    let a = server("ro-token", |_| {}).await;
    let id = idea(&a, &sol(), "Экспорт отчётов в PDF", json!({})).await;
    let (_, ro) =
        a.st.system
            .issue_token(a.st.admin_id, "ro", zam_store::Access::Ro)
            .await
            .unwrap();
    let start_ro = ok_as(
        &a,
        &ro,
        Some("ro-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    assert!(section_lines(&start_ro).is_none(), "{start_ro}");
    assert!(!text(&start_ro).contains(&id), "{start_ro}");
    let pr = ok_as(
        &a,
        &ro,
        Some("ro-2"),
        json!({"event": "prompt", "solution": G, "prompt": "экспорт отчётов в PDF"}),
    )
    .await;
    assert!(idea_items(&pr).is_empty(), "{pr}");
    // тот же запрос токеном rw — идея есть
    let j = prompt(&a, "rw-1", "экспорт отчётов в PDF").await;
    assert_eq!(idea_items(&j), [format!("w:{id}")], "{j}");
}
