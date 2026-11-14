//! Приёмка M6 (спецификация M6 §17, задача 13b) — только новые по сути проверки:
//! - `acceptance_ts1_vs_b4`, `acceptance_ts2_vs_b56` — итоговый замер на holdout по индексу отчёта
//!   (`ZAM_M6_REPORT`, `eval/tools/m6_report.py`): пороги сверяются по **исходным** файлам
//!   сравнений `cargo xtask eval`, а не по сводке индекса;
//! - `acceptance_migration_0025` — база, созданная настоящим сервером v0.0.10-m5.7 (идея,
//!   вопрос-предложение, задача из идеи, показ идеи в сессии), открывается M6 без потерь.
//!
//! Совместимость с бинарником v0.0.10-m5.7 и p95 `/recall(prompt)` — в
//! `crates/zam-cli/tests/m6_acceptance.rs`. Прочие пункты §17 покрыты тестами задач этапа (таблица
//! «пункт → тесты» — журнал этапа, «Задача 13b»): п. 3 `search::brief_markdown_within_budget*`,
//! `recall::rules_reserved_never_cut`; п. 5 `agent_dates`; п. 6 `mcp::*session*`; п. 7
//! `sessions::*epoch*`, `ideas_recall::ideas_seen_across_epochs`; п. 8 `read_focus*`; п. 9
//! `recall_frozen::digest_deterministic_two_sessions`; п. 10–12 — сетки задач 9–11
//! (`eval/results/2026-11-08-*`); п. 13 `cache_api::cache_put_echo_true_old_shape`, `agent_setup`;
//! п. 14 `zam-store::migrate_rebuild::migrate_0025_from_m57_db`; п. 16 `xtask::ab`,
//! `sessions::holdout_*`; п. 17 `code_api`, `zam-code`; п. 18 тесты задач 15a–15c и e2e; п. 19
//! `snapshot` (CLI и сервер); п. 20 `ideas_recall::idea_hints_top5_match_m57`; п. 21 `mcp`,
//! `index_rules`, `zam-cli::sessions`; п. 22–23 `backup`, `transfer_*`; п. 24 — журнал 18c; п. 25 —
//! тесты задачи 19.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use common::process::ServerProc;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e60";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("корень репозитория")
}

fn load(p: &Path) -> Value {
    let t = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&t).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Индекс отчёта `ZAM_M6_REPORT` (путь — от корня репозитория или абсолютный).
fn report() -> Option<Value> {
    let p = std::env::var_os("ZAM_M6_REPORT")?;
    let p = PathBuf::from(p);
    let p = if p.is_absolute() { p } else { repo().join(p) };
    Some(load(&p))
}

/// Файл роли `role` из индекса отчёта.
fn file_of(idx: &Value, role: &str) -> PathBuf {
    let f = idx["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|f| f["role"] == role)
        .unwrap_or_else(|| panic!("в индексе нет роли {role}"));
    repo().join(f["path"].as_str().unwrap())
}

fn num(v: &Value, what: &str) -> f64 {
    v.as_f64()
        .unwrap_or_else(|| panic!("{what}: не число ({v})"))
}

/// Часть сравнения ТС-2 (`всего`, `standard`, `heavy`).
fn part<'a>(c: &'a Value, key: &str, name: &str) -> &'a Value {
    c[key]
        .as_array()
        .unwrap_or_else(|| panic!("нет {key}"))
        .iter()
        .find(|p| p["part"] == name)
        .unwrap_or_else(|| panic!("нет части {name} в {key}"))
}

