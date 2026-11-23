//! Ночная проверка памяти (M7.3 §6.5, задача 5): прогон тем же путём, что у агента, без следов;
//! глубокий поиск вне бюджета; вердикт к базе на сервере (ночные прогоны за дату через
//! `check::run_night` с замороженными часами); принятие нормы; копия, переиндексация, предел,
//! прерванный прогон; подсказки выключены; посторонние вопросы по решениям; строка дайджеста;
//! ручной прогон (409, остановка), срок хранения, выключатели, маска для токена.
//!
//! Сервер без модели — поиск без векторов (`degraded`); тесты вердикта по FTS ставят тестовую
//! точку «с векторами» (`set_degraded(Some(false))`). Тесты с моделью (MiniLM, Qdrant) без
//! ORT/модели/Qdrant пропускаются, в CI (`ZAM_REQUIRE_ML=1`) — падают.
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use jiff::{SignedDuration, Timestamp, tz::TimeZone};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::backup::runner::RunBy;
use zam_server::check;
use zam_server::config::{CheckMode, ServerConfig};
use zam_server::works::NightCtx;
use zam_server::works::night::test_hooks::{self as night_hooks, Jobs};
use zam_store::check::Trigger;
use zam_store::probes::{NewProbe, ProbeAuthor, ProbeKind, ProbeTarget, Split};
use zam_store::{SpaceKind, time::freeze_now};

use common::api::{Api, api, api_with, id, ok, problem};
use common::check::{Reset, SERIAL, space_of, ts};

/// Ночь `n` ноября 2026 (часы — в окне 04:30 UTC + 1 мин).
fn day(n: u32) -> String {
    format!("2026-11-{n:02}")
}

/// Сервер «с векторами» для вердикта по FTS и без паузы между пробами.
fn quick() {
    check::test_hooks::set_pause(Duration::ZERO);
    check::test_hooks::set_degraded(Some(false));
}

fn note_text(i: usize) -> String {
    format!("# Заметка {i}\n\nz{i}alpha z{i}bravo — запись знаний.\n")
}

fn question(i: usize) -> String {
    format!("z{i}alpha z{i}bravo")
}

/// Проба вида `probe` части `test` о цели `target`.
async fn add_probe(a: &Api, user: Id, space: Id, q: &str, target: &str) -> Id {
    let db = a.st.users.get(user).await.unwrap();
    let out = zam_store::probes::insert_probe(
        &db,
        NewProbe {
            space: Some(space),
            question: q.into(),
            kind: ProbeKind::Probe,
            targets: vec![ProbeTarget {
                target: target.into(),
                rev: 1,
            }],
            split: Split::Test,
            overlap: Some(0.0),
            author_kind: ProbeAuthor::Agent,
            author: None,
            work_id: None,
        },
    )
    .await
    .unwrap();
    match out {
        zam_store::probes::InsertOutcome::Inserted(id) => id,
        o => panic!("проба не записана: {o:?}"),
    }
}

/// Сервер с `n` заметками Knowledge и пробой о каждой (часть `test`).
async fn fixture(
    name: &str,
    n: usize,
    tweak: impl FnOnce(&mut ServerConfig),
) -> (Api, Vec<String>) {
    let a = api(name, tweak).await;
    let kn = space_of(&a, a.st.admin_id, SpaceKind::Knowledge).await;
    let mut notes = Vec::new();
    for i in 0..n {
        let nid = id(&a
            .note("knowledge", &format!("kb/n{i}.md"), &note_text(i))
            .await);
        add_probe(&a, a.st.admin_id, kn, &question(i), &format!("n:{nid}")).await;
        notes.push(nid);
    }
    (a, notes)
}

/// Заметка `i` без слов её пробы (цель больше не находится).
async fn damage(a: &Api, node: &str, i: usize) {
    let rev = rev_of(a, node).await;
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{node}"),
            Some(json!({"content": format!("# Заметка {i}\n\nпусто.\n")})),
            &[("if-match", rev.as_str())],
        )
        .await;
    ok(&r, StatusCode::OK);
}

/// Текущая ревизия заметки (для `If-Match`).
async fn rev_of(a: &Api, node: &str) -> String {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    zam_store::get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .current_rev
        .to_string()
}

/// Перезапуск сервера на том же каталоге с конфигурацией `cfg`: первый токен прежний (новый
/// старт его не печатает).
async fn restart(a: Api, cfg: ServerConfig) -> Api {
    let token = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    let mut b = api_with(cfg).await;
    b.token = token;
    b
}

async fn run_json(a: &Api, run: &str) -> Value {
    let r = a
        .t(Method::GET, &format!("/api/v1/check/runs/{run}"), None)
        .await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// Ночной прогон пользователя `user` за ночь `d` (часы — в окне); прогона нет — `None`.
async fn night_of(a: &Api, user: Id, d: &str) -> Option<Id> {
    let _clock = freeze_now(ts(&format!("{d}T04:31:00Z")));
    let ctx = NightCtx::new(a.st.clone());
    check::run_night(&ctx, user, d.parse().unwrap())
        .await
        .unwrap()
}

/// Ночной прогон администратора — JSON прогона.
async fn night(a: &Api, d: &str) -> Value {
    let run = night_of(a, a.st.admin_id, d).await.expect("ночной прогон");
    run_json(a, &run.to_string()).await
}

fn kind(r: &Value) -> &str {
    r["verdict"]["kind"].as_str().unwrap_or("—")
}

/// Текст дайджеста первого `session_start` (`source = startup`) по токену.
async fn digest(a: &Api, source: &str) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "session_start", "source": source})),
        )
        .await;
    ok(&r, StatusCode::OK);
    r.json()["text"].as_str().unwrap_or_default().to_string()
}

/// Пять ночей базы (1..=5 ноября); вердикты — `baseline` 0/5 … 4/5.
async fn base_nights(a: &Api) {
    for n in 1..=5 {
        let r = night(a, &day(n)).await;
        assert_eq!(kind(&r), "baseline", "{r}");
        assert_eq!(r["verdict"]["base_n"], n - 1);
    }
}

