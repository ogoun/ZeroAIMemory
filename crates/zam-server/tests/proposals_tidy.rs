//! Уборка памяти (M7.5, спецификация §8.7 «Проверка»): сборщик кандидатов (почти-дубли по окну
//! косинуса, противоречия, устаревшие, шумные, непроходящие пробы), срез за ночь и порядок типов,
//! отсев повторов и отклонённых человеком, вид работы `tidy` (проверка операций, маска секрета,
//! удаления нет), применение человеком (`merge` с текстом и без, `retract`, `revisit`,
//! `triggers`, `term`, `append`; ревизия не та — `stale` и 409; токену — 403), ничего без
//! человека, Human не собирается, отсев — один раз на запуск вида.
//!
//! Векторы почти-дублей кладутся прямо в кэш `embeddings` под тестовой моделью
//! (`works::tidy::test_hooks::set_model`); ни модели, ни Qdrant. Давность (`revisit` 14 суток,
//! `contradicts` 3 суток, начало учёта открытий) — правкой времени строк в SQLite, без заморозки
//! часов. Тесты файла идут **по одному** (`SERIAL`): тестовые точки общие для процесса.
mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api, ok};
use jiff::SignedDuration;
use serde_json::{Value, json};
use zam_core::split::Split;
use zam_core::{Id, TargetKind, TargetRef};
use zam_server::config::ServerConfig;
use zam_server::works::package::MASK_LITERAL_REASON;
use zam_server::works::{NightCtx, queue};
use zam_store::works::{self as store, WorkItem, WorkKind};

const PROPOSALS: &str = "/api/v1/proposals";
const RECALL: &str = "/api/v1/recall";
const WORKS: &str = "/api/v1/works";
const S1: &str = "tidy-session-1";
const G1: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e42";
const MODEL: &str = "fake/tidy@1";
const REASON: &str = "Записи говорят об одном и том же: уборка оставляет одну подсказку";

/// Тесты файла — по одному (см. модуль).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Сервер теста: тик ночи — 1 ч (ночь его тиком не запускается), модель векторов — тестовая,
/// кандидатов в пакете — до 50; исполнитель — агент, прочие виды — `off`.
async fn server(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    zam_server::works::night::test_hooks::set_tick(Duration::from_secs(3600));
    zam_server::works::tidy::test_hooks::set_model(Some(MODEL));
    let a = api(&format!("tidy-{name}"), |c| {
        c.works.batch_records = 50;
        tweak(c);
    })
    .await;
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/auth/settings",
            Some(json!({"works": {"mode": "agent",
                                  "kinds": {"probes": "off", "rules": "off", "echo": "off"}}})),
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    a
}

async fn db(a: &Api) -> zam_store::Db {
    a.st.users.get(a.st.admin_id).await.unwrap()
}

async fn space_id(a: &Api, kind: zam_store::SpaceKind) -> Id {
    zam_store::list_spaces(&db(a).await)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap()
        .id
}

fn entry_id(target: &str) -> Id {
    target.split_once(':').unwrap().1.parse().unwrap()
}

/// Insight токеном в Knowledge: заголовок `title`, прочие поля — `extra`; ссылка `i:<id>`.
async fn insight(a: &Api, title: &str, extra: Value) -> String {
    let mut body = json!({"kind": "insight", "title": title,
                          "body": format!("{title}: подробности и следствия."),
                          "data": {"kind": "gotcha"}, "force": true});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        body[k] = v;
    }
    let r = a
        .t(Method::POST, "/api/v1/spaces/knowledge/entries", Some(body))
        .await;
    ok(&r, StatusCode::CREATED);
    format!("i:{}", r.json()["id"].as_str().unwrap())
}

/// Термин токеном в Knowledge; ссылка `t:<id>`.
async fn term(a: &Api, title: &str) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(json!({"kind": "term", "title": title,
                        "data": {"definition": format!("{title} — понятие проекта")}})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    format!("t:{}", r.json()["id"].as_str().unwrap())
}

/// Запись по ссылке.
async fn entry(a: &Api, target: &str) -> zam_store::Entry {
    zam_store::get_entry(&db(a).await, entry_id(target))
        .await
        .unwrap()
}

/// Вектор записи: все ключи её чанков в кэше `embeddings` под тестовой моделью.
async fn set_vector(a: &Api, target: &str, v: &[f32]) {
    let id = entry_id(target);
    let db = db(a).await;
    let keys: Vec<String> = db
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT DISTINCT embed_key FROM chunks
                 WHERE entry_id = ?1 AND embed_key IS NOT NULL",
            )?;
            let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert!(!keys.is_empty(), "у записи {target} нет чанков");
    let items = keys
        .into_iter()
        .map(|k| {
            (
                k,
                zam_store::Embedding {
                    dense: v.to_vec(),
                    sparse: Vec::new(),
                },
            )
        })
        .collect();
    zam_store::put_embeddings(&db, MODEL, items).await.unwrap();
}

/// Вектор с косинусом `c` к `[1, 0, 0, 0]`.
fn at_cos(c: f32) -> Vec<f32> {
    vec![c, (1.0 - c * c).sqrt(), 0.0, 0.0]
}

/// Пара почти-дублей в области `area` с косинусом `c`: ссылки (старшая, младшая).
async fn pair(a: &Api, area: &str, c: f32, extra: Value) -> (String, String) {
    let mut e = json!({"area": area});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        e[k] = v;
    }
    let x = insight(a, &format!("Сборка {area}: первая"), e.clone()).await;
    let y = insight(a, &format!("Сборка {area}: вторая"), e).await;
    set_vector(a, &x, &[1.0, 0.0, 0.0, 0.0]).await;
    set_vector(a, &y, &at_cos(c)).await;
    (x, y)
}

