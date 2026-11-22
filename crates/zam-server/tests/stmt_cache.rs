//! Размер кэша подготовленных запросов SQLite (M7.1 §4.8, задача 9; ревью, Minor 1).
//!
//! Одно соединение базы выполняет на горячих путях сколько-то запросов из кэша `prepare_cached`.
//! Горячие пути — `/recall(prompt)`, `/search`, `/read` и частый хук `/recall(post_tool)` (пул
//! читателей у них общий). Число запросов должно быть меньше `zam_store::db::STATEMENT_CACHE` с
//! запасом не меньше 1,5×, и так же — вместе с дайджестом `session_start`. Иначе LRU вытесняет
//! запросы по кругу, и они готовятся заново на каждом вызове (корень роста M6.6,
//! `i:01M80RNR5QEHWADSZXM69SRNBW`).
//!
//! Мир — как у `zam-cli/tests/m6_acceptance.rs::acceptance_hook_budgets` (30 заметок, 10 insight,
//! 50 идей человека). Добавлены две открытые задачи и термин, чтобы шли каналы задач и словаря.
//!
//! Учёт — `zam_store::test_hooks::sql_texts_*` (фича `test-hooks`, `trace_v2`).
//! - Запрос в кэше — тот, чей подготовленный запрос запускался повторно
//!   (`SQLITE_STMTSTATUS_RUN` > 0 в начале запуска).
//! - Разовые (`query_row`, `execute_batch`) и внутренние запросы SQLite и FTS5 не в счёт.
//!
//! Учёт общий для процесса, поэтому тест — один в своём двоичном файле.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, api, id};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";

