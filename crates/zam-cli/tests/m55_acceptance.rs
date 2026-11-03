//! Приёмка M5.5 «Отчёты», сценарии 1–2 спецификации §12 (задача 8): `zam report` с тремя PNG,
//! формулой и Mermaid против настоящего zam-server — страница отчёта (`format=html`, сессия
//! WebUI) с тремя адресами картинок, `.math` и `mermaid-src`; тот же отчёт через MCP
//! `memory_log kind=report` (картинки base64) — тот же HTML с точностью до id. Страница и PDF в
//! браузере — e2e `acceptance_pdf_from_cli_report`. Прочие сценарии приёмки —
//! `crates/zam-server/tests/m55_acceptance.rs`. PNG-фикстуры генерируются в тесте.
mod common;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Output;

use serde_json::{Value, json};
use zam_core::Id;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::{Access, entry_attachments};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e55";
const PW: &str = "pw";

/// Сервер в фоновом потоке: адрес, токен `rw` администратора, его id, состояние и рантайм.
struct Srv {
    url: String,
    tok: String,
    admin: Id,
    st: zam_server::AppState,
    rt: tokio::runtime::Handle,
}

impl Srv {
    fn start(name: &str) -> Srv {
        let data = common::sandbox("m55-accept", &format!("{name}-data"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let handle = rt.handle().clone();
            rt.block_on(async move {
                let cfg = ServerConfig {
                    data_dir: data,
                    ..ServerConfig::default()
                };
                let (app, st, _) = build(cfg, Some(PW.into())).await.unwrap();
                let admin = st.admin_id;
                let (_, t) = st.system.issue_token(admin, "t", Access::Rw).await.unwrap();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                tx.send((url, t, admin, st.clone(), handle)).unwrap();
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            });
        });
        let (url, tok, admin, st, rt) = rx.recv().unwrap();
        Srv {
            url,
            tok,
            admin,
            st,
            rt,
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                // системный прокси не нужен: сервер на 127.0.0.1
                .proxy(None)
                .build(),
        )
    }

    /// Cookie сессии WebUI администратора (свой пароль, без смены).
    fn login(&self) -> String {
        let r = Self::agent()
            .post(&format!("{}/api/v1/auth/login", self.url))
            .header("content-type", "application/json")
            .send(json!({"login": "admin", "password": PW}).to_string())
            .unwrap();
        assert_eq!(r.status(), 200);
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    /// Страница отчёта `format=html` сессией WebUI.
    fn html(&self, cookie: &str, id: &str) -> Value {
        let mut r = Self::agent()
            .get(&format!("{}/api/v1/entries/{id}?format=html", self.url))
            .header("cookie", cookie)
            .call()
            .unwrap();
        let text = r.body_mut().read_to_string().unwrap();
        assert_eq!(r.status(), 200, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    /// Вызов инструмента MCP: текст результата и признак ошибки.
    fn mcp(&self, name: &str, args: Value) -> (String, bool) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}});
        let mut r = Self::agent()
            .post(&format!("{}/mcp", self.url))
            .header("authorization", &format!("Bearer {}", self.tok))
            .header("accept", "application/json, text/event-stream")
            .header("x-zam-solution", G)
            .header("content-type", "application/json")
            .send(body.to_string())
            .unwrap();
        let text = r.body_mut().read_to_string().unwrap();
        assert_eq!(r.status(), 200, "{text}");
        let j: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        let res = &j["result"];
        (
            res["content"][0]["text"].as_str().unwrap_or("").to_string(),
            res["isError"].as_bool() == Some(true),
        )
    }

    fn files(&self, id: &str) -> Vec<zam_store::EntryAttachment> {
        let db = self.rt.block_on(self.st.users.get(self.admin)).unwrap();
        self.rt
            .block_on(entry_attachments(&db, id.parse().unwrap()))
            .unwrap()
    }
}

/// Каталог проекта с `solution.toml` решения `G`.
fn project(name: &str) -> std::path::PathBuf {
    let d = common::sandbox("m55-accept", name);
    std::fs::create_dir_all(d.join(".zeroaimemory")).unwrap();
    std::fs::write(
        d.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Приёмка\"\n"),
    )
    .unwrap();
    d
}

