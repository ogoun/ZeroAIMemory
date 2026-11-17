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

/// `zam insight triggers <id> [--trigger ВИД:ШАБЛОН]… [--clear]` (M6.6 §8, замечание 13): набор
/// заменяется целиком без замещения записи — `PATCH {triggers}` с `If-Match` ревизии из `GET`;
/// `--clear` снимает все. Вывод — `<id>@<rev> триггеров K` и строки `триггер: вид:шаблон`, как в
/// `insight show`. Ни `--trigger`, ни `--clear`, оба вместе, неверный триггер — код 6 до
/// запроса.
#[test]
fn insight_triggers_replace_and_clear() {
    let (url, tok) = start("triggers");
    let r = tmp("triggers-repo");
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
            "LF в shell",
            "--scope",
            "global",
            "--trigger",
            "file_glob:**/*",
        ],
        "CRLF ломает shebang.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    let o = z(&[
        "insight",
        "triggers",
        &format!("i:{id}"),
        "--trigger",
        "file_glob:**/*.sh",
        "--trigger",
        "error_regex:bad interpreter: /bin/bash",
    ]);
    // триггеры в ревизию записи не входят (M3): ревизия та же
    assert_eq!(
        ok(&o),
        format!(
            "{id}@1 триггеров 2\nтриггер: file_glob:**/*.sh\n\
             триггер: error_regex:bad interpreter: /bin/bash\n"
        )
    );
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["rev"], 1, "{e}");
    assert_eq!(e["status"], "active", "запись не замещена: {e}");
    assert_eq!(e["triggers"][0]["pattern"], "**/*.sh", "{e}");
    assert_eq!(e["triggers"][1]["type"], "error_regex", "{e}");
    assert_eq!(e["triggers"].as_array().unwrap().len(), 2, "{e}");
    let o = z(&["insight", "triggers", &id, "--clear"]);
    assert_eq!(ok(&o), format!("{id}@1 триггеров 0\n"));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["triggers"], serde_json::json!([]), "{e}");
    let j = json(&z(&[
        "insight",
        "triggers",
        &id,
        "--trigger",
        "keyword:crlf",
        "--json",
    ]));
    assert_eq!(
        (&j["rev"], &j["triggers"][0]["pattern"]),
        (&serde_json::json!(1), &serde_json::json!("crlf"))
    );
    // ошибки разбора — код 6 до запроса: недоступный адрес не мешает
    for args in [
        vec!["insight", "triggers", "01E"],
        vec![
            "insight",
            "triggers",
            "01E",
            "--clear",
            "--trigger",
            "keyword:x",
        ],
        vec!["insight", "triggers", "01E", "--trigger", "glob:*.sh"],
    ] {
        let o = zam("http://127.0.0.1:9", "t", &r, &args, b"");
        assert_eq!(o.status.code(), Some(6), "{args:?}: {}", err(&o));
    }
}

/// `zam insight triggers` берёт ревизию из `GET /entries/{id}` и шлёт `If-Match` с ней; запись
/// изменилась между чтением и правкой (409) — код 4, как у `certify`.
#[test]
fn insight_triggers_sends_if_match() {
    let (url, seen) = fake_entry_server(
        200,
        r#"{"id":"01E","rev":4,"status":"active","triggers":[]}"#,
    );
    let r = tmp("triggers-if-match");
    let o = zam(
        &url,
        "t",
        &r,
        &["insight", "triggers", "01E", "--clear"],
        b"",
    );
    assert_eq!(ok(&o), "01E@4 триггеров 0\n");
    let seen = seen.lock().unwrap().clone();
    let patch: Vec<&Seen> = seen.iter().filter(|s| s.0 == "PATCH").collect();
    assert_eq!(patch.len(), 1, "{seen:?}");
    assert_eq!(patch[0].1, "/api/v1/entries/01E");
    assert_eq!(patch[0].2.as_deref(), Some("\"3\""), "{seen:?}");
    let (url, _) = fake_entry_server(
        409,
        r#"{"type":"/problems/conflict","title":"Conflict","status":409,"detail":"ревизия записи 5, а не 3","rev":5}"#,
    );
    let o = zam(
        &url,
        "t",
        &r,
        &["insight", "triggers", "01E", "--clear"],
        b"",
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(
        err(&o).contains("запись изменилась (rev 3 → 5): перечитайте"),
        "{}",
        err(&o)
    );
}

/// REST-запрос теста к серверу без системного прокси: код и JSON ответа (`Null` — не JSON).
fn rest(url: &str, tok: &str, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    let mut r = a
        .post(format!("{url}/api/v1{path}"))
        .header("Authorization", format!("Bearer {tok}"))
        .content_type("application/json")
        .send(body.to_string())
        .unwrap_or_else(|e| panic!("POST {path}: {e}"));
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    (code, serde_json::from_str(&text).unwrap_or_default())
}

/// Id записей выдачи `POST /search` (`entry_id` карточки записи или `id`).
fn search_ids(v: &serde_json::Value) -> Vec<String> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["entry_id"].as_str().or(i["id"].as_str()))
        .map(String::from)
        .collect()
}

