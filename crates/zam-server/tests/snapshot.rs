//! Снимок сессии на сервере (M6 §8а, задача 17c): `PUT|GET /sessions/{id}/snapshot` (явная
//! сессия, чужое — 404, предел размера, повторная маскировка), срок хранения и удаление с
//! сессией, показ «последнего известного состояния» в дайджесте и пакете восстановления
//! (условие «давно не обновлялась», живость по `last_seen_at`, признаки «работа продолжилась»,
//! квоты частей, выключатель, детерминизм).
//!
//! Все тесты файла замораживают часы (`freeze_now` держит блокировку тестовых точек): часы общие
//! для процесса, и сроки (живость 10 мин, давность 30 мин, срок захвата) задаются ими. Повторный
//! `freeze_now` — только после drop прежнего guard (блок `{ … }` или [`at`]).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_server::config::ServerConfig;
use zam_store::time::freeze_now;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e17";
const RECALL: &str = "/api/v1/recall";
const TITLE: &str = "### Последнее известное состояние";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

/// Момент `2026-11-10` + `hm` (`"08:00"`) UTC.
fn t(hm: &str) -> jiff::Timestamp {
    format!("2026-11-10T{hm}:00Z").parse().unwrap()
}

/// Часы на `hm` до drop результата.
fn at(hm: &str) -> impl Drop {
    freeze_now(t(hm))
}

fn entries() -> String {
    format!("/api/v1/spaces/{G}/entries")
}

fn snap_uri(id: &str) -> String {
    format!("/api/v1/sessions/{}/snapshot", common::api::enc(id))
}

/// Сервер с решением `G` (`tweak` — конфигурация).
async fn world(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let _c = at("07:00");
    let a = api(&format!("snapshot-{name}"), |c| {
        roomy(c);
        tweak(c);
    })
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Снимок"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    a
}

