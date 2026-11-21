//! `zam init`, `zam status` (версия блока), `zam mcp` и `zam guide` (§13.2, §14; задача 11 M4)
//! против настоящего zam-server в том же процессе тестов. Проект и «домашний» каталог — во
//! временном каталоге целей сборки (`CARGO_TARGET_TMPDIR`); в проекте есть `.git`, чтобы корень
//! не поднялся до репозитория ZeroAIMemory. Реальные CLAUDE.md, `.claude` и профиль
//! пользователя не трогаются.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

/// Версия блока инструкции сервера (`zam_core::AGENT_BLOCK_VERSION`): маркеры и напоминания
/// сверяются с ней, а не с числом — новая версия блока не требует правки теста.
const V: u32 = zam_core::AGENT_BLOCK_VERSION;

/// Маркер начала текущего блока.
fn begin() -> String {
    format!("<!-- zeroaimemory:begin v{V} -->")
}

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-init-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна».
fn start(name: &str) -> (String, String) {
    let (url, mut t) = start_n(name, 1);
    (url, t.remove(0))
}

/// [`start`] с `n` токенами `rw` пользователя «Анна».
fn start_n(name: &str, n: usize) -> (String, Vec<String>) {
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
            let mut tokens = Vec::with_capacity(n);
            for i in 0..n {
                let label = format!("t{i}");
                let (_, t) = st
                    .system
                    .issue_token(u.id, &label, Access::Rw)
                    .await
                    .unwrap();
                tokens.push(t);
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, tokens)).unwrap();
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

struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str, url: &str, token: &str) -> Proj {
        let root = tmp(name);
        let repo = root.join("repo");
        let home = root.join("home");
        // корень git — сам проект (иначе корнем стал бы репозиторий ZeroAIMemory)
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            repo,
            home,
        }
    }

    /// `zam` в проекте с чистым окружением (без ZAM_* и переменных агента).
    fn cmd(&self) -> Command {
        let mut c = common::zam_cmd();
        c.env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("COMPUTERNAME", "init-box")
            .env("HOSTNAME", "init-box")
            .current_dir(&self.repo);
        c
    }

    fn init(&self, extra: &[&str], stdin: &str) -> Output {
        let mut c = self.cmd();
        c.args(["init", "--url", &self.url, "--token", &self.token])
            .args(extra)
            .write_stdin(stdin.to_string());
        c.output().unwrap()
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.repo.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    fn json(&self, rel: &str) -> Value {
        serde_json::from_str(&self.read(rel)).unwrap()
    }

    /// Снимок файлов проекта (без `.git`) и домашнего конфига.
    fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().display().to_string();
                    out.push((rel, std::fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.repo, &self.repo, &mut out);
        walk(&self.home, &self.home, &mut out);
        out.sort();
        out
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) {
    assert!(o.status.success(), "stdout: {}\nstderr: {}", out(o), err(o));
}

/// GET с токеном.
fn api_get(p: &Proj, path: &str) -> (u16, String) {
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    let mut r = a
        .get(&format!("{}/api/v1{path}", p.url))
        .header("Authorization", format!("Bearer {}", p.token))
        .call()
        .unwrap();
    let code = r.status().as_u16();
    (code, r.body_mut().read_to_string().unwrap())
}

/// POST JSON с токеном.
fn api_post(p: &Proj, path: &str, body: &Value) -> u16 {
    api_post_h(p, path, body, &[])
}

/// POST JSON с токеном и заголовками.
fn api_post_h(p: &Proj, path: &str, body: &Value, headers: &[(&str, &str)]) -> u16 {
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    let mut rb = a
        .post(&format!("{}/api/v1{path}", p.url))
        .header("Authorization", format!("Bearer {}", p.token));
    for (k, v) in headers {
        rb = rb.header(*k, *v);
    }
    rb.content_type("application/json")
        .send(body.to_string().as_bytes())
        .unwrap()
        .status()
        .as_u16()
}

#[test]
fn init_creates_solution_block_hooks_mcp() {
    let (url, tok) = start("create");
    let p = Proj::new("create", &url, &tok);
    let o = p.init(&["--name", "Демо", "--yes"], "");
    ok(&o);
    let stdout = out(&o);
    // solution.toml с новым GUID и именем
    let sol: toml::Table = toml::from_str(&p.read(".zeroaimemory/solution.toml")).unwrap();
    let guid = sol["agent_solution_id"].as_str().unwrap().to_string();
    assert_eq!(sol["name"].as_str(), Some("Демо"));
    assert!(guid.parse::<zam_core::SolutionGuid>().is_ok(), "{guid}");
    assert!(stdout.contains(&guid), "{stdout}");
    // решение зарегистрировано на сервере (владелец — Анна)
    let (code, body) = api_get(&p, &format!("/solutions/{guid}"));
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("\"role\":\"owner\""), "{body}");
    // текущий блок с адресом и решением, без токена
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with(&format!("{}\n", begin())), "{md}");
    assert!(md.contains(&format!("ZeroAIMemoryUrl={url}\n")), "{md}");
    assert!(md.contains(&guid) && md.contains("«Демо»"), "{md}");
    assert!(!md.contains(&tok), "токен в CLAUDE.md");
    assert!(!p.repo.join("AGENTS.md").exists());
    // хуки Claude Code — exec form
    let s = p.json(".claude/settings.json");
    assert_eq!(
        s["hooks"]["PreToolUse"][0]["matcher"],
        "Write|Edit|MultiEdit|NotebookEdit|Bash|Agent|Task"
    );
    assert_eq!(
        s["hooks"]["PreToolUse"][0]["hooks"][0],
        json!({"type": "command", "command": "zam", "args": ["hook", "pre-tool"], "timeout": 1})
    );
    for ev in [
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUseFailure",
        "Stop",
        "PreCompact",
    ] {
        assert_eq!(s["hooks"][ev][0]["hooks"][0]["command"], "zam", "{ev}");
        assert_eq!(s["hooks"][ev][0]["hooks"][0]["timeout"], 2, "{ev}");
    }
    // .mcp.json — stdio `zam mcp` (M6 §7.1): без токена и адреса, решение — из solution.toml
    let m = p.json(".mcp.json");
    let z = &m["mcpServers"]["zeroaimemory"];
    assert_eq!(
        *z,
        json!({"type": "stdio", "command": "zam", "args": ["mcp"]}),
        "{m}"
    );
    assert!(!p.read(".mcp.json").contains(&tok));
    assert!(!p.read(".claude/settings.json").contains(&tok));
    assert_eq!(p.read(".zeroaimemory/.gitignore"), "cache/\nlogs/\n");
    // токен — в домашнем конфиге ключом своего адреса (M7 §4.4), адрес машины — `url` (новая
    // машина, режим block: его не было); zam mcp берёт токен оттуда же — подсказки про ZAM_TOKEN нет
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(p.home.join(".zeroaimemory/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(tok.as_str()),
        "{home:?}"
    );
    assert_eq!(home["url"].as_str(), Some(url.as_str()), "{home:?}");
    assert!(home.get("token").is_none(), "{home:?}");
    assert!(
        stdout.contains(&format!(
            "Токен сохранён в ~/.zeroaimemory/config.toml [servers.\"{url}\"]"
        )),
        "{stdout}"
    );
    assert!(!stdout.contains(">> ~/.profile"), "{stdout}");
    assert!(!stdout.contains("ZAM_TOKEN"), "{stdout}");
    assert!(stdout.contains("zam mcp"), "{stdout}");
    // машину zam mcp берёт сам (ZAM_MACHINE или имя хоста) — подсказки нет
    assert!(!stdout.contains("ZAM_MACHINE"), "{stdout}");
    assert!(
        !stdout.contains(&tok) && !err(&o).contains(&tok),
        "токен в выводе"
    );

    // zam без флагов находит сервер (блок) и токен (домашний конфиг); полный протокол
    let o = p.cmd().arg("guide").output().unwrap();
    ok(&o);
    assert!(out(&o).contains("memory_cache_put"), "{}", out(&o));
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    assert!(
        out(&o).contains(&format!("Блок инструкции: v{V} (актуален)")),
        "{}",
        out(&o)
    );
}

