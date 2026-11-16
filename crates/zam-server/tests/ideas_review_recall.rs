//! Очередь разбора идей в `/recall` (M6.5 §7, задача 4): раздел дайджеста `### Идеи на разбор`
//! (только первый `session_start` сессии, `high` первыми, затем самые старые, хвост — кандидаты
//! предложения M5.7 с указанием `action=propose`), строка плана из идей, пакет
//! `event=idea_review` (похожие записи решения, история разборов и доводы человека, `limit`),
//! подсказка к `prompt` в режиме `review`, режим `propose` — поведение M5.7, режим владельца базы
//! в общем решении. Сервер без моделей (поиск — FTS).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, id, ok};
use serde_json::{Value, json};
use zam_server::config::{IdeasMode, ServerConfig};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e61";
const RECALL: &str = "/api/v1/recall";
const REVIEW_SECTION: &str = "### Идеи на разбор";
const SECTION: &str = "### Идеи";
/// Строка-указание раздела M5.7 (режим `propose`).
const HINT: &str = "Предложи человеку одну: memory_update_entry action=propose — вопрос «начать / \
                    отложить / отказаться»; без ответа «начать» не начинай.";

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

/// Строка-указание очереди разбора с длиной `n`.
fn review_hint(n: usize) -> String {
    format!(
        "Непросмотренных идей: {n}. Разбери, когда закончишь текущий шаг: memory_recall \
         event=idea_review → memory_update_entry action=review."
    )
}

/// Разбор не короче `ideas.review_min_chars` (200); `head` — первая строка.
fn summary(head: &str) -> String {
    let s = format!(
        "{head}\nВ плане такого пункта нет, в ТЗ журнал «Идеи» описан без этого, сделанного по \
         теме нет. Решение принято по сверке с планом, ТЗ и сделанным: так владелец видит, что \
         стало с идеей, и не повторяет обсуждённое."
    );
    assert!(s.chars().count() >= 200, "{}", s.chars().count());
    s
}

/// Сервер с решением `G` администратора; `recall.timeout_ms` поднят (отладочная сборка на HDD);
/// режим идей — умолчание сервера `review`, если `tweak` его не меняет.
async fn server(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let a = api(&format!("idrev-recall-{name}"), |c| {
        c.recall.timeout_ms = 30_000;
        tweak(c);
    })
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Проект"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    a
}

/// Запись токеном `tok` в решение `G`; id.
async fn create_tok(a: &Api, tok: &str, body: Value) -> String {
    let r = a.tok(tok, Method::POST, &sol(), Some(body)).await;
    ok(&r, StatusCode::CREATED);
    id(&r.json())
}

/// Идея человека токеном администратора с телом `body` и полями `extra` в `data`.
async fn idea(a: &Api, title: &str, body: &str, extra: Value) -> String {
    let mut data = json!({"origin": "human"});
    for (k, v) in extra.as_object().unwrap() {
        data[k] = v.clone();
    }
    create_tok(
        a,
        &a.token,
        json!({"kind": "idea", "title": title, "body": body, "data": data}),
    )
    .await
}

/// Задача токеном администратора.
async fn task(a: &Api, title: &str, body: &str, data: Value) -> String {
    create_tok(
        a,
        &a.token,
        json!({"kind": "task", "title": title, "body": body, "data": data}),
    )
    .await
}

async fn post(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.t(
        Method::POST,
        &format!("/api/v1/entries/{id}/{op}"),
        Some(body),
    )
    .await
}

/// Статус задачи `task` токеном администратора (с `If-Match`).
async fn task_status(a: &Api, task: &str, status: &str) {
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{task}"),
            Some(json!({"status": status})),
        )
        .await;
    ok(&r, StatusCode::OK);
}

/// Разбор идеи токеном администратора; ответ `{entry, task?}`.
async fn review(a: &Api, id: &str, body: Value) -> Value {
    let r = post(a, id, "review", body).await;
    ok(&r, StatusCode::OK);
    r.json()
}