/// Строки таблицы `table` базы администратора (сериализованы, по порядку колонок `order`).
async fn rows(a: &Api, table: &str, order: &str) -> Vec<String> {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let sql = format!("SELECT * FROM {table} ORDER BY {order}");
    db.read(move |c| {
        let mut st = c.prepare(&sql)?;
        let n = st.column_count();
        let out = st
            .query_map([], |r| {
                let mut s = String::new();
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    s.push_str(&format!("{v:?}|"));
                }
                Ok(s)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    })
    .await
    .unwrap()
}

/// Следы выдачи: `usage`, `usage_opened`, `session_seen`, `session_tokens`,
/// `tokens_unattributed`, обращения к выжимкам кэша.
async fn traces(a: &Api) -> Vec<Vec<String>> {
    vec![
        rows(a, "usage", "rowid").await,
        rows(a, "usage_opened", "session_id, target_id").await,
        rows(a, "session_seen", "rowid").await,
        rows(a, "session_tokens", "rowid").await,
        rows(a, "tokens_unattributed", "day, event").await,
        rows(a, "sessions", "id").await,
        {
            let db = a.st.users.get(a.st.admin_id).await.unwrap();
            db.read(|c| {
                let mut st =
                    c.prepare("SELECT id, hits, last_used_at FROM answer_cache ORDER BY id")?;
                let out = st
                    .query_map([], |r| {
                        Ok(format!(
                            "{}|{}|{}",
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await
            .unwrap()
        },
    ]
}

/// §6.5 п. 2: прогон не оставляет следов — строки `usage`, `usage_opened`, `session_seen`,
/// `session_tokens`, `tokens_unattributed`, сессии и `hits`/`last_used_at` выжимок прежние; а
/// прогон шёл поиском (выжимка кэша — первой карточкой) и подсказками (insight — подсказкой).
#[tokio::test]
async fn check_run_leaves_no_traces() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, notes) = fixture("check-traces", 3, |_| {}).await;
    let kn = space_of(&a, a.st.admin_id, SpaceKind::Knowledge).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "Решение z9alpha z9bravo",
                        "body": "Решение z9alpha z9bravo принято.", "data": {"kind": "decision"}}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let ins = id(&r.json());
    let ins_probe = add_probe(
        &a,
        a.st.admin_id,
        kn,
        "решение z9alpha z9bravo",
        &format!("i:{ins}"),
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/cache",
            Some(json!({"query": question(0), "summary": "кратко",
                        "sources": [{"id": format!("n:{}", notes[0])}]})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    // следы агента: сессия, показы и токены
    for body in [
        json!({"event": "session_start", "session_id": "s-trace"}),
        json!({"event": "prompt", "prompt": "решение z9alpha z9bravo", "session_id": "s-trace"}),
    ] {
        ok(
            &a.t(Method::POST, "/api/v1/recall", Some(body)).await,
            StatusCode::OK,
        );
    }
    let before = traces(&a).await;
    assert!(!before[0].is_empty() && !before[2].is_empty(), "{before:?}");
    let ctx = NightCtx::new(a.st.clone());
    let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    assert_eq!(traces(&a).await, before);
    let j = run_json(&a, &run.to_string()).await;
    assert_eq!(j["status"], "done", "{j}");
    let res = j["results"].as_array().unwrap();
    let of = |p: &str| res.iter().find(|r| r["probe"] == p).unwrap().clone();
    let ins_row = of(&format!("x:{ins_probe}"));
    assert_eq!(ins_row["prompt_rank"], 1, "{j}");
    assert!(
        res.iter()
            .filter(|r| r["kind"] == "probe")
            .all(|r| r["search_rank"] == 1),
        "{j}"
    );
    // выжимка доступна поиску агента: обычный поиск её учитывает (`hits + 1`)
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": question(0), "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["items"][0]["kind"], "cache", "{}", r.text());
    assert_ne!(traces(&a).await[6], before[6]);
}

/// §6.5 п. 2: глубокий поиск (`top_k = 50`, без среза бюджетом) — только у проб вне бюджета
/// основного поиска: он различает `beyond` (нашлась за границей) и `none` (не нашлась).
#[tokio::test]
async fn check_deep_search_only_outside_budget() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let a = api("check-deep", |c| c.search.budget_tokens = 70).await;
    let kn = space_of(&a, a.st.admin_id, SpaceKind::Knowledge).await;
    let strong = id(&a
        .note(
            "knowledge",
            "kb/strong.md",
            "# Сильная\n\nhotelindia hotelindia hotelindia juliet.\n",
        )
        .await);
    let weak = id(&a
        .note(
            "knowledge",
            "kb/weak.md",
            "# Слабая\n\nhotelindia juliet и много других слов здесь.\n",
        )
        .await);
    let lost = id(&a
        .note(
            "knowledge",
            "kb/lost.md",
            "# Потерянная\n\nничего общего.\n",
        )
        .await);
    // предусловие: в бюджет основного поиска входит одна карточка
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "hotelindia juliet", "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(
        r.json()["items"].as_array().unwrap().len(),
        1,
        "{}",
        r.text()
    );
    let p_strong = add_probe(
        &a,
        a.st.admin_id,
        kn,
        "hotelindia juliet",
        &format!("n:{strong}"),
    )
    .await;
    let p_weak = add_probe(
        &a,
        a.st.admin_id,
        kn,
        "juliet hotelindia",
        &format!("n:{weak}"),
    )
    .await;
    let p_lost = add_probe(
        &a,
        a.st.admin_id,
        kn,
        "xrayabsent yankeeabsent",
        &format!("n:{lost}"),
    )
    .await;
    let ctx = NightCtx::new(a.st.clone());
    let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    let j = run_json(&a, &run.to_string()).await;
    let res = j["results"].as_array().unwrap();
    let of = |p: Id| res.iter().find(|r| r["probe"] == format!("x:{p}")).unwrap();
    assert_eq!(of(p_strong)["search_rank"], 1);
    assert!(
        of(p_strong)["deep_rank"].is_null(),
        "в бюджете — без глубокого: {j}"
    );
    assert!(of(p_weak)["search_rank"].is_null());
    assert_eq!(of(p_weak)["deep_rank"], 2, "{j}");
    assert_eq!(of(p_weak)["search_cut"], 1);
    assert!(of(p_lost)["search_rank"].is_null() && of(p_lost)["deep_rank"].is_null());
    let m = &j["metrics"]["test"];
    assert!(
        (m["beyond"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9,
        "{m}"
    );
    assert!(
        (m["none"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9,
        "{m}"
    );
    let failing = j["metrics"]["failing"].as_array().unwrap();
    let advice: Vec<&str> = failing
        .iter()
        .filter_map(|f| f["advice"].as_str())
        .collect();
    assert!(advice.contains(&"term") && advice.iter().any(|a| *a == "ranking" || *a == "neighbor"));
}

/// §6.5 п. 5 на сервере: пять ночей — `baseline` N/5, шестая без изменений — `ok` (база 5/5);
/// статус — последний вердикт и тренд.
#[tokio::test]
async fn check_baseline_then_ok() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, _) = fixture("check-baseline", 6, |_| {}).await;
    base_nights(&a).await;
    let r = night(&a, &day(6)).await;
    assert_eq!(kind(&r), "ok", "{r}");
    assert_eq!(r["verdict"]["base_n"], 5);
    assert_eq!(r["verdict"]["base_of"], 5);
    assert_eq!(r["trigger"], "nightly");
    assert_eq!(r["night"], day(6));
    let s = a.t(Method::GET, "/api/v1/check/status", None).await;
    ok(&s, StatusCode::OK);
    let s = s.json();
    assert_eq!(s["last"]["verdict"]["kind"], "ok", "{s}");
    assert_eq!(s["trend"].as_array().unwrap().len(), 6);
    assert_eq!(s["trend"][0]["hit_search"], 1.0);
    assert_eq!(s["probes"]["by_kind"]["probe"], 6);
    assert_eq!(s["check"]["user"], "on");
    // повтор ночи — второго ночного прогона нет
    assert!(night_of(&a, a.st.admin_id, &day(6)).await.is_none());
}

/// §6.5 п. 5 (ревью Н3): три ночи без починки — регрессия и строка дайджеста держатся, эпизод
/// тот же, база прежняя (медиана — по прогонам до эпизода).
#[tokio::test]
async fn check_steady_regression_persists() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, notes) = fixture("check-steady", 12, |_| {}).await;
    base_nights(&a).await;
    assert!(
        digest(&a, "startup")
            .await
            .find("Ночная проверка")
            .is_none()
    );
    for (i, n) in notes.iter().enumerate().take(3) {
        damage(&a, n, i).await;
    }
    let mut episode = None;
    for n in 6..=8 {
        let r = night(&a, &day(n)).await;
        assert_eq!(kind(&r), "regression", "{r}");
        let ep = r["verdict"]["episode"].as_str().unwrap().to_string();
        assert_eq!(*episode.get_or_insert(ep.clone()), ep);
        assert_eq!(r["episode"], ep.as_str());
        assert_eq!(r["verdict"]["base_n"], 5);
        assert_eq!(r["verdict"]["compared_with"]["hit_search"], 1.0);
        assert_eq!(r["verdict"]["flags"][0]["name"], "lost_steady");
        let d = digest(&a, "startup").await;
        assert!(
            d.contains("Ночная проверка памяти: ухудшение поиска (потеря устойчивых: 3 из 12) — zam check report"),
            "{d}"
        );
    }
}

/// §6.5 п. 5: эпизод, начавшийся после смены версии сервера, — `after_update` с версией; строка
/// дайджеста — «после обновления до …».
#[tokio::test]
async fn check_regression_after_update_flagged() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    check::test_hooks::set_version(Some("0.0.16-m7.3"));
    let (a, notes) = fixture("check-after-update", 12, |_| {}).await;
    base_nights(&a).await;
    check::test_hooks::set_version(Some("0.0.17-m7.4"));
    for (i, n) in notes.iter().enumerate().take(4) {
        damage(&a, n, i).await;
    }
    let r = night(&a, &day(6)).await;
    assert_eq!(kind(&r), "regression", "{r}");
    assert_eq!(r["verdict"]["after_update"], true);
    assert_eq!(r["verdict"]["after_update_version"], "0.0.17-m7.4");
    assert_eq!(r["context"]["server"], "0.0.17-m7.4");
    let d = digest(&a, "startup").await;
    assert!(
        d.contains("ухудшение поиска после обновления до 0.0.17-m7.4"),
        "{d}"
    );
    let s = a.t(Method::GET, "/api/v1/check/status", None).await.json();
    let codes: Vec<&str> = s["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"after_update") && codes.contains(&"index"),
        "{s}"
    );
}

/// §6.5 п. 5 (решение 48): «Принять как норму» — только сессия WebUI (токен — 403, агент не
/// прячет регрессию), аудит `check.accept`; после принятия строки дайджеста нет, следующая ночь —
/// `baseline` с базой от принятого прогона.
#[tokio::test]
async fn check_accept_new_norm_human_only() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, notes) = fixture("check-accept", 12, |_| {}).await;
    base_nights(&a).await;
    for (i, n) in notes.iter().enumerate().take(3) {
        damage(&a, n, i).await;
    }
    let r = night(&a, &day(6)).await;
    assert_eq!(kind(&r), "regression");
    let run = r["run"].as_str().unwrap().to_string();
    assert!(
        digest(&a, "startup")
            .await
            .contains("Ночная проверка памяти")
    );
    let body = json!({"run": run});
    let t = a
        .t(Method::POST, "/api/v1/check/accept", Some(body.clone()))
        .await;
    assert_eq!(t.status, StatusCode::FORBIDDEN, "{}", t.text());
    let h = a
        .adm(Method::POST, "/api/v1/check/accept", Some(body), None)
        .await;
    ok(&h, StatusCode::OK);
    assert!(h.json()["accepted_at"].is_string());
    assert_eq!(h.json()["accepted_by"], "admin");
    assert!(
        !digest(&a, "startup")
            .await
            .contains("Ночная проверка памяти")
    );
    let audit: Vec<Value> =
        a.st.system
            .list_audit(100, None)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.action == "check.accept")
            .map(|r| r.details)
            .collect();
    assert_eq!(audit, vec![json!({"run": run, "verdict": "regression"})]);
    let next = night(&a, &day(7)).await;
    assert_eq!(kind(&next), "baseline", "{next}");
    assert_eq!(next["verdict"]["base_n"], 1);
}

