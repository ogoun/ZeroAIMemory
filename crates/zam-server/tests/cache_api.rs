//! Кэш выжимок через REST (§9.3–9.4, §12; задача 7 M4). Сервер без моделей: попадание —
//! точное по `(scope_hash, query_norm)`.
//!
//! Пользователи общего решения: A — владелец, B — участник (одобряется в тесте).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";
const Q: &str = "Как деплоим parser-сервис";

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn put(a: &Api, tok: &str, body: Value, h: &[(&str, &str)]) -> Resp {
    a.tok_h(tok, Method::POST, "/api/v1/cache", Some(body), h)
        .await
}

async fn search(a: &Api, tok: &str, body: Value, h: &[(&str, &str)]) -> Resp {
    let r = a
        .tok_h(tok, Method::POST, "/api/v1/search", Some(body), h)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r
}

async fn note_by(a: &Api, tok: &str, space: &str, path: &str, text: &str) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": text})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    id(&r.json())
}

struct W {
    a: Api,
    a_tok: String,
    b_id: String,
    b_tok: String,
}

/// Решение G владельца A, запрос доступа B (ещё не одобрен), заметка решения.
async fn world(name: &str) -> (W, String) {
    let a = api(&format!("cache-{name}"), |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let reg = json!({ "agent_solution_id": G, "name": "Общее" });
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let n = note_by(
        &a,
        &a_tok,
        G,
        "Деплой",
        "# Деплой\n\nСборка multi-stage, для arm64 нужен buildx.\n",
    )
    .await;
    (
        W {
            a,
            a_tok,
            b_id,
            b_tok,
        },
        n,
    )
}

impl W {
    async fn approve(&self, access: &str) {
        let r = self
            .a
            .tok(
                &self.a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{}/approve", self.b_id),
                Some(json!({ "access": access })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    async fn revoke(&self) {
        let r = self
            .a
            .tok(
                &self.a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{}/revoke", self.b_id),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
}

/// Поиск без `no_cache` отдаёт действующую выжимку первым элементом (md и json); с
/// `no_cache` — нет; запись выжимки с `X-Zam-Session` — запись в память (счётчик сессии).
#[tokio::test]
async fn search_returns_cache_first() {
    let a = api("cache-first", |_| {}).await;
    let n = id(&a
        .note(
            "knowledge",
            "Инструменты/Bash",
            "# Bash\n\nСкрипты bash на windows пишите с LF.\n",
        )
        .await);
    let r = put(
        &a,
        &a.token,
        json!({"query": "bash скрипты на windows", "summary": "Пишите bash-скрипты с LF (n:x).",
               "sources": [{"id": format!("n:{n}"), "rev": 1}]}),
        &[("x-zam-session", "s-cache"), ("x-zam-machine", "box")],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let c = r.json();
    assert_eq!(c["state"], "valid", "{c}");
    assert_eq!(c["sources"][0]["id"], format!("n:{n}"));
    assert_eq!(c["sources"][0]["rev"], 1);
    assert!(c["ref"].as_str().unwrap().starts_with("c_"));
    let s = a
        .t(Method::GET, "/api/v1/sessions/s-cache", None)
        .await
        .json();
    assert_eq!(s["memory_writes"], 1, "{s}");

    let md = search(
        &a,
        &a.token,
        json!({"query": "Bash  скрипты на Windows", "format": "md"}),
        &[("x-zam-session", "s-cache")],
    )
    .await
    .text();
    let first = md.lines().next().unwrap();
    let date = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
    assert_eq!(
        first,
        format!(
            "[cache] «bash скрипты на windows» — выжимка {date}, 1 источник, все актуальны (id: {})",
            c["ref"].as_str().unwrap()
        ),
        "{md}"
    );
    assert_eq!(
        md.lines().nth(1),
        Some("Пишите bash-скрипты с LF (n:x)."),
        "{md}"
    );
    assert!(
        md.contains(&format!("1. n:{n} Knowledge/Инструменты/Bash")),
        "{md}"
    );
    let j = search(
        &a,
        &a.token,
        json!({"query": "bash скрипты на windows"}),
        &[],
    )
    .await
    .json();
    let it = &j["items"][0];
    assert_eq!(it["kind"], "cache", "{j}");
    assert_eq!(it["text"], "Пишите bash-скрипты с LF (n:x).");
    assert_eq!(it["state"], "valid");
    assert_eq!(it["sources_count"], 1);
    assert_eq!(j["items"][1]["node_id"], n, "{j}");
    assert!(j["timings"]["cache_ms"].is_number(), "{j}");
    // чужая сессия: автор и машина в строке
    let md = search(
        &a,
        &a.token,
        json!({"query": "bash скрипты на windows", "format": "md"}),
        &[],
    )
    .await
    .text();
    assert!(
        md.lines()
            .next()
            .unwrap()
            .ends_with(" · автор admin, машина box"),
        "{md}"
    );
    let j = search(
        &a,
        &a.token,
        json!({"query": "bash скрипты на windows", "no_cache": true}),
        &[],
    )
    .await
    .json();
    assert_eq!(j["items"][0]["node_id"], n, "{j}");
    assert!(j["timings"]["cache_ms"].is_null());
    // другая область поиска — другой ключ
    let j = search(
        &a,
        &a.token,
        json!({"query": "bash скрипты на windows", "scope": {"include_rules": false}}),
        &[],
    )
    .await
    .json();
    assert_ne!(j["items"][0]["kind"], "cache", "{j}");
    // список и карточка; summary больше предела — 413
    let l = a
        .t(Method::GET, "/api/v1/cache?q=windows", None)
        .await
        .json();
    assert_eq!(l["items"].as_array().unwrap().len(), 1, "{l}");
    assert_eq!(l["items"][0]["hits"], 3, "{l}");
    let big = "слово ".repeat(800);
    let r = put(
        &a,
        &a.token,
        json!({"query": "q", "summary": big, "sources": [{"id": n}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
}

/// Критерий приёмки M4: изменение источника — выжимка `stale` со списком изменившихся
/// источников, в поиске — строка `[cache, stale: …]` без текста выжимки.
#[tokio::test]
async fn source_change_makes_stale_with_list() {
    let a = api("cache-stale", |_| {}).await;
    let n1 = id(&a
        .note("knowledge", "Деплой", "# Деплой\n\nbuildx для arm64.\n")
        .await);
    let n2 = id(&a.note("knowledge", "Сеть", "# Сеть\n\nпорт 8080.\n").await);
    let c = put(
        &a,
        &a.token,
        json!({"query": Q, "summary": "Деплой: buildx, порт 8080.",
               "sources": [{"id": &n1}, {"id": format!("n:{n2}")}]}),
        &[],
    )
    .await;
    assert_eq!(c.status, StatusCode::CREATED, "{}", c.text());
    let cid = id(&c.json());
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{n1}"),
            Some(json!({"content": "# Деплой\n\nbuildx не нужен.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let g = a
        .t(Method::GET, &format!("/api/v1/cache/{cid}"), None)
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text());
    let g = g.json();
    assert_eq!(g["state"], "stale", "{g}");
    assert_eq!(
        g["stale"],
        json!([{"target": format!("n:{n1}"), "reason": "changed"}]),
        "{g}"
    );
    let md = search(&a, &a.token, json!({"query": Q, "format": "md"}), &[])
        .await
        .text();
    let first = md.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("[cache, stale: n:{n1} (changed)] «{Q}»")),
        "{md}"
    );
    assert!(!md.contains("Деплой: buildx, порт 8080."), "{md}");
    let j = search(&a, &a.token, json!({"query": Q}), &[]).await.json();
    assert_eq!(j["items"][0]["state"], "stale", "{j}");
    assert!(j["items"][0].get("text").is_none(), "{j}");
    // удаление второго — ещё одна причина; список по state
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{n2}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let g = a
        .t(Method::GET, &format!("/api/v1/cache/c_{cid}"), None)
        .await
        .json();
    assert_eq!(g["stale"].as_array().unwrap().len(), 2, "{g}");
    assert_eq!(g["stale"][1]["reason"], "deleted");
    let l = a
        .t(Method::GET, "/api/v1/cache?state=stale", None)
        .await
        .json();
    assert_eq!(l["items"].as_array().unwrap().len(), 1, "{l}");
    let l = a
        .t(Method::GET, "/api/v1/cache?state=valid", None)
        .await
        .json();
    assert!(l["items"].as_array().unwrap().is_empty(), "{l}");
    // устаревшая ревизия в запросе — 409 с текущей
    let r = put(
        &a,
        &a.token,
        json!({"query": "x", "summary": "s", "sources": [{"id": &n1, "rev": 1}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["rev"], 2);
}

/// Выжимка участника в решении общая: владелец видит её в поиске по решению с автором и
/// машиной, в списке решения — с автором; удалить чужую выжимку участник не может, автор — да.
#[tokio::test]
async fn cache_shared_in_solution_with_author() {
    let (w, n) = world("shared").await;
    w.approve("rw").await;
    let r = put(
        &w.a,
        &w.b_tok,
        json!({"query": Q, "summary": "Сборка multi-stage, arm64 — buildx.", "solution": G,
               "sources": [{"id": format!("{G}:/Деплой")}]}),
        &[("x-zam-session", "sb"), ("x-zam-machine", "boris-pc")],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let c = r.json();
    assert_eq!(c["author"], "Борис", "{c}");
    assert_eq!(c["agent_solution_id"], G);
    assert_eq!(c["sources"][0]["id"], format!("n:{n}"));
    let md = search(
        &w.a,
        &w.a_tok,
        json!({"query": Q, "scope": {"solution": G}, "format": "md"}),
        &[("x-zam-session", "sa")],
    )
    .await
    .text();
    let first = md.lines().next().unwrap();
    assert!(first.starts_with("[cache] «"), "{md}");
    assert!(first.ends_with(" · автор Борис, машина boris-pc"), "{md}");
    // заголовок X-Zam-Solution — то же решение
    let j = search(
        &w.a,
        &w.a_tok,
        json!({"query": Q}),
        &[("x-zam-solution", G)],
    )
    .await
    .json();
    assert_eq!(j["items"][0]["kind"], "cache", "{j}");
    assert_eq!(j["items"][0]["author"], "Борис");
    assert_eq!(j["items"][0]["agent_solution_id"], G);
    let l =
        w.a.tok(
            &w.a_tok,
            Method::GET,
            &format!("/api/v1/cache?solution={G}"),
            None,
        )
        .await
        .json();
    assert_eq!(l["items"][0]["author"], "Борис", "{l}");
    // выжимку владельца участник удалить не может; свою — может
    let r = put(
        &w.a,
        &w.a_tok,
        json!({"query": "другой", "summary": "s", "solution": G, "sources": [{"id": &n}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let own = id(&r.json());
    // fix round 1 (I1): участник сохраняет тот же запрос — выжимка владельца остаётся
    let r = put(
        &w.a,
        &w.b_tok,
        json!({"query": "другой", "summary": "версия Бориса", "solution": G,
               "sources": [{"id": &n}]}),
        &[("x-zam-session", "sb")],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let borisa = id(&r.json());
    // фильтр по автору (WebUI «Кэш выжимок», задача 13)
    let list = |q: String| {
        let w = &w;
        async move { w.a.tok(&w.a_tok, Method::GET, &q, None).await }
    };
    let all = list(format!("/api/v1/cache?solution={G}")).await.json();
    assert_eq!(all["items"].as_array().unwrap().len(), 3, "{all}");
    let by_b = list(format!("/api/v1/cache?solution={G}&author={}", w.b_id))
        .await
        .json();
    let items = by_b["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{by_b}");
    assert!(items.iter().all(|i| i["author"] == "Борис"), "{by_b}");
    let r = list(format!("/api/v1/cache?solution={G}&author=мусор")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    for c in [&own, &borisa] {
        let r =
            w.a.tok(&w.a_tok, Method::GET, &format!("/api/v1/cache/{c}"), None)
                .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r =
        w.a.tok(
            &w.b_tok,
            Method::DELETE,
            &format!("/api/v1/cache/{own}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r =
        w.a.tok(
            &w.b_tok,
            Method::DELETE,
            &format!("/api/v1/cache/{}", id(&c)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r =
        w.a.tok(
            &w.a_tok,
            Method::GET,
            &format!("/api/v1/cache/{}", id(&c)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// Отзыв доступа: выжимки решения участнику больше не выдаются — ни по id, ни списком, ни
/// в поиске. Выжимка владельца с источником в его Knowledge участнику не видна и до отзыва.
#[tokio::test]
async fn revoked_participant_cannot_read_solution_cache() {
    let (w, n) = world("revoked").await;
    w.approve("ro").await;
    let private = note_by(
        &w.a,
        &w.a_tok,
        "knowledge",
        "Личное",
        "# Личное\n\nтолько A\n",
    )
    .await;
    let r = put(
        &w.a,
        &w.a_tok,
        json!({"query": Q, "summary": "Выжимка решения.", "solution": G, "sources": [{"id": &n}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let cid = id(&r.json());
    let r = put(
        &w.a,
        &w.a_tok,
        json!({"query": "с личным", "summary": "Тайное из Knowledge.", "solution": G,
               "sources": [{"id": &n}, {"id": &private}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let hidden = id(&r.json());
    let path = format!("/api/v1/cache/{cid}");
    let r = w.a.tok(&w.b_tok, Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r =
        w.a.tok(
            &w.b_tok,
            Method::GET,
            &format!("/api/v1/cache/{hidden}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let l =
        w.a.tok(
            &w.b_tok,
            Method::GET,
            &format!("/api/v1/cache?solution={G}"),
            None,
        )
        .await
        .json();
    assert_eq!(l["items"].as_array().unwrap().len(), 1, "{l}");
    assert!(!l.to_string().contains("Тайное"), "{l}");
    let j = search(
        &w.a,
        &w.b_tok,
        json!({"query": Q, "scope": {"solution": G}}),
        &[],
    )
    .await
    .json();
    assert_eq!(j["items"][0]["kind"], "cache", "{j}");
    // участник ro писать в кэш решения не может
    let r = put(
        &w.a,
        &w.b_tok,
        json!({"query": "b", "summary": "s", "solution": G, "sources": [{"id": &n}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    w.revoke().await;
    let r = w.a.tok(&w.b_tok, Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["state"], "revoked");
    let r =
        w.a.tok(
            &w.b_tok,
            Method::GET,
            &format!("/api/v1/cache?solution={G}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let j = search(
        &w.a,
        &w.b_tok,
        json!({"query": Q, "scope": {"solution": G}}),
        &[],
    )
    .await
    .json();
    assert!(
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "cache"),
        "{j}"
    );
    assert!(!j.to_string().contains("Выжимка решения."), "{j}");
}

/// Источник из Human (по id и пути) и Vault — 422 `cache-forbidden-source`; несуществующий —
/// 422 `cache-source-not-found`.
#[tokio::test]
async fn cache_rejects_human_and_vault_sources() {
    let a = api("cache-forbidden", |_| {}).await;
    let user = a.admin_id();
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "human", "path": "тайна", "kind": "note", "content": "секрет\n"})),
            Some(&user),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let human = id(&r.json());
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/vault",
            Some(json!({"title": "git", "data": {"type": "token"}, "secret": "zv-secret-x7Lp2Rt9Wd"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let vault = id(&r.json());
    for src in [
        human.clone(),
        format!("n:{human}"),
        "human:/тайна".to_string(),
        vault.clone(),
    ] {
        let r = put(
            &a,
            &a.token,
            json!({"query": "q", "summary": "s", "sources": [{"id": &src}]}),
            &[],
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{src}: {}",
            r.text()
        );
        assert_eq!(
            r.json()["type"],
            "/problems/cache-forbidden-source",
            "{src}"
        );
        assert!(!r.text().contains("секрет"));
    }
    let r = put(
        &a,
        &a.token,
        json!({"query": "q", "summary": "s", "sources": [{"id": zam_core::Id::new().to_string()}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/cache-source-not-found");
    let r = put(
        &a,
        &a.token,
        json!({"query": "q", "summary": "s", "sources": []}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // fix round 1 (M4): отозванный insight — 422 cache-source-inactive
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "insight", "title": "Неверное", "body": "x",
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let ins = id(&r.json());
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{ins}/retract"),
            Some(json!({"reason": "опровергнуто"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = put(
        &a,
        &a.token,
        json!({"query": "q", "summary": "s", "sources": [{"id": format!("i:{ins}")}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.json()["type"], "/problems/cache-source-inactive");
    // fix round 1 (M1): scope.spaces и all_solutions — 400
    let n = id(&a.note("knowledge", "Есть", "# Есть\n").await);
    for scope in [
        json!({"spaces": ["knowledge"]}),
        json!({"all_solutions": true}),
    ] {
        let r = put(
            &a,
            &a.token,
            json!({"query": "q", "summary": "s", "scope": scope, "sources": [{"id": &n}]}),
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{scope}: {}", r.text());
    }
}

/// Источник из другой базы — 422 `cache-cross-db`: участник цитирует свой Knowledge в кэше
/// решения и заметку решения в личном кэше.
#[tokio::test]
async fn cache_cross_db_source_422() {
    let (w, n) = world("cross").await;
    w.approve("rw").await;
    let mine = note_by(&w.a, &w.b_tok, "knowledge", "Моё", "# Моё\n\nличное B\n").await;
    for (body, what) in [
        (
            json!({"query": "q", "summary": "s", "solution": G, "sources": [{"id": &mine}]}),
            "личный источник в кэше решения",
        ),
        (
            json!({"query": "q", "summary": "s", "sources": [{"id": format!("n:{n}")}]}),
            "источник решения в личном кэше",
        ),
        (
            json!({"query": "q", "summary": "s", "sources": [{"id": format!("{G}:/Деплой")}]}),
            "путь решения в личном кэше",
        ),
    ] {
        let r = put(&w.a, &w.b_tok, body, &[]).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{what}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "/problems/cache-cross-db", "{what}");
    }
    // в своей базе — можно
    let r = put(
        &w.a,
        &w.b_tok,
        json!({"query": "q", "summary": "s", "sources": [{"id": &mine}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

/// `promote` создаёт заметку со связями `derived_from` на источники и отмечает
/// `promoted_to`; `POST /cache` с `promote` — insight сразу; повтор — 409.
#[tokio::test]
async fn promote_creates_note_with_derived_from() {
    let a = api("cache-promote", |_| {}).await;
    let n1 = id(&a.note("knowledge", "Деплой", "# Деплой\n\nbuildx.\n").await);
    let n2 = id(&a.note("knowledge", "Сеть", "# Сеть\n\nпорт.\n").await);
    let r = put(
        &a,
        &a.token,
        json!({"query": Q, "summary": "# Деплой parser\n\nbuildx и порт 8080.\n",
               "sources": [{"id": &n1}, {"id": &n2}]}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let cid = id(&r.json());
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/cache/{cid}/promote"),
            Some(json!({"as": "note", "path": "Выжимки/Деплой parser"})),
            &[("x-zam-session", "sp")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    let note_ref = j["promoted"].as_str().unwrap().to_string();
    assert_eq!(j["promoted_to"], note_ref.as_str(), "{j}");
    let nid = note_ref.strip_prefix("n:").unwrap();
    let node = a
        .t(
            Method::GET,
            &format!("/api/v1/nodes/{nid}?format=json"),
            None,
        )
        .await;
    assert_eq!(node.status, StatusCode::OK, "{}", node.text());
    assert_eq!(node.json()["node"]["path"], "Выжимки/Деплой parser");
    let links = a
        .t(Method::GET, &format!("/api/v1/nodes/{nid}/links"), None)
        .await
        .json();
    let dst: Vec<&str> = links["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "derived_from")
        .map(|e| e["dst"].as_str().unwrap())
        .collect();
    assert_eq!(dst.len(), 2, "{links}");
    assert!(dst.contains(&format!("n:{n1}").as_str()) && dst.contains(&format!("n:{n2}").as_str()));
    let s = a.t(Method::GET, "/api/v1/sessions/sp", None).await.json();
    assert_eq!(s["memory_writes"], 1, "{s}");
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/cache/{cid}/promote"),
            Some(json!({"as": "note", "path": "Выжимки/ещё"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // fix round 1 (M3): превращение при сохранении на занятый путь — 409, выжимки нет
    let before = a.t(Method::GET, "/api/v1/cache", None).await.json()["items"]
        .as_array()
        .unwrap()
        .len();
    let r = put(
        &a,
        &a.token,
        json!({"query": "занято", "summary": "s", "sources": [{"id": &n1}],
               "promote": {"as": "note", "path": "Деплой"}}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let l = a
        .t(
            Method::GET,
            &format!("/api/v1/cache?q={}", common::api::enc("занято")),
            None,
        )
        .await
        .json();
    assert!(l["items"].as_array().unwrap().is_empty(), "{l}");
    let after = a.t(Method::GET, "/api/v1/cache", None).await.json()["items"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(before, after);
    // сразу при сохранении — insight
    let r = put(
        &a,
        &a.token,
        json!({"query": "порт сервиса", "summary": "Порт 8080.", "sources": [{"id": &n2}],
               "promote": {"as": "insight", "kind": "decision"}}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    let eref = j["promoted"].as_str().unwrap();
    let eid = eref.strip_prefix("e:").unwrap();
    let e = a
        .t(Method::GET, &format!("/api/v1/entries/{eid}"), None)
        .await
        .json();
    assert_eq!(e["title"], "порт сервиса", "{e}");
    assert_eq!(e["data"]["kind"], "decision");
    let back = a
        .t(Method::GET, &format!("/api/v1/nodes/{n2}/backlinks"), None)
        .await
        .json();
    assert!(
        back["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["type"] == "derived_from" && x["src"] == eref),
        "{back}"
    );
}

/// Стенд Debian 13 (доработка): `POST /cache` с секцией-хвостом (`Тесты`) — как `/read`; в
/// источнике — полный путь (`Сборка > Тесты`); неоднозначный хвост — 422.
#[tokio::test]
async fn cache_put_section_tail_stores_full_path() {
    let a = api("cache-tail", |_| {}).await;
    let n = id(&a
        .note(
            "knowledge",
            "Сборка",
            "# Сборка\n\nmake.\n\n## Тесты\n\ncargo test.\n\n# A\n\n## Шаги\n\n1.\n\n# B\n\n## Шаги\n\n2.\n",
        )
        .await);
    let c = put(
        &a,
        &a.token,
        json!({"query": "как тестировать сборку", "summary": "cargo test.",
               "sources": [{"id": &n, "section": "Тесты"}]}),
        &[],
    )
    .await;
    assert_eq!(c.status, StatusCode::CREATED, "{}", c.text());
    let v = c.json();
    assert_eq!(v["sources"][0]["section"], "Сборка > Тесты", "{v}");
    // чтение по тому же хвосту — та же секция
    let r = a
        .t(
            Method::POST,
            "/api/v1/read",
            Some(json!({"items": [{"id": &n, "section": "Тесты"}]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"][0]["text"], "## Тесты\n\ncargo test.\n\n");
    let c = put(
        &a,
        &a.token,
        json!({"query": "шаги", "summary": "шаги.",
               "sources": [{"id": &n, "section": "Шаги"}]}),
        &[],
    )
    .await;
    assert_eq!(c.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", c.text());
    assert!(c.text().contains("неоднозначен"), "{}", c.text());
}
