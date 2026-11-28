//! Пробы ночной проверки памяти (M7.3 §6.4 пп. 1, 3–5, задача 5): часть по цели, пробы из кэша
//! выжимок, посторонние вопросы, снятие при уходе цели, пробы вне поиска, подсказок и экспорта,
//! пробы в копии сервера. Вид работы `probes` (исполнитель пишет пробы) — задача 4.
mod common;

use std::io::Read;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::check;
use zam_server::works::NightCtx;
use zam_store::probes::{
    InsertOutcome, NewProbe, PROBES_MAX, ProbeAuthor, ProbeKind, ProbeTarget, Split,
};
use zam_store::{SpaceKind, time::freeze_now};

use common::api::{Api, api, id, ok};
use common::check::{Reset, SERIAL, space_of, ts};

/// Пространство вида `kind` базы администратора.
async fn space_id(a: &Api, kind: SpaceKind) -> Id {
    space_of(a, a.st.admin_id, kind).await
}

/// Проба вида `probe` о цели `target` в пространстве `space` (часть — по цели).
async fn probe(a: &Api, space: Id, q: &str, target: &str) -> InsertOutcome {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    zam_store::probes::insert_probe(
        &db,
        NewProbe {
            space: Some(space),
            question: q.into(),
            kind: ProbeKind::Probe,
            targets: vec![ProbeTarget {
                target: target.into(),
                rev: 1,
            }],
            split: zam_search::eval::probe_split(target),
            overlap: Some(0.0),
            author_kind: ProbeAuthor::Agent,
            author: None,
            work_id: None,
        },
    )
    .await
    .unwrap()
}

async fn probes(a: &Api, query: &str) -> Vec<Value> {
    let r = a
        .t(Method::GET, &format!("/api/v1/check/probes{query}"), None)
        .await;
    ok(&r, StatusCode::OK);
    r.json()["items"].as_array().cloned().unwrap_or_default()
}

/// Ночной прогон администратора за ночь `day` с часами в окне (04:30 UTC + 1 мин).
async fn night(a: &Api, day: &str) -> Option<Id> {
    let _clock = freeze_now(ts(&format!("{day}T04:31:00Z")));
    let ctx = NightCtx::new(a.st.clone());
    check::run_night(&ctx, a.st.admin_id, day.parse().unwrap())
        .await
        .unwrap()
}

fn fast() {
    check::test_hooks::set_pause(Duration::ZERO);
}

/// §6.4 п. 2: часть пробы — по ссылке цели: та же цель — та же часть в ответе `GET
/// /check/probes` и у второй пробы о той же цели; фильтр `split=` отдаёт только свою часть.
#[tokio::test]
async fn probes_split_by_target_stable() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-split", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let mut want = Vec::new();
    for i in 0..12 {
        let t = format!("n:{}", Id::new());
        assert!(matches!(
            probe(&a, kn, &format!("вопрос номер {i} о записи"), &t).await,
            InsertOutcome::Inserted(_)
        ));
        // та же цель — та же часть и у второй пробы
        probe(&a, kn, &format!("второй вопрос {i} о той же записи"), &t).await;
        want.push((t.clone(), zam_search::eval::probe_split(&t)));
    }
    let items = probes(&a, "?kind=probe").await;
    assert_eq!(items.len(), 24);
    for (t, s) in &want {
        let of: Vec<&Value> = items
            .iter()
            .filter(|p| p["targets"][0]["ref"] == t.as_str())
            .collect();
        assert_eq!(of.len(), 2);
        assert!(of.iter().all(|p| p["split"] == s.as_str()), "{t}");
    }
    let test = probes(&a, "?split=test").await;
    assert!(test.iter().all(|p| p["split"] == "test"));
    let n_test = want.iter().filter(|(_, s)| *s == Split::Test).count();
    assert_eq!(test.len(), 2 * n_test);
    // повтор вопроса в пространстве — отказ; предел активных — 500
    assert_eq!(
        probe(
            &a,
            kn,
            "Вопрос номер 0  о записи",
            &format!("n:{}", Id::new())
        )
        .await,
        InsertOutcome::Duplicate
    );
    assert_eq!(PROBES_MAX, 500);
}