/// §6.5 п. 5 (П1): принять можно только последний законченный ночной прогон открытого эпизода —
/// прежний прогон эпизода, прогон без регрессии и повторное принятие — 409.
#[tokio::test]
async fn check_accept_only_latest_episode_run() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, notes) = fixture("check-accept-latest", 12, |_| {}).await;
    base_nights(&a).await;
    let base = a
        .t(Method::GET, "/api/v1/check/runs?limit=1", None)
        .await
        .json()["items"][0]["run"]
        .as_str()
        .unwrap()
        .to_string();
    for (i, n) in notes.iter().enumerate().take(3) {
        damage(&a, n, i).await;
    }
    let first = night(&a, &day(6)).await["run"]
        .as_str()
        .unwrap()
        .to_string();
    let last = night(&a, &day(7)).await["run"]
        .as_str()
        .unwrap()
        .to_string();
    let accept = |run: String| {
        let a = &a;
        async move {
            a.adm(
                Method::POST,
                "/api/v1/check/accept",
                Some(json!({"run": run})),
                None,
            )
            .await
        }
    };
    problem(
        &accept(first).await,
        StatusCode::CONFLICT,
        "check-accept-not-latest",
    );
    problem(
        &accept(base).await,
        StatusCode::CONFLICT,
        "check-accept-not-latest",
    );
    ok(&accept(last.clone()).await, StatusCode::OK);
    problem(
        &accept(last).await,
        StatusCode::CONFLICT,
        "check-accept-not-latest",
    );
    let r = accept(Id::new().to_string()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// §6.5 п. 4: смена отпечатка (режим Qdrant в конфигурации) — `changed` с тем, что сменилось, без
/// флагов и строки; дальше база копится по новому отпечатку. Смена модели — тест с моделью ниже;
/// реранкер у сервера без эмбеддера берётся из прошлого прогона (ревью r1, Important 1), его смена
/// — модульный тест `check::run::tests`.
#[tokio::test]
async fn check_changed_on_fingerprint() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, notes) = fixture("check-changed", 12, |_| {}).await;
    base_nights(&a).await;
    for (i, n) in notes.iter().enumerate().take(3) {
        damage(&a, n, i).await;
    }
    assert_eq!(kind(&night(&a, &day(6)).await), "regression");
    let mut cfg = (*a.st.cfg).clone();
    // сервер теста — без модели: Qdrant не запускается, режим — только часть отпечатка
    cfg.qdrant.mode = zam_server::config::QdrantMode::Edge;
    let a = restart(a, cfg).await;
    let r = night(&a, &day(7)).await;
    assert_eq!(kind(&r), "changed", "{r}");
    assert_eq!(r["verdict"]["changed"]["what"], "qdrant");
    assert_eq!(r["verdict"]["changed"]["to"], "edge");
    assert_eq!(r["verdict"]["flags"], json!([]));
    assert!(r["verdict"]["episode"].is_null());
    assert!(
        !digest(&a, "startup")
            .await
            .contains("Ночная проверка памяти")
    );
    let next = night(&a, &day(8)).await;
    assert_eq!(
        (kind(&next), next["verdict"]["base_n"].as_u64()),
        ("baseline", Some(1))
    );
}

