//! Жизненный цикл управляемого Qdrant (§18.1 ТЗ, задача 12 M2): MiniLM на CPU-сборке
//! ORT и Qdrant 1.19.2 из `target/deps`.
//!
//! Здесь — то, что требует настоящего процесса Qdrant под супервизором сервера:
//! - процесс Qdrant убит посреди индексации — воркер получает `Unavailable`, задания не
//!   теряются, супервизор перезапускает Qdrant, очередь догоняет, поиск снова векторный;
//! - `zam-server` (процесс) убит — Qdrant завершается (Job Object / PDEATHSIG); проверка
//!   по pid из `<data_dir>/qdrant/qdrant.pid`; каталог данных длиной в предел Windows
//!   (190 символов, пути хранилища Qdrant глубже MAX_PATH), перезапуск сервера;
//! - Windows: каталог данных длиннее предела — понятная ошибка при старте.
//!
//! Остальные сценарии §18.1 проверяются рядом и здесь не дублируются:
//! - Qdrant остановлен, запись и FTS работают, очередь догоняет —
//!   `index_worker::qdrant_down_queues_jobs_fts_works_and_queue_catches_up`;
//! - метка хранилища старой версии → пересборка из `embeddings` без инференса —
//!   `index_worker::managed_storage_rebuild_reindexes_from_embeddings_without_inference`;
//! - коллекция удалена → reindex из `embeddings` —
//!   `index_api::status_ready_and_admin_reindex_restores_points_without_inference`;
//! - Human и `index: false` не попадают в Qdrant —
//!   `index_worker::human_and_index_false_never_reach_qdrant`;
//! - сирота из pid-файла, падения подряд, родитель-проба убит — `zam-index/tests/supervisor.rs`.
//!
//! Без ORT или MiniLM тесты пропускаются; в CI (`ZAM_REQUIRE_ML=1`) — падают.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::Api;
use common::ml::{MINILM, READY, have, ml, until, vector_ready};
use common::process::ServerProc;
use common::qdrant::{free_port, qdrant_bin};
use serde_json::{Value, json};
use zam_index::VectorFilter;
use zam_index::supervisor::{is_qdrant_process, pid_alive};
use zam_server::config::{QdrantMode, ServerConfig};
use zam_store::{chunks_for_index, get_node, job_stats};

/// Предел исчезновения Qdrant после смерти родителя или `kill` (§4.3: ≤ 5 с).
const GONE: Duration = Duration::from_secs(5);

/// Заметка из двух разделов (порядок событий задают точки остановки, а не размер пачки:
/// маленькие пачки — «быстрый» режим, в котором тайминг подвёл бы).
fn body(i: usize) -> String {
    let mut s = format!("# Заметка {i}\n\n");
    for k in 0..2 {
        s.push_str(&format!(
            "## Раздел {k}\n\nПроект номер {i} собирается командой cargo build, тесты идут через \
             cargo test, раздел {k}. Конфигурация лежит в каталоге deploy, журнал пишется в logs. \
             Переменные окружения задаёт скрипт dev-env; временные файлы — только внутри .tmp.\n\n"
        ));
    }
    s
}

/// Управляемый Qdrant из `target/deps` на свободных портах; возвращает HTTP-порт.
fn managed(c: &mut ServerConfig) -> u16 {
    c.qdrant.mode = QdrantMode::Managed;
    c.qdrant.binary = Some(qdrant_bin());
    c.qdrant.http_port = free_port();
    c.qdrant.grpc_port = free_port();
    c.qdrant.http_port
}

/// Точки коллекции с `space_kind = kind` — точный подсчёт REST API Qdrant по payload,
/// мимо адаптера и фильтров `VectorFilter`.
fn count_space_kind(http_port: u16, key: &str, kind: &str) -> u64 {
    let (code, v) = common::process::http(
        SocketAddr::from((Ipv4Addr::LOCALHOST, http_port)),
        "POST",
        &format!("/collections/{}/points/count", zam_index::COLLECTION),
        Some(key),
        Some(&json!({
            "exact": true,
            "filter": {"must": [{"key": "space_kind", "match": {"value": kind}}]},
        })),
    )
    .expect("count Qdrant");
    assert_eq!(code, 200, "{v}");
    v["result"]["count"].as_u64().expect("count")
}