/// §6.4 п. 3: перед ночным прогоном — пробы из действующих выжимок кэша (вид `cache`, автор
/// `system`, цели — источники); недействительная (источник правлен) — нет; повтор ночью — нет.
#[tokio::test]
async fn probes_from_cache_valid_only() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    fast();
    let a = api("probes-cache", |_| {}).await;
    let n1 = id(&a
        .note(
            "knowledge",
            "kb/деплой.md",
            "# Деплой\n\nkappazulu готов.\n",
        )
        .await);
    let n2 = id(&a
        .note("knowledge", "kb/сборка.md", "# Сборка\n\nlimaxray готов.\n")
        .await);
    for (q, n) in [("как идёт kappazulu", &n1), ("как идёт limaxray", &n2)] {
        let r = a
            .t(
                Method::POST,
                "/api/v1/cache",
                Some(
                    json!({"query": q, "summary": "кратко", "sources": [{"id": format!("n:{n}")}]}),
                ),
            )
            .await;
        ok(&r, StatusCode::CREATED);
    }
    // источник второй выжимки правлен — она недействительна
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{n2}"),
            Some(json!({"content": "# Сборка\n\nlimaxray переписан.\n"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::OK);
    assert!(night(&a, "2026-11-10").await.is_some());
    let cache = probes(&a, "?kind=cache").await;
    assert_eq!(cache.len(), 1, "{cache:?}");
    assert_eq!(cache[0]["question"], "как идёт kappazulu");
    assert_eq!(cache[0]["author_kind"], "system");
    assert_eq!(cache[0]["targets"][0]["ref"], format!("n:{n1}"));
    // следующая ночь: проба уже есть — повтора нет
    assert!(night(&a, "2026-11-11").await.is_some());
    assert_eq!(probes(&a, "?kind=cache").await.len(), 1);
}

/// §6.4 п. 3 (ревью r1, Minor 1): кандидаты из кэша — действующие выжимки **без пробы**: у новой
/// выжимки проба уже есть — кандидатом становится более старая, а не никто.
#[tokio::test]
async fn probes_from_cache_skips_probed() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-cache-skip", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let n = id(&a
        .note("knowledge", "kb/c.md", "# C\n\nоскарпапа и дельтаэхо.\n")
        .await);
    for q in ["первый вопрос оскарпапа", "второй вопрос дельтаэхо"]
    {
        let r = a
            .t(
                Method::POST,
                "/api/v1/cache",
                Some(
                    json!({"query": q, "summary": "кратко", "sources": [{"id": format!("n:{n}")}]}),
                ),
            )
            .await;
        ok(&r, StatusCode::CREATED);
    }
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let all = zam_store::probes::cache_candidates(&db, 10).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].query, "второй вопрос дельтаэхо", "новые первыми");
    // у новой выжимки проба есть (тот же вопрос в регистре и пробелах иначе)
    probe(&a, kn, "Второй  вопрос дельтаэхо", &format!("n:{n}")).await;
    let one = zam_store::probes::cache_candidates(&db, 1).await.unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].query, "первый вопрос оскарпапа");
}

/// §6.4 п. 4: набор посторонних вопросов сервера — 30 активных проб вида `negative` без
/// пространства и целей после прогона; второй прогон их не удваивает.
#[tokio::test]
async fn probes_negative_set_loaded() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    fast();
    let a = api("probes-negative", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let n = id(&a.note("knowledge", "kb/a.md", "# A\n\nmikeoscar.\n").await);
    probe(&a, kn, "где mikeoscar", &format!("n:{n}")).await;
    assert!(night(&a, "2026-11-10").await.is_some());
    assert!(night(&a, "2026-11-11").await.is_some());
    let neg = probes(&a, "?kind=negative&status=active").await;
    assert_eq!(neg.len(), check::negatives::NEGATIVES_COUNT);
    assert!(
        neg.iter()
            .all(|p| p["space"].is_null() && p["targets"] == json!([]))
    );
    assert!(neg.iter().all(|p| p["author_kind"] == "system"));
    // посторонние вопросы в предел активных проб с целью не входят
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let c = zam_store::probes::probe_counts(&db).await.unwrap();
    assert_eq!(c.active, 31);
}

