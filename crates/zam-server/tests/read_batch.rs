//! Чтение пачкой `POST /read` (§9.3 шаг 2, §12; задача 6 M4). Сервер без моделей.
//!
//! Пользователи общего решения: A — владелец, B — запросил доступ (ожидает), C — посторонний.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, req};
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{SeenKey, WHOLE_TARGET, count_tokens, seen_revs};

const READ: &str = "/api/v1/read";
const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e52";

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn read(a: &Api, body: Value) -> Resp {
    a.t(Method::POST, READ, Some(body)).await
}

async fn read_ok(a: &Api, body: Value) -> Value {
    let r = read(a, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn insight(a: &Api, title: &str, body: &str) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": title, "body": body,
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id(&r.json())
}

/// Длинный текст: `n` строк по ~10 токенов.
fn long_text(title: &str, n: usize) -> String {
    let mut t = format!("# {title}\n\n");
    for i in 0..n {
        t.push_str(&format!("Строка номер {i} про квазар и далёкие звёзды.\n"));
    }
    t
}

/// Ревизия цели `node` целиком, выданной сессии, в базе пользователя `user`.
async fn seen_rev(
    a: &Api,
    user: &str,
    session: &str,
    kind: TargetKind,
    target: &str,
) -> Option<i64> {
    let user: Id = user.parse().unwrap();
    let db = a.st.users.get(user).await.unwrap();
    let key = SeenKey {
        owner_id: user,
        target: TargetRef {
            kind,
            id: target.parse().unwrap(),
        },
        part: WHOLE_TARGET.to_string(),
    };
    seen_revs(&db, session, std::slice::from_ref(&key))
        .await
        .unwrap()
        .get(&key)
        .copied()
}

/// Узлы по `n:`, голому id и пути, запись по `i:` и голому id — одним вызовом; уровни и секция.
#[tokio::test]
async fn read_batch_mixed_nodes_entries_paths() {
    let a = api("read-mixed", |_| {}).await;
    let text = "# A\nПервое. Второе.\n## B\nТекст B.\n";
    let n = a.note("knowledge", "Инструменты/Bash", text).await;
    let nid = id(&n);
    let e = insight(&a, "LF в bash-скриптах", "Используйте LF.\n").await;
    let j = read_ok(
        &a,
        json!({"items": [
            {"id": format!("n:{nid}")},
            {"id": nid},
            {"id": "knowledge:/Инструменты/Bash", "section": "A > B"},
            {"id": "knowledge:/Инструменты/Bash", "tier": "L1"},
            {"id": format!("i:{e}")},
            {"id": e},
            {"id": format!("i:{e}"), "tier": "L0"},
        ], "budget_tokens": 5000}),
    )
    .await;
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 7, "{j}");
    for k in [0, 1] {
        assert_eq!(items[k]["text"], text, "{j}");
        assert_eq!(items[k]["ref"], format!("n:{nid}"));
        assert_eq!(items[k]["kind"], "node");
        assert_eq!(items[k]["space"], "Knowledge");
        assert_eq!(items[k]["path"], "Инструменты/Bash");
        assert_eq!(items[k]["rev"], 1);
        assert!(items[k]["tokens"].as_u64().unwrap() > 0);
    }
    assert_eq!(items[1]["id"], nid);
    assert_eq!(items[2]["text"], "## B\nТекст B.\n", "{j}");
    assert_eq!(items[2]["section"], "A > B");
    assert!(
        items[3]["text"]
            .as_str()
            .unwrap()
            .starts_with("A — Первое.\n  B — Текст B."),
        "{j}"
    );
    for k in [4, 5] {
        assert_eq!(items[k]["text"], "Используйте LF.\n", "{j}");
        assert_eq!(items[k]["ref"], format!("i:{e}"));
        assert_eq!(items[k]["kind"], "insight");
        assert_eq!(items[k]["title"], "LF в bash-скриптах");
        assert_eq!(items[k]["rev"], 1);
    }
    assert_eq!(items[6]["text"], "LF в bash-скриптах", "{j}");
    assert!(j["total_tokens"].as_u64().unwrap() > 0);
}

