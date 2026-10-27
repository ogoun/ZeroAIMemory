//! Проактивное вспоминание `POST /recall` (§10.3; задача 8 M4): дайджест старта сессии,
//! подсказки к запросу и к ошибке инструмента, без повторов в сессии, учёт показов, решение без
//! доступа. Сервер без моделей (поиск — FTS); `recall.timeout_ms` поднят, чтобы отладочная
//! сборка на HDD не обрезала сбор (проверка тайм-аута — отдельным тестом).
//!
//! Пользователи общего решения: A — владелец, B — участник.
mod common;

use std::time::Instant;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e52";
const RECALL: &str = "/api/v1/recall";

fn roomy(c: &mut ServerConfig) {
    c.recall.timeout_ms = 30_000;
}

async fn server(name: &str) -> Api {
    let a = api(&format!("recall-{name}"), roomy).await;
    allow_rules(&a).await;
    a
}

/// Запись агентов в Rules (`rules.agent_write`): тесты пишут правила токеном.
async fn allow_rules(a: &Api) {
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// `POST /recall` токеном `tok` в сессии `session` (`None` — без сессии).
async fn recall(a: &Api, tok: &str, session: Option<&str>, body: Value) -> Resp {
    let h: Vec<(&str, &str)> = session.map(|s| ("x-zam-session", s)).into_iter().collect();
    a.tok_h(tok, Method::POST, RECALL, Some(body), &h).await
}

async fn ok(a: &Api, tok: &str, session: Option<&str>, body: Value) -> Value {
    let r = recall(a, tok, session, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Запись журнала в пространстве `space` токеном `tok`; id.
async fn entry(a: &Api, tok: &str, space: &str, body: Value) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn insight(a: &Api, tok: &str, space: &str, title: &str, body: &str, kind: &str) -> String {
    entry(
        a,
        tok,
        space,
        json!({"kind": "insight", "title": title, "body": body, "data": {"kind": kind}}),
    )
    .await
}

async fn note_as(a: &Api, tok: &str, space: &str, path: &str, content: &str) -> String {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

async fn register(a: &Api, tok: &str) -> Resp {
    a.tok(
        tok,
        Method::POST,
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": G, "name": "Общее"})),
    )
    .await
}

fn ids(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// Каждая строка подсказки начинается с `ZeroAIMemory:` и несёт свою ссылку.
fn assert_marked(j: &Value) {
    let text = j["text"].as_str().unwrap();
    for id in ids(j) {
        let line = text
            .lines()
            .find(|l| l.ends_with(&format!("— {id}")))
            .unwrap_or_else(|| panic!("нет строки {id}: {text}"));
        assert!(line.starts_with("ZeroAIMemory: "), "{line}");
    }
}

async fn shown(a: &Api, owner: &str, kind: TargetKind, id: &str) -> zam_store::UsageCounts {
    let db = a.st.users.get(owner.parse::<Id>().unwrap()).await.unwrap();
    zam_store::usage_counts(
        &db,
        TargetRef {
            kind,
            id: id.parse().unwrap(),
        },
    )
    .await
    .unwrap()
}

/// Дайджест не превышает `recall.digest_tokens` (1500) и `budget_tokens` запроса, даже когда
/// кандидатов на порядок больше; `hooks` — все типы включены (настроек пользователя ещё нет).
#[tokio::test]
async fn session_start_digest_within_budget() {
    let a = server("budget").await;
    let section = "Длинное пояснение правила с подробностями и примерами. ".repeat(30);
    for i in 0..12 {
        a.note(
            "rules",
            &format!("правило-{i}"),
            &format!("# Правило {i}\n\n## Раздел А\n\n{section}\n\n## Раздел Б\n\n{section}\n"),
        )
        .await;
    }
    for i in 0..15 {
        insight(
            &a,
            &a.token,
            "knowledge",
            &format!("Ловушка номер {i} с длинным заголовком про сборку и окружение"),
            "текст",
            "gotcha",
        )
        .await;
    }
    let j = ok(&a, &a.token, Some("s1"), json!({"event": "session_start"})).await;
    let tokens = j["tokens"].as_u64().unwrap();
    assert!(tokens > 0 && tokens <= 1500, "{tokens}");
    let text = j["text"].as_str().unwrap();
    assert_eq!(zam_store::count_tokens(text) as u64, tokens);
    assert!(!j["items"].as_array().unwrap().is_empty());
    assert_marked(&j);
    assert_eq!(
        j["hooks"],
        json!({"session_start": true, "prompt": true, "pre_tool": true, "post_tool": true,
               "checkpoint": true})
    );
    for k in ["collect_ms", "record_ms", "total_ms"] {
        assert!(j["timings"][k].is_number(), "{k}: {j}");
    }
    // бюджет запроса меньше умолчания
    let j = ok(
        &a,
        &a.token,
        Some("s2"),
        json!({"event": "session_start", "budget_tokens": 120}),
    )
    .await;
    let tokens = j["tokens"].as_u64().unwrap();
    assert!(tokens > 0 && tokens <= 120, "{tokens}: {}", j["text"]);
    // некорректный бюджет и событие — 400
    for bad in [
        json!({"event": "session_start", "budget_tokens": 0}),
        json!({"event": "pre_tool"}),
        json!({"event": "session_start", "solution": "не-guid"}),
        json!({"event": "session_start", "session_id": ""}),
    ] {
        let r = recall(&a, &a.token, None, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
}

/// Состав дайджеста: Rules (L1), корневые узлы решения с L0, действующие gotcha/failure решения
/// и Knowledge (без `discovery` и записей `revisit` среди них), `revisit`, последние 5 записей
/// changelog, ожидающие запросы доступа владельцу, напоминание об устаревшем блоке.
#[tokio::test]
async fn digest_includes_rules_gotchas_revisit_changelog() {
    let a = server("digest").await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (_, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &a_tok).await.status, StatusCode::CREATED);
    let rule = note_as(
        &a,
        &a_tok,
        "rules",
        "стиль",
        "# Стиль\n\n## Коммиты\n\nКоммиты пишем по-русски.\n",
    )
    .await;
    let root = note_as(
        &a,
        &a_tok,
        G,
        "README",
        "---\nl0: Сервис расчёта доставки\n---\n# README\n\nСервис расчёта доставки.\n",
    )
    .await;
    let gotcha = insight(
        &a,
        &a_tok,
        G,
        "LF в bash-скриптах",
        "CRLF ломает bash",
        "gotcha",
    )
    .await;
    let failure = insight(&a, &a_tok, "knowledge", "Падение сборки", "x", "failure").await;
    let discovery = insight(&a, &a_tok, G, "Находка", "x", "discovery").await;
    let revisit = entry(
        &a,
        &a_tok,
        G,
        json!({"kind": "insight", "title": "Проверить таймауты", "body": "x",
               "data": {"kind": "decision"}, "status": "revisit"}),
    )
    .await;
    let mut changes = Vec::new();
    for i in 0..6 {
        changes.push(
            entry(
                &a,
                &a_tok,
                G,
                json!({"kind": "changelog", "title": format!("Изменение {i}"), "body": "x",
                       "data": {"kind": "fix"}}),
            )
            .await,
        );
    }
    // запрос доступа Бориса ждёт ответа Анны
    assert_eq!(register(&a, &b_tok).await.status, StatusCode::ACCEPTED);

    let j = ok(
        &a,
        &a_tok,
        Some("d1"),
        json!({"event": "session_start", "solution": G, "agent_block_version": 1}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    let got = ids(&j);
    assert!(got.contains(&format!("n:{rule}")), "{text}");
    assert!(got.contains(&format!("n:{root}")), "{text}");
    assert!(text.contains("Коммиты"), "L1 правила: {text}");
    assert!(
        text.contains("Сервис расчёта доставки"),
        "L0 решения: {text}"
    );
    assert!(got.contains(&format!("i:{gotcha}")), "{text}");
    assert!(got.contains(&format!("i:{failure}")), "{text}");
    assert!(!got.contains(&format!("i:{discovery}")), "{text}");
    assert!(got.contains(&format!("i:{revisit}")), "{text}");
    // последние 5 изменений
    for c in &changes[1..] {
        assert!(got.contains(&format!("c:{c}")), "{c}: {text}");
    }
    assert!(!got.contains(&format!("c:{}", changes[0])), "{text}");
    assert!(
        text.contains("ZeroAIMemory: LF в bash-скриптах [gotcha, hypothesis] — i:"),
        "{text}"
    );
    assert!(
        text.contains(
            "ZeroAIMemory: Блок ZeroAIMemory устарел (v1 → v2): обнови через `zam init` или WebUI"
        ),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("ZeroAIMemory: запросы доступа") && l.contains(": 1 ")),
        "{text}"
    );
    for h in ["### Правила", "### Решение", "### Ловушки и сбои"] {
        assert!(text.contains(h), "{h}: {text}");
    }
    assert_marked(&j);
    let kinds: Vec<(&str, &str)> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["kind"].as_str().unwrap(), i["reason"].as_str().unwrap()))
        .collect();
    for want in [
        ("note", "rules"),
        ("note", "solution"),
        ("insight", "gotcha"),
        ("insight", "failure"),
        ("insight", "revisit"),
        ("changelog", "changelog"),
    ] {
        assert!(kinds.contains(&want), "{want:?}: {kinds:?}");
    }
    // актуальный блок — без напоминания; без решения — только Knowledge и Rules
    let j = ok(
        &a,
        &a_tok,
        Some("d2"),
        json!({"event": "session_start", "agent_block_version": 2}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(!text.contains("устарел"), "{text}");
    let got = ids(&j);
    assert!(got.contains(&format!("i:{failure}")), "{text}");
    assert!(!got.contains(&format!("i:{gotcha}")), "{text}");
    // usage — показ в базе владельца с источником session_start
    let c = shown(&a, &a_id, TargetKind::Entry, &gotcha).await;
    assert_eq!(c.shown, 1);
}

/// `prompt`: 0–3 подсказки — insight и заметки Rules (не заметки Knowledge) с баллом не ниже
/// `recall.prompt_min_score`; нерелевантный запрос — пустой `text`.
#[tokio::test]
async fn prompt_returns_0_to_3_above_threshold() {
    let a = server("prompt").await;
    let mut ins = Vec::new();
    for i in 0..5 {
        ins.push(
            insight(
                &a,
                &a.token,
                "knowledge",
                &format!("Квазар и пульсар, заметка {i}"),
                &format!("Квазар светит ярче галактики, случай {i}."),
                "discovery",
            )
            .await,
        );
    }
    let k_note = a
        .note(
            "knowledge",
            "астро",
            "# Астро\n\nКвазар — ядро галактики.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let j = ok(
        &a,
        &a.token,
        Some("p1"),
        json!({"event": "prompt", "prompt": "почему квазар светит"}),
    )
    .await;
    let got = ids(&j);
    assert!((1..=3).contains(&got.len()), "{j}");
    assert!(!got.contains(&format!("n:{k_note}")), "{j}");
    assert!(got.iter().all(|i| i.starts_with("i:")), "{j}");
    assert!(j.get("hooks").is_none());
    assert_marked(&j);
    // Rules ищутся
    let rule = a
        .note(
            "rules",
            "телескоп",
            "# Телескоп\n\nТелескоп калибруем перед каждой съёмкой.\n",
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let j = ok(
        &a,
        &a.token,
        Some("p2"),
        json!({"event": "prompt", "prompt": "калибровка телескоп"}),
    )
    .await;
    assert_eq!(ids(&j), [format!("n:{rule}")], "{j}");
    assert!(
        j["text"]
            .as_str()
            .unwrap()
            .starts_with("ZeroAIMemory: правило телескоп › Телескоп — n:"),
        "{j}"
    );
    // ничего похожего — тишина
    let j = ok(
        &a,
        &a.token,
        Some("p3"),
        json!({"event": "prompt", "prompt": "рецепт борща со сметаной"}),
    )
    .await;
    assert_eq!(j["text"], "", "{j}");
    assert!(j["items"].as_array().unwrap().is_empty());
    // порог выше любого балла — тишина и для релевантного запроса
    let b = api("recall-prompt-strict", |c| {
        roomy(c);
        c.recall.prompt_min_score = 1.0;
    })
    .await;
    insight(
        &b,
        &b.token,
        "knowledge",
        "Квазар",
        "Квазар светит.",
        "discovery",
    )
    .await;
    insight(
        &b,
        &b.token,
        "knowledge",
        "Квазар второй",
        "Квазар и квазар.",
        "discovery",
    )
    .await;
    let j = ok(
        &b,
        &b.token,
        None,
        json!({"event": "prompt", "prompt": "квазар"}),
    )
    .await;
    assert!(ids(&j).len() <= 1, "{j}");
}

/// `post_tool`: совпадение `error_regex` (с учётом `tools`) и FTS по нормализованной ошибке;
/// строка «эта ошибка уже разбиралась»; случайное общее слово подсказку не поднимает.
#[tokio::test]
async fn post_tool_matches_error_regex_and_fts() {
    let a = server("post-tool").await;
    let crlf = entry(
        &a,
        &a.token,
        "knowledge",
        json!({"kind": "insight", "title": "CRLF в скриптах", "body": "Сохраняй *.sh с LF.",
               "data": {"kind": "gotcha"},
               "triggers": [{"type": "error_regex", "pattern": r"bad interpreter|\^M"}]}),
    )
    .await;
    let only_write = entry(
        &a,
        &a.token,
        "knowledge",
        json!({"kind": "insight", "title": "Только для Write", "body": "x",
               "data": {"kind": "gotcha"},
               "triggers": [{"type": "error_regex", "pattern": "bad interpreter",
                             "tools": ["Write"]}]}),
    )
    .await;
    let docker = insight(
        &a,
        &a.token,
        "knowledge",
        "Нет доступа к docker",
        "permission denied при подключении к docker: добавь пользователя в группу docker",
        "failure",
    )
    .await;
    let noise = insight(&a, &a.token, "knowledge", "Сокеты", "unix сокет", "pattern").await;

    let err = "/bin/bash: ./deploy.sh: /bin/bash^M: bad interpreter: No such file or directory";
    let j = ok(
        &a,
        &a.token,
        Some("t1"),
        json!({"event": "post_tool", "tool": "Bash", "error": err}),
    )
    .await;
    let got = ids(&j);
    assert!(got.contains(&format!("i:{crlf}")), "{j}");
    assert!(!got.contains(&format!("i:{only_write}")), "{j}");
    assert_eq!(j["items"][0]["reason"], "error_regex");
    assert!(
        j["text"].as_str().unwrap().starts_with(&format!(
            "ZeroAIMemory: эта ошибка уже разбиралась: CRLF в скриптах [gotcha, hypothesis] — i:{crlf}"
        )),
        "{j}"
    );
    // без `tool` триггер с `tools` тоже срабатывает
    let j = ok(
        &a,
        &a.token,
        Some("t1"),
        json!({"event": "post_tool", "error": err}),
    )
    .await;
    assert!(ids(&j).contains(&format!("i:{only_write}")), "{j}");

    let err = "dial unix /var/run/docker.sock: connect: permission denied (code 13)";
    let j = ok(
        &a,
        &a.token,
        Some("t1"),
        json!({"event": "post_tool", "tool": "Bash", "error": err}),
    )
    .await;
    let got = ids(&j);
    assert_eq!(got, [format!("i:{docker}")], "{j}");
    assert_eq!(j["items"][0]["reason"], "error_fts");
    assert!(!got.contains(&format!("i:{noise}")));
    assert_marked(&j);
    // пустая ошибка — тишина
    let j = ok(
        &a,
        &a.token,
        None,
        json!({"event": "post_tool", "error": "  "}),
    )
    .await;
    assert_eq!(j["text"], "");
}

/// Подсказка `session_start`/`prompt` показывается сессии один раз (в другой сессии — снова),
/// `post_tool` — при каждой ошибке; без сессии повторы не учитываются.
#[tokio::test]
async fn recall_no_repeat_in_session_except_post_tool() {
    let a = server("no-repeat").await;
    let g = entry(
        &a,
        &a.token,
        "knowledge",
        json!({"kind": "insight", "title": "Квазар коварен", "body": "Квазар светит ярко.",
               "data": {"kind": "gotcha"},
               "triggers": [{"type": "error_regex", "pattern": "quasar failed"}]}),
    )
    .await;
    let me = format!("i:{g}");
    let start = json!({"event": "session_start"});
    let j = ok(&a, &a.token, Some("s1"), start.clone()).await;
    assert!(ids(&j).contains(&me), "{j}");
    // дайджест повторяется (после `/compact` и `resume` — тот же session_id)
    let mut again = start.clone();
    again["source"] = json!("compact");
    let j = ok(&a, &a.token, Some("s1"), again).await;
    assert!(ids(&j).contains(&me), "{j}");
    // а запрос в той же сессии показанное дайджестом не повторяет
    let prompt = json!({"event": "prompt", "prompt": "квазар"});
    let j = ok(&a, &a.token, Some("s1"), prompt.clone()).await;
    assert!(!ids(&j).contains(&me), "{j}");
    // другая сессия: запрос показывает, повторный запрос — уже нет
    let j = ok(&a, &a.token, Some("s2"), prompt.clone()).await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    let j = ok(&a, &a.token, Some("s2"), prompt.clone()).await;
    assert_eq!(j["text"], "", "{j}");
    // сессия из тела сильнее заголовка
    let mut body = prompt.clone();
    body["session_id"] = json!("s3");
    let j = ok(&a, &a.token, Some("s2"), body).await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    // ошибка — при каждой
    let err = json!({"event": "post_tool", "error": "quasar failed at 0x1F"});
    for _ in 0..2 {
        let j = ok(&a, &a.token, Some("s1"), err.clone()).await;
        assert_eq!(ids(&j), [me.as_str()], "{j}");
    }
    // без сессии — без учёта повторов
    for _ in 0..2 {
        let j = ok(&a, &a.token, None, prompt.clone()).await;
        assert_eq!(ids(&j), [me.as_str()], "{j}");
    }
    // сессия создана неявно, запись в память не засчитана
    let s = a.t(Method::GET, "/api/v1/sessions/s1", None).await.json();
    assert_eq!(s["memory_writes"], 0, "{s}");
}

/// Каждый показ — `usage(recall_shown, <событие>)` в базе цели: у общего решения — в базе
/// владельца с пользователем-участником и его сессией.
#[tokio::test]
async fn recall_records_usage() {
    let a = server("usage").await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &a_tok).await.status, StatusCode::CREATED);
    assert_eq!(register(&a, &b_tok).await.status, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({"access": "ro"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let e = entry(
        &a,
        &a_tok,
        G,
        json!({"kind": "insight", "title": "Квазар в решении", "body": "Квазар.",
               "data": {"kind": "gotcha"},
               "triggers": [{"type": "error_regex", "pattern": "quasar"}]}),
    )
    .await;
    let me = format!("i:{e}");
    let j = ok(
        &a,
        &b_tok,
        Some("b1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    assert!(ids(&j).contains(&me), "{j}");
    let j = ok(
        &a,
        &b_tok,
        Some("b1"),
        json!({"event": "post_tool", "solution": G, "error": "quasar"}),
    )
    .await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    let j = ok(
        &a,
        &a_tok,
        Some("a1"),
        json!({"event": "prompt", "solution": G, "prompt": "квазар"}),
    )
    .await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    assert_eq!(shown(&a, &a_id, TargetKind::Entry, &e).await.shown, 3);
    assert_eq!(shown(&a, &b_id, TargetKind::Entry, &e).await.shown, 0);
    let db = a.st.users.get(a_id.parse::<Id>().unwrap()).await.unwrap();
    let rows: Vec<(String, Option<String>, String)> = db
        .read(|c| {
            let mut st = c.prepare(
                "SELECT user_id, session_id, source FROM usage WHERE event = 'recall_shown'
                 ORDER BY id",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        rows,
        [
            (b_id.clone(), Some("b1".into()), "session_start".into()),
            (b_id.clone(), Some("b1".into()), "post_tool".into()),
            (a_id.clone(), Some("a1".into()), "prompt".into()),
        ]
    );
    // показ участнику отмечен в его базе (сессия — его), чужой Knowledge не подмешан
    let k = insight(
        &a,
        &a_tok,
        "knowledge",
        "Квазар Анны",
        "Квазар личный.",
        "gotcha",
    )
    .await;
    let j = ok(
        &a,
        &b_tok,
        Some("b2"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    assert!(!ids(&j).contains(&format!("i:{k}")), "{j}");
    let j = ok(
        &a,
        &b_tok,
        Some("b2"),
        json!({"event": "prompt", "solution": G, "prompt": "квазар личный"}),
    )
    .await;
    assert!(!ids(&j).contains(&format!("i:{k}")), "{j}");
}

/// Решение без доступа (отозван, ожидает) исключается с `warnings`, как в поиске; остаётся
/// личная память участника.
#[tokio::test]
async fn recall_excludes_revoked_solution_with_warning() {
    let a = server("revoked").await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let (_, c_tok) = a.new_user("Вера").await;
    assert_eq!(register(&a, &a_tok).await.status, StatusCode::CREATED);
    assert_eq!(register(&a, &b_tok).await.status, StatusCode::ACCEPTED);
    assert_eq!(register(&a, &c_tok).await.status, StatusCode::ACCEPTED);
    for step in ["approve", "revoke"] {
        let r = a
            .tok(
                &a_tok,
                Method::POST,
                &format!("/api/v1/solutions/{G}/access/{b_id}/{step}"),
                Some(json!({})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{step}: {}", r.text());
    }
    let sol = entry(
        &a,
        &a_tok,
        G,
        json!({"kind": "insight", "title": "Квазар решения", "body": "Квазар.",
               "data": {"kind": "gotcha"},
               "triggers": [{"type": "error_regex", "pattern": "quasar"}]}),
    )
    .await;
    let own = insight(
        &a,
        &b_tok,
        "knowledge",
        "Квазар Бориса",
        "Квазар мой.",
        "gotcha",
    )
    .await;
    for (event, extra) in [
        ("session_start", json!({})),
        ("prompt", json!({"prompt": "квазар"})),
        ("post_tool", json!({"error": "quasar мой квазар"})),
    ] {
        let mut body = json!({"event": event, "solution": G});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let j = ok(&a, &b_tok, Some(&format!("r-{event}")), body.clone()).await;
        assert!(!ids(&j).contains(&format!("i:{sol}")), "{event}: {j}");
        assert!(
            j["warnings"]
                .as_array()
                .unwrap()
                .contains(&json!("solution access revoked")),
            "{event}: {j}"
        );
        if event != "post_tool" {
            assert!(ids(&j).contains(&format!("i:{own}")), "{event}: {j}");
        }
        // ожидающий доступа
        let j = ok(&a, &c_tok, None, body).await;
        assert!(!ids(&j).contains(&format!("i:{sol}")), "{event}: {j}");
        assert!(
            j["warnings"]
                .as_array()
                .unwrap()
                .contains(&json!("solution access pending")),
            "{event}: {j}"
        );
    }
    // решение из заголовка — так же
    let r = a
        .tok_h(
            &b_tok,
            Method::POST,
            RECALL,
            Some(json!({"event": "session_start"})),
            &[("x-zam-solution", G)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.json()["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("solution access revoked"))
    );
}

/// Срок сбора (`recall.timeout_ms` минус резерв на учёт) вышел посреди Rules — отдаётся
/// собранное (часть правил) и `warnings: ["recall timeout"]`, не ошибка. Чтение правил
/// задерживается тестовой меткой (фича `test-hooks`).
#[tokio::test]
async fn recall_timeout_returns_partial_with_warning() {
    let a = api("recall-timeout", |c| c.recall.timeout_ms = 600).await;
    allow_rules(&a).await;
    let mark = zam_server::api::recall::TEST_SLOW_RULE_MARK;
    for i in 0..30 {
        a.note(
            "rules",
            &format!("r{i:02}"),
            &format!("# Правило {i}\n\nТекст {mark}.\n"),
        )
        .await;
    }
    let j = ok(&a, &a.token, Some("t"), json!({"event": "session_start"})).await;
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("recall timeout")),
        "{j}"
    );
    // часть правил собрана (20 медленных по 40 мс в 560 мс не укладываются)
    let n = j["items"].as_array().unwrap().len();
    assert!((1..20).contains(&n), "{n}: {j}");
    assert!(j["text"].as_str().unwrap().contains("### Правила"), "{j}");
}

/// `post_tool` участника общего решения не цепляет `error_regex` и FTS из Knowledge владельца —
/// только из решения и своего Knowledge.
#[tokio::test]
async fn post_tool_member_ignores_owner_knowledge() {
    let a = server("post-tool-member").await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    assert_eq!(register(&a, &a_tok).await.status, StatusCode::CREATED);
    assert_eq!(register(&a, &b_tok).await.status, StatusCode::ACCEPTED);
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let trig = |title: &str| {
        json!({"kind": "insight", "title": title, "body": "quasar overflow разобран",
               "data": {"kind": "gotcha"},
               "triggers": [{"type": "error_regex", "pattern": "quasar overflow"}]})
    };
    let owner_k = entry(&a, &a_tok, "knowledge", trig("Квазар Анны")).await;
    let sol = entry(&a, &a_tok, G, trig("Квазар решения")).await;
    let j = ok(
        &a,
        &b_tok,
        Some("m1"),
        json!({"event": "post_tool", "solution": G, "error": "fatal: quasar overflow"}),
    )
    .await;
    let got = ids(&j);
    assert!(got.contains(&format!("i:{sol}")), "{j}");
    assert!(!got.contains(&format!("i:{owner_k}")), "{j}");
    // владельцу — оба
    let j = ok(
        &a,
        &a_tok,
        None,
        json!({"event": "post_tool", "solution": G, "error": "fatal: quasar overflow"}),
    )
    .await;
    let got = ids(&j);
    assert!(
        got.contains(&format!("i:{sol}")) && got.contains(&format!("i:{owner_k}")),
        "{j}"
    );
}

/// Длинный stack trace (30+ слов) находит короткий insight по FTS: порог покрытия не выше 4
/// слов, совпадение — по границе слова.
#[tokio::test]
async fn post_tool_long_trace_finds_short_insight() {
    let a = server("post-tool-trace").await;
    let short = insight(
        &a,
        &a.token,
        "knowledge",
        "Пул соединений исчерпан",
        "connection pool exhausted: timeout waiting for idle connection — подними max_size",
        "failure",
    )
    .await;
    let decoy = insight(
        &a,
        &a.token,
        "knowledge",
        "Профили",
        "profile settings",
        "pattern",
    )
    .await;
    let trace = "thread main panicked at called Result unwrap on an Err value:         connection pool exhausted timeout waiting for idle connection after 30s         stack backtrace: core panicking panic_fmt core result unwrap_failed app db         acquire app handlers orders list tokio runtime task harness poll future         hyper server conn file profile";
    assert!(trace.split_whitespace().count() > 30);
    let j = ok(
        &a,
        &a.token,
        None,
        json!({"event": "post_tool", "error": trace}),
    )
    .await;
    let got = ids(&j);
    assert!(got.contains(&format!("i:{short}")), "{j}");
    assert!(!got.contains(&format!("i:{decoy}")), "{j}");
}

// ---- замер бюджета (§10.3, §17) ----

/// Корпус eval (`eval/corpus`, `eval/insights.toml`) в Knowledge и решении; запросы — из
/// `eval/queries.toml`.
async fn load_eval(a: &Api) -> Vec<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
    assert_eq!(register(a, &a.token).await.status, StatusCode::CREATED);
    let mut stack = vec![root.join("corpus")];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                let rel = p
                    .strip_prefix(root.join("corpus"))
                    .unwrap()
                    .with_extension("");
                let rel = rel.to_string_lossy().replace('\\', "/");
                let text = std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n");
                a.note("knowledge", &rel, &text).await;
                n += 1;
            }
        }
    }
    let ins: toml::Table = std::fs::read_to_string(root.join("insights.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let mut m = 0;
    for i in ins["insight"].as_array().unwrap() {
        let space = if i.get("scope").and_then(|v| v.as_str()) == Some("solution") {
            G
        } else {
            "knowledge"
        };
        let r = a
            .t(
                Method::POST,
                &format!("/api/v1/spaces/{space}/entries"),
                Some(json!({
                    "kind": "insight",
                    "title": i["title"].as_str().unwrap(),
                    "body": i["body"].as_str().unwrap(),
                    "data": {"kind": i["kind"].as_str().unwrap()},
                    "force": true,
                })),
            )
            .await;
        if r.status == StatusCode::CREATED {
            m += 1;
        }
    }
    let q: toml::Table = std::fs::read_to_string(root.join("queries.toml"))
        .unwrap()
        .parse()
        .unwrap();
    eprintln!("RECALL-BENCH корпус: {n} заметок, {m} insight");
    q["query"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["text"].as_str().unwrap().to_string())
        .collect()
}

fn p95(v: &mut [f64]) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)])
}

/// p95 `/recall` на корпусе eval (FTS-only): `session_start`, `prompt`, `post_tool` — по 60
/// вызовов, новая сессия на каждый. Итог — в журнал этапа. Запуск:
/// `cargo test -p zam-server --release --test recall -- --ignored --nocapture recall_p95`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn recall_p95_under_budget() {
    let a = api("recall-bench", |c| c.recall.timeout_ms = 60_000).await;
    let queries = load_eval(&a).await;
    let errors = [
        "/bin/bash^M: bad interpreter: No such file or directory",
        "error: linking with `link.exe` failed: exit code: 1181",
        "fatal: unable to access 'https://example.com/repo.git/': SSL certificate problem",
        "dial unix /var/run/docker.sock: connect: permission denied",
        "os error 3: The system cannot find the path specified",
    ];
    for (event, label) in [
        ("session_start", "session_start"),
        ("prompt", "prompt"),
        ("post_tool", "post_tool"),
    ] {
        let (mut wall, mut server) = (Vec::new(), Vec::new());
        for i in 0..60 {
            let body = match event {
                "session_start" => json!({"event": event, "solution": G}),
                "prompt" => json!({"event": event, "solution": G,
                                   "prompt": queries[i % queries.len()]}),
                _ => json!({"event": event, "solution": G, "error": errors[i % errors.len()]}),
            };
            let t = Instant::now();
            let j = ok(&a, &a.token, Some(&format!("bench-{event}-{i}")), body).await;
            wall.push(t.elapsed().as_secs_f64() * 1000.0);
            server.push(j["timings"]["total_ms"].as_f64().unwrap());
            assert!(
                !j["warnings"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("recall timeout"))
            );
        }
        let (w50, w95) = p95(&mut wall);
        let (s50, s95) = p95(&mut server);
        eprintln!(
            "RECALL-BENCH {label}: обработчик p50 {s50:.1} мс, p95 {s95:.1} мс; \
             с маршрутизацией p50 {w50:.1} мс, p95 {w95:.1} мс"
        );
        // бюджет сервера (§10.3): 200 мс
        assert!(s95 <= 200.0, "{label}: p95 {s95:.1} мс");
    }
}

/// `record: false` (задача 10, fix round 2): подсказки выдаются, но без `session_seen` и `usage` —
/// хук `session-start` с выключенным дайджестом берёт только `hooks`.
#[tokio::test]
async fn recall_record_false_skips_accounting() {
    let a = server("record-false").await;
    let (u_id, tok) = a.new_user("Вера").await;
    let e = insight(
        &a,
        &tok,
        "knowledge",
        "Квазар светит",
        "Квазар — ядро.",
        "gotcha",
    )
    .await;
    let me = format!("i:{e}");
    let j = ok(
        &a,
        &tok,
        Some("nr-1"),
        json!({"event": "session_start", "record": false}),
    )
    .await;
    assert!(ids(&j).contains(&me), "{j}");
    assert!(j["hooks"].is_object(), "{j}");
    assert!(j["timings"]["record_ms"].is_null(), "{j}");
    assert!(!j["warnings"].to_string().contains("recall timeout"), "{j}");
    assert_eq!(shown(&a, &u_id, TargetKind::Entry, &e).await.shown, 0);
    // показ не отмечен — prompt той же сессии подсказку выдаёт
    let j = ok(
        &a,
        &tok,
        Some("nr-1"),
        json!({"event": "prompt", "prompt": "квазар"}),
    )
    .await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    assert_eq!(shown(&a, &u_id, TargetKind::Entry, &e).await.shown, 1);
}
