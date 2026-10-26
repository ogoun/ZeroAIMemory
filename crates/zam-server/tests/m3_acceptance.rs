//! Приёмка M3 (§18.1 ТЗ, задача 20): настоящий процесс `zam-server` (`common::process`),
//! MiniLM на CPU-сборке ORT и Qdrant 1.19.2 из `target/deps`.
//!
//! Три сценария сквозь весь сервер — от HTTP до байтов на диске:
//! - «Изоляция»: Human и Vault не попадают в Qdrant, в выдачу по токену и в `logs/`,
//!   значения Vault — в FTS; участник без доступа к решению получает 403 `solution-access`
//!   на дерево, узел, записи, граф и вложение, поиск исключает решение с предупреждением;
//!   после одобрения `ro` — чтение пространства решения, запись 403, Knowledge, Rules,
//!   Human и Vault владельца — 404; в режиме `rw` правка участника ложится ревизией с его
//!   атрибуцией; после отзыва тот же токен — 403 на следующем запросе;
//! - «Замещение без потери»: insight-гипотеза с триггером замещается теоремой с evidence;
//!   поиск по умолчанию находит новую, с `include_history` — обе; старая `superseded`,
//!   ревизии целы, ребро `supersedes`, триггеры старой не выдаются; агент не отзывает
//!   `axiom`, человеческий токен — отзывает;
//! - «Секрет через агента»: запись и раскрытие с `X-Zam-Session`/`X-Zam-Machine`, аудит
//!   `vault.reveal` без значения, значения нет в байтах каталога данных и журнале;
//!   перезапуск с тем же мастер-ключом раскрывает, с чужим — 503 `vault-locked`.
//!
//! Подробные матрицы прав каждого пути — в `solutions_access.rs`, `search_multi.rs`,
//! `insights.rs`, `certainty.rs`, `graph.rs` и `vault.rs`; здесь — сквозная проверка
//! собранного процесса. Без ORT, MiniLM или Qdrant тесты пропускаются; в CI
//! (`ZAM_REQUIRE_ML=1`) — падают.
mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::ml::{MINILM, READY, have, ml};
use common::process::{AdminWeb, ServerProc, http};
use common::qdrant::{TestQdrant, free_port};
use serde_json::{Value, json};
use zam_server::config::{LogFormat, QdrantMode, ServerConfig};

/// Решения сценариев (у каждого свой GUID — каталоги данных и так разные).
const G_ISO: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e71";
const G_SUP: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e72";
const G_SEC: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e73";
const BOUNDARY: &str = "zam-m3-acceptance-7d1c";
const SESSION: &str = "sess-m3-acceptance";
const MACHINE: &str = "box-m3-acceptance";

/// Уникальная строка: в исходниках теста её нет целиком, поиск по байтам однозначен.
fn marker(tag: &str) -> String {
    format!("zm3{tag}{}{}", "Qx7Lp2", "Rt9Wd4")
}

// ---- каталог, конфигурация, процесс ----

