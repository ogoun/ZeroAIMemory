//! Insights через REST (задача 8 M3, §7.5, §9.1–9.2, §10.3): дедупликация (заголовок и
//! вектор), `force`, дописывание, замещение, отзыв, `revisit`, переходы статусов, `scope:
//! global`, триггеры (проверка, `GET /triggers` с `ETag`, изоляция участника), строгий фильтр
//! области в поиске.
//!
//! Пользователи: A — владелец решения, B — участник. Серверы без моделей (FTS,
//! `dedup: degraded`); векторная дедупликация — MiniLM на CPU-сборке ORT и настоящий Qdrant,
//! без них тест пропускается (в CI, `ZAM_REQUIRE_ML=1`, — падает).
mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::config::ServerConfig;
use zam_store::{IndexState, SpaceKind, get_entry, list_spaces};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e21";
/// Предел ожидания индексации (инференс на CPU, запись в Qdrant на HDD).
const INDEXED: Duration = Duration::from_secs(240);

struct W {
    a: Api,
    a_id: String,
    a_tok: String,
    b_id: String,
    b_tok: String,
}

/// Владелец A регистрирует решение `G`; при `member` участник B получает доступ `rw`.
async fn world(name: &str, member: bool) -> W {
    world_with(name, member, |_| {}).await
}

