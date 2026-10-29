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
    // блок v3 с адресом и решением, без токена
    let md = p.read("CLAUDE.md");
    assert!(md.starts_with("<!-- zeroaimemory:begin v3 -->\n"), "{md}");
    assert!(md.contains(&format!("ZeroAIMemoryUrl={url}\n")), "{md}");
    assert!(md.contains(&guid) && md.contains("«Демо»"), "{md}");
    assert!(!md.contains(&tok), "токен в CLAUDE.md");
    assert!(!p.repo.join("AGENTS.md").exists());
    // хуки Claude Code — exec form
    let s = p.json(".claude/settings.json");
    assert_eq!(
        s["hooks"]["PreToolUse"][0]["matcher"],
        "Write|Edit|Bash|Agent|Task"
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
    assert_eq!(
        home["url"].as_str(),
        Some(url.as_str()),
        "адрес — вместе с токеном"
    );
    assert!(stdout.contains("Токен и адрес сохранены"), "{stdout}");
    assert!(!stdout.contains(">> ~/.profile"), "{stdout}");
    assert!(stdout.contains("ZAM_TOKEN"), "{stdout}");
    assert!(
        stdout.contains("SetEnvironmentVariable('ZAM_TOKEN'"),
        "{stdout}"
    );
    assert!(stdout.contains("export ZAM_TOKEN="), "{stdout}");
    // ZAM_MACHINE в окружении теста нет — подсказка для MCP (финальное ревью M4, M-4)
    assert!(stdout.contains("ZAM_MACHINE"), "{stdout}");
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
        out(&o).contains("Блок инструкции: v3 (актуален)"),
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
        md.contains("<!-- zeroaimemory:begin v3 -->\r\n"),
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
    let o = p.init(&["--agent", "codex", "--lang", "en", "--yes"], "");
    ok(&o);
    let md = p.read("AGENTS.md");
    assert!(
        md.starts_with("<!-- zeroaimemory:begin v3 -->\n## ZeroAIMemory memory\n"),
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
        s.matches("Блок ZeroAIMemory устарел (v1 → v3): обнови через `zam init` или WebUI")
            .count(),
        1,
        "{s}"
    );
    let o = p.cmd().args(["status", "--json"]).output().unwrap();
    ok(&o);
    let j: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        j["agent_block"],
        json!({"version": 1, "current": 3, "outdated": true})
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
    assert!(
        md.starts_with("\u{feff}<!-- zeroaimemory:begin v3 -->\n"),
        "{md:?}"
    );
    assert!(md.ends_with("<!-- zeroaimemory:end -->\nхвост\n"), "{md:?}");
    assert_eq!(md.matches("zeroaimemory:begin").count(), 1);
    assert!(!md.contains(&tok));
    let s = p.read(".claude/settings.json");
    assert!(s.starts_with('\u{feff}'), "BOM сохранён: {s:?}");
    let v: Value = serde_json::from_str(s.trim_start_matches('\u{feff}')).unwrap();
    assert_eq!(v["model"], "opus");
    assert!(v["hooks"]["Stop"].is_array(), "{v}");
    // токен из блока v1 перенесён вместе с адресом; совет сменить токен
    let home: toml::Table =
        toml::from_str(&std::fs::read_to_string(p.home.join(".zeroaimemory/config.toml")).unwrap())
            .unwrap();
    assert_eq!(home["token"].as_str(), Some(tok.as_str()));
    assert_eq!(home["url"].as_str(), Some(url.as_str()));
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
        t.starts_with(
            "# свой текст

<!-- zeroaimemory:begin v3 -->"
        ),
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
            "Write|Edit|Bash|Agent|Task".to_string(),
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

/// Хуки и блок M4 (v2) заменяются набором v3: прежние обработчики `zam hook` удаляются, а не
/// дублируются; чужие обработчики остаются на местах.
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
        md.starts_with("# Проект\n\n<!-- zeroaimemory:begin v3 -->\n"),
        "{md}"
    );
    assert!(!md.contains("старые правила"), "{md}");
    let s = p.json(".claude/settings.json");
    for (ev, want) in [
        ("SessionStart", vec![(String::new(), "session-start")]),
        ("UserPromptSubmit", vec![(String::new(), "prompt")]),
        (
            "PreToolUse",
            vec![("Write|Edit|Bash|Agent|Task".to_string(), "pre-tool")],
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
