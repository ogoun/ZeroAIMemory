//! `zam log insight` и `zam insight …` против настоящего zam-server в том же процессе тестов
//! (§7.5, §13.2): триггеры, ответ 409 о дубле с подсказками, `--force`, замещение, отзыв,
//! пересмотр, дописывание, `--scope global`. Сервер без моделей (дубли — по заголовку).
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e31";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-insights", name)
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна».
fn start(name: &str) -> (String, String) {
    let data = tmp(&format!("{name}-data"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let cfg = ServerConfig {
                data_dir: data,
                ..ServerConfig::default()
            };
            let (app, st, _) = build(cfg, Some("pw".into())).await.unwrap();
            let u = st.system.create_user("Анна", false).await.unwrap();
            let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, t)).unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
    });
    rx.recv().unwrap()
}

/// Запрос к поддельному серверу: метод, путь, `If-Match`.
type Seen = (String, String, Option<String>);

/// Поддельный HTTP-сервер для проверки заголовков правки: `GET` отдаёт запись с `rev: 3`,
/// `PATCH` — `patch_status` с телом `patch_body`. Возвращает адрес и журнал запросов.
fn fake_entry_server(
    patch_status: u16,
    patch_body: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (m, path) = (
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
            );
            let (mut len, mut if_match) = (0usize, None);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    match k.trim().to_ascii_lowercase().as_str() {
                        "content-length" => len = v.trim().parse().unwrap_or(0),
                        "if-match" => if_match = Some(v.trim().to_string()),
                        _ => {}
                    }
                }
            }
            let mut body = vec![0u8; len];
            let _ = r.read_exact(&mut body);
            log.lock().unwrap().push((m.clone(), path, if_match));
            let (status, text) = if m == "GET" {
                (
                    200,
                    r#"{"id":"01E","rev":3,"certainty":"hypothesis","data":{"kind":"gotcha"}}"#,
                )
            } else {
                (patch_status, patch_body)
            };
            let ctype = if status < 300 {
                "application/json"
            } else {
                "application/problem+json"
            };
            let _ = write!(
                c,
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (url, seen)
}

fn zam(url: &str, token: &str, cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
        .write_stdin(stdin.to_vec())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        out(o),
        err(o)
    );
    out(o)
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_str(&ok(o)).unwrap_or_else(|e| panic!("не JSON ({e}): {}", out(o)))
}

/// Id из короткого вывода `<id>@<rev>…`.
fn id_of(s: &str) -> String {
    s.trim().split('@').next().unwrap().to_string()
}

/// `zam insight certify` обычным токеном: theorem — с доказательством (без него — 422, код 6),
/// axiom (по указанию пользователя) — с причиной; правка идёт с ревизией из `GET` (§11.3).
#[test]
fn insight_certify_evidence_and_axiom() {
    let (url, tok) = start("certify");
    let r = tmp("certify-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let o = zam(
        &url,
        &tok,
        &r,
        &[
            "log",
            "insight",
            "--kind",
            "experiment",
            "--title",
            "Метод нерабочий",
            "--scope",
            "global",
        ],
        "Не сработал на наших данных.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    assert_eq!(
        json(&z(&["insight", "show", &id, "--json"]))["certainty"],
        "hypothesis"
    );
    // theorem без доказательства — отказ сервера (422)
    let o = z(&[
        "insight",
        "certify",
        &id,
        "theorem",
        "--reason",
        "перепроверил",
    ]);
    assert_eq!(o.status.code(), Some(6), "{}", out(&o));
    assert!(err(&o).contains("evidence"), "{}", err(&o));
    let ev = r.join("ev.json");
    std::fs::write(
        &ev,
        r#"{"checked": "3 набора данных", "method": "повторный прогон", "result": "не работает"}"#,
    )
    .unwrap();
    let o = z(&[
        "insight",
        "certify",
        &id,
        "theorem",
        "--reason",
        "подтверждено",
        "--evidence-file",
        ev.to_str().unwrap(),
    ]);
    assert!(ok(&o).trim().ends_with("theorem"), "{}", out(&o));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["certainty"], "theorem");
    assert_eq!(e["data"]["kind"], "experiment", "data сохранена: {e}");
    assert_eq!(e["data"]["evidence"]["method"], "повторный прогон");
    // axiom обычным токеном — по указанию пользователя, с причиной
    let o = z(&[
        "insight",
        "certify",
        &id,
        "axiom",
        "--reason",
        "пользователь утвердил",
    ]);
    assert!(ok(&o).trim().ends_with("axiom"), "{}", out(&o));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(
        (&e["certainty"], &e["rev"]),
        (&serde_json::json!("axiom"), &serde_json::json!(3))
    );
    // append и revisit — тоже с ревизией
    let o = zam(
        &url,
        &tok,
        &r,
        &["insight", "append", &id],
        "ещё\n".as_bytes(),
    );
    ok(&o);
    ok(&z(&["insight", "revisit", &id, "--reason", "новые данные"]));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(
        (&e["status"], &e["rev"]),
        (&serde_json::json!("revisit"), &serde_json::json!(5))
    );
}