#[test]
fn init_is_idempotent_and_preserves_foreign_text_and_hooks() {
    let (url, tok) = start("idem");
    let p = Proj::new("idem", &url, &tok);
    let g = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e81";
    std::fs::create_dir_all(p.repo.join(".zeroaimemory")).unwrap();
    std::fs::write(
        p.repo.join(".zeroaimemory/solution.toml"),
        format!("agent_solution_id = \"{g}\"\nname = \"Старое\"\n"),
    )
    .unwrap();
    // CLAUDE.md с чужим текстом и прежним блоком v1 (с токеном)
    let head = "# Проект\r\n\r\nСвой текст — не трогать.\r\n";
    let tail = "\r\n## Ещё раздел\r\nконец";
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        format!(
            "{head}<!-- zeroaimemory:begin v1 -->\r\nZeroAIMemoryUrl={url}\r\nZeroAIMemoryToken={tok}\r\n<!-- zeroaimemory:end -->{tail}"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(p.repo.join(".claude")).unwrap();
    let settings = json!({
        "permissions": {"allow": ["Bash(ls)"]},
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "my-lint"}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "zam", "args": ["hook", "checkpoint"], "timeout": 9}]}]
        },
        "model": "opus"
    });
    // порядок ключей файла: permissions, hooks, model (не алфавитный)
    std::fs::write(
        p.repo.join(".claude/settings.json"),
        "{\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]},\n  \"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"my-lint\"}]}], \"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"zam\", \"args\": [\"hook\", \"checkpoint\"], \"timeout\": 9}]}]},\n  \"model\": \"opus\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        p.repo.join(".mcp.json"),
        "{\"mcpServers\": {\"other\": {\"command\": \"x\"}}}",
    )
    .unwrap();

    // без --yes и с ответом «n»: блок и solution — да, файлы агента — нет
    let o = p.init(&[], "n\n");
    ok(&o);
    assert!(
        out(&o).contains("Хуки и .mcp.json не записаны"),
        "{}",
        out(&o)
    );
    assert_eq!(
        p.read(".mcp.json"),
        "{\"mcpServers\": {\"other\": {\"command\": \"x\"}}}"
    );

    let o = p.init(&["--yes"], "");
    ok(&o);
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with(head), "{md:?}");
    assert!(md.ends_with(tail), "{md:?}");
    assert!(
        md.contains(&format!("{}\r\n", begin())),
        "CRLF файла: {md:?}"
    );
    assert!(!md.contains(&tok), "прежний токен убран из блока");
    assert!(md.contains("«Старое»"), "{md}");
    assert_eq!(md.matches("zeroaimemory:begin").count(), 1);
    // токен из прежнего блока перенесён в домашний конфиг
    assert!(
        std::fs::read_to_string(p.home.join(".zeroaimemory/config.toml"))
            .unwrap()
            .contains(&tok)
    );
    let s = p.json(".claude/settings.json");
    let pre = s["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 2, "{s}");
    assert_eq!(pre[0]["hooks"][0]["command"], "my-lint");
    assert_eq!(pre[1]["hooks"][0]["args"][1], "pre-tool");
    let stop = s["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(
        stop.len(),
        1,
        "наш прежний хук заменён, а не продублирован: {s}"
    );
    assert_eq!(stop[0]["hooks"][0]["timeout"], 2);
    assert_eq!(s["permissions"], settings["permissions"]);
    assert_eq!(s["model"], "opus");
    let text = p.read(".claude/settings.json");
    let pos = |k: &str| text.find(&format!("\"{k}\"")).unwrap();
    assert!(
        pos("permissions") < pos("hooks") && pos("hooks") < pos("model"),
        "{text}"
    );
    let m = p.json(".mcp.json");
    assert_eq!(m["mcpServers"]["other"], json!({"command": "x"}));
    assert_eq!(m["mcpServers"]["zeroaimemory"]["command"], "zam");

    // повторный запуск ничего не меняет
    let before = p.snapshot();
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert!(out(&o).contains("Файлы проекта актуальны"), "{}", out(&o));
    assert_eq!(p.snapshot(), before);

    // другой --id при существующем solution.toml — ошибка конфигурации
    let o = p.init(
        &["--id", "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e82", "--yes"],
        "",
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));

    // битый JSON — код 1, ни один файл не изменён
    std::fs::write(p.repo.join(".mcp.json"), "{\"mcpServers\": ").unwrap();
    std::fs::write(p.repo.join("CLAUDE.md"), "только текст\n").unwrap();
    let before = p.snapshot();
    let o = p.init(&["--yes"], "");
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(
        err(&o).contains(".mcp.json: некорректный JSON"),
        "{}",
        err(&o)
    );
    assert_eq!(p.snapshot(), before);
}

#[test]
fn init_codex_writes_codex_hooks() {
    let (url, tok) = start("codex");
    let p = Proj::new("codex", &url, &tok);
    let o = p.init(&["--agent", "codex", "--lang", "en", "--yes"], "");
    ok(&o);
    let md = p.read("AGENTS.md");
    assert!(
        md.starts_with(&format!("{}\n## ZeroAIMemory memory\n", begin())),
        "{md}"
    );
    assert!(!p.repo.join("CLAUDE.md").exists());
    assert!(!p.repo.join(".claude").exists());
    assert!(
        !p.repo.join(".mcp.json").exists(),
        "Codex не читает .mcp.json"
    );
    assert!(out(&o).contains("~/.codex/config.toml"), "{}", out(&o));
    let h = p.json(".codex/hooks.json");
    let cmd = |ev: &str| {
        h["hooks"][ev][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(cmd("SessionStart"), "zam hook session-start --agent codex");
    assert_eq!(cmd("PreToolUse"), "zam hook pre-tool --agent codex");
    assert_eq!(cmd("PostToolUse"), "zam hook post-tool --agent codex");
    assert_eq!(cmd("Stop"), "zam hook checkpoint --agent codex");
    assert_eq!(h["hooks"]["PreToolUse"][0]["matcher"], "Bash|apply_patch");
    assert!(h["hooks"].get("PostToolUseFailure").is_none(), "{h}");
    assert!(
        !out(&o).contains("SetEnvironmentVariable"),
        "без MCP подсказка не нужна"
    );
    // both: CLAUDE.md и AGENTS.md, хуки обоих; Codex-хуки не дублируются
    let o = p.init(&["--agent", "both", "--yes"], "");
    ok(&o);
    assert!(p.read("CLAUDE.md").contains("## Память ZeroAIMemory"));
    assert!(
        p.read("AGENTS.md").contains("## Память ZeroAIMemory"),
        "язык по умолчанию — ru"
    );
    assert!(p.repo.join(".claude/settings.json").is_file());
    let h = p.json(".codex/hooks.json");
    assert_eq!(h["hooks"]["Stop"].as_array().unwrap().len(), 1, "{h}");
}

/// Прогрев сервера без сроков: первый запрос пользователя открывает его базу (миграции) — под
/// нагрузкой полного прогона дольше 5 с `zam status` (профиль `Probe`), и блок пропадал из
/// вывода. Проверяется предупреждение об устаревшем блоке, а не время первого ответа.
fn warm(p: &Proj) {
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    for path in ["/spaces", "/agent-setup"] {
        let st = a
            .get(&format!("{}/api/v1{path}", p.url))
            .header("Authorization", format!("Bearer {}", p.token))
            .call()
            .unwrap()
            .status();
        assert_eq!(st.as_u16(), 200, "прогрев {path}");
    }
}

#[test]
fn status_warns_outdated_block() {
    let (url, tok) = start("status");
    let p = Proj::new("status", &url, &tok);
    warm(&p);
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        format!("<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={url}\nZeroAIMemoryToken={tok}\n<!-- zeroaimemory:end -->\n"),
    )
    .unwrap();
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    let s = out(&o);
    assert_eq!(
        s.matches(&format!(
            "Блок ZeroAIMemory устарел (v1 → v{V}): обнови через `zam init` или WebUI"
        ))
        .count(),
        1,
        "{s}"
    );
    let o = p.cmd().args(["status", "--json"]).output().unwrap();
    ok(&o);
    let j: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        j["agent_block"],
        json!({"version": 1, "current": V, "outdated": true})
    );
}

#[test]
fn mcp_stdio_proxy_roundtrip() {
    let (url, tok) = start("mcp");
    let p = Proj::new("mcp", &url, &tok);
    let g = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e91";
    std::fs::create_dir_all(p.repo.join(".zeroaimemory")).unwrap();
    std::fs::write(
        p.repo.join(".zeroaimemory/solution.toml"),
        format!("agent_solution_id = \"{g}\"\nname = \"Прокси\"\n"),
    )
    .unwrap();
    assert_eq!(
        api_post(
            &p,
            "/solutions",
            &json!({"agent_solution_id": g, "name": "Прокси"})
        ),
        201
    );
    let lines = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "stdio-test", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        // запись в пространство решения из solution.toml (X-Zam-Solution) и чтение дерева
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "memory_write", "arguments": {"path": "notes/proxy.md", "content": "# Прокси\n\nтекст\n"}}}),
    ];
    let mut stdin: String = lines.iter().map(|l| format!("{l}\n")).collect();
    stdin.push_str("не json\n\n");
    let o = p
        .cmd()
        .env("ZAM_URL", &url)
        .env("ZAM_TOKEN", &tok)
        .arg("mcp")
        .write_stdin(stdin)
        .timeout(std::time::Duration::from_secs(60))
        .output()
        .unwrap();
    ok(&o);
    let resp: Vec<Value> = out(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    assert_eq!(
        resp.len(),
        4,
        "по строке на запрос (уведомление — без ответа): {resp:?}"
    );
    let by_id = |id: Value| {
        resp.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id}: {resp:?}"))
    };
    assert_eq!(
        by_id(json!(1))["result"]["serverInfo"]["name"],
        "zeroaimemory"
    );
    let tools = by_id(json!(2))["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        tools.iter().any(|t| t["name"] == "memory_search"),
        "{tools:?}"
    );
    let w = by_id(json!(3));
    assert_eq!(w["result"]["isError"], false, "{w}");
    assert_eq!(by_id(Value::Null)["error"]["code"], -32700);
    // запись ушла в решение из solution.toml
    let (code, body) = api_get(&p, &format!("/spaces/{g}/tree?depth=2"));
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("proxy.md"), "{body}");
    assert!(!err(&o).contains(&tok));

    // неверный токен: ответ JSON-RPC error с текстом сервера, код выхода 0
    let o = p
        .cmd()
        .env("ZAM_URL", &url)
        .env(
            "ZAM_TOKEN",
            "zam_000000000000000000000000000000000000000000",
        )
        .arg("mcp")
        .write_stdin(format!("{}\n", lines[2]))
        .output()
        .unwrap();
    ok(&o);
    let r: Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(r["id"], 2);
    assert_eq!(r["error"]["code"], -32000);
    assert!(
        r["error"]["message"].as_str().unwrap().contains("HTTP 401"),
        "{r}"
    );
}