async fn recall_as(a: &Api, tok: &str, session: Option<&str>, body: Value) -> Value {
    let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
    let r = a.tok_h(tok, Method::POST, RECALL, Some(body), &h).await;
    ok(&r, StatusCode::OK);
    r.json()
}

async fn recall(a: &Api, session: Option<&str>, body: Value) -> Value {
    recall_as(a, &a.token, session, body).await
}

/// `session_start` решения `G` в сессии `session`.
async fn start(a: &Api, session: &str) -> Value {
    recall(
        a,
        Some(session),
        json!({"event": "session_start", "solution": G}),
    )
    .await
}

/// Пакет разбора идей решения `G` с полями `extra`.
async fn pack(a: &Api, extra: Value) -> Value {
    let mut body = json!({"event": "idea_review", "solution": G});
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    recall(a, None, body).await
}

fn text(j: &Value) -> &str {
    j["text"].as_str().unwrap()
}

/// Ссылки элементов ответа с `reason` по порядку (все — `kind: idea`).
fn items(j: &Value, reason: &str) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["reason"] == reason)
        .map(|i| {
            assert_eq!(i["kind"], "idea", "{i}");
            i["id"].as_str().unwrap().to_string()
        })
        .collect()
}

/// Строки раздела `title` (без заголовка) до пустой строки; нет раздела — `None`.
fn section_lines(j: &Value, title: &str) -> Option<Vec<String>> {
    let rest = text(j).split(&format!("{title}\n")).nth(1)?;
    let body = rest.split("\n\n").next().unwrap();
    Some(body.lines().map(str::to_string).collect())
}

/// Блок идеи `w` пакета: от её строки до строки следующей идеи.
fn idea_block<'t>(t: &'t str, w: &str) -> &'t str {
    let at = t
        .find(&format!("— w:{w}\n"))
        .unwrap_or_else(|| panic!("нет идеи {w}: {t}"));
    let rest = &t[at..];
    // строка самой идеи начинается раньше `at`: следующая строка `ZeroAIMemory: ` — уже другая
    let end = rest.find("\nZeroAIMemory: ").unwrap_or(rest.len());
    &rest[..end]
}

fn tomorrow() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(1))
        .unwrap()
        .to_string()
}

fn yesterday() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_sub(jiff::Span::new().days(1))
        .unwrap()
        .to_string()
}