/// §17 п. 1: ТС-1 к B4 на holdout — среднее по K = 3 индексации (CUDA fp16, критерий) и контроль
/// int8 CPU (выводы о полноте те же).
#[test]
#[ignore = "итоговый замер: ZAM_M6_REPORT — индекс eval/results/<дата>-m6-bge-m3-fp16-tokens.json"]
fn acceptance_ts1_vs_b4() {
    let Some(idx) = report() else {
        eprintln!("ZAM_M6_REPORT не задан — пропуск");
        return;
    };
    let c = load(&file_of(&idx, "ts1.cuda"));
    assert_eq!(c["split"], "holdout");
    for side in ["arm_files", "base_files"] {
        let n = c[side].as_array().map_or(0, Vec::len);
        assert!(n >= 3, "{side}: повторов {n} < 3 (методика, правило 3)");
    }
    let ex: Vec<&str> = c["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for id in ["s-19", "s-44", "s-45"] {
        assert!(ex.contains(&id), "к B4 {id} должен быть исключён: {ex:?}");
    }
    let d = num(&c["d_micro"]["lo"], "Δ микро-полноты");
    let l = c["net_losses"].as_i64().unwrap();
    let r_hi = num(&c["r"]["hi"], "R.hi");
    assert!(d >= -0.02, "нижняя граница Δ микро-полноты {d} < −0,02");
    assert!(l <= 1, "чистые потери L = {l} > 1");
    assert!(r_hi < 1.0, "верхняя граница R {r_hi} ≥ 1,00");
    eprintln!(
        "ZAM_INFO ТС-1 к B4 (CUDA, K=3): R {:.3} [{:.3}–{r_hi:.3}], Δ микро {d:.3}, L {l}; цель \
         R ≤ 0,70 — {}",
        num(&c["r"]["mean"], "R"),
        num(&c["r"]["lo"], "R.lo"),
        if num(&c["r"]["mean"], "R") <= 0.70 {
            "достигнута"
        } else {
            "не достигнута (факт)"
        }
    );
    let cpu = load(&file_of(&idx, "ts1.cpu"));
    assert_eq!(cpu["split"], "holdout");
    let d = num(&cpu["d_micro"]["lo"], "CPU Δ микро-полноты");
    let l = cpu["net_losses"].as_i64().unwrap();
    assert!(
        d >= -0.02 && l <= 1,
        "контроль int8 CPU: выводы о полноте другие (Δ {d}, L {l})"
    );
}

/// §17 п. 2: ТС-2 на holdout у обоих агентов хуков — полный `R` к B5.6 (standard) ≤ 1,15, `R` без
/// Rules к B5.7 (standard) ≤ 1,00, обязательные строки 1,0, Δ `hint_recall` ≥ −0,02 (оба
/// варианта), подсказки-идеи 1,0, повторов с промахами нет, плечи одних суток UTC.
#[test]
#[ignore = "итоговый замер: ZAM_M6_REPORT — индекс eval/results/<дата>-m6-bge-m3-fp16-tokens.json"]
fn acceptance_ts2_vs_b56() {
    let Some(idx) = report() else {
        eprintln!("ZAM_M6_REPORT не задан — пропуск");
        return;
    };
    for agent in ["claude", "codex"] {
        let role = |r: &str| file_of(&idx, &format!("ts2.{agent}.{r}"));
        let v56 = load(&role("vs_b56"));
        let v57 = load(&role("vs_b57"));
        let mut arms = vec![("m6", load(&role("m6"))), ("b56", load(&role("b56")))];
        arms.push(("b57", load(&role("b57"))));
        let mut dates = std::collections::BTreeSet::new();
        for (n, a) in &arms {
            let i = &a["info"];
            assert_eq!(i["split"], "holdout", "{agent} {n}");
            assert_eq!(i["device"], "cuda", "{agent} {n}");
            assert_eq!(i["repeat"], 3, "{agent} {n}");
            assert_eq!(i["hook_agent"], agent, "{agent} {n}");
            dates.insert(i["date"].as_str().unwrap().to_string());
            let misses = &a["summary"]["reps_with_misses"];
            assert!(
                misses.as_array().is_some_and(Vec::is_empty),
                "{agent} {n}: повторы с промахом бюджета хука или неразобранным выводом {misses}"
            );
        }
        assert_eq!(dates.len(), 1, "{agent}: плечи разных суток UTC {dates:?}");
        let rf = num(
            &part(&v56, "parts", "standard")["total"]["r"]["mean"],
            "R полный",
        );
        let rn = num(
            &part(&v57, "parts_no_rules", "standard")["total"]["r"]["mean"],
            "R без Rules",
        );
        assert!(rf <= 1.15, "{agent}: R полный к B5.6, standard {rf} > 1,15");
        assert!(
            rn <= 1.00,
            "{agent}: R без Rules к B5.7, standard {rn} > 1,00"
        );
        let all = part(&v56, "parts", "всего");
        for d in ["d_hint_recall", "d_hint_recall_strict"] {
            let lo = num(&all[d]["lo"], d);
            assert!(lo >= -0.02, "{agent}: {d} нижняя граница {lo} < −0,02");
        }
        let m6 = &arms[0].1;
        for s in m6["sessions"].as_array().unwrap() {
            for r in s["required"].as_array().into_iter().flatten() {
                assert_eq!(
                    r["found"], true,
                    "{agent}: обязательная строка не выдана: {} r{} {r}",
                    s["id"], s["rep"]
                );
            }
        }
        for (rep, t) in m6["summary"]["required_by_rep"].as_object().unwrap() {
            assert_eq!(
                t["found"], t["total"],
                "{agent}: обязательные строки, повтор {rep}"
            );
        }
        let ih = &m6["summary"]["idea_hint_recall"];
        assert_eq!(ih["found"], ih["total"], "{agent}: подсказки-идеи M6 {ih}");
        if let Some(m) = v57["idea_hints_match"].as_object() {
            let block = &idx["ts2"][agent];
            let ex = block["idea_hint_exceptions"]["count"].as_u64().unwrap_or(0);
            assert_eq!(
                m["found"].as_u64().unwrap() + ex,
                m["total"].as_u64().unwrap(),
                "{agent}: подсказки-идеи не совпали с B5.7 сверх расхождений по стоп-основам"
            );
        }
        eprintln!(
            "ZAM_INFO ТС-2 {agent}: R полный к B5.6 standard {rf:.3}, без Rules к B5.7 standard \
             {rn:.3}"
        );
    }
}