/// M6.6 §9 (замечание 8): флаги полей решения `zam log insight --kind decision` → `data.decision`
/// со всеми полями; фильтр поиска `filters.decided_by` находит запись; у `insight supersede` без
/// `--kind` флаги решения принимаются, когда замещаемая запись — решение.
#[test]
fn log_insight_decision_flags() {
    let (url, tok) = start("decision-flags");
    let r = tmp("decision-flags-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let o = zam(
        &url,
        &tok,
        &r,
        &[
            "log",
            "insight",
            "--kind",
            "decision",
            "--title",
            "Кэш сборки: sccache",
            "--scope",
            "global",
            "--context",
            "Сборка идёт 20 минут",
            "--option",
            "ccache",
            "--option",
            "без кэша",
            "--chosen",
            "sccache",
            "--rationale",
            "Работает с MSVC",
            "--consequences",
            "Нужен каталог кэша",
            "--decided-by",
            "human",
            "--revisit-if",
            "Появится удалённый кэш",
        ],
        "Кэш сборки ускоряет повторную сборку.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["data"]["kind"], "decision", "{e}");
    assert_eq!(
        e["data"]["decision"],
        serde_json::json!({
            "context": "Сборка идёт 20 минут",
            "options": ["ccache", "без кэша"],
            "chosen": "sccache",
            "rationale": "Работает с MSVC",
            "consequences": "Нужен каталог кэша",
            "decided_by": "human",
            "revisit_if": "Появится удалённый кэш"
        }),
        "{e}"
    );
    // поиск с фильтром decided_by: human находит, agent — нет
    let search = |by: &str| {
        let (c, v) = rest(
            &url,
            &tok,
            "/search",
            &serde_json::json!({"query": "кэш сборки", "top_k": 20,
                                "filters": {"decided_by": by}}),
        );
        assert_eq!(c, 200, "{v}");
        search_ids(&v)
    };
    assert!(search("human").contains(&id), "{:?}", search("human"));
    assert!(!search("agent").contains(&id), "{:?}", search("agent"));

    // замещение решения без --kind: вид — у старой записи (decision), флаги решения приняты
    let o = z(&[
        "insight",
        "supersede",
        &id,
        "--title",
        "Кэш сборки: sccache с удалённым хранилищем",
        "--chosen",
        "sccache + S3",
        "--decided-by",
        "agent",
    ]);
    let s = ok(&o);
    assert!(s.contains(&format!("(замещает {id})")), "{s}");
    let n = json(&z(&["insight", "show", &id_of(&s), "--json"]));
    assert_eq!(n["data"]["kind"], "decision", "{n}");
    assert_eq!(
        n["data"]["decision"],
        serde_json::json!({"chosen": "sccache + S3", "decided_by": "agent"}),
        "{n}"
    );
}

/// M6.6 §9: флаги решения — только у `--kind decision` (у замещения без `--kind` — вид старой
/// записи), иначе код 6 до записи; больше 8 `--option` — код 6 до любого запроса.
#[test]
fn decision_flags_need_kind_decision() {
    let r = tmp("decision-kind-repo");
    let mut many: Vec<String> = Vec::new();
    for i in 1..=9 {
        many.push("--option".into());
        many.push(format!("вариант {i}"));
    }
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let log = |kind: &'static str| -> Vec<&'static str> {
        vec![
            "log", "insight", "--kind", kind, "--title", "Т", "--scope", "global",
        ]
    };
    let cases: Vec<Vec<&str>> = vec![
        [&log("gotcha")[..], &["--chosen", "x"]].concat(),
        [
            &log("pattern")[..],
            &["--context", "к", "--revisit-if", "п"],
        ]
        .concat(),
        [&log("decision")[..], &many].concat(),
        [&["insight", "supersede", "01E", "--title", "Т"][..], &many].concat(),
    ];
    // до запроса: недоступный адрес не мешает — код 6, а не 2
    for args in &cases {
        let o = zam("http://127.0.0.1:9", "t", &r, args, b"");
        assert_eq!(o.status.code(), Some(6), "{args:?}: {}", err(&o));
    }
    let o = zam("http://127.0.0.1:9", "t", &r, &cases[0], b"");
    assert!(
        err(&o).contains("--chosen") && err(&o).contains("--kind decision"),
        "{}",
        err(&o)
    );
    let o = zam("http://127.0.0.1:9", "t", &r, &cases[2], b"");
    assert!(err(&o).contains("не больше 8"), "{}", err(&o));

    // замещение не-решения без --kind: вид старой записи — gotcha, код 6, запись не замещена
    let (url, tok) = start("decision-kind");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let id = id_of(&ok(&z(&[
        "log",
        "insight",
        "--kind",
        "gotcha",
        "--title",
        "LF в shell-скриптах",
        "--scope",
        "global",
    ])));
    let o = z(&[
        "insight",
        "supersede",
        &id,
        "--title",
        "LF в shell-скриптах: уточнение",
        "--rationale",
        "r",
    ]);
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    assert!(
        err(&o).contains("--rationale") && err(&o).contains("gotcha"),
        "{}",
        err(&o)
    );
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["status"], "active", "запись не замещена: {e}");
    // явный --kind decision у замещения — флаги приняты
    let o = z(&[
        "insight",
        "supersede",
        &id,
        "--kind",
        "decision",
        "--title",
        "LF в shell-скриптах: решение",
        "--chosen",
        "LF везде",
    ]);
    let n = json(&z(&["insight", "show", &id_of(&ok(&o)), "--json"]));
    assert_eq!(n["data"]["kind"], "decision", "{n}");
    assert_eq!(n["data"]["decision"]["chosen"], "LF везде", "{n}");
}