/// Каталог теста, конфигурация с журналом `trace` в файл (`<data>/logs`) и привязкой к
/// свободному порту; `tweak` — модели и Qdrant.
fn config(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> (PathBuf, PathBuf, SocketAddr) {
    let root = common::tmp(&format!("m3acc-{name}"));
    let data = root.join("data");
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
    let mut c = common::cfg_in(data.clone());
    c.server.bind = addr;
    c.log.level = "trace".into();
    c.log.format = LogFormat::Text;
    tweak(&mut c);
    let conf = root.join("zam.toml");
    std::fs::write(&conf, toml::to_string(&c).unwrap()).unwrap();
    (conf, data, addr)
}

/// MiniLM на CPU и внешний Qdrant теста (его API-ключ нужен для прямого подсчёта точек).
fn with_ml(c: &mut ServerConfig, q: &TestQdrant) {
    ml(c, MINILM);
    c.qdrant.mode = QdrantMode::External;
    c.qdrant.url = Some(q.grpc_url());
    c.qdrant.api_key = Some(q.api_key.clone());
}

/// Ждёт, пока сервер начнёт принимать запросы (bind — после открытия баз и модели).
fn listening(srv: &mut ServerProc, token: &str) {
    let t = Instant::now();
    while srv.try_call("GET", "/api/v1/spaces", token, None).is_err() {
        srv.assert_running("приём соединений");
        assert!(t.elapsed() < READY, "сервер не слушает порт за {READY:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Индексация всех пользователей в покое (статус в сессии администратора): векторный
/// канал включён, очередь пуста, пачки и отложенных свёрток L0 нет.
fn idle(srv: &mut ServerProc, web: &AdminWeb) {
    let session = web.cookie.strip_prefix("zam_session=").unwrap().to_string();
    let t = Instant::now();
    let mut last: String;
    loop {
        srv.assert_running("покой индексации");
        match srv.admin_get("/api/v1/index/status", &session) {
            Ok((200, s))
                if s["vector"]["available"] == true
                    && s["queue"]["queued"] == 0
                    && s["queue"]["running"] == 0
                    && s["queue"]["failed"] == 0
                    && s["worker"]["batch_running"] == false
                    && s["worker"]["rollups_pending"] == 0 =>
            {
                return;
            }
            Ok((code, s)) => last = format!("{code} {s}"),
            Err(e) => last = format!("нет ответа: {e}"),
        }
        assert!(
            t.elapsed() < READY,
            "индексация не пришла в покой за {READY:?}; {last}\nstderr:\n{}",
            srv.tail()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Новый пользователь: (id, токен `rw`).
fn new_user(srv: &ServerProc, web: &AdminWeb, name: &str) -> (String, String) {
    let (code, j) = srv.admin_call(
        web,
        "POST",
        "/api/v1/admin/users",
        Some(&json!({ "name": name })),
        None,
    );
    assert_eq!(code, 201, "{j}");
    (s(&j["user"]["id"]), s(&j["token"]["plaintext"]))
}

fn s(v: &Value) -> String {
    v.as_str()
        .unwrap_or_else(|| panic!("не строка: {v}"))
        .to_string()
}

/// Создание (201) — тело ответа.
#[track_caller]
fn created((code, j): (u16, Value)) -> Value {
    assert_eq!(code, 201, "{j}");
    j
}

/// 403 `/problems/solution-access` с состоянием `state`.
#[track_caller]
fn denied((code, j): (u16, Value), state: &str, what: &str) {
    assert_eq!(code, 403, "{what}: {j}");
    assert_eq!(j["type"], "/problems/solution-access", "{what}: {j}");
    assert_eq!(j["state"], state, "{what}: {j}");
}

/// 403 `/problems/certainty` с кодом `axiom-requires-human`.
#[track_caller]
fn axiom_denied((code, j): (u16, Value)) {
    assert_eq!(code, 403, "{j}");
    assert_eq!(j["type"], "/problems/certainty", "{j}");
    assert_eq!(j["code"], "axiom-requires-human", "{j}");
}

fn note(space: &str, path: &str, content: &str) -> Value {
    json!({"space": space, "path": path, "kind": "note", "content": content})
}

/// `multipart/form-data` вложения к узлу `node`.
fn multipart(node: &str, file: &str, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{node}\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(data);
    b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    b
}

/// Поиск токеном: 200, тело.
fn search(srv: &ServerProc, token: &str, body: Value) -> Value {
    let (code, j) = srv.call("POST", "/api/v1/search", token, Some(&body));
    assert_eq!(code, 200, "{j}");
    j
}

/// Значения поля `key` элементов выдачи.
fn hits(j: &Value, key: &str) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"))
        .iter()
        .filter_map(|i| i[key].as_str().map(str::to_string))
        .collect()
}

/// Файлы каталога (рекурсивно), содержащие байты `needle`.
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_containing(&p, needle));
        } else if let Ok(b) = std::fs::read(&p)
            && b.windows(needle.len()).any(|w| w == needle)
        {
            out.push(p);
        }
    }
    out
}

/// В каталоге журналов есть файл журнала сервера (иначе проверка «нет в журнале» пуста).
fn has_server_log(data: &Path) {
    let logs = data.join("logs");
    let any = std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("{}: {e}", logs.display()))
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("zam-server.log")
                && e.metadata().is_ok_and(|m| m.len() > 0)
        });
    assert!(any, "нет непустого журнала сервера в {}", logs.display());
}

