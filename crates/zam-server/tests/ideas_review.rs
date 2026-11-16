//! Разбор идей агентом через REST (M6.5 §5–§7, задача 3): `POST /entries/{id}/review` (исходы,
//! аудит, повтор по `Idempotency-Key`, отказы), режим `ideas.mode` (настройка пользователя,
//! умолчание сервера `[ideas] default_mode`, режим владельца базы в общем решении), `propose`
//! идеи `new` в режиме `review` — 409, правка возвращённой идеи человеком (`by: human` токена,
//! сессия WebUI); для WebUI (§9, задача 8) — счётчики статусов `entries/counts`, фильтр
//! `from_idea` и поле `idea` задачи. Сервер без моделей (поиск — FTS).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, id, ok, problem};
use serde_json::{Value, json};
use zam_server::config::IdeasMode;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e57";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const SETTINGS: &str = "/api/v1/auth/settings";

fn sol() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

fn ent(id: &str) -> String {
    format!("/api/v1/entries/{id}")
}

/// Разбор не короче `ideas.review_min_chars` (200).
fn summary() -> String {
    let s = "Идея просит показывать статус разбора в ленте. В плане такого пункта нет, в ТЗ §7 \
             журнал «Идеи» описан без статусов разбора, сделанного по теме нет. Решение — в план: \
             это канал обратной связи обкатки, без него владелец не видит, что стало с идеей."
        .to_string();
    assert!(s.chars().count() >= 200, "{}", s.chars().count());
    s
}

/// Сервер `name` с решением `G` администратора; `tweak` — конфигурация.
async fn setup_with(name: &str, tweak: impl FnOnce(&mut zam_server::config::ServerConfig)) -> Api {
    let a = api(&format!("idrev-{name}"), tweak).await;
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

async fn setup(name: &str) -> Api {
    setup_with(name, |_| {}).await
}

/// Запись токеном `tok` в `path`.
async fn create_tok(a: &Api, tok: &str, path: &str, body: Value) -> String {
    let r = a.tok(tok, Method::POST, path, Some(body)).await;
    ok(&r, StatusCode::CREATED);
    id(&r.json())
}

/// Идея токеном администратора в `path` с `origin`.
async fn idea_tok(a: &Api, path: &str, title: &str, origin: &str) -> String {
    create_tok(
        a,
        &a.token,
        path,
        json!({"kind": "idea", "title": title, "body": "Замысел идеи",
                "data": {"origin": origin}}),
    )
    .await
}

/// Задача токеном администратора в решении `G`.
async fn task_tok(a: &Api, title: &str) -> String {
    create_tok(a, &a.token, &sol(), json!({"kind": "task", "title": title})).await
}

async fn get(a: &Api, id: &str) -> Value {
    let r = a.t(Method::GET, &ent(id), None).await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// `POST /entries/{id}/review` токеном `tok` с заголовками `extra`.
async fn review_h(a: &Api, tok: &str, id: &str, body: Value, extra: &[(&str, &str)]) -> Resp {
    a.tok_h(
        tok,
        Method::POST,
        &format!("{}/review", ent(id)),
        Some(body),
        extra,
    )
    .await
}

async fn review(a: &Api, id: &str, body: Value) -> Resp {
    review_h(a, &a.token, id, body, &[]).await
}

async fn post_tok(a: &Api, id: &str, op: &str, body: Value) -> Resp {
    a.t(Method::POST, &format!("{}/{op}", ent(id)), Some(body))
        .await
}

/// Запрос сессии WebUI администратора с `If-Match` текущей ревизии.
async fn adm_rev(a: &Api, m: Method, uri: &str, body: Option<Value>) -> Resp {
    let cur = a.adm(Method::GET, uri, None, None).await;
    let rev = cur.json()["rev"].to_string();
    a.adm_h(m, uri, body, &[("if-match", rev.as_str())]).await
}

async fn audit(a: &Api) -> Vec<zam_store::AuditRow> {
    a.st.system.list_audit(500, None).await.unwrap()
}

fn rows<'a>(rows: &'a [zam_store::AuditRow], action: &str) -> Vec<&'a zam_store::AuditRow> {
    rows.iter().filter(|r| r.action == action).collect()
}

