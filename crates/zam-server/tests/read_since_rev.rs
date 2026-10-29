//! Заметки: чтение изменений `since_rev`, возраст записей и проверенные команды (M4.5 §7,
//! задача 12). Сервер без моделей (поиск — FTS).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;

const READ: &str = "/api/v1/read";
const KN: &str = "/api/v1/spaces/knowledge/entries";
const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9ea7";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

async fn read(a: &Api, items: Value) -> Resp {
    a.t(Method::POST, READ, Some(json!({ "items": items })))
        .await
}

async fn read_one(a: &Api, item: Value) -> Value {
    let r = read(a, json!([item])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"][0].clone()
}

/// Текст из `n` пронумерованных строк.
fn lines(n: usize) -> String {
    let mut t = String::from("# Заметка\n\n");
    for i in 0..n {
        t.push_str(&format!("Строка {i} про сборку проекта.\n"));
    }
    t
}

/// Новая ревизия заметки `nid` (текущая — `rev`).
async fn put(a: &Api, nid: &str, rev: i64, content: &str) {
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{nid}"),
            Some(json!({ "content": content })),
            &[("if-match", &rev.to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn insight(a: &Api, title: &str, body: &str) -> String {
    let r = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "insight", "title": title, "body": body,
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id(&r.json())
}

/// Заметка: дифф с ревизии 1 — унифицированный, контекст 3, только изменённая строка; в md —
/// пометка диффа; отданный целиком дифф отмечает выданное сессии.
#[tokio::test]
async fn read_since_rev_note_diff() {
    let a = api("since-note", |_| {}).await;
    let v1 = lines(30);
    let n = a.note("knowledge", "Сборка", &v1).await;
    let nid = id(&n);
    let v2 = v1.replace("Строка 15 про сборку", "Строка 15 про релизную сборку");
    put(&a, &nid, 1, &v2).await;
    let it = read_one(&a, json!({"id": format!("n:{nid}"), "since_rev": 1})).await;
    assert_eq!(it["rev"], 2, "{it}");
    assert_eq!(it["since_rev"], 1, "{it}");
    assert!(
        it.get("text").is_none() && it.get("content").is_none(),
        "{it}"
    );
    assert!(it.get("diff_skipped").is_none(), "{it}");
    let diff = it["diff"].as_str().unwrap();
    assert!(diff.starts_with("--- rev 1\n+++ rev 2\n"), "{diff}");
    assert!(
        diff.contains("-Строка 15 про сборку проекта.\n+Строка 15 про релизную сборку проекта.\n"),
        "{diff}"
    );
    // контекст 3: соседние строки 12–18, дальние — нет
    assert!(
        diff.contains(" Строка 12 ") && diff.contains(" Строка 18 "),
        "{diff}"
    );
    assert!(
        !diff.contains("Строка 11 ") && !diff.contains("Строка 19 "),
        "{diff}"
    );
    // тот же дифф, что у GET /nodes/{id}/diff
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/nodes/{nid}/diff?from=1&to=2"),
            None,
        )
        .await;
    assert_eq!(r.text(), diff);
    // md: заголовок элемента, пометка и дифф
    let r = a
        .t(
            Method::POST,
            READ,
            Some(
                json!({"items": [{"id": format!("n:{nid}"), "since_rev": 1}],
                        "format": "md", "session_id": "s-since"}),
            ),
        )
        .await;
    let md = r.text();
    assert!(md.contains("(изменения с rev1, дифф)\n--- rev 1\n"), "{md}");
    // выдано сессии: поиск той же ревизии помечает заметку `seen`
    let s = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "релизную сборку", "session_id": "s-since"})),
        )
        .await
        .json();
    assert_eq!(s["items"][0]["seen"], true, "{s}");
}

