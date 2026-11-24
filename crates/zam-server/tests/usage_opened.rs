//! «Открыл ли агент подсказку» и «Шумные» (M7.3, спецификация `docs/specs/2026-11-21-m7-polish.md`
//! §6.6 «Проверка»): чтение цели по токену явной сессией (`POST /read`, `GET /entries/{id}`,
//! `GET /nodes/{id}`, `GET /nodes/by-path`) после показа этой цели подсказкой `/recall` той же
//! сессии в той же эпохе пишет строку `usage_opened` (одну на сессию и цель); без показа, без
//! сессии, в другой эпохе и из сессии WebUI — ничего. `usage {shown, opened, useful, useless}` —
//! в карточке и ленте; лента `noisy=1` — «шумные» по окну `max(now − 30 сут,
//! meta.usage_opened_since)` и поле `opened_since`.
//!
//! Сервер без моделей: подсказка — триггер `error_regex` к ошибке (`post_tool`) или правило Rules
//! в дайджесте `session_start`; `recall.timeout_ms` поднят, как в `recall.rs`.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, enc, id, ok};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};

const RECALL: &str = "/api/v1/recall";
const READ: &str = "/api/v1/read";
const SESSION: &str = "x-zam-session";
const RULE_PATH: &str = "квазар-правило";

async fn server(name: &str) -> Api {
    let a = api(&format!("opened-{name}"), |c| c.recall.timeout_ms = 30_000).await;
    // правило Rules пишется токеном (`rules.agent_write`)
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    a
}

/// Insight Knowledge с триггером ошибки `pattern`; id без буквы.
async fn insight(a: &Api, title: &str, pattern: &str) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": title, "body": format!("{title}."),
                        "data": {"kind": "gotcha"},
                        "triggers": [{"type": "error_regex", "pattern": pattern}]}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    id(&r.json())
}

/// Ссылки подсказок ответа `/recall`.
fn hint_ids(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// Подсказка к ошибке `error` в сессии `session`; ссылка `expect` — среди показанных.
async fn hint(a: &Api, session: &str, error: &str, expect: &str) {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "post_tool", "error": error})),
            &[(SESSION, session)],
        )
        .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert!(hint_ids(&j).iter().any(|i| i == expect), "{j}");
}

/// Дайджест старта сессии `session`; ссылка `expect` — среди показанных.
async fn digest(a: &Api, session: &str, expect: &str) {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "session_start"})),
            &[(SESSION, session)],
        )
        .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert!(hint_ids(&j).iter().any(|i| i == expect), "{j}");
}

/// `POST /read` цели `reference` по токену; `session` — `X-Zam-Session` (`None` — без сессии).
async fn read(a: &Api, session: Option<&str>, reference: &str, item: Value) -> Resp {
    let mut it = json!({"id": reference});
    if let (Some(o), Some(extra)) = (it.as_object_mut(), item.as_object()) {
        o.extend(extra.clone());
    }
    let h: Vec<(&str, &str)> = session.map(|s| (SESSION, s)).into_iter().collect();
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            READ,
            Some(json!({"items": [it]})),
            &h,
        )
        .await;
    ok(&r, StatusCode::OK);
    r
}