/// `ideas.mode` пользователя `user` администратором.
async fn set_mode(a: &Api, user: &str, mode: &str) {
    let r = a
        .adm(
            Method::PATCH,
            &format!("/api/v1/admin/users/{user}/settings"),
            Some(json!({"ideas": {"mode": mode}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["ideas"]["mode"], mode, "{}", r.text());
}

/// Возвращённая идея (исход `return`) токеном администратора.
async fn returned_idea(a: &Api, title: &str) -> String {
    let i = idea_tok(a, &sol(), title, "human").await;
    let r = review(
        a,
        &i,
        json!({"outcome": "return", "summary": summary(),
                "missing": "Какие статусы показывать в ленте?"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "returned");
    i
}

/// §5: `plan` токеном → 200, задача `open` в пространстве идеи, идея `accepted` с
/// `data.review` и `data.decided` агента; аудит `entry.idea.review {outcome, task}`; повтор с
/// тем же `Idempotency-Key` — тот же итог с `replay: true`, без второй задачи и аудита; второй
/// разбор без ключа — 409 `transition-not-allowed`.
#[tokio::test]
async fn review_rest_plan_and_audit() {
    let a = setup("plan").await;
    let basis = task_tok(&a, "Опорная задача плана").await;
    let i = idea_tok(&a, &sol(), "Статусы разбора в ленте", "human").await;
    let body = json!({
        "outcome": "plan",
        "summary": summary(),
        "basis": [format!("k:{basis}")],
        "task": {"body": "1. Статусы в ленте\n2. Плашка разбора", "priority": "high"},
        "message": "разбор идеи",
    });
    let key = [("idempotency-key", "rev-plan-1")];
    let r = review_h(&a, &a.token, &i, body.clone(), &key).await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["replay"], false, "{j}");
    let task = &j["task"];
    let tid = id(task);
    assert_eq!(task["kind"], "task");
    assert_eq!(task["status"], "open");
    assert_eq!(task["title"], "Статусы разбора в ленте");
    assert_eq!(task["space_id"], j["entry"]["space_id"]);
    assert_eq!(task["data"]["priority"], "high");
    assert!(
        task["body"].as_str().unwrap().contains(&format!("w:{i}")),
        "{task}"
    );
    let e = &j["entry"];
    assert_eq!(e["status"], "accepted");
    assert_eq!(e["data"]["task_id"], tid.as_str());
    let rv = &e["data"]["review"];
    assert_eq!(rv["outcome"], "plan");
    assert_eq!(rv["summary"], summary());
    assert_eq!(rv["basis"], json!([format!("k:{basis}")]));
    assert_eq!(rv["task"], format!("k:{tid}"));
    assert_eq!(rv["by_user"], a.admin_id());
    assert!(rv.get("by").is_none(), "признак агента — decided.by: {rv}");
    assert_eq!(e["data"]["decided"]["by"], "agent");
    assert_eq!(e["data"]["decided"]["channel"], "review");
    assert_eq!(e["data"]["decided"]["decision"], "accept");
    let all = audit(&a).await;
    let rs = rows(&all, "entry.idea.review");
    assert_eq!(rs.len(), 1, "{rs:?}");
    assert_eq!(rs[0].target.as_deref(), Some(i.as_str()));
    assert_eq!(rs[0].details["outcome"], "plan");
    assert_eq!(rs[0].details["task"], tid.as_str());
    assert_eq!(rs[0].details["kind"], "idea");
    // в аудите нет текста разбора
    assert!(!rs[0].details.to_string().contains("Идея просит"));

    // повтор после обрыва связи: тот же ключ — тот же итог, без второй задачи и аудита
    let r = review_h(&a, &a.token, &i, body.clone(), &key).await;
    ok(&r, StatusCode::OK);
    let j2 = r.json();
    assert_eq!(j2["replay"], true, "{j2}");
    assert_eq!(j2["task"]["id"], tid.as_str());
    assert_eq!(j2["entry"]["rev"], e["rev"]);
    assert_eq!(rows(&audit(&a).await, "entry.idea.review").len(), 1);
    let r = a
        .t(Method::GET, &format!("{}?kind=task", sol()), None)
        .await;
    ok(&r, StatusCode::OK);
    let tasks = r.json()["items"].as_array().unwrap().len();
    assert_eq!(tasks, 2, "опорная и одна задача плана: {}", r.text());

    // второй разбор той же идеи (без ключа) — 409 transition-not-allowed
    let r = review(&a, &i, body).await;
    problem(&r, StatusCode::CONFLICT, "conflict");
    assert_eq!(r.json()["code"], "transition-not-allowed", "{}", r.text());

    // plan с task.id открытой задачи — без новой задачи, ребро на идею
    let k = idea_tok(&a, &sol(), "Совпадает с пунктом плана", "human").await;
    let r = review(
        &a,
        &k,
        json!({"outcome": "plan", "summary": summary(), "task": {"id": format!("k:{tid}")}}),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["task"]["id"], tid.as_str());
    assert_eq!(r.json()["entry"]["data"]["task_id"], tid.as_str());
    // decline и done — без задачи в ответе
    let d = idea_tok(&a, &sol(), "Отклонить", "human").await;
    let r = review(&a, &d, json!({"outcome": "decline", "summary": summary()})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "declined");
    assert!(r.json().get("task").is_none(), "{}", r.text());
    let all = audit(&a).await;
    let rs = rows(&all, "entry.idea.review");
    assert_eq!(rs.len(), 3);
    let last = rs
        .iter()
        .find(|r| r.target.as_deref() == Some(d.as_str()))
        .unwrap();
    assert_eq!(last.details["outcome"], "decline");
    assert!(last.details.get("task").is_none(), "{:?}", last.details);
}

/// Отказы `review`: сессия WebUI — 422 «разбор — действие агента»; идея Knowledge — 422; идея
/// «от агента» — 409 `idea-agent-origin`; не идея — 422; ошибки тела — 400/422; `data.review`
/// в `PATCH` — 422 «поле ведёт сервер»; режим `propose` у владельца — 409 `idea-mode`.
#[tokio::test]
async fn review_rest_refusals() {
    let a = setup("refusals").await;
    let i = idea_tok(&a, &sol(), "Идея человека", "human").await;
    let plan = json!({"outcome": "plan", "summary": summary()});
    // сессия WebUI (человек) — 422
    let r = a
        .adm(
            Method::POST,
            &format!("{}/review", ent(&i)),
            Some(plan.clone()),
            None,
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        r.text().contains("разбор — действие агента"),
        "{}",
        r.text()
    );
    // общая идея Knowledge — 422 (остаётся по M5.7)
    let kn = idea_tok(&a, KN, "Общая идея", "human").await;
    let r = review(&a, &kn, plan.clone()).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("Knowledge"), "{}", r.text());
    assert_eq!(get(&a, &kn).await["status"], "new");
    // идея «от агента» — 409 idea-agent-origin
    let ag = idea_tok(&a, &sol(), "Идея агента", "agent").await;
    let r = review(&a, &ag, plan.clone()).await;
    problem(&r, StatusCode::CONFLICT, "idea-agent-origin");
    // не идея — 422
    let t = task_tok(&a, "Просто задача").await;
    let r = review(&a, &t, plan.clone()).await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    // ошибки тела: неизвестное поле (в том числе by) — 400; неверные значения — 422 с именем
    // поля в начале detail
    let ghost = format!("k:{}", zam_core::Id::new());
    let bad = StatusCode::BAD_REQUEST;
    let unp = StatusCode::UNPROCESSABLE_ENTITY;
    for (body, want, field) in [
        (
            json!({"outcome": "plan", "summary": summary(), "by": "agent"}),
            bad,
            "",
        ),
        (
            json!({"outcome": "plan", "summary": summary(), "task": {"name": "x"}}),
            bad,
            "",
        ),
        (
            json!({"outcome": "maybe", "summary": summary()}),
            unp,
            "outcome:",
        ),
        (json!({"summary": summary()}), unp, "outcome:"),
        (json!({"outcome": "plan"}), unp, "summary:"),
        (
            json!({"outcome": "plan", "summary": "коротко"}),
            unp,
            "summary:",
        ),
        (
            json!({"outcome": "return", "summary": summary()}),
            unp,
            "missing:",
        ),
        (
            json!({"outcome": "plan", "summary": summary(),
                   "task": {"id": format!("k:{t}"), "title": "и новая"}}),
            unp,
            "task:",
        ),
        (
            json!({"outcome": "plan", "summary": summary(), "task": {"id": "k:не-id"}}),
            unp,
            "task.id:",
        ),
        (
            json!({"outcome": "plan", "summary": summary(), "task": {"priority": "urgent"}}),
            unp,
            "task.priority:",
        ),
        (
            json!({"outcome": "plan", "summary": summary(), "basis": [ghost.clone()]}),
            unp,
            "basis:",
        ),
        (
            json!({"outcome": "plan", "summary": summary(), "space": "knowledge"}),
            unp,
            "space:",
        ),
    ] {
        let r = review(&a, &i, body.clone()).await;
        assert_eq!(r.status, want, "{body}: {}", r.text());
        let detail = r.json()["detail"].as_str().unwrap_or_default().to_string();
        assert!(detail.starts_with(field), "{body}: {detail}");
    }
    assert_eq!(get(&a, &i).await["status"], "new", "отказы идею не меняют");
    // `space` идеи (GUID решения) допустим
    let s = idea_tok(&a, &sol(), "С пространством", "human").await;
    let r = review(
        &a,
        &s,
        json!({"outcome": "decline", "summary": summary(), "space": G}),
    )
    .await;
    ok(&r, StatusCode::OK);
    // data.review в PATCH — 422 «поле ведёт сервер» (сессия по идее человека, токен по своей)
    for key in ["review", "reviews", "returned_at"] {
        let r = adm_rev(
            &a,
            Method::PATCH,
            &ent(&i),
            Some(json!({"data": {key: {"outcome": "plan"}}})),
        )
        .await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains("поле ведёт сервер"), "{}", r.text());
    }
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&ag),
            Some(json!({"data": {"review": {"outcome": "plan"}}})),
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("поле ведёт сервер"), "{}", r.text());
    // режим propose у владельца базы — 409 idea-mode; обратно review — разбор проходит
    set_mode(&a, &a.admin_id(), "propose").await;
    let r = review(&a, &i, plan.clone()).await;
    problem(&r, StatusCode::CONFLICT, "idea-mode");
    assert!(r.text().contains("propose"), "{}", r.text());
    assert_eq!(get(&a, &i).await["status"], "new");
    set_mode(&a, &a.admin_id(), "review").await;
    let r = review(&a, &i, plan).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "accepted");
}