/// Запись журнала: дифф снимков ревизий — `title`, тело и `data` каноническим JSON.
#[tokio::test]
async fn read_since_rev_entry_diff() {
    let a = api("since-entry", |_| {}).await;
    let body: String = (0..12)
        .map(|i| format!("Абзац {i} о переносе строк.\n"))
        .collect();
    let e = insight(&a, "Скрипты — только LF", &body).await;
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{e}"),
            Some(json!({"title": "Shell-скрипты — только LF",
                        "data": {"kind": "gotcha", "where": "ci"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rev = r.json()["rev"].as_i64().unwrap();
    assert!(rev >= 2, "{}", r.text());
    let it = read_one(&a, json!({"id": format!("i:{e}"), "since_rev": 1})).await;
    assert_eq!(it["rev"], rev, "{it}");
    let diff = it["diff"]
        .as_str()
        .unwrap_or_else(|| panic!("нет diff: {it}"));
    assert!(
        diff.starts_with(&format!("--- rev 1\n+++ rev {rev}\n")),
        "{diff}"
    );
    assert!(
        diff.contains("-title: Скрипты — только LF\n+title: Shell-скрипты — только LF\n"),
        "{diff}"
    );
    assert!(diff.contains("+  \"where\": \"ci\""), "{diff}");
    // тело не менялось: дальние абзацы в контекст не попали
    assert!(!diff.contains("Абзац 8 "), "{diff}");
}

/// Дифф длиннее полного текста — полный текст с `diff_skipped: true`.
#[tokio::test]
async fn read_since_rev_diff_longer_full_text() {
    let a = api("since-long", |_| {}).await;
    let n = a.note("knowledge", "Коротко", "альфа\n").await;
    let nid = id(&n);
    put(&a, &nid, 1, "бета\n").await;
    let it = read_one(&a, json!({"id": nid, "since_rev": 1})).await;
    assert_eq!(it["rev"], 2, "{it}");
    assert_eq!(it["content"], "бета\n", "{it}");
    assert_eq!(it["diff_skipped"], true, "{it}");
    assert!(it.get("diff").is_none() && it.get("text").is_none(), "{it}");
    let r = a
        .t(
            Method::POST,
            READ,
            Some(json!({"items": [{"id": nid, "since_rev": 1}], "format": "md"})),
        )
        .await;
    assert!(
        r.text()
            .contains("(дифф длиннее текста — полный текст)\nбета\n"),
        "{}",
        r.text()
    );
}

/// `since_rev` равна текущей — `unchanged` без текста (заметка и запись).
#[tokio::test]
async fn read_since_rev_current_unchanged() {
    let a = api("since-same", |_| {}).await;
    let nid = id(&a.note("knowledge", "Одна", "текст\n").await);
    let e = insight(&a, "Ловушка", "тело").await;
    let r = read(
        &a,
        json!([
            {"id": format!("n:{nid}"), "since_rev": 1},
            {"id": format!("i:{e}"), "since_rev": 1},
        ]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for it in r.json()["items"].as_array().unwrap() {
        assert_eq!(it["unchanged"], true, "{it}");
        assert_eq!(it["rev"], 1, "{it}");
        for k in ["text", "diff", "content"] {
            assert!(it.get(k).is_none(), "{it}");
        }
    }
}

/// `since_rev` < 1, больше текущей, с `if_rev`, с `tier` кроме L2 или с `section` — 400.
#[tokio::test]
async fn read_since_rev_invalid_400() {
    let a = api("since-bad", |_| {}).await;
    let nid = id(&a.note("knowledge", "Одна", "# A\n\nтекст\n").await);
    let e = insight(&a, "Ловушка", "тело").await;
    let bad = [
        json!({"id": nid, "since_rev": 0}),
        json!({"id": nid, "since_rev": -3}),
        json!({"id": nid, "since_rev": 2}),
        json!({"id": format!("i:{e}"), "since_rev": 5}),
        json!({"id": nid, "since_rev": 1, "if_rev": 1}),
        json!({"id": nid, "since_rev": 1, "tier": "L1"}),
        json!({"id": nid, "since_rev": 1, "section": "A"}),
    ];
    for item in bad {
        let r = read(&a, json!([item])).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{item}: {}", r.text());
    }
}

/// Сдвиг `updated_at` узла или записи в прошлое на `days` дней (прямо в базе).
async fn age_by(a: &Api, table: &'static str, id: &str, days: i64) {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let ts = zam_store::time::fmt_ts(
        jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24 * days),
    );
    let id = id.to_string();
    db.write(move |t| {
        t.execute(
            &format!("UPDATE {table} SET updated_at = ?2 WHERE id = ?1"),
            [&id, &ts],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// `age_days` у элементов поиска и подсказок `/recall`: от `updated_at`, у заметки с
/// `verified_at` — от более позднего; в md поиска — «N дн. назад» / «сегодня».
#[tokio::test]
async fn age_days_in_search_and_hints() {
    let a = api("age-days", roomy).await;
    let e = insight(&a, "Квазарная ловушка", "Квазарный сбой при сборке.").await;
    age_by(&a, "entries", &e, 3).await;
    let today = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
    let checked = id(&a
        .note(
            "knowledge",
            "Квазарная команда",
            &format!("---\nverified_at: {today}\n---\n# Команда\n\nквазарная сборка\n"),
        )
        .await);
    let stale = id(&a
        .note(
            "knowledge",
            "Квазарная старая",
            "# Старая\n\nквазарная сборка\n",
        )
        .await);
    for n in [&checked, &stale] {
        age_by(&a, "nodes", n, 10).await;
    }
    let j = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "квазарная", "top_k": 10})),
        )
        .await
        .json();
    let age = |key: &str, id: &str| -> Value {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i[key] == id)
            .unwrap_or_else(|| panic!("нет {id}: {j}"))["age_days"]
            .clone()
    };
    assert_eq!(age("entry_id", &e), 3);
    assert_eq!(age("node_id", &stale), 10);
    // проверена сегодня: возраст от `verified_at`, а не от давней правки
    assert_eq!(age("node_id", &checked), 0);
    let md = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "квазарная", "top_k": 10, "format": "md"})),
        )
        .await
        .text();
    let line = |needle: &str| {
        md.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("нет {needle}: {md}"))
            .to_string()
    };
    assert!(
        line(&format!("i:{e}")).contains(" · 3 дн. назад · ~"),
        "{md}"
    );
    assert!(
        line(&format!("n:{stale}")).contains(" · 10 дн. назад · ~"),
        "{md}"
    );
    assert!(
        line(&format!("n:{checked}")).contains(" · сегодня · ~"),
        "{md}"
    );
    // подсказки дайджеста: ловушка с возрастом
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "session_start"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let hint = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == format!("i:{e}"))
        .unwrap_or_else(|| panic!("нет подсказки: {j}"))
        .clone();
    assert_eq!(hint["age_days"], 3, "{j}");
}