/// `GET uri` по токену в сессии `session` (`None` — без сессии).
async fn get(a: &Api, session: Option<&str>, uri: &str) -> Value {
    let h: Vec<(&str, &str)> = session.map(|s| (SESSION, s)).into_iter().collect();
    let r = a.tok_h(&a.token, Method::GET, uri, None, &h).await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// Новая эпоха `session_seen` сессии (`POST /sessions {id, new_epoch}`).
async fn new_epoch(a: &Api, session: &str) {
    let r = a
        .t(
            Method::POST,
            "/api/v1/sessions",
            Some(json!({"id": session, "new_epoch": true})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
}

/// Строки `usage_opened` базы администратора: (сессия, вид, id цели, источник, пользователь).
async fn opened_rows(a: &Api) -> Vec<(String, String, String, Option<String>, String)> {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    db.read(|c| {
        let mut st = c.prepare(
            "SELECT session_id, target_kind, target_id, source, user_id FROM usage_opened
             ORDER BY session_id, target_kind, target_id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
    .unwrap()
}

/// Правило Rules (заметка) — подсказка дайджеста старта; id узла.
async fn rule(a: &Api) -> String {
    let n = a
        .note(
            "rules",
            RULE_PATH,
            "# Квазар\n\nПеред сборкой проверяй квазар.\n",
        )
        .await;
    n["id"].as_str().unwrap().to_string()
}

/// §6.6 п. 2: чтение цели после её подсказки в той же сессии — `POST /read` и `GET /entries/{id}`
/// (запись), `GET /nodes/{id}` и `GET /nodes/by-path` (узел) — строка `usage_opened` в базе цели;
/// `source` — событие показа, пользователь — читавший.
#[tokio::test]
async fn opened_after_hint_read() {
    let a = server("after-hint").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let me = format!("i:{e}");
    let n = rule(&a).await;
    let mn = format!("n:{n}");

    hint(&a, "s-read", "quasar", &me).await;
    read(&a, Some("s-read"), &me, json!({"tier": "L0"})).await;
    hint(&a, "s-get", "quasar", &me).await;
    get(&a, Some("s-get"), &format!("/api/v1/entries/{e}")).await;
    digest(&a, "s-node", &mn).await;
    get(&a, Some("s-node"), &format!("/api/v1/nodes/{n}")).await;
    digest(&a, "s-path", &mn).await;
    get(
        &a,
        Some("s-path"),
        &format!(
            "/api/v1/nodes/by-path?space=rules&path={}&tier=L1",
            enc(RULE_PATH)
        ),
    )
    .await;

    let admin = a.admin_id();
    let row = |s: &str, k: &str, id: &str, src: &str| {
        (
            s.to_string(),
            k.to_string(),
            id.to_string(),
            Some(src.to_string()),
            admin.clone(),
        )
    };
    assert_eq!(
        opened_rows(&a).await,
        [
            row("s-get", "entry", &e, "post_tool"),
            row("s-node", "node", &n, "session_start"),
            row("s-path", "node", &n, "session_start"),
            row("s-read", "entry", &e, "post_tool"),
        ]
    );
}

/// `/read` любой формы — секция, фокус, `unchanged` по `if_rev`, элемент `(budget)` — открытие
/// (решение исполнителя 1); ошибка элемента — нет.
#[tokio::test]
async fn opened_read_any_form() {
    let a = server("read-forms").await;
    let n = rule(&a).await;
    let mn = format!("n:{n}");
    let big = a
        .note(
            "knowledge",
            "длинная",
            &format!("# Длинная\n\n{}\n", "Слово за словом. ".repeat(200)),
        )
        .await;
    let mb = format!("n:{}", big["id"].as_str().unwrap());
    for (s, item) in [
        ("s-sec", json!({"section": "Квазар"})),
        ("s-focus", json!({"focus": "квазар"})),
        ("s-unch", json!({"if_rev": 1})),
        ("s-err", json!({"section": "Нет такой секции"})),
    ] {
        digest(&a, s, &mn).await;
        let j = read(&a, Some(s), &mn, item).await.json();
        let it = &j["items"][0];
        match s {
            "s-unch" => assert_eq!(it["unchanged"], true, "{j}"),
            "s-err" => assert!(it["error"].is_string(), "{j}"),
            _ => assert!(it["error"].is_null(), "{j}"),
        }
    }
    // правило — вторым элементом за длинной заметкой в малом бюджете: `(budget)`
    digest(&a, "s-budget", &mn).await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            READ,
            Some(json!({"items": [{"id": mb}, {"id": mn}], "budget_tokens": 40})),
            &[(SESSION, "s-budget")],
        )
        .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["items"][1]["budget"], true, "{j}");
    let rows: Vec<(String, String)> = opened_rows(&a)
        .await
        .into_iter()
        .map(|r| (r.0, r.2))
        .collect();
    let want: Vec<(String, String)> = ["s-budget", "s-focus", "s-sec", "s-unch"]
        .iter()
        .map(|s| (s.to_string(), n.clone()))
        .collect();
    assert_eq!(rows, want);
}

/// JSON-RPC `tools/call` к `/mcp` по первому токену с заголовками `extra`; текст ответа.
async fn mcp_tool(a: &Api, extra: &[(&str, &str)], name: &str, args: Value) -> String {
    let mut b = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(axum::http::header::HOST, "memory.lan:7700")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(
            axum::http::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", a.token),
        );
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let r = a
        .send(b.body(axum::body::Body::from(body.to_string())).unwrap())
        .await;
    ok(&r, StatusCode::OK);
    let j = r.json();
    assert!(j["result"]["isError"] != true, "{name}: {j}");
    j["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// MCP: служебный `GET /nodes/by-path` самого `memory_write` (id заметки для правки) — не
/// открытие; `memory_read` с явной сессией — открытие (решение исполнителя 3).
#[tokio::test]
async fn opened_via_mcp_read_not_service_get() {
    let a = server("mcp").await;
    let n = rule(&a).await;
    let mn = format!("n:{n}");
    digest(&a, "s-mcp", &mn).await;
    let h = [(SESSION, "s-mcp")];
    mcp_tool(
        &a,
        &h,
        "memory_write",
        json!({"path": format!("rules:/{RULE_PATH}"),
               "content": "# Квазар\n\nПеред сборкой дважды проверяй квазар.\n",
               "base_rev": 1}),
    )
    .await;
    assert!(opened_rows(&a).await.is_empty());
    let text = mcp_tool(&a, &h, "memory_read", json!({"items": [{"id": mn}]})).await;
    assert!(text.contains("дважды"), "{text}");
    let rows = opened_rows(&a).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].2.as_str()),
        ("s-mcp", n.as_str())
    );
}

/// Некорректный `X-Zam-Session` у `GET /entries/{id}` и `GET /nodes/{id}` — прежний ответ 200, без
/// открытия (решение исполнителя 4: раньше заголовок здесь не читался).
#[tokio::test]
async fn get_bad_session_header_ignored() {
    let a = server("bad-session").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let n = rule(&a).await;
    let bad = "x".repeat(200);
    let plain = get(&a, None, &format!("/api/v1/entries/{e}")).await;
    let j = get(&a, Some(bad.as_str()), &format!("/api/v1/entries/{e}")).await;
    assert_eq!(j, plain);
    get(&a, Some(bad.as_str()), &format!("/api/v1/nodes/{n}")).await;
    assert!(opened_rows(&a).await.is_empty());
}

/// Одно событие на сессию и цель (первичный ключ): повторные чтения той же сессией, в том числе
/// после новой эпохи с новым показом, строк не добавляют; другая сессия — своя строка.
#[tokio::test]
async fn opened_once_per_session() {
    let a = server("once").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let me = format!("i:{e}");
    hint(&a, "s1", "quasar", &me).await;
    read(&a, Some("s1"), &me, json!({})).await;
    read(&a, Some("s1"), &me, json!({"tier": "L1"})).await;
    get(&a, Some("s1"), &format!("/api/v1/entries/{e}")).await;
    new_epoch(&a, "s1").await;
    hint(&a, "s1", "quasar", &me).await;
    read(&a, Some("s1"), &me, json!({})).await;
    assert_eq!(opened_rows(&a).await.len(), 1);
    hint(&a, "s2", "quasar", &me).await;
    read(&a, Some("s2"), &me, json!({})).await;
    let rows = opened_rows(&a).await;
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["s1", "s2"]
    );
    let j = get(&a, None, &format!("/api/v1/entries/{e}")).await;
    assert_eq!(j["usage"]["opened"], 2, "{j}");
}

/// Без показа цели этой сессии, без сессии и при показе другой цели — открытия нет.
#[tokio::test]
async fn not_opened_without_hint() {
    let a = server("no-hint").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let f = insight(&a, "Пульсар в тестах", "pulsar").await;
    let (me, mf) = (format!("i:{e}"), format!("i:{f}"));
    // показана e — прочитана f
    hint(&a, "s1", "quasar", &me).await;
    read(&a, Some("s1"), &mf, json!({})).await;
    get(&a, Some("s1"), &format!("/api/v1/entries/{f}")).await;
    // сессия без подсказок
    read(&a, Some("s2"), &me, json!({})).await;
    get(&a, Some("s2"), &format!("/api/v1/entries/{e}")).await;
    // показ в s3 — чтение без сессии
    hint(&a, "s3", "quasar", &me).await;
    read(&a, None, &me, json!({})).await;
    get(&a, None, &format!("/api/v1/entries/{e}")).await;
    assert!(opened_rows(&a).await.is_empty());
}

/// Показ в прежней эпохе сессии открытием не считается; новый показ в новой эпохе — считается.
#[tokio::test]
async fn not_opened_other_epoch() {
    let a = server("epoch").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let me = format!("i:{e}");
    hint(&a, "s1", "quasar", &me).await;
    new_epoch(&a, "s1").await;
    read(&a, Some("s1"), &me, json!({})).await;
    get(&a, Some("s1"), &format!("/api/v1/entries/{e}")).await;
    assert!(opened_rows(&a).await.is_empty());
    hint(&a, "s1", "quasar", &me).await;
    read(&a, Some("s1"), &me, json!({})).await;
    let rows = opened_rows(&a).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].3.as_deref(), Some("post_tool"));
}