/// §6.4 п. 1: цель удалена, отозвана или замещена — проба `retired` (`target_gone`) при
/// следующем прогоне; изменённая цель пробу не снимает.
#[tokio::test]
async fn probes_retire_on_target_gone() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    fast();
    let a = api("probes-retire", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let gone = id(&a
        .note("knowledge", "kb/gone.md", "# Gone\n\nnovemberecho.\n")
        .await);
    let kept = id(&a
        .note("knowledge", "kb/kept.md", "# Kept\n\nquebecromeo.\n")
        .await);
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "Решение sierratango", "body": "x",
                        "data": {"kind": "decision"}}),
            ),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let ins = id(&r.json());
    probe(&a, kn, "где novemberecho", &format!("n:{gone}")).await;
    probe(&a, kn, "где quebecromeo", &format!("n:{kept}")).await;
    probe(&a, kn, "что решили про sierratango", &format!("i:{ins}")).await;
    ok(
        &a.t(Method::DELETE, &format!("/api/v1/nodes/{gone}"), None)
            .await,
        StatusCode::NO_CONTENT,
    );
    ok(
        &a.t(
            Method::POST,
            &format!("/api/v1/entries/{ins}/retract"),
            Some(json!({"reason": "опровергнуто"})),
        )
        .await,
        StatusCode::OK,
    );
    ok(
        &a.tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{kept}"),
            Some(json!({"content": "# Kept\n\nquebecromeo, правлено.\n"})),
            &[("if-match", "1")],
        )
        .await,
        StatusCode::OK,
    );
    assert!(night(&a, "2026-11-10").await.is_some());
    let retired = probes(&a, "?kind=probe&status=retired").await;
    assert_eq!(retired.len(), 2, "{retired:?}");
    assert!(retired.iter().all(|p| p["retired_reason"] == "target_gone"));
    let active = probes(&a, "?kind=probe&status=active").await;
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["targets"][0]["ref"], format!("n:{kept}"));
}

/// §6 «Общее», §6.4 п. 1: пробы — вне поиска, подсказок (дайджест и к запросу) и экспорта
/// пространства по токену: ни одного вхождения вопроса пробы.
#[tokio::test]
async fn probes_not_in_search_export_digest() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-invisible", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let rules = space_id(&a, SpaceKind::Rules).await;
    let n = id(&a
        .note("knowledge", "kb/note.md", "# Заметка\n\nобычный текст.\n")
        .await);
    let marker = "whiskeyprobemarker";
    probe(
        &a,
        kn,
        &format!("где {marker} в знаниях"),
        &format!("n:{n}"),
    )
    .await;
    probe(
        &a,
        rules,
        &format!("правило {marker} в Rules"),
        &format!("n:{n}"),
    )
    .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": marker, "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert_eq!(r.json()["items"], json!([]), "{}", r.text());
    for body in [
        json!({"event": "session_start"}),
        json!({"event": "prompt", "prompt": format!("где {marker} в знаниях")}),
    ] {
        let r = a.t(Method::POST, "/api/v1/recall", Some(body)).await;
        ok(&r, StatusCode::OK);
        assert!(!r.text().contains(marker), "{}", r.text());
    }
    for space in ["knowledge", "rules"] {
        let r = a
            .t(Method::GET, &format!("/api/v1/spaces/{space}/export"), None)
            .await;
        ok(&r, StatusCode::OK);
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(r.body.to_vec())).unwrap();
        for i in 0..z.len() {
            let mut f = z.by_index(i).unwrap();
            let mut s = Vec::new();
            f.read_to_end(&mut s).unwrap();
            assert!(
                !String::from_utf8_lossy(&s).contains(marker),
                "{space}: {:?} содержит вопрос пробы",
                f.name()
            );
        }
    }
}