/// M6.6 §9: доказательство без файла — `--evidence-checked`, `--evidence-method`,
/// `--evidence-result` (все три вместе) → `data.evidence`; с `--evidence-file` — конфликт.
#[test]
fn log_insight_evidence_flags() {
    let (url, tok) = start("evidence-flags");
    let r = tmp("evidence-flags-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let base = [
        "log",
        "insight",
        "--kind",
        "discovery",
        "--title",
        "sccache ускоряет сборку вдвое",
        "--scope",
        "global",
    ];
    let ev = [
        "--evidence-checked",
        "3 сборки",
        "--evidence-method",
        "чистая и повторная сборка",
        "--evidence-result",
        "20 мин → 9 мин",
    ];
    let id = id_of(&ok(&z(
        &[&base[..], &["--certainty", "theorem"], &ev].concat()
    )));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["certainty"], "theorem", "{e}");
    assert_eq!(
        e["data"]["evidence"],
        serde_json::json!({"checked": "3 сборки", "method": "чистая и повторная сборка",
                           "result": "20 мин → 9 мин"}),
        "{e}"
    );
    // неполный набор и конфликт с файлом — код 6 до запроса
    let f = r.join("ev.json");
    std::fs::write(&f, r#"{"checked": "c", "method": "m", "result": "r"}"#).unwrap();
    for extra in [
        vec!["--evidence-checked", "c", "--evidence-method", "m"],
        vec!["--evidence-result", "r"],
        [&ev[..], &["--evidence-file", f.to_str().unwrap()]].concat(),
    ] {
        let args = [&base[..], &extra].concat();
        let o = zam("http://127.0.0.1:9", "t", &r, &args, b"");
        assert_eq!(o.status.code(), Some(6), "{extra:?}: {}", err(&o));
    }
}