/// Дайджест: строка «Проверенные команды» — до трёх заметок решения с `verified_at`, новые
/// проверки первыми; заметки без `verified_at` и вне решения не входят.
#[tokio::test]
async fn digest_verified_commands_line() {
    let a = api("verified-cmds", roomy).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Команды"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // без проверенных команд строки нет
    let digest = async || {
        let r = a
            .t(
                Method::POST,
                "/api/v1/recall",
                Some(json!({"event": "session_start", "solution": G})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()["text"].as_str().unwrap().to_string()
    };
    assert!(!digest().await.contains("Проверенные команды"));
    for (path, day) in [
        ("Сборка", "01"),
        ("Тесты", "04"),
        ("Релиз", "03"),
        ("Деплой", "02"),
    ] {
        let text = format!("---\nverified_at: 2026-10-{day}\n---\n# {path}\n\ncargo xtask\n");
        a.note(G, path, &text).await;
    }
    a.note(G, "Черновик", "# Черновик\n\nне проверено\n").await;
    a.note(
        "knowledge",
        "Личная",
        "---\nverified_at: 2026-10-28\n---\n# Личная\n\nвне решения\n",
    )
    .await;
    let text = digest().await;
    let line = text
        .lines()
        .find(|l| l.starts_with("Проверенные команды: "))
        .unwrap_or_else(|| panic!("нет строки: {text}"));
    assert_eq!(
        line,
        "Проверенные команды: Тесты (проверено 2026-10-04); Релиз (проверено 2026-10-03); \
         Деплой (проверено 2026-10-02)"
    );
}