/// §6.5 пп. 4–5 (ревью r1, Important 1): прошлые ночные прогоны — на CUDA (устройство `cuda:0`,
/// реранкер GPU), текущий сервер без загруженного эмбеддера: отпечаток (модель, устройство,
/// **реранкер**) — из прошлого прогона, `fp_hash` прежний, вердикт — регрессия перехода в
/// `degraded` со строкой дайджеста, а не `changed`.
#[tokio::test]
async fn check_degraded_on_cuda_keeps_fingerprint() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    check::test_hooks::set_pause(Duration::ZERO);
    let (a, _) = fixture("check-cuda-degraded", 3, |_| {}).await;
    let fp = json!({
        "model": "bge-m3-fp16@1a2b3c4d",
        "device": "cuda:0",
        "reranker": a.st.cfg.ml.rerank_model_gpu,
        "qdrant": a.st.cfg.qdrant.mode,
        "check_version": check::CHECK_VERSION,
    });
    let hash = check::test_hooks::fp_hash(&fp);
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    for n in 1..=5 {
        let id = Id::new();
        zam_store::check::start_run(
            &db,
            id,
            Trigger::Nightly,
            Some(day(n).parse().unwrap()),
            ts(&format!("{}T04:31:00Z", day(n))),
        )
        .await
        .unwrap();
        let f = zam_store::check::RunFinish {
            status: zam_store::check::RunStatus::Done,
            fingerprint: Some(fp.clone()),
            fp_hash: Some(hash.clone()),
            // контекст — как у текущего сервера: эпизод не «после обновления», строка с флагом
            context: Some(check::test_hooks::context(&a.st)),
            metrics: Some(json!({"search_p95_ms": 10.0, "prompt_on": true, "degraded": false})),
            verdict: Some(json!({"kind": if n < 5 { "baseline" } else { "ok" }, "flags": []})),
            flags: Vec::new(),
            episode: None,
            error: None,
        };
        zam_store::check::finish_run(&db, id, f, Vec::new())
            .await
            .unwrap();
    }
    // сервер теста — без модели: поиск без векторов, эмбеддера и устройства нет
    assert!(a.st.index.model_id().is_none() && a.st.index.device.is_none());
    let r = night(&a, &day(6)).await;
    assert_eq!(r["fp_hash"], hash.as_str(), "{r}");
    assert_eq!(r["fingerprint"]["device"], "cuda:0");
    assert_eq!(
        r["fingerprint"]["reranker"],
        a.st.cfg.ml.rerank_model_gpu.as_str()
    );
    assert_eq!(kind(&r), "regression", "{r}");
    assert_eq!(r["verdict"]["after_update"], false, "{r}");
    assert_eq!(r["verdict"]["flags"][0]["name"], "degraded");
    assert!(
        digest(&a, "startup")
            .await
            .contains("(degraded: поиск без векторов)")
    );
}

/// Review Focus 5: прогон, оборванный остановкой (`running` в базе), не мешает ручному
/// `POST /check/run` после перезапуска — 202 и законченный прогон.
#[tokio::test]
async fn check_interrupted_run_not_blocking_manual() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, _) = fixture("check-interrupted-manual", 2, |_| {}).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    zam_store::check::start_run(
        &db,
        Id::new(),
        Trigger::Manual,
        None,
        zam_store::time::now(),
    )
    .await
    .unwrap();
    drop(db);
    let cfg = (*a.st.cfg).clone();
    let a = restart(a, cfg).await;
    let r = a.t(Method::POST, "/api/v1/check/run", None).await;
    ok(&r, StatusCode::ACCEPTED);
    let run: Id = r.json()["run"].as_str().unwrap().parse().unwrap();
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let t0 = Instant::now();
    loop {
        let r = zam_store::check::get_run(&db, run).await.unwrap();
        if r.as_ref().is_some_and(|r| r.status.as_str() == "done") {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "ручной прогон не закончился: {r:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let runs = zam_store::check::runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs.iter()
            .any(|x| x.error.as_deref() == Some("interrupted"))
    );
}

/// §6.5 п. 1: идёт переиндексация — прогон `skipped` без метрик и вердикта; ночью не повторяется.
#[tokio::test]
async fn check_skipped_on_reindex() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    check::test_hooks::set_reindexing(Some(true));
    let (a, _) = fixture("check-reindex", 2, |_| {}).await;
    let r = night(&a, &day(1)).await;
    assert_eq!(r["status"], "skipped", "{r}");
    assert!(r["verdict"].is_null() && r["metrics"].is_null());
    assert!(night_of(&a, a.st.admin_id, &day(1)).await.is_none());
}

/// §6.5 п. 1: идёт копия — ночной прогон ждёт её конца (опрос — тестовая точка), затем идёт.
#[tokio::test]
async fn check_waits_backup() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    check::test_hooks::set_backup_poll(Duration::from_millis(10));
    let (a, _) = fixture("check-backup-wait", 2, |_| {}).await;
    let guard = a.st.backups.try_start(RunBy::WebUi).unwrap();
    let _clock = freeze_now(ts("2026-11-01T04:31:00Z"));
    let ctx = NightCtx::new(a.st.clone());
    let user = a.st.admin_id;
    let task =
        tokio::spawn(
            async move { check::run_night(&ctx, user, "2026-11-01".parse().unwrap()).await },
        );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "прогон ждёт копию");
    let db = a.st.users.get(user).await.unwrap();
    assert!(zam_store::check::runs(&db, 10).await.unwrap().is_empty());
    guard.release();
    let run = task.await.unwrap().unwrap().expect("прогон после копии");
    drop(_clock);
    let j = run_json(&a, &run.to_string()).await;
    assert_eq!(j["status"], "done");
    assert_eq!(j["flags"], json!([]));
}

/// §6.5 пп. 1, 5: копия, начавшаяся посреди прогона, — флаг `backup_overlap` (задержки этого
/// прогона не сравниваются — `verdict::tests::check_thresholds`).
#[tokio::test]
async fn check_backup_overlap_flag() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    check::test_hooks::set_pause(Duration::from_millis(60));
    let (a, _) = fixture("check-backup-overlap", 5, |_| {}).await;
    let ctx = NightCtx::new(a.st.clone());
    let user = a.st.admin_id;
    let task = tokio::spawn(async move { check::run_now(&ctx, user, Trigger::Manual).await });
    let db = a.st.users.get(user).await.unwrap();
    let t0 = Instant::now();
    while zam_store::check::runs(&db, 1).await.unwrap().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    let guard = a.st.backups.try_start(RunBy::Schedule).unwrap();
    let run = task.await.unwrap().unwrap();
    guard.release();
    let j = run_json(&a, &run.to_string()).await;
    assert_eq!(j["flags"], json!(["backup_overlap"]), "{j}");
}

/// §6.5 п. 1: предел прогона (`check_max_min`, тестовая точка — мс) превышен — `timeout`, метрики
/// по пройденным пробам, без вердикта.
#[tokio::test]
async fn check_timeout_partial() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    check::test_hooks::set_pause(Duration::from_millis(100));
    check::test_hooks::set_max_run(Duration::from_millis(150));
    let (a, _) = fixture("check-timeout", 6, |_| {}).await;
    let ctx = NightCtx::new(a.st.clone());
    let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    let j = run_json(&a, &run.to_string()).await;
    assert_eq!(j["status"], "timeout", "{j}");
    assert!(j["verdict"].is_null());
    let n = j["metrics"]["all"]["n"].as_u64().unwrap();
    assert!((1..6).contains(&n), "пройдено {n} из 6: {j}");
    assert_eq!(j["results"].as_array().unwrap().len() as u64, n);
}

