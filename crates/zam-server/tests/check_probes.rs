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