#[test]
fn init_bom_files_and_v1_token_without_flag() {
    let (url, tok) = start("bom");
    let p = Proj::new("bom", &url, &tok);
    // блок v1 первой строкой файла с BOM — единственный источник адреса и токена
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        format!(
            "\u{feff}<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={url}\nZeroAIMemoryToken={tok}\n<!-- zeroaimemory:end -->\nхвост\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(p.repo.join(".claude")).unwrap();
    std::fs::write(
        p.repo.join(".claude/settings.json"),
        "\u{feff}{\"model\": \"opus\"}\n",
    )
    .unwrap();
    let o = p.cmd().args(["init", "--yes"]).output().unwrap();
    ok(&o);
    let stdout = out(&o);
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with(&format!("\u{feff}{}\n", begin())), "{md:?}");
    assert!(md.ends_with("<!-- zeroaimemory:end -->\nхвост\n"), "{md:?}");
    assert_eq!(md.matches("zeroaimemory:begin").count(), 1);
    assert!(!md.contains(&tok));
    let s = p.read(".claude/settings.json");
    assert!(s.starts_with('\u{feff}'), "BOM сохранён: {s:?}");
    let v: Value = serde_json::from_str(s.trim_start_matches('\u{feff}')).unwrap();
    assert_eq!(v["model"], "opus");
    assert!(v["hooks"]["Stop"].is_array(), "{v}");
    // токен из блока v1 перенесён ключом своего адреса (M7 §4.4), адрес — и `url`; совет сменить
    // токен
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(p.home.join(".zeroaimemory/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(tok.as_str()),
        "{home:?}"
    );
    assert_eq!(home["url"].as_str(), Some(url.as_str()), "{home:?}");
    assert!(home.get("token").is_none(), "{home:?}");
    assert!(stdout.contains("выпустите новый токен"), "{stdout}");
    assert!(!stdout.contains(&tok) && !err(&o).contains(&tok));
    // повтор без флагов: адрес — из блока, токен — из домашнего конфига; ничего не меняется
    let before = p.snapshot();
    let o = p.cmd().args(["init", "--yes"]).output().unwrap();
    ok(&o);
    assert_eq!(p.snapshot(), before);
    assert!(!out(&o).contains("выпустите новый токен"));

    // в домашнем конфиге другой токен: перенос из блока его не заменяет
    let other = "zam_000000000000000000000000000000000000000001";
    let cfg = p.home.join(".zeroaimemory/config.toml");
    std::fs::write(&cfg, format!("url = \"{url}\"\ntoken = \"{other}\"\n")).unwrap();
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        format!("<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={url}\nZeroAIMemoryToken={tok}\n<!-- zeroaimemory:end -->\n"),
    )
    .unwrap();
    let o = p.cmd().args(["init", "--yes"]).output().unwrap();
    ok(&o);
    assert!(err(&o).contains("другой токен — не заменён"), "{}", err(&o));
    assert!(std::fs::read_to_string(&cfg).unwrap().contains(other));
    assert!(!p.read("CLAUDE.md").contains(&tok));
    // явный --token заменяет с предупреждением
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert!(
        err(&o).contains("был другой токен — заменён"),
        "{}",
        err(&o)
    );
    let c = std::fs::read_to_string(&cfg).unwrap();
    assert!(c.contains(&tok) && !c.contains(other), "{c}");
}

#[test]
fn mcp_proxy_lets_server_pick_session() {
    let (url, tok) = start("mcp-session");
    let p = Proj::new("mcp-session", &url, &tok);
    let g = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e92";
    std::fs::create_dir_all(p.repo.join(".zeroaimemory")).unwrap();
    std::fs::write(
        p.repo.join(".zeroaimemory/solution.toml"),
        format!("agent_solution_id = \"{g}\"\nname = \"Сессии\"\n"),
    )
    .unwrap();
    assert_eq!(
        api_post(
            &p,
            "/solutions",
            &json!({"agent_solution_id": g, "name": "Сессии"})
        ),
        201
    );
    // сессия хука этой машины и решения — её и выберет сервер (R13)
    assert_eq!(
        api_post_h(
            &p,
            "/sessions",
            &json!({"id": "hook-s", "agent": "claude-code", "solution": g}),
            &[("X-Zam-Machine", "init-box")]
        ),
        201
    );
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "memory_write", "arguments": {"path": "s.md", "content": "# S\n"}}});
    // явная сессия окружения агента прокси не передаётся (fix round 1, I-4)
    let o = p
        .cmd()
        .env("ZAM_URL", &url)
        .env("ZAM_TOKEN", &tok)
        .env("ZAM_SESSION", "explicit-s")
        .env("ZAM_AGENT", "1")
        .arg("mcp")
        .write_stdin(format!("{call}\n"))
        .output()
        .unwrap();
    ok(&o);
    let r: Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(r["result"]["isError"], false, "{r}");
    let (code, body) = api_get(&p, "/sessions/hook-s");
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["memory_writes"], 1, "{v}");
    assert_eq!(api_get(&p, "/sessions/explicit-s").0, 404);
}

/// Поддельный сервер MCP: на каждое соединение — заранее заданный сырой ответ HTTP.
fn fake_server(response: &'static [u8]) -> String {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            // заголовки и тело запроса (Content-Length)
            let mut buf = Vec::new();
            let mut b = [0u8; 4096];
            let head_end = loop {
                let n = c.read(&mut b).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&b[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let n = c.read(&mut b).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&b[..n]);
            }
            let _ = c.write_all(response);
            let _ = c.flush();
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{addr}")
}

#[test]
fn mcp_proxy_answers_every_request_on_transport_errors() {
    let p = Proj::new(
        "mcp-errors",
        "http://127.0.0.1:9",
        "zam_000000000000000000000000000000000000000000",
    );
    let run = |url: &str, stdin: Vec<u8>| -> Vec<Value> {
        let o = p
            .cmd()
            .env("ZAM_URL", url)
            .env("ZAM_TOKEN", &p.token)
            .arg("mcp")
            .write_stdin(stdin)
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .unwrap();
        ok(&o);
        out(&o)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    };
    let req = |id: i64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"});
    let err_of = |r: &[Value], id: Value| -> String {
        let e = r
            .iter()
            .find(|v| v["id"] == id)
            .unwrap_or_else(|| panic!("нет ответа на {id}: {r:?}"));
        assert_eq!(e["error"]["code"], -32000, "{e}");
        e["error"]["message"].as_str().unwrap().to_string()
    };
    // SSE без ответа на запрос
    let sse = fake_server(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n: ping\n\nevent: x\n\n",
    );
    let r = run(&sse, format!("{}\n", req(7)).into_bytes());
    assert!(err_of(&r, json!(7)).contains("без ответа"), "{r:?}");
    // тело короче Content-Length (обрыв)
    let cut = fake_server(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{\"jsonrpc\"",
    );
    let r = run(&cut, format!("{}\n", req(8)).into_bytes());
    assert!(err_of(&r, json!(8)).contains("чтение ответа"), "{r:?}");
    // сервер недоступен: пачка — ошибка на каждый id; не UTF-8 — −32700, работа продолжается
    // строка длиннее 4 МиБ — −32600, работа продолжается (fix round 3, N-5)
    let mut stdin = vec![b'x'; 4 * 1024 * 1024 + 10];
    stdin.push(b'\n');
    stdin.extend_from_slice(b"\xff\xfe\n");
    stdin.extend(
        format!(
            "{}\n",
            json!([req(1), {"jsonrpc": "2.0", "method": "n"}, req(2)])
        )
        .into_bytes(),
    );
    let r = run("http://127.0.0.1:9", stdin);
    assert_eq!(r.len(), 4, "{r:?}");
    assert_eq!(r[0]["error"]["code"], -32600, "{r:?}");
    assert_eq!(r[1]["error"]["code"], -32700, "{r:?}");
    err_of(&r, json!(1));
    err_of(&r, json!(2));
}

#[test]
fn init_both_tokens_differ_and_stale_tmp_removed() {
    let (url, tok) = start("both-tokens");
    let p = Proj::new("both-tokens", &url, &tok);
    let other = "zam_000000000000000000000000000000000000000002";
    let block = |t: &str| {
        format!(
            "<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={url}\nZeroAIMemoryToken={t}\n<!-- zeroaimemory:end -->\n"
        )
    };
    std::fs::write(p.repo.join("CLAUDE.md"), block(&tok)).unwrap();
    std::fs::write(p.repo.join("AGENTS.md"), block(other)).unwrap();
    // временный файл прерванного запуска (fix round 3, N-4)
    std::fs::create_dir_all(p.repo.join(".claude")).unwrap();
    let stale = p.repo.join(".claude/.settings.json.zam-init.999.tmp");
    std::fs::write(&stale, "{").unwrap();
    let keep = p.repo.join(".claude/.settings.json.mine.tmp");
    std::fs::write(&keep, "чужой").unwrap();
    let o = p
        .cmd()
        .args(["init", "--agent", "both", "--yes"])
        .output()
        .unwrap();
    ok(&o);
    assert!(err(&o).contains("разные токены"), "{}", err(&o));
    let home = std::fs::read_to_string(p.home.join(".zeroaimemory/config.toml")).unwrap();
    assert!(home.contains(&tok) && !home.contains(other), "{home}");
    for f in ["CLAUDE.md", "AGENTS.md"] {
        let t = p.read(f);
        assert!(!t.contains(&tok) && !t.contains(other), "{f}: {t}");
    }
    assert!(!stale.exists(), "временный файл zam init удалён");
    assert!(keep.exists(), "чужой файл не тронут");
}

/// Симлинк на файл `link → target`; `false`, если создать нельзя (Windows без режима
/// разработчика или прав администратора) — тогда тест пропускается.
fn symlink_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_file(target, link);
    match r {
        Ok(()) => true,
        Err(e) => {
            eprintln!("симлинк не создан ({e}) — тест пропущен");
            false
        }
    }
}