/// Все точки коллекции с payload — прямым REST API Qdrant, мимо адаптера и его фильтров.
fn qdrant_points(q: &TestQdrant) -> Vec<Value> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, q.http_port));
    let mut out = Vec::new();
    let mut offset = Value::Null;
    loop {
        let (code, v) = http(
            addr,
            "POST",
            &format!("/collections/{}/points/scroll", zam_index::COLLECTION),
            Some(&q.api_key),
            Some(
                &json!({"limit": 1000, "with_payload": true, "with_vector": false,
                         "offset": offset}),
            ),
        )
        .expect("scroll Qdrant");
        assert_eq!(code, 200, "{v}");
        out.extend(v["result"]["points"].as_array().unwrap().iter().cloned());
        offset = v["result"]["next_page_offset"].clone();
        if offset.is_null() {
            return out;
        }
    }
}

// ---- 1. Изоляция ----

/// Human и Vault вне Qdrant, выдачи по токену и журнала; доступ участника к решению:
/// ожидание → 403, `ro` → чтение без записи и без остального владельца, `rw` → запись с
/// атрибуцией участника, отзыв → 403 сразу.
#[test]
fn isolation_human_vault_and_solution_access() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("m3acc-iso");
    let (conf, data, addr) = config("iso", |c| with_ml(c, &q));
    let mut srv = ServerProc::start(&conf, addr);
    let admin = srv.initial_token(READY);
    listening(&mut srv, &admin);
    let web = srv.admin_login();
    let (a_id, a) = new_user(&srv, &web, "Анна");
    let (b_id, b) = new_user(&srv, &web, "Борис");

    // данные владельца A: Human и Rules — только из WebUI, остальное — агентом A
    let human_word = marker("human");
    let vault_value = marker("vault");
    let shared_word = "квазарометрия";
    let (code, h) = srv.admin_call(
        &web,
        "POST",
        "/api/v1/nodes",
        Some(&note(
            "human",
            "дневник",
            &format!("# Дневник\n\nЛичное наблюдение {human_word} о сне и усталости.\n"),
        )),
        Some(&a_id),
    );
    assert_eq!(code, 201, "{h}");
    let human_id = s(&h["id"]);
    let (code, r) = srv.admin_call(
        &web,
        "POST",
        "/api/v1/nodes",
        Some(&note(
            "rules",
            "Правило",
            "# Правило\n\nКоммиты — на русском.\n",
        )),
        Some(&a_id),
    );
    assert_eq!(code, 201, "{r}");
    let rules_id = s(&r["id"]);
    let k = created(srv.call(
        "POST",
        "/api/v1/nodes",
        &a,
        Some(&note(
            "knowledge",
            "личное/сборка",
            &format!("# Сборка\n\nЛичная заметка Анны про {shared_word} и cargo.\n"),
        )),
    ));
    let k_id = s(&k["id"]);
    let v = created(srv.call_h(
        "POST",
        "/api/v1/spaces/knowledge/vault",
        &a,
        Some(&json!({"title": "github", "data": {"type": "token"}, "secret": vault_value})),
        &[("X-Zam-Session", SESSION), ("X-Zam-Machine", MACHINE)],
    ));
    let vault_id = s(&v["id"]);
    // решение A: заметка, вложение, insight
    let reg = json!({"agent_solution_id": G_ISO, "name": "Общее"});
    created(srv.call("POST", "/api/v1/solutions", &a, Some(&reg)));
    let sol = created(srv.call(
        "POST",
        "/api/v1/nodes",
        &a,
        Some(&note(
            G_ISO,
            "архитектура",
            &format!("# Архитектура\n\nОбщая заметка решения про {shared_word} и деплой.\n"),
        )),
    ));
    let sol_id = s(&sol["id"]);
    let (code, up) = srv.call_bytes(
        "POST",
        "/api/v1/blobs",
        &a,
        Some((
            &format!("multipart/form-data; boundary={BOUNDARY}"),
            &multipart(&sol_id, "a.bin", b"solution-blob"),
        )),
    );
    assert_eq!(code, 201, "{}", String::from_utf8_lossy(&up));
    let sha = s(&serde_json::from_slice::<Value>(&up).unwrap()["sha256"]);
    let blob = format!("/api/v1/blobs/{sha}/a.bin");
    let ins = created(srv.call(
        "POST",
        &format!("/api/v1/spaces/{G_ISO}/entries"),
        &a,
        Some(&json!({"kind": "insight", "title": "Деплой только из main",
                     "body": "Ветки фич не выкатываются.", "data": {"kind": "decision"}})),
    ));
    let ins_id = s(&ins["id"]);
    idle(&mut srv, &web);

    // Human и Vault — не в Qdrant (точки есть, и среди них нет ни Human, ни Vault)
    let points = qdrant_points(&q);
    assert!(
        points
            .iter()
            .any(|p| p["payload"]["node_id"] == k_id.as_str()),
        "заметка Knowledge в Qdrant: проверка содержательна"
    );
    assert!(
        points
            .iter()
            .any(|p| p["payload"]["node_id"] == sol_id.as_str()),
        "заметка решения в Qdrant"
    );
    for p in &points {
        let pl = &p["payload"];
        assert_ne!(pl["space_kind"], "human", "{p}");
        assert_ne!(pl["node_id"], human_id.as_str(), "{p}");
        assert_ne!(pl["entry_id"], vault_id.as_str(), "{p}");
        let t = p.to_string();
        assert!(!t.contains(&human_word) && !t.contains(&vault_value), "{p}");
    }
    // Human не виден по токену ни узлом, ни в поиске, ни в списке пространств
    for tok in [&a, &admin] {
        let (code, j) = srv.call("GET", &format!("/api/v1/nodes/{human_id}"), tok, None);
        assert_eq!(code, 404, "{j}");
        let j = search(&srv, tok, json!({"query": human_word}));
        assert!(!j.to_string().contains(&human_id), "{j}");
        assert!(!j.to_string().contains(&human_word), "{j}");
        let (_, sp) = srv.call("GET", "/api/v1/spaces", tok, None);
        assert!(
            !sp["spaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["kind"] == "human"),
            "{sp}"
        );
    }
    // значения Vault нет в FTS: поиск по значению его не находит
    for mode in ["chunks", "notes"] {
        let j = search(&srv, &a, json!({"query": vault_value, "mode": mode}));
        assert!(!j.to_string().contains(&vault_value), "{j}");
        assert!(hits(&j, "entry_id").is_empty(), "{j}");
    }

    // B запросил доступ и ждёт: 403 на каждом пути, поиск — с предупреждением
    let (code, j) = srv.call("POST", "/api/v1/solutions", &b, Some(&reg));
    assert_eq!(code, 202, "{j}");
    let solution_paths = [
        format!("/api/v1/spaces/{G_ISO}/tree"),
        format!("/api/v1/nodes/{sol_id}"),
        format!("/api/v1/spaces/{G_ISO}/entries"),
        format!("/api/v1/entries/{ins_id}"),
        format!("/api/v1/graph?node={sol_id}"),
        blob.clone(),
    ];
    for p in &solution_paths {
        denied(srv.call("GET", p, &b, None), "pending", p);
    }
    let sq = json!({"query": shared_word, "scope": {"solution": G_ISO}});
    let j = search(&srv, &b, sq.clone());
    assert_eq!(j["warnings"], json!(["solution access pending"]), "{j}");
    assert!(!hits(&j, "node_id").contains(&sol_id), "{j}");

    // одобрение ro: чтение пространства решения, запись — 403
    let (code, j) = srv.call(
        "POST",
        &format!("/api/v1/solutions/{G_ISO}/access/{b_id}/approve"),
        &a,
        Some(&json!({"access": "ro"})),
    );
    assert_eq!(code, 200, "{j}");
    for p in &solution_paths {
        let (code, _) = if p == &blob {
            let (c, b) = srv.call_bytes("GET", p, &b, None);
            assert_eq!(b, b"solution-blob", "{p}");
            (c, Value::Null)
        } else {
            srv.call("GET", p, &b, None)
        };
        assert_eq!(code, 200, "{p}");
    }
    let j = search(&srv, &b, sq.clone());
    assert_eq!(j["warnings"], json!([]), "{j}");
    assert!(hits(&j, "node_id").contains(&sol_id), "{j}");
    assert!(
        !hits(&j, "node_id").contains(&k_id),
        "Knowledge A не в выдаче B: {j}"
    );
    let (code, j) = srv.call(
        "POST",
        "/api/v1/nodes",
        &b,
        Some(&note(G_ISO, "взлом", "# x\n")),
    );
    assert_eq!(code, 403, "{j}");
    let (code, j) = srv.call_h(
        "PUT",
        &format!("/api/v1/nodes/{sol_id}"),
        &b,
        Some(&json!({"content": "# правка\n"})),
        &[("If-Match", "\"1\"")],
    );
    assert_eq!(code, 403, "{j}");
    let (code, j) = srv.call(
        "POST",
        &format!("/api/v1/spaces/{G_ISO}/entries"),
        &b,
        Some(&json!({"kind": "insight", "title": "x", "data": {"kind": "gotcha"}})),
    );
    assert_eq!(code, 403, "{j}");
    // Knowledge, Rules, Human и Vault A — 404 по id; пространства A, кроме решения, — 404
    for p in [
        format!("/api/v1/nodes/{k_id}"),
        format!("/api/v1/nodes/{rules_id}"),
        format!("/api/v1/nodes/{human_id}"),
        format!("/api/v1/vault/{vault_id}"),
        format!("/api/v1/vault/{vault_id}?reveal=1"),
    ] {
        let (code, j) = srv.call("GET", &p, &b, None);
        assert_eq!(code, 404, "{p}: {j}");
        assert!(!j.to_string().contains(&vault_value), "{j}");
    }
    let (_, sp) = srv.call("GET", "/api/v1/spaces", &a, None);
    let a_spaces: Vec<(String, String)> = sp["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (s(&x["id"]), s(&x["kind"])))
        .collect();
    assert!(a_spaces.iter().any(|(_, k)| k == "knowledge"), "{sp}");
    for (id, kind) in &a_spaces {
        let (code, j) = srv.call("GET", &format!("/api/v1/spaces/{id}/tree"), &b, None);
        let want = if kind == "solution" { 200 } else { 404 };
        assert_eq!(code, want, "{kind}: {j}");
    }

    // режим rw: правка участника — ревизия в базе A с атрибуцией B (§19: запись с атрибуцией)
    let (code, j) = srv.call(
        "POST",
        &format!("/api/v1/solutions/{G_ISO}/access/{b_id}/approve"),
        &a,
        Some(&json!({"access": "rw"})),
    );
    assert_eq!(code, 200, "{j}");
    let (code, j) = srv.call_h(
        "PUT",
        &format!("/api/v1/nodes/{sol_id}"),
        &b,
        Some(&json!({"content": "# Архитектура\n\nПравка участника.\n"})),
        &[("If-Match", "\"1\"")],
    );
    assert_eq!(code, 200, "{j}");
    let (code, revs) = srv.call(
        "GET",
        &format!("/api/v1/nodes/{sol_id}/revisions"),
        &a,
        None,
    );
    assert_eq!(code, 200, "{revs}");
    assert_eq!(revs[0]["rev"], 2, "{revs}");
    assert_eq!(revs[0]["user_id"], b_id.as_str(), "{revs}");
    assert_eq!(revs[1]["user_id"], a_id.as_str(), "{revs}");

    // отзыв: тот же токен B — 403 уже на следующем запросе
    let (code, j) = srv.call(
        "POST",
        &format!("/api/v1/solutions/{G_ISO}/access/{b_id}/revoke"),
        &a,
        None,
    );
    assert_eq!(code, 200, "{j}");
    for p in &solution_paths {
        denied(srv.call("GET", p, &b, None), "revoked", p);
    }
    denied(
        srv.call_h(
            "PUT",
            &format!("/api/v1/nodes/{sol_id}"),
            &b,
            Some(&json!({"content": "# после отзыва
"})),
            &[("If-Match", "\"2\"")],
        ),
        "revoked",
        "PUT после отзыва",
    );
    let j = search(&srv, &b, sq);
    assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
    assert!(hits(&j, "node_id").is_empty(), "{j}");

    // журнал и байты: Human и значение Vault — нет в logs/, значение — нигде на диске
    srv.kill();
    has_server_log(&data);
    for needle in [&human_word, &vault_value] {
        let hits = files_containing(&data.join("logs"), needle.as_bytes());
        assert!(hits.is_empty(), "{needle} в журнале: {hits:?}");
        let hits = files_containing(&q.dir, needle.as_bytes());
        assert!(hits.is_empty(), "{needle} в хранилище Qdrant: {hits:?}");
    }
    let hits = files_containing(&data, vault_value.as_bytes());
    assert!(
        hits.is_empty(),
        "значение Vault в каталоге данных: {hits:?}"
    );
    assert!(!srv.tail().contains(&vault_value) && !srv.tail().contains(&human_word));
}

// ---- 2. Замещение без потери ----

/// Гипотеза X с триггером → `supersede` теоремой Y с evidence: поиск, история, ребро,
/// триггеры; `axiom` отзывает только человек.
#[test]
fn supersede_keeps_history_and_axiom_needs_human() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("m3acc-sup");
    let (conf, _data, addr) = config("sup", |c| with_ml(c, &q));
    let mut srv = ServerProc::start(&conf, addr);
    let agent = srv.initial_token(READY);
    listening(&mut srv, &agent);
    let web = srv.admin_login();
    let reg = json!({"agent_solution_id": G_SUP, "name": "Замещение"});
    created(srv.call("POST", "/api/v1/solutions", &agent, Some(&reg)));
    let entries = format!("/api/v1/spaces/{G_SUP}/entries");

    let x = created(srv.call(
        "POST",
        &entries,
        &agent,
        Some(
            &json!({"kind": "insight", "title": "Flaky tests come from the cache",
                     "body": "Clearing the build cache seems to fix flaky integration tests.",
                     "data": {"kind": "gotcha"}, "certainty": "hypothesis",
                     "triggers": [{"type": "keyword", "pattern": "flaky cache"}]}),
        ),
    ));
    let x_id = s(&x["id"]);
    assert_eq!(x["certainty"], "hypothesis", "{x}");
    let (code, j) = srv.call(
        "POST",
        &format!("/api/v1/entries/{x_id}/supersede"),
        &agent,
        Some(&json!({"title": "Flaky tests come from shared ports",
                     "body": "Integration tests were flaky because parallel runs shared ports; \
                              the build cache was not the cause.",
                     "certainty": "theorem",
                     "data": {"kind": "discovery", "evidence": {
                         "checked": "ci runs", "method": "50 runs with unique ports",
                         "result": "0 failures"}},
                     "triggers": [{"type": "keyword", "pattern": "shared ports"}]})),
    );
    assert_eq!(code, 201, "{j}");
    let y = &j["entry"];
    let y_id = s(&y["id"]);
    assert_eq!(y["certainty"], "theorem", "{y}");
    assert_eq!(y["supersedes_id"], x_id.as_str(), "{y}");
    assert_eq!(j["superseded"]["status"], "superseded", "{j}");
    idle(&mut srv, &web);

    // поиск: по умолчанию — только Y, с историей — обе
    let qy = json!({"query": "flaky integration tests", "scope": {"solution": G_SUP},
                    "filters": {"kinds": ["insight"]}});
    let j = search(&srv, &agent, qy.clone());
    assert_eq!(j["degraded"], false, "{j}");
    let ids = hits(&j, "entry_id");
    assert!(ids.contains(&y_id) && !ids.contains(&x_id), "{j}");
    let mut qh = qy;
    qh["include_history"] = json!(true);
    let ids = hits(&search(&srv, &agent, qh), "entry_id");
    assert!(ids.contains(&y_id) && ids.contains(&x_id), "{ids:?}");

    // X — superseded, ревизии целы, ребро supersedes Y → X
    let (code, xe) = srv.call("GET", &format!("/api/v1/entries/{x_id}"), &agent, None);
    assert_eq!(code, 200, "{xe}");
    assert_eq!(xe["status"], "superseded", "{xe}");
    assert_eq!(xe["superseded_by_id"], y_id.as_str(), "{xe}");
    assert_eq!(xe["body"], x["body"], "{xe}");
    assert_eq!(
        xe["triggers"][0]["pattern"], "flaky cache",
        "триггеры сохранены"
    );
    let (_, revs) = srv.call(
        "GET",
        &format!("/api/v1/entries/{x_id}/revisions"),
        &agent,
        None,
    );
    let st: Vec<&Value> = revs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["status"])
        .collect();
    assert_eq!(st, [&json!("active"), &json!("superseded")], "{revs}");
    let (code, links) = srv.call(
        "GET",
        &format!("/api/v1/entries/{y_id}/links"),
        &agent,
        None,
    );
    assert_eq!(code, 200, "{links}");
    assert!(
        links["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "supersedes"
                && e["src"] == format!("e:{y_id}")
                && e["dst"] == format!("e:{x_id}")),
        "{links}"
    );
    // триггеры X не выдаются, Y — выдаются
    let (code, t) = srv.call(
        "GET",
        &format!("/api/v1/triggers?solution={G_SUP}"),
        &agent,
        None,
    );
    assert_eq!(code, 200, "{t}");
    let pats: Vec<&str> = t["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["pattern"].as_str())
        .collect();
    assert_eq!(pats, ["shared ports"], "{t}");

    // axiom: человек создаёт, агент не отзывает (403), человеческий токен — отзывает
    let (code, ht) = srv.admin_call(
        &web,
        "POST",
        &format!(
            "/api/v1/admin/users/{}/tokens",
            s(&srv.admin_call(&web, "GET", "/api/v1/auth/me", None, None).1["user_id"])
        ),
        Some(&json!({"name": "человек", "human": true})),
        None,
    );
    assert_eq!(code, 201, "{ht}");
    assert_eq!(ht["info"]["human"], true, "{ht}");
    let human = s(&ht["plaintext"]);
    let ax = created(srv.call(
        "POST",
        &entries,
        &human,
        Some(
            &json!({"kind": "insight", "title": "Release only from main",
                     "body": "Owner rule.", "data": {"kind": "decision"}}),
        ),
    ));
    assert_eq!(ax["certainty"], "axiom", "{ax}");
    assert_eq!(ax["author"]["kind"], "human", "{ax}");
    let ax_id = s(&ax["id"]);
    let retract = format!("/api/v1/entries/{ax_id}/retract");
    axiom_denied(srv.call(
        "POST",
        &retract,
        &agent,
        Some(&json!({"reason": "агент считает устаревшим"})),
    ));
    let (_, cur) = srv.call("GET", &format!("/api/v1/entries/{ax_id}"), &agent, None);
    assert_eq!(cur["status"], "active", "{cur}");
    let (code, j) = srv.call(
        "POST",
        &retract,
        &human,
        Some(&json!({"reason": "правило отменено владельцем"})),
    );
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["status"], "retracted", "{j}");
    assert_eq!(j["retract_reason"], "правило отменено владельцем", "{j}");
    srv.kill();
}

// ---- 3. Секрет через агента ----

/// Чтение записи Vault с раскрытием и заголовками атрибуции.
fn reveal(srv: &ServerProc, token: &str, id: &str) -> (u16, Value) {
    srv.call_h(
        "GET",
        &format!("/api/v1/vault/{id}?reveal=1"),
        token,
        None,
        &[("X-Zam-Session", SESSION), ("X-Zam-Machine", MACHINE)],
    )
}

/// Агент пишет и раскрывает секрет; аудит без значения; значения нет на диске и в
/// журнале; тот же мастер-ключ после перезапуска раскрывает, чужой — 503 `vault-locked`.
#[test]
fn secret_through_agent_audited_and_bound_to_master_key() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("m3acc-sec");
    let (conf, data, addr) = config("sec", |c| with_ml(c, &q));
    let mut srv = ServerProc::start(&conf, addr);
    let agent = srv.initial_token(READY);
    listening(&mut srv, &agent);
    let reg = json!({"agent_solution_id": G_SEC, "name": "Секреты"});
    created(srv.call("POST", "/api/v1/solutions", &agent, Some(&reg)));
    let value = marker("secret");
    let item = created(srv.call_h(
        "POST",
        &format!("/api/v1/spaces/{G_SEC}/vault"),
        &agent,
        Some(&json!({"title": "deploy-key", "secret": value,
                     "data": {"type": "token", "url": "https://git.example.com"}})),
        &[("X-Zam-Session", SESSION), ("X-Zam-Machine", MACHINE)],
    ));
    assert!(!item.to_string().contains(&value), "{item}");
    let id = s(&item["id"]);
    let (code, j) = reveal(&srv, &agent, &id);
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["secret"], value.as_str());
    // аудит раскрытия: сессия и машина, значения нет
    let web = srv.admin_login();
    let (code, audit) = srv.admin_call(&web, "GET", "/api/v1/admin/audit?limit=500", None, None);
    assert_eq!(code, 200, "{audit}");
    let row = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["action"] == "vault.reveal")
        .unwrap_or_else(|| panic!("нет vault.reveal: {audit}"))
        .clone();
    assert_eq!(row["target"], id.as_str(), "{row}");
    assert_eq!(row["details"]["session"], SESSION, "{row}");
    assert_eq!(row["details"]["machine"], MACHINE, "{row}");
    assert!(!audit.to_string().contains(&value), "значение в аудите");
    idle(&mut srv, &web);

    // байты каталога данных (базы с WAL, журнал) и хранилища Qdrant — без значения
    srv.kill();
    has_server_log(&data);
    let hits = files_containing(&data, value.as_bytes());
    assert!(hits.is_empty(), "значение в каталоге данных: {hits:?}");
    let hits = files_containing(&q.dir, value.as_bytes());
    assert!(hits.is_empty(), "значение в хранилище Qdrant: {hits:?}");
    assert!(!srv.tail().contains(&value), "значение в stderr");

    // тот же мастер-ключ: перезапуск раскрывает
    let mut srv = ServerProc::start(&conf, addr);
    listening(&mut srv, &agent);
    let (code, j) = reveal(&srv, &agent, &id);
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["secret"], value.as_str());
    srv.kill();

    // чужой мастер-ключ: Vault заблокирован, остальной сервер работает, ключ не перезаписан
    let key_file = data.join("master.key");
    let original = std::fs::read(&key_file).expect("master.key создан при первом старте");
    let foreign = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=\n";
    std::fs::write(&key_file, foreign).unwrap();
    let mut srv = ServerProc::start(&conf, addr);
    listening(&mut srv, &agent);
    let (code, j) = reveal(&srv, &agent, &id);
    assert_eq!(code, 503, "{j}");
    assert_eq!(j["type"], "/problems/vault-locked", "{j}");
    assert!(!j.to_string().contains(&value), "{j}");
    let (code, j) = srv.call("GET", "/api/v1/spaces", &agent, None);
    assert_eq!(code, 200, "{j}");
    srv.kill();
    assert_eq!(std::fs::read(&key_file).unwrap(), foreign.as_bytes());
    std::fs::write(&key_file, original).unwrap();
}