async fn world_with(name: &str, member: bool, tweak: impl FnOnce(&mut ServerConfig)) -> W {
    let a = api(&format!("insights-{name}"), tweak).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let w = W {
        a,
        a_id,
        a_tok,
        b_id,
        b_tok,
    };
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(reg.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    if member {
        let r =
            w.a.tok(&w.b_tok, Method::POST, "/api/v1/solutions", Some(reg))
                .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
        let r =
            w.a.tok(
                &w.a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{}/approve", w.b_id),
                Some(json!({})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    w
}

fn insight(title: &str, body: &str) -> Value {
    json!({"kind": "insight", "title": title, "body": body, "data": {"kind": "gotcha"}})
}

fn with(mut v: Value, k: &str, x: Value) -> Value {
    v[k] = x;
    v
}

fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

impl W {
    /// `POST /spaces/{space}/entries` токеном `tok` с заголовками.
    async fn post_in(&self, tok: &str, space: &str, body: Value, extra: &[(&str, &str)]) -> Resp {
        self.a
            .tok_h(
                tok,
                Method::POST,
                &format!("/api/v1/spaces/{space}/entries"),
                Some(body),
                extra,
            )
            .await
    }

    async fn post(&self, body: Value) -> Resp {
        self.post_in(&self.a_tok, G, body, &[]).await
    }

    /// Создаёт insight владельцем в решении: 201, запись.
    async fn created(&self, body: Value) -> Value {
        let r = self.post(body).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()
    }

    async fn req(&self, tok: &str, m: Method, uri: &str, body: Option<Value>) -> Resp {
        self.a.tok(tok, m, uri, body).await
    }

    async fn patch(&self, id: &str, body: Value) -> Resp {
        self.req(
            &self.a_tok,
            Method::PATCH,
            &format!("/api/v1/entries/{id}"),
            Some(body),
        )
        .await
    }

    async fn entry(&self, id: &str) -> Value {
        let r = self
            .req(
                &self.a_tok,
                Method::GET,
                &format!("/api/v1/entries/{id}"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()
    }

    async fn search(&self, body: Value) -> Value {
        let r = self
            .req(&self.a_tok, Method::POST, "/api/v1/search", Some(body))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()
    }

    async fn triggers(&self, tok: &str, q: &str, inm: Option<&str>) -> Resp {
        let extra: Vec<(&str, &str)> = inm.map(|v| ("if-none-match", v)).into_iter().collect();
        self.a
            .tok_h(
                tok,
                Method::GET,
                &format!("/api/v1/triggers{q}"),
                None,
                &extra,
            )
            .await
    }
}

/// `entry_id` элементов выдачи поиска.
fn hit_ids(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"))
        .iter()
        .filter_map(|i| i["entry_id"].as_str().map(str::to_string))
        .collect()
}

/// Шаблоны триггеров ответа `GET /triggers` по порядку.
fn patterns(j: &Value) -> Vec<String> {
    j["triggers"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"))
        .iter()
        .map(|t| t["pattern"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn dedup_title_409_with_candidates() {
    let w = world("dedup-title", false).await;
    let first = w
        .created(insight(
            "Bash-скрипты должны иметь LF",
            "CRLF ломает shebang.",
        ))
        .await;
    assert_eq!(first["dedup"], "degraded");
    assert_eq!(first["status"], "active");
    assert_eq!(first["triggers"], json!([]));
    // тот же нормализованный заголовок — 409 с кандидатами
    let r = w
        .post(insight("  bash скрипты должны иметь LF!  ", "другое тело"))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let p = r.json();
    assert_eq!(p["type"], "/problems/possible-duplicate");
    assert_eq!(p["dedup"], "degraded");
    let c = p["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["id"], first["id"]);
    assert_eq!(c[0]["reason"], "title");
    assert_eq!(c[0]["status"], "active");
    assert!(c[0]["score"].is_null());
    // другая область — не дубль
    let r = w
        .post_in(
            &w.a_tok,
            G,
            insight("Bash-скрипты должны иметь LF", "для api"),
            &[("x-zam-area", "api")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["area"], "api");
    // повтор по Idempotency-Key дедупликацию не проходит: та же запись, 200
    let body = insight("Уникальный заголовок", "x");
    let r = w
        .post_in(&w.a_tok, G, body.clone(), &[("idempotency-key", "k1")])
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r2 = w
        .post_in(&w.a_tok, G, body, &[("idempotency-key", "k1")])
        .await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.json()["id"], r.json()["id"]);
    assert_eq!(r2.json()["dedup"], "skipped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dedup_vector_409() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("ins-vec");
    let a = api("ins-vec", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let body = "A shell script saved with Windows CRLF line endings fails on Linux with \
                bad interpreter: /bin/bash^M. Convert the file to LF before running it.";
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(insight("Shell scripts need LF line endings", body)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let first = r.json();
    assert_eq!(first["dedup"], "full");
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let fid: Id = id_of(&first).parse().unwrap();
    a.st.index.worker.notify();
    until(
        "insight проиндексирован",
        INDEXED,
        async || get_entry(&db, fid).await.unwrap().index_state == IndexState::Indexed,
    )
    .await;
    // другой заголовок (без совпадения нормализованного), тот же смысл — дубль по вектору
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(insight("Shell scripts require LF line endings", body)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let p = r.json();
    assert_eq!(p["type"], "/problems/possible-duplicate");
    assert_eq!(p["dedup"], "full");
    let c = &p["candidates"][0];
    assert_eq!(c["id"], first["id"]);
    assert_eq!(c["reason"], "vector");
    assert!(c["score"].as_f64().unwrap() >= 0.90, "{c}");
    // другой смысл — запись проходит с полной проверкой
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(insight(
                "Qdrant collection needs payload indexes",
                "Filtering by user_id without a payload index scans every point.",
            )),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["dedup"], "full");
    // замещённая запись — уже не кандидат (фильтр статусов и проверка по SQLite)
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id_of(&first)),
            Some(json!({"reason": "переписано"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(insight("Shell scripts require LF line endings", body)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn dedup_degraded_without_model() {
    let w = world("dedup-degraded", false).await;
    assert!(w.a.st.index.vector_available().is_err());
    let e = w
        .created(insight("Без модели проверка только по заголовку", "x"))
        .await;
    assert_eq!(e["dedup"], "degraded");
    // похожий по смыслу, но с другим заголовком — без модели не ловится
    let e = w
        .created(insight("Без модели дедупликация лишь по заголовку", "x"))
        .await;
    assert_eq!(e["dedup"], "degraded");
}

#[tokio::test]
async fn force_writes_duplicate() {
    let w = world("force", false).await;
    let a = w.created(insight("Дубль", "1")).await;
    let r = w.post(insight("дубль", "2")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let b = w
        .created(with(insight("дубль", "2"), "force", json!(true)))
        .await;
    assert_eq!(b["dedup"], "skipped");
    assert_ne!(a["id"], b["id"]);
    let r = w
        .req(
            &w.a_tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/entries?kind=insight"),
            None,
        )
        .await;
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn append_body_to_existing() {
    let w = world("append", false).await;
    let a = w
        .created(insight("Дописать сюда", "Первое наблюдение."))
        .await;
    let r = w.post(insight("Дописать сюда", "Второе")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let cand = id_of(&r.json()["candidates"][0]);
    assert_eq!(cand, id_of(&a));
    let r = w
        .patch(&cand, json!({"append_body": "Второе наблюдение."}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let e = r.json();
    assert_eq!(e["body"], "Первое наблюдение.\n\nВторое наблюдение.");
    assert_eq!(e["rev"], 2);
}

#[tokio::test]
async fn supersede_old_superseded_new_linked_history_kept() {
    let w = world("supersede", false).await;
    let old = w
        .created(insight("Метод X нерабочий", "Не сработал на данных Y."))
        .await;
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&old)),
            Some(json!({"title": "Метод X работает с параметром Z", "body": "Параметры были неверны.",
                        "data": {"kind": "discovery"},
                        "triggers": [{"type": "keyword", "pattern": "метод x"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    let (n, o) = (&j["entry"], &j["superseded"]);
    assert_eq!(n["supersedes_id"], old["id"]);
    assert_eq!(n["status"], "active");
    assert_eq!(n["triggers"][0]["pattern"], "метод x");
    assert_eq!(o["status"], "superseded");
    assert_eq!(o["superseded_by_id"], n["id"]);
    // ничего не удалено: старая читается, история дописана
    let got = w.entry(&id_of(&old)).await;
    assert_eq!(got["status"], "superseded");
    let r = w
        .req(
            &w.a_tok,
            Method::GET,
            &format!("/api/v1/entries/{}/revisions", id_of(&old)),
            None,
        )
        .await;
    let revs: Vec<Value> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].clone())
        .collect();
    assert_eq!(revs, vec![json!("active"), json!("superseded")]);
    // повторное замещение — 409
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&old)),
            Some(json!({"title": "ещё", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // поиск по умолчанию — только действующая; с историей — обе
    let q = json!({"query": "Метод", "scope": {"solution": G}, "filters": {"kinds": ["insight"]}});
    assert_eq!(hit_ids(&w.search(q.clone()).await), vec![id_of(n)]);
    let all = hit_ids(&w.search(with(q, "include_history", json!(true))).await);
    assert!(
        all.contains(&id_of(&old)) && all.contains(&id_of(n)),
        "{all:?}"
    );
    // замещение при создании (`supersedes_id`): дубль заголовка не мешает
    let r = w
        .post(with(
            insight("Метод X работает с параметром Z", "Уточнение."),
            "supersedes_id",
            n["id"].clone(),
        ))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert_eq!(j["supersedes_id"], n["id"]);
    assert_eq!(j["superseded"]["status"], "superseded");
    assert_eq!(j["dedup"], "skipped");
    // замещать можно только insight того же пространства
    let r = w
        .post_in(
            &w.a_tok,
            "knowledge",
            with(insight("x", ""), "supersedes_id", j["id"].clone()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn retract_requires_reason_422() {
    let w = world("retract", false).await;
    let a = w.created(insight("Вывод А", "")).await;
    let uri = format!("/api/v1/entries/{}/retract", id_of(&a));
    for body in [json!({}), json!({"reason": "   "})] {
        let r = w.req(&w.a_tok, Method::POST, &uri, Some(body)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    }
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &uri,
            Some(json!({"reason": "опровергнуто замером"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let e = r.json();
    assert_eq!(e["status"], "retracted");
    assert_eq!(e["retract_reason"], "опровергнуто замером");
    // повторно — 409
    let r = w
        .req(&w.a_tok, Method::POST, &uri, Some(json!({"reason": "ещё"})))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // отзыв замещённой разрешён: старый вывод тоже может оказаться ошибочным
    let b = w.created(insight("Вывод Б", "")).await;
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&b)),
            Some(json!({"title": "Вывод Б2", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id_of(&b)),
            Some(json!({"reason": "и он был неверен"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["superseded_by_id"].is_string());
    // changelog не отзывается
    let r = w
        .post(json!({"kind": "changelog", "title": "ch", "data": {"kind": "fix"}}))
        .await;
    let ch = r.json();
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id_of(&ch)),
            Some(json!({"reason": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn revisit_listed_and_searchable() {
    let w = world("revisit", false).await;
    let a = w
        .created(insight("Пересмотреть кэш выжимок", "Идея на потом."))
        .await;
    w.created(insight("Другой инсайт про кэш", "Действующий."))
        .await;
    let r = w
        .patch(
            &id_of(&a),
            json!({"status": "revisit", "reason": "новые данные"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "revisit");
    let r = w
        .req(
            &w.a_tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/entries?kind=insight&status=revisit"),
            None,
        )
        .await;
    let items = r.json()["items"].clone();
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["id"], a["id"]);
    // поиск по умолчанию выдаёт и revisit
    let j = w
        .search(json!({"query": "Пересмотреть кэш", "scope": {"solution": G}}))
        .await;
    let item = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["entry_id"] == a["id"])
        .unwrap_or_else(|| panic!("{j}"));
    assert_eq!(item["status"], "revisit");
    // создание сразу в revisit и возврат в active
    let b = w
        .created(with(
            insight("Сразу пересмотреть", ""),
            "status",
            json!("revisit"),
        ))
        .await;
    assert_eq!(b["status"], "revisit");
    let r = w.patch(&id_of(&a), json!({"status": "active"})).await;
    assert_eq!(r.json()["status"], "active");
}

#[tokio::test]
async fn invalid_status_transition_422() {
    let w = world("transitions", false).await;
    let a = w.created(insight("Переходы", "")).await;
    for s in ["superseded", "retracted"] {
        let r = w.patch(&id_of(&a), json!({"status": s})).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{s}: {}",
            r.text()
        );
        // и при создании
        let r = w
            .post(with(insight(&format!("новая {s}"), ""), "status", json!(s)))
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{s}: {}",
            r.text()
        );
    }
    // из замещения и отзыва не возвращаются
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&a)),
            Some(json!({"title": "Переходы 2", "data": {"kind": "gotcha"}})),
        )
        .await;
    let n = r.json()["entry"].clone();
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id_of(&n)),
            Some(json!({"reason": "нет"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    for (id, s) in [
        (id_of(&a), "active"),
        (id_of(&a), "revisit"),
        (id_of(&n), "active"),
    ] {
        let r = w.patch(&id, json!({"status": s})).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{s}: {}",
            r.text()
        );
    }
    assert_eq!(w.entry(&id_of(&a)).await["status"], "superseded");
    // статус есть только у insight
    let ch = w
        .post(json!({"kind": "changelog", "title": "ch", "data": {"kind": "fix"}}))
        .await
        .json();
    let r = w.patch(&id_of(&ch), json!({"status": "revisit"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn global_scope_goes_to_knowledge() {
    let w = world("global", true).await;
    let a_db = w.a.st.users.get(w.a_id.parse().unwrap()).await.unwrap();
    let knowledge = |spaces: Vec<zam_store::Space>| {
        spaces
            .into_iter()
            .find(|s| s.kind == SpaceKind::Knowledge)
            .unwrap()
            .id
            .to_string()
    };
    let a_k = knowledge(list_spaces(&a_db).await.unwrap());
    let e = w
        .created(with(
            insight("Глобальный вывод", ""),
            "scope",
            json!("global"),
        ))
        .await;
    assert_eq!(e["space_id"], json!(a_k));
    // по пути knowledge без scope — тоже Knowledge
    let r = w
        .post_in(&w.a_tok, "knowledge", insight("Ещё глобальный", ""), &[])
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["space_id"], json!(a_k));
    // scope: solution в Knowledge и неизвестный scope — 422; Rules — 403, Human — 404
    let r = w
        .post_in(
            &w.a_tok,
            "knowledge",
            with(insight("x", ""), "scope", json!("solution")),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = w.post(with(insight("x", ""), "scope", json!("team"))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = w.post_in(&w.a_tok, "rules", insight("x", ""), &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = w.post_in(&w.a_tok, "human", insight("x", ""), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // участник с scope: global пишет в свой Knowledge, а не во владельца
    let r = w
        .post_in(
            &w.b_tok,
            G,
            with(insight("Вывод Бориса", ""), "scope", json!("global")),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let b_db = w.a.st.users.get(w.b_id.parse().unwrap()).await.unwrap();
    assert_eq!(
        r.json()["space_id"],
        json!(knowledge(list_spaces(&b_db).await.unwrap()))
    );
    let bid: Id = id_of(&r.json()).parse().unwrap();
    assert!(get_entry(&b_db, bid).await.is_ok());
    assert!(get_entry(&a_db, bid).await.is_err());
    // Knowledge владельца участнику не существует: замещение по его id — 404, а не 422
    let r = w
        .post_in(
            &w.b_tok,
            G,
            with(insight("Подмена", ""), "supersedes_id", e["id"].clone()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // отзыв доступа: путь решения проверяется и при scope: global — 403
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/revoke", w.b_id),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w
        .post_in(
            &w.b_tok,
            G,
            with(insight("После отзыва", ""), "scope", json!("global")),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/solution-access");
}

#[tokio::test]
async fn triggers_validation_422_with_index() {
    let w = world("triggers-422", false).await;
    let bad = [
        (
            json!([{"type": "file_glob", "pattern": "**/*.sh"}, {"type": "file_glob", "pattern": "a[b"}]),
            "triggers[1]",
        ),
        (
            json!([{"type": "command_regex", "pattern": "(a)\\1"}]),
            "triggers[0]",
        ),
        (json!([{"type": "glob", "pattern": "x"}]), "triggers[0]"),
        (
            json!([{"type": "keyword", "pattern": "x", "tools": vec!["T"; 17]}]),
            "triggers[0]",
        ),
        (
            Value::Array(vec![json!({"type": "keyword", "pattern": "x"}); 33]),
            "triggers",
        ),
    ];
    for (t, idx) in &bad {
        let r = w
            .post(with(insight("Триггеры", ""), "triggers", t.clone()))
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{t}: {}",
            r.text()
        );
        let d = r.json()["detail"].as_str().unwrap().to_string();
        assert!(d.starts_with(idx), "{d}");
    }
    let e = w
        .created(with(
            insight("Триггеры", ""),
            "triggers",
            json!([{"type": "file_glob", "pattern": "**/*.sh", "tools": ["Write", "Edit"]},
                   {"type": "error_regex", "pattern": "\\^M|bad interpreter"}]),
        ))
        .await;
    assert_eq!(e["triggers"].as_array().unwrap().len(), 2);
    assert_eq!(e["triggers"][0]["type"], "file_glob");
    assert_eq!(e["triggers"][0]["tools"], json!(["Write", "Edit"]));
    // PATCH: неверный набор — 422 с индексом, прежний набор остаётся
    let r = w
        .patch(
            &id_of(&e),
            json!({"triggers": [{"type": "error_regex", "pattern": "(?=x)"}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(
        r.json()["detail"]
            .as_str()
            .unwrap()
            .starts_with("triggers[0]")
    );
    assert_eq!(
        w.entry(&id_of(&e)).await["triggers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // триггеры только у insight
    let ch = w
        .post(json!({"kind": "changelog", "title": "ch", "data": {"kind": "fix"}}))
        .await
        .json();
    let r = w
        .patch(
            &id_of(&ch),
            json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

#[tokio::test]
async fn triggers_etag_304_and_changes_after_edit() {
    let w = world("triggers-etag", false).await;
    let e = w
        .created(with(
            insight("LF в shell", ""),
            "triggers",
            json!([{"type": "file_glob", "pattern": "**/*.sh", "tools": ["Write"]}]),
        ))
        .await;
    let k = w
        .post_in(
            &w.a_tok,
            "knowledge",
            with(
                insight("Глобально про CRLF", ""),
                "triggers",
                json!([{"type": "error_regex", "pattern": "bad interpreter"}]),
            ),
            &[],
        )
        .await;
    assert_eq!(k.status, StatusCode::CREATED, "{}", k.text());
    let q = format!("?solution={G}");
    let r = w.triggers(&w.a_tok, &q, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let etag = j["etag"].as_str().unwrap().to_string();
    assert_eq!(r.header("etag"), format!("\"{etag}\""));
    assert_eq!(patterns(&j), vec!["**/*.sh", "bad interpreter"]);
    let t = &j["triggers"][0];
    assert_eq!(t["entry_id"], e["id"]);
    assert_eq!(t["type"], "file_glob");
    assert_eq!(t["tools"], json!(["Write"]));
    assert_eq!(t["title"], "LF в shell");
    assert_eq!(t["insight_kind"], "gotcha");
    assert_eq!(t["space"], G);
    assert_eq!(j["triggers"][1]["space"], "knowledge");
    // If-None-Match — 304 без тела; X-Zam-Solution вместо параметра — тот же набор
    let quoted = format!("\"{etag}\"");
    let r = w.triggers(&w.a_tok, &q, Some(&quoted)).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert!(r.body.is_empty());
    let r =
        w.a.tok_h(
            &w.a_tok,
            Method::GET,
            "/api/v1/triggers",
            None,
            &[("x-zam-solution", G), ("if-none-match", &quoted)],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    // правка триггеров — новый ETag
    let r = w
        .patch(
            &id_of(&e),
            json!({"triggers": [{"type": "file_glob", "pattern": "**/*.bash"}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["triggers"][0]["pattern"], "**/*.bash");
    let r = w.triggers(&w.a_tok, &q, Some(&quoted)).await;
    assert_eq!(r.status, StatusCode::OK);
    let j2 = r.json();
    assert_ne!(j2["etag"].as_str().unwrap(), etag);
    assert_eq!(patterns(&j2), vec!["**/*.bash", "bad interpreter"]);
    // правка заголовка — тоже (заголовок входит в кэш)
    let etag2 = j2["etag"].as_str().unwrap().to_string();
    w.patch(&id_of(&e), json!({"title": "LF в bash"})).await;
    let j3 = w.triggers(&w.a_tok, &q, None).await.json();
    assert_ne!(j3["etag"].as_str().unwrap(), etag2);
    // без решения — только Knowledge; не GUID — 400; неизвестное решение — 404
    let r = w.triggers(&w.a_tok, "", None).await;
    assert_eq!(patterns(&r.json()), vec!["bad interpreter"]);
    let r = w.triggers(&w.a_tok, "?solution=knowledge", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = w
        .triggers(
            &w.a_tok,
            "?solution=0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eff",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

#[tokio::test]
async fn triggers_exclude_superseded_and_retracted() {
    let w = world("triggers-status", false).await;
    let trig = |p: &str| json!([{"type": "keyword", "pattern": p}]);
    let a = w
        .created(with(insight("А", ""), "triggers", trig("a")))
        .await;
    let b = w
        .created(with(insight("Б", ""), "triggers", trig("b")))
        .await;
    w.created(with(
        with(insight("В", ""), "triggers", trig("c")),
        "status",
        json!("revisit"),
    ))
    .await;
    let q = format!("?solution={G}");
    let mut got = patterns(&w.triggers(&w.a_tok, &q, None).await.json());
    got.sort();
    assert_eq!(got, vec!["a", "b", "c"]);
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&a)),
            Some(json!({"title": "А2", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id_of(&b)),
            Some(json!({"reason": "неверно"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        patterns(&w.triggers(&w.a_tok, &q, None).await.json()),
        vec!["c"]
    );
    // триггеры замещённой и отозванной записи сохранены (ничего не удаляется)
    assert_eq!(w.entry(&id_of(&a)).await["triggers"][0]["pattern"], "a");
}

#[tokio::test]
async fn member_triggers_only_solution_and_own_knowledge() {
    let w = world("triggers-member", true).await;
    let trig = |p: &str| json!([{"type": "keyword", "pattern": p}]);
    w.created(with(insight("Решение", ""), "triggers", trig("solution")))
        .await;
    let r = w
        .post_in(
            &w.a_tok,
            "knowledge",
            with(
                insight("Личное Анны", ""),
                "triggers",
                trig("anna-knowledge"),
            ),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = w
        .post_in(
            &w.b_tok,
            "knowledge",
            with(
                insight("Личное Бориса", ""),
                "triggers",
                trig("boris-knowledge"),
            ),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let q = format!("?solution={G}");
    let j = w.triggers(&w.b_tok, &q, None).await.json();
    assert_eq!(patterns(&j), vec!["solution", "boris-knowledge"]);
    // владелец видит свои
    let j = w.triggers(&w.a_tok, &q, None).await.json();
    assert_eq!(patterns(&j), vec!["solution", "anna-knowledge"]);
    // отзыв доступа — 403 со следующего запроса; без решения — только свой Knowledge
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{}/revoke", w.b_id),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = w.triggers(&w.b_tok, &q, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["state"], "revoked");
    let j = w.triggers(&w.b_tok, "", None).await.json();
    assert_eq!(patterns(&j), vec!["boris-knowledge"]);
}

#[tokio::test]
async fn strict_area_filter() {
    let w = world("area", false).await;
    let web = w
        .post_in(
            &w.a_tok,
            G,
            insight("Кэш области web", "кэш"),
            &[("x-zam-area", "web")],
        )
        .await
        .json();
    let api = w
        .post_in(
            &w.a_tok,
            G,
            insight("Кэш области api", "кэш"),
            &[("x-zam-area", "api")],
        )
        .await
        .json();
    let none = w.created(insight("Кэш без области", "кэш")).await;
    assert_eq!(
        (web["area"].clone(), api["area"].clone()),
        (json!("web"), json!("api"))
    );
    let q = |area: Option<&str>| {
        let mut scope = json!({"solution": G});
        if let Some(a) = area {
            scope["area"] = json!(a);
        }
        json!({"query": "кэш", "scope": scope, "filters": {"kinds": ["insight"]}})
    };
    let mut all = hit_ids(&w.search(q(None)).await);
    all.sort();
    let mut want = vec![id_of(&web), id_of(&api), id_of(&none)];
    want.sort();
    assert_eq!(all, want);
    assert_eq!(
        hit_ids(&w.search(q(Some(" web "))).await),
        vec![id_of(&web)]
    );
    assert_eq!(hit_ids(&w.search(q(Some("api"))).await), vec![id_of(&api)]);
    assert!(hit_ids(&w.search(q(Some("ml"))).await).is_empty());
    // пустая область — без фильтра; слишком длинная — 400
    assert_eq!(hit_ids(&w.search(q(Some(""))).await).len(), 3);
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            "/api/v1/search",
            Some(q(Some(&"a".repeat(129)))),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // буст текущей области (X-Zam-Area) поднимает запись своей области первой
    for (area, first) in [("web", &web), ("api", &api)] {
        let r =
            w.a.tok_h(
                &w.a_tok,
                Method::POST,
                "/api/v1/search",
                Some(q(None)),
                &[("x-zam-area", area)],
            )
            .await;
        assert_eq!(hit_ids(&r.json())[0], id_of(first), "{area}");
    }
}

/// Строки аудита с действием `action`: цели (`target`) по порядку.
async fn audit_targets(w: &W, action: &str) -> Vec<String> {
    let r =
        w.a.adm(Method::GET, "/api/v1/admin/audit?limit=200", None, None)
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"] == action)
        .map(|i| i["target"].as_str().unwrap_or("").to_string())
        .collect()
}

#[tokio::test]
async fn supersede_audited_on_both_paths() {
    let w = world("supersede-audit", false).await;
    let a = w.created(insight("Аудит А", "")).await;
    let b = w.created(insight("Аудит Б", "")).await;
    // POST /entries/{id}/supersede
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&a)),
            Some(json!({"title": "Аудит А2", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // supersedes_id при создании; повтор по ключу новой строки не добавляет
    let body = with(insight("Аудит Б2", ""), "supersedes_id", b["id"].clone());
    for want in [StatusCode::CREATED, StatusCode::OK] {
        let r = w
            .post_in(&w.a_tok, G, body.clone(), &[("idempotency-key", "sup-b")])
            .await;
        assert_eq!(r.status, want, "{}", r.text());
    }
    let mut got = audit_targets(&w, "entry.supersede").await;
    got.sort();
    let mut want = vec![id_of(&a), id_of(&b)];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn superseded_and_retracted_content_is_frozen_409() {
    let w = world("frozen", false).await;
    let a = w.created(insight("Замороженная", "Текст.")).await;
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id_of(&a)),
            Some(json!({"title": "Новая", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let b = w.created(insight("Отозванная", "Текст.")).await;
    let r = w
        .req(
            &w.a_tok,
            Method::POST,
            &format!("/api/v1/entries/{}/retract", id_of(&b)),
            Some(json!({"reason": "неверно"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    for id in [id_of(&a), id_of(&b)] {
        for patch in [
            json!({"title": "Переписано"}),
            json!({"body": "Переписано"}),
            json!({"append_body": "Ещё"}),
            json!({"data": {"kind": "failure"}}),
            json!({"tags": ["x"]}),
            json!({"area": "web"}),
            json!({"triggers": [{"type": "keyword", "pattern": "x"}]}),
        ] {
            let r = w.patch(&id, patch.clone()).await;
            assert_eq!(r.status, StatusCode::CONFLICT, "{patch}: {}", r.text());
        }
        let e = w.entry(&id).await;
        assert_eq!(
            (e["rev"].clone(), e["body"].clone()),
            (json!(2), json!("Текст."))
        );
    }
    // действующая запись правится как обычно
    let n = w.created(insight("Живая", "")).await;
    let r = w.patch(&id_of(&n), json!({"title": "Живая 2"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn idempotency_fingerprint_includes_triggers() {
    let w = world("idem-triggers", false).await;
    let body = with(
        insight("С триггерами", ""),
        "triggers",
        json!([{"type": "keyword", "pattern": "lf"}]),
    );
    let k = [("idempotency-key", "trig-1")];
    let r = w.post_in(&w.a_tok, G, body.clone(), &k).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // тот же набор (ключевое слово с пробелами нормализуется) — та же запись
    let same = with(
        body.clone(),
        "triggers",
        json!([{"type": "keyword", "pattern": " lf "}]),
    );
    let r2 = w.post_in(&w.a_tok, G, same, &k).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.json()["id"], r.json()["id"]);
    // иной набор — 422 idempotency-mismatch
    for other in [
        json!([{"type": "keyword", "pattern": "crlf"}]),
        json!([]),
        json!([{"type": "keyword", "pattern": "lf"}, {"type": "keyword", "pattern": "x"}]),
    ] {
        let r = w
            .post_in(
                &w.a_tok,
                G,
                with(body.clone(), "triggers", other.clone()),
                &k,
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{other}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/idempotency-mismatch");
    }
}

#[tokio::test]
async fn search_boosts_gotcha_above_equal_note() {
    // умолчание задачи 18 M3 — 1,05 (ниже theorem); механизм проверяется на явных 1,15
    let w = world_with("boost", false, |c| {
        c.search.boost_gotcha = 1.15;
        c.search.boost_failure = 1.15;
    })
    .await;
    let title = "Кодировка конца строк";
    let body = "Скрипты оболочки ломаются от символа возврата каретки.";
    let r =
        w.a.tok(
            &w.a_tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": G, "path": "notes/eol", "kind": "note",
                        "content": format!("# {title}\n\n{body}\n")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let note = id_of(&r.json());
    let gotcha = w.created(insight(title, body)).await;
    // тот же текст другого вида — без буста
    let plain = w
        .created(with(
            with(insight(title, body), "data", json!({"kind": "discovery"})),
            "force",
            json!(true),
        ))
        .await;
    let j = w
        .search(json!({"query": "возврата каретки", "scope": {"solution": G}, "mode": "chunks"}))
        .await;
    let items = j["items"].as_array().unwrap();
    let pos = |key: &str, id: &str| {
        items
            .iter()
            .position(|i| i[key] == id)
            .unwrap_or_else(|| panic!("нет {id}: {j}"))
    };
    let (g, p, n) = (
        pos("entry_id", &id_of(&gotcha)),
        pos("entry_id", &id_of(&plain)),
        pos("node_id", &note),
    );
    assert_eq!(g, 0, "{j}");
    assert!(g < n && g < p, "{j}");
    // буст — множитель 1,15 к баллу записи того же текста другого вида, и при балле выше 1
    // после буста: нормировка делением на лучший сохраняет отношение (раньше проверка шла
    // только при балле < 1). Три равных текста стоят в канале на местах 1–3 — до бустов
    // баллы различаются не больше чем в (rrf_k + 3) / (rrf_k + 1) ≈ 3,3 % раза.
    let score = |i: usize| items[i]["score"].as_f64().unwrap();
    assert!(score(g) <= 1.0, "{j}");
    let ratio = score(g) / score(p);
    assert!(
        ratio > 1.15 * 61.0 / 63.0 - 1e-3 && ratio < 1.15 * 63.0 / 61.0 + 1e-3,
        "{ratio}: {j}"
    );
}