/// Симлинк AGENTS.md → CLAUDE.md остаётся ссылкой, пишется целевой файл (fix round 3, N-3;
/// fix round 4 — и на Windows, если симлинк можно создать).
#[test]
fn init_keeps_symlink_and_writes_target() {
    let (url, tok) = start("symlink");
    let p = Proj::new("symlink", &url, &tok);
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        "# свой текст
",
    )
    .unwrap();
    if !symlink_file(Path::new("CLAUDE.md"), &p.repo.join("AGENTS.md")) {
        return;
    }
    let o = p.init(&["--agent", "codex", "--yes"], "");
    ok(&o);
    let meta = std::fs::symlink_metadata(p.repo.join("AGENTS.md")).unwrap();
    assert!(meta.file_type().is_symlink(), "ссылка сохранена");
    let t = p.read("CLAUDE.md");
    assert!(
        t.starts_with(&format!("# свой текст\n\n{}", begin())),
        "{t}"
    );
}

/// Симлинк AGENTS.md → файл вне проекта (`~/.bashrc`): отказ с кодом 1 даже с `--yes`,
/// внешний файл и проект не изменились, решение не зарегистрировано (fix round 4, F1).
#[test]
fn init_refuses_symlink_outside_project() {
    let (url, tok) = start("symlink-out");
    let p = Proj::new("symlink-out", &url, &tok);
    let bashrc = p.home.join(".bashrc");
    std::fs::write(
        &bashrc,
        "export A=1
",
    )
    .unwrap();
    if !symlink_file(&bashrc, &p.repo.join("AGENTS.md")) {
        return;
    }
    let before = p.snapshot();
    let o = p.init(&["--agent", "codex", "--yes"], "");
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    let e = err(&o);
    assert!(
        e.contains("AGENTS.md") && e.contains("за пределы проекта") && e.contains(".bashrc"),
        "{e}"
    );
    assert_eq!(
        std::fs::read_to_string(&bashrc).unwrap(),
        "export A=1
"
    );
    assert_eq!(p.snapshot(), before, "ничего не записано");
    let (_, body) = api_get(&p, "/solutions");
    assert!(
        !body.contains("\"repo\""),
        "решение не зарегистрировано: {body}"
    );
}

/// Висячий симлинк AGENTS.md: отказ, ссылка не заменена обычным файлом (fix round 4, F3).
#[test]
fn init_refuses_dangling_symlink() {
    let (url, tok) = start("symlink-dangling");
    let p = Proj::new("symlink-dangling", &url, &tok);
    if !symlink_file(Path::new("missing.md"), &p.repo.join("AGENTS.md")) {
        return;
    }
    let o = p.init(&["--agent", "codex", "--yes"], "");
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("AGENTS.md") && e.contains("висячая"), "{e}");
    let meta = std::fs::symlink_metadata(p.repo.join("AGENTS.md")).unwrap();
    assert!(meta.file_type().is_symlink(), "ссылка осталась ссылкой");
    assert!(!p.repo.join("missing.md").exists());
}

/// Наши обработчики события в настройках: `(matcher, вид хука)`.
fn ours_in(s: &Value, ev: &str) -> Vec<(String, String)> {
    s["hooks"][ev]
        .as_array()
        .map(|g| {
            g.iter()
                .flat_map(|g| {
                    let m = g["matcher"].as_str().unwrap_or("").to_string();
                    g["hooks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(move |h| {
                            let cmd = h["command"].as_str()?;
                            let kind = if cmd == "zam" {
                                h["args"][1].as_str()?.to_string()
                            } else {
                                cmd.strip_prefix("zam hook ")?
                                    .split_whitespace()
                                    .next()?
                                    .to_string()
                            };
                            Some((m.clone(), kind))
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// M4.5 §11: `zam init` пишет новые события (Claude — `PostToolUse` успеха, `SubagentStart`,
/// `pre-tool` на `Agent|Task`; Codex — `PostToolUse` `Bash|apply_patch`, `SubagentStart`);
/// повторный запуск ничего не меняет.
#[test]
fn init_merges_new_events_idempotent() {
    let (url, tok) = start("m45-events");
    let p = Proj::new("m45-events", &url, &tok);
    let o = p.init(&["--agent", "both", "--yes"], "");
    ok(&o);
    let s = p.json(".claude/settings.json");
    assert_eq!(
        ours_in(&s, "PostToolUse"),
        [(
            "Read|Write|Edit|MultiEdit|NotebookEdit|Bash|mcp__zeroaimemory__memory_log".to_string(),
            "post-tool".to_string()
        )]
    );
    assert_eq!(s["hooks"]["PostToolUse"][0]["hooks"][0]["timeout"], 1);
    assert_eq!(
        ours_in(&s, "SubagentStart"),
        [(String::new(), "subagent-start".to_string())]
    );
    assert_eq!(
        ours_in(&s, "PreToolUse"),
        [(
            "Write|Edit|MultiEdit|NotebookEdit|Bash|Agent|Task".to_string(),
            "pre-tool".to_string()
        )]
    );
    assert_eq!(
        ours_in(&s, "PreCompact"),
        [(String::new(), "checkpoint".to_string())]
    );
    let h = p.json(".codex/hooks.json");
    assert_eq!(
        ours_in(&h, "PostToolUse"),
        [("Bash|apply_patch".to_string(), "post-tool".to_string())]
    );
    assert_eq!(
        ours_in(&h, "SubagentStart"),
        [(String::new(), "subagent-start".to_string())]
    );
    assert_eq!(
        h["hooks"]["SubagentStart"][0]["hooks"][0]["command"],
        "zam hook subagent-start --agent codex"
    );
    // повторный запуск — те же файлы байт в байт
    let before = p.snapshot();
    let o = p.init(&["--agent", "both", "--yes"], "");
    ok(&o);
    assert!(out(&o).contains("Файлы проекта актуальны"), "{}", out(&o));
    assert_eq!(p.snapshot(), before);
}

/// Matcher `PreToolUse` прежней сборки M4.5 (`Write|Edit|Bash|Agent|Task`, без `MultiEdit` и
/// `NotebookEdit`; финальное ревью M4.5, M-3) заменяется новым, а не дублируется; повторный
/// запуск ничего не меняет.
#[test]
fn init_updates_pre_tool_matcher() {
    let (url, tok) = start("m45-matcher");
    let p = Proj::new("m45-matcher", &url, &tok);
    ok(&p.init(&["--agent", "claude", "--yes"], ""));
    let path = p.repo.join(".claude/settings.json");
    let mut s = p.json(".claude/settings.json");
    s["hooks"]["PreToolUse"][0]["matcher"] = json!("Write|Edit|Bash|Agent|Task");
    std::fs::write(&path, s.to_string()).unwrap();
    ok(&p.init(&["--agent", "claude", "--yes"], ""));
    let s = p.json(".claude/settings.json");
    assert_eq!(
        ours_in(&s, "PreToolUse"),
        [(
            "Write|Edit|MultiEdit|NotebookEdit|Bash|Agent|Task".to_string(),
            "pre-tool".to_string()
        )],
        "{s}"
    );
    let before = p.snapshot();
    let o = p.init(&["--agent", "claude", "--yes"], "");
    ok(&o);
    assert!(out(&o).contains("Файлы проекта актуальны"), "{}", out(&o));
    assert_eq!(p.snapshot(), before);
}

/// Хуки и блок M4 (v2) заменяются текущим набором (`AGENT_BLOCK_VERSION`): прежние обработчики `zam hook`
/// удаляются, а не дублируются; чужие обработчики остаются на местах.
#[test]
fn init_replaces_v2_handlers() {
    let (url, tok) = start("m45-v2");
    let p = Proj::new("m45-v2", &url, &tok);
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        format!("# Проект\n\n<!-- zeroaimemory:begin v2 -->\nZeroAIMemoryUrl={url}\nстарые правила\n<!-- zeroaimemory:end -->\n"),
    )
    .unwrap();
    let z = |kind: &str, t: u32| json!({"type": "command", "command": "zam", "args": ["hook", kind], "timeout": t});
    // набор M4 (v2) и чужие обработчики рядом
    let v2 = json!({"hooks": {
        "SessionStart": [{"hooks": [z("session-start", 2)]}],
        "UserPromptSubmit": [{"hooks": [z("prompt", 2)]}],
        "PreToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": "my-lint"}]},
            {"matcher": "Write|Edit|Bash", "hooks": [z("pre-tool", 1)]}
        ],
        "PostToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "fmt"}, z("post-tool", 2)]}],
        "PostToolUseFailure": [{"matcher": "Bash", "hooks": [z("post-tool", 2)]}],
        "Stop": [{"hooks": [z("checkpoint", 2)]}],
        "PreCompact": [{"hooks": [z("checkpoint", 2)]}]
    }});
    std::fs::create_dir_all(p.repo.join(".claude")).unwrap();
    std::fs::write(p.repo.join(".claude/settings.json"), v2.to_string()).unwrap();
    std::fs::create_dir_all(p.repo.join(".codex")).unwrap();
    std::fs::write(
        p.repo.join(".codex/hooks.json"),
        json!({"hooks": {
            "PostToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "zam hook post-tool --agent codex", "timeout": 2}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "zam hook checkpoint --agent codex", "timeout": 2}]}]
        }})
        .to_string(),
    )
    .unwrap();
    let o = p.init(&["--agent", "both", "--yes"], "");
    ok(&o);
    let md = p.read("CLAUDE.md");
    assert!(
        md.starts_with(&format!("# Проект\n\n{}\n", begin())),
        "{md}"
    );
    assert!(!md.contains("старые правила"), "{md}");
    let s = p.json(".claude/settings.json");
    for (ev, want) in [
        ("SessionStart", vec![(String::new(), "session-start")]),
        ("UserPromptSubmit", vec![(String::new(), "prompt")]),
        (
            "PreToolUse",
            vec![(
                "Write|Edit|MultiEdit|NotebookEdit|Bash|Agent|Task".to_string(),
                "pre-tool",
            )],
        ),
        (
            "PostToolUse",
            vec![(
                "Read|Write|Edit|MultiEdit|NotebookEdit|Bash|mcp__zeroaimemory__memory_log"
                    .to_string(),
                "post-tool",
            )],
        ),
        (
            "PostToolUseFailure",
            vec![("Bash".to_string(), "post-tool")],
        ),
        ("SubagentStart", vec![(String::new(), "subagent-start")]),
        ("Stop", vec![(String::new(), "checkpoint")]),
        ("PreCompact", vec![(String::new(), "checkpoint")]),
    ] {
        let want: Vec<(String, String)> =
            want.into_iter().map(|(m, k)| (m, k.to_string())).collect();
        assert_eq!(ours_in(&s, ev), want, "{ev}: {s}");
    }
    // чужие обработчики — на своих местах
    let pre = s["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre[0]["hooks"][0]["command"], "my-lint", "{s}");
    let post = s["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post[0]["matcher"], "Edit", "{s}");
    assert_eq!(post[0]["hooks"].as_array().unwrap().len(), 1, "{s}");
    assert_eq!(post[0]["hooks"][0]["command"], "fmt", "{s}");
    let h = p.json(".codex/hooks.json");
    assert_eq!(
        ours_in(&h, "PostToolUse"),
        [("Bash|apply_patch".to_string(), "post-tool".to_string())]
    );
    assert_eq!(h["hooks"]["Stop"].as_array().unwrap().len(), 1, "{h}");
    assert_eq!(
        ours_in(&h, "SubagentStart"),
        [(String::new(), "subagent-start".to_string())]
    );
    // повторный запуск идемпотентен
    let before = p.snapshot();
    ok(&p.init(&["--agent", "both", "--yes"], ""));
    assert_eq!(p.snapshot(), before);
}

/// git в каталоге `dir` с потолками тестов (не поднимается до репозитория ZeroAIMemory), своим
/// автором и без подписи коммитов — как в `tests/code.rs`.
fn git(dir: &Path, args: &[&str]) -> Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=zam",
            "-c",
            "user.email=zam@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .env("GIT_CEILING_DIRECTORIES", common::ceilings())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap()
}

/// git с обязательным кодом 0.
fn git_ok(dir: &Path, args: &[&str]) {
    let o = git(dir, args);
    assert!(o.status.success(), "git {args:?}: {}", err(&o));
}

/// Сколько в тексте строк, равных `line`.
fn lines_eq(text: &str, line: &str) -> usize {
    text.lines().filter(|l| *l == line).count()
}

/// Настоящий репозиторий git в проекте вместо пустого `.git`: `info/exclude` и `check-ignore`
/// — его, а не репозитория ZeroAIMemory.
fn git_init(p: &Proj) {
    let _ = std::fs::remove_dir_all(p.repo.join(".git"));
    let o = git(&p.repo, &["init", "-q"]);
    assert!(o.status.success(), "git init: {}", err(&o));
}

/// `.git/info/exclude` проекта (нет — пусто).
fn exclude(p: &Proj) -> String {
    std::fs::read_to_string(p.repo.join(".git/info/exclude")).unwrap_or_default()
}

/// Исключён ли путь проекта из git (`git check-ignore -q`).
fn git_ignored(p: &Proj, rel: &str) -> bool {
    git(&p.repo, &["check-ignore", "-q", rel]).status.success()
}

/// Домашний конфиг `zam` проекта теста.
fn home_cfg(p: &Proj) -> PathBuf {
    p.home.join(".zeroaimemory").join("config.toml")
}

/// Свой шаблон блока `claude.ru` администратора: вход логином и паролем, `PATCH /admin/settings`.
fn admin_template(p: &Proj, tpl: &str) {
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    let mut r = a
        .post(&format!("{}/api/v1/auth/login", p.url))
        .content_type("application/json")
        .send(json!({"login": "admin", "password": "pw"}).to_string())
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "вход администратора");
    let cookie = r
        .headers()
        .get("set-cookie")
        .and_then(|c| c.to_str().ok())
        .and_then(|c| c.split(';').next())
        .expect("cookie сессии администратора")
        .to_string();
    let v: Value = serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap();
    let csrf = v["csrf"].as_str().unwrap().to_string();
    let st = a
        .patch(&format!("{}/api/v1/admin/settings", p.url))
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", &csrf)
        .content_type("application/json")
        .send(json!({"agent_setup": {"templates": {"claude.ru": tpl}}}).to_string())
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 200, "шаблон блока");
}