/// Сессия WebUI (cookie) с заголовком сессии, в которой цель показана агенту, открытия не пишет.
#[tokio::test]
async fn not_opened_webui() {
    let a = server("webui").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let me = format!("i:{e}");
    hint(&a, "s1", "quasar", &me).await;
    let admin = a.admin_id();
    let h = [(SESSION, "s1"), ("x-zam-user", admin.as_str())];
    let r = a
        .adm_h(Method::GET, &format!("/api/v1/entries/{e}"), None, &h)
        .await;
    ok(&r, StatusCode::OK);
    let r = a
        .adm_h(
            Method::POST,
            READ,
            Some(json!({"items": [{"id": me}], "session_id": "s1"})),
            &h,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert!(opened_rows(&a).await.is_empty());
}

/// `usage {shown, opened, useful, useless}` — в `GET /entries/{id}` и у insight ленты; признак
/// `usage_opened` в `/health.features`.
#[tokio::test]
async fn usage_opened_in_entry_json() {
    let a = server("json").await;
    let e = insight(&a, "Квазар в сборке", "quasar").await;
    let me = format!("i:{e}");
    hint(&a, "s1", "quasar", &me).await;
    hint(&a, "s2", "quasar", &me).await;
    read(&a, Some("s1"), &me, json!({})).await;
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{e}/feedback"),
            Some(json!({"value": "useful"})),
        )
        .await;
    ok(&r, StatusCode::NO_CONTENT);
    let want = json!({"shown": 2, "opened": 1, "useful": 1, "useless": 0});
    let j = get(&a, None, &format!("/api/v1/entries/{e}")).await;
    assert_eq!(j["usage"], want, "{j}");
    let l = get(&a, None, "/api/v1/spaces/knowledge/entries?kind=insight").await;
    let it = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == e.as_str())
        .unwrap();
    assert_eq!(it["usage"], want, "{l}");
    // без `noisy` поля `opened_since` у ленты нет (прежний ответ)
    assert!(l.get("opened_since").is_none(), "{l}");
    let h = a
        .send(
            axum::http::Request::get("/api/v1/health")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    let feats = h.json()["features"].clone();
    assert!(
        feats
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "usage_opened"),
        "{feats}"
    );
}