async fn post(a: &Api, uri: &str, session: &str, body: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            uri,
            Some(body),
            &[("x-zam-session", session)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    r.json()
}

async fn created(a: &Api, uri: &str, body: Value) -> String {
    let r = a.t(Method::POST, uri, Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{uri}: {}", r.text());
    id(&r.json())
}

const PROMPTS: [&str; 5] = [
    "как улучшить кэш и деплой, шаг 3",
    "почему поиск медленный после миграции",
    "настрой логирование для деплоя",
    "миграции базы падают на шаге 7",
    "добавь проверку кэша",
];

/// Один круг запросов: на каждый промпт — `prompt` новой сессией и той же сессией второй раз
/// (показанное и строка очереди уже отмечены), поиск `brief` и `chunks`, чтение заметки и insight,
/// подсказка к ошибке `post_tool`.
async fn round(a: &Api, tag: &str, note: &str, insight: &str, digest: bool) {
    for (i, text) in PROMPTS.iter().enumerate() {
        if digest {
            let body = json!({"event": "session_start", "solution": G, "source": "startup"});
            post(a, "/api/v1/recall", &format!("{tag}-{i}"), body).await;
        }
        let body = json!({"event": "prompt", "solution": G, "prompt": text});
        post(a, "/api/v1/recall", &format!("{tag}-{i}"), body.clone()).await;
        post(a, "/api/v1/recall", &format!("{tag}-{i}"), body).await;
        for mode in ["brief", "chunks"] {
            let body = json!({"query": text, "mode": mode, "top_k": 8,
                              "scope": {"solution": G, "include_knowledge": true}});
            post(a, "/api/v1/search", &format!("{tag}-{i}"), body).await;
        }
        let body = json!({"items": [{"id": note}, {"id": format!("i:{insight}")}]});
        post(a, "/api/v1/read", &format!("{tag}-{i}"), body).await;
        // `post_tool` — тот же пул читателей и частый хук: его запросы делят кэш с `prompt`
        let err = format!("Exit code 1\nerror: {text}: проверь шаг {i}");
        let body = json!({"event": "post_tool", "solution": G, "tool": "Bash", "error": err});
        post(a, "/api/v1/recall", &format!("{tag}-{i}"), body).await;
    }
}

#[tokio::test]
async fn statement_cache_fits_hot_paths() {
    let a = api("stmt-cache", |c| c.recall.timeout_ms = 30_000).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G, "name": "Кэш запросов"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let topics = ["кэш", "деплой", "поиск", "логирование", "миграции"];
    let mut note = String::new();
    for i in 0..30 {
        let t = topics[i % 5];
        let n = id(&a
            .note(
                "knowledge",
                &format!("p95/{t}-{i}"),
                &format!("# {t} {i}\n\nЗаметка про {t}: шаг {i}, настройка и проверка.\n"),
            )
            .await);
        if i == 0 {
            note = n;
        }
    }
    let sol = format!("/api/v1/spaces/{G}/entries");
    let mut insight = String::new();
    for i in 0..10 {
        let t = topics[i % 5];
        let e = created(
            &a,
            &sol,
            json!({"kind": "insight", "title": format!("Ловушка {t} {i}"),
                   "body": format!("При работе с {t} проверь шаг {i}."),
                   "data": {"kind": "gotcha"}}),
        )
        .await;
        if i == 0 {
            insight = e;
        }
    }
    for i in 0..50 {
        created(
            &a,
            &sol,
            json!({"kind": "idea", "title": format!("Идея {i}: улучшить {}", topics[i % 5]),
                   "body": "Замысел на будущее.", "data": {"origin": "human"}}),
        )
        .await;
    }
    for title in ["Миграции базы на шаге 7", "Логирование деплоя"]
    {
        created(
            &a,
            &sol,
            json!({"kind": "task", "title": title, "body": "Шаги."}),
        )
        .await;
    }
    created(
        &a,
        "/api/v1/spaces/knowledge/entries",
        json!({"kind": "term", "title": "Деплой", "body": "",
               "data": {"definition": "Выкладка сборки на сервер."}}),
    )
    .await;
    // первый круг — вне учёта: разовые чтения при первом обращении (словарь, настройки)
    round(&a, "warm", &note, &insight, true).await;
    let cache = zam_store::db::STATEMENT_CACHE;
    // горячие пути — порог: запас не меньше 1,5×
    let hot = measure(&a, "hot", &note, &insight, false).await;
    assert!(
        hot * 3 <= cache * 2,
        "запросов в кэше одного соединения {hot}, кэш {cache}: запас меньше 1,5× — подними \
         zam_store::db::STATEMENT_CACHE"
    );
    // с дайджестом `session_start` на каждый промпт (запросы дайджеста — раз за сессию, но пул
    // читателей общий): тот же запас
    let all = measure(&a, "digest", &note, &insight, true).await;
    assert!(
        all * 3 <= cache * 2,
        "с дайджестом запросов в кэше одного соединения {all}, кэш {cache}: запас меньше 1,5×"
    );
}

/// Место в кэше `prepare_cached` соединения: повторно запущенные запросы, кроме внутренних SQLite
/// и FTS5 (их тексты — с `'main'.`); разовые (`query_row`, `execute_batch` транзакций) в кэш не
/// идут.
fn cached(t: &[zam_store::test_hooks::SqlText]) -> usize {
    t.iter()
        .filter(|x| x.reused && !x.sql.contains("'main'."))
        .count()
}

/// Три круга [`round`] под учётом SQL: наибольшее по соединениям число запросов в кэше
/// ([`cached`]); числа по соединениям и тексты наибольшего — в вывод (`ZAM_INFO`, для журнала).
async fn measure(a: &Api, tag: &str, note: &str, insight: &str, digest: bool) -> usize {
    zam_store::test_hooks::sql_texts_start();
    for k in 0..3 {
        round(a, &format!("{tag}{k}"), note, insight, digest).await;
    }
    let conns = zam_store::test_hooks::sql_texts_stop();
    for (path, texts) in &conns {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        eprintln!(
            "ZAM_INFO stmt_cache {tag} {name}: в кэше {}, всего разных текстов SQL {}",
            cached(texts),
            texts.len()
        );
    }
    let Some((path, texts)) = conns.iter().max_by_key(|(_, t)| cached(t)) else {
        panic!("{tag}: учёт SQL пуст");
    };
    eprintln!("ZAM_INFO stmt_cache {tag} наибольшее — {}:", path.display());
    for t in texts {
        let sql: String = t.sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let mark = if t.reused { "кэш" } else { "раз" };
        eprintln!("  {mark} {}", sql.chars().take(160).collect::<String>());
    }
    let most = cached(texts);
    eprintln!("ZAM_INFO stmt_cache {tag}: наибольшее соединение — {most} запросов в кэше");
    most
}
