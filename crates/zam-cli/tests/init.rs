//! `zam init`, `zam status` (версия блока), `zam mcp` и `zam guide` (§13.2, §14; задача 11 M4)
//! против настоящего zam-server в том же процессе тестов. Проект и «домашний» каталог — во
//! временном каталоге целей сборки (`CARGO_TARGET_TMPDIR`); в проекте есть `.git`, чтобы корень
//! не поднялся до репозитория ZeroAIMemory. Реальные CLAUDE.md, `.claude` и профиль
//! пользователя не трогаются.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-init-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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
        let mut c = Command::cargo_bin("zam").unwrap();
        for k in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
            "ZAM_URL",
            "ZAM_TOKEN",
            "ZAM_SESSION",
            "ZAM_AGENT",
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_CHILD_SESSION",
            "CODEX_SANDBOX",
            "CODEX_SANDBOX_NETWORK_DISABLED",
            "CODEX_THREAD_ID",
        ] {
            c.env_remove(k);
        }
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
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    a.post(&format!("{}/api/v1{path}", p.url))
        .header("Authorization", format!("Bearer {}", p.token))
        .content_type("application/json")
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
    // блок v2 с адресом и решением, без токена
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with("<!-- zeroaimemory:begin v2 -->\n"), "{md}");
    assert!(md.contains(&format!("ZeroAIMemoryUrl={url}\n")), "{md}");
    assert!(md.contains(&guid) && md.contains("«Демо»"), "{md}");
    assert!(!md.contains(&tok), "токен в CLAUDE.md");
    assert!(!p.repo.join("AGENTS.md").exists());
    // хуки Claude Code — exec form
    let s = p.json(".claude/settings.json");
    assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "Write|Edit|Bash");
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
    // .mcp.json — без токена, с решением
    let m = p.json(".mcp.json");
    let z = &m["mcpServers"]["zeroaimemory"];
    assert_eq!(z["type"], "http");
    assert_eq!(z["url"], format!("{url}/mcp"));
    assert_eq!(z["headers"]["Authorization"], "Bearer ${ZAM_TOKEN}");
    assert_eq!(z["headers"]["X-Zam-Solution"], guid.as_str());
    assert!(!p.read(".mcp.json").contains(&tok));
    assert!(!p.read(".claude/settings.json").contains(&tok));
    assert_eq!(p.read(".zeroaimemory/.gitignore"), "cache/\nlogs/\n");
    // токен — в домашнем конфиге; подсказка про ZAM_TOKEN для Windows и Linux
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(p.home.join(".zeroaimemory/config.toml")).unwrap())
            .unwrap();
    assert_eq!(home["token"].as_str(), Some(tok.as_str()));
    assert!(stdout.contains("ZAM_TOKEN"), "{stdout}");
    assert!(
        stdout.contains("SetEnvironmentVariable('ZAM_TOKEN'"),
        "{stdout}"
    );
    assert!(stdout.contains("export ZAM_TOKEN="), "{stdout}");
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
        out(&o).contains("Блок инструкции: v2 (актуален)"),
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
        md.contains("<!-- zeroaimemory:begin v2 -->\r\n"),
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
    assert_eq!(
        m["mcpServers"]["zeroaimemory"]["headers"]["X-Zam-Solution"],
        g
    );

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
    let o = p.init(
        &["--agent", "codex", "--lang", "en", "--no-mcp", "--yes"],
        "",
    );
    ok(&o);
    let md = p.read("AGENTS.md");
    assert!(
        md.starts_with("<!-- zeroaimemory:begin v2 -->\n## ZeroAIMemory memory\n"),
        "{md}"
    );
    assert!(!p.repo.join("CLAUDE.md").exists());
    assert!(!p.repo.join(".claude").exists());
    assert!(!p.repo.join(".mcp.json").exists(), "--no-mcp");
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

#[test]
fn status_warns_outdated_block() {
    let (url, tok) = start("status");
    let p = Proj::new("status", &url, &tok);
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        format!("<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={url}\nZeroAIMemoryToken={tok}\n<!-- zeroaimemory:end -->\n"),
    )
    .unwrap();
    let o = p.cmd().arg("status").output().unwrap();
    ok(&o);
    let s = out(&o);
    assert_eq!(
        s.matches("Блок ZeroAIMemory устарел (v1 → v2): обнови через `zam init` или WebUI")
            .count(),
        1,
        "{s}"
    );
    let o = p.cmd().args(["status", "--json"]).output().unwrap();
    ok(&o);
    let j: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        j["agent_block"],
        json!({"version": 1, "current": 2, "outdated": true})
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