/// Стенд Debian 13: `log insight --certainty axiom` — только с `--reason` (без неё код 6 до
/// запроса); `zam read` принимает ссылки выдачи `i:<id>` и `n:<id>`, с пачкой и без (B2).
#[test]
fn log_axiom_with_reason_and_read_refs() {
    let (url, tok) = start("axiom-refs");
    let r = tmp("axiom-refs-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let base = [
        "log",
        "insight",
        "--kind",
        "decision",
        "--title",
        "Сборка только под musl",
        "--scope",
        "global",
        "--certainty",
        "axiom",
    ];
    // без --reason — отказ CLI, сервер не вызывается: недоступный адрес не мешает
    let o = zam(
        "http://127.0.0.1:9",
        &tok,
        &r,
        &base,
        "Пользователь: собираем zam только под musl.\n".as_bytes(),
    );
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    assert!(err(&o).contains("--reason"), "{}", err(&o));
    let mut a = base.to_vec();
    a.extend(["--reason", "пользователь продиктовал"]);
    let o = z(
        &a,
        "Пользователь: собираем zam только под musl.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    assert_eq!(
        json(&z(&["insight", "show", &id, "--json"], b""))["certainty"],
        "axiom"
    );

    // ссылка записи из выдачи поиска: текст тела, с пачкой — карточка с текстом
    let iref = format!("i:{id}");
    let s = ok(&z(&["read", &iref], b""));
    assert_eq!(s, "Пользователь: собираем zam только под musl.\n");
    let s = ok(&z(&["read", &iref, "--tier", "L0"], b""));
    assert_eq!(s, "Сборка только под musl\n");
    let s = ok(&z(&["read", &iref, "--batch"], b""));
    assert!(s.contains("только под musl.") && s.contains(&iref), "{s}");
    let v = json(&z(&["read", &iref, "--json"], b""));
    assert_eq!(v["certainty"], "axiom", "{v}");
    let o = z(&["read", &iref, "--rev", "1"], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    let missing = format!("i:{}", zam_core::Id::new());
    let o = z(&["read", &missing], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));

    // ссылка узла n:<id>
    let o = z(
        &["write", "knowledge:Сборка"],
        "# Сборка\nmusl\n".as_bytes(),
    );
    let node = id_of(&ok(&o));
    let nref = format!("n:{node}");
    assert_eq!(ok(&z(&["read", &nref], b"")), "# Сборка\nmusl\n");
    let s = ok(&z(&["read", &nref, &iref, "--batch"], b""));
    assert!(
        s.contains("musl\n") && s.contains("только под musl."),
        "{s}"
    );
}