const PROBES: &str = "/api/v1/check/probes";

/// Insight пространства `space` первым токеном; id.
async fn insight_id(a: &Api, space: &str, title: &str) -> String {
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(json!({"kind": "insight", "title": title, "body": "текст",
                        "data": {"kind": "gotcha"}})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    id(&r.json())
}

/// M7.4 §7.6 п. 3: ручная проба `POST /check/probes {question, targets, space?}` — 201 `{id}`;
/// вид `manual`, автор по принципалу (токен — `agent`, сессия WebUI — `human`), часть — общим
/// правилом по первой цели, ревизия цели — текущая, пространство — цели или явное; цели из
/// другого пространства — 422.
#[tokio::test]
async fn check_probes_add_manual() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-add-manual", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let n = id(&a
        .note("knowledge", "kb/ручная.md", "# Ручная\n\nalphamike.\n")
        .await);
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{n}"),
            Some(json!({"content": "# Ручная\n\nalphamike, правлено.\n"})),
            &[("if-match", "1")],
        )
        .await;
    ok(&r, StatusCode::OK);
    let ins = insight_id(&a, "knowledge", "Ловушка bravonovember").await;
    let t = format!("n:{n}");
    let r = a
        .t(
            Method::POST,
            PROBES,
            Some(json!({"question": "где alphamike", "targets": [t, format!("i:{ins}")]})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let pid = r.json()["id"].as_str().unwrap().to_string();
    assert!(pid.starts_with("x:"), "{pid}");
    let items = probes(&a, "?kind=manual").await;
    assert_eq!(items.len(), 1, "{items:?}");
    let p = &items[0];
    assert_eq!(p["id"], pid);
    assert_eq!(p["question"], "где alphamike");
    assert_eq!(p["author_kind"], "agent");
    assert_eq!(p["status"], "active");
    assert_eq!(p["space"], kn.to_string());
    assert_eq!(p["split"], zam_search::eval::probe_split(&t).as_str());
    assert_eq!(
        p["targets"],
        json!([{"ref": t, "rev": 2}, {"ref": format!("i:{ins}"), "rev": 1}])
    );
    // сессия WebUI — автор `human`; пространство явно
    let r = a
        .adm(
            Method::POST,
            PROBES,
            Some(
                json!({"question": "что за bravonovember", "targets": [format!("i:{ins}")],
                        "space": "knowledge"}),
            ),
            None,
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let items = probes(&a, "?kind=manual").await;
    assert_eq!(items[0]["author_kind"], "human", "{items:?}");
    assert_eq!(items[0]["author"], "admin");
    // цель не из пространства пробы — 422
    let r = a
        .t(
            Method::POST,
            PROBES,
            Some(
                json!({"question": "в rules alphamike", "targets": [format!("n:{n}")],
                        "space": "rules"}),
            ),
        )
        .await;
    ok(&r, StatusCode::UNPROCESSABLE_ENTITY);
    // ручная проба — не запись памяти: в поиске её нет
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "что за bravonovember", "format": "json"})),
        )
        .await;
    ok(&r, StatusCode::OK);
    assert!(!r.text().contains(&pid), "{}", r.text());
}