/// `if_rev` равен текущей ревизии — `unchanged` без текста; другая — текст.
#[tokio::test]
async fn read_if_rev_unchanged() {
    let a = api("read-if-rev", |_| {}).await;
    let nid = id(&a.note("knowledge", "n", "# Один\n").await);
    let e = insight(&a, "Запись", "тело").await;
    let j = read_ok(
        &a,
        json!({"items": [
            {"id": &nid, "if_rev": 1},
            {"id": &nid, "if_rev": 7},
            {"id": &e, "if_rev": 1},
            {"id": &e, "if_rev": 0},
        ]}),
    )
    .await;
    let it = j["items"].as_array().unwrap();
    assert_eq!(it[0]["unchanged"], true, "{j}");
    assert!(it[0].get("text").is_none(), "{j}");
    assert_eq!(it[0]["rev"], 1);
    assert_eq!(it[1]["text"], "# Один\n");
    assert!(it[1].get("unchanged").is_none());
    assert_eq!(it[2]["unchanged"], true, "{j}");
    assert_eq!(it[3]["text"], "тело");

    // после правки прежняя ревизия уже не «текущая»
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({"content": "# Два\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let j = read_ok(&a, json!({"items": [{"id": &nid, "if_rev": 1}]})).await;
    assert_eq!(j["items"][0]["text"], "# Два\n", "{j}");
    assert_eq!(j["items"][0]["rev"], 2);
    let r = read(
        &a,
        json!({"items": [{"id": &nid, "if_rev": 2}], "format": "md"}),
    )
    .await;
    let md = r.text();
    assert!(md.contains("rev2 · unchanged"), "{md}");
    assert!(!md.contains("Два"), "{md}");
}

/// Не влезший в бюджет элемент обрезается с пометкой, следующие — `(budget)`; `max_tokens`.
#[tokio::test]
async fn read_budget_truncates_with_marker() {
    let a = api("read-budget", |_| {}).await;
    let big = long_text("Большая", 80);
    let b1 = id(&a.note("knowledge", "big1", &big).await);
    let b2 = id(&a.note("knowledge", "big2", &big).await);
    let full = count_tokens(&big);
    assert!(full > 300, "{full}");

    let j = read_ok(
        &a,
        json!({"items": [{"id": &b1}, {"id": &b2}], "budget_tokens": 150}),
    )
    .await;
    let it = j["items"].as_array().unwrap();
    assert_eq!(it[0]["truncated"], true, "{j}");
    let more = it[0]["more_tokens"].as_u64().unwrap();
    let text = it[0]["text"].as_str().unwrap();
    assert!(big.starts_with(text) && text.len() < big.len());
    assert!(
        more > 0 && (count_tokens(text) as u64) + more <= full as u64 + 1,
        "{j}"
    );
    assert_eq!(it[0]["tokens"], full);
    assert_eq!(it[1]["budget"], true, "{j}");
    assert!(it[1].get("text").is_none());
    assert!(j["total_tokens"].as_u64().unwrap() <= 150, "{j}");

    // md: пометка и `(budget)`
    let r = read(
        &a,
        json!({"items": [{"id": &b1}, {"id": &b2}], "budget_tokens": 150, "format": "md"}),
    )
    .await;
    let md = r.text();
    assert!(md.contains(&format!("…ещё {more} токенов")), "{md}");
    assert!(md.contains("\n(budget)\n"), "{md}");
    assert!(md.starts_with("### n:"), "{md}");

    // max_tokens режет элемент независимо от бюджета
    let j = read_ok(
        &a,
        json!({"items": [{"id": &b1, "max_tokens": 40}, {"id": &b2}], "budget_tokens": 5000}),
    )
    .await;
    let it = j["items"].as_array().unwrap();
    assert_eq!(it[0]["truncated"], true, "{j}");
    assert!(count_tokens(it[0]["text"].as_str().unwrap()) <= 40, "{j}");
    assert!(it[1].get("truncated").is_none(), "{j}");
    assert_eq!(it[1]["text"], big);

    // бюджет по умолчанию — из конфигурации (1500)
    let huge = id(&a
        .note("knowledge", "huge", &long_text("Огромная", 400))
        .await);
    let j = read_ok(&a, json!({"items": [{"id": huge}]})).await;
    assert_eq!(j["items"][0]["truncated"], true, "{j}");
    assert!(j["total_tokens"].as_u64().unwrap() <= 1500, "{j}");
}

struct W {
    a: Api,
    a_tok: String,
    b_id: String,
    b_tok: String,
    c_tok: String,
}

/// A регистрирует решение, B запрашивает доступ (без одобрения).
async fn world(name: &str) -> (W, String, String) {
    let a = api(&format!("read-{name}"), |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_, c_tok) = a.new_user("Вера").await;
    let reg = json!({ "agent_solution_id": G, "name": "Общее" });
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "Общая", "kind": "note",
                        "content": "# Общая\n\nтекст решения\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let node = id(&r.json());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(json!({"kind": "insight", "title": "Общий", "body": "x",
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let entry = id(&r.json());
    (
        W {
            a,
            a_tok,
            b_id,
            b_tok,
            c_tok,
        },
        node,
        entry,
    )
}

/// Ошибки лежат в элементах, ответ — 200: чужой Human, решение без доступа, посторонний,
/// неизвестный id, мусор, удалённый узел, нет секции, папка.
#[tokio::test]
async fn read_item_errors_inline() {
    let (w, node, entry) = world("errors").await;
    let a = &w.a;
    // Human администратора: токен его не видит
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space":"human","path":"тайна","kind":"note","content":"секрет\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let human = id(&r.json());
    let own = id(&a.note("knowledge", "своя", "# Своя\n\nтекст\n").await);
    let folder = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "папка", "kind": "folder"})),
        )
        .await;
    assert_eq!(folder.status, StatusCode::CREATED, "{}", folder.text());
    let folder = id(&folder.json());
    let gone = id(&a.note("knowledge", "удалю", "# Х\n").await);
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{gone}"), None)
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let unknown = Id::new().to_string();

    let j = read_ok(
        a,
        json!({"items": [
            {"id": &human},
            {"id": "human:/тайна"},
            {"id": &unknown},
            {"id": format!("n:{unknown}")},
            {"id": "мусор"},
            {"id": &gone},
            {"id": &own, "section": "Нет такой"},
            {"id": &folder},
            {"id": &own},
        ]}),
    )
    .await;
    let it = j["items"].as_array().unwrap();
    let err = |k: usize| it[k]["error"].as_str().unwrap_or("").to_string();
    assert_eq!(err(0), "not_found", "{j}");
    assert_eq!(err(1), "not_found", "{j}");
    assert_eq!(err(2), "not_found", "{j}");
    assert_eq!(err(3), "not_found", "{j}");
    assert_eq!(err(4), "not_found", "{j}");
    assert_eq!(err(5), "not_found", "{j}");
    assert_eq!(err(6), "not_found", "{j}");
    assert_eq!(it[6]["detail"], "секция не найдена");
    assert_eq!(err(7), "invalid", "{j}");
    assert_eq!(it[0]["id"], human);
    assert!(!j.to_string().contains("секрет"), "{j}");
    // остальные элементы читаются
    assert_eq!(it[8]["text"], "# Своя\n\nтекст\n", "{j}");

    // B ждёт одобрения: решение — solution_access, но свои узлы читаются
    let body = json!({"items": [{"id": &node}, {"id": &entry}, {"id": format!("{G}:/Общая")}]});
    let r =
        w.a.tok(&w.b_tok, Method::POST, READ, Some(body.clone()))
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    for k in 0..3 {
        let e = &j["items"][k];
        assert_eq!(e["error"], "solution_access", "{j}");
        assert_eq!(e["state"], "pending", "{j}");
        assert_eq!(e["agent_solution_id"], G, "{j}");
    }
    assert!(!j.to_string().contains("текст решения"), "{j}");
    // посторонний C: решения для него нет
    let r = w.a.tok(&w.c_tok, Method::POST, READ, Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    for k in 0..2 {
        assert_eq!(j["items"][k]["error"], "not_found", "{j}");
    }
    assert!(!j.to_string().contains("текст решения"), "{j}");

    // md: ошибка — заголовок с кодом
    let r =
        w.a.tok(
            &w.c_tok,
            Method::POST,
            READ,
            Some(json!({"items": [{"id": &node}], "format": "md"})),
        )
        .await;
    assert!(r.text().contains("error: not_found"), "{}", r.text());
}