/// pid из `<data_dir>/qdrant/qdrant.pid`.
fn pid_file(data_dir: &Path) -> Option<u32> {
    std::fs::read_to_string(data_dir.join("qdrant").join("qdrant.pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Жёстко завершает чужой процесс (как сбой или `kill -9` администратора).
fn kill_pid(pid: u32) {
    let st = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output()
            .map(|o| o.status)
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    assert!(
        st.is_ok_and(|s| s.success()),
        "не удалось завершить pid {pid}"
    );
}

fn wait_gone(pid: u32, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !pid_alive(pid)
}

/// При панике теста штатно останавливает воркер и супервизор, а с ним Qdrant: иначе
/// процесс Qdrant (и попытки супервизора его перезапустить) жили бы до конца тестового
/// бинарника. Остановка идёт из отдельного потока через `Handle::block_on`: из `Drop`
/// внутри runtime блокироваться нельзя, а задачи воркера выполняют потоки runtime теста
/// (`multi_thread`).
struct ShutdownOnPanic(zam_server::AppState);

impl Drop for ShutdownOnPanic {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        // воркер и супервизор могли остаться на тестовых точках остановки
        self.0.index.worker.release_write();
        if let Some(sup) = self.0.index.supervisor() {
            sup.release_restart();
        }
        let st = self.0.clone();
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let _ = std::thread::spawn(move || {
            rt.block_on(async {
                let _ = tokio::time::timeout(Duration::from_secs(30), st.shutdown()).await;
            });
        })
        .join();
    }
}

async fn state(a: &Api, node: &str) -> String {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .index_state
}

/// Точки заметок (без папок) под префиксом пути.
async fn points(a: &Api, prefix: &str) -> u64 {
    let f = VectorFilter {
        user_id: a.st.admin_id,
        space_ids: vec![],
        path_prefix: Some(prefix.into()),
        levels: ["chunk", "section", "note"].map(String::from).to_vec(),
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

async fn status(a: &Api) -> Value {
    let r = a.t(Method::GET, "/api/v1/index/status", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Процесс Qdrant убит посреди индексации. Порядок событий задаётся тестовыми точками
/// остановки (фича `test-hooks` zam-index), а не таймингом:
/// 1. воркер стоит перед записью пачки (`hold_next_write`): эмбеддинги посчитаны,
///    задания захвачены;
/// 2. Qdrant убит, супервизор стоит перед перезапуском (`hold_next_restart`, состояние
///    `Restarting`) — Qdrant гарантированно лежит, пока идёт запись;
/// 3. воркер отпущен: запись пачки получает `Unavailable`, задания пачки возвращаются
///    в очередь (`jobs_requeued` — именно этот путь, а не ошибки свёрток L0, `ensure` или
///    reindex), канал выключен;
/// 4. перезапуск отпущен: новый процесс Qdrant (`restarts` = 1), очередь догоняет без
///    неудавшихся заданий, точки всех заметок на месте, поиск — без деградации.
///
/// Без точки на перезапуске результат зависел от ОС: на Windows отказ соединения с
/// закрытым портом localhost приходит лишь через ~2 с, клиент Qdrant переподключается,
/// и к этому моменту супервизор успевал поднять Qdrant — запись просто проходила позже.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qdrant_killed_mid_indexing_restarts_and_queue_catches_up() {
    if !have(MINILM) {
        return;
    }
    let dir = common::tmp("lc-kill");
    let mut c = common::cfg_in(dir.clone());
    ml(&mut c, MINILM);
    let http_port = managed(&mut c);
    let a = common::api::api_with(c).await;
    let _guard = ShutdownOnPanic(a.st.clone());
    vector_ready(&a).await;
    let sup = a.st.index.supervisor().unwrap();
    let w = &a.st.index.worker;

    w.hold_next_write();
    sup.hold_next_restart();
    const NOTES: usize = 12;
    let mut notes = Vec::new();
    for n in 0..NOTES {
        let v = a.note("knowledge", &format!("kill/n{n}"), &body(n)).await;
        notes.push(v["id"].as_str().unwrap().to_string());
        if n == NOTES / 2 {
            // Human в той же очереди: и после перезапуска не попадает в Qdrant
            let r = a
                .adm(
                    Method::POST,
                    "/api/v1/nodes",
                    Some(json!({"space": "human", "path": format!("diary{n}"),
                                "kind": "note", "content": body(n)})),
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        }
    }
    // 1. пачка у записи
    assert!(
        w.wait_write_held(READY).await,
        "воркер не дошёл до записи пачки: {:?}",
        w.status()
    );
    assert!(w.status().batch_running);
    let requeued0 = w.jobs_requeued();
    // 2. Qdrant убит и не поднимается
    let qpid = pid_file(&dir).expect("pid-файл Qdrant");
    assert_eq!(Some(qpid), sup.pid());
    kill_pid(qpid);
    assert!(wait_gone(qpid, GONE), "Qdrant {qpid} не завершился");
    assert!(
        sup.wait_restart_held(GONE).await,
        "супервизор не заметил падения"
    );
    until("канал выключен", GONE, async || {
        a.st.index.vector_available().is_err()
    })
    .await;
    // 3. запись пачки отказывает (на Windows отказ соединения — секунды, клиент
    // переподключается): задания возвращены в очередь, не потрачены и не failed
    let t = Instant::now();
    w.release_write();
    until(
        "задания пачки вернулись в очередь",
        Duration::from_secs(30),
        async || w.jobs_requeued() > requeued0,
    )
    .await;
    let requeued = w.jobs_requeued() - requeued0;
    eprintln!(
        "запись отказала через {:?}, заданий возвращено в очередь: {requeued}",
        t.elapsed()
    );
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    assert_eq!(job_stats(&db).await.unwrap().failed, 0);
    // 4. перезапуск: новый процесс в pid-файле, канал включён
    sup.release_restart();
    until("Qdrant вернулся", READY, async || {
        sup.pid().is_some_and(|p| p != qpid) && a.st.index.vector_available().is_ok()
    })
    .await;
    assert_eq!(pid_file(&dir), sup.pid());

    // очередь догнала: все заметки проиндексированы, неудавшихся заданий нет
    until("очередь догнала", READY, async || {
        let st = job_stats(&db).await.unwrap();
        st.queued + st.running == 0 && !a.st.index.worker.status().batch_running
    })
    .await;
    assert_eq!(job_stats(&db).await.unwrap().failed, 0);
    let mut want = 0;
    for n in &notes {
        assert_eq!(state(&a, n).await, "indexed", "заметка {n}");
        want += chunks_for_index(&db, n.parse().unwrap())
            .await
            .unwrap()
            .len() as u64;
    }
    assert_eq!(points(&a, "kill").await, want);
    // Human нет в Qdrant: подсчёт по payload `space_kind` (знания — есть)
    let key = sup.api_key().expect("ключ Qdrant");
    assert_eq!(count_space_kind(http_port, &key, "human"), 0);
    assert!(count_space_kind(http_port, &key, "knowledge") >= want);
    let s = status(&a).await;
    assert_eq!(s["qdrant"]["state"], "ready", "{s}");
    assert_eq!(s["qdrant"]["restarts"], 1, "{s}");
    assert_eq!(s["queue"]["failed"], 0, "{s}");
    assert_eq!(s["vector"]["available"], true, "{s}");
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "собирается командой cargo build"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["degraded"], false, "{j}");
    assert!(
        j["items"][0]["path"]
            .as_str()
            .is_some_and(|p| p.starts_with("kill/")),
        "{j}"
    );
    a.st.shutdown().await;
}

/// Поиск сразу после убийства Qdrant (задача 15 M2): ответ из FTS с `degraded: true` и
/// `qdrant_down` за доли секунды, а не после отказа соединения. На Windows отказ соединения
/// с закрытым портом localhost приходит через ~2 с на попытку, клиент Qdrant повторяет
/// запрос, а супервизор меняет состояние только после разбора журнала процесса: до
/// исправления первый поиск после падения ждал 2–4 с. Теперь доступность при `Ready`
/// сверяется с живостью процесса (`Supervisor::process_alive`), а соединение ограничено
/// 300 мс. Перезапуск держит тестовая точка (`hold_next_restart`): Qdrant гарантированно
/// лежит всё время проверки.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_right_after_qdrant_kill_degrades_without_waiting() {
    if !have(MINILM) {
        return;
    }
    let dir = common::tmp("lc-kill-search");
    let mut c = common::cfg_in(dir.clone());
    ml(&mut c, MINILM);
    managed(&mut c);
    let a = common::api::api_with(c).await;
    let _guard = ShutdownOnPanic(a.st.clone());
    vector_ready(&a).await;
    let v = a.note("knowledge", "kill-search/n0", &body(0)).await;
    let id = v["id"].as_str().unwrap().to_string();
    until(
        "заметка проиндексирована",
        READY,
        async || state(&a, &id).await == "indexed",
    )
    .await;
    let search = async || {
        let t = Instant::now();
        let r = a
            .t(
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": "собирается командой cargo build", "mode": "chunks"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        (t.elapsed(), r.json())
    };
    let (_, j) = search().await;
    assert_eq!(j["degraded"], false, "{j}");

    let sup = a.st.index.supervisor().unwrap();
    sup.hold_next_restart();
    let qpid = sup.pid().expect("pid Qdrant");
    kill_pid(qpid);
    // без ожидания супервизора: первый же поиск после смерти процесса
    let (dt, j) = search().await;
    eprintln!("первый поиск после убийства Qdrant: {dt:?}");
    assert_eq!(j["degraded"], true, "{j}");
    assert_eq!(j["degraded_reason"], "qdrant_down", "{j}");
    assert!(
        j["items"][0]["path"]
            .as_str()
            .is_some_and(|p| p.starts_with("kill-search/")),
        "FTS находит заметку: {j}"
    );
    assert!(dt < Duration::from_secs(1), "поиск ждал {dt:?}");
    // и пока Qdrant лежит — так же быстро
    assert!(
        sup.wait_restart_held(GONE).await,
        "супервизор не заметил падения"
    );
    let (dt, j) = search().await;
    assert_eq!(j["degraded"], true, "{j}");
    assert!(dt < Duration::from_secs(1), "поиск ждал {dt:?}");
    sup.release_restart();
    until("Qdrant вернулся", READY, async || {
        sup.pid().is_some_and(|p| p != qpid) && a.st.index.vector_available().is_ok()
    })
    .await;
    let (_, j) = search().await;
    assert_eq!(j["degraded"], false, "{j}");
    a.st.shutdown().await;
}

/// Длина пути в символах UTF-16 (так считает MAX_PATH).
fn wlen(p: &Path) -> usize {
    p.to_string_lossy().encode_utf16().count()
}

/// Каталог данных (с кириллицей) внутри `CARGO_TARGET_TMPDIR` длиной ровно в предел
/// Windows [`zam_store::MAX_DATA_DIR_CHARS_WINDOWS`] (190 символов): самый длинный
/// допустимый путь проверяется целиком.
fn long_data_dir(root: &Path) -> PathBuf {
    let max = zam_store::MAX_DATA_DIR_CHARS_WINDOWS;
    let mut d = root.join("данные-сервера");
    assert!(
        wlen(&d) + 30 < max,
        "корень теста длиннее предела: {}",
        d.display()
    );
    while wlen(&d) + 30 < max {
        d.push("уровень-вложенности");
    }
    // последний сегмент добивает путь ровно до предела
    let rest = max - wlen(&d) - 1;
    d.push("ж".repeat(rest));
    assert_eq!(wlen(&d), max);
    d
}

/// Самый длинный путь файла под `dir` (символов UTF-16 — так считает MAX_PATH).
fn longest_path(dir: &Path) -> usize {
    let mut best = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            best = best.max(wlen(&p));
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            }
        }
    }
    best
}

/// Ждёт статуса, удовлетворяющего условию; возвращает последний. Отказ в соединении —
/// «ещё не готов» (сервер слушает порт только после открытия баз и загрузки модели);
/// умерший процесс — сразу паника с хвостом stderr.
fn wait_status(srv: &mut ServerProc, token: &str, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let t = Instant::now();
    let mut last: String;
    loop {
        srv.assert_running(what);
        match srv.try_call("GET", "/api/v1/index/status", token, None) {
            Ok((200, s)) if f(&s) => return s,
            Ok((code, s)) => last = format!("{code} {s}"),
            Err(e) => last = format!("нет ответа: {e}"),
        }
        assert!(
            t.elapsed() < READY,
            "не дождались за {READY:?}: {what}; {last}\nstderr:\n{}",
            srv.tail()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Индексация пришла в покой: очередь пуста, точки есть.
fn settled(s: &Value) -> bool {
    s["vector"]["available"] == true
        && s["queue"]["queued"] == 0
        && s["queue"]["running"] == 0
        && s["qdrant"]["points"].as_u64().is_some_and(|p| p > 0)
}

fn search_vector(srv: &ServerProc, token: &str) {
    let (code, j) = srv.call(
        "POST",
        "/api/v1/search",
        token,
        Some(&json!({"query": "длинный путь каталога данных"})),
    );
    assert_eq!(code, 200, "{j}");
    assert_eq!(j["degraded"], false, "{j}");
    assert_eq!(j["items"][0]["path"], "long/n", "{j}");
}

/// `zam-server` — настоящий процесс с каталогом данных длиннее 150 символов. Индексация
/// и векторный поиск работают (на Windows файлы хранилища Qdrant глубже MAX_PATH).
/// Сервер убит жёстко — Qdrant из pid-файла завершается за ≤ 5 с (Job Object на Windows,
/// PDEATHSIG на Linux). Повторный старт: новый Qdrant, точки на месте, поиск векторный.
#[test]
fn killed_server_takes_qdrant_down_and_restarts_with_long_data_dir() {
    if !have(MINILM) {
        return;
    }
    let root = common::tmp("lc-proc");
    let data = long_data_dir(&root);
    std::fs::create_dir_all(&data).unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
    let mut c = common::cfg_in(data.clone());
    ml(&mut c, MINILM);
    let _ = managed(&mut c);
    c.server.bind = addr;
    let conf = root.join("zam.toml");
    std::fs::write(&conf, toml::to_string(&c).unwrap()).unwrap();

    let mut srv = ServerProc::start(&conf, addr);
    let token = srv.initial_token(READY);
    wait_status(&mut srv, &token, "векторный канал", |s| {
        s["vector"]["available"] == true
    });
    let (code, n) = srv.call(
        "POST",
        "/api/v1/nodes",
        &token,
        Some(
            &json!({"space": "knowledge", "path": "long/n", "kind": "note",
                     "content": "# Путь\n\nДлинный путь каталога данных не мешает индексации.\n"}),
        ),
    );
    assert_eq!(code, 201, "{n}");
    let s = wait_status(&mut srv, &token, "индексация", settled);
    let pts = s["qdrant"]["points"].as_u64().unwrap();
    search_vector(&srv, &token);
    let deepest = longest_path(&data.join("qdrant").join("storage"));
    eprintln!(
        "каталог данных {} символов, самый длинный путь хранилища {deepest}",
        wlen(&data)
    );
    if cfg!(windows) {
        assert!(
            deepest > 260,
            "хранилище не вышло за MAX_PATH ({deepest}): тест не проверяет длинные пути"
        );
    }

    // сервер убит: Qdrant уходит вместе с ним
    let q1 = pid_file(&data).expect("pid-файл Qdrant");
    assert!(pid_alive(q1) && is_qdrant_process(q1));
    srv.kill();
    assert!(wait_gone(q1, GONE), "Qdrant {q1} пережил zam-server");

    // повторный старт в том же каталоге: первый токен не выдаётся, прежний действует
    let mut srv = ServerProc::start(&conf, addr);
    let s = wait_status(&mut srv, &token, "после перезапуска", |s| {
        settled(s) && s["qdrant"]["state"] == "ready"
    });
    assert_eq!(s["qdrant"]["points"], pts, "{s}");
    assert_eq!(s["qdrant"]["restarts"], 0, "{s}");
    let q2 = pid_file(&data).expect("pid-файл Qdrant");
    assert_ne!(q1, q2);
    assert!(pid_alive(q2) && is_qdrant_process(q2));
    search_vector(&srv, &token);

    // штатная остановка: Linux — SIGTERM, Qdrant останавливается, pid-файл удалён;
    // Windows — Ctrl+C фоновому процессу не послать, жёсткое завершение (Job Object)
    #[cfg(unix)]
    {
        srv.terminate(Duration::from_secs(30));
        assert!(wait_gone(q2, GONE), "Qdrant {q2} пережил остановку сервера");
        assert_eq!(pid_file(&data), None, "pid-файл не удалён");
    }
    #[cfg(windows)]
    {
        srv.kill();
        assert!(wait_gone(q2, GONE), "Qdrant {q2} пережил zam-server");
    }
    assert!(!srv.running());
}

/// Windows: каталог данных на символ длиннее предела — сервер и команды CLI (миграции,
/// загрузки моделей и CUDA, управление Qdrant) не стартуют и называют
/// причину и способ её устранить (без проверки SQLite отказал бы позже с «unable to open
/// database file»). Модели и Qdrant не нужны.
#[cfg(windows)]
#[test]
fn too_long_data_dir_fails_with_hint() {
    let root = common::tmp("lc-too-long");
    let mut data = long_data_dir(&root);
    data.as_mut_os_string().push("ж");
    let missing = root.join("нет.toml");
    let missing = missing.to_str().unwrap();
    // каждая команда с каталогом данных проверяет длину до работы; у загрузок манифест
    // заведомо отсутствует — без проверки длины ошибка была бы о манифесте, а не сеть
    let cmds: [&[&str]; 5] = [
        &["migrate"],
        &["models", "fetch", "--manifest", missing],
        &["cuda", "fetch", "--manifest", missing],
        &["qdrant", "status"],
        &["qdrant", "reset", "--yes"],
    ];
    for args in cmds {
        let out = common::process::command()
            .args(args)
            .arg("--data-dir")
            .arg(&data)
            .output()
            .expect("запуск zam-server");
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("слишком длинный")
                && err.contains("MAX_PATH")
                && err.contains("--data-dir"),
            "{args:?}: {err}"
        );
    }
    assert!(!data.exists(), "каталог данных создан");
}
