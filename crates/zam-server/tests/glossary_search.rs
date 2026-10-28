//! Словарь в поиске и вспоминании (M4.5, задача 3): канал `alias` (расширение запроса именами и
//! псевдонимами терминов), `expanded` в ответе, вес `search.w_fts_alias`, умолчания видов
//! (`term`, `question`), кэш словаря по версии, подсказка термина к промпту и строка словаря в
//! дайджесте. Сервер без моделей (FTS-only): канал `alias` работает в FTS.
mod common;

use std::time::Instant;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};

const KN: &str = "/api/v1/spaces/knowledge/entries";
const RECALL: &str = "/api/v1/recall";

fn recall_cfg(c: &mut zam_server::config::ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

async fn term(a: &Api, title: &str, aliases: &[&str]) -> String {
    let r = a
        .t(
            Method::POST,
            KN,
            Some(json!({"kind": "term", "title": title, "body": "",
                "data": {"definition": format!("Определение: {title}"), "aliases": aliases}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn search(a: &Api, body: Value) -> Value {
    let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Заметки (`node_id`) выдачи по запросу `q` среди видов `kinds`.
async fn found_nodes(a: &Api, q: &str) -> (Vec<String>, Value) {
    let j = search(a, json!({"query": q, "filters": {"kinds": ["note"]}})).await;
    let ids = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["node_id"].as_str().map(str::to_string))
        .collect();
    (ids, j)
}

async fn recall(a: &Api, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Запрос «summary» находит заметку со словом «выжимка», только когда в словаре есть термин с
/// псевдонимом `summary`; хит несёт источник `alias`.
#[tokio::test]
async fn alias_channel_finds_ru_by_en() {
    let a = api("gs-alias", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Глоссарий/Кэш",
            "# Кэш ответов\n\nВыжимка записи хранится отдельно и отдаётся быстро.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (ids, j) = found_nodes(&a, "summary").await;
    assert!(ids.is_empty(), "без словаря не находит: {j}");
    assert_eq!(j["expanded"], json!([]), "{j}");

    term(&a, "Выжимка", &["summary"]).await;
    let (ids, j) = found_nodes(&a, "summary").await;
    assert_eq!(ids, vec![n], "{j}");
    let src = j["items"][0]["sources"].as_array().unwrap();
    assert!(src.contains(&json!("alias")), "{j}");
    // сниппет канала alias — с подсветкой найденного слова
    let snippet = j["items"][0]["snippet"].as_str().unwrap();
    assert!(snippet.to_lowercase().contains("выжимка"), "{j}");
}

/// `expanded: [{term, word, with}]`: слово запроса, термин и остальные имена.
#[tokio::test]
async fn expanded_in_response() {
    let a = api("gs-expanded", |_| {}).await;
    term(&a, "Выжимка", &["summary", "digest"]).await;
    let j = search(&a, json!({"query": "Нужен SUMMARY записи"})).await;
    assert_eq!(
        j["expanded"],
        json!([{"term": "Выжимка", "word": "SUMMARY", "with": ["Выжимка", "digest"]}]),
        "{j}"
    );
    // запрос без совпадений со словарём — пустой список
    let j = search(&a, json!({"query": "ничего общего"})).await;
    assert_eq!(j["expanded"], json!([]), "{j}");
}

/// `search.w_fts_alias = 0` выключает канал: заметка по псевдониму не находится, расширения нет.
#[tokio::test]
async fn alias_weight_zero_disables() {
    let a = api("gs-alias-off", |c| c.search.w_fts_alias = 0.0).await;
    a.note("knowledge", "Глоссарий/Кэш", "# Кэш\n\nВыжимка записи.\n")
        .await;
    term(&a, "Выжимка", &["summary"]).await;
    let (ids, j) = found_nodes(&a, "summary").await;
    assert!(ids.is_empty(), "{j}");
    assert_eq!(j["expanded"], json!([]), "{j}");
}

/// Не больше 8 групп в запросе; псевдоним короче 3 символов — точным словом: «db» не находит
/// «dbx» и не расширяет слово «dbx».
#[tokio::test]
async fn alias_expansion_capped_and_short_exact() {
    let a = api("gs-alias-cap", |_| {}).await;
    for i in 0..12 {
        term(&a, &format!("Термин{i}"), &[&format!("al{i}x")]).await;
    }
    let q = (0..12)
        .map(|i| format!("al{i}x"))
        .collect::<Vec<_>>()
        .join(" ");
    let j = search(&a, json!({"query": q})).await;
    assert_eq!(j["expanded"].as_array().unwrap().len(), 8, "{j}");

    term(&a, "Database", &["db"]).await;
    let n_dbx = a
        .note(
            "knowledge",
            "Заметки/dbx",
            "# Первая\n\nЗапуск dbx для проверки.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let n_db = a
        .note(
            "knowledge",
            "Заметки/db",
            "# Вторая\n\nСмотри db и сервер.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (ids, j) = found_nodes(&a, "database").await;
    assert_eq!(ids, vec![n_db], "{j}");
    assert!(!ids.contains(&n_dbx));
    assert_eq!(j["expanded"][0]["with"], json!(["db"]), "{j}");
    // слово «dbx» не совпадает с псевдонимом «db»: расширения нет
    let j = search(&a, json!({"query": "dbx"})).await;
    assert_eq!(j["expanded"], json!([]), "{j}");
}

/// Канал `alias` на словаре из 2000 терминов: p95 поиска с каналом (чтение кэша словаря,
/// расширение и FTS-запрос `alias`) превышает p95 без канала не больше чем на 10 мс.
///
/// Метод: один прогретый набор из 200 запросов, каждый содержит слово-псевдоним; запросы с
/// каналом и без него чередуются (дрейф диска и планировщика бьёт по обеим ветвям поровну); три
/// круга, берётся наименьшая разность p95 — настоящая регрессия видна в каждом круге, дребезг
/// HDD и CI — нет. Термины вставляются одной транзакцией напрямую (REST — `ensure_names_free`
/// на каждый из 2000 вызовов); цепочка записи терминов покрыта остальными тестами.
#[tokio::test]
async fn alias_expansion_large_glossary_latency() {
    let a = api("gs-alias-latency", |_| {}).await;
    for i in 0..40 {
        a.note(
            "knowledge",
            &format!("Статьи/Заметка{i}"),
            &format!(
                "# Заметка {i}\n\nНастройка кэша и очередей, термин{} встречается тут.\n",
                i * 7
            ),
        )
        .await;
    }
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let kn = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == zam_store::SpaceKind::Knowledge)
        .unwrap()
        .id;
    db.write(move |tx| {
        let now = zam_store::time::fmt_ts(jiff::Timestamp::now());
        for i in 0..2000 {
            let data = json!({"definition": format!("Определение {i}"),
                "aliases": [format!("al{i}x"), format!("термин{i}")],
                "not_to_confuse": [], "code": []});
            tx.execute(
                "INSERT INTO entries (id, space_id, kind, title, title_norm, data, status,
                                      author_kind, created_at, updated_at)
                 VALUES (?1, ?2, 'term', ?3, ?4, ?5, 'active', 'system', ?6, ?6)",
                (
                    zam_core::Id::new(),
                    kn,
                    format!("Имя{i}"),
                    format!("имя{i}"),
                    data.to_string(),
                    now.clone(),
                ),
            )?;
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('glossary_version', '1')
             ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let spaces: Vec<zam_core::Id> = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let queries: Vec<String> = (0..200)
        .map(|k| format!("настройка кэша al{}x очередей", (k * 37) % 2000))
        .collect();
    let query = |text: &str, groups: Vec<zam_store::AliasGroup>| zam_store::FtsQuery {
        text: text.to_string(),
        space_ids: spaces.clone(),
        path_prefix: None,
        top_k: 100,
        targets: zam_store::TargetFilter::default(),
        alias_groups: groups,
    };
    let run_with = |text: &str| {
        let text = text.to_string();
        let (st, db, spaces) = (a.st.clone(), db.clone(), spaces.clone());
        async move {
            let idx = st.glossary.index(&db).await.unwrap();
            let words = zam_store::query_terms(&text);
            let groups = zam_server::glossary::alias_groups(&idx.expand(&words, &spaces));
            assert!(!groups.is_empty(), "{text}");
            groups
        }
    };
    // прогрев: кэш словаря, страницы FTS, планы запросов
    for q in queries.iter().take(30) {
        let g = run_with(q).await;
        zam_store::search_fts_channels(&db, query(q, g))
            .await
            .unwrap();
        zam_store::search_fts_channels(&db, query(q, Vec::new()))
            .await
            .unwrap();
    }
    let p95 = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[(v.len() * 95).div_ceil(100) - 1]
    };
    let mut best = f64::MAX;
    let mut report = Vec::new();
    for _ in 0..3 {
        let (mut with, mut without) = (Vec::new(), Vec::new());
        for q in &queries {
            let t = Instant::now();
            let g = run_with(q).await;
            let ch = zam_store::search_fts_channels(&db, query(q, g))
                .await
                .unwrap();
            with.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(!ch.alias.is_empty(), "{q}");
            let t = Instant::now();
            zam_store::search_fts_channels(&db, query(q, Vec::new()))
                .await
                .unwrap();
            without.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let (pw, po) = (p95(&mut with), p95(&mut without));
        report.push((pw, po));
        best = best.min(pw - po);
    }
    eprintln!("p95 с каналом / без, мс по кругам: {report:?}; наименьшая разность {best:.2}");
    assert!(best <= 10.0, "канал alias: +{best:.2} мс p95 ({report:?})");
}

/// Расширение не входит в ключ кэша выжимок: выжимка, записанная до появления словаря,
/// остаётся первым элементом выдачи после него (тот же `scope_hash` и `query_norm`).
#[tokio::test]
async fn cache_key_unchanged_by_expansion() {
    let a = api("gs-cache-key", |_| {}).await;
    let n = a
        .note(
            "knowledge",
            "Глоссарий/Кэш",
            "# Кэш\n\nВыжимка записи хранится отдельно.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = a
        .t(
            Method::POST,
            "/api/v1/cache",
            Some(
                json!({"query": "summary записи", "summary": "Выжимка хранится отдельно.",
                        "sources": [{"id": format!("n:{n}"), "rev": 1}]}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let first = |j: &Value| j["items"][0]["kind"].clone();
    let j = search(&a, json!({"query": "summary записи"})).await;
    assert_eq!(first(&j), "cache", "{j}");
    assert_eq!(j["expanded"], json!([]), "{j}");
    term(&a, "Выжимка", &["summary"]).await;
    let j = search(&a, json!({"query": "summary записи"})).await;
    assert_eq!(first(&j), "cache", "{j}");
    assert_eq!(j["expanded"][0]["word"], "summary", "{j}");
}

/// Подсказка термина к промпту: до двух строк `ZeroAIMemory: термин «…» — … — t:<id>`, каждый
/// термин раз за сессию, `reason: glossary`; в другой сессии показывается снова.
#[tokio::test]
async fn recall_prompt_term_hint_once() {
    let a = api("gs-recall-hint", recall_cfg).await;
    let t1 = term(&a, "Выжимка", &["summary"]).await;
    term(&a, "Чанк", &[]).await;
    term(&a, "Индекс", &[]).await;
    let body = json!({"event": "prompt", "prompt": "Как summary связана с чанк и индекс?"});
    let j = recall(&a, "s-gl", body.clone()).await;
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "не больше двух строк: {j}");
    assert!(items.iter().all(|i| i["reason"] == "glossary"), "{j}");
    assert_eq!(items[0]["id"], format!("t:{t1}"), "{j}");
    let text = j["text"].as_str().unwrap();
    assert!(
        text.contains(&format!(
            "ZeroAIMemory: термин «Выжимка» — Определение: Выжимка — t:{t1}"
        )),
        "{text}"
    );
    // второй промпт той же сессии: показанные термины молчат, остаётся ещё не показанный
    let j2 = recall(&a, "s-gl", body.clone()).await;
    let ids2: Vec<&str> = j2["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids2.len(), 1, "{j2}");
    assert!(!items.iter().any(|i| i["id"] == ids2[0]), "{j2}");
    // третий — тишина
    let j3 = recall(&a, "s-gl", body.clone()).await;
    assert_eq!(j3["items"], json!([]), "{j3}");
    assert_eq!(j3["text"], "", "{j3}");
    // другая сессия — снова
    let j4 = recall(&a, "s-other", body).await;
    assert_eq!(j4["items"].as_array().unwrap().len(), 2, "{j4}");
    // слово вне первых 512 символов не подсказывается
    let far = format!("{} summary", "слово ".repeat(100));
    let j5 = recall(&a, "s-far", json!({"event": "prompt", "prompt": far})).await;
    assert_eq!(j5["items"], json!([]), "{j5}");
}

/// Дайджест `session_start`: строка `Словарь: N терминов — memory_search kinds=[term]`, если
/// N > 0.
#[tokio::test]
async fn digest_glossary_line() {
    let a = api("gs-digest", recall_cfg).await;
    let j = recall(&a, "s-d0", json!({"event": "session_start"})).await;
    assert!(!j["text"].as_str().unwrap().contains("Словарь:"), "{j}");
    term(&a, "Выжимка", &["summary"]).await;
    term(&a, "Чанк", &[]).await;
    let j = recall(&a, "s-d1", json!({"event": "session_start"})).await;
    let text = j["text"].as_str().unwrap();
    assert!(
        text.contains("Словарь: 2 терминов — memory_search kinds=[term]"),
        "{text}"
    );
    // строка не подсказка записи: в items её нет
    assert!(
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["reason"] != "glossary"),
        "{j}"
    );
}

/// Кэш словаря сбрасывается записью `term` (версия растёт), а без записи не пересобирается;
/// предупреждения `term_alias` берут тот же кэш, что и канал `alias`.
#[tokio::test]
async fn glossary_cache_invalidates_on_term_write() {
    let a = api("gs-cache-invalidate", |_| {}).await;
    term(&a, "Выжимка", &["summary"]).await;
    let g = &a.st.glossary;
    let b0 = g.builds();
    let j = search(&a, json!({"query": "summary"})).await;
    assert_eq!(j["expanded"].as_array().unwrap().len(), 1, "{j}");
    let b1 = g.builds();
    assert_eq!(b1, b0 + 1, "первый поиск строит индекс");
    search(&a, json!({"query": "summary"})).await;
    assert_eq!(g.builds(), b1, "повторный поиск берёт кэш");
    // предупреждение о псевдониме при записи заметки — из того же кэша, без пересборки
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "Заметки/А", "kind": "note",
                        "content": "# Про summary\n\nНужен summary.\n"}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["warnings"],
        json!(["term_alias: «summary» → «Выжимка»"])
    );
    assert_eq!(g.builds(), b1, "предупреждения берут тот же кэш");
    // новый термин — версия растёт, индекс пересобирается и расширяет запрос
    term(&a, "Чанк", &["chunk"]).await;
    let j = search(&a, json!({"query": "chunk"})).await;
    assert_eq!(j["expanded"][0]["term"], "Чанк", "{j}");
    assert_eq!(g.builds(), b1 + 1);
    // отзыв термина убирает расширение
    let tid = term(&a, "Кэш", &["cache"]).await;
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{tid}/retract"),
            Some(json!({"reason": "тест"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = search(&a, json!({"query": "cache"})).await;
    assert_eq!(j["expanded"], json!([]), "{j}");
}

/// Открытый вопрос не попадает в выдачу без явного `kinds` ни по умолчанию, ни через
/// `include_history`; явный `kinds: [question]` его находит.
#[tokio::test]
async fn open_question_only_with_explicit_kinds() {
    let a = api("gs-open-question", |_| {}).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let kn = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == zam_store::SpaceKind::Knowledge)
        .unwrap()
        .id;
    let (q, _) = zam_store::create_entry(
        &db,
        zam_store::NewEntry {
            refs: Vec::new(),
            space_id: kn,
            kind: zam_store::EntryKind::Question,
            title: "Кэшевание выжимок".into(),
            body: "Нужно ли кэшевание выжимок для общих решений.".into(),
            data: json!({}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &zam_store::Attribution {
            author_kind: zam_store::AuthorKind::System,
            token_id: None,
            session_id: None,
            machine: None,
            message: None,
            user_id: None,
            area: None,
            solution: None,
        },
    )
    .await
    .unwrap();
    let qid = q.id.to_string();
    let has = |j: &Value| {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["entry_id"] == qid.as_str())
    };
    let j = search(&a, json!({"query": "кэшевание"})).await;
    assert!(!has(&j), "по умолчанию: {j}");
    let j = search(&a, json!({"query": "кэшевание", "include_history": true})).await;
    assert!(!has(&j), "include_history без kinds: {j}");
    let j = search(
        &a,
        json!({"query": "кэшевание", "filters": {"kinds": ["question"]}}),
    )
    .await;
    assert!(has(&j), "явный kinds: {j}");
    let j = search(
        &a,
        json!({"query": "кэшевание", "include_history": true, "filters": {"kinds": ["question"]}}),
    )
    .await;
    assert!(has(&j), "include_history с kinds: {j}");
}