/// M7.4 §7.6 п. 3: повтор вопроса (та же нормализованная строка у активной пробы пространства) —
/// 409 `/problems/probe-duplicate`; предел активных проб с целью — 409 `/problems/probes-limit`.
#[tokio::test]
async fn check_probes_add_duplicate_409() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-add-dup", |_| {}).await;
    let n = id(&a
        .note("knowledge", "kb/d.md", "# D\n\ncharlieoscar.\n")
        .await);
    let body = |q: &str| json!({"question": q, "targets": [format!("n:{n}")]});
    ok(
        &a.t(Method::POST, PROBES, Some(body("где charlieoscar")))
            .await,
        StatusCode::CREATED,
    );
    let r = a
        .t(Method::POST, PROBES, Some(body("  Где   CHARLIEOSCAR ")))
        .await;
    common::api::problem(&r, StatusCode::CONFLICT, "probe-duplicate");
    assert_eq!(probes(&a, "?kind=manual").await.len(), 1);
    // предел: активных проб с целью — PROBES_MAX
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let target = json!([{"ref": format!("n:{n}"), "rev": 1}]).to_string();
    let now = zam_store::time::fmt_ts(zam_store::time::now());
    db.write(move |t| {
        for i in 1..PROBES_MAX {
            t.execute(
                "INSERT INTO probes (id, space_id, question, kind, targets, split, author_kind,
                                     status, created_at)
                 VALUES (?1, ?2, ?3, 'probe', ?4, 'dev', 'agent', 'active', ?5)",
                rusqlite::params![
                    Id::new(),
                    kn.to_string(),
                    format!("вопрос предела {i}"),
                    target,
                    now
                ],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let r = a
        .t(
            Method::POST,
            PROBES,
            Some(body("сверх предела charlieoscar")),
        )
        .await;
    common::api::problem(&r, StatusCode::CONFLICT, "probes-limit");
}

/// M7.4 §7.6 п. 3: цель не найдена или не действует (нет записи, удалена, отозвана, буква не того
/// вида) — 422; ссылка не `<буква>:<id>`, без целей, пустой вопрос — 400. Ничего не записано.
#[tokio::test]
async fn check_probes_add_bad_target_422() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-add-bad", |_| {}).await;
    let gone = id(&a.note("knowledge", "kb/g.md", "# G\n\ndeltaecho.\n").await);
    ok(
        &a.t(Method::DELETE, &format!("/api/v1/nodes/{gone}"), None)
            .await,
        StatusCode::NO_CONTENT,
    );
    let ins = insight_id(&a, "knowledge", "Отозванный foxtrotgolf").await;
    ok(
        &a.t(
            Method::POST,
            &format!("/api/v1/entries/{ins}/retract"),
            Some(json!({"reason": "опровергнуто"})),
        )
        .await,
        StatusCode::OK,
    );
    let live = insight_id(&a, "knowledge", "Живой hotelindia").await;
    for t in [
        format!("n:{}", Id::new()),
        format!("n:{gone}"),
        format!("i:{ins}"),
        // insight по букве заметки
        format!("n:{live}"),
    ] {
        let r = a
            .t(
                Method::POST,
                PROBES,
                Some(json!({"question": "где deltaecho", "targets": [t]})),
            )
            .await;
        common::api::problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "unprocessable");
        assert!(r.text().contains(&t), "{t}: {}", r.text());
    }
    for body in [
        json!({"question": "где deltaecho", "targets": ["deltaecho"]}),
        json!({"question": "где deltaecho", "targets": []}),
        json!({"question": "   ", "targets": [format!("i:{live}")]}),
        json!({"question": "где deltaecho", "targets": [format!("i:{live}")], "kind": "probe"}),
    ] {
        let r = a.t(Method::POST, PROBES, Some(body.clone())).await;
        ok(&r, StatusCode::BAD_REQUEST);
    }
    assert!(probes(&a, "?kind=manual").await.is_empty());
}