/// Время в формате хранилища.
fn ts(t: Timestamp) -> String {
    zam_store::time::fmt_ts(t)
}

/// `n` событий `event` цели `target` в `usage` с временем `at` (сессия `sess`).
async fn usage_rows(a: &Api, target: &str, event: &str, at: Timestamp, n: usize, sess: &str) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let (target, event, at, sess) = (
        target.to_string(),
        event.to_string(),
        ts(at),
        sess.to_string(),
    );
    let user = a.st.admin_id;
    db.write(move |t| {
        let mut st = t.prepare(
            "INSERT INTO usage (ts, user_id, session_id, target_kind, target_id, event, source)
             VALUES (?1, ?2, ?3, 'entry', ?4, ?5, 'prompt')",
        )?;
        for _ in 0..n {
            st.execute(rusqlite::params![at, user, sess, target, event])?;
        }
        Ok(())
    })
    .await
    .unwrap();
}

/// `meta.usage_opened_since` базы администратора.
async fn set_since(a: &Api, at: Timestamp) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let v = ts(at);
    db.write(move |t| {
        t.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'usage_opened_since'",
            [v],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// Ленты «шумных» insight Knowledge: ид записей (по порядку ленты) и ответ.
async fn noisy(a: &Api) -> (Vec<String>, Value) {
    let j = get(
        a,
        None,
        "/api/v1/spaces/knowledge/entries?kind=insight&noisy=1",
    )
    .await;
    let ids = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    (ids, j)
}

/// Открытие записи `target` сессией `session` в момент `at` (`usage_opened`).
async fn opened_at(a: &Api, target: &str, session: &str, at: Timestamp) {
    zam_store::record_opened(
        &a.st.users.get(a.st.admin_id).await.unwrap(),
        &zam_store::OpenedEvent {
            ts: at,
            user_id: a.st.admin_id,
            session_id: session.into(),
            target: TargetRef {
                kind: TargetKind::Entry,
                id: target.parse::<Id>().unwrap(),
            },
            source: zam_store::UsageSource::Prompt,
        },
    )
    .await
    .unwrap();
}

/// §6.6 п. 3, решения 68 и круга правок 1 задачи 6: «шумные» — `shown ≥ 10` и `opened = 0` по
/// событиям окна `max(now − 30 сут, usage_opened_since)`, `useful = 0` — за всё время: показы до
/// начала учёта открытий и старше 30 суток в окно не входят, открытие старше окна запись не
/// защищает, а любая оценка «полезно» (до начала учёта, старше 30 суток) — исключает; у записей —
/// прежний `usage` за всё время; `opened_since` — начало учёта. Строки `usage` пишутся по порядку
/// времени, как у сервера (`id` растёт со временем записи — по нему `noisy_ids` берёт окно).
#[tokio::test]
async fn noisy_window_from_migration() {
    let a = server("noisy").await;
    let now = zam_store::time::now();
    let day = SignedDuration::from_hours(24);
    let never = "noisy-never-matches";
    let before_since = insight(&a, "До начала учёта", never).await;
    let noisy_one = insight(&a, "Шумная", never).await;
    let opened = insight(&a, "Открытая", never).await;
    let useful = insight(&a, "Полезная", never).await;
    let useful_early = insight(&a, "Полезная до учёта", never).await;
    let useful_old = insight(&a, "Полезная давно", never).await;
    let opened_old = insight(&a, "Открытая давно", never).await;
    let few = insight(&a, "Редкая", never).await;
    let old = insight(&a, "Старая", never).await;
    let since = now - day * 3;
    set_since(&a, since).await;
    // давние показы — в счётчиках записи, но не в окне
    usage_rows(&a, &noisy_one, "recall_shown", now - day * 50, 5, "w0").await;
    usage_rows(&a, &useful_old, "useful", now - day * 45, 1, "w0").await;
    opened_at(&a, &opened_old, "w0", now - day * 45).await;
    usage_rows(&a, &old, "recall_shown", now - day * 40, 12, "w1").await;
    usage_rows(&a, &before_since, "recall_shown", now - day * 5, 12, "w1").await;
    // оценка «полезно» до начала учёта открытий
    usage_rows(&a, &useful_early, "useful", now - day * 5, 1, "w1").await;
    for t in [
        &noisy_one,
        &opened,
        &useful,
        &useful_early,
        &useful_old,
        &opened_old,
    ] {
        usage_rows(&a, t, "recall_shown", now - day, 12, "w1").await;
    }
    opened_at(&a, &opened, "w1", now - day).await;
    usage_rows(&a, &useful, "useful", now - day, 1, "w1").await;
    usage_rows(&a, &few, "recall_shown", now - day, 9, "w1").await;

    let (mut ids, j) = noisy(&a).await;
    ids.sort();
    let mut want = vec![noisy_one.clone(), opened_old.clone()];
    want.sort();
    assert_eq!(ids, want, "{j}");
    assert_eq!(j["opened_since"], ts(since), "{j}");
    let card = |id: &str| {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap()["usage"]
            .clone()
    };
    assert_eq!(
        card(&noisy_one),
        json!({"shown": 17, "opened": 0, "useful": 0, "useless": 0}),
        "{j}"
    );
    // открытие старше окна — в счётчиках записи, но запись «шумная»
    assert_eq!(
        card(&opened_old),
        json!({"shown": 12, "opened": 1, "useful": 0, "useless": 0}),
        "{j}"
    );

    // учёт открытий идёт дольше 30 суток — граница окна «30 суток»
    let long_ago = now - day * 60;
    set_since(&a, long_ago).await;
    let (mut ids, j) = noisy(&a).await;
    ids.sort();
    let mut want = vec![before_since, noisy_one, opened_old];
    want.sort();
    assert_eq!(ids, want, "{j}");
    assert_eq!(j["opened_since"], ts(long_ago), "{j}");

    // `noisy` — фильтр живой ленты insight; значение — флаг
    for (uri, code) in [
        (
            "/api/v1/spaces/knowledge/entries?kind=changelog&noisy=1",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/api/v1/spaces/knowledge/entries?kind=insight&noisy=1&deleted=only",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/api/v1/spaces/knowledge/entries?kind=insight&noisy=maybe",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r = a.t(Method::GET, uri, None).await;
        assert_eq!(r.status, code, "{uri}: {}", r.text());
    }
}