/// Бинарник `zam-server` v0.0.10-m5.7: `ZAM_M57_SERVER_BIN` или `target/tags/v0.0.10-m5.7/release`.
fn m57_server() -> Option<PathBuf> {
    let p = std::env::var_os("ZAM_M57_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo()
                .join("target/tags/v0.0.10-m5.7/release")
                .join(if cfg!(windows) {
                    "zam-server.exe"
                } else {
                    "zam-server"
                })
        });
    p.is_file().then_some(p)
}

fn call(s: &ServerProc, tok: &str, m: &str, path: &str, body: Option<Value>) -> Value {
    let (code, v) = s.call(m, path, tok, body.as_ref());
    assert!((200..300).contains(&code), "{m} {path}: {code} {v}");
    v
}

fn recall_http(s: &ServerProc, tok: &str, session: &str, body: &Value) -> Value {
    let auth = format!("Bearer {tok}");
    let (code, _, v) = common::process::http_with(
        s.addr,
        "POST",
        "/api/v1/recall",
        &[("Authorization", &auth), ("X-Zam-Session", session)],
        Some(body),
    )
    .unwrap();
    assert_eq!(code, 200, "recall: {v}");
    v
}

fn user_db(data: &Path) -> PathBuf {
    std::fs::read_dir(data.join("users"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "db"))
        .expect("база пользователя")
}

/// `session_start` решения `G` в сессии `s` (M6 в процессе теста).
async fn session_start(a: &common::api::Api, tok: &str, s: &str) -> common::api::Resp {
    a.tok_h(
        tok,
        Method::POST,
        "/api/v1/recall",
        Some(json!({"event": "session_start", "solution": G})),
        &[("x-zam-session", s)],
    )
    .await
}