/// M6.6 §10.1 (замечание 1): `--url-to home` — блок без строки адреса, адрес — в домашнем
/// конфиге (токен и свои ключи там не трогаются), `zam status` без флагов находит сервер.
#[test]
fn init_url_home_block_without_url() {
    let (url, tok) = start("url-home");
    let p = Proj::new("url-home", &url, &tok);
    let cfg = home_cfg(&p);
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg,
        format!("token = \"{tok}\"\n\n[hooks]\nprompt = false\n"),
    )
    .unwrap();
    // токен — из окружения: в домашний конфиг пишется только адрес
    let o = p
        .cmd()
        .env("ZAM_TOKEN", &tok)
        .args(["init", "--url", &url, "--url-to", "home", "--yes"])
        .output()
        .unwrap();
    ok(&o);
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with(&format!("{}\n", begin())), "{md}");
    assert!(md.contains("## Память ZeroAIMemory"), "{md}");
    assert!(
        !md.contains("ZeroAIMemoryUrl") && !md.contains(&url),
        "адрес в блоке: {md}"
    );
    for f in [
        ".mcp.json",
        ".claude/settings.json",
        ".zeroaimemory/solution.toml",
    ] {
        assert!(!p.read(f).contains(&url), "{f}");
    }
    // ключ без адреса и тот же токен (`ZAM_TOKEN`) — ключ этого адреса (M7 §4.4, круг правок 1)
    let home: toml::Table = toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(home["url"].as_str(), Some(url.as_str()));
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(tok.as_str()),
        "{home:?}"
    );
    assert!(home.get("token").is_none(), "{home:?}");
    assert_eq!(home["hooks"]["prompt"].as_bool(), Some(false), "свои ключи");
    assert!(out(&o).contains("--url-to home"), "{}", out(&o));
    assert!(!err(&o).contains("окажется в репозитории"), "{}", err(&o));
    // zam без флагов: адрес — из домашнего конфига
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    let s = out(&o);
    assert!(s.contains(&format!("Сервер:   {url}")), "{s}");
    assert!(s.contains("Источник: ~/.zeroaimemory/config.toml"), "{s}");
    assert!(
        s.contains(&format!("Блок инструкции: v{V} (актуален)")),
        "{s}"
    );
}

/// `--url-to local`: блок без адреса, адрес — строкой в `CLAUDE.local.md` (свой текст файла
/// сохраняется), файл — в `info/exclude` после подтверждения (отказ — предупреждение);
/// домашний адрес не пишется; повтор без флага ничего не меняет; Codex — предупреждение: агент
/// Codex `CLAUDE.local.md` не читает, а `zam` и хуки адрес находят (финальное ревью M6.6, M2).
#[test]
fn init_url_local_writes_claude_local_and_exclude() {
    let (url, tok) = start("url-local");
    let p = Proj::new("url-local", &url, &tok);
    git_init(&p);
    std::fs::write(p.repo.join("CLAUDE.local.md"), "# мои заметки\n").unwrap();
    let ex0 = exclude(&p);
    // файлы агента — да, исключение — нет
    let o = p.init(&["--url-to", "local"], "y\nn\n");
    ok(&o);
    assert_eq!(
        p.read("CLAUDE.local.md"),
        format!("# мои заметки\nZeroAIMemoryUrl={url}\n")
    );
    let md = p.read("CLAUDE.md");
    assert!(
        !md.contains("ZeroAIMemoryUrl") && !md.contains(&url),
        "адрес в блоке: {md}"
    );
    assert!(p.repo.join(".mcp.json").is_file(), "файлы агента записаны");
    assert_eq!(exclude(&p), ex0, "исключение не подтверждено");
    assert!(err(&o).contains("CLAUDE.local.md не внесён"), "{}", err(&o));
    assert!(!git_ignored(&p, "CLAUDE.local.md"));
    // --yes — без вопроса; ожидание не зависит от того, создал ли git init файл exclude
    let o = p.init(&["--url-to", "local", "--yes"], "");
    ok(&o);
    let ex = exclude(&p);
    assert!(ex.starts_with(&ex0), "{ex}");
    assert_eq!(lines_eq(&ex, "/CLAUDE.local.md"), 1, "{ex}");
    assert!(git_ignored(&p, "CLAUDE.local.md"));
    assert!(out(&o).contains("--url-to local"), "{}", out(&o));
    assert!(
        out(&o).contains("CLAUDE.local.md внесён в .git/info/exclude."),
        "{}",
        out(&o)
    );
    // токен — в домашнем конфиге ключом своего адреса (M7 §4.4), адреса машины там нет
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(home_cfg(&p)).unwrap()).unwrap();
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(tok.as_str()),
        "{home:?}"
    );
    assert!(
        home.get("url").is_none() && home.get("token").is_none(),
        "{home:?}"
    );
    // повтор без флага: режим сохранён, ничего не меняется, строка исключения одна
    let before = p.snapshot();
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert_eq!(p.snapshot(), before);
    assert_eq!(exclude(&p), ex);
    // zam без флагов: адрес — из CLAUDE.local.md
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    let s = out(&o);
    assert!(
        s.contains(&format!("Сервер:   {url}")) && s.contains("CLAUDE.local.md"),
        "{s}"
    );
    // Codex: агент CLAUDE.local.md не читает и адреса в контексте не видит, `zam` и хуки
    // находят — и у одного Codex, и у обоих; причина про AGENTS.md у Claude Code была неверной
    for agent in ["codex", "both"] {
        let o = p.init(&["--url-to", "local", "--agent", agent, "--yes"], "");
        ok(&o);
        assert!(
            err(&o).contains("CLAUDE.local.md агент Codex не читает и адреса сервера в контексте")
                && err(&o).contains("zam, хуки и zam mcp адрес находят"),
            "{agent}: {}",
            err(&o)
        );
        assert!(!err(&o).contains("Claude Code не читает"), "{}", err(&o));
        assert!(!p.read("AGENTS.md").contains(&url));
        // адрес находит и `zam` (`locate::resolve` от агента не зависит)
        let o = p.cmd().arg("status").output().unwrap();
        ok(&o);
        assert!(
            out(&o).contains(&format!("Сервер:   {url}")) && out(&o).contains("CLAUDE.local.md"),
            "{agent}: {}",
            out(&o)
        );
    }
}