/// Пустой список, больше `read.max_items`, `locator`, некорректные поля — ошибки запроса.
#[tokio::test]
async fn read_max_items_422() {
    let a = api("read-limits", |c| c.read.max_items = 3).await;
    let nid = id(&a.note("knowledge", "n", "# N\n").await);
    let item = || json!({"id": &nid});
    let r = read(&a, json!({"items": [item(), item(), item()]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = read(&a, json!({"items": [item(), item(), item(), item()]})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = read(&a, json!({"items": []})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // locator — M5
    let r = read(&a, json!({"items": [{"id": &nid, "locator": "p.4"}]})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/locator-unsupported");
    for bad in [
        json!({"items": [{"id": &nid, "tier": "L9"}]}),
        json!({"items": [{"id": &nid, "max_tokens": 0}]}),
        json!({"items": [item()], "budget_tokens": 0}),
        json!({"items": [item()], "format": "html"}),
        json!({"items": [item()], "session_id": ""}),
        json!({"items": [{"id": &nid, "лишнее": 1}]}),
        json!({}),
    ] {
        let r = read(&a, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    // по умолчанию 20
    let b = api("read-limits-default", |_| {}).await;
    let many: Vec<Value> = (0..21)
        .map(|_| json!({"id": Id::new().to_string()}))
        .collect();
    let r = read(&b, json!({"items": many[..20]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = read(&b, json!({"items": many})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // без токена — 401
    let r = b
        .send(
            req(Method::POST, READ, Some(&json!({"items": [item()]})))
                .body(axum::body::Body::from(
                    json!({"items": [item()]}).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

/// Отмечается только полный текст L2 без секции и без обрезки; `/read` — не запись в память.
#[tokio::test]
async fn read_marks_seen() {
    let a = api("read-seen", |_| {}).await;
    let user = a.admin_id();
    let text = "# A\nПервое.\n## B\nТекст B.\n";
    let whole = id(&a.note("knowledge", "whole", text).await);
    let sect = id(&a.note("knowledge", "sect", text).await);
    let tier1 = id(&a.note("knowledge", "tier1", text).await);
    let cut = id(&a.note("knowledge", "cut", &long_text("Длинная", 80)).await);
    let same = id(&a.note("knowledge", "same", text).await);
    let ent = insight(&a, "Запись", "тело записи").await;
    let ent_l0 = insight(&a, "Другая", "тело другой").await;
    let h = [("x-zam-session", "s-read")];
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            READ,
            Some(json!({"items": [
                {"id": &whole},
                {"id": &sect, "section": "A > B"},
                {"id": &tier1, "tier": "L1"},
                {"id": &cut, "max_tokens": 20},
                {"id": &same, "if_rev": 1},
                {"id": &ent},
                {"id": &ent_l0, "tier": "L0"},
                {"id": Id::new().to_string()},
            ], "budget_tokens": 5000})),
            &h,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let node = |t: &str| (TargetKind::Node, t.to_string());
    assert_eq!(
        seen_rev(&a, &user, "s-read", TargetKind::Node, &whole).await,
        Some(1)
    );
    assert_eq!(
        seen_rev(&a, &user, "s-read", TargetKind::Entry, &ent).await,
        Some(1)
    );
    for (k, t) in [
        node(&sect),
        node(&tier1),
        node(&cut),
        node(&same),
        (TargetKind::Entry, ent_l0.clone()),
    ] {
        assert_eq!(seen_rev(&a, &user, "s-read", k, &t).await, None, "{t}");
    }
    // сессия создана, но чтение не записывает в память
    let s = a.t(Method::GET, "/api/v1/sessions/s-read", None).await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text());
    assert_eq!(s.json()["memory_writes"], 0);

    // обрезка бюджетом тоже не отмечает; тело `session_id` сильнее заголовка
    let r = read(
        &a,
        json!({"items": [{"id": &cut}], "budget_tokens": 60, "session_id": "s-body"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        seen_rev(&a, &user, "s-body", TargetKind::Node, &cut).await,
        None
    );
    let r = read(
        &a,
        json!({"items": [{"id": &cut}], "budget_tokens": 5000, "session_id": "s-body"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        seen_rev(&a, &user, "s-body", TargetKind::Node, &cut).await,
        Some(1)
    );
    // без сессии ничего не пишется; поиск видит выданное целиком как `seen`
    let j = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "Первое", "mode": "brief"})),
            &h,
        )
        .await
        .json();
    let items = j["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["seen"] == true), "{j}");
}

/// md: заголовки элементов и последняя строка `~<total> tok`.
#[tokio::test]
async fn read_md_total_tokens_line() {
    let a = api("read-md", |_| {}).await;
    let nid = id(&a
        .note(
            "knowledge",
            "Папка/Заметка",
            "# Т\nТекст заметки.\n## Раздел\nТело раздела.\n",
        )
        .await);
    let e = insight(&a, "LF в bash", "Используйте LF.").await;
    let r = read(
        &a,
        json!({"items": [{"id": &nid}, {"id": &nid, "section": "Т > Раздел"}, {"id": &e}],
               "format": "md", "budget_tokens": 3000}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.header("content-type").starts_with("text/markdown"));
    let md = r.text();
    let lines: Vec<&str> = md.lines().collect();
    let last = *lines.last().unwrap();
    assert!(
        last.starts_with('~') && last.ends_with(" tok"),
        "последняя строка: {last:?}\n{md}"
    );
    let total: usize = last
        .trim_start_matches('~')
        .trim_end_matches(" tok")
        .parse()
        .unwrap();
    let heads: Vec<&&str> = lines.iter().filter(|l| l.starts_with("### ")).collect();
    assert_eq!(heads.len(), 3, "{md}");
    assert!(
        heads[0].starts_with(&format!("### n:{nid} Knowledge/Папка/Заметка  rev1 · ~")),
        "{}",
        heads[0]
    );
    assert!(
        heads[1].contains("Knowledge/Папка/Заметка › Т > Раздел  rev1 · ~"),
        "{}",
        heads[1]
    );
    assert!(
        heads[2].starts_with(&format!("### i:{e} insight/gotcha «LF в bash»  active")),
        "{}",
        heads[2]
    );
    assert!(heads[2].ends_with(" tok"), "{}", heads[2]);
    assert!(md.contains("Тело раздела."));
    // total — оценка выданного без последней строки
    let body = md[..md.rfind("~").unwrap()].trim_end();
    let real = count_tokens(body);
    assert!(
        total.abs_diff(real) <= 8 + real / 10,
        "total {total} против {real}\n{md}"
    );
}

/// Принятый участник читает пространство решения, но не Knowledge владельца (по id — `not_found`).
#[tokio::test]
async fn read_member_sees_solution_not_owner_knowledge() {
    let (w, node, entry) = world("member").await;
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/approve", w.b_id),
            Some(json!({"access": "ro"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "личное", "kind": "note",
                        "content": "# Личное\n\nтолько владелец\n"}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let private = id(&r.json());
    let r =
        w.a.tok(
            &w.b_tok,
            Method::POST,
            READ,
            Some(
                json!({"items": [{"id": &node}, {"id": &entry}, {"id": &private},
                                  {"id": format!("{G}:/Общая")}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let it = j["items"].as_array().unwrap();
    assert_eq!(it[0]["text"], "# Общая\n\nтекст решения\n", "{j}");
    assert_eq!(it[1]["ref"], format!("i:{entry}"), "{j}");
    assert_eq!(it[2]["error"], "not_found", "{j}");
    assert_eq!(it[3]["text"], "# Общая\n\nтекст решения\n", "{j}");
    assert!(!j.to_string().contains("только владелец"), "{j}");
}

/// Префиксы `c:` и `r:` читают changelog и release; ссылка в ответе — по виду записи.
#[tokio::test]
async fn read_entry_prefixes_c_r() {
    let (w, _, _) = world("prefixes").await;
    let uri = format!("/api/v1/spaces/{G}/entries");
    let post = |body: Value| w.a.tok(&w.a_tok, Method::POST, &uri, Some(body));
    let c = post(
        json!({"kind": "changelog", "title": "Экспорт", "body": "Добавлен экспорт.",
                        "data": {"kind": "feat"}}),
    )
    .await;
    assert_eq!(c.status, StatusCode::CREATED, "{}", c.text());
    let c = id(&c.json());
    let r = post(json!({"kind": "release", "title": "Релиз 1.0", "body": "Заметки релиза.",
                        "data": {"version": "1.0", "commits": [{"sha": "abc123", "subject": "feat: x"}]}}))
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rel = id(&r.json());
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            READ,
            Some(
                json!({"items": [{"id": format!("c:{c}")}, {"id": format!("r:{rel}")}],
                        "format": "md", "budget_tokens": 3000}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let md = r.text();
    assert!(
        md.contains(&format!("### c:{c} changelog/feat «Экспорт»")),
        "{md}"
    );
    assert!(
        md.contains(&format!("### r:{rel} release «Релиз 1.0»")),
        "{md}"
    );
    assert!(
        md.contains("Добавлен экспорт.") && md.contains("Заметки релиза."),
        "{md}"
    );
}

/// Путь секции, совпадающий у нескольких секций, — `invalid` в элементе; ответ 200.
#[tokio::test]
async fn read_section_ambiguous_invalid() {
    let a = api("read-ambiguous", |_| {}).await;
    let nid = id(&a.note("knowledge", "двойной", "# A\nx\n# A\nz\n").await);
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "section": "A"}, {"id": &nid}]}),
    )
    .await;
    assert_eq!(j["items"][0]["error"], "invalid", "{j}");
    assert!(
        j["items"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("неоднозначен"),
        "{j}"
    );
    assert_eq!(j["items"][1]["text"], "# A\nx\n# A\nz\n");
    let r = read(&a, json!({"items": [{"id": &nid, "section": "A"}]})).await;
    assert_eq!(r.header("x-zam-read-error"), "invalid");
    let r = read(&a, json!({"items": [{"id": &nid}]})).await;
    assert!(r.headers.get("x-zam-read-error").is_none());
}