/// §17 п. 14 на настоящей базе: v0.0.10-m5.7 (схема 0024) создаёт идею человека, показывает её в
/// дайджесте сессии (`session_seen`), предлагает вторую (вопрос-предложение) и принимает третью
/// (задача из идеи); M6 открывает каталог — миграция 0025 без потерь: `bak-v24`, строки
/// `session_seen` в эпохе 0, идея «показана» в своей сессии и возвращается в новой, вопрос и задача
/// на месте, `integrity_check` и `foreign_key_check` чисты, новые столбцы с умолчаниями.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_migration_0025() {
    let Some(exe) = m57_server() else {
        eprintln!(
            "нет бинарника v0.0.10-m5.7 (ZAM_M57_SERVER_BIN, target/tags/v0.0.10-m5.7/release) — \
             пропуск"
        );
        return;
    };
    let root = common::tmp("m6-migration-0025");
    let data = root.join("data");
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, common::qdrant::free_port()));
    let conf = root.join("m57.toml");
    std::fs::write(
        &conf,
        format!(
            "data_dir = '{}'\n\n[server]\nbind = \"{addr}\"\n",
            data.display()
        ),
    )
    .unwrap();
    let mut old = ServerProc::start_exe(&exe, &conf, addr);
    let tok = old.initial_token(Duration::from_secs(120));
    let t0 = std::time::Instant::now();
    while old.try_call("GET", "/api/v1/health", &tok, None).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(60), "{}", old.tail());
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, h) = old.call("GET", "/api/v1/health", &tok, None);
    assert_eq!(h["version"], "0.0.10-m5.7", "{h}");
    call(
        &old,
        &tok,
        "POST",
        "/api/v1/solutions",
        Some(json!({"agent_solution_id": G, "name": "Миграция 0025"})),
    );
    let ents = format!("/api/v1/spaces/{G}/entries");
    let idea = |title: &str| {
        let v = call(
            &old,
            &tok,
            "POST",
            &ents,
            Some(
                json!({"kind": "idea", "title": title, "body": "Замысел человека.",
                        "data": {"origin": "human", "priority": "high"}}),
            ),
        );
        v["id"].as_str().unwrap().to_string()
    };
    let shown = idea("Экспорт отчётов продавцов в PDF");
    let proposed = idea("Подсказки адреса при оформлении заказа");
    let accepted = idea("Подарочные карты в витрине");
    let digest = recall_http(
        &old,
        &tok,
        "m57-s1",
        &json!({"event": "session_start", "solution": G}),
    );
    let text = digest["text"].as_str().unwrap();
    assert!(text.contains("### Идеи") && text.contains(&shown), "{text}");
    let p = call(
        &old,
        &tok,
        "POST",
        &format!("/api/v1/entries/{proposed}/propose"),
        Some(json!({"text": "подходит к текущей задаче"})),
    );
    let question = p["question"]["id"].as_str().unwrap().to_string();
    call(
        &old,
        &tok,
        "POST",
        &format!("/api/v1/entries/{accepted}/decide"),
        Some(json!({"decision": "accept", "by": "human"})),
    );
    let acc = call(
        &old,
        &tok,
        "GET",
        &format!("/api/v1/entries/{accepted}"),
        None,
    );
    assert_eq!(acc["status"], "accepted", "{acc}");
    // записи подтверждены ответами; на Windows штатной остановки у процесса теста нет —
    // жёсткое завершение (WAL SQLite восстанавливается при открытии)
    #[cfg(unix)]
    old.terminate(Duration::from_secs(30));
    #[cfg(not(unix))]
    old.kill();
    drop(old);

    let db = user_db(&data);
    let (seen_before, version_before) = {
        let c = rusqlite::Connection::open(&db).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM session_seen", [], |r| r.get(0))
            .unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        (n, v)
    };
    assert_eq!(version_before, 24, "база v0.0.10-m5.7 — схема 0024");
    assert!(seen_before > 0, "показ идеи не отмечен в session_seen");

    // M6 на том же каталоге
    let a = common::api::api_with(common::cfg_in(data.clone())).await;
    let r = a.tok(&tok, Method::GET, "/api/v1/health", None).await;
    assert_eq!(r.status, StatusCode::OK);
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 25, "миграция 0025");
        let ok: String = c
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        let fk: i64 = c
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk, 0, "foreign_key_check не пуст");
        let (n, epochs): (i64, i64) = c
            .query_row(
                "SELECT count(*), coalesce(sum(epoch), 0) FROM session_seen",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (n, epochs),
            (seen_before, 0),
            "строки session_seen — в эпохе 0"
        );
        for (table, col) in [
            ("sessions", "seen_epoch"),
            ("sessions", "holdout"),
            ("code_refs", "recv_count"),
            ("session_tokens", "reread_calls"),
        ] {
            let has: i64 = c
                .query_row(
                    &format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                    [col],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has, 1, "{table}.{col}");
        }
        for t in [
            "session_snapshots",
            "tokens_unattributed",
            "node_idempotency",
        ] {
            let has: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has, 1, "таблица {t}");
        }
    }
    let bak = db.with_file_name(format!(
        "{}.bak-v24",
        db.file_name().unwrap().to_string_lossy()
    ));
    assert!(bak.exists(), "нет {}", bak.display());
    for (id, status) in [(&proposed, "proposed"), (&accepted, "accepted")] {
        let r = a
            .tok(&tok, Method::GET, &format!("/api/v1/entries/{id}"), None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["status"], status, "{}", r.text());
    }
    let r = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/entries/{question}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "вопрос-предложение: {}", r.text());
    assert_eq!(r.json()["kind"], "question", "{}", r.text());
    let r = a
        .tok(
            &tok,
            Method::GET,
            &format!("/api/v1/spaces/{G}/entries?kind=task"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.text().contains("Подарочные карты"),
        "задача из принятой идеи: {}",
        r.text()
    );
    let same = session_start(&a, &tok, "m57-s1").await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.text());
    let same = same.json()["text"].as_str().unwrap_or_default().to_string();
    assert!(
        !same.contains(&shown),
        "идея, показанная v0.0.10-m5.7, вернулась в своей сессии: {same}"
    );
    let fresh = session_start(&a, &tok, "m6-s2").await;
    let fresh = fresh.json()["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        fresh.contains(&shown),
        "в новой сессии идея должна вернуться: {fresh}"
    );
}