/// §6.5 п. 1: прогон, оборванный остановкой сервера (`running` в базе), при следующем старте —
/// `failed` с `error = "interrupted"` и ночным прогоном окна не считается: прогон той же ночи
/// после перезапуска идёт.
#[tokio::test]
async fn check_interrupted_run_recovered() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, _) = fixture("check-interrupted", 2, |_| {}).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let stale = Id::new();
    zam_store::check::start_run(
        &db,
        stale,
        Trigger::Nightly,
        Some(day(1).parse().unwrap()),
        zam_store::time::now(),
    )
    .await
    .unwrap();
    drop(db);
    let cfg = (*a.st.cfg).clone();
    let a = restart(a, cfg).await;
    // первое обращение к прогонам базы после старта — прерванный прогон уже `failed`
    let s = a.adm(Method::GET, "/api/v1/check/status", None, None).await;
    ok(&s, StatusCode::OK);
    assert!(s.json()["running"].is_null(), "{}", s.text());
    assert_eq!(s.json()["last"]["status"], "failed");
    assert_eq!(s.json()["last"]["error"], "interrupted");
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let r = zam_store::check::get_run(&db, stale)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, zam_store::check::RunStatus::Failed);
    assert!(
        !zam_store::check::has_nightly(&db, day(1).parse().unwrap())
            .await
            .unwrap()
    );
    let r = night(&a, &day(1)).await;
    assert_eq!(r["status"], "done", "{r}");
}

/// §6.5 п. 2: `hooks.prompt = false` у пользователя — подсказки проверке не обходят настройку:
/// `hit_prompt` и `negatives_shown` — `null`, посторонние вопросы не идут.
#[tokio::test]
async fn check_prompt_metrics_null_when_hooks_off() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, _) = fixture("check-hooks-off", 3, |_| {}).await;
    let r = a
        .adm(
            Method::PATCH,
            &format!("/api/v1/admin/users/{}/settings", a.admin_id()),
            Some(json!({"hooks": {"prompt": false}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    let ctx = NightCtx::new(a.st.clone());
    let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    let j = run_json(&a, &run.to_string()).await;
    let m = &j["metrics"];
    assert_eq!(m["prompt_on"], false);
    assert!(
        m["test"]["hit_prompt"].is_null() && m["negatives_shown"].is_null(),
        "{m}"
    );
    assert_eq!(m["negatives_n"], 0);
    assert!(
        j["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] == "probe")
    );
    assert!(m["prompt_p95_ms"].is_null());
}

/// §6.5 п. 2 (решения 63, 69): посторонние вопросы — в области каждого решения пользователя с
/// пробами (строка результатов на решение), без решения — если таких решений нет.
#[tokio::test]
async fn check_negatives_per_solution() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let a = api("check-neg-solutions", |_| {}).await;
    let guids = [
        "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e01",
        "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e02",
    ];
    let mut sols = Vec::new();
    for (k, g) in guids.iter().enumerate() {
        let r = a
            .t(
                Method::POST,
                "/api/v1/solutions",
                Some(json!({"agent_solution_id": g, "name": format!("Решение {k}")})),
            )
            .await;
        assert!(r.status.is_success(), "{}", r.text());
        let n = id(&a.note(g, "readme.md", &note_text(k)).await);
        let db = a.st.users.get(a.st.admin_id).await.unwrap();
        let space = zam_store::list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.solution_guid.as_deref() == Some(g))
            .unwrap()
            .id;
        add_probe(&a, a.st.admin_id, space, &question(k), &format!("n:{n}")).await;
        sols.push(space);
    }
    let ctx = NightCtx::new(a.st.clone());
    let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    let j = run_json(&a, &run.to_string()).await;
    let neg: Vec<&Value> = j["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "negative")
        .collect();
    let per = check::negatives::NEGATIVES_COUNT;
    assert_eq!(neg.len(), 2 * per, "{j}");
    for s in &sols {
        assert_eq!(
            neg.iter().filter(|r| r["space"] == s.to_string()).count(),
            per
        );
    }
    assert_eq!(j["metrics"]["negatives_n"], 2 * per);
    // без решений — посторонние вопросы без решения (`space` пуст)
    let (b, _) = fixture("check-neg-none", 1, |_| {}).await;
    let ctx = NightCtx::new(b.st.clone());
    let run = check::run_now(&ctx, b.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    let j = run_json(&b, &run.to_string()).await;
    let neg: Vec<&Value> = j["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "negative")
        .collect();
    assert_eq!(neg.len(), per);
    assert!(neg.iter().all(|r| r["space"].is_null()));
}

/// §6.5 п. 7 (сторож бюджета): строка дайджеста — только при регрессии, на первом
/// `session_start` сессии (не после сжатия, возобновления и очистки, не к запросу); дайджест со
/// строкой — в пределах `recall.digest_tokens`; эпизод снят — строки нет.
#[tokio::test]
async fn digest_regression_line_only_when_regressed() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, notes) = fixture("check-digest-line", 12, |_| {}).await;
    base_nights(&a).await;
    const LINE: &str = "Ночная проверка памяти";
    assert!(!digest(&a, "startup").await.contains(LINE));
    for (i, n) in notes.iter().enumerate().take(3) {
        damage(&a, n, i).await;
    }
    assert_eq!(kind(&night(&a, &day(6)).await), "regression");
    let d = digest(&a, "startup").await;
    assert!(d.contains(LINE), "{d}");
    let budget = a.st.cfg.recall.digest_tokens;
    assert!(
        zam_store::count_tokens(&d) <= budget,
        "{} > {budget}",
        zam_store::count_tokens(&d)
    );
    let line = d.lines().find(|l| l.contains(LINE)).unwrap();
    assert!(zam_store::count_tokens(line) <= 30, "{line}");
    for source in ["compact", "resume", "clear"] {
        assert!(!digest(&a, source).await.contains(LINE), "{source}");
    }
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "prompt", "prompt": "z0alpha z0bravo что там"})),
        )
        .await;
    assert!(!r.text().contains(LINE));
    // ночь без регрессии (заметки починены) — эпизод снят, строки нет
    for (i, n) in notes.iter().enumerate().take(3) {
        let rev = rev_of(&a, n).await;
        ok(
            &a.tok_h(
                &a.token,
                Method::PUT,
                &format!("/api/v1/nodes/{n}"),
                Some(json!({"content": note_text(i)})),
                &[("if-match", rev.as_str())],
            )
            .await,
            StatusCode::OK,
        );
    }
    assert_eq!(kind(&night(&a, &day(7)).await), "ok");
    assert!(!digest(&a, "startup").await.contains(LINE));
}

/// §6.5 п. 7 (ревью r1, Minor 6): строка дайджеста — не длиннее 30 токенов точным счётчиком
/// bge-m3 (им сервер считает бюджеты) во всех вариантах, в том числе самом длинном — «после
/// обновления до <версия>» с флагом без `hit`; ru и en.
#[tokio::test]
async fn digest_line_fits_30_tokens() {
    use zam_markdown::TokenCounter;
    let Some(counter) = common::ml::bge_counter() else {
        return;
    };
    assert!(counter.is_exact());
    let mut long = Vec::new();
    for after in [false, true] {
        for flag in [
            // крайние числа: самые длинные записи
            json!({"name": "hit_search", "value": 0.0, "median": 1.0, "delta_pp": -100.0}),
            json!({"name": "lost_steady", "lost": 4000, "steady": 40000}),
            json!({"name": "p95", "value": 180000.0, "median": 90000.0}),
            json!({"name": "negatives_shown", "value": 1.0, "median": 0.0, "delta_pp": 100.0}),
            json!({"name": "degraded"}),
        ] {
            let mut v = json!({"kind": "regression", "flags": [flag], "after_update": after});
            if after {
                v["after_update_version"] = json!("0.0.17-m7.10");
            }
            let v = check::verdict::from_json(&v).unwrap();
            for lang in [check::verdict::LineLang::Ru, check::verdict::LineLang::En] {
                let line = check::verdict::digest_line(&v, lang).unwrap();
                let n = counter.count(&line);
                if n > 30 {
                    long.push(format!("{n}: {line}"));
                }
            }
        }
    }
    assert!(long.is_empty(), "длиннее 30 токенов:\n{}", long.join("\n"));
}