/// Строка stderr об умолчании агента (M6.6 §9, замечание 8).
const DEFAULT_NOTE: &str =
    "certainty: hypothesis (умолчание агента; проверено — --certainty theorem и доказательство)";

/// M6.6 §9: без `--certainty` у `log insight` и `insight supersede` — строка в stderr об
/// умолчании `hypothesis`; stdout `<id>@<rev>` прежний; с явным `--certainty` строки нет.
#[test]
fn log_insight_notes_default_hypothesis() {
    let (url, tok) = start("default-hypothesis");
    let r = tmp("default-hypothesis-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let log = |title: &str, extra: &[&str]| {
        let mut a = vec![
            "log", "insight", "--kind", "pattern", "--title", title, "--scope", "global",
        ];
        a.extend_from_slice(extra);
        z(&a)
    };
    let o = log("Скрипты сборки — в xtask", &[]);
    let s = ok(&o);
    let id = id_of(&s);
    assert_eq!(s, format!("{id}@1\n"), "stdout прежний");
    assert_eq!(id.len(), 26, "{s}");
    assert!(
        err(&o).lines().any(|l| l == DEFAULT_NOTE),
        "stderr: {}",
        err(&o)
    );
    let o = log("Тесты — рядом с кодом", &["--certainty", "hypothesis"]);
    ok(&o);
    assert!(!err(&o).contains("certainty:"), "{}", err(&o));
    // замещение без --certainty — та же строка; stdout прежний
    let o = z(&[
        "insight",
        "supersede",
        &id,
        "--title",
        "Скрипты сборки и выпуска — в xtask",
    ]);
    let s = ok(&o);
    assert!(s.ends_with(&format!("@1 (замещает {id})\n")), "{s}");
    assert!(
        err(&o).lines().any(|l| l == DEFAULT_NOTE),
        "stderr: {}",
        err(&o)
    );
}

/// M6.6 §9 (16г): лента insight по умолчанию — страница сервера (50) и строка stderr
/// «показано N, есть ещё: --limit 200 или --all»; `--all` — вся лента обходом `next_cursor`;
/// `--limit` и `--all` — конфликт (код 6); `--knowledge` — синоним `--global`.
#[test]
fn insight_list_more_hint_and_all() {
    let (url, tok) = start("list-all");
    let r = tmp("list-all-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    for i in 0..61 {
        let (c, v) = rest(
            &url,
            &tok,
            "/spaces/knowledge/entries",
            &serde_json::json!({"kind": "insight", "title": format!("Приём номер {i:02}"),
                                "body": "", "data": {"kind": "pattern"}, "scope": "global",
                                "force": true}),
        );
        assert_eq!(c, 201, "{v}");
    }
    let lines = |o: &Output| ok(o).lines().count();
    let o = z(&["insight", "list", "--global"]);
    assert_eq!(lines(&o), 50, "{}", out(&o));
    assert_eq!(
        err(&o),
        "показано 50, есть ещё: --limit 200 или --all\n",
        "{}",
        err(&o)
    );
    let o = z(&["insight", "list", "--global", "--all"]);
    assert_eq!(lines(&o), 61, "{}", out(&o));
    assert!(!err(&o).contains("есть ещё"), "{}", err(&o));
    let j = json(&z(&["insight", "list", "--knowledge", "--all", "--json"]));
    assert_eq!(j["items"].as_array().unwrap().len(), 61, "{j}");
    assert!(j["next_cursor"].is_null(), "{j}");
    let ids: std::collections::BTreeSet<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 61, "без повторов между страницами");
    let o = z(&["insight", "list", "--global", "--limit", "10"]);
    assert_eq!(lines(&o), 10, "{}", out(&o));
    assert!(
        err(&o).contains("показано 10, есть ещё: --limit 200 или --all"),
        "{}",
        err(&o)
    );
    let o = z(&["insight", "list", "--knowledge", "--limit", "61"]);
    assert_eq!(lines(&o), 61, "{}", out(&o));
    assert!(err(&o).is_empty(), "{}", err(&o));
    let o = zam(
        "http://127.0.0.1:9",
        "t",
        &r,
        &["insight", "list", "--global", "--all", "--limit", "5"],
        b"",
    );
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
}