/// Прямой SQL по базе администратора (наступивший `horizon`).
async fn sql(a: &Api, q: &'static str, id: &str, v: &str) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let (id, v) = (id.to_string(), v.to_string());
    let n = db
        .write(move |tx| Ok(tx.execute(q, [id, v])?))
        .await
        .unwrap();
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

/// §7: в режиме `review` `propose` идеи `new` решения — 409 `idea-mode` («идею разбирает
/// агент»); `deferred` с наступившим `horizon`, `accepted` со снятой задачей (вопрос «вернуть»,
/// M5.7 §16) и общая идея Knowledge `new` предлагаются как в M5.7; в режиме `propose` — `propose`
/// идеи `new` проходит.
#[tokio::test]
async fn propose_new_idea_in_review_mode_is_409() {
    let a = setup("propose-mode").await;
    let i = idea_tok(&a, &sol(), "Новая идея", "human").await;
    let r = post_tok(&a, &i, "propose", json!({"text": "подходит"})).await;
    problem(&r, StatusCode::CONFLICT, "idea-mode");
    assert!(r.text().contains("action=review"), "{}", r.text());
    assert_eq!(get(&a, &i).await["status"], "new");
    // отложенная человеком, horizon наступил — предлагается вопросом
    let d = idea_tok(&a, &sol(), "Отложенная идея", "human").await;
    let r = post_tok(
        &a,
        &d,
        "decide",
        json!({"decision": "defer", "by": "human", "horizon": "2099-01-01"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    sql(
        &a,
        "UPDATE entries SET data = json_set(data, '$.horizon', ?2) WHERE id = ?1",
        &d,
        &yesterday(),
    )
    .await;
    let r = post_tok(&a, &d, "propose", json!({"text": "срок наступил"})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "proposed");
    // принятая человеком, задача снята — вопрос «вернуть» по M5.7 (§16), статус прежний
    let acc = idea_tok(&a, &sol(), "Принятая идея", "human").await;
    let r = post_tok(
        &a,
        &acc,
        "decide",
        json!({"decision": "accept", "by": "human"}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let task = id(&r.json()["task"]);
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&task),
            Some(json!({"status": "dropped", "reason": "не до того"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let r = post_tok(&a, &acc, "propose", json!({"text": "кэш снова нужен"})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "accepted", "{}", r.text());
    assert_eq!(
        r.json()["question"]["data"]["proposal"],
        "reopen",
        "{}",
        r.text()
    );
    // общая идея Knowledge — по M5.7
    let kn = idea_tok(&a, KN, "Общая идея", "human").await;
    let r = post_tok(&a, &kn, "propose", json!({})).await;
    ok(&r, StatusCode::OK);
    // идея «от агента» — прежняя ошибка M5.7, а не режим
    let ag = idea_tok(&a, &sol(), "Идея агента", "agent").await;
    let r = post_tok(&a, &ag, "propose", json!({})).await;
    problem(&r, StatusCode::CONFLICT, "idea-agent-origin");
    // режим propose — поведение M5.7
    set_mode(&a, &a.admin_id(), "propose").await;
    let r = post_tok(&a, &i, "propose", json!({})).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["entry"]["status"], "proposed");
    // умолчание сервера propose — то же без настройки пользователя
    let b = setup_with("propose-default", |c| {
        c.ideas.default_mode = IdeasMode::Propose;
    })
    .await;
    let j = idea_tok(&b, &sol(), "Новая идея", "human").await;
    let r = post_tok(&b, &j, "propose", json!({})).await;
    ok(&r, StatusCode::OK);
    let r = review(&b, &j, json!({"outcome": "decline", "summary": summary()})).await;
    problem(&r, StatusCode::CONFLICT, "idea-mode");
}

/// §6: правка возвращённой идеи токеном с `by: human` — снова `new` (без `returned_at`, история
/// разборов остаётся); без `by` (агент дописал) — остаётся `returned`; правка сессией WebUI —
/// `new`. `by` — только у токена и только с правкой заголовка или тела идеи, иначе 422.
#[tokio::test]
async fn human_patch_returned_by_token_with_by_human() {
    let a = setup("by-human").await;
    let i = returned_idea(&a, "Вернуть на доработку").await;
    assert!(get(&a, &i).await["data"]["returned_at"].is_string());
    // агент дописал — статус прежний
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&i),
            Some(json!({"append_body": "Агент: уточню у человека."})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "returned");
    // by: agent — то же
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&i),
            Some(json!({"append_body": "Агент: ещё заметка.", "by": "agent"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "returned");
    // слова человека из чата — by: human: снова на разбор
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&i),
            Some(json!({"append_body": "Человек: статусы — все шесть.", "by": "human"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let e = r.json();
    assert_eq!(e["status"], "new", "{e}");
    assert!(e["data"].get("returned_at").is_none(), "{e}");
    assert_eq!(e["data"]["reviews"].as_array().unwrap().len(), 1, "{e}");
    assert!(
        e["body"]
            .as_str()
            .unwrap()
            .contains("Человек: статусы — все шесть."),
        "{e}"
    );
    // идею человека токен по-прежнему не переписывает, и с by: human (только дополнение)
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &ent(&i),
            Some(json!({"title": "Другой заголовок", "by": "human"})),
        )
        .await;
    problem(&r, StatusCode::FORBIDDEN, "idea-human-owned");
    // by — только с правкой заголовка или тела идеи и только у токена
    let t = task_tok(&a, "Задача").await;
    for (uri, body) in [
        (ent(&i), json!({"by": "human"})),
        (ent(&i), json!({"tags": ["ux"], "by": "human"})),
        (ent(&t), json!({"append_body": "x", "by": "human"})),
    ] {
        let r = a
            .tok_rev(&a.token, Method::PATCH, &uri, Some(body.clone()))
            .await;
        ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.text().contains("by"), "{body}: {}", r.text());
    }
    let r = adm_rev(
        &a,
        Method::PATCH,
        &ent(&i),
        Some(json!({"append_body": "x", "by": "human"})),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("by"), "{}", r.text());
    // правка человеком в WebUI — тоже на разбор
    let w = returned_idea(&a, "Правка в WebUI").await;
    let r = adm_rev(
        &a,
        Method::PATCH,
        &ent(&w),
        Some(json!({"body": "Замысел идеи: все шесть статусов."})),
    )
    .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "new");
}

/// §7: `ideas.mode` в `GET/PATCH /auth/settings` и у администратора: по умолчанию — `[ideas]
/// default_mode` сервера; правка сессией с аудитом `{key, from, to}`; строка не из режимов — 422,
/// не строка — 400; повтор действующего значения — без аудита; [`ideas_mode`] — то же значение.
#[tokio::test]
async fn ideas_mode_setting_roundtrip() {
    let a = setup("mode-setting").await;
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    ok(&r, StatusCode::OK);
    assert_eq!(
        r.json()["ideas"],
        json!({"proactive": true, "mode": "review"})
    );
    let admin: zam_core::Id = a.admin_id().parse().unwrap();
    let mode = zam_server::api::user_settings::ideas_mode(&a.st, admin)
        .await
        .unwrap();
    assert_eq!(mode, IdeasMode::Review);
    let r = a
        .adm(
            Method::PATCH,
            SETTINGS,
            Some(json!({"ideas": {"mode": "propose"}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["ideas"]["mode"], "propose");
    assert_eq!(r.json()["ideas"]["proactive"], true);
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    assert_eq!(r.json()["ideas"]["mode"], "propose");
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/admin/users/{}/settings", a.admin_id()),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["ideas"]["mode"], "propose");
    let mode = zam_server::api::user_settings::ideas_mode(&a.st, admin)
        .await
        .unwrap();
    assert_eq!(mode, IdeasMode::Propose);
    // строка не из режимов — 422, не строка (в том числе null) — 400, как тип `proactive`;
    // настройка прежняя
    for (bad, want) in [
        (
            json!({"ideas": {"mode": "auto"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"ideas": {"mode": ""}}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (json!({"ideas": {"mode": 1}}), StatusCode::BAD_REQUEST),
        (json!({"ideas": {"mode": null}}), StatusCode::BAD_REQUEST),
    ] {
        let r = a
            .adm(Method::PATCH, SETTINGS, Some(bad.clone()), None)
            .await;
        ok(&r, want);
        assert!(r.text().contains("ideas.mode"), "{bad}: {}", r.text());
    }
    let r = a.adm(Method::GET, SETTINGS, None, None).await;
    assert_eq!(r.json()["ideas"]["mode"], "propose");
    // токен свою настройку не меняет
    let r = a
        .t(
            Method::PATCH,
            SETTINGS,
            Some(json!({"ideas": {"mode": "review"}})),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN);
    // повтор того же значения — без аудита; оба поля ideas в одной правке
    let r = a
        .adm(
            Method::PATCH,
            SETTINGS,
            Some(json!({"ideas": {"mode": "propose", "proactive": false}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(
        r.json()["ideas"],
        json!({"proactive": false, "mode": "propose"})
    );
    let all = audit(&a).await;
    let rs = rows(&all, "user.settings.update");
    let details: Vec<&Value> = rs.iter().map(|r| &r.details).collect();
    assert_eq!(details.len(), 2, "{details:?}");
    assert!(
        details.contains(&&json!({"key": "ideas.mode", "from": "review", "to": "propose"})),
        "{details:?}"
    );
    assert!(
        details.contains(&&json!({"key": "ideas.proactive", "from": true, "to": false})),
        "{details:?}"
    );
    // умолчание сервера — propose: без настройки пользователя действует оно
    let b = setup_with("mode-default", |c| {
        c.ideas.default_mode = IdeasMode::Propose
    })
    .await;
    let r = b.adm(Method::GET, SETTINGS, None, None).await;
    assert_eq!(r.json()["ideas"]["mode"], "propose");
    let admin_b: zam_core::Id = b.admin_id().parse().unwrap();
    let mode = zam_server::api::user_settings::ideas_mode(&b.st, admin_b)
        .await
        .unwrap();
    assert_eq!(mode, IdeasMode::Propose);
    // review пользователя важнее умолчания сервера
    let r = b
        .adm(
            Method::PATCH,
            SETTINGS,
            Some(json!({"ideas": {"mode": "review"}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["ideas"]["mode"], "review");
    let mode = zam_server::api::user_settings::ideas_mode(&b.st, admin_b)
        .await
        .unwrap();
    assert_eq!(mode, IdeasMode::Review);
}

/// §7 «Общие решения»: участник `ro` — 403; участник `rw` разбирает, `data.review.by_user` —
/// его id, задача — в базе владельца; действует режим владельца базы: `propose` участника не
/// мешает, `propose` владельца — 409 `idea-mode` и участнику.
#[tokio::test]
async fn review_shared_solution_rights_and_owner_mode() {
    let a = api("idrev-shared", |_| {}).await;
    let (o_id, o_tok) = a.new_user("Ольга").await;
    let (m_id, m_tok) = a.new_user("Марк").await;
    let (r_id, r_tok) = a.new_user("Рита").await;
    let r = a
        .tok(
            &o_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Общее"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    for t in [&m_tok, &r_tok] {
        let r = a
            .tok(
                t,
                Method::POST,
                "/api/v1/solutions",
                Some(json!({"agent_solution_id": G, "name": "Общее"})),
            )
            .await;
        ok(&r, StatusCode::ACCEPTED);
    }
    a.set_access(&o_tok, G, &m_id, "rw").await;
    a.set_access(&o_tok, G, &r_id, "ro").await;
    let idea = |title: &str| json!({"kind": "idea", "title": title, "body": "Замысел", "data": {"origin": "human"}});
    let i = create_tok(&a, &o_tok, &sol(), idea("Идея владельца")).await;
    let j = create_tok(&a, &o_tok, &sol(), idea("Вторая идея")).await;
    let plan = json!({"outcome": "plan", "summary": summary()});
    // участник ro — 403
    let r = review_h(&a, &r_tok, &i, plan.clone(), &[]).await;
    ok(&r, StatusCode::FORBIDDEN);
    // режим участника propose при режиме владельца review — не мешает
    set_mode(&a, &m_id, "propose").await;
    let r = review_h(&a, &m_tok, &i, plan.clone(), &[]).await;
    ok(&r, StatusCode::OK);
    let jr = r.json();
    assert_eq!(jr["entry"]["status"], "accepted");
    assert_eq!(jr["entry"]["data"]["review"]["by_user"], m_id.as_str());
    let tid: zam_core::Id = id(&jr["task"]).parse().unwrap();
    let o_db = a.st.users.get(o_id.parse().unwrap()).await.unwrap();
    assert!(
        zam_store::get_entry(&o_db, tid).await.is_ok(),
        "задача — в базе владельца"
    );
    let m_db = a.st.users.get(m_id.parse().unwrap()).await.unwrap();
    assert!(zam_store::get_entry(&m_db, tid).await.is_err());
    // аудит — пользователь участника, база владельца
    let all = audit(&a).await;
    let rs = rows(&all, "entry.idea.review");
    assert_eq!(rs.len(), 1, "{rs:?}");
    assert_eq!(rs[0].user_id.map(|u| u.to_string()), Some(m_id.clone()));
    assert_eq!(rs[0].details["owner"], o_id.as_str());
    // режим владельца propose — участнику тоже 409, propose участника идеи new — проходит
    set_mode(&a, &o_id, "propose").await;
    set_mode(&a, &m_id, "review").await;
    let r = review_h(&a, &m_tok, &j, plan, &[]).await;
    problem(&r, StatusCode::CONFLICT, "idea-mode");
    let r = a
        .tok(
            &m_tok,
            Method::POST,
            &format!("{}/propose", ent(&j)),
            Some(json!({})),
        )
        .await;
    ok(&r, StatusCode::OK);
}

/// Лента токеном: `items` ответа `GET <path>`.
async fn items(a: &Api, path: &str) -> Vec<Value> {
    let r = a.t(Method::GET, path, None).await;
    ok(&r, StatusCode::OK);
    r.json()["items"].as_array().unwrap().clone()
}

fn ids_of(items: &[Value]) -> Vec<String> {
    items.iter().map(id).collect()
}

/// §9 (WebUI, задача 8): `GET /spaces/{space}/entries/counts?kind=idea` — все статусы идеи
/// (нулевые тоже), числа совпадают с лентой по каждому статусу и с её фильтрами (`origin`);
/// `from_idea=1` у задач — только задачи с системным ребром `derived_from` на живую идею (и у
/// счётчиков задач); у задачи из идеи в `GET` и в ленте — `idea {id, title, status}` первой идеи
/// и `ideas` — все; у идеи ленты — `task` с заголовком; ошибки фильтров — 400/422.
#[tokio::test]
async fn entries_counts_and_from_idea() {
    let a = setup("counts").await;
    let n1 = idea_tok(&a, &sol(), "Новая первая", "human").await;
    idea_tok(&a, &sol(), "Новая вторая", "human").await;
    idea_tok(&a, &sol(), "Своя идея агента", "agent").await;
    returned_idea(&a, "На доработку").await;
    let pl = idea_tok(&a, &sol(), "В план идея", "human").await;
    let r = review(&a, &pl, json!({"outcome": "plan", "summary": summary()})).await;
    ok(&r, StatusCode::OK);
    let tid = id(&r.json()["task"]);
    let d = idea_tok(&a, &sol(), "Отклонить идею", "human").await;
    let r = review(&a, &d, json!({"outcome": "decline", "summary": summary()})).await;
    ok(&r, StatusCode::OK);
    let plain = task_tok(&a, "Обычная задача").await;

    // счётчики: все статусы вида, в том числе нулевые
    let r = a
        .t(Method::GET, &format!("{}/counts?kind=idea", sol()), None)
        .await;
    ok(&r, StatusCode::OK);
    let c = r.json();
    assert_eq!(
        c,
        json!({"new": 3, "proposed": 0, "accepted": 1, "deferred": 0, "declined": 1,
                "done": 0, "returned": 1}),
        "{c}"
    );
    // счётчик каждого статуса — столько же, сколько строк ленты с этим статусом
    for (s, n) in c.as_object().unwrap() {
        let got = items(&a, &format!("{}?kind=idea&status={s}&limit=200", sol())).await;
        assert_eq!(got.len() as u64, n.as_u64().unwrap(), "статус {s}");
    }
    // фильтр ленты: только идеи человека
    let r = a
        .t(
            Method::GET,
            &format!("{}/counts?kind=idea&origin=human", sol()),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["new"], 2, "{}", r.text());
    let human_new = items(&a, &format!("{}?kind=idea&status=new&origin=human", sol())).await;
    assert_eq!(human_new.len(), 2);

    // from_idea=1 — только задача из идеи; у неё — idea, у обычной — нет
    let from = items(&a, &format!("{}?kind=task&from_idea=1", sol())).await;
    assert_eq!(ids_of(&from), vec![tid.clone()]);
    assert_eq!(from[0]["idea"]["id"], pl.as_str());
    assert_eq!(from[0]["idea"]["title"], "В план идея");
    assert_eq!(from[0]["idea"]["status"], "accepted");
    assert_eq!(from[0]["idea"]["space_id"], from[0]["space_id"]);
    let all = items(&a, &format!("{}?kind=task", sol())).await;
    assert_eq!(all.len(), 2, "{all:?}");
    let p = all.iter().find(|t| id(t) == plain).unwrap();
    assert!(p.get("idea").is_none() && p.get("ideas").is_none(), "{p}");
    let r = a
        .t(
            Method::GET,
            &format!("{}/counts?kind=task&from_idea=1", sol()),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["open"], 1, "{}", r.text());
    // GET задачи — idea первой идеи
    let t = get(&a, &tid).await;
    assert_eq!(t["idea"]["id"], pl.as_str(), "{t}");
    assert_eq!(t["ideas"].as_array().unwrap().len(), 1);
    // у идеи ленты — задача с заголовком (строка решения «В план → k:<id> «…»»)
    let acc = items(&a, &format!("{}?kind=idea&status=accepted", sol())).await;
    assert_eq!(acc[0]["task"]["id"], tid.as_str(), "{:?}", acc[0]);
    assert_eq!(acc[0]["task"]["title"], "В план идея");
    assert_eq!(acc[0]["task"]["status"], "open");

    // вторая идея на ту же задачу (plan с task.id): ideas — обе по времени ребра, idea — исходная
    // идея задачи (n1 создана раньше pl, но привязана к задаче позже)
    let r = review(
        &a,
        &n1,
        json!({"outcome": "plan", "summary": summary(), "task": {"id": format!("k:{tid}")}}),
    )
    .await;
    ok(&r, StatusCode::OK);
    let t = get(&a, &tid).await;
    assert_eq!(t["idea"]["id"], pl.as_str(), "исходная идея задачи: {t}");
    let both: Vec<&str> = t["ideas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(both, vec![pl.as_str(), n1.as_str()]);
    // идеи в корзине (человек) — задача больше не «из идеи»
    for i in [&n1, &pl] {
        let r = adm_rev(&a, Method::DELETE, &ent(i), None).await;
        assert!(r.status.is_success(), "{}", r.text());
    }
    assert!(
        items(&a, &format!("{}?kind=task&from_idea=1", sol()))
            .await
            .is_empty()
    );
    let t = get(&a, &tid).await;
    assert!(t.get("idea").is_none(), "{t}");

    // ошибки: без kind и вид без статусов — 400, фильтр не своего вида — 422, флаг — 400
    for (q, s) in [
        ("counts", StatusCode::BAD_REQUEST),
        ("counts?kind=changelog", StatusCode::BAD_REQUEST),
        (
            "counts?kind=task&origin=human",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "counts?kind=idea&from_idea=1",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("counts?kind=task&from_idea=2", StatusCode::BAD_REQUEST),
        ("?kind=idea&from_idea=1", StatusCode::UNPROCESSABLE_ENTITY),
        ("?kind=task&from_idea=yes", StatusCode::BAD_REQUEST),
    ] {
        let sep = if q.starts_with('?') { "" } else { "/" };
        let r = a.t(Method::GET, &format!("{}{sep}{q}", sol()), None).await;
        ok(&r, s);
    }
}

/// §9, ревью задачи 8: у задачи общего решения, появившейся из личной идеи Knowledge владельца
/// («Начать» с задачей в решении), участник идеи не видит — ни в `GET`, ни в ленте, ни в фильтре
/// `from_idea` и счётчиках; владелец видит. Идея самого решения видна обоим.
#[tokio::test]
async fn task_idea_hidden_from_member_when_in_owner_knowledge() {
    let a = api("idrev-task-idea-rights", |_| {}).await;
    let (_o_id, o_tok) = a.new_user("Ольга").await;
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
    let idea = |title: &str| json!({"kind": "idea", "title": title, "body": "Замысел", "data": {"origin": "human"}});
    // личная идея Knowledge владельца → «Начать» с задачей в общем решении
    let k = create_tok(&a, &o_tok, KN, idea("Личная идея владельца")).await;
    let r = a
        .tok(
            &o_tok,
            Method::POST,
            &format!("{}/decide", ent(&k)),
            Some(json!({"decision": "accept", "by": "human", "space": G})),
        )
        .await;
    ok(&r, StatusCode::OK);
    let kt = id(&r.json()["task"]);
    // идея самого решения → разбор «в план»
    let s = create_tok(&a, &o_tok, &sol(), idea("Идея решения")).await;
    let r = review_h(
        &a,
        &o_tok,
        &s,
        json!({"outcome": "plan", "summary": summary()}),
        &[],
    )
    .await;
    ok(&r, StatusCode::OK);
    let st = id(&r.json()["task"]);

    let get_as = |tok: String, id: String| {
        let a = &a;
        async move {
            let r = a.tok(&tok, Method::GET, &ent(&id), None).await;
            ok(&r, StatusCode::OK);
            r.json()
        }
    };
    let list_as = |tok: String, q: &'static str| {
        let a = &a;
        async move {
            let r = a
                .tok(&tok, Method::GET, &format!("{}{q}", sol()), None)
                .await;
            ok(&r, StatusCode::OK);
            r.json()
        }
    };
    // владелец: обе задачи «из идеи», у задачи Knowledge — её идея
    let t = get_as(o_tok.clone(), kt.clone()).await;
    assert_eq!(t["idea"]["id"], k.as_str(), "{t}");
    assert_ne!(
        t["idea"]["space_id"], t["space_id"],
        "идея — в Knowledge: {t}"
    );
    let from = list_as(o_tok.clone(), "?kind=task&from_idea=1").await;
    let mut ids: Vec<String> = from["items"].as_array().unwrap().iter().map(id).collect();
    ids.sort();
    let mut want = vec![kt.clone(), st.clone()];
    want.sort();
    assert_eq!(ids, want);
    let c = list_as(o_tok.clone(), "/counts?kind=task&from_idea=1").await;
    assert_eq!(c["open"], 2, "{c}");

    // участник: у задачи из Knowledge владельца идеи нет; идея решения — есть
    let t = get_as(m_tok.clone(), kt.clone()).await;
    assert!(t.get("idea").is_none() && t.get("ideas").is_none(), "{t}");
    let t = get_as(m_tok.clone(), st.clone()).await;
    assert_eq!(t["idea"]["id"], s.as_str(), "{t}");
    let all = list_as(m_tok.clone(), "?kind=task").await;
    let kt_row = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| id(x) == kt)
        .cloned()
        .unwrap();
    assert!(kt_row.get("idea").is_none(), "{kt_row}");
    let from = list_as(m_tok.clone(), "?kind=task&from_idea=1").await;
    let ids: Vec<String> = from["items"].as_array().unwrap().iter().map(id).collect();
    assert_eq!(ids, vec![st.clone()]);
    let c = list_as(m_tok.clone(), "/counts?kind=task&from_idea=1").await;
    assert_eq!(c["open"], 1, "{c}");
}