/// Время «сейчас − `days` суток».
fn days_ago(days: i64) -> String {
    zam_store::time::fmt_ts(zam_store::time::now() - SignedDuration::from_hours(24 * days))
}

/// Часы — «сейчас − `days` суток» до конца guard (ревизии записей только дописываются: давность
/// `revisit` задаётся временем записи).
fn back(days: i64) -> zam_store::test_hooks::FreezeGuard {
    zam_store::time::freeze_now(zam_store::time::now() - SignedDuration::from_hours(24 * days))
}

/// Insight, записанный `days` суток назад.
async fn aged(a: &Api, title: &str, extra: Value, days: i64) -> String {
    let _f = back(days);
    insight(a, title, extra).await
}

/// Связи `contradicts` — `days` суток назад.
async fn age_contradicts(a: &Api, days: i64) {
    let ts = days_ago(days);
    db(a)
        .await
        .write(move |t| {
            t.execute(
                "UPDATE edges SET created_at = ?1 WHERE type = 'contradicts'",
                [ts],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Пара записей, противоречащих друг другу, старше 3 суток: (первая, вторая).
async fn contradiction(a: &Api, name: &str) -> (String, String) {
    let x = insight(a, &format!("{name}: так"), json!({"area": name})).await;
    let y = insight(
        a,
        &format!("{name}: иначе"),
        json!({"area": format!("{name}-2"),
               "links": [{"type": "contradicts", "target": format!("entry:{}", entry_id(&x))}]}),
    )
    .await;
    age_contradicts(a, 4).await;
    (x, y)
}

/// Проба на запись `target` и три законченных ночных прогона, где её цель не нашлась.
async fn probe_fails(a: &Api, targets: &[&str]) {
    let db = db(a).await;
    let space = space_id(a, zam_store::SpaceKind::Knowledge).await;
    let mut ids = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let rev = entry(a, t).await.rev;
        let out = zam_store::probes::insert_probe(
            &db,
            zam_store::probes::NewProbe {
                space: Some(space),
                question: format!("Как настроить сборку под номером {i} без лишних шагов?"),
                kind: zam_store::probes::ProbeKind::Manual,
                targets: vec![zam_store::probes::ProbeTarget {
                    target: (*t).to_string(),
                    rev,
                }],
                split: Split::Dev,
                overlap: None,
                author_kind: zam_store::probes::ProbeAuthor::Human,
                author: None,
                work_id: None,
            },
        )
        .await
        .unwrap();
        match out {
            zam_store::probes::InsertOutcome::Inserted(id) => ids.push(id),
            o => panic!("проба не вставлена: {o:?}"),
        }
    }
    for n in 1..=3i64 {
        let run = Id::new();
        let night = zam_store::time::utc_today()
            .checked_sub(jiff::Span::new().days(4 - n))
            .unwrap();
        let started = zam_store::time::now() - SignedDuration::from_hours(24 * (4 - n));
        zam_store::check::start_run(
            &db,
            run,
            zam_store::check::Trigger::Nightly,
            Some(night),
            started,
        )
        .await
        .unwrap();
        let results = ids
            .iter()
            .map(|p| zam_store::check::CheckResult {
                probe_id: *p,
                space: Some(space),
                search_rank: None,
                search_cut: Some(5),
                deep_rank: None,
                prompt_rank: None,
                search_ms: None,
                prompt_ms: None,
            })
            .collect();
        let f = zam_store::check::RunFinish {
            status: zam_store::check::RunStatus::Done,
            fingerprint: None,
            fp_hash: None,
            context: None,
            metrics: None,
            verdict: None,
            flags: Vec::new(),
            episode: None,
            error: None,
        };
        zam_store::check::finish_run(&db, run, f, results)
            .await
            .unwrap();
    }
}

/// Запись «шумная»: учёт открытий — с 31 суток назад, 10 показов без открытий и «полезно».
async fn noisy(a: &Api, target: &str) {
    let db = db(a).await;
    let since = days_ago(31);
    db.write(move |t| {
        t.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'usage_opened_since'",
            [since],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let events: Vec<zam_store::UsageEvent> = (0..10)
        .map(|i| zam_store::UsageEvent {
            user_id: a.st.admin_id,
            session_id: Some(format!("shown-{i}")),
            target: TargetRef {
                kind: TargetKind::Entry,
                id: entry_id(target),
            },
            event: zam_store::UsageKind::RecallShown,
            source: Some(zam_store::UsageSource::Prompt),
        })
        .collect();
    zam_store::record_usages(&db, &events).await.unwrap();
}

/// Ночь даты «сегодня + `days`» (режим агента окно не проверяет).
async fn night(a: &Api, days: i64) {
    let d = zam_store::time::utc_today()
        .checked_add(jiff::Span::new().days(days))
        .unwrap();
    let until = zam_store::time::now() + SignedDuration::from_hours(1);
    queue::night(&NightCtx::new(a.st.clone()), d, until)
        .await
        .unwrap();
}

/// Работы вида `tidy`, новые первыми.
async fn tidy_works(a: &Api) -> Vec<store::Work> {
    store::list(&db(a).await, None, Some(WorkKind::Tidy), 200)
        .await
        .unwrap()
}

/// Кандидаты последней работы `tidy`.
async fn last_items(a: &Api) -> Vec<WorkItem> {
    tidy_works(a)
        .await
        .first()
        .map(|w| w.items.clone())
        .unwrap_or_default()
}

/// Кандидат с записями `refs` (в любом порядке) среди `items`.
fn find<'a>(items: &'a [WorkItem], refs: &[&str]) -> Option<&'a WorkItem> {
    items.iter().find(|i| {
        let mut got: Vec<String> = i.all_refs().into_iter().map(|(r, _)| r).collect();
        let mut want: Vec<String> = refs.iter().map(|r| (*r).to_string()).collect();
        got.sort();
        want.sort();
        got == want
    })
}

/// Пакет работы `tidy` сессии [`S1`] (текст и id работы).
async fn package(a: &Api) -> (String, String) {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            RECALL,
            Some(json!({"event": "work", "kind": "tidy"})),
            &[("x-zam-session", S1)],
        )
        .await;
    ok(&r, StatusCode::OK);
    (
        r.json()["work"].as_str().unwrap().to_string(),
        r.json()["text"].as_str().unwrap_or("").to_string(),
    )
}

/// Итог работы `work` той же сессией.
async fn result(a: &Api, work: &str, items: Value) -> Value {
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            &format!("{WORKS}/{work}/result"),
            Some(json!({"items": items})),
            &[("x-zam-session", S1)],
        )
        .await;
    ok(&r, StatusCode::OK);
    r.json()
}

/// Пакет и итог `items`.
async fn do_work(a: &Api, items: Value) -> Value {
    let (work, _) = package(a).await;
    result(a, &work, items).await
}

/// Предложения уборки (человек), новые первыми.
async fn tidy_cards(a: &Api, status: &str) -> Vec<Value> {
    let r = a
        .adm(
            Method::GET,
            &format!("{PROPOSALS}?type=tidy&status={status}"),
            None,
            None,
        )
        .await;
    ok(&r, StatusCode::OK);
    r.json()["proposals"].as_array().unwrap().clone()
}

/// Предложение уборки с записью `target` (человек).
async fn card_of(a: &Api, target: &str) -> Value {
    tidy_cards(a, "proposed")
        .await
        .into_iter()
        .find(|c| {
            c["targets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["ref"] == target)
        })
        .unwrap_or_else(|| panic!("нет предложения с {target}"))
}

/// «Применить» человеком (WebUI).
async fn accept(a: &Api, o: &str, body: Option<Value>) -> Resp {
    a.adm(Method::POST, &format!("{PROPOSALS}/{o}/accept"), body, None)
        .await
}

/// Строки `proposals` типа `tidy` со статусом `status`.
async fn tidy_rows(a: &Api, status: &str) -> usize {
    let status = status.to_string();
    db(a)
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM proposals WHERE type = 'tidy' AND status = ?1",
                [status],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap() as usize
}

/// Записей журнала в базе и их ревизий.
async fn entries_count(a: &Api) -> (i64, i64) {
    db(a)
        .await
        .read(|c| {
            Ok(c.query_row(
                "SELECT (SELECT count(*) FROM entries), (SELECT count(*) FROM entry_revisions)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}

/// Причины ревизий записи (последняя — первой).
async fn revision_reasons(a: &Api, target: &str) -> Vec<String> {
    let id = entry_id(target);
    db(a)
        .await
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT coalesce(reason, '') FROM entry_revisions WHERE entry_id = ?1
                 ORDER BY rev DESC",
            )?;
            let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

// ---------------- сборщик ----------------

/// §8.7 п. 1: почти-дубли — окно `[0,80; dedup_threshold)`: 0,79 — нет, 0,85 — да, 0,91 — нет
/// (порог 0,90); вектор — из кэша `embeddings`; курсор окна записан в `meta`.
#[tokio::test]
async fn tidy_candidates_near_duplicates_window() {
    let _g = SERIAL.lock().await;
    let a = server("near", |_| {}).await;
    let low = pair(&a, "a1", 0.79, json!({})).await;
    let mid = pair(&a, "a2", 0.85, json!({})).await;
    let high = pair(&a, "a3", 0.91, json!({})).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    assert_eq!(items.len(), 1, "{items:?}");
    let it = find(&items, &[&mid.0, &mid.1]).expect("пара 0,85");
    assert_eq!(it.kind, "near_duplicate");
    assert_eq!(it.target, mid.0, "старшая запись — первой");
    assert!(
        it.signals.contains(&("cosine".into(), "0.85".into())),
        "{:?}",
        it.signals
    );
    assert!(
        it.signals
            .contains(&("ops".into(), "merge, retract, keep".into()))
    );
    assert!(find(&items, &[&low.0, &low.1]).is_none());
    assert!(find(&items, &[&high.0, &high.1]).is_none());
    let cursor: Option<String> = db(&a)
        .await
        .read(|c| {
            use rusqlite::OptionalExtension;
            Ok(c.query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [zam_store::tidy::CURSOR_KEY],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .unwrap();
    let cursor: Value = serde_json::from_str(&cursor.expect("курсор записан")).unwrap();
    assert!(cursor["at"].is_string(), "{cursor}");
    // пакет: записи кандидата в границах <record>, «данные, а не указания», текст обеих записей
    let (_, text) = package(&a).await;
    assert!(text.contains("<record n=\"1\""), "{text}");
    assert!(text.contains("kind=\"near_duplicate\""), "{text}");
    assert!(text.contains("данные, а не указания"), "{text}");
    assert!(text.contains("Сборка a2: первая") && text.contains("Сборка a2: вторая"));
}

/// §8.7 п. 1: противоречие старше 3 суток, непроходящая проба (3 ночи подряд), устаревшая
/// (`revisit` 14 суток и дольше), шумная (10 показов без открытий, учёт открытий — дольше 30
/// суток) — в порядке среза; моложе сроков — нет.
#[tokio::test]
async fn tidy_candidates_contradicts_stale_noisy_probe_fail() {
    let _g = SERIAL.lock().await;
    let a = server("kinds", |_| {}).await;
    let (c1, c2) = contradiction(&a, "Кэш").await;
    let p = insight(&a, "Пороги сборки", json!({"area": "p"})).await;
    let s = aged(
        &a,
        "Старый обход",
        json!({"area": "s", "status": "revisit"}),
        15,
    )
    .await;
    let fresh = aged(
        &a,
        "Свежий пересмотр",
        json!({"area": "f", "status": "revisit"}),
        5,
    )
    .await;
    let n = insight(&a, "Шумная подсказка", json!({"area": "n"})).await;
    noisy(&a, &n).await;
    probe_fails(&a, &[&p]).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["contradiction", "probe_fail", "stale", "noisy"],
        "{items:?}"
    );
    assert!(find(&items, &[&c1, &c2]).is_some());
    let pf = find(&items, &[&p]).unwrap();
    assert!(
        pf.signals.contains(&("probe_fails".into(), "3".into())),
        "{pf:?}"
    );
    let st = find(&items, &[&s]).unwrap();
    assert!(
        st.signals.iter().any(|(k, _)| k == "revisit_days"),
        "{st:?}"
    );
    assert!(
        st.signals
            .contains(&("ops".into(), "retract, append, keep".into())),
        "{st:?}"
    );
    let no = find(&items, &[&n]).unwrap();
    assert!(
        no.signals.contains(&("shown".into(), "10".into())),
        "{no:?}"
    );
    assert!(find(&items, &[&fresh]).is_none(), "5 суток revisit — рано");
}

/// §8.7 п. 1: до 20 кандидатов за ночь на пользователя; при срезе — противоречия раньше
/// устаревших.
#[tokio::test]
async fn tidy_night_limit_order() {
    let _g = SERIAL.lock().await;
    let a = server("limit", |_| {}).await;
    let mut stale = Vec::new();
    for i in 0..22 {
        let s = aged(
            &a,
            &format!("Устаревшее {i}"),
            json!({"area": format!("s{i}"), "status": "revisit"}),
            20,
        )
        .await;
        stale.push(s);
    }
    let (c1, c2) = contradiction(&a, "Позднее").await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    assert_eq!(items.len(), zam_store::tidy::NIGHT_MAX, "{}", items.len());
    assert_eq!(items[0].kind, "contradiction");
    assert!(find(&items, &[&c1, &c2]).is_some());
    assert!(items[1..].iter().all(|i| i.kind == "stale"));
    // старшие устаревшие — первыми
    assert_eq!(items[1].target, stale[0]);
    // та же ночь ещё раз — предел исчерпан, новых работ нет
    night(&a, 0).await;
    assert_eq!(tidy_works(&a).await.len(), 1);
}

/// §8.7 п. 1: кандидат с предложением по тому же ключу (тип, ссылки, ревизии) не повторяется,
/// `keep` — до новой ревизии; отклонённый человеком — по ссылкам, ни в какой ревизии.
#[tokio::test]
async fn tidy_dedup_and_declined_any_rev() {
    let _g = SERIAL.lock().await;
    let a = server("dedup", |_| {}).await;
    let p1 = pair(&a, "d1", 0.85, json!({})).await;
    let p2 = pair(&a, "d2", 0.85, json!({})).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    assert_eq!(items.len(), 2, "{items:?}");
    let n1 = find(&items, &[&p1.0, &p1.1]).unwrap().n;
    let n2 = find(&items, &[&p2.0, &p2.1]).unwrap().n;
    let r = do_work(
        &a,
        json!([
            {"n": n1, "op": "retract", "target": p1.1, "reason": REASON},
            {"n": n2, "op": "keep", "reason": "Это разные случаи, хоть и похожи по тексту"},
        ]),
    )
    .await;
    assert_eq!(r["accepted"], json!([n1, n2]), "{r}");
    assert_eq!(tidy_rows(&a, "proposed").await, 1);
    assert_eq!(tidy_rows(&a, "skipped").await, 1);
    // следующая ночь: оба кандидата уже решены (предложение, keep) — работы нет
    night(&a, 1).await;
    assert_eq!(tidy_works(&a).await.len(), 1);
    // человек отклоняет первое; обе пары получают новые ревизии
    let o = card_of(&a, &p1.1).await["id"].as_str().unwrap().to_string();
    ok(
        &a.adm(
            Method::POST,
            &format!("{PROPOSALS}/{o}/decline"),
            Some(json!({})),
            None,
        )
        .await,
        StatusCode::OK,
    );
    for t in [&p1.0, &p2.0] {
        let r = a
            .tok_rev(
                &a.token,
                Method::PATCH,
                &format!("/api/v1/entries/{}", entry_id(t)),
                Some(json!({"append_body": "уточнение"})),
            )
            .await;
        ok(&r, StatusCode::OK);
        set_vector(&a, t, &[1.0, 0.0, 0.0, 0.0]).await;
    }
    night(&a, 2).await;
    let items = last_items(&a).await;
    assert!(
        find(&items, &[&p2.0, &p2.1]).is_some(),
        "keep — до новой ревизии: {items:?}"
    );
    assert!(
        find(&items, &[&p1.0, &p1.1]).is_none(),
        "отклонённый человеком — ни в какой ревизии: {items:?}"
    );
}

/// §8.7 п. 1: `insights.dedup_threshold ≤ 0,80` — окно почти-дублей пусто: пропуск (WARN).
#[tokio::test]
async fn tidy_threshold_below_window_skipped() {
    let _g = SERIAL.lock().await;
    let a = server("threshold", |c| c.insights.dedup_threshold = 0.80).await;
    pair(&a, "t1", 0.85, json!({})).await;
    night(&a, 0).await;
    assert!(tidy_works(&a).await.is_empty());
}

/// §8.7 п. 1: записи Human сборщику недоступны (пространства вида — решения и Knowledge).
#[tokio::test]
async fn tidy_human_space_never_collected() {
    let _g = SERIAL.lock().await;
    let a = server("human", |_| {}).await;
    let human = space_id(&a, zam_store::SpaceKind::Human).await;
    let mut at = zam_store::Attribution::new(zam_store::AuthorKind::Human);
    at.user_id = Some(a.st.admin_id);
    let f = back(30);
    let (h, _) = zam_store::create_entry(
        &db(&a).await,
        zam_store::NewEntry {
            space_id: human,
            kind: zam_store::EntryKind::Insight,
            title: "Личная заметка о пересмотре".into(),
            body: "Её не трогает уборка.".into(),
            data: json!({"kind": "gotcha"}),
            area: None,
            tags: Vec::new(),
            status: Some(zam_store::EntryStatus::Revisit),
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
            refs: None,
        },
        &at,
    )
    .await
    .unwrap();
    drop(f);
    let h = format!("i:{}", h.id);
    let k = aged(
        &a,
        "Пересмотр в Knowledge",
        json!({"status": "revisit"}),
        30,
    )
    .await;
    night(&a, 0).await;
    let ws = tidy_works(&a).await;
    assert_eq!(ws.len(), 1, "{ws:?}");
    assert_ne!(ws[0].space_id, human);
    let refs: Vec<String> = ws[0].items.iter().map(|i| i.target.clone()).collect();
    assert_eq!(refs, [k]);
    assert!(!refs.contains(&h), "запись Human — не кандидат");
}

/// §8.7 п. 4: отсев — один раз на запуск вида: при двух пространствах вида `offered_refs`
/// считается один раз за ночь.
#[tokio::test]
async fn sift_computed_once_per_kind_run() {
    let _g = SERIAL.lock().await;
    let a = server("sift", |_| {}).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": G1, "name": "Решение уборки"})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    pair(&a, "k1", 0.85, json!({})).await;
    let spaces = queue::kind_spaces(&db(&a).await, WorkKind::Tidy, None)
        .await
        .unwrap();
    assert_eq!(spaces.len(), 2, "решение и Knowledge");
    let sifts = || zam_server::works::kinds::test_hooks::sifts(a.st.admin_id, WorkKind::Tidy);
    assert_eq!(sifts(), 0);
    night(&a, 0).await;
    assert_eq!(sifts(), 1, "один раз на запуск, а не на пространство");
    night(&a, 1).await;
    assert_eq!(sifts(), 2);
    assert_eq!(
        zam_server::works::kinds::test_hooks::sifts(a.st.admin_id, WorkKind::Probes),
        0,
        "вид off — запуска нет"
    );
}

// ---------------- вид работы: проверка ----------------

/// §8.7 п. 2: проверка операции — операция из допустимых кандидата, ссылки только из
/// кандидата, `reason` 20..=2000, `revisit` — только у действующего, триггеры и термин — по их
/// правилам, `merge` — разные записи; верные — приняты.
#[tokio::test]
async fn tidy_validator_cases() {
    let _g = SERIAL.lock().await;
    let a = server("validator", |_| {}).await;
    let p1 = pair(&a, "v1", 0.85, json!({})).await;
    let p2 = pair(&a, "v2", 0.85, json!({})).await;
    let s = aged(
        &a,
        "Пересмотр давно",
        json!({"area": "vs", "status": "revisit"}),
        20,
    )
    .await;
    let t = term(&a, "Сборщик").await;
    let pi = insight(&a, "Проба не проходит", json!({"area": "vp"})).await;
    probe_fails(&a, &[&t, &pi]).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    let n = |refs: &[&str]| find(&items, refs).unwrap_or_else(|| panic!("{refs:?}")).n;
    let (n1, n2, ns, nt, np) = (
        n(&[&p1.0, &p1.1]),
        n(&[&p2.0, &p2.1]),
        n(&[&s]),
        n(&[&t]),
        n(&[&pi]),
    );
    let (work, _) = package(&a).await;
    let r = result(
        &a,
        &work,
        json!([
            // почти-дублю `revisit` недопустим
            {"n": n1, "op": "revisit", "target": p1.0, "reason": REASON},
            // ссылка не из кандидата
            {"n": n2, "op": "retract", "target": p1.1, "reason": REASON},
            // `revisit` у записи уже в `revisit` — не из допустимых
            {"n": ns, "op": "revisit", "target": s, "reason": REASON},
            // псевдоним без букв
            {"n": nt, "op": "term", "target": t, "aliases": ["--"], "reason": REASON},
            // триггер с неизвестным типом
            {"n": np, "op": "triggers", "target": pi,
             "triggers": [{"type": "nope", "pattern": "x"}], "reason": REASON},
        ]),
    )
    .await;
    let reasons: Vec<(u64, String)> = r["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["n"].as_u64().unwrap(),
                x["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let why = |n: usize| {
        reasons
            .iter()
            .find(|(k, _)| *k == n as u64)
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| panic!("{n} не отклонён: {r}"))
    };
    assert!(why(n1).contains("недопустима"), "{}", why(n1));
    assert!(
        why(n2).contains("только из записей кандидата"),
        "{}",
        why(n2)
    );
    assert!(why(ns).contains("недопустима"), "{}", why(ns));
    assert!(why(nt).contains("aliases"), "{}", why(nt));
    assert!(why(np).contains("triggers[0]"), "{}", why(np));
    assert_eq!(tidy_rows(&a, "proposed").await, 0);
    // следующая ночь (работа закрыта) — те же кандидаты, проверка короткого довода и merge
    night(&a, 1).await;
    let items = last_items(&a).await;
    let n1 = find(&items, &[&p1.0, &p1.1]).unwrap().n;
    let n2 = find(&items, &[&p2.0, &p2.1]).unwrap().n;
    let nt = find(&items, &[&t]).unwrap().n;
    let np = find(&items, &[&pi]).unwrap().n;
    let ns = find(&items, &[&s]).unwrap().n;
    let (work, _) = package(&a).await;
    let r = result(
        &a,
        &work,
        json!([
            {"n": n1, "op": "merge", "keep": p1.0, "drop": p1.0, "reason": REASON},
            {"n": n2, "op": "retract", "target": p2.1, "reason": "коротко"},
            {"n": nt, "op": "term", "target": t, "aliases": ["Сборщик ночи"], "reason": REASON},
            {"n": np, "op": "triggers", "target": pi,
             "triggers": [{"type": "keyword", "pattern": "пороги"}], "reason": REASON},
            {"n": ns, "op": "append", "target": s, "text": "Проверено снова: всё ещё верно.",
             "reason": REASON},
        ]),
    )
    .await;
    let rejected: Vec<u64> = r["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["n"].as_u64().unwrap())
        .collect();
    assert_eq!(rejected, [n1 as u64, n2 as u64], "{r}");
    assert_eq!(r["accepted"], json!([nt, np, ns]), "{r}");
    assert_eq!(tidy_rows(&a, "proposed").await, 3);
}

/// §6.3 п. 4, §8.7 п. 2: текст операции с маской секрета — отказ; «Применить» с маской в
/// правке человека — 422.
#[tokio::test]
async fn tidy_merge_text_with_mask_rejected() {
    let _g = SERIAL.lock().await;
    let a = server("mask", |_| {}).await;
    let (x, y) = pair(&a, "m1", 0.85, json!({})).await;
    let (x2, y2) = pair(&a, "m2", 0.85, json!({})).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    let n1 = find(&items, &[&x, &y]).unwrap().n;
    let n2 = find(&items, &[&x2, &y2]).unwrap().n;
    let r = do_work(
        &a,
        json!([
            {"n": n1, "op": "merge", "keep": x, "drop": y,
             "text": "Ключ сборки ‹secret› в окружении.", "reason": REASON},
            {"n": n2, "op": "merge", "keep": x2, "drop": y2,
             "text": "Объединённый текст без секретов.", "reason": REASON},
        ]),
    )
    .await;
    assert_eq!(r["rejected"][0]["n"], n1, "{r}");
    assert_eq!(r["rejected"][0]["reason"], MASK_LITERAL_REASON);
    let o = card_of(&a, &x2).await["id"].as_str().unwrap().to_string();
    let r = accept(
        &a,
        &o,
        Some(json!({"text": "Правка с ‹secret-ab12-1› внутри."})),
    )
    .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(entry(&a, &x2).await.rev, 1, "ничего не применено");
}

/// Review Focus 2, §8.7 п. 2: удаления нет — `delete` не операция, в `ops` пакета его нет.
#[tokio::test]
async fn tidy_no_delete_op() {
    let _g = SERIAL.lock().await;
    let a = server("nodelete", |_| {}).await;
    let (x, y) = pair(&a, "nd", 0.85, json!({})).await;
    night(&a, 0).await;
    let it = last_items(&a).await.remove(0);
    let ops = it
        .signals
        .iter()
        .find(|(k, _)| k == "ops")
        .unwrap()
        .1
        .clone();
    assert!(!ops.contains("delete"), "{ops}");
    let r = do_work(
        &a,
        json!([{"n": it.n, "op": "delete", "target": y, "reason": REASON}]),
    )
    .await;
    assert!(
        r["rejected"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("удаления нет"),
        "{r}"
    );
    assert_eq!(tidy_rows(&a, "proposed").await, 0);
    for t in [&x, &y] {
        let e = entry(&a, t).await;
        assert!(e.deleted_at.is_none());
        assert_eq!(e.status, Some(zam_store::EntryStatus::Active));
    }
}

// ---------------- применение человеком ----------------

/// Review Focus 2, §8.7 п. 3: итог работы ничего не меняет в знаниях — записи с прежними
/// ревизиями и статусами, новых записей нет, предложение ждёт человека.
#[tokio::test]
async fn tidy_nothing_applied_without_human() {
    let _g = SERIAL.lock().await;
    let a = server("nohuman", |_| {}).await;
    let (x, y) = pair(&a, "nh", 0.85, json!({})).await;
    night(&a, 0).await;
    let n = last_items(&a).await[0].n;
    let before = entries_count(&a).await;
    let r = do_work(
        &a,
        json!([{"n": n, "op": "merge", "keep": x, "drop": y,
                "text": "Объединённый текст.", "reason": REASON}]),
    )
    .await;
    assert_eq!(r["accepted"], json!([n]), "{r}");
    for t in [&x, &y] {
        let e = entry(&a, t).await;
        assert_eq!(e.rev, 1, "{t}");
        assert_eq!(e.status, Some(zam_store::EntryStatus::Active));
    }
    assert_eq!(before, entries_count(&a).await);
    let cards = tidy_cards(&a, "proposed").await;
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["op"]["op"], "merge");
    assert_eq!(cards[0]["candidate"], "near_duplicate");
    assert_eq!(cards[0]["stale"], false);
    assert_eq!(cards[0]["records"][0]["ref"], x.as_str());
    assert!(
        cards[0]["records"][0]["l1"]
            .as_str()
            .unwrap()
            .contains("Сборка nh")
    );
    let c = a.t(Method::GET, &format!("{PROPOSALS}/counts"), None).await;
    assert_eq!(c.json()["tidy"], 1, "{}", c.text());
}

/// §8.7 п. 3: «Применить» `merge` с текстом — `keep` замещается новой записью с этим телом,
/// `drop` отзывается «дубль i:<новая>»; достоверность — меньшая из двух, не выше `theorem`
/// без правки человеком (с правкой — как есть); причина ревизии — «из предложения o:<id>
/// (сессия …)»; аудит `proposal.accept`.
#[tokio::test]
async fn tidy_apply_merge_with_text() {
    let _g = SERIAL.lock().await;
    let a = server("merge-text", |_| {}).await;
    let ax = json!({"certainty": "axiom"});
    let (x, y) = pair(&a, "mt1", 0.85, ax.clone()).await;
    let (x2, y2) = pair(&a, "mt2", 0.85, ax).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    let n1 = find(&items, &[&x, &y]).unwrap().n;
    let n2 = find(&items, &[&x2, &y2]).unwrap().n;
    let text = "Объединённый текст двух записей о сборке.";
    do_work(
        &a,
        json!([
            {"n": n1, "op": "merge", "keep": x, "drop": y, "text": text, "reason": REASON},
            {"n": n2, "op": "merge", "keep": x2, "drop": y2, "text": text, "reason": REASON},
        ]),
    )
    .await;
    let card = card_of(&a, &x).await;
    assert_eq!(card["records"][0]["human_decision"], true, "{card}");
    let o = card["id"].as_str().unwrap().to_string();
    let r = accept(&a, &o, None).await;
    ok(&r, StatusCode::OK);
    let new = r.json()["result"].as_str().unwrap().to_string();
    assert!(new.starts_with("i:"), "{}", r.text());
    let n = entry(&a, &new).await;
    assert_eq!(n.body, text);
    assert_eq!(
        n.certainty,
        Some(zam_store::Certainty::Theorem),
        "без правки — не выше theorem"
    );
    assert_eq!(n.supersedes_id, Some(entry_id(&x)));
    assert_eq!(
        entry(&a, &x).await.status,
        Some(zam_store::EntryStatus::Superseded)
    );
    let d = entry(&a, &y).await;
    assert_eq!(d.status, Some(zam_store::EntryStatus::Retracted));
    let why = d.retract_reason.unwrap_or_default();
    assert!(why.starts_with(&format!("дубль {new}")), "{why}");
    assert!(
        why.contains(&format!("из предложения {o} (сессия {S1})")),
        "{why}"
    );
    assert_eq!(r.json()["proposal"]["status"], "accepted");
    assert_eq!(r.json()["proposal"]["result_ref"], new.as_str());
    let r2 = a.adm(Method::GET, "/api/v1/admin/audit", None, None).await;
    let row = r2.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["action"] == "proposal.accept")
        .cloned()
        .expect("аудит proposal.accept");
    assert_eq!(row["details"]["type"], "tidy");
    assert_eq!(row["details"]["result"], new.as_str());
    // правка человеком — достоверность как есть (axiom)
    let o2 = card_of(&a, &x2).await["id"].as_str().unwrap().to_string();
    let r = accept(
        &a,
        &o2,
        Some(json!({"text": "Текст, поправленный человеком."})),
    )
    .await;
    ok(&r, StatusCode::OK);
    let n2 = entry(&a, r.json()["result"].as_str().unwrap()).await;
    assert_eq!(n2.body, "Текст, поправленный человеком.");
    assert_eq!(n2.certainty, Some(zam_store::Certainty::Axiom));
}

/// §8.7 п. 3: `merge` без текста — `drop` отзывается «дубль i:<keep>», `keep` не меняется.
#[tokio::test]
async fn tidy_apply_merge_without_text() {
    let _g = SERIAL.lock().await;
    let a = server("merge-plain", |_| {}).await;
    let (x, y) = pair(&a, "mp", 0.85, json!({})).await;
    night(&a, 0).await;
    let n = last_items(&a).await[0].n;
    do_work(
        &a,
        json!([{"n": n, "op": "merge", "keep": x, "drop": y, "reason": REASON}]),
    )
    .await;
    let o = card_of(&a, &x).await["id"].as_str().unwrap().to_string();
    let r = accept(&a, &o, None).await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["result"], y.as_str());
    let k = entry(&a, &x).await;
    assert_eq!((k.rev, k.status), (1, Some(zam_store::EntryStatus::Active)));
    let d = entry(&a, &y).await;
    assert_eq!(d.status, Some(zam_store::EntryStatus::Retracted));
    assert!(
        d.retract_reason
            .unwrap_or_default()
            .starts_with(&format!("дубль {x}"))
    );
}

/// §8.7 п. 3: `retract`, `revisit`, `triggers`, `term` (новый и псевдонимы), `append` —
/// применяются человеком от его имени, причина ревизии — довод и «из предложения o:<id>».
#[tokio::test]
async fn tidy_apply_retract_revisit_triggers_term_append() {
    let _g = SERIAL.lock().await;
    let a = server("ops", |_| {}).await;
    let (c1, c2) = contradiction(&a, "Порядок").await;
    let (d1, d2) = contradiction(&a, "Режим").await;
    let s = aged(
        &a,
        "Отжившее",
        json!({"area": "rs", "status": "revisit"}),
        20,
    )
    .await;
    let p = insight(&a, "Пороги", json!({"area": "rp"})).await;
    let q = insight(&a, "Без термина", json!({"area": "rq"})).await;
    let t = term(&a, "Окно ночи").await;
    probe_fails(&a, &[&p, &q, &t]).await;
    night(&a, 0).await;
    let items = last_items(&a).await;
    let n = |refs: &[&str]| find(&items, refs).unwrap_or_else(|| panic!("{refs:?}")).n;
    let r = do_work(
        &a,
        json!([
            {"n": n(&[&c1, &c2]), "op": "revisit", "target": c1, "reason": REASON},
            {"n": n(&[&d1, &d2]), "op": "append", "target": d2,
             "text": "Уточнение: верно для ночного режима.", "reason": REASON},
            {"n": n(&[&s]), "op": "retract", "target": s, "reason": REASON},
            {"n": n(&[&p]), "op": "triggers", "target": p,
             "triggers": [{"type": "keyword", "pattern": "пороги сборки"}], "reason": REASON},
            {"n": n(&[&q]), "op": "term", "title": "Ночное окно",
             "definition": "Время, когда сервер ставит внутренние работы.",
             "aliases": ["окно работ"], "reason": REASON},
            {"n": n(&[&t]), "op": "term", "target": t, "aliases": ["ночное окно работ"],
             "reason": REASON},
        ]),
    )
    .await;
    assert_eq!(r["rejected"], json!([]), "{r}");
    for target in [&c1, &d2, &s, &p, &q, &t] {
        let o = card_of(&a, target).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let r = accept(&a, &o, None).await;
        ok(&r, StatusCode::OK);
    }
    let e = entry(&a, &c1).await;
    assert_eq!(e.status, Some(zam_store::EntryStatus::Revisit));
    assert!(revision_reasons(&a, &c1).await[0].contains("из предложения o:"));
    assert!(
        entry(&a, &d2)
            .await
            .body
            .ends_with("Уточнение: верно для ночного режима.")
    );
    let e = entry(&a, &s).await;
    assert_eq!(e.status, Some(zam_store::EntryStatus::Retracted));
    assert!(e.retract_reason.unwrap().starts_with(REASON));
    let tr = zam_store::entry_triggers(&db(&a).await, entry_id(&p))
        .await
        .unwrap();
    assert_eq!(tr.len(), 1);
    assert_eq!(tr[0].pattern, "пороги сборки");
    let terms = zam_store::all_active_terms(&db(&a).await).await.unwrap();
    assert!(terms.iter().any(|g| g.title == "Ночное окно"), "{terms:?}");
    let w = terms.iter().find(|g| g.title == "Окно ночи").unwrap();
    assert_eq!(w.aliases, ["ночное окно работ"]);
    assert_eq!(tidy_rows(&a, "accepted").await, 6);
}

/// §8.7 п. 3: запись изменилась после предложения — карточка `stale`, «Применить» — 409
/// `code: stale`, предложение `stale`, ничего не применено.
#[tokio::test]
async fn tidy_apply_stale_rev_409() {
    let _g = SERIAL.lock().await;
    let a = server("stale", |_| {}).await;
    let (x, y) = pair(&a, "st", 0.85, json!({})).await;
    night(&a, 0).await;
    let n = last_items(&a).await[0].n;
    do_work(
        &a,
        json!([{"n": n, "op": "retract", "target": y, "reason": REASON}]),
    )
    .await;
    let o = card_of(&a, &y).await["id"].as_str().unwrap().to_string();
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{}", entry_id(&x)),
            Some(json!({"append_body": "правка после предложения"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(card_of(&a, &y).await["stale"], true);
    let r = accept(&a, &o, None).await;
    ok(&r, StatusCode::CONFLICT);
    assert_eq!(r.json()["code"], "stale", "{}", r.text());
    assert_eq!(tidy_rows(&a, "stale").await, 1);
    assert_eq!(
        entry(&a, &y).await.status,
        Some(zam_store::EntryStatus::Active)
    );
    // второе «Применить» — уже не открыто
    ok(&accept(&a, &o, None).await, StatusCode::CONFLICT);
}

/// §8.7 п. 3: «Применить» и «Отклонить» — только сессия WebUI; токену — 403, ничего не меняется.
#[tokio::test]
async fn tidy_accept_token_forbidden() {
    let _g = SERIAL.lock().await;
    let a = server("token", |_| {}).await;
    let (x, y) = pair(&a, "tk", 0.85, json!({})).await;
    night(&a, 0).await;
    let n = last_items(&a).await[0].n;
    do_work(
        &a,
        json!([{"n": n, "op": "merge", "keep": x, "drop": y, "reason": REASON}]),
    )
    .await;
    let o = card_of(&a, &x).await["id"].as_str().unwrap().to_string();
    for path in ["accept", "decline"] {
        let r = a
            .t(
                Method::POST,
                &format!("{PROPOSALS}/{o}/{path}"),
                Some(json!({})),
            )
            .await;
        ok(&r, StatusCode::FORBIDDEN);
    }
    assert_eq!(tidy_rows(&a, "proposed").await, 1);
    assert_eq!(
        entry(&a, &y).await.status,
        Some(zam_store::EntryStatus::Active)
    );
    // предложение уборки создаёт только работа: POST /proposals type=tidy — 400
    let r = a
        .t(
            Method::POST,
            PROPOSALS,
            Some(json!({"type": "tidy", "path": "x", "content": "y", "reason": REASON})),
        )
        .await;
    ok(&r, StatusCode::BAD_REQUEST);
}

/// §8.7 п. 2: протокол `zam guide` (ru и en синхронно) — строка о виде `tidy` в разделе
/// «Внутренние работы»: операции, довод, удаления нет, применяет человек.
#[tokio::test]
async fn guide_tidy_line() {
    let _g = SERIAL.lock().await;
    let a = server("guide", |_| {}).await;
    for (q, head, wants) in [
        (
            "",
            "## Внутренние работы\n",
            ["**Уборка (`tidy`)**", "удаления нет", "только по `n`"],
        ),
        (
            "?lang=en",
            "## Internal works\n",
            ["**Tidying (`tidy`)**", "no delete", "only by `n`"],
        ),
    ] {
        let r = a
            .tok(
                &a.token,
                Method::GET,
                &format!("/api/v1/agent-guide{q}"),
                None,
            )
            .await;
        ok(&r, StatusCode::OK);
        let g = r.text();
        let sec = g
            .split(head)
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap()
            .to_string();
        for w in wants {
            assert!(sec.contains(w), "{w}: {sec}");
        }
    }
}