/// `--url-to local` (ревью задачи 10, I1; решение координатора, круг 2): `CLAUDE.local.md` в
/// индексе git — отказ (код 1) до записи файлов и регистрации решения: адрес попал бы в
/// репозиторий; правило «!» в `.gitignore` сильнее `info/exclude` — после записи git повторно
/// проверяет, итог — предупреждение; строка уже есть — без вопроса и без повторной записи.
#[test]
fn init_url_local_tracked_or_negated_not_reported_excluded() {
    let (url, tok) = start("url-local-tracked");
    let p = Proj::new("url-local-tracked", &url, &tok);
    git_init(&p);
    // CLAUDE.local.md закоммичен (руководство писало «обычно не коммитится», а не «никогда»)
    std::fs::write(p.repo.join("CLAUDE.local.md"), "# мои\n").unwrap();
    git_ok(&p.repo, &["add", "CLAUDE.local.md"]);
    git_ok(&p.repo, &["commit", "-q", "-m", "local"]);
    let ex0 = exclude(&p);
    let before = p.snapshot();
    let o = p.init(&["--url-to", "local"], "y\n");
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    let (s, e) = (out(&o), err(&o));
    assert!(
        e.contains(
            "CLAUDE.local.md в индексе git — адрес попал бы в репозиторий: git rm --cached \
             CLAUDE.local.md или zam init --url-to home"
        ),
        "{e}"
    );
    assert!(!s.contains("Внести") && !s.contains("внесён"), "{s}");
    assert_eq!(p.snapshot(), before, "файлы проекта не тронуты");
    assert_eq!(p.read("CLAUDE.local.md"), "# мои\n");
    assert_eq!(exclude(&p), ex0, "исключение не писалось");
    let (_, body) = api_get(&p, "/solutions");
    assert!(
        !body.contains("\"repo\""),
        "решение не зарегистрировано: {body}"
    );
    // снят с индекса, но .gitignore возвращает его правилом «!»
    git_ok(&p.repo, &["rm", "--cached", "-q", "CLAUDE.local.md"]);
    std::fs::write(p.repo.join(".gitignore"), "!/CLAUDE.local.md\n").unwrap();
    let o = p.init(&["--url-to", "local", "--yes"], "");
    ok(&o);
    let (s, e) = (out(&o), err(&o));
    assert!(!s.contains("внесён"), "{s}");
    assert!(
        e.contains(
            "строка /CLAUDE.local.md дописана в .git/info/exclude, но git по-прежнему не исключает"
        ),
        "{e}"
    );
    let ex = exclude(&p);
    assert_eq!(lines_eq(&ex, "/CLAUDE.local.md"), 1, "{ex}");
    // строка уже есть: без вопроса (stdin пуст) и без второй строки
    let o = p.init(&["--url-to", "local"], "");
    ok(&o);
    assert!(!out(&o).contains("Внести"), "{}", out(&o));
    assert!(
        err(&o).contains("строка /CLAUDE.local.md уже есть в .git/info/exclude"),
        "{}",
        err(&o)
    );
    assert_eq!(exclude(&p), ex);
    // без правила «!» — исключён, итог молчит
    std::fs::remove_file(p.repo.join(".gitignore")).unwrap();
    let o = p.init(&["--url-to", "local", "--yes"], "");
    ok(&o);
    assert!(git_ignored(&p, "CLAUDE.local.md"));
    assert!(!err(&o).contains("CLAUDE.local.md"), "{}", err(&o));
    assert_eq!(exclude(&p), ex);
}

/// Проект в подкаталоге репозитория со спецсимволом шаблонов gitignore в имени (`sub[1]`) и в
/// связанном worktree (ревью задачи 10, I1 и M6): строка исключения — с экранированным
/// префиксом, в общем `info/exclude` основного репозитория, и действует в обоих рабочих копиях.
#[test]
fn init_url_local_subdir_in_worktree_escaped_prefix() {
    let (url, tok) = start("url-local-wt");
    let root = tmp("url-local-wt");
    let (main, wt, home) = (root.join("main"), root.join("wt"), root.join("home"));
    std::fs::create_dir_all(main.join("sub[1]/.zeroaimemory")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(main.join("sub[1]/.zeroaimemory/keep"), "x\n").unwrap();
    git_ok(&main, &["init", "-q"]);
    git_ok(&main, &["add", "sub[1]/.zeroaimemory/keep"]);
    git_ok(&main, &["commit", "-q", "-m", "init"]);
    git_ok(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let proj = wt.join("sub[1]");
    let ex_path = main.join(".git/info/exclude");
    let ex0 = std::fs::read_to_string(&ex_path).unwrap_or_default();
    let o = common::zam_cmd()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(&proj)
        .args(["init", "--url", &url, "--token", &tok])
        .args(["--url-to", "local", "--yes"])
        .output()
        .unwrap();
    ok(&o);
    assert!(out(&o).contains("CLAUDE.local.md внесён в "), "{}", out(&o));
    let ex = std::fs::read_to_string(&ex_path).unwrap();
    assert!(ex.starts_with(&ex0), "{ex}");
    assert_eq!(lines_eq(&ex, "/sub\\[1\\]/CLAUDE.local.md"), 1, "{ex}");
    assert!(
        std::fs::read_to_string(proj.join("CLAUDE.local.md"))
            .unwrap()
            .contains(&format!("ZeroAIMemoryUrl={url}")),
    );
    for dir in [&proj, &main.join("sub[1]")] {
        let o = git(dir, &["check-ignore", "-q", "CLAUDE.local.md"]);
        assert!(o.status.success(), "{}: не исключён", dir.display());
    }
}

/// Переход с `--url-to local` на `home` или `block` (ревью задачи 10, M1): строка адреса уходит
/// из `CLAUDE.local.md` (файл только с ней — удаляется, свой текст остаётся), иначе она важнее
/// и повтор без флага выбрал бы `local`; повтор держит новый режим.
#[test]
fn init_url_local_to_home_or_block_drops_local_line() {
    let (url, tok) = start("url-local-switch");
    let p = Proj::new("url-local-switch", &url, &tok);
    let local = p.repo.join("CLAUDE.local.md");
    // local → home: файл, созданный zam init, удаляется
    ok(&p.init(&["--url-to", "local", "--yes"], ""));
    assert_eq!(
        p.read("CLAUDE.local.md"),
        format!("ZeroAIMemoryUrl={url}\n")
    );
    let o = p.init(&["--url-to", "home", "--yes"], "");
    ok(&o);
    assert!(!local.exists(), "CLAUDE.local.md удалён");
    let s = out(&o);
    assert!(s.contains("удалён   CLAUDE.local.md"), "{s}");
    assert!(
        s.contains("CLAUDE.local.md удалён: кроме строки адреса"),
        "{s}"
    );
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(home_cfg(&p)).unwrap()).unwrap();
    assert_eq!(home["url"].as_str(), Some(url.as_str()));
    let before = p.snapshot();
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert!(out(&o).contains("--url-to home"), "{}", out(&o));
    assert_eq!(p.snapshot(), before);
    // local → block: свой текст файла остаётся, строки адреса нет
    std::fs::write(&local, "# мои\n").unwrap();
    ok(&p.init(&["--url-to", "local", "--yes"], ""));
    let o = p.init(&["--url-to", "block", "--yes"], "");
    ok(&o);
    assert_eq!(p.read("CLAUDE.local.md"), "# мои\n");
    assert!(
        out(&o).contains("Строка ZeroAIMemoryUrl убрана из CLAUDE.local.md (--url-to block)"),
        "{}",
        out(&o)
    );
    assert!(
        p.read("CLAUDE.md")
            .contains(&format!("ZeroAIMemoryUrl={url}\n"))
    );
    let before = p.snapshot();
    ok(&p.init(&["--yes"], ""));
    assert_eq!(p.snapshot(), before);
}

/// Переход `local` → `block`, когда в итоговом блоке нет строки адреса (ревью задачи 10, круг 2,
/// N2): строка в `CLAUDE.local.md` остаётся с предупреждением, иначе адрес потерялся бы. Шаблон
/// без `{url}` сервер не принимает (`validate_template` — 400), поэтому случай — `{url}` не
/// строкой `ZeroAIMemoryUrl=`: такой адрес `zam` из блока не читает.
#[test]
fn init_url_local_to_block_keeps_line_without_block_url() {
    let (url, tok) = start("url-local-keep");
    let p = Proj::new("url-local-keep", &url, &tok);
    ok(&p.init(&["--url-to", "local", "--yes"], ""));
    admin_template(
        &p,
        "## Память\nСервер памяти — {url}.\nРешение: {solution}.\n",
    );
    // токен — из окружения: домашний конфиг адреса не получит
    let o = p
        .cmd()
        .env("ZAM_TOKEN", &tok)
        .args(["init", "--url", &url, "--url-to", "block", "--yes"])
        .output()
        .unwrap();
    ok(&o);
    assert_eq!(
        p.read("CLAUDE.local.md"),
        format!("ZeroAIMemoryUrl={url}\n")
    );
    assert!(
        err(&o).contains(
            "строка ZeroAIMemoryUrl в CLAUDE.local.md оставлена: в блоке нет строки \
             ZeroAIMemoryUrl="
        ),
        "{}",
        err(&o)
    );
    assert!(
        !out(&o).contains("убрана") && !out(&o).contains("удалён"),
        "{}",
        out(&o)
    );
    // адрес не потерян: zam без флагов находит сервер по строке CLAUDE.local.md
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    assert!(out(&o).contains(&format!("Сервер:   {url}")), "{}", out(&o));
}

/// Переход с `local`, когда `CLAUDE.local.md` — символическая ссылка (ревью задачи 10, круг 2,
/// N1): ссылка внутри проекта не удаляется — в её цель пишется текст без строки адреса; ссылка
/// за пределы проекта — предупреждение без правки, `zam init` не падает. Без прав на ссылки
/// (Windows без режима разработчика) — SKIP.
#[test]
fn init_url_local_symlink_switch_keeps_link() {
    const NAME: &str = "init_url_local_symlink_switch_keeps_link";
    let (url, tok) = start("url-local-link");
    let p = Proj::new("url-local-link", &url, &tok);
    let link = p.repo.join("CLAUDE.local.md");
    let real = p.repo.join("notes-local.md");
    std::fs::write(&real, "").unwrap();
    if !symlink_file(Path::new("notes-local.md"), &link) {
        eprintln!("SKIP {NAME}: символическую ссылку создать нельзя (нет прав)");
        return;
    }
    ok(&p.init(&["--url-to", "local", "--yes"], ""));
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        format!("ZeroAIMemoryUrl={url}\n")
    );
    let o = p.init(&["--url-to", "home", "--yes"], "");
    ok(&o);
    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink(), "ссылка осталась");
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "",
        "цель — без строки"
    );
    assert!(
        out(&o).contains("Строка ZeroAIMemoryUrl убрана из CLAUDE.local.md"),
        "{}",
        out(&o)
    );
    // ссылка за пределы проекта со строкой адреса: предупреждение и пропуск
    std::fs::remove_file(&link).unwrap();
    let outside = p.home.join("outside-local.md");
    let text = format!("ZeroAIMemoryUrl={url}\n");
    std::fs::write(&outside, &text).unwrap();
    assert!(symlink_file(&outside, &link));
    let o = p.init(&["--url-to", "block", "--yes"], "");
    ok(&o);
    let e = err(&o);
    assert!(
        e.contains("строка ZeroAIMemoryUrl в CLAUDE.local.md оставлена")
            && e.contains("за пределы проекта"),
        "{e}"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), text);
}