/// M7.4 §7.6 п. 3, Review Focus 4: снять пробу (`DELETE /check/probes/{id} {reason}`) может только
/// человек — агентский токен 403, проба остаётся; сессия WebUI — 204, аудит `check.probe.retire
/// {probe, reason}`; снятая, чужая и несуществующая — 404; причина 3..=500 символов.
#[tokio::test]
async fn check_probes_retire_human_only() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-retire-human", |_| {}).await;
    let n = id(&a.note("knowledge", "kb/r.md", "# R\n\njulietkilo.\n").await);
    let r = a
        .t(
            Method::POST,
            PROBES,
            Some(json!({"question": "где julietkilo", "targets": [format!("n:{n}")]})),
        )
        .await;
    ok(&r, StatusCode::CREATED);
    let pid = r.json()["id"].as_str().unwrap().to_string();
    let uri = format!("{PROBES}/{pid}");
    let reason = json!({"reason": "вопрос сформулирован неверно"});
    let r = a.t(Method::DELETE, &uri, Some(reason.clone())).await;
    ok(&r, StatusCode::FORBIDDEN);
    assert!(r.text().contains("человек"), "{}", r.text());
    assert_eq!(probes(&a, "?status=active&kind=manual").await.len(), 1);
    // чужая: сессия другого пользователя — 404 (в его базе пробы нет)
    let u = common::reports::ready_user(&a, "olga", "192.0.2.41").await;
    let r = common::reports::web(&a, &u.web, Method::DELETE, &uri, Some(reason.clone())).await;
    ok(&r, StatusCode::NOT_FOUND);
    // причина короче 3 символов — 422
    let r = a
        .adm(Method::DELETE, &uri, Some(json!({"reason": " x "})), None)
        .await;
    common::api::problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "reason-required");
    let r = a
        .adm(Method::DELETE, &uri, Some(reason.clone()), None)
        .await;
    ok(&r, StatusCode::NO_CONTENT);
    let retired = probes(&a, "?status=retired&kind=manual").await;
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0]["retired_reason"], "вопрос сформулирован неверно");
    let audit = a.st.system.list_audit(50, None).await.unwrap();
    let row = audit
        .iter()
        .find(|r| r.action == "check.probe.retire")
        .expect("аудит check.probe.retire");
    assert_eq!(row.actor, "admin");
    assert_eq!(row.user_id, Some(a.st.admin_id));
    assert_eq!(
        row.details,
        json!({"probe": pid, "reason": "вопрос сформулирован неверно"})
    );
    // снятая и несуществующая — 404
    let r = a
        .adm(Method::DELETE, &uri, Some(reason.clone()), None)
        .await;
    ok(&r, StatusCode::NOT_FOUND);
    let r = a
        .adm(
            Method::DELETE,
            &format!("{PROBES}/x:{}", Id::new()),
            Some(reason),
            None,
        )
        .await;
    ok(&r, StatusCode::NOT_FOUND);
}

/// M7.4 §7.6 п. 3: ввод пробы — `rw`: токен `ro` — 403 (список ему по-прежнему доступен).
#[tokio::test]
async fn check_probes_ro_403() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-ro", |_| {}).await;
    let n = id(&a.note("knowledge", "kb/ro.md", "# RO\n\nlimamike.\n").await);
    let ro = a.issue(&a.admin_id(), "ro").await;
    let r = a
        .tok(
            &ro,
            Method::POST,
            PROBES,
            Some(json!({"question": "где limamike", "targets": [format!("n:{n}")]})),
        )
        .await;
    ok(&r, StatusCode::FORBIDDEN);
    let r = a.tok(&ro, Method::GET, PROBES, None).await;
    ok(&r, StatusCode::OK);
    assert!(probes(&a, "?kind=manual").await.is_empty());
}

/// §6 «Общее»: пробы — в копии сервера (база целиком): копия снята с работающего сервера, в её
/// базе пользователя пробы на месте.
#[tokio::test]
async fn probes_in_backup() {
    let _serial = SERIAL.lock().await;
    let a = api("probes-backup", |_| {}).await;
    let kn = space_id(&a, SpaceKind::Knowledge).await;
    let n = id(&a.note("knowledge", "kb/b.md", "# B\n\nyankeezulu.\n").await);
    probe(&a, kn, "где yankeezulu", &format!("n:{n}")).await;
    let dest = common::tmp("probes-backup-copies");
    let cfg = (*a.st.cfg).clone();
    let opts = zam_server::backup::BackupOpts {
        dir: Some(dest.clone()),
        zip: None,
    };
    let rep = tokio::task::spawn_blocking(move || zam_server::backup::backup(&cfg, &opts))
        .await
        .unwrap()
        .unwrap();
    let db = rep.path.join("users").join(format!("{}.db", a.st.admin_id));
    let c = rusqlite::Connection::open(&db).unwrap();
    let q: String = c
        .query_row(
            "SELECT question FROM probes WHERE kind = 'probe'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(q, "где yankeezulu");
}