/// Отложенная идея с наступившим `horizon`: `defer` человеком, затем `horizon` — вчера (SQL).
async fn defer_due(a: &Api, id: &str) {
    let r = post(
        a,
        id,
        "decide",
        json!({"decision": "defer", "by": "human", "horizon": tomorrow()}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let (id, y) = (id.to_string(), yesterday());
    let q = "UPDATE entries SET data = json_set(data, '$.horizon', ?2) WHERE id = ?1";
    let n = db
        .write(move |tx| Ok(tx.execute(q, [id, y])?))
        .await
        .unwrap();
    assert_eq!(n, 1);
}

// ---- дайджест ----

/// 4 идеи `new` (одна `high`) → первый `session_start` — раздел `### Идеи на разбор`: строка
/// «Непросмотренных идей: 4» и 3 строки — `high`, затем самые старые; `items` — `reason:
/// idea_review`. Второй `session_start` той же сессии (обычный и после `compact`) — без раздела;
/// новая сессия — снова с ним.
#[tokio::test]
async fn digest_review_section_first_start_only() {
    let a = server("first", |_| {}).await;
    let old = idea(&a, "Обычная старая", "Замысел.", json!({})).await;
    let mid = idea(
        &a,
        "Обычная средняя",
        "Замысел.",
        json!({"priority": "normal"}),
    )
    .await;
    let high = idea(&a, "Срочная идея", "Замысел.", json!({"priority": "high"})).await;
    let new = idea(&a, "Обычная новая", "Замысел.", json!({})).await;
    let j = start(&a, "s1").await;
    let lines = section_lines(&j, REVIEW_SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(lines.len(), 4, "указание и 3 идеи: {j}");
    assert_eq!(lines[0], review_hint(4), "{j}");
    assert_eq!(
        lines[1],
        format!("ZeroAIMemory: идея «Срочная идея» [high] — w:{high}"),
        "{j}"
    );
    assert!(lines[2].ends_with(&format!("w:{old}")), "{j}");
    assert!(lines[3].ends_with(&format!("w:{mid}")), "{j}");
    assert!(!text(&j).contains(&new), "{j}");
    assert!(!text(&j).contains(&format!("{SECTION}\n")), "{j}");
    assert!(!text(&j).contains("propose"), "{j}");
    assert_eq!(
        items(&j, "idea_review"),
        [high, old, mid].map(|i| format!("w:{i}")),
        "{j}"
    );
    assert!(items(&j, "idea").is_empty(), "{j}");
    // второй старт той же сессии — без раздела: обычный и после сжатия
    let j = start(&a, "s1").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    let j = recall(
        &a,
        Some("s1"),
        json!({"event": "session_start", "solution": G, "source": "compact"}),
    )
    .await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    // новая сессия — снова
    let j = start(&a, "s2").await;
    assert!(text(&j).contains(REVIEW_SECTION), "{j}");
}

/// Идея `deferred` с наступившим `horizon` в режиме `review` — строкой после очереди разбора с
/// указанием `action=propose` (`reason: idea`); без очереди — раздел M5.7 `### Идеи` из неё
/// одной, как в M5.7.
#[tokio::test]
async fn digest_review_keeps_deferred_due_proposal() {
    let a = server("tail", |_| {}).await;
    let queued = idea(&a, "Идея в очереди", "Замысел.", json!({})).await;
    let due = idea(&a, "Отложенная созревшая", "Замысел.", json!({})).await;
    defer_due(&a, &due).await;
    let j = start(&a, "s1").await;
    let lines = section_lines(&j, REVIEW_SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(lines.len(), 3, "{j}");
    assert_eq!(lines[0], review_hint(1), "{j}");
    assert_eq!(
        lines[1],
        format!("ZeroAIMemory: идея «Идея в очереди» [normal] — w:{queued}"),
        "{j}"
    );
    assert!(
        lines[2].starts_with(
            "ZeroAIMemory: идея «Отложенная созревшая» [normal] — срок отсрочки вышел"
        ),
        "{j}"
    );
    assert!(lines[2].contains(&format!("w:{due}")), "{j}");
    assert!(lines[2].contains("action=propose"), "{j}");
    assert_eq!(items(&j, "idea_review"), [format!("w:{queued}")], "{j}");
    assert_eq!(items(&j, "idea"), [format!("w:{due}")], "{j}");
    // очередь разобрана — раздел M5.7 из одной отложенной идеи
    review(
        &a,
        &queued,
        json!({"outcome": "return", "summary": summary("Не хватает подробностей."),
               "missing": "Что именно показывать?"}),
    )
    .await;
    let j = start(&a, "s2").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    let lines = section_lines(&j, SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(
        lines,
        [
            HINT.to_string(),
            format!("ZeroAIMemory: идея «Отложенная созревшая» [normal] — w:{due}")
        ],
        "{j}"
    );
}

/// Строка плана: две открытые задачи из идей (`normal` старше, `high` новее) — следующая
/// `high`; задача без ребра к идее (даже `high`) и снятая задача из идеи не считаются.
#[tokio::test]
async fn digest_plan_line_orders_by_priority() {
    let a = server("plan", |_| {}).await;
    let plan = |title: &str, priority: &str| {
        json!({"outcome": "plan", "summary": summary("Решение — в план."),
               "task": {"title": title, "body": "1. Шаг.", "priority": priority}})
    };
    let i1 = idea(&a, "Идея обычная", "Замысел.", json!({})).await;
    let t1 = review(&a, &i1, plan("Обычный пункт", "normal")).await;
    let i2 = idea(&a, "Идея срочная", "Замысел.", json!({})).await;
    let t2 = id(&review(&a, &i2, plan("Срочный пункт", "high")).await["task"]);
    task(
        &a,
        "Ручная задача",
        "Без идеи.",
        json!({"priority": "high"}),
    )
    .await;
    let i3 = idea(&a, "Идея снятая", "Замысел.", json!({})).await;
    let t3 = id(&review(&a, &i3, plan("Снятый пункт", "high")).await["task"]);
    task_status(&a, &t3, "dropped").await;
    assert_eq!(t1["task"]["status"], "open", "{t1}");
    let t1 = id(&t1["task"]);
    let j = start(&a, "s1").await;
    let line = format!("План из идей: 2 открытых, следующая — k:{t2} «Срочный пункт» [high]");
    assert!(text(&j).lines().any(|l| l == line), "{j}");
    // строка — не подсказка записи: без `items`
    assert!(
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"] != format!("k:{t2}")),
        "{j}"
    );
    // и в пакете восстановления
    let j = recall(
        &a,
        Some("s1"),
        json!({"event": "session_start", "solution": G, "source": "compact"}),
    )
    .await;
    assert!(text(&j).lines().any(|l| l == line), "{j}");
    // ревью задачи 4, Important 2: взятый в работу пункт — в счёте, но «следующим» не бывает
    task_status(&a, &t2, "in_progress").await;
    let j = start(&a, "s2").await;
    let line = format!("План из идей: 2 открытых, следующая — k:{t1} «Обычный пункт» [normal]");
    assert!(text(&j).lines().any(|l| l == line), "{j}");
    // открытых не осталось — строка без «следующая»
    task_status(&a, &t1, "blocked").await;
    let j = start(&a, "s3").await;
    assert!(
        text(&j).lines().any(|l| l == "План из идей: 2 открытых"),
        "{j}"
    );
    assert!(!text(&j).contains("следующая"), "{j}");
}

/// `ideas.proactive = false` пользователя гасит и раздел «Идеи на разбор», и строку плана; пакет
/// `idea_review` — явный вызов — по-прежнему отвечает.
#[tokio::test]
async fn proactive_off_hides_review_section_and_plan() {
    let a = server("proactive-off", |_| {}).await;
    let planned = idea(&a, "Идея в плане", "Замысел.", json!({})).await;
    review(
        &a,
        &planned,
        json!({"outcome": "plan", "summary": summary("Решение — в план.")}),
    )
    .await;
    let queued = idea(&a, "Идея в очереди", "Замысел.", json!({})).await;
    let j = start(&a, "on").await;
    assert!(text(&j).contains(&format!("{REVIEW_SECTION}\n")), "{j}");
    assert!(text(&j).contains("План из идей: 1 открытых"), "{j}");
    let r = a
        .adm(
            Method::PATCH,
            &format!("/api/v1/admin/users/{}/settings", a.admin_id()),
            Some(json!({"ideas": {"proactive": false}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let j = start(&a, "off").await;
    assert!(!text(&j).contains("### Идеи"), "{j}");
    assert!(!text(&j).contains("План из идей"), "{j}");
    assert!(items(&j, "idea_review").is_empty(), "{j}");
    let j = pack(&a, json!({})).await;
    assert_eq!(items(&j, "idea_review"), [format!("w:{queued}")], "{j}");
}

/// Режим `review`: идея `accepted` со снятой задачей — строкой-хвостом очереди с пометкой «задача
/// снята, можно предложить вернуть» и указанием «предложи вернуть: action=propose с text».
#[tokio::test]
async fn digest_review_tail_released_task() {
    let a = server("tail-released", |_| {}).await;
    let queued = idea(&a, "Идея в очереди", "Замысел.", json!({})).await;
    let started = idea(&a, "Начатая идея", "Замысел.", json!({})).await;
    let r = post(
        &a,
        &started,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    task_status(&a, &id(&r.json()["task"]), "dropped").await;
    let j = start(&a, "s1").await;
    let lines = section_lines(&j, REVIEW_SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(lines.len(), 3, "{j}");
    assert_eq!(lines[0], review_hint(1), "{j}");
    assert!(lines[1].ends_with(&format!("w:{queued}")), "{j}");
    assert_eq!(
        lines[2],
        format!(
            "ZeroAIMemory: идея «Начатая идея» [normal] — задача снята, можно предложить вернуть \
             — w:{started} (предложи вернуть: action=propose с text — почему пора; решает человек)"
        ),
        "{j}"
    );
    assert_eq!(items(&j, "idea"), [format!("w:{started}")], "{j}");
}

/// Пакет с сессией отмечает показ: раздел дайджеста этой сессии не повторяется, в новой — есть.
#[tokio::test]
async fn pack_in_session_suppresses_digest_section() {
    let a = server("pack-session", |_| {}).await;
    let x = idea(&a, "Идея", "Замысел.", json!({})).await;
    let j = recall(
        &a,
        Some("s1"),
        json!({"event": "idea_review", "solution": G}),
    )
    .await;
    assert_eq!(items(&j, "idea_review"), [format!("w:{x}")], "{j}");
    let j = start(&a, "s1").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    let j = start(&a, "s2").await;
    assert!(text(&j).contains(REVIEW_SECTION), "{j}");
}

// ---- пакет idea_review ----

/// Пакет: у идеи «экспорт в PDF» похожие — отклонённая идея «PDF-экспорт отчётов» с первой
/// строкой её разбора и задача «печать отчёта», сама идея — не похожая; строка шагов разбора.
/// Отклонённую вернули на разбор с доводом — в её блоке прежний разбор и довод человека.
/// `limit: 1` — одна идея (самая старая).
#[tokio::test]
async fn idea_review_pack_has_similar_and_reasons() {
    let a = server("pack", |_| {}).await;
    let declined = idea(
        &a,
        "PDF-экспорт отчётов",
        "Выгрузка отчётов в PDF для бухгалтерии.",
        json!({}),
    )
    .await;
    let why = "Отклонено: печать отчёта уже покрывает выгрузку в PDF.";
    review(
        &a,
        &declined,
        json!({"outcome": "decline", "summary": summary(why)}),
    )
    .await;
    let print = task(
        &a,
        "Печать отчёта",
        "Печать отчёта из браузера, в том числе в PDF.",
        json!({}),
    )
    .await;
    let x = idea(
        &a,
        "Экспорт в PDF",
        "Выгружать отчёт продаж в PDF одной кнопкой.",
        json!({}),
    )
    .await;
    let j = pack(&a, json!({})).await;
    let t = text(&j);
    assert!(
        t.starts_with(&format!("{REVIEW_SECTION}\nШаги разбора: ")),
        "{j}"
    );
    assert!(t.contains("action=review"), "{j}");
    assert_eq!(items(&j, "idea_review"), [format!("w:{x}")], "{j}");
    let b = idea_block(t, &x);
    assert!(
        b.starts_with(&format!(
            "— w:{x}\n  Выгружать отчёт продаж в PDF одной кнопкой."
        )),
        "{b}"
    );
    assert!(t.contains(&format!(
        "ZeroAIMemory: идея «Экспорт в PDF» [normal] — w:{x}"
    )));
    assert!(
        b.contains(&format!(
            "\n  - w:{declined} · declined · «PDF-экспорт отчётов» — разбор: {why}"
        )),
        "{b}"
    );
    assert!(
        b.contains(&format!("\n  - k:{print} · open · «Печать отчёта»")),
        "{b}"
    );
    assert!(
        !b.contains(&format!("- w:{x}")),
        "сама идея — не похожая: {b}"
    );
    assert!(!b.contains("История:"), "{b}");
    assert!(t.ends_with("\nПоказано идей на разбор: 1 из 1.\n"), "{t}");
    // вернули на разбор с доводом — довод и прежний разбор в пакете
    let reason = "Бухгалтерии нужен именно PDF: печать из браузера не подходит.";
    let r = post(
        &a,
        &declined,
        "decide",
        json!({"decision": "reopen", "reason": reason, "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = pack(&a, json!({})).await;
    assert_eq!(
        items(&j, "idea_review"),
        [format!("w:{declined}"), format!("w:{x}")],
        "{j}"
    );
    let b = idea_block(text(&j), &declined);
    assert!(b.contains("\n  История:\n"), "{b}");
    assert!(b.contains(&format!(" разбор decline: {why}")), "{b}");
    assert!(
        b.contains(&format!(" довод человека (reopen): {reason}")),
        "{b}"
    );
    let (rev, arg) = (
        b.find(" разбор decline").unwrap(),
        b.find(" довод").unwrap(),
    );
    assert!(rev < arg, "история — по времени: {b}");
    // `limit: 1` — одна идея, «показано 1 из 2»
    let j = pack(&a, json!({"limit": 1})).await;
    assert_eq!(items(&j, "idea_review"), [format!("w:{declined}")], "{j}");
    assert!(!text(&j).contains(&format!("— w:{x}\n")), "{j}");
    assert!(
        text(&j).contains("\nПоказано идей на разбор: 1 из 2.\n"),
        "{j}"
    );
    assert!(!text(&j).contains("не показано"), "{j}");
    // не вошедшие в бюджет — строкой «не показано» с адресом (ревью задачи 4, Minor 3)
    let j = pack(&a, json!({"budget_tokens": 20})).await;
    assert!(items(&j, "idea_review").is_empty(), "{j}");
    assert!(
        text(&j).contains(
            "Показано идей на разбор: 0 из 2.\nZeroAIMemory: не показано — идей на разбор: 2 \
             (memory_recall event=idea_review после разбора показанных)\n"
        ),
        "{j}"
    );
}

/// Причина у отклонённой похожей идеи — только из её отклонения (ревью задачи 4, Important 1):
/// человек отложил «после релиза» и отказался без причины — причины нет; отказался с причиной —
/// «причина человека: …»; отклонил разбором — «разбор: …».
#[tokio::test]
async fn pack_declined_reason_only_from_decline() {
    let a = server("pack-decline", |_| {}).await;
    let silent = idea(
        &a,
        "PDF-выгрузка отчётов",
        "Выгрузка отчётов в PDF.",
        json!({}),
    )
    .await;
    let r = post(
        &a,
        &silent,
        "decide",
        json!({"decision": "defer", "by": "human", "horizon": tomorrow(),
               "reason": "после релиза"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let r = post(
        &a,
        &silent,
        "decide",
        json!({"decision": "decline", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let said = idea(
        &a,
        "PDF-отчёт бухгалтерии",
        "Отчёт в PDF для бухгалтерии.",
        json!({}),
    )
    .await;
    let r = post(
        &a,
        &said,
        "decide",
        json!({"decision": "decline", "by": "human", "reason": "дублирует печать отчёта"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let x = idea(&a, "Экспорт в PDF", "Выгружать отчёт в PDF.", json!({})).await;
    let j = pack(&a, json!({})).await;
    let b = idea_block(text(&j), &x);
    let line = |w: &str| {
        b.lines()
            .find(|l| l.starts_with(&format!("  - w:{w} ")))
            .unwrap_or_else(|| panic!("нет похожей {w}: {b}"))
            .to_string()
    };
    assert_eq!(
        line(&silent),
        format!("  - w:{silent} · declined · «PDF-выгрузка отчётов»"),
        "{b}"
    );
    assert!(!b.contains("после релиза"), "{b}");
    assert_eq!(
        line(&said),
        format!(
            "  - w:{said} · declined · «PDF-отчёт бухгалтерии» — причина человека: дублирует \
             печать отчёта"
        ),
        "{b}"
    );
}

/// Частичный пакет по сроку: у «медленной» идеи (тестовая метка) похожие не собраны, у первой —
/// собраны; `warnings: ["recall timeout"]`; обе идеи в ответе.
#[tokio::test]
async fn pack_partial_on_timeout() {
    let a = server("pack-timeout", |_| {}).await;
    let first = idea(&a, "Экспорт отчётов", "Выгрузка отчётов.", json!({})).await;
    task(&a, "Печать отчётов", "Печать.", json!({})).await;
    let mark = zam_server::api::recall::TEST_SLOW_IDEA_MARK;
    let slow = idea(&a, &format!("Медленная идея {mark}"), "Замысел.", json!({})).await;
    let j = pack(&a, json!({"timeout_ms": 3000})).await;
    assert_eq!(
        items(&j, "idea_review"),
        [format!("w:{first}"), format!("w:{slow}")],
        "{j}"
    );
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "recall timeout"),
        "{j}"
    );
    assert!(
        idea_block(text(&j), &first).contains("\n  Похожие:\n"),
        "{j}"
    );
    assert!(
        idea_block(text(&j), &slow).contains("Похожие: не собраны"),
        "{j}"
    );
}

/// Пакет без идей — строка-уведомление: очередь пуста, решения нет, нет права записи, режим
/// `propose` владельца; `limit` вне 1..=20 — 400.
#[tokio::test]
async fn idea_review_pack_notices_and_limit() {
    let a = server("notices", |_| {}).await;
    let j = pack(&a, json!({})).await;
    assert_eq!(text(&j).trim_end(), "Идей на разбор нет.", "{j}");
    assert!(j["items"].as_array().unwrap().is_empty(), "{j}");
    let j = recall(&a, None, json!({"event": "idea_review"})).await;
    assert!(
        text(&j).starts_with("Идей на разбор нет: решение не задано"),
        "{j}"
    );
    for l in [0, 21] {
        let r = a
            .t(
                Method::POST,
                RECALL,
                Some(json!({"event": "idea_review", "solution": G, "limit": l})),
            )
            .await;
        ok(&r, StatusCode::BAD_REQUEST);
        assert!(r.text().contains("limit"), "{}", r.text());
    }
    // токен `ro` — разбор дал бы 403: уведомление вместо идей
    idea(&a, "Идея", "Замысел.", json!({})).await;
    let ro = a.issue(&a.admin_id(), "ro").await;
    let j = recall_as(
        &a,
        &ro,
        None,
        json!({"event": "idea_review", "solution": G}),
    )
    .await;
    assert!(
        text(&j).starts_with("Разбор идей недоступен: нет права записи"),
        "{j}"
    );
    assert!(j["items"].as_array().unwrap().is_empty(), "{j}");
    let b = server("notices-propose", |c| {
        c.ideas.default_mode = IdeasMode::Propose
    })
    .await;
    idea(&b, "Идея", "Замысел.", json!({})).await;
    let j = pack(&b, json!({})).await;
    assert!(
        text(&j).starts_with("Разбор идей выключен: ideas.mode = propose"),
        "{j}"
    );
    assert!(j["items"].as_array().unwrap().is_empty(), "{j}");
}

// ---- подсказка к prompt ----

/// Режим `review`: подсказка к запросу у идеи `new` — «к задаче подходит идея на разбор
/// w:<id>», без `propose`; `reason: idea_review`.
#[tokio::test]
async fn prompt_hint_review_wording() {
    let a = server("prompt", |_| {}).await;
    let x = idea(&a, "Экспорт отчётов в PDF", "Замысел.", json!({})).await;
    let j = recall(
        &a,
        Some("p1"),
        json!({"event": "prompt", "solution": G, "prompt": "как сделать экспорт отчётов в PDF"}),
    )
    .await;
    let t = text(&j);
    assert!(
        t.contains(&format!("к задаче подходит идея на разбор w:{x}")),
        "{j}"
    );
    assert!(!t.contains("propose"), "{j}");
    assert_eq!(items(&j, "idea_review"), [format!("w:{x}")], "{j}");
}

// ---- режим propose ----

/// `default_mode = propose`: раздел `### Идеи` и строка-указание M5.7, `reason: idea`, без
/// строки плана (задача из «начать» — тоже из идеи), подсказка к запросу — с `propose`.
#[tokio::test]
async fn propose_mode_digest_unchanged() {
    let a = server("propose", |c| c.ideas.default_mode = IdeasMode::Propose).await;
    let x = idea(&a, "Экспорт отчётов в PDF", "Замысел.", json!({})).await;
    let started = idea(&a, "Начатая идея", "Замысел.", json!({})).await;
    let r = post(
        &a,
        &started,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let j = start(&a, "s1").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    assert!(!text(&j).contains("План из идей"), "{j}");
    let lines = section_lines(&j, SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(
        lines,
        [
            HINT.to_string(),
            format!("ZeroAIMemory: идея «Экспорт отчётов в PDF» [normal] — w:{x}")
        ],
        "{j}"
    );
    assert_eq!(items(&j, "idea"), [format!("w:{x}")], "{j}");
    let j = recall(
        &a,
        Some("p1"),
        json!({"event": "prompt", "solution": G, "prompt": "как сделать экспорт отчётов в PDF"}),
    )
    .await;
    assert!(text(&j).contains("action=propose"), "{j}");
    assert!(!text(&j).contains("на разбор"), "{j}");
}

// ---- общее решение ----

/// Общее решение: действует режим владельца базы. Участник с режимом `propose` при режиме
/// владельца `review` получает очередь разбора; владелец переключился на `propose` — участнику
/// раздел M5.7, пакет — «разбор выключен».
#[tokio::test]
async fn shared_solution_uses_owner_mode() {
    let a = api("idrev-recall-shared", |c| c.recall.timeout_ms = 30_000).await;
    let (o_id, o_tok) = a.new_user("Ольга").await;
    let (m_id, m_tok) = a.new_user("Марк").await;
    let r = a
        .tok(
            &o_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::ACCEPTED);
    a.set_access(&o_tok, G, &m_id, "rw").await;
    let i = create_tok(
        &a,
        &o_tok,
        json!({"kind": "idea", "title": "Идея владельца", "body": "Замысел.",
               "data": {"origin": "human"}}),
    )
    .await;
    let set_mode = |user: String, mode: &'static str| {
        let a = &a;
        async move {
            let r = a
                .adm(
                    Method::PATCH,
                    &format!("/api/v1/admin/users/{user}/settings"),
                    Some(json!({"ideas": {"mode": mode}})),
                    None,
                )
                .await;
            ok(&r, StatusCode::OK);
        }
    };
    set_mode(m_id.clone(), "propose").await;
    let start_m = |s: &'static str| {
        recall_as(
            &a,
            &m_tok,
            Some(s),
            json!({"event": "session_start", "solution": G}),
        )
    };
    let j = start_m("m1").await;
    let lines = section_lines(&j, REVIEW_SECTION).unwrap_or_else(|| panic!("{j}"));
    assert_eq!(lines[0], review_hint(1), "{j}");
    assert_eq!(items(&j, "idea_review"), [format!("w:{i}")], "{j}");
    // владелец — propose: участнику (режим review) раздел M5.7 и пакет «выключен»
    set_mode(o_id, "propose").await;
    set_mode(m_id, "review").await;
    let j = start_m("m2").await;
    assert!(!text(&j).contains(REVIEW_SECTION), "{j}");
    assert_eq!(items(&j, "idea"), [format!("w:{i}")], "{j}");
    let j = recall_as(
        &a,
        &m_tok,
        None,
        json!({"event": "idea_review", "solution": G}),
    )
    .await;
    assert!(text(&j).starts_with("Разбор идей выключен"), "{j}");
}