/// Своя строка `ZeroAIMemoryUrl=` вне блока в CLAUDE.md при `--url-to home` (ревью задачи 10,
/// M3): текст пользователя не вырезается, но предупреждение — адрес уйдёт в репозиторий.
#[test]
fn init_url_home_warns_own_url_line_outside_block() {
    let (url, tok) = start("url-own-line");
    let p = Proj::new("url-own-line", &url, &tok);
    let own = "ZeroAIMemoryUrl=http://198.51.100.7:7700";
    std::fs::write(p.repo.join("CLAUDE.md"), format!("# Проект\n{own}\n")).unwrap();
    let o = p.init(&["--url-to", "home", "--yes"], "");
    ok(&o);
    assert!(
        err(&o).contains(
            "в CLAUDE.md своя строка ZeroAIMemoryUrl вне блока ZeroAIMemory — адрес сервера \
             уйдёт в репозиторий"
        ),
        "{}",
        err(&o)
    );
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with(&format!("# Проект\n{own}\n")), "{md}");
    assert!(!md.contains(&url), "{md}");
}

/// Потолок поиска: git у `zam init` не поднимается выше `ZAM_CEILING_DIRECTORIES` — проект без
/// своего репозитория внутри чужого (здесь — внешний репозиторий теста) не трогает чужой
/// `info/exclude` (замечание обкатки 5: тест переписал настоящий CLAUDE.md).
#[test]
fn init_url_local_respects_git_ceiling() {
    let (url, tok) = start("url-ceiling");
    let root = tmp("url-ceiling");
    let outer = root.join("outer");
    let proj = outer.join("proj");
    let home = root.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    assert!(git(&outer, &["init", "-q"]).status.success());
    let ex = outer.join(".git/info/exclude");
    let ex0 = std::fs::read_to_string(&ex).unwrap_or_default();
    let mut ceil: Vec<PathBuf> = std::env::split_paths(&common::ceilings()).collect();
    ceil.push(outer.clone());
    let o = common::zam_cmd()
        .env(zam_core::CEILING_ENV, std::env::join_paths(ceil).unwrap())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(&proj)
        .args(["init", "--url", &url, "--token", &tok])
        .args(["--url-to", "local", "--yes"])
        .output()
        .unwrap();
    ok(&o);
    assert!(proj.join("CLAUDE.local.md").is_file());
    assert_eq!(
        std::fs::read_to_string(&ex).unwrap_or_default(),
        ex0,
        "внешний репозиторий не тронут"
    );
}

/// Повтор `zam init` без `--url-to` сохраняет режим: в прежнем блоке нет строки адреса — не
/// `block`, поэтому замена устаревшего блока новой версией адрес в CLAUDE.md не возвращает;
/// `--url-to block` возвращает строку, и дальше повтор держит `block`.
#[test]
fn init_rerun_keeps_url_mode() {
    let (url, tok) = start("url-keep");
    let p = Proj::new("url-keep", &url, &tok);
    ok(&p.init(&["--url-to", "home", "--yes"], ""));
    // блок прежней версии, записанный в режиме home (без адреса)
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        "# Проект\n\n<!-- zeroaimemory:begin v7 -->\n## Память ZeroAIMemory\nстарые правила\n<!-- zeroaimemory:end -->\n",
    )
    .unwrap();
    let o = p.init(&["--yes"], "");
    ok(&o);
    let md = p.read("CLAUDE.md");
    assert!(
        md.starts_with(&format!("# Проект\n\n{}\n", begin())),
        "{md}"
    );
    assert!(!md.contains("старые правила"), "{md}");
    assert!(
        !md.contains("ZeroAIMemoryUrl") && !md.contains(&url),
        "адрес вернулся в блок: {md}"
    );
    assert!(out(&o).contains("--url-to home"), "{}", out(&o));
    let before = p.snapshot();
    ok(&p.init(&["--yes"], ""));
    assert_eq!(p.snapshot(), before);
    // явный block возвращает строку адреса; дальше повтор держит block
    ok(&p.init(&["--url-to", "block", "--yes"], ""));
    assert!(
        p.read("CLAUDE.md")
            .contains(&format!("ZeroAIMemoryUrl={url}\n")),
        "{}",
        p.read("CLAUDE.md")
    );
    let before = p.snapshot();
    ok(&p.init(&["--yes"], ""));
    assert_eq!(p.snapshot(), before);
}

/// Адрес есть в своём шаблоне администратора не только строкой `ZeroAIMemoryUrl=`: в режимах
/// home и local — отказ (код 1) до записи файлов и регистрации решения; в block — как раньше.
#[test]
fn init_url_home_refuses_url_elsewhere_in_template() {
    let (url, tok) = start("url-tpl");
    let p = Proj::new("url-tpl", &url, &tok);
    for tpl in [
        "## Память\nZeroAIMemoryUrl={url}\nWebUI: {url}/ui\nРешение: {solution}.\n",
        "## Память\nСервер памяти — {url}.\nРешение: {solution}.\n",
    ] {
        admin_template(&p, tpl);
        let before = p.snapshot();
        for mode in ["home", "local"] {
            let o = p.init(&["--url-to", mode, "--yes"], "");
            assert_eq!(o.status.code(), Some(1), "{mode}: {}", out(&o));
            let e = err(&o);
            assert!(
                e.contains("CLAUDE.md") && e.contains("шаблон") && e.contains("--url-to block"),
                "{mode}: {e}"
            );
            assert_eq!(p.snapshot(), before, "{mode}: ничего не записано");
        }
    }
    let (_, body) = api_get(&p, "/solutions");
    assert!(
        !body.contains("\"repo\""),
        "решение не зарегистрировано: {body}"
    );
    // block — как раньше: адрес в блоке там, где его поставил шаблон
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert!(
        p.read("CLAUDE.md")
            .contains(&format!("Сервер памяти — {url}.")),
        "{}",
        p.read("CLAUDE.md")
    );
}

/// `--url-to home`, а в домашнем конфиге другой адрес: отказ (код 1) с советом `--url-to local`,
/// ничего не записано; тот же адрес (с «/» в конце) — конфиг не переписывается, свой токен там
/// не заменяется.
#[test]
fn init_url_home_conflicting_home_url() {
    let (url, tok) = start("url-home-conflict");
    let p = Proj::new("url-home-conflict", &url, &tok);
    let cfg = home_cfg(&p);
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "url = \"http://198.51.100.20:7700\"\n").unwrap();
    let before = p.snapshot();
    let o = p.init(&["--url-to", "home", "--yes"], "");
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    let e = err(&o);
    assert!(
        e.contains("http://198.51.100.20:7700") && e.contains("--url-to local"),
        "{e}"
    );
    assert_eq!(p.snapshot(), before, "ничего не записано");
    // тот же адрес: токен zam init — из окружения, в конфиге — свой
    let other = "zam_000000000000000000000000000000000000000003";
    let text = format!("url = \"{url}/\"\ntoken = \"{other}\"\n");
    std::fs::write(&cfg, &text).unwrap();
    let o = p
        .cmd()
        .env("ZAM_TOKEN", &tok)
        .args(["init", "--url", &url, "--url-to", "home", "--yes"])
        .output()
        .unwrap();
    ok(&o);
    assert_eq!(
        std::fs::read_to_string(&cfg).unwrap(),
        text,
        "конфиг тот же"
    );
    assert!(!p.read("CLAUDE.md").contains(&url));
}