/// Регистрирует сессию (`solution` — `G` или без решения).
async fn register(a: &Api, id: &str, solution: bool, parent: Option<&str>) {
    let mut b = json!({"id": id, "agent": "claude-code"});
    if solution {
        b["solution"] = json!(G);
    }
    if let Some(p) = parent {
        b["parent_id"] = json!(p);
    }
    let r = a.t(Method::POST, "/api/v1/sessions", Some(b)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

/// Тело `PUT` (как `PutBody` клиента 17b).
fn body(taken_at: &str, data: Value) -> Value {
    json!({"taken_at": taken_at, "trigger": "stop", "agent": "claude", "status": "complete",
           "skipped": 0, "data": data})
}

async fn put_as(a: &Api, token: &str, id: &str, header: Option<&str>, b: Value) -> Resp {
    let h: Vec<(&str, &str)> = header.map(|h| ("x-zam-session", h)).into_iter().collect();
    a.tok_h(token, Method::PUT, &snap_uri(id), Some(b), &h)
        .await
}

/// `PUT` снимка сессии `id` с явной сессией, снятого в `hm`; ожидается успех.
async fn put_ok(a: &Api, id: &str, hm: &str, data: Value) {
    let r = put_as(a, &a.token, id, Some(id), body(&t(hm).to_string(), data)).await;
    assert!(r.status.is_success(), "{}: {}", r.status, r.text());
}

async fn session_json(a: &Api, id: &str) -> Value {
    let r = a
        .t(
            Method::GET,
            &format!("/api/v1/sessions/{}", common::api::enc(id)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Задача в решении: создаётся и переводится в `in_progress` сессией `session` (ревизия этой
/// сессии), затем захватывается ею; id задачи.
async fn task_of(a: &Api, session: &str, title: &str) -> String {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &entries(),
            Some(json!({"kind": "task", "title": title,
                        "data": {"next_step": "продолжить"}})),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    patch_task(a, session, &id, json!({"status": "in_progress"})).await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{id}/claim"),
            Some(json!({})),
            &[("x-zam-session", session)],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    id
}

/// Правка записи с `If-Match` текущей ревизии от сессии `session`.
async fn patch_task(a: &Api, session: &str, id: &str, b: Value) {
    let uri = format!("/api/v1/entries/{id}");
    let rev = a.t(Method::GET, &uri, None).await.json()["rev"].to_string();
    let r = a
        .tok_h(
            &a.token,
            Method::PATCH,
            &uri,
            Some(b),
            &[("x-zam-session", session), ("if-match", rev.as_str())],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Дайджест старта сессии `session` в решении `G`.
async fn digest(a: &Api, session: &str) -> String {
    recall(a, session, json!({"event": "session_start", "solution": G})).await
}

async fn recall(a: &Api, session: &str, b: Value) -> String {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(b),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["text"].as_str().unwrap().to_string()
}

/// Раздел снимка в тексте (до пустой строки перед следующим разделом).
fn section(text: &str) -> Option<&str> {
    let start = text.find(TITLE)?;
    let rest = &text[start..];
    Some(rest.split("\n\n").next().unwrap())
}

fn data_files() -> Value {
    json!({"prompts": ["почини сборку"], "files": ["src/lib.rs"],
           "commands": [{"cmd": "cargo build", "failed": true}]})
}

/// Сессия `A` решения с задачей (ревизия и захват в 08:00) и снимком в 09:00; id задачи.
async fn abandoned(a: &Api) -> String {
    let _c = at("08:00");
    register(a, "A", true, None).await;
    let task = task_of(a, "A", "Задача оборванной сессии").await;
    drop(_c);
    let _c = at("09:00");
    put_ok(
        a,
        "A",
        "09:00",
        json!({"compact_summary": "Current Work: правка парсера. КОНСПЕКТ-МАРКЕР",
               "prompts": ["почини парсер"], "files": ["src/parser.rs"],
               "commands": [{"cmd": "cargo test -p parser", "failed": true}]}),
    )
    .await;
    task
}

// ---- запись и права ----

/// §8а: `PUT` только при явной сессии (`X-Zam-Session` = `{id}`), иначе 400 `session-required`;
/// сессии нет — 404 `session-not-found`; `ro` — 403; не JSON — 400; неизвестный повод — 422;
/// записанное читается `GET`, список сессий несёт `snapshot_at`.
#[tokio::test]
async fn put_requires_explicit_session() {
    let a = world("explicit", |_| {}).await;
    let _c = at("08:00");
    register(&a, "s1", true, None).await;
    let b = body("2026-11-10T07:59:00Z", data_files());
    for header in [None, Some("s2"), Some("")] {
        let r = put_as(&a, &a.token, "s1", header, b.clone()).await;
        assert_eq!(
            r.status,
            StatusCode::BAD_REQUEST,
            "{header:?}: {}",
            r.text()
        );
        assert_eq!(
            r.json()["type"],
            "/problems/session-required",
            "{}",
            r.text()
        );
    }
    assert_eq!(
        a.t(Method::GET, &snap_uri("s1"), None).await.status,
        StatusCode::NOT_FOUND
    );
    // сессии нет — явный тип 404 (клиент отличает от старого сервера)
    let r = put_as(&a, &a.token, "nope", Some("nope"), b.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/session-not-found");
    assert_eq!(
        session_json_status(&a, "nope").await,
        StatusCode::NOT_FOUND,
        "PUT не регистрирует сессию"
    );
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = put_as(&a, &ro, "s1", Some("s1"), b.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = a
        .send(
            common::api::req(Method::PUT, &snap_uri("s1"), None)
                .header("authorization", format!("Bearer {}", a.token))
                .header("x-zam-session", "s1")
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{не json"))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let mut bad = b.clone();
    bad["trigger"] = json!("compact");
    let r = put_as(&a, &a.token, "s1", Some("s1"), bad).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // явная сессия — 204; неизвестное поле нового клиента не мешает
    let mut newer = b.clone();
    newer["future_field"] = json!(1);
    let r = put_as(&a, &a.token, "s1", Some("s1"), newer).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let g = a.t(Method::GET, &snap_uri("s1"), None).await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text());
    let g = g.json();
    assert_eq!(g["data"]["files"], json!(["src/lib.rs"]));
    assert_eq!(g["agent"], "claude");
    assert_eq!(g["trigger"], "stop");
    assert!(g["bytes"].as_u64().unwrap() > 0);
    // PUT заменяет снимок целиком
    put_ok(&a, "s1", "08:00", json!({"prompts": ["второй"]})).await;
    let g = a.t(Method::GET, &snap_uri("s1"), None).await.json();
    assert_eq!(g["data"]["files"], json!([]));
    assert_eq!(g["data"]["prompts"], json!(["второй"]));
    let list = a.t(Method::GET, "/api/v1/sessions", None).await.json();
    let s1 = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "s1")
        .unwrap();
    assert!(
        s1["snapshot_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-11-10T08:00:00"),
        "{s1}"
    );
}

async fn session_json_status(a: &Api, id: &str) -> StatusCode {
    a.t(Method::GET, &format!("/api/v1/sessions/{id}"), None)
        .await
        .status
}

/// Передача 17b (I3): `PUT` снимка не освежает `last_seen_at` и не считается `memory_writes`.
#[tokio::test]
async fn put_keeps_last_seen_and_memory_writes() {
    let a = world("untouched", |_| {}).await;
    {
        let _c = at("08:00");
        register(&a, "s1", true, None).await;
    }
    let before = session_json(&a, "s1").await;
    let _c = at("09:00");
    put_ok(&a, "s1", "09:00", data_files()).await;
    let after = session_json(&a, "s1").await;
    assert_eq!(after["last_seen_at"], before["last_seen_at"], "{after}");
    assert_eq!(after["memory_writes"], 0, "{after}");
}

/// §8а: чужой снимок — 404, как несуществующий: другой пользователь и участник общего решения
/// (его сессии — в его базе); запись в чужую сессию — 404, снимок владельца не меняется.
#[tokio::test]
async fn foreign_snapshot_404() {
    let a = world("foreign", |_| {}).await;
    let _c = at("08:00");
    let (_, owner) = a.new_user("Анна").await;
    let (member_id, member) = a.new_user("Борис").await;
    let (_, stranger) = a.new_user("Вера").await;
    let reg = |tok: String| {
        let a = &a;
        async move {
            a.tok(
                &tok,
                Method::POST,
                "/api/v1/solutions",
                Some(
                    json!({"agent_solution_id": "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e18",
                            "name": "Общее"}),
                ),
            )
            .await
            .status
        }
    };
    assert_eq!(reg(owner.clone()).await, StatusCode::CREATED);
    assert_eq!(reg(member.clone()).await, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &owner,
            Method::POST,
            &format!(
                "/api/v1/solutions/0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e18/access/{member_id}/approve"
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .tok(
            &owner,
            Method::POST,
            "/api/v1/sessions",
            Some(json!({"id": "own-s", "solution": "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e18"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = put_as(
        &a,
        &owner,
        "own-s",
        Some("own-s"),
        body("2026-11-10T07:00:00Z", data_files()),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        a.tok(&owner, Method::GET, &snap_uri("own-s"), None)
            .await
            .status,
        StatusCode::OK
    );
    for tok in [&member, &stranger, &a.token] {
        let r = a.tok(tok, Method::GET, &snap_uri("own-s"), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
        let r = put_as(
            &a,
            tok,
            "own-s",
            Some("own-s"),
            body("2026-11-10T07:30:00Z", json!({"prompts": ["чужое"]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    }
    let g = a
        .tok(&owner, Method::GET, &snap_uri("own-s"), None)
        .await
        .json();
    assert_eq!(g["data"]["files"], json!(["src/lib.rs"]), "{g}");
}

/// §8а: всё тело больше `snapshot.max_bytes` — 413; в пределе — записывается.
#[tokio::test]
async fn put_413_over_limit() {
    let a = world("limit", |c| c.snapshot.max_bytes = 2048).await;
    let _c = at("08:00");
    register(&a, "s1", true, None).await;
    let fit = body(
        "2026-11-10T07:00:00Z",
        json!({"prompts": ["я".repeat(800)]}),
    );
    assert!(fit.to_string().len() <= 2048);
    let r = put_as(&a, &a.token, "s1", Some("s1"), fit).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    for n in [1100, 20_000] {
        let big = body("2026-11-10T07:00:00Z", json!({"prompts": ["я".repeat(n)]}));
        let r = put_as(&a, &a.token, "s1", Some("s1"), big).await;
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{n}: {}", r.text());
    }
    let g = a.t(Method::GET, &snap_uri("s1"), None).await.json();
    assert_eq!(
        g["data"]["prompts"][0].as_str().unwrap().chars().count(),
        800
    );
}

/// §8а: сервер повторяет скан секретов каждого поля, маскирует сам и отвечает
/// `warnings: ["secrets_masked"]`; в базе (файлах каталога данных) приманки нет.
#[tokio::test]
async fn server_remasks_secrets() {
    let a = world("remask", |_| {}).await;
    let _c = at("08:00");
    register(&a, "s1", true, None).await;
    // приманка собирается конкатенацией (push protection)
    let key = format!("{}{}", "AKIA", "QWERTYUIOPASDFGH");
    let data = json!({"compact_summary": format!("ключ {key} в конспекте"),
                      "prompts": [format!("используй {key}")],
                      "files": ["src/a.rs"],
                      "commands": [{"cmd": format!("aws configure set key {key}")}]});
    let r = put_as(
        &a,
        &a.token,
        "s1",
        Some("s1"),
        body("2026-11-10T07:00:00Z", data),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["warnings"], json!(["secrets_masked"]));
    let g = a.t(Method::GET, &snap_uri("s1"), None).await;
    let text = g.text();
    assert!(!text.contains(&key), "{text}");
    assert_eq!(text.matches("‹secret›").count(), 3, "{text}");
    let leaked = common::files_containing(&a.st.cfg.data_dir, key.as_bytes());
    assert!(leaked.is_empty(), "{leaked:?}");
    // без находок — 204 без тела
    let r = put_as(
        &a,
        &a.token,
        "s1",
        Some("s1"),
        body("2026-11-10T07:00:00Z", data_files()),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

/// §8а: снимок старше `snapshot.keep_days` (от `taken_at`) удаляет обслуживание;
/// `gc_sessions` удаляет снимок вместе с сессией.
#[tokio::test]
async fn gc_sessions_and_keep_days_remove() {
    let a = world("gc", |c| c.snapshot.keep_days = 3).await;
    {
        let _c = at("08:00");
        register(&a, "old", true, None).await;
        register(&a, "new", true, None).await;
        put_ok(&a, "old", "08:00", data_files()).await;
    }
    let _c = at("09:00");
    put_ok(&a, "new", "09:00", data_files()).await;
    let day = jiff::SignedDuration::from_hours(24);
    let sec = jiff::SignedDuration::from_secs(1);
    let gc = |now: jiff::Timestamp| zam_server::maintenance::gc_snapshots(&a.st, now);
    assert_eq!(gc(t("08:00") + day * 3 - sec).await, 0, "моложе keep_days");
    assert_eq!(gc(t("08:30") + day * 3).await, 1);
    assert_eq!(
        a.t(Method::GET, &snap_uri("old"), None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        a.t(Method::GET, &snap_uri("new"), None).await.status,
        StatusCode::OK
    );
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let n = zam_server::maintenance::gc_sessions(&a.st, t("09:00") + day * 31).await;
    assert!(n >= 2, "{n}");
    assert!(zam_store::get_snapshot(&db, "new").await.unwrap().is_none());
    assert_eq!(
        a.t(Method::GET, &snap_uri("new"), None).await.status,
        StatusCode::NOT_FOUND
    );
}

// ---- показ ----

/// §8а: дайджест другой сессии показывает снимок оборванной сессии, если её задача давно не
/// обновлялась (строка `задача k:<id> не обновлялась с …`, конспект, запросы, файлы, команды);
/// задача обновлена недавно — раздела нет; задачи нет — раздел только при изменённых файлах.
#[tokio::test]
async fn digest_shows_last_state_when_task_stale() {
    let a = world("stale", |_| {}).await;
    let task = abandoned(&a).await;
    let text = {
        let _c = at("10:00");
        digest(&a, "B").await
    };
    let sec = section(&text).unwrap_or_else(|| panic!("нет раздела: {text}"));
    assert!(
        sec.starts_with(
            "### Последнее известное состояние (снимок 2026-11-10 09:00:00, извлечено автоматически)"
        ),
        "{sec}"
    );
    assert!(
        sec.contains(&format!(
            "задача k:{task} не обновлялась с 2026-11-10 08:00:00"
        )),
        "{sec}"
    );
    for want in [
        "КОНСПЕКТ-МАРКЕР",
        "«почини парсер»",
        "Файлы: src/parser.rs",
        "`cargo test -p parser` (ошибка)",
    ] {
        assert!(sec.contains(want), "{want}: {sec}");
    }
    let order: Vec<usize> = ["задача k:", "Конспект", "Запросы:", "Файлы:", "Команды:"]
        .iter()
        .map(|w| sec.find(w).unwrap())
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{sec}");
    // своя сессия своего снимка в дайджесте не видит (другие сессии)
    {
        let _c = at("10:01");
        assert!(section(&digest(&a, "A").await).is_none());
    }
    // задача обновлена за 10 минут до снимка (меньше snapshot_stale_min) — раздела нет
    {
        let _c = at("10:05");
        patch_task(&a, "A", &task, json!({"data": {"next_step": "свежий шаг"}})).await;
    }
    {
        let _c = at("10:15");
        put_ok(&a, "A", "10:15", data_files()).await;
    }
    let text = {
        let _c = at("11:00");
        digest(&a, "B2").await
    };
    assert!(section(&text).is_none(), "{text}");
    // задачи нет: с файлами — раздел без строки задачи, без файлов — нет
    {
        let _c = at("11:10");
        patch_task(&a, "A", &task, json!({"status": "done"})).await;
    }
    {
        let _c = at("11:20");
        put_ok(&a, "A", "11:20", data_files()).await;
    }
    let text = {
        let _c = at("12:00");
        digest(&a, "B3").await
    };
    let sec = section(&text).unwrap_or_else(|| panic!("нет раздела: {text}"));
    assert!(
        !sec.contains("задача k:") && sec.contains("Файлы: src/lib.rs"),
        "{sec}"
    );
    {
        let _c = at("12:10");
        put_ok(&a, "A", "12:10", json!({"prompts": ["только запрос"]})).await;
    }
    let text = {
        let _c = at("13:00");
        digest(&a, "B4").await
    };
    assert!(section(&text).is_none(), "{text}");
}

/// §8а: живая сессия (нет `ended_at`, `last_seen_at` моложе 10 мин) — снимок не показывается;
/// решает `last_seen_at` сервера: закончилась (`ended`) или молчит 10 мин — показывается.
#[tokio::test]
async fn digest_hides_live_session_by_last_seen() {
    let a = world("live", |_| {}).await;
    abandoned(&a).await;
    // обращение A в 09:55 (касание сессии), дайджест B в 10:00 — A жива
    {
        let _c = at("09:55");
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": "парсер", "mode": "brief"})),
                &[("x-zam-session", "A")],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    {
        let _c = at("10:00");
        assert!(section(&digest(&a, "B").await).is_none());
    }
    {
        let _c = at("10:06");
        assert!(
            section(&digest(&a, "B").await).is_some(),
            "молчит больше 10 мин"
        );
    }
    // снова обращение и конец сессии: ended — показывается сразу
    {
        let _c = at("10:20");
        let r = a
            .t(
                Method::PATCH,
                "/api/v1/sessions/A",
                Some(json!({"ended": true})),
            )
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": "парсер", "mode": "brief"})),
                &[("x-zam-session", "A")],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    {
        let _c = at("10:21");
        assert!(
            section(&digest(&a, "B").await).is_some(),
            "ended_at — не живая"
        );
    }
}

/// §8а: работа после снимка продолжилась — (а) другая сессия решения писала в память позже
/// снимка (субагент сессии снимка — не «другая»), (б) ревизия задачи снимка другой сессией,
/// (в) действующий чужой захват задачи. Сессия без решения для (б) и (в) — чтобы (а) не
/// сработал.
#[tokio::test]
async fn digest_hides_when_work_continued() {
    // (а)
    let a = world("continued-a", |_| {}).await;
    abandoned(&a).await;
    {
        let _c = at("09:30");
        register(&a, "A:sub", true, Some("A")).await;
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                "/api/v1/nodes",
                Some(
                    json!({"space": G, "path": "заметка-субагента", "kind": "note",
                            "content": "# С\n\nтекст"}),
                ),
                &[("x-zam-session", "A:sub")],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    {
        let _c = at("10:00");
        assert!(
            section(&digest(&a, "B").await).is_some(),
            "запись субагента сессии снимка — не «другая сессия»"
        );
    }
    {
        let _c = at("10:10");
        register(&a, "C", true, None).await;
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": G, "path": "заметка-C", "kind": "note",
                            "content": "# C\n\nтекст"})),
                &[("x-zam-session", "C")],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    {
        let _c = at("10:20");
        assert!(
            section(&digest(&a, "B").await).is_none(),
            "(а) запись другой сессии"
        );
    }

    // (б)
    let a = world("continued-b", |_| {}).await;
    let task = abandoned(&a).await;
    {
        let _c = at("10:00");
        register(&a, "C", false, None).await;
        assert!(section(&digest(&a, "B").await).is_some());
        patch_task(
            &a,
            "C",
            &task,
            json!({"data": {"next_step": "C продолжает"}}),
        )
        .await;
        let s = session_json(&a, "C").await;
        assert_eq!(s["solution"], Value::Null, "{s}");
    }
    {
        let _c = at("10:10");
        assert!(
            section(&digest(&a, "B").await).is_none(),
            "(б) ревизия другой сессии"
        );
    }

    // (в): захват A истёк (120 мин с 08:00), C захватывает
    let a = world("continued-c", |_| {}).await;
    let task = abandoned(&a).await;
    {
        let _c = at("10:01");
        register(&a, "C", false, None).await;
        assert!(section(&digest(&a, "B").await).is_some());
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                &format!("/api/v1/entries/{task}/claim"),
                Some(json!({})),
                &[("x-zam-session", "C")],
            )
            .await;
        assert!(r.status.is_success(), "{}", r.text());
    }
    {
        let _c = at("10:05");
        assert!(
            section(&digest(&a, "B").await).is_none(),
            "(в) чужой захват"
        );
    }
}

/// §8а: пакет восстановления (`compact`/`resume` той же сессии) — раздел при том же условии, но
/// без конспекта; дайджест другой сессии — с конспектом.
#[tokio::test]
async fn recovery_without_summary() {
    let a = world("recovery", |_| {}).await;
    abandoned(&a).await;
    let text = {
        let _c = at("09:01");
        recall(
            &a,
            "A",
            json!({"event": "session_start", "source": "compact", "solution": G}),
        )
        .await
    };
    let sec = section(&text).unwrap_or_else(|| panic!("нет раздела: {text}"));
    assert!(sec.contains("Файлы: src/parser.rs"), "{sec}");
    assert!(
        !sec.contains("КОНСПЕКТ-МАРКЕР") && !sec.contains("Конспект"),
        "{sec}"
    );
    let text = {
        let _c = at("09:02");
        recall(&a, "A", json!({"event": "recovery", "solution": G})).await
    };
    let sec = section(&text).unwrap_or_else(|| panic!("нет раздела: {text}"));
    assert!(!sec.contains("КОНСПЕКТ-МАРКЕР"), "{sec}");
    // задачи — раньше снимка
    assert!(
        text.find("### Задачи").unwrap() < text.find(TITLE).unwrap(),
        "{text}"
    );
    let text = {
        let _c = at("10:00");
        digest(&a, "B").await
    };
    assert!(section(&text).unwrap().contains("КОНСПЕКТ-МАРКЕР"));
}

/// §8а: квоты частей — файлы ≤ 80, команды ≤ 80, запросы ≤ 120 токенов, весь раздел ≤
/// `snapshot_tokens`; остаются последние элементы, конспект — хвост. Задачи при малом бюджете
/// дайджеста снимок не вытесняет.
#[tokio::test]
async fn snapshot_quota_parts() {
    let a = world("quota", |_| {}).await;
    {
        let _c = at("08:00");
        register(&a, "A", true, None).await;
        task_of(&a, "A", "Задача квот").await;
    }
    {
        let _c = at("09:00");
        let files: Vec<String> = (0..20)
            .map(|i| format!("crates/module_{i}/src/very/long/path/file_{i}.rs"))
            .collect();
        let commands: Vec<Value> = (0..10)
            .map(|i| json!({"cmd": format!("cargo test -p crate_{i} --test suite_{i} -- --nocapture filter_{i}")}))
            .collect();
        let prompts: Vec<String> = (0..5)
            .map(|i| format!("запрос номер {i}: {}", "длинный текст запроса ".repeat(10)))
            .collect();
        let summary = format!(
            "НАЧАЛО-КОНСПЕКТА {} ХВОСТ-КОНСПЕКТА",
            "середина конспекта ".repeat(150)
        );
        put_ok(
            &a,
            "A",
            "09:00",
            json!({"compact_summary": summary, "prompts": prompts, "files": files,
                   "commands": commands}),
        )
        .await;
    }
    let _c = at("10:00");
    let text = digest(&a, "B").await;
    let sec = section(&text).unwrap_or_else(|| panic!("нет раздела: {text}"));
    let line = |p: &str| {
        sec.lines()
            .find(|l| l.starts_with(p))
            .unwrap_or_else(|| panic!("нет «{p}»: {sec}"))
    };
    let tok = |s: &str| zam_store::count_tokens(&format!("{s}\n"));
    assert!(tok(line("Файлы:")) <= 80, "{}", line("Файлы:"));
    assert!(tok(line("Команды:")) <= 80, "{}", line("Команды:"));
    assert!(tok(line("Запросы:")) <= 120, "{}", line("Запросы:"));
    assert!(line("Файлы:").contains("file_19.rs") && !line("Файлы:").contains("file_0.rs"));
    assert!(line("Команды:").contains("filter_9"));
    assert!(line("Запросы:").contains("запрос номер 4"));
    assert!(
        sec.contains("ХВОСТ-КОНСПЕКТА") && !sec.contains("НАЧАЛО-КОНСПЕКТА"),
        "{sec}"
    );
    assert!(
        zam_store::count_tokens(&format!("\n{sec}\n")) <= 400 + 2,
        "{sec}"
    );
    // малый бюджет: задачи остаются, снимок — в остатке
    let small = recall(
        &a,
        "B5",
        json!({"event": "session_start", "solution": G, "budget_tokens": 120}),
    )
    .await;
    assert!(small.contains("Задача квот"), "{small}");
}

/// §8а: `recall.snapshot_tokens = 0` — раздела нет, снимок хранится.
#[tokio::test]
async fn snapshot_tokens_zero_hides() {
    let a = world("zero", |c| c.recall.snapshot_tokens = 0).await;
    abandoned(&a).await;
    let _c = at("10:00");
    assert!(section(&digest(&a, "B").await).is_none());
    let text = recall(&a, "A", json!({"event": "recovery", "solution": G})).await;
    assert!(section(&text).is_none(), "{text}");
    assert_eq!(
        a.t(Method::GET, &snap_uri("A"), None).await.status,
        StatusCode::OK
    );
}

/// §8, §8а: дайджест со снимком детерминирован — два старта разных сессий в те же сутки UTC
/// дают побайтово одинаковый текст.
#[tokio::test]
async fn digest_with_snapshot_deterministic() {
    let a = world("det", |_| {}).await;
    abandoned(&a).await;
    let first = {
        let _c = at("10:00");
        digest(&a, "det-1").await
    };
    let second = {
        let _c = at("21:30");
        digest(&a, "det-2").await
    };
    assert!(section(&first).is_some(), "{first}");
    assert_eq!(first, second, "{first}\n---\n{second}");
}

/// `guide` (ru, en): раздел о снимке — ключи клиента и сервера, события, остаточный риск.
#[tokio::test]
async fn guide_has_snapshot_section() {
    let a = world("guide", |_| {}).await;
    for (q, title, risk) in [
        ("", "Снимок сессии", "не диктуйте"),
        ("?lang=en", "Session snapshot", "do not dictate"),
    ] {
        let g = a
            .t(Method::GET, &format!("/api/v1/agent-guide{q}"), None)
            .await
            .text();
        let sec = g
            .split(&format!("\n## {title}\n"))
            .nth(1)
            .unwrap_or_else(|| panic!("нет раздела «{title}»"));
        let sec = sec.split("\n## ").next().unwrap();
        for want in [
            "hooks.session_snapshot",
            "snapshot_tail_kb",
            "snapshot_ms",
            "PostCompact",
            "SessionEnd",
            "recall.snapshot_tokens",
            "zam session snapshot",
            risk,
        ] {
            assert!(sec.contains(want), "{q}: {want}: {sec}");
        }
    }
}
