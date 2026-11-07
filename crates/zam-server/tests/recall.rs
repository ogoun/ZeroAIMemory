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

/// Каждая строка подсказки начинается с `ZeroAIMemory:` и несёт свою ссылку; строка-индекс
/// (M6 §8: changelog, revisit) — начинается со ссылки.
fn assert_marked(j: &Value) {
    let text = j["text"].as_str().unwrap();
    for id in ids(j) {
        let index = format!("{id} «");
        let line = text
            .lines()
            .find(|l| l.ends_with(&format!(" {id}")) || l.starts_with(&index))
            .unwrap_or_else(|| panic!("нет строки {id}: {text}"));
        assert!(
            line.starts_with("ZeroAIMemory: ") || line.starts_with(&index),
            "{line}"
        );
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
/// кандидатов на порядок больше; `hooks` — все типы включены (настройки пользователя по умолчанию).
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
    let text = j["text"].as_str().unwrap();
    assert_eq!(zam_store::count_tokens(text) as u64, tokens);
    // M6 §8: Rules — сверх бюджета, остальное — в `digest_tokens`
    let rest = without_section(text, "### Правила");
    assert!(
        !rest.is_empty() && zam_store::fits_budget(&rest, 1500),
        "{tokens}: {rest}"
    );
    assert!(j["rules_tokens"].as_u64().unwrap() > 0, "{j}");
    assert!(!j["items"].as_array().unwrap().is_empty());
    assert_marked(&j);
    assert_eq!(
        j["hooks"],
        json!({"session_start": true, "prompt": true, "pre_tool": true, "post_tool": true,
               "checkpoint": true, "subagent_start": true, "file_context": true,
               "session_journal": true, "recovery": true, "code_map": true})
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
    let rest = without_section(j["text"].as_str().unwrap(), "### Правила");
    assert!(
        !rest.is_empty() && zam_store::fits_budget(&rest, 120),
        "{}",
        j["text"]
    );
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
        text.contains("ZeroAIMemory: LF в bash-скриптах [gotcha·hyp] i:"),
        "{text}"
    );
    assert!(
        text.contains(
            "ZeroAIMemory: Блок ZeroAIMemory устарел (v1 → v6): обнови через `zam init` или WebUI"
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
        json!({"event": "session_start", "agent_block_version": zam_core::AGENT_BLOCK_VERSION}),
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
            .starts_with("ZeroAIMemory: правило телескоп › Телескоп n:"),
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
            "ZeroAIMemory: эта ошибка уже разбиралась: CRLF в скриптах [gotcha·hyp] i:{crlf}"
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

/// N4 (R5, задача 13): тип выключен в настройках пользователя на сервере — `/recall` этого типа
/// отвечает пустым `text` и показ не учитывает; у `session_start` в ответе — `hooks`.
#[tokio::test]
async fn recall_disabled_hook_kind_is_silent() {
    let a = server("hooks-off").await;
    let (u_id, tok) = a.new_user("Лиза").await;
    let e = insight(
        &a,
        &tok,
        "knowledge",
        "Пульсар мигает",
        "Пульсар — нейтронная звезда.",
        "gotcha",
    )
    .await;
    let me = format!("i:{e}");
    let set = |hooks: Value| {
        let uri = format!("/api/v1/admin/users/{u_id}/settings");
        let a = &a;
        async move {
            let r = a
                .adm(Method::PATCH, &uri, Some(json!({ "hooks": hooks })), None)
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        }
    };
    set(json!({"session_start": false, "prompt": false, "post_tool": false})).await;
    let j = ok(&a, &tok, Some("off-1"), json!({"event": "session_start"})).await;
    assert_eq!(j["text"], "", "{j}");
    assert!(ids(&j).is_empty(), "{j}");
    assert_eq!(
        j["hooks"],
        json!({"session_start": false, "prompt": false, "pre_tool": true,
               "post_tool": false, "checkpoint": true, "subagent_start": true,
               "file_context": true, "session_journal": true, "recovery": true,
               "code_map": true})
    );
    let j = ok(
        &a,
        &tok,
        Some("off-1"),
        json!({"event": "prompt", "prompt": "пульсар"}),
    )
    .await;
    assert_eq!(j["text"], "", "{j}");
    assert!(j.get("hooks").is_none(), "{j}");
    let j = ok(
        &a,
        &tok,
        Some("off-1"),
        json!({"event": "post_tool", "tool": "Bash", "error": "пульсар мигает"}),
    )
    .await;
    assert_eq!(j["text"], "", "{j}");
    assert_eq!(shown(&a, &u_id, TargetKind::Entry, &e).await.shown, 0);
    // показ не отмечен — после включения `prompt` той же сессии подсказку выдаёт
    set(json!({"prompt": true})).await;
    let j = ok(
        &a,
        &tok,
        Some("off-1"),
        json!({"event": "prompt", "prompt": "пульсар"}),
    )
    .await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    assert_eq!(shown(&a, &u_id, TargetKind::Entry, &e).await.shown, 1);
    // настройки одного пользователя не влияют на другого
    let j = ok(
        &a,
        &a.token,
        Some("other-1"),
        json!({"event": "session_start"}),
    )
    .await;
    assert_eq!(j["hooks"]["session_start"], true, "{j}");
}

// ---- M5.6 задача 6: строка карты кода в дайджесте и пакете восстановления (§10) ----

/// Карта решения `G` токеном `tok`: файлы `(путь, [(qual, строка)])`, снимок `main@abcdef1`,
/// машина `box`.
async fn load_map(a: &Api, tok: &str, files: &[(&str, &[(&str, u32)])]) {
    let h = |n: usize| format!("{n:064x}");
    let manifest: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, (p, _))| json!({"path": p, "hash": h(i + 1)}))
        .collect();
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/code/sync",
            Some(
                json!({"solution": G, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "manifest": manifest}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let up: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, (p, syms))| {
            let symbols: Vec<Value> = syms
                .iter()
                .map(|(q, line)| {
                    let name = q.rsplit("::").next().unwrap();
                    json!({"name": name, "qual": q, "kind": "function", "line": line,
                           "end_line": line + 2, "sig": format!("fn {name}()"),
                           "body_hash": format!("{line:016x}"), "exported": true})
                })
                .collect();
            json!({"path": p, "lang": "rust", "hash": h(i + 1), "size": 10,
                   "parse_state": "ok", "symbols": symbols, "refs": []})
        })
        .collect();
    let r = a
        .tok(
            tok,
            Method::PUT,
            "/api/v1/code/files",
            Some(
                json!({"solution": G, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "files": up}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Строка карты кода в тексте или `None`.
fn code_line(j: &Value) -> Option<String> {
    j["text"]
        .as_str()
        .unwrap()
        .lines()
        .find(|l| l.starts_with("ZeroAIMemory: карта кода:"))
        .map(str::to_string)
}

#[tokio::test]
async fn digest_code_line_under_80_tokens_after_verified() {
    let a = server("code-line").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    note_as(
        &a,
        &a.token,
        G,
        "Команды/Сборка",
        "---\nverified_at: 2026-11-01\n---\n# Сборка\n\ncargo build\n",
    )
    .await;
    load_map(
        &a,
        &a.token,
        &[
            ("src/store.rs", &[("Store::open", 1), ("Store::close", 9)]),
            ("src/api.rs", &[("route", 1)]),
        ],
    )
    .await;
    let j = ok(
        &a,
        &a.token,
        Some("code-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let line = code_line(&j).unwrap_or_else(|| panic!("нет строки карты: {j}"));
    assert!(
        line.starts_with(&format!(
            "ZeroAIMemory: карта кода: 2 файла, 3 символа, снимок {} (main@abcdef1); центральные: ",
            zam_store::time::utc_today()
        )),
        "{line}"
    );
    assert!(line.ends_with(" — `memory_code`/`zam code map`"), "{line}");
    assert!(!line.contains("box"), "без машины: {line}");
    assert!(zam_store::count_tokens(&line) <= 80, "{line}");
    // сразу после строки проверенных команд
    let text = j["text"].as_str().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let v = lines
        .iter()
        .position(|l| l.starts_with("Проверенные команды:"))
        .unwrap_or_else(|| panic!("нет проверенных команд: {text}"));
    assert!(
        lines[v + 1].starts_with("ZeroAIMemory: карта кода:"),
        "{text}"
    );
    // длинные имена: центральные сокращаются, строка ≤ 80 токенов
    let long: Vec<String> = (0..3)
        .map(|k| format!("VeryLongTypeNameNumber{k}ForDigestBudget::method_with_long_name_{k}"))
        .collect();
    let syms: Vec<(&str, u32)> = long.iter().map(|q| (q.as_str(), 1)).collect();
    let files: Vec<(String, Vec<(&str, u32)>)> = syms
        .iter()
        .enumerate()
        .map(|(k, s)| (format!("src/long{k}.rs"), vec![*s]))
        .collect();
    let refs: Vec<(&str, &[(&str, u32)])> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_slice()))
        .collect();
    load_map(&a, &a.token, &refs).await;
    let j = ok(
        &a,
        &a.token,
        Some("code-2"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let line = code_line(&j).unwrap_or_else(|| panic!("нет строки карты: {j}"));
    assert!(zam_store::count_tokens(&line) <= 80, "{line}");
}

#[tokio::test]
async fn digest_no_map_no_line() {
    let a = server("code-none").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let j = ok(
        &a,
        &a.token,
        Some("none-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    assert_eq!(code_line(&j), None, "{j}");
}

#[tokio::test]
async fn digest_code_map_off_no_line() {
    let a = server("code-off").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    load_map(&a, &a.token, &[("src/a.rs", &[("alpha", 1)])]).await;
    let uri = format!("/api/v1/admin/users/{}/settings", a.admin_id());
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"code_map": false}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // хуки (`session_start`, запрос с полем `code_map`) — без строки
    for body in [
        json!({"event": "session_start", "solution": G}),
        json!({"event": "recovery", "solution": G, "code_map": true}),
    ] {
        let j = ok(&a, &a.token, Some("off-1"), body.clone()).await;
        assert_eq!(code_line(&j), None, "{body}: {j}");
    }
    // ревью задачи 6, M2: явный вызов (MCP `memory_recall`, REST без `code_map`) не гасится
    let j = ok(
        &a,
        &a.token,
        Some("off-1"),
        json!({"event": "recovery", "solution": G}),
    )
    .await;
    assert!(code_line(&j).is_some(), "{j}");
    // `[hooks] code_map = false` клиента (поле запроса) — без строки и при включённом сервере
    let r = a
        .adm(
            Method::PATCH,
            &uri,
            Some(json!({"hooks": {"code_map": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = ok(
        &a,
        &a.token,
        Some("off-2"),
        json!({"event": "session_start", "solution": G, "code_map": false}),
    )
    .await;
    assert_eq!(code_line(&j), None, "{j}");
    // `code.enabled = false` сервера — тоже без строки
    let b = api("recall-code-disabled", |c| {
        roomy(c);
        c.code.enabled = false;
    })
    .await;
    assert_eq!(register(&b, &b.token).await.status, StatusCode::CREATED);
    let j = ok(
        &b,
        &b.token,
        Some("dis-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    assert_eq!(code_line(&j), None, "{j}");
}

#[tokio::test]
async fn recovery_has_code_line() {
    let a = server("code-recovery").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    load_map(&a, &a.token, &[("src/a.rs", &[("alpha", 1)])]).await;
    for body in [
        json!({"event": "recovery", "solution": G}),
        json!({"event": "session_start", "solution": G, "source": "compact"}),
    ] {
        let j = ok(&a, &a.token, Some("rec-1"), body.clone()).await;
        let line = code_line(&j).unwrap_or_else(|| panic!("{body}: нет строки: {j}"));
        assert!(line.contains("1 файл, 1 символ"), "{line}");
        assert!(line.contains("центральные: alpha"), "{line}");
    }
}

/// M6 §7.2: подсказка `prompt`, показанная в прежней эпохе, после новой эпохи (`new_epoch`) снова
/// показывается — контекст агента потерян; в той же эпохе — один раз.
#[tokio::test]
async fn prompt_hint_returns_after_new_epoch() {
    let a = server("prompt-epoch").await;
    let g = entry(
        &a,
        &a.token,
        "knowledge",
        json!({"kind": "insight", "title": "Квазар коварен", "body": "Квазар светит ярко.",
               "data": {"kind": "gotcha"}}),
    )
    .await;
    let me = format!("i:{g}");
    let prompt = json!({"event": "prompt", "prompt": "квазар"});
    let j = ok(&a, &a.token, Some("pe"), prompt.clone()).await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    let j = ok(&a, &a.token, Some("pe"), prompt.clone()).await;
    assert_eq!(j["text"], "", "{j}");
    let r = a
        .t(
            Method::POST,
            "/api/v1/sessions",
            Some(json!({"id": "pe", "new_epoch": true})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = ok(&a, &a.token, Some("pe"), prompt.clone()).await;
    assert_eq!(ids(&j), [me.as_str()], "{j}");
    let j = ok(&a, &a.token, Some("pe"), prompt.clone()).await;
    assert_eq!(j["text"], "", "{j}");
}

// ---- задача 8a M6 (§8): Rules-резерв, компактные индексы, ключи `[recall]` ----

/// Текст без раздела `title` (заголовок, его строки и пустая строка перед ним) — часть
/// дайджеста, что упаковывается в бюджет (Rules — сверх него, M6 §8).
fn without_section(text: &str, title: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|l| *l == title) else {
        return text.to_string();
    };
    let end = lines[start..]
        .iter()
        .position(|l| l.is_empty())
        .map_or(lines.len(), |k| start + k);
    let mut kept: Vec<&str> = Vec::new();
    if start > 0 && lines[start - 1].is_empty() {
        // пустая строка перед заголовком уходит вместе с разделом
        kept.extend(&lines[..start - 1]);
        kept.extend(&lines[end..]);
    } else {
        // раздел первый: уходит пустая строка после него
        kept.extend(lines.get(end + 1..).unwrap_or_default());
    }
    kept.join("\n")
}

/// Строки раздела `title` (без заголовка); нет раздела — пусто.
fn section(text: &str, title: &str) -> Vec<String> {
    let Some(rest) = text.split(&format!("{title}\n")).nth(1) else {
        return Vec::new();
    };
    rest.split("\n\n")
        .next()
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// M6 §8, §17 п. 9: все Rules (до 20, L1 ≤ 200 токенов) выдаются сверх `digest_tokens` даже
/// при бюджете 100, в дайджесте и в пакете восстановления; остальное — в бюджете;
/// `rules_tokens` — размер раздела Rules.
#[tokio::test]
async fn rules_reserved_never_cut() {
    let a = api("recall-rules-reserved", |c| {
        roomy(c);
        c.recall.digest_tokens = 100;
        c.recall.recovery_tokens = 100;
    })
    .await;
    allow_rules(&a).await;
    let long = "Длинное пояснение правила с подробностями и примерами. ".repeat(40);
    let mut rules = Vec::new();
    for i in 0..21 {
        let n = a
            .note(
                "rules",
                &format!("правило-{i:02}"),
                &format!("# Правило {i}\n\n{long}\n"),
            )
            .await;
        rules.push(format!("n:{}", n["id"].as_str().unwrap()));
    }
    for i in 0..8 {
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
    for event in ["session_start", "recovery"] {
        let j = ok(&a, &a.token, Some("rr-1"), json!({"event": event})).await;
        let text = j["text"].as_str().unwrap();
        let got = ids(&j);
        // до RULES_MAX = 20 по приоритету (надёжность, затем давность создания): первые 20
        // созданных — на месте, самое новое — строкой о непоказанных (ревью 8a, I1)
        for r in &rules[..20] {
            assert!(got.contains(r), "{event}: {r}: {text}");
        }
        assert!(
            !got.contains(&rules[20]),
            "{event}: сверх RULES_MAX: {text}"
        );
        let rules_part = section(text, "### Правила");
        assert_eq!(
            rules_part.last().map(String::as_str),
            Some("ZeroAIMemory: ещё 1 правил не показаны — memory_tree Rules"),
            "{event}: {text}"
        );
        let rest = without_section(text, "### Правила");
        assert!(
            zam_store::fits_budget(&rest, 100),
            "{event}: без Rules {} > 100: {rest}",
            zam_store::count_tokens(&rest)
        );
        let rules_tokens = j["rules_tokens"].as_u64().unwrap_or_else(|| panic!("{j}"));
        assert!(rules_tokens > 100, "{event}: {j}");
    }
    // пакет под задачу — L0 Rules в своём бюджете, как раньше
    let j = ok(
        &a,
        &a.token,
        Some("rr-2"),
        json!({"event": "brief", "budget_tokens": 100}),
    )
    .await;
    assert!(j["tokens"].as_u64().unwrap() <= 100, "{j}");
}

/// M6 §8: порядок разделов не зависит от `usage` (`recall_shown`, `useful`) и показов других
/// сессий — два старта разных сессий подряд дают одинаковый `text`.
#[tokio::test]
async fn digest_order_ignores_recall_shown() {
    let a = server("order-usage").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let mut traps = Vec::new();
    for i in 0..8 {
        traps.push(insight(&a, &a.token, G, &format!("Ловушка {i}"), "x", "gotcha").await);
    }
    for i in 0..8 {
        entry(
            &a,
            &a.token,
            G,
            json!({"kind": "changelog", "title": format!("Изменение {i}"), "body": "x",
                   "data": {"kind": "fix"}}),
        )
        .await;
    }
    let body = json!({"event": "session_start", "solution": G});
    let first = ok(&a, &a.token, Some("ou-1"), body.clone()).await;
    // показы и полезность самой старой ловушки — много раз, в других сессиях
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let old = traps[0].clone();
    db.write(move |tx| {
        for k in 0..50 {
            for ev in ["recall_shown", "useful"] {
                tx.execute(
                    "INSERT INTO usage (target_kind, target_id, event, source, session_id, \
                     user_id, ts) SELECT 'entry', ?1, ?2, source, ?3, user_id, ts FROM usage \
                     LIMIT 1",
                    rusqlite::params![old, ev, format!("other-{k}")],
                )?;
            }
        }
        Ok(())
    })
    .await
    .unwrap();
    let second = ok(&a, &a.token, Some("ou-2"), body.clone()).await;
    assert_eq!(first["text"], second["text"], "{first}\n---\n{second}");
}

/// M6 §8: раздел `### Идеи` (как задачи и ответы человека) идёт в бюджет первым — компактные
/// индексы его не вытесняют; Rules — сверх бюджета.
#[tokio::test]
async fn ideas_section_not_cut_by_budget() {
    let a = api("recall-ideas-budget", |c| {
        roomy(c);
        c.recall.digest_tokens = 260;
    })
    .await;
    allow_rules(&a).await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    for i in 0..6 {
        a.note(
            "rules",
            &format!("п{i}"),
            &format!("# Правило {i}\n\nТекст правила {i}."),
        )
        .await;
    }
    let mut ideas = Vec::new();
    for t in ["Кэш миниатюр", "Экспорт отчётов", "Тёмная тема"] {
        let r = a
            .t(
                Method::POST,
                &format!("/api/v1/spaces/{G}/entries"),
                Some(json!({"kind": "idea", "title": t, "body": "Замысел.",
                            "data": {"origin": "human", "priority": "high"}})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        ideas.push(format!("w:{}", r.json()["id"].as_str().unwrap()));
    }
    for i in 0..10 {
        insight(
            &a,
            &a.token,
            G,
            &format!("Ловушка сборки номер {i}"),
            "x",
            "gotcha",
        )
        .await;
        entry(
            &a,
            &a.token,
            G,
            json!({"kind": "changelog", "title": format!("Изменение {i}"), "body": "x",
                   "data": {"kind": "fix"}}),
        )
        .await;
    }
    let j = ok(
        &a,
        &a.token,
        Some("ib-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    let got = ids(&j);
    for w in &ideas {
        assert!(got.contains(w), "{w}: {text}");
    }
    // правило — строка-заголовок и строка L1
    assert_eq!(section(text, "### Правила").len(), 12, "{text}");
}

/// Строка-индекс `<ссылка> «<заголовок>» · <N>t`.
fn index_line(line: &str, reference: &str, title: &str) -> bool {
    line.strip_prefix(&format!("{reference} «{title}» · "))
        .and_then(|n| n.strip_suffix('t'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// M6 §8: changelog и revisit — компактные строки-индексы `<ссылка> «заголовок» · Nt`.
#[tokio::test]
async fn changelog_index_line_format() {
    let a = server("index-lines").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let body = "Исправлен разбор дат в отчёте. ".repeat(10);
    let c = entry(
        &a,
        &a.token,
        G,
        json!({"kind": "changelog", "title": "Разбор дат", "body": body,
               "data": {"kind": "fix"}}),
    )
    .await;
    let r = entry(
        &a,
        &a.token,
        G,
        json!({"kind": "insight", "title": "Проверить таймауты", "body": "Таймауты шлюза.",
               "data": {"kind": "decision"}, "status": "revisit"}),
    )
    .await;
    let j = ok(
        &a,
        &a.token,
        Some("il-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    let changes = section(text, "### Последние изменения");
    assert_eq!(changes.len(), 1, "{text}");
    let line = &changes[0];
    assert!(index_line(line, &format!("c:{c}"), "Разбор дат"), "{line}");
    let n: usize = line
        .rsplit(" · ")
        .next()
        .unwrap()
        .trim_end_matches('t')
        .parse()
        .unwrap();
    assert!(n > 20, "размер записи: {line}");
    let revisit = section(text, "### Пересмотреть (revisit)");
    assert_eq!(revisit.len(), 1, "{text}");
    assert!(
        index_line(&revisit[0], &format!("i:{r}"), "Проверить таймауты"),
        "{}",
        revisit[0]
    );
    // индексы — тоже подсказки: в `items[]` и в учёте показов
    let got = ids(&j);
    assert!(
        got.contains(&format!("c:{c}")) && got.contains(&format!("i:{r}")),
        "{j}"
    );
}

/// M6 §8: `recall.digest_entries` — предел записей в разделах дайджеста (ловушки, changelog).
#[tokio::test]
async fn digest_entries_limits_sections() {
    let a = api("recall-digest-entries", |c| {
        roomy(c);
        c.recall.digest_entries = 2;
    })
    .await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    for i in 0..4 {
        insight(&a, &a.token, G, &format!("Ловушка {i}"), "x", "gotcha").await;
        entry(
            &a,
            &a.token,
            G,
            json!({"kind": "changelog", "title": format!("Изменение {i}"), "body": "x",
                   "data": {"kind": "fix"}}),
        )
        .await;
    }
    let j = ok(
        &a,
        &a.token,
        Some("de-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert_eq!(section(text, "### Ловушки и сбои").len(), 2, "{text}");
    assert_eq!(section(text, "### Последние изменения").len(), 2, "{text}");
}

/// M6 §8: корень решения — одна строка, L0 — до 20 токенов.
#[tokio::test]
async fn solution_root_l0_cut_to_20_tokens() {
    let a = server("root-l0").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let l0 = "Сервис расчёта доставки заказов по регионам с учётом тарифов перевозчиков, \
              весовых коэффициентов, праздников и окон приёма на складах партнёров";
    let root = note_as(
        &a,
        &a.token,
        G,
        "README",
        &format!("---\nl0: {l0}\n---\n# README\n\nТекст.\n"),
    )
    .await;
    let j = ok(
        &a,
        &a.token,
        Some("rl-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    let line = section(text, "### Решение")
        .into_iter()
        .find(|l| l.ends_with(&format!("n:{root}")))
        .unwrap_or_else(|| panic!("нет корня: {text}"));
    let shown = line
        .split_once(" — ")
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("нет L0: {line}"));
    let shown = shown.rsplit_once(" n:").map_or(shown, |(s, _)| s);
    assert!(shown.starts_with("Сервис расчёта доставки"), "{line}");
    assert!(
        shown.chars().count() < l0.chars().count(),
        "L0 не сокращён: {line}"
    );
    assert!(zam_store::count_tokens(shown) <= 21, "{line}");
}

/// M6 §8, решение 16: `recall.digest_code_map_tokens = 0` (умолчание) — только строка-указатель
/// карты; больше 0 — мини-карта `### Карта кода` (центральные символы) в пределах ключа.
#[tokio::test]
async fn code_map_pointer_only_at_zero() {
    let files: &[(&str, &[(&str, u32)])] = &[
        ("src/store.rs", &[("Store::open", 1), ("Store::close", 9)]),
        ("src/api.rs", &[("route", 1), ("handler", 7)]),
        ("src/db.rs", &[("Db::query", 3)]),
    ];
    let a = server("code-pointer").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    load_map(&a, &a.token, files).await;
    let body = json!({"event": "session_start", "solution": G});
    let j = ok(&a, &a.token, Some("cp-1"), body.clone()).await;
    let text = j["text"].as_str().unwrap();
    assert!(code_line(&j).is_some(), "{text}");
    assert!(!text.contains("### Карта кода"), "{text}");
    let b = api("recall-code-mini", |c| {
        roomy(c);
        c.recall.digest_code_map_tokens = 60;
    })
    .await;
    assert_eq!(register(&b, &b.token).await.status, StatusCode::CREATED);
    load_map(&b, &b.token, files).await;
    let j = ok(&b, &b.token, Some("cp-2"), body).await;
    let text = j["text"].as_str().unwrap();
    assert!(code_line(&j).is_some(), "{text}");
    let mini = section(text, "### Карта кода");
    assert!(!mini.is_empty(), "{text}");
    assert!(
        mini.iter()
            .all(|l| l.starts_with("src/") && l.contains(':')),
        "{mini:?}"
    );
    let block = format!("\n### Карта кода\n{}\n", mini.join("\n"));
    assert!(
        zam_store::fits_budget(&block, 60),
        "{}: {mini:?}",
        zam_store::count_tokens(&block)
    );
}

/// Предложение zero-agent (приложение исследования 2026-11-06): задача в `open`, захваченная
/// сессией (`claim`), видна в дайджесте — своя первой с пометкой, чужая — после задач в работе,
/// с пометкой; незахваченная `open` — нет.
#[tokio::test]
async fn digest_shows_claimed_open_tasks() {
    let a = server("claimed-open").await;
    let mut ids_of = Vec::new();
    for title in [
        "Моя захваченная",
        "Чужая захваченная",
        "Свободная",
        "В работе",
    ] {
        ids_of.push(
            entry(
                &a,
                &a.token,
                "knowledge",
                json!({"kind": "task", "title": title, "data": {"next_step": "шаг"}}),
            )
            .await,
        );
    }
    let (mine, other, free, busy) = (&ids_of[0], &ids_of[1], &ids_of[2], &ids_of[3]);
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{busy}"),
            Some(json!({"status": "in_progress"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for (t, s) in [(mine, "cl-me"), (other, "cl-other")] {
        let r = a
            .tok_h(
                &a.token,
                Method::POST,
                &format!("/api/v1/entries/{t}/claim"),
                Some(json!({})),
                &[("x-zam-session", s)],
            )
            .await;
        assert!(r.status.is_success(), "{}", r.text());
    }
    let j = ok(
        &a,
        &a.token,
        Some("cl-me"),
        json!({"event": "session_start"}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    let lines = section(text, "### Задачи");
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].contains("Моя захваченная") && lines[0].contains("захвачена этой сессией"),
        "{text}"
    );
    assert!(lines[0].contains(&format!("k:{mine}")), "{text}");
    assert!(lines[1].contains("[in_progress] В работе"), "{text}");
    assert!(
        lines[2].contains("Чужая захваченная") && lines[2].contains("захвачена другой сессией"),
        "{text}"
    );
    assert!(!text.contains(free.as_str()), "{text}");
    // у другой сессии — наоборот
    let j = ok(
        &a,
        &a.token,
        Some("cl-other"),
        json!({"event": "session_start"}),
    )
    .await;
    let lines = section(j["text"].as_str().unwrap(), "### Задачи");
    assert!(
        lines[0].contains("Чужая захваченная") && lines[0].contains("захвачена этой сессией"),
        "{lines:?}"
    );
    assert!(
        lines[2].contains("Моя захваченная") && lines[2].contains("захвачена другой сессией"),
        "{lines:?}"
    );
}

// ---- ревью задачи 8a (C1, I1, I3, M1, M3): предел вывода канала, приоритет правил ----

/// Пределы каналов агентов (`zam-cli` `hook::output`): Claude Code — символы, Codex — байты.
const CLAUDE_MAX_CHARS: usize = 9000;
const CODEX_MAX_BYTES: usize = 7500;

/// Текст правила: `n` разделов кириллицей (L1 — оглавление с первыми фразами, ≈ 200 токенов при
/// `n = 12`) и `l0` во frontmatter.
fn rule_text(title: &str, l0: &str, n: usize) -> String {
    let body: String = (0..n)
        .map(|k| {
            format!(
                "## Пункт {k}\n\nПредложение {k} правила «{title}» требует проверки изменений и \
                 записи в журнал.\n\n"
            )
        })
        .collect();
    format!("---\nl0: {l0}\n---\n# {title}\n\n{body}")
}

/// Хвост первой фразы L0 правил мира: длиннее предела L0 правила (40 токенов) — строка L0
/// обрезается с «…».
const RULE_L0_TAIL: &str = "проверять каждое изменение на стенде, записывать решение и его причину в \
    журнал задачи, согласовывать выпуск с дежурным и владельцем сервиса, не трогать чужие ветки, \
    стенды и секреты, сообщать о сбоях в канал команды";
/// Строка-пояснение раздела Rules о сокращённых правилах.
const SHORT_NOTE: &str =
    "ZeroAIMemory: правила одной строкой сокращены («…» — обрезано) — полностью memory_read n:<id>";

/// Мир «20 правил × 200 токенов кириллицей» с решением, ловушками, задачей и картой кода; id
/// правил по порядку создания.
async fn heavy_rules_world(name: &str, rules: usize) -> (Api, Vec<String>) {
    let a = server(name).await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let mut ids = Vec::new();
    let dirs = ["security", "process", "backend", "frontend", "infra"];
    for i in 0..rules {
        let path = format!("{}/правило-{i:02}", dirs[i % dirs.len()]);
        let r = note_as(
            &a,
            &a.token,
            "rules",
            &path,
            &rule_text(
                &format!("Правило {i}"),
                &format!("Кратко о правиле {i}: {RULE_L0_TAIL}. Вторая фраза L0."),
                12,
            ),
        )
        .await;
        ids.push(format!("n:{r}"));
    }
    for i in 0..3 {
        insight(
            &a,
            &a.token,
            G,
            &format!("Ловушка сборки {i}"),
            "x",
            "gotcha",
        )
        .await;
    }
    let t = entry(
        &a,
        &a.token,
        G,
        json!({"kind": "task", "title": "Задача в работе", "data": {"next_step": "шаг"}}),
    )
    .await;
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{t}"),
            Some(json!({"status": "in_progress"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    load_map(&a, &a.token, &[("src/store.rs", &[("Store::open", 1)])]).await;
    (a, ids)
}

/// Все правила в тексте — блоком (L1 или L0) или учтены строкой «ещё N правил не показаны».
fn assert_rules_accounted(what: &str, text: &str, ids: &[String]) {
    let missing = ids.iter().filter(|r| !text.contains(r.as_str())).count();
    if missing > 0 {
        assert!(
            text.contains(&format!("ZeroAIMemory: ещё {missing} правил не показаны")),
            "{what}: {missing} правил пропали молча: {text}"
        );
    }
}

/// Ревью 8a, C1: с `output_limit` канала (Codex — байты, Claude — символы) весь текст в
/// пределе — клиенту резать нечего; правила — первыми (хвост приоритета — L0), карта кода и
/// «Ловушки» — на месте.
#[tokio::test]
async fn digest_fits_output_limit_codex_and_claude() {
    let (a, ids) = heavy_rules_world("limit-agents", 20).await;
    for (unit, max) in [("bytes", CODEX_MAX_BYTES), ("chars", CLAUDE_MAX_CHARS)] {
        let j = ok(
            &a,
            &a.token,
            Some(&format!("lim-{unit}")),
            json!({"event": "session_start", "solution": G, "source": "startup",
                   "code_map": true, "output_limit": {"unit": unit, "max": max}}),
        )
        .await;
        let text = j["text"].as_str().unwrap();
        let size = if unit == "bytes" {
            text.len()
        } else {
            text.chars().count()
        };
        assert!(size <= max, "{unit}: {size} > {max}");
        assert_rules_accounted(unit, text, &ids);
        assert!(
            ids.iter().all(|r| text.contains(r.as_str())),
            "{unit}: L0 всех 20 правил помещается: {text}"
        );
        // хвост приоритета — L0 (без L1), голова — с L1
        assert!(text.contains("— Кратко о правиле 19"), "{unit}: {text}");
        assert!(
            text.contains("Предложение 0 правила «Правило 0»"),
            "{unit}: {text}"
        );
        assert!(code_line(&j).is_some(), "{unit}: нет карты кода: {text}");
        assert!(text.contains("### Ловушки и сбои"), "{unit}: {text}");
        // повторное ревью 8a: обрезанный L0 — с «…», строка-пояснение — ровно одна
        let l0_lines: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("— Кратко о правиле"))
            .collect();
        assert!(!l0_lines.is_empty(), "{unit}: {text}");
        for l in &l0_lines {
            assert!(
                l.contains("…") && !l.contains("Вторая фраза"),
                "{unit}: {l}"
            );
        }
        assert_eq!(text.matches(SHORT_NOTE).count(), 1, "{unit}: {text}");
    }
    // без предела (REST, MCP) — L1 у всех
    let j = ok(
        &a,
        &a.token,
        Some("lim-none"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.len() > CODEX_MAX_BYTES, "{}", text.len());
    assert!(!text.contains("— Кратко о правиле"), "{text}");
    assert!(!text.contains(SHORT_NOTE), "{text}");
}

/// Ревью 8a, C1: хук старого клиента (есть `source`/`code_map`, нет `output_limit`) —
/// консервативные 7500 байт; восстановление с местом под сводку журнала — в своём пределе.
#[tokio::test]
async fn old_hook_client_gets_conservative_limit() {
    let (a, ids) = heavy_rules_world("limit-old", 20).await;
    let j = ok(
        &a,
        &a.token,
        Some("old-1"),
        json!({"event": "session_start", "solution": G, "source": "startup", "code_map": true}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.len() <= CODEX_MAX_BYTES, "{}", text.len());
    assert_rules_accounted("old", text, &ids);
    // пакет восстановления: клиент вычел 1500 байт под сводку журнала
    let j = ok(
        &a,
        &a.token,
        Some("old-1"),
        json!({"event": "session_start", "solution": G, "source": "compact", "code_map": true,
               "output_limit": {"unit": "bytes", "max": CODEX_MAX_BYTES - 1500}}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.len() <= CODEX_MAX_BYTES - 1500, "{}", text.len());
    assert_rules_accounted("recovery", text, &ids);
    assert!(text.contains("### Задачи"), "{text}");
    // некорректный предел — 400
    for bad in [
        json!({"event": "session_start", "output_limit": {"unit": "bytes", "max": 0}}),
        json!({"event": "session_start", "output_limit": {"unit": "words", "max": 10}}),
    ] {
        let r = recall(&a, &a.token, None, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
}

/// Ревью 8a, C1: правила не влезают в предел и строками L0 — с конца приоритета снимаются, а
/// строка называет их число; правило с `axiom` остаётся.
#[tokio::test]
async fn rules_over_limit_dropped_with_honest_line() {
    let (a, ids) = heavy_rules_world("limit-drop", 20).await;
    let j = ok(
        &a,
        &a.token,
        Some("drop-1"),
        json!({"event": "session_start", "solution": G, "source": "startup",
               "output_limit": {"unit": "bytes", "max": 1200}}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.len() <= 1200, "{}", text.len());
    let shown = ids.iter().filter(|r| text.contains(r.as_str())).count();
    assert!(shown > 0 && shown < 20, "{shown}: {text}");
    assert_rules_accounted("drop", text, &ids);
    // первые по приоритету (старые) — на месте, последние — сняты
    assert!(text.contains(ids[0].as_str()), "{text}");
    assert!(!text.contains(ids[19].as_str()), "{text}");
}

/// Ревью 8a, I1: приоритет правил — надёжность, затем давность создания, не алфавит: при
/// 21 правиле не показывается самое новое, а не `security/*` в хвосте алфавита; `axiom` —
/// всегда в числе показанных.
#[tokio::test]
async fn rules_priority_not_alphabetical() {
    let a = server("rules-priority").await;
    let mut ids = Vec::new();
    // сначала — `security/secrets` (хвост алфавита), затем `a/…`
    let sec = note_as(
        &a,
        &a.token,
        "rules",
        "security/secrets",
        "# Secrets\n\nНе в репозиторий.",
    )
    .await;
    for i in 0..19 {
        ids.push(
            note_as(
                &a,
                &a.token,
                "rules",
                &format!("a/п{i:02}"),
                &format!("# П{i}\n\nТекст."),
            )
            .await,
        );
    }
    // новейшее — `axiom`: надёжность важнее давности
    let ax = note_as(
        &a,
        &a.token,
        "rules",
        "zz/axiom",
        "---\ncertainty: axiom\n---\n# Аксиома\n\nВсегда.",
    )
    .await;
    let j = ok(
        &a,
        &a.token,
        Some("pr-1"),
        json!({"event": "session_start"}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.contains(&format!("n:{sec}")), "{text}");
    assert!(text.contains(&format!("n:{ax}")), "{text}");
    assert!(
        !text.contains(&format!("n:{}", ids[18])),
        "новейшее — сверх RULES_MAX: {text}"
    );
    assert!(text.contains("ещё 1 правил не показаны"), "{text}");
    // вывод — по пути
    let a_pos = text.find("правило a/п00").unwrap();
    let s_pos = text.find("правило security/secrets").unwrap();
    assert!(a_pos < s_pos, "{text}");
}

/// Ревью 8a, M1: в пакете субагенту задача, захваченная родителем, — «захвачена родительской
/// сессией».
#[tokio::test]
async fn brief_marks_parent_claim() {
    let a = server("brief-parent").await;
    let t = entry(
        &a,
        &a.token,
        "knowledge",
        json!({"kind": "task", "title": "Задача родителя", "data": {"next_step": "шаг"}}),
    )
    .await;
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("/api/v1/entries/{t}/claim"),
            Some(json!({})),
            &[("x-zam-session", "parent-1")],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let j = ok(
        &a,
        &a.token,
        Some("parent-1:sub"),
        json!({"event": "brief", "parent_id": "parent-1"}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(
        text.contains("[open · захвачена родительской сессией] Задача родителя"),
        "{text}"
    );
}

/// Ревью 8a, M3: заголовок в строке-индексе changelog — до 20 токенов, как L0 корня.
#[tokio::test]
async fn index_title_cut_like_root_l0() {
    let a = server("index-title").await;
    assert_eq!(register(&a, &a.token).await.status, StatusCode::CREATED);
    let long = "Исправлен разбор дат в отчёте продавца по возвратам товаров с учётом часовых \
                поясов, летнего времени и праздничных дней всех регионов доставки";
    let c = entry(
        &a,
        &a.token,
        G,
        json!({"kind": "changelog", "title": long, "body": "x", "data": {"kind": "fix"}}),
    )
    .await;
    let j = ok(
        &a,
        &a.token,
        Some("it-1"),
        json!({"event": "session_start", "solution": G}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    let line = section(text, "### Последние изменения")
        .into_iter()
        .find(|l| l.starts_with(&format!("c:{c} «")))
        .unwrap_or_else(|| panic!("{text}"));
    let title = line
        .trim_start_matches(&format!("c:{c} «"))
        .rsplit_once("» · ")
        .unwrap()
        .0;
    assert!(title.chars().count() < long.chars().count(), "{line}");
    assert!(zam_store::count_tokens(title) <= 21, "{line}");
}

/// Повторное ревью 8a, п. 3: правило, у которого L1 не длиннее L0 больше чем в 1,4 раза, не
/// переводится в L0 — шаг урезания идёт к следующему правилу.
#[tokio::test]
async fn short_l1_rule_stays_l1() {
    let (a, ids) = heavy_rules_world("limit-short-l1", 19).await;
    // самое новое (хвост приоритета) — с коротким L1, равным по длине L0
    let short = note_as(
        &a,
        &a.token,
        "rules",
        "zz/коротко",
        "---\nl0: Коротко. Одна фраза.\n---\n# Коротко\n\nОдна фраза.\n",
    )
    .await;
    let j = ok(
        &a,
        &a.token,
        Some("short-1"),
        json!({"event": "session_start", "solution": G, "source": "startup",
               "output_limit": {"unit": "bytes", "max": CODEX_MAX_BYTES}}),
    )
    .await;
    let text = j["text"].as_str().unwrap();
    assert!(text.len() <= CODEX_MAX_BYTES, "{}", text.len());
    // 19 правил мира и короткое — все 20 на месте
    assert!(ids.iter().all(|r| text.contains(r.as_str())), "{text}");
    let block = text
        .split("ZeroAIMemory: правило ")
        .find(|b| b.contains(&format!("n:{short}")))
        .unwrap_or_else(|| panic!("нет правила: {text}"));
    assert!(
        block.contains("\n  Коротко"),
        "короткий L1 остался L1: {block}"
    );
    // урезаны другие правила
    assert!(text.contains("— Кратко о правиле"), "{text}");
}

/// Повторное ревью 8a, m2: MCP `memory_recall` с `source` (восстановление) — без предела
/// канала: хуков в MCP нет, текст не урезается под 7500 байт.
#[tokio::test]
async fn mcp_recall_with_source_has_no_limit() {
    let (a, _) = heavy_rules_world("limit-mcp", 20).await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "memory_recall",
                                 "arguments": {"event": "recovery", "source": "compact"}}});
    let r = a
        .send(
            axum::http::Request::builder()
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
                )
                .header("x-zam-session", "mcp-limit-1")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let text = r.text();
    assert!(
        !text.contains("— Кратко о правиле"),
        "MCP — L1 у всех: {text}"
    );
    assert!(text.len() > CODEX_MAX_BYTES, "{}", text.len());
}