/// Режим block в репозитории git: CLAUDE.md не исключён или уже в индексе (исключение тогда не
/// действует) — предупреждение «адрес сервера окажется в репозитории»; исключён или режим home
/// — без него.
#[test]
fn init_block_warns_tracked_claude_md() {
    const W: &str = "адрес сервера окажется в репозитории: zam init --url-to home";
    let (url, tok) = start("url-block-warn");
    let p = Proj::new("url-block-warn", &url, &tok);
    git_init(&p);
    let o = p.init(&["--yes"], "");
    ok(&o);
    let e = err(&o);
    assert!(
        e.contains(&format!("CLAUDE.md не исключён из git — {W}")),
        "{e}"
    );
    assert_eq!(e.matches(W).count(), 1, "{e}");
    let exclude_path = p.repo.join(".git/info/exclude");
    std::fs::write(&exclude_path, "/CLAUDE.md\n").unwrap();
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert!(!err(&o).contains(W), "{}", err(&o));
    // в индексе (`git add -f` при `/CLAUDE.md` в exclude) — исключение не действует
    git_ok(&p.repo, &["add", "-f", "CLAUDE.md"]);
    let o = p.init(&["--yes"], "");
    ok(&o);
    assert!(
        err(&o).contains(&format!("CLAUDE.md не исключён из git — {W}")),
        "{}",
        err(&o)
    );
    git_ok(&p.repo, &["rm", "--cached", "-q", "CLAUDE.md"]);
    std::fs::write(&exclude_path, "").unwrap();
    let o = p.init(&["--url-to", "home", "--yes"], "");
    ok(&o);
    assert!(!err(&o).contains(W), "{}", err(&o));
    assert!(!p.read("CLAUDE.md").contains(&url));
}

/// M7 §4.4: `zam init` второго сервера на той же машине пишет свой ключ `[servers."<адрес>"]` и
/// не трогает ключ первого, предупреждения о замене нет; каждый проект берёт ключ своего адреса.
/// Адреса — двух тестовых серверов на 127.0.0.1 (`zam init` ходит на сервер).
#[test]
fn init_second_server_keeps_first() {
    let (u1, t1) = start("second-srv-1");
    let (u2, t2) = start("second-srv-2");
    let p1 = Proj::new("second-srv-1", &u1, &t1);
    let mut p2 = Proj::new("second-srv-2", &u2, &t2);
    p2.home = p1.home.clone();
    ok(&p1.init(&["--yes"], ""));
    let o = p2.init(&["--yes"], "");
    ok(&o);
    assert!(!err(&o).contains("заменён"), "{}", err(&o));
    assert!(
        out(&o).contains(&format!(
            "Токен сохранён в ~/.zeroaimemory/config.toml [servers.\"{u2}\"]"
        )),
        "{}",
        out(&o)
    );
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(home_cfg(&p1)).unwrap()).unwrap();
    assert_eq!(
        home["servers"][&u1]["token"].as_str(),
        Some(t1.as_str()),
        "{home:?}"
    );
    assert_eq!(
        home["servers"][&u2]["token"].as_str(),
        Some(t2.as_str()),
        "{home:?}"
    );
    assert!(home.get("token").is_none(), "{home:?}");
    // адрес машины — первого сервера: block второго его не заменяет
    assert_eq!(home["url"].as_str(), Some(u1.as_str()), "{home:?}");
    for p in [&p1, &p2] {
        let o = p.cmd().arg("status").output().unwrap();
        ok(&o);
        assert!(
            out(&o).contains(&format!(
                "Источник токена: ~/.zeroaimemory/config.toml [servers.\"{}\"]",
                p.url
            )),
            "{}",
            out(&o)
        );
    }
}

/// M7 §4.4: прежний формат домашнего конфига (`url` и `token` верхнего уровня) `zam init --token`
/// для того же адреса переводит в `[servers."<адрес>"]`: верхнего `token` нет, `url` и свои ключи
/// файла сохранены; дальше ключ берётся из `[servers]`.
#[test]
fn init_migrates_legacy_home_config() {
    let (url, tok) = start("legacy-home");
    let p = Proj::new("legacy-home", &url, &tok);
    let cfg = home_cfg(&p);
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let old = ["zam_", &"0".repeat(39), "5"].concat();
    std::fs::write(
        &cfg,
        format!("url = \"{url}/\"\ntoken = \"{old}\"\n\n[hooks]\nprompt = false\n"),
    )
    .unwrap();
    let o = p.init(&["--yes"], "");
    ok(&o);
    let home: toml::Table = toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(tok.as_str()),
        "{home:?}"
    );
    assert!(home.get("token").is_none(), "{home:?}");
    assert_eq!(home["url"].as_str(), Some(format!("{url}/").as_str()));
    assert_eq!(home["hooks"]["prompt"].as_bool(), Some(false), "свои ключи");
    assert!(!std::fs::read_to_string(&cfg).unwrap().contains(&old));
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    assert!(
        out(&o).contains(&format!(
            "Источник токена: ~/.zeroaimemory/config.toml [servers.\"{url}\"]"
        )),
        "{}",
        out(&o)
    );
}

/// M7 §4.4 (решение координатора): ключ без адреса (`token` без `url`, так писал `--url-to local`
/// 0.0.13-m6.6) `zam init` второго сервера с другим ключом оставляет как есть и предупреждает:
/// он больше не используется — команда привязки, маска вместо ключа; `url` рядом с ним не пишется
/// (сделал бы его ключом этого адреса); на адрес без своей записи прежний ключ не уходит.
#[test]
fn init_second_server_leaves_unbound_token() {
    let (url, tok) = start("unbound-home");
    let p = Proj::new("unbound-home", &url, &tok);
    let cfg = home_cfg(&p);
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let old = ["zam_", &"6".repeat(40)].concat();
    std::fs::write(&cfg, format!("token = \"{old}\"\n")).unwrap();
    let o = p.init(&["--yes"], "");
    ok(&o);
    let e = err(&o);
    assert!(
        e.contains("ключ без адреса")
            && e.contains("больше не используется")
            && e.contains("привяжите: zam init --url"),
        "{e}"
    );
    assert!(e.contains(&zam_core::mask_token(&old)), "{e}");
    assert!(!e.contains(&old) && !out(&o).contains(&old));
    assert!(!e.contains("заменён"), "{e}");
    let home: toml::Table = toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(home["token"].as_str(), Some(old.as_str()), "{home:?}");
    assert!(home.get("url").is_none(), "{home:?}");
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(tok.as_str()),
        "{home:?}"
    );
    // прежний ключ больше не используется: адрес без своей записи — отказ с подсказкой
    let o = p
        .cmd()
        .args(["status", "--url", "http://192.0.2.9:7700"])
        .output()
        .unwrap();
    let e = err(&o);
    assert_eq!(o.status.code(), Some(1), "{}{e}", out(&o));
    assert!(
        e.contains("zam init --url http://192.0.2.9:7700 --token") && e.contains("ключ без адреса"),
        "{e}"
    );
    // предупреждение не теряется (круг правок 1): повтор без `--token` и повтор с тем же
    // `--token` (без записи) — та же строка
    let before = p.snapshot();
    for o in [
        p.cmd().args(["init", "--yes"]).output().unwrap(),
        p.init(&["--yes"], ""),
    ] {
        ok(&o);
        let e = err(&o);
        assert!(
            e.contains("ключ без адреса") && e.contains("больше не используется"),
            "{e}"
        );
        assert!(!e.contains(&old), "{e}");
    }
    assert_eq!(p.snapshot(), before);
}

/// M7 §4.4 (круг правок 1): `--url-to home` при ключе без адреса T1 — ключ без адреса не
/// становится ключом этого адреса молча и не удаляется. Другой токен (`--token` или `ZAM_TOKEN`)
/// — отказ (код 1) до любой записи с маской и советом привязать или удалить; тот же токен
/// (`ZAM_TOKEN=T1`, в конфиге уже есть `[servers]`) — переносится в `[servers."<адрес>"]`.
#[test]
fn init_url_home_unbound_token() {
    let (url, toks) = start_n("url-home-unbound", 2);
    let (t1, t2) = (&toks[0], &toks[1]);
    let p = Proj::new("url-home-unbound", &url, t1);
    let cfg = home_cfg(&p);
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let other = ["zam_", &"9".repeat(40)].concat();
    std::fs::write(
        &cfg,
        format!("token = \"{t1}\"\n\n[servers.\"http://192.0.2.7:7700\"]\ntoken = \"{other}\"\n"),
    )
    .unwrap();
    let before = p.snapshot();
    let home_init = |c: &mut Command, extra: &[&str]| {
        c.args(["init", "--url", &url, "--url-to", "home", "--yes"])
            .args(extra)
            .output()
            .unwrap()
    };
    for o in [
        home_init(&mut p.cmd(), &["--token", t2]),
        home_init(p.cmd().env("ZAM_TOKEN", t2), &[]),
    ] {
        let e = err(&o);
        assert_eq!(o.status.code(), Some(1), "{}{e}", out(&o));
        assert!(
            e.contains("ключ без адреса")
                && e.contains(&zam_core::mask_token(t1))
                && e.contains("удалите строку token"),
            "{e}"
        );
        assert!(!e.contains(t1.as_str()) && !e.contains(t2.as_str()), "{e}");
        assert_eq!(p.snapshot(), before, "файл не изменён");
    }
    let o = home_init(p.cmd().env("ZAM_TOKEN", t1), &[]);
    ok(&o);
    let home: toml::Table = toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert!(home.get("token").is_none(), "{home:?}");
    assert_eq!(home["url"].as_str(), Some(url.as_str()));
    assert_eq!(
        home["servers"][&url]["token"].as_str(),
        Some(t1.as_str()),
        "{home:?}"
    );
    assert_eq!(
        home["servers"]["http://192.0.2.7:7700"]["token"].as_str(),
        Some(other.as_str())
    );
    assert!(!err(&o).contains("ключ без адреса"), "{}", err(&o));
}