/// §6.5 п. 1: ручной прогон — 202 (человек или токен `rw`; `ro` — 403); второй, пока идёт
/// первый, — 409 `/problems/check-running`; `shutdown` с прогоном в полёте укладывается в срок
/// раннера и освобождает базу, прогон — `failed` (`interrupted`).
#[tokio::test]
async fn check_run_409_concurrent() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    check::test_hooks::set_pause(Duration::from_millis(300));
    let (a, _) = fixture("check-409", 8, |_| {}).await;
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a.tok(&ro, Method::POST, "/api/v1/check/run", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = a.t(Method::POST, "/api/v1/check/run", None).await;
    ok(&r, StatusCode::ACCEPTED);
    let run = r.json()["run"].as_str().unwrap().to_string();
    let again = a.t(Method::POST, "/api/v1/check/run", None).await;
    problem(&again, StatusCode::CONFLICT, "check-running");
    assert_eq!(again.json()["run"], run.as_str());
    let human = a.adm(Method::POST, "/api/v1/check/run", None, None).await;
    problem(&human, StatusCode::CONFLICT, "check-running");
    // прогон в полёте — остановка сервера
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let t0 = Instant::now();
    while zam_store::check::runs(&db, 1).await.unwrap().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(db);
    let path = a.st.data_dir.user_db(&a.st.admin_id);
    let cfg = (*a.st.cfg).clone();
    let t0 = Instant::now();
    a.st.shutdown().await;
    assert!(
        t0.elapsed() <= Duration::from_secs(6),
        "shutdown {:?}",
        t0.elapsed()
    );
    drop(a);
    let moved = path.with_extension("db.moved");
    let t0 = Instant::now();
    loop {
        match std::fs::rename(&path, &moved) {
            Ok(()) => break,
            Err(e) => assert!(t0.elapsed() < Duration::from_secs(3), "база открыта: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    std::fs::rename(&moved, &path).unwrap();
    let a = api_with(cfg).await;
    let r = a
        .adm(
            Method::GET,
            &format!("/api/v1/check/runs/{run}"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["status"], "failed", "{}", r.text());
    assert_eq!(r.json()["error"], "interrupted");
}

/// §6.5 п. 7: прогоны хранятся 365 суток от `started_at` (воркер обслуживания) — вместе с их
/// результатами.
#[tokio::test]
async fn check_runs_retention_365_days() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let (a, _) = fixture("check-retention", 2, |_| {}).await;
    let ctx = NightCtx::new(a.st.clone());
    let old = {
        let _clock = freeze_now(ts("2025-11-01T04:31:00Z"));
        check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
            .await
            .unwrap()
    };
    let fresh = {
        let _clock = freeze_now(ts("2026-11-01T04:31:00Z"));
        check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
            .await
            .unwrap()
    };
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    assert!(
        !zam_store::check::results(&db, old)
            .await
            .unwrap()
            .is_empty()
    );
    let n = zam_server::maintenance::gc_check_runs(&a.st, ts("2026-11-23T00:00:00Z")).await;
    assert_eq!(n, 1);
    assert!(zam_store::check::get_run(&db, old).await.unwrap().is_none());
    assert!(
        zam_store::check::results(&db, old)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        zam_store::check::get_run(&db, fresh)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        zam_server::maintenance::CHECK_RUNS_TTL,
        SignedDuration::from_hours(24 * 365)
    );
}

/// Окно ночи вокруг «сейчас» (UTC, `at` — минутой раньше).
fn night_now(c: &mut ServerConfig, check: CheckMode) {
    window_at(c, -1, check);
}

/// Окно ночи с началом `at` = сейчас + `mins` минут (UTC), 2 ч.
fn window_at(c: &mut ServerConfig, mins: i64, check: CheckMode) {
    let at = (Timestamp::now() + SignedDuration::from_mins(mins)).to_zoned(TimeZone::UTC);
    c.works.at = at.strftime("%H:%M").to_string();
    c.works.tz = "UTC".into();
    c.works.window_min = 120;
    c.works.check = check;
}

/// Тик ночи 20 мс (раннеры, запущенные после вызова); флаг — работа очереди ночи прошла (она
/// идёт после проверки всех пользователей).
fn queue_signal() -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    night_hooks::set_tick(Duration::from_millis(20));
    night_hooks::set_jobs(Jobs {
        check: None,
        queue: Some(Arc::new(move |_, _, _| {
            d.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        })),
    });
    done
}

async fn wait_flag(f: &AtomicBool) {
    let t0 = Instant::now();
    while !f.load(Ordering::SeqCst) {
        assert!(t0.elapsed() < Duration::from_secs(30), "ночь не прошла");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_run(a: &Api, user: Id, trigger: &str) -> zam_store::check::CheckRun {
    let db = a.st.users.get(user).await.unwrap();
    let t0 = Instant::now();
    loop {
        let runs = zam_store::check::runs(&db, 10).await.unwrap();
        if let Some(r) = runs.into_iter().find(|r| {
            r.trigger.as_str() == trigger && r.status != zam_store::check::RunStatus::Running
        }) {
            return r;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "нет прогона {trigger}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Решение 76: `[works] check = "off"` и `works.check = off` у пользователя выключают только
/// ночной прогон — ручной `POST /check/run` идёт; `[works] enabled = false` — 409
/// `/problems/works-disabled`.
#[tokio::test]
async fn check_off_by_config_and_user() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    // 1. сервер: `check = "off"` — ночь прошла, ночного прогона нет; ручной — есть
    let done = queue_signal();
    let (a, _) = fixture("check-off-server", 2, |c| night_now(c, CheckMode::Off)).await;
    wait_flag(&done).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    assert!(zam_store::check::runs(&db, 10).await.unwrap().is_empty());
    ok(
        &a.t(Method::POST, "/api/v1/check/run", None).await,
        StatusCode::ACCEPTED,
    );
    assert_eq!(
        wait_run(&a, a.st.admin_id, "manual").await.status.as_str(),
        "done"
    );
    a.st.shutdown().await;
    drop(a);
    // 2. пользователь: `works.check = off` — у него ночного прогона нет, у администратора — есть.
    // Данные — до ночи: сервер с окном через 6 ч, затем перезапуск с окном «сейчас»
    night_hooks::reset();
    let (a, _) = fixture("check-off-user", 2, |c| {
        window_at(c, 360, CheckMode::Nightly)
    })
    .await;
    let (uid, utok) = a.new_user("bob").await;
    let user: Id = uid.parse().unwrap();
    ok(
        &a.adm(
            Method::PATCH,
            &format!("/api/v1/admin/users/{uid}/settings"),
            Some(json!({"works": {"check": "off"}})),
            None,
        )
        .await,
        StatusCode::OK,
    );
    let kn = space_of(&a, user, SpaceKind::Knowledge).await;
    let r = a
        .tok(
            &utok,
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "kb/b.md", "kind": "note",
                        "content": note_text(0)}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    add_probe(&a, user, kn, &question(0), &format!("n:{}", id(&r.json()))).await;
    let mut cfg = (*a.st.cfg).clone();
    night_now(&mut cfg, CheckMode::Nightly);
    let done = queue_signal();
    let a = restart(a, cfg).await;
    wait_flag(&done).await;
    assert_eq!(
        wait_run(&a, a.st.admin_id, "nightly").await.status.as_str(),
        "done"
    );
    let db = a.st.users.get(user).await.unwrap();
    assert!(
        zam_store::check::runs(&db, 10).await.unwrap().is_empty(),
        "works.check = off — ночного прогона нет"
    );
    ok(
        &a.tok(&utok, Method::POST, "/api/v1/check/run", None).await,
        StatusCode::ACCEPTED,
    );
    assert_eq!(wait_run(&a, user, "manual").await.status.as_str(), "done");
    a.st.shutdown().await;
    drop(a);
    // 3. `[works] enabled = false` — ручной прогон 409
    let (a, _) = fixture("check-off-disabled", 1, |c| c.works.enabled = false).await;
    problem(
        &a.t(Method::POST, "/api/v1/check/run", None).await,
        StatusCode::CONFLICT,
        "works-disabled",
    );
}

/// §6 «Общее» («Маскировка»): `GET /check/status`, `/check/runs/{id}`, `/check/probes` токену —
/// вопрос пробы с `‹secret›`, человеку (сессия WebUI) — исходник.
#[tokio::test]
async fn check_runs_masked_for_token() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    quick();
    let a = api("check-masked", |_| {}).await;
    let kn = space_of(&a, a.st.admin_id, SpaceKind::Knowledge).await;
    let n = id(&a.note("knowledge", "kb/x.md", "# X\n\nничего.\n").await);
    // фиктивный ключ — склейкой во время выполнения (push protection)
    let secret = format!("{}{}", "ghp_", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
    let q = format!("где лежит ключ {secret} для сборки");
    add_probe(&a, a.st.admin_id, kn, &q, &format!("n:{n}")).await;
    let ctx = NightCtx::new(a.st.clone());
    let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
        .await
        .unwrap();
    for uri in [
        "/api/v1/check/status".to_string(),
        format!("/api/v1/check/runs/{run}"),
        "/api/v1/check/probes?kind=probe".to_string(),
    ] {
        let t = a.t(Method::GET, &uri, None).await;
        ok(&t, StatusCode::OK);
        assert!(!t.text().contains(&secret), "{uri}: {}", t.text());
        assert!(t.text().contains("‹secret›"), "{uri}: {}", t.text());
        let h = a.adm(Method::GET, &uri, None, None).await;
        ok(&h, StatusCode::OK);
        assert!(h.text().contains(&secret), "{uri}: {}", h.text());
    }
}

// ---------------- с моделью (MiniLM, Qdrant) ----------------

mod ml {
    use super::*;
    use common::ml::{MINILM, external, have, ml, until, vector_ready};
    use common::qdrant::TestQdrant;

    /// Заметки на английском и вопросы без общих слов и подстрок (ни `fts`, ни `trigram`):
    /// цель находит только векторный канал.
    pub const NOTES: [(&str, &str); 6] = [
        (
            "Rollout",
            "Blue green switching: twin server pools, traffic swap.",
        ),
        (
            "Snapshots",
            "Nightly postgres dumps travel to cold storage abroad.",
        ),
        (
            "Espresso",
            "Descale the espresso maker monthly using citric acid.",
        ),
        (
            "Leave",
            "Employees get twenty eight paid vacation days annually.",
        ),
        (
            "Garage",
            "After nine pm cars stay on the lower parking level.",
        ),
        (
            "Printer",
            "Paper stuck: open tray two, pull sheet gently outward.",
        ),
    ];
    pub const QUESTIONS: [&str; 6] = [
        "how is fresh code shipped safely",
        "where do backups of databases live",
        "kitchen appliance limescale cleaning frequency",
        "holiday allowance per worker",
        "where should vehicles remain overnight",
        "document blocked inside office hardware",
    ];

    pub async fn server(
        name: &str,
        q: &TestQdrant,
        model: Option<&str>,
        dir: Option<std::path::PathBuf>,
    ) -> Api {
        let tweak = |c: &mut ServerConfig| {
            if let Some(m) = model {
                ml(c, m);
            }
            external(c, q);
            c.recall.timeout_ms = 30_000;
        };
        match dir {
            None => api(name, tweak).await,
            Some(d) => {
                let mut c = common::cfg_in(d);
                tweak(&mut c);
                api_with(c).await
            }
        }
    }

    /// Заметки и пробы; ждём векторной индексации.
    pub async fn notes(a: &Api) -> Vec<String> {
        let kn = space_of(a, a.st.admin_id, SpaceKind::Knowledge).await;
        let mut out = Vec::new();
        for (i, (t, b)) in NOTES.iter().enumerate() {
            let n = id(&a
                .note(
                    "knowledge",
                    &format!("kb/{i}.md"),
                    &format!("# {t}\n\n{b}\n"),
                )
                .await);
            add_probe(a, a.st.admin_id, kn, QUESTIONS[i], &format!("n:{n}")).await;
            out.push(n);
        }
        let db = a.st.users.get(a.st.admin_id).await.unwrap();
        until(
            "заметки проиндексированы",
            Duration::from_secs(240),
            async || {
                a.st.index.worker.notify();
                let mut all = true;
                for n in &out {
                    let node = zam_store::get_node(&db, n.parse().unwrap()).await.unwrap();
                    all &= node.index_state == "indexed";
                }
                all
            },
        )
        .await;
        out
    }

    /// §6.5 п. 2 (решение 47): ранги прогона совпадают с `/search` агента по токену с
    /// умолчаниями (гибрид с векторами).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn check_same_path_as_agent() {
        let _serial = SERIAL.lock().await;
        let _reset = Reset;
        check::test_hooks::set_pause(Duration::ZERO);
        if !have(MINILM) {
            return;
        }
        let q = TestQdrant::start("check-same-path");
        let a = server("check-same-path", &q, Some(MINILM), None).await;
        vector_ready(&a).await;
        let notes = notes(&a).await;
        let ctx = NightCtx::new(a.st.clone());
        let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
            .await
            .unwrap();
        let j = run_json(&a, &run.to_string()).await;
        assert_eq!(j["metrics"]["degraded"], false, "{j}");
        let res = j["results"].as_array().unwrap();
        for (i, n) in notes.iter().enumerate() {
            let r = a
                .t(
                    Method::POST,
                    "/api/v1/search",
                    Some(json!({"query": QUESTIONS[i], "format": "json"})),
                )
                .await;
            ok(&r, StatusCode::OK);
            assert_eq!(r.json()["degraded"], false);
            let items = r.json()["items"].as_array().cloned().unwrap();
            let rank = items
                .iter()
                .position(|it| it["node_id"] == n.as_str())
                .map(|p| p + 1);
            let row = res
                .iter()
                .find(|x| x["targets"][0]["ref"] == format!("n:{n}"))
                .unwrap();
            assert_eq!(
                row["search_rank"].as_u64(),
                rank.map(|r| r as u64),
                "{i}: {row}"
            );
            assert_eq!(row["search_cut"].as_u64(), Some(items.len() as u64));
        }
    }

    /// §6.5 п. 5: точки Qdrant части целей удалены — `regression` с флагом «потеря устойчивых» и
    /// советом «проверьте индекс».
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn check_regression_on_index_damage() {
        let _serial = SERIAL.lock().await;
        let _reset = Reset;
        check::test_hooks::set_pause(Duration::ZERO);
        if !have(MINILM) {
            return;
        }
        let q = TestQdrant::start("check-index-damage");
        let a = server("check-index-damage", &q, Some(MINILM), None).await;
        vector_ready(&a).await;
        let notes = notes(&a).await;
        base_nights(&a).await;
        let steady =
            a.t(Method::GET, "/api/v1/check/status", None).await.json()["trend"][0]["hit_search"]
                .as_f64()
                .unwrap();
        assert!(steady >= 0.99, "все цели находятся векторами: {steady}");
        let v = a.st.index.vector.clone().unwrap();
        for n in notes.iter().take(4) {
            v.delete_node(a.st.admin_id, n.parse().unwrap())
                .await
                .unwrap();
        }
        let r = night(&a, &day(6)).await;
        assert_eq!(kind(&r), "regression", "{r}");
        assert!(
            r["verdict"]["flags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["name"] == "lost_steady"),
            "{r}"
        );
        let s = a.t(Method::GET, "/api/v1/check/status", None).await.json();
        assert!(
            s["advice"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["code"] == "index"
                    && x["text"].as_str().unwrap().contains("zam-server reindex")),
            "{s}"
        );
    }

    /// §6.5 п. 5: переход «с векторами → без» (сервер перезапущен без модели) — регрессия со
    /// строкой дайджеста; следующая ночь без векторов — `degraded` без строки; отпечаток прежний
    /// (без модели — модель прошлого прогона).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn check_degraded_transition_then_steady() {
        let _serial = SERIAL.lock().await;
        let _reset = Reset;
        check::test_hooks::set_pause(Duration::ZERO);
        if !have(MINILM) {
            return;
        }
        let q = TestQdrant::start("check-degraded");
        let a = server("check-degraded", &q, Some(MINILM), None).await;
        vector_ready(&a).await;
        notes(&a).await;
        base_nights(&a).await;
        let dir = a.st.cfg.data_dir.clone();
        let fp = night(&a, &day(6)).await["fp_hash"].clone();
        let token = a.token.clone();
        a.st.shutdown().await;
        drop(a);
        let mut a = server("check-degraded", &q, None, Some(dir)).await;
        a.token = token;
        let r = night(&a, &day(7)).await;
        assert_eq!(r["fp_hash"], fp, "{r}");
        assert_eq!(kind(&r), "regression", "{r}");
        assert_eq!(r["verdict"]["flags"][0]["name"], "degraded");
        assert!(
            digest(&a, "startup")
                .await
                .contains("(degraded: поиск без векторов)")
        );
        let r = night(&a, &day(8)).await;
        assert_eq!(kind(&r), "degraded", "{r}");
        assert!(
            !digest(&a, "startup")
                .await
                .contains("Ночная проверка памяти")
        );
    }

    /// Замер (журнал этапа, §6.5 «Замер»): ручной прогон 100 проб на сервере с моделью
    /// (MiniLM, CPU, пауза 25 мс по умолчанию) — время прогона и одной пробы. Не входит в обычный
    /// прогон: `cargo test -p zam-server --test check_run -- --ignored check_timing_100_probes`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "замер"]
    async fn check_timing_100_probes() {
        let _serial = SERIAL.lock().await;
        let _reset = Reset;
        if !have(MINILM) {
            return;
        }
        let q = TestQdrant::start("check-timing");
        let a = server("check-timing", &q, Some(MINILM), None).await;
        vector_ready(&a).await;
        let kn = space_of(&a, a.st.admin_id, SpaceKind::Knowledge).await;
        let mut ids = Vec::new();
        for i in 0..100 {
            let (t, b) = NOTES[i % NOTES.len()];
            let n = id(&a
                .note(
                    "knowledge",
                    &format!("kb/t{i}.md"),
                    &format!("# {t} {i}\n\n{b} z{i}alpha.\n"),
                )
                .await);
            add_probe(
                &a,
                a.st.admin_id,
                kn,
                &format!("{} z{i}alpha", QUESTIONS[i % 6]),
                &format!("n:{n}"),
            )
            .await;
            ids.push(n);
        }
        let db = a.st.users.get(a.st.admin_id).await.unwrap();
        until(
            "заметки проиндексированы",
            Duration::from_secs(600),
            async || {
                a.st.index.worker.notify();
                let mut all = true;
                for n in &ids {
                    all &= zam_store::get_node(&db, n.parse().unwrap())
                        .await
                        .unwrap()
                        .index_state
                        == "indexed";
                }
                all
            },
        )
        .await;
        let ctx = NightCtx::new(a.st.clone());
        let t0 = Instant::now();
        let run = check::run_now(&ctx, a.st.admin_id, Trigger::Manual)
            .await
            .unwrap();
        let took = t0.elapsed();
        let j = run_json(&a, &run.to_string()).await;
        let m = &j["metrics"];
        eprintln!(
            "ZAM_TIMING проверка памяти: 100 проб + {} посторонних за {took:?} ({:.0} мс на запрос с паузой 25 мс); поиск p50 {} p95 {} мс, подсказки p50 {} p95 {} мс; hit_search {}",
            m["negatives_n"],
            took.as_secs_f64() * 1000.0 / 130.0,
            m["search_p50_ms"],
            m["search_p95_ms"],
            m["prompt_p50_ms"],
            m["prompt_p95_ms"],
            m["all"]["hit_search"]
        );
        assert_eq!(j["status"], "done");
    }

    /// §6.5 п. 4: смена модели эмбеддингов — `changed` (`what = model`), эпизод закрыт.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn check_changed_on_fingerprint_model() {
        let _serial = SERIAL.lock().await;
        let _reset = Reset;
        check::test_hooks::set_pause(Duration::ZERO);
        if !have(MINILM) {
            return;
        }
        // каталог моделей теста: MiniLM и её копия под другим id
        let models = common::tmp("check-changed-models");
        for (to, id) in [(MINILM, MINILM), ("minilm-copy", "minilm-copy")] {
            let src = common::ml::models().join(MINILM);
            let dst = models.join(to);
            std::fs::create_dir_all(&dst).unwrap();
            for e in std::fs::read_dir(&src).unwrap() {
                let e = e.unwrap();
                std::fs::copy(e.path(), dst.join(e.file_name())).unwrap();
            }
            let spec = std::fs::read_to_string(dst.join("model.toml")).unwrap();
            std::fs::write(
                dst.join("model.toml"),
                spec.replace(&format!("id = \"{MINILM}\""), &format!("id = \"{id}\"")),
            )
            .unwrap();
        }
        let q = TestQdrant::start("check-changed-model");
        let with = |m: &'static str, dir: Option<std::path::PathBuf>| {
            let models = models.clone();
            let q = &q;
            async move {
                let tweak = |c: &mut ServerConfig| {
                    ml(c, m);
                    c.ml.models_dir = Some(models.clone());
                    external(c, q);
                    c.recall.timeout_ms = 30_000;
                };
                match dir {
                    None => api("check-changed-model", tweak).await,
                    Some(d) => {
                        let mut c = common::cfg_in(d);
                        tweak(&mut c);
                        api_with(c).await
                    }
                }
            }
        };
        let a = with(MINILM, None).await;
        vector_ready(&a).await;
        notes(&a).await;
        base_nights(&a).await;
        let dir = a.st.cfg.data_dir.clone();
        let token = a.token.clone();
        a.st.shutdown().await;
        drop(a);
        let mut a = with("minilm-copy", Some(dir)).await;
        a.token = token;
        let r = night(&a, &day(6)).await;
        assert_eq!(kind(&r), "changed", "{r}");
        assert_eq!(r["verdict"]["changed"]["what"], "model");
        assert!(
            r["verdict"]["changed"]["to"]
                .as_str()
                .unwrap()
                .starts_with("minilm-copy"),
            "{r}"
        );
    }
}