/// `certify` берёт `rev` из `GET /entries/{id}` и шлёт `If-Match` с ним.
#[test]
fn insight_certify_sends_if_match() {
    let (url, seen) = fake_entry_server(
        200,
        r#"{"id":"01E","rev":4,"certainty":"axiom","status":"active"}"#,
    );
    let r = tmp("certify-if-match");
    let o = zam(
        &url,
        "t",
        &r,
        &["insight", "certify", "01E", "axiom", "--reason", "указание"],
        b"",
    );
    assert!(ok(&o).trim().ends_with("axiom"), "{}", out(&o));
    let seen = seen.lock().unwrap().clone();
    let patch: Vec<&Seen> = seen.iter().filter(|s| s.0 == "PATCH").collect();
    assert_eq!(patch.len(), 1, "{seen:?}");
    assert_eq!(patch[0].1, "/api/v1/entries/01E");
    assert_eq!(patch[0].2.as_deref(), Some("\"3\""), "{seen:?}");
    assert!(
        seen.iter()
            .any(|s| s.0 == "GET" && s.1 == "/api/v1/entries/01E"),
        "{seen:?}"
    );
}

/// Запись изменилась между чтением и правкой (409 с `rev` текущей) — сообщение и код 4
/// (как у заметок, решение контроллера R7).
#[test]
fn insight_certify_conflict_exit_4() {
    let (url, _) = fake_entry_server(
        409,
        r#"{"type":"/problems/conflict","title":"Conflict","status":409,"detail":"ревизия записи 5, а не 3","rev":5}"#,
    );
    let r = tmp("certify-conflict");
    let o = zam(
        &url,
        "t",
        &r,
        &["insight", "certify", "01E", "hypothesis", "--reason", "r"],
        b"",
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(
        err(&o).contains("запись изменилась (rev 3 → 5): перечитайте"),
        "{}",
        err(&o)
    );
}

/// Флаг `--human` упразднён (§11.4): clap отвергает неизвестный аргумент до запроса (код 6).
#[test]
fn human_flag_removed() {
    let r = tmp("human-flag");
    let o = zam(
        "http://127.0.0.1:9",
        "t",
        &r,
        &[
            "insight", "certify", "01E", "axiom", "--reason", "r", "--human",
        ],
        b"",
    );
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    assert!(
        err(&o).contains("unexpected argument '--human'"),
        "{}",
        err(&o)
    );
}

#[test]
fn insight_log_with_triggers_dedup_and_supersede() {
    let (url, tok) = start("main");
    let r = tmp("repo");
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Инсайты\"\n"),
    )
    .unwrap();
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let title = "Bash-скрипты должны иметь LF";
    let log = |extra: &[&str], stdin: &[u8]| {
        let mut a = vec!["log", "insight", "--kind", "gotcha", "--title", title];
        a.extend_from_slice(extra);
        z(&a, stdin)
    };

    // запись с триггерами и тегом; без модели — пометка о проверке по заголовку
    let o = log(
        &[
            "--trigger",
            "file_glob:**/*.sh",
            "--trigger",
            "error_regex:bad interpreter",
            "--tag",
            "bash",
        ],
        "CRLF ломает shebang.\n".as_bytes(),
    );
    let first = id_of(&ok(&o));
    assert!(ok(&o).trim().ends_with("@1"), "{}", out(&o));
    assert!(err(&o).contains("только по заголовку"), "{}", err(&o));
    let e = json(&z(&["insight", "show", &first, "--json"], b""));
    assert_eq!(e["body"], "CRLF ломает shebang.\n");
    assert_eq!(e["tags"], serde_json::json!(["bash"]));
    assert_eq!(e["triggers"][0]["type"], "file_glob");
    assert_eq!(e["triggers"][0]["pattern"], "**/*.sh");
    assert_eq!(e["triggers"][1]["pattern"], "bad interpreter");
    let card = ok(&z(&["insight", "show", &first], b""));
    assert!(card.contains("[active] gotcha") && card.contains("триггер: file_glob:**/*.sh"));

    // дубль: код 4, кандидаты и подсказки
    let o = log(&[], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let e2 = err(&o);
    assert!(e2.contains(&first) && e2.contains("— title"), "{e2}");
    assert!(
        e2.contains("--supersedes") && e2.contains("--force") && e2.contains("zam insight append"),
        "{e2}"
    );
    // неверный триггер — код 6 до запроса
    let o = log(&["--trigger", "glob:*.sh"], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));

    // --force записывает дубль; --supersedes замещает исходную запись
    let forced = id_of(&ok(&log(&["--force"], "Повтор.".as_bytes())));
    assert_ne!(forced, first);
    let o = log(&["--supersedes", &first], "Уточнение.".as_bytes());
    let newer = id_of(&ok(&o));
    let old = json(&z(&["insight", "show", &first, "--json"], b""));
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["superseded_by_id"], newer.as_str());

    // zam insight supersede: вид по умолчанию — у замещаемой
    let o = z(
        &[
            "insight",
            "supersede",
            &newer,
            "--title",
            "Shell-скрипты: только LF",
            "--trigger",
            "keyword:crlf",
        ],
        "Итог.".as_bytes(),
    );
    let s = ok(&o);
    assert!(s.contains(&format!("(замещает {newer})")), "{s}");
    let latest = id_of(&s);
    let l = json(&z(&["insight", "show", &latest, "--json"], b""));
    assert_eq!(l["data"]["kind"], "gotcha");
    assert_eq!(l["supersedes_id"], newer.as_str());
    assert_eq!(l["triggers"][0]["pattern"], "crlf");

    // лента по статусу
    let lst = ok(&z(&["insight", "list", "--status", "superseded"], b""));
    assert!(lst.contains(&first) && lst.contains(&newer), "{lst}");
    assert!(!lst.contains(&latest), "{lst}");

    // пересмотр, дописывание, отзыв
    let o = ok(&z(
        &["insight", "revisit", &forced, "--reason", "проверить"],
        b"",
    ));
    assert!(o.trim().ends_with("revisit"), "{o}");
    let o = ok(&z(
        &["insight", "append", &forced],
        "Дополнение.".as_bytes(),
    ));
    assert!(o.contains("@3"), "{o}");
    let f = json(&z(&["insight", "show", &forced, "--json"], b""));
    assert_eq!(f["body"], "Повтор.\n\nДополнение.");
    let o = z(&["insight", "retract", &forced, "--reason", "дубль"], b"");
    assert!(ok(&o).trim().ends_with("retracted"), "{}", out(&o));
    let o = z(&["insight", "retract", &forced, "--reason", "ещё"], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let o = z(&["insight", "append", &latest], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));

    // --scope global — в Knowledge
    let g = id_of(&ok(&z(
        &[
            "log",
            "insight",
            "--kind",
            "pattern",
            "--title",
            "Глобальный приём",
            "--scope",
            "global",
        ],
        b"",
    )));
    let lst = ok(&z(&["insight", "list", "--global"], b""));
    assert!(lst.contains(&g) && !lst.contains(&first), "{lst}");
    let lst = json(&z(&["insight", "list", "--json"], b""));
    assert!(
        !lst["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == g.as_str())
    );
}