/// PNG-подобные байты: сигнатура и `n` байт по `seed`.
fn png(seed: u8, n: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

/// base64 (стандартный алфавит с выравниванием) — у `zam-cli` нет своей зависимости.
fn b64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn zam(url: &str, tok: &str, cwd: &Path, args: &[&str]) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", tok)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

/// Id отчёта из адреса `…/j/reports/<id>` в тексте.
fn report_id(text: &str) -> String {
    let at = text
        .find("/j/reports/")
        .unwrap_or_else(|| panic!("нет адреса: {text}"));
    text[at + "/j/reports/".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

const IMAGES: [&str; 3] = ["figs/loss.png", "figs/acc.png", "figs/lr.png"];

/// Тело отчёта: заголовок, строчная и выключная формулы, Mermaid, три картинки.
const BODY: &str = "# Обучение модели\n\nПотери $L = -\\sum_i y_i \\log p_i$ падают.\n\n\
$$\\int_0^1 x\\,dx = \\frac{1}{2}$$\n\n```mermaid\ngraph LR; A[Энкодер]-->B[Декодер]\n```\n\n\
![потери](figs/loss.png)\n\n![точность](figs/acc.png)\n\n![шаг](figs/lr.png)\n";

/// HTML страницы без id отчёта.
fn html_of(page: &Value, id: &str) -> String {
    page["html"].as_str().unwrap().replace(id, "ID")
}

/// Отчёт `zam report` проекта `name`: id и байты картинок.
fn cli_report(s: &Srv, name: &str) -> (String, Vec<Vec<u8>>) {
    let d = project(name);
    let imgs: Vec<Vec<u8>> = (0..3).map(|i| png(i as u8 + 10, 300 + i)).collect();
    for (n, data) in IMAGES.iter().zip(&imgs) {
        let p = d.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    std::fs::write(d.join("report.md"), BODY).unwrap();
    let o = zam(&s.url, &s.tok, &d, &["report", "report.md"]);
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        stdout.starts_with(&format!("Отчёт: {}/#/u/", s.url)),
        "{stdout}"
    );
    (report_id(&stdout), imgs)
}

/// §12 п. 1: `zam report` → ссылка; страница — три картинки по адресам файлов записи, формулы,
/// исходник Mermaid для гидратации.
#[test]
fn acceptance_cli_report_three_png_formula_mermaid() {
    let s = Srv::start("cli");
    let (cli, _) = cli_report(&s, "cli");
    let files = s.files(&cli);
    assert_eq!(files.len(), 3, "{files:?}");
    let page = s.html(&s.login(), &cli);
    let html = page["html"].as_str().unwrap();
    for f in &files {
        assert_eq!(f.mime, "image/png");
        let src = format!(
            "src=\"api/v1/entries/{cli}/attachments/{}/{}\"",
            f.sha256, f.filename
        );
        assert!(html.contains(&src), "{src} в {html}");
    }
    assert!(html.contains("class=\"math"), "{html}");
    assert!(html.contains("<pre class=\"mermaid-src\""), "{html}");
    assert!(html.contains("Энкодер"), "{html}");
    assert_eq!(page["entry"]["title"], "Обучение модели");
    assert!(page["warnings"].as_array().unwrap().is_empty(), "{page}");
}

/// §12 п. 2: тот же отчёт через MCP `memory_log kind=report` (картинки base64) — тот же HTML с
/// точностью до id, те же файлы.
#[test]
fn acceptance_mcp_report_same() {
    let s = Srv::start("mcp");
    let (cli, imgs) = cli_report(&s, "mcp");
    let images: Vec<Value> = IMAGES
        .iter()
        .zip(&imgs)
        .map(|(n, data)| json!({"name": n, "data": b64(data)}))
        .collect();
    let (text, err) = s.mcp(
        "memory_log",
        json!({"kind": "report", "title": "Обучение модели", "body": BODY, "images": images}),
    );
    assert!(!err, "{text}");
    assert!(text.starts_with("Отчёт записан: "), "{text}");
    let mcp = report_id(&text);
    assert_ne!(mcp, cli);
    let cookie = s.login();
    let (p1, p2) = (s.html(&cookie, &cli), s.html(&cookie, &mcp));
    assert_eq!(html_of(&p2, &mcp), html_of(&p1, &cli));
    assert_eq!(p2["toc"], p1["toc"]);
    let pairs = |id: &str| {
        let mut v: Vec<_> = s
            .files(id)
            .into_iter()
            .map(|f| (f.filename, f.sha256, f.mime, f.size))
            .collect();
        v.sort();
        v
    };
    assert_eq!(pairs(&mcp), pairs(&cli), "те же файлы");
}