/// `zam insight feedback` (§10.5): оценки попадают в `usage` записи и видны в `insight show`.
#[test]
fn insight_feedback_counts() {
    let (url, tok) = start("feedback");
    let r = tmp("feedback-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let o = zam(
        &url,
        &tok,
        &r,
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "LF в bash",
            "--scope",
            "global",
        ],
        "CRLF ломает shebang.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    assert_eq!(
        ok(&z(&["insight", "feedback", &id, "useful"])).trim(),
        format!("{id} useful")
    );
    ok(&z(&[
        "insight",
        "feedback",
        &id,
        "useless",
        "--session",
        "s-1",
    ]));
    ok(&z(&["insight", "feedback", &id, "useful"]));
    let j = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(
        j["usage"],
        serde_json::json!({"shown": 0, "useful": 2, "useless": 1})
    );
    let text = ok(&z(&["insight", "show", &id]));
    assert!(
        text.contains("показы: 0 · полезно: 2 · бесполезно: 1"),
        "{text}"
    );
    // неизвестная оценка — отказ разбора аргументов, неизвестная запись — ошибка сервера
    assert_ne!(
        z(&["insight", "feedback", &id, "meh"]).status.code(),
        Some(0)
    );
    let missing = zam_core::Id::new().to_string();
    assert_ne!(
        z(&["insight", "feedback", &missing, "useful"])
            .status
            .code(),
        Some(0)
    );
}
