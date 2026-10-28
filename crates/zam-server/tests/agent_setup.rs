//! Подключение агента (§14, §15.5; задача 11 M4): `GET /agent-setup` (блок инструкции v2,
//! фрагменты `.mcp.json` и хуков, установка `zam`), шаблоны в `settings` с версией,
//! `GET /agent-guide`, `GET /downloads/zam/{os}-{arch}`.
mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::api::{Api, api, enc};
use serde_json::{Value, json};
use zam_markdown::TokenCounter;

/// Заголовок Host запросов (адрес сервера без `server.public_url`).
const HOST: (&str, &str) = ("host", "memory.lan:7700");
/// Длинный внешний адрес для проверки бюджета (IP и порт).
const LONG_URL: &str = "http://192.168.100.200:7700";
/// Длинное имя решения.
const LONG_NAME: &str = "ZeroAIMemory TradingPlatform Core";
/// Длинный внешний адрес: имя хоста, порт и префикс пути.
const VERY_LONG_URL: &str =
    "https://zeroaimemory.engineering-department.example-corp.internal:17700/memory";

async fn setup(a: &Api, tok: &str, q: &str) -> Value {
    let r = a
        .tok_h(
            tok,
            Method::GET,
            &format!("/api/v1/agent-setup?{q}"),
            None,
            &[HOST],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.text());
    r.json()
}

/// Счётчик токенов модели bge-m3 (как у чанкера) из `target/models`: установленная модель или
/// одиночный токенизатор `tokenizers/bge-m3-int8/` (его качает `cargo xtask fetch-models
/// --test`, в том числе в CI); нет — `None` (в CI с `ZAM_REQUIRE_ML` — падение).
fn bge_counter() -> Option<zam_ml::BgeTokenCounter> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/models");
    let tokenizers = dir.join("tokenizers");
    for (d, id) in [
        (&dir, "bge-m3-int8"),
        (&dir, "bge-m3-fp16"),
        (&tokenizers, "bge-m3-int8"),
    ] {
        if let Ok(c) = zam_ml::BgeTokenCounter::load(d, id) {
            return Some(c);
        }
    }
    assert!(
        std::env::var_os("ZAM_REQUIRE_ML").is_none(),
        "нет токенизатора bge-m3: cargo xtask fetch-models --test"
    );
    eprintln!("пропуск: нет токенизатора bge-m3 в target/models");
    None
}

#[tokio::test]
async fn agent_setup_block_under_500_tokens_ru_en() {
    let Some(counter) = bge_counter() else {
        return;
    };
    assert!(counter.is_exact());
    // адрес по IP и имя из 33 символов; длинный адрес с префиксом и кириллическое имя из 128
    // символов — в блоке обрезается до 40 с «…» (fix round 1, I-2)
    let long_name: String = "Платформа алгоритмической торговли и риск-менеджмента "
        .repeat(3)
        .chars()
        .take(128)
        .collect();
    let shown_long: String = long_name.chars().take(40).collect::<String>() + "…";
    let cases = [
        (
            LONG_URL.to_string(),
            LONG_NAME.to_string(),
            LONG_NAME.to_string(),
        ),
        (VERY_LONG_URL.to_string(), long_name.clone(), shown_long),
    ];
    for (i, (url, name, shown)) in cases.iter().enumerate() {
        let a = api(&format!("agent-setup-tokens-{i}"), |c| {
            c.server.public_url = Some(format!("{url}/"));
        })
        .await;
        for lang in ["ru", "en"] {
            for agent in ["claude", "agents", "codex"] {
                for rules in ["true", "false"] {
                    let j = setup(
                        &a,
                        &a.token,
                        &format!(
                            "agent={agent}&lang={lang}&solution=new&name={}&rules_write={rules}",
                            enc(name)
                        ),
                    )
                    .await;
                    let block = j["block"].as_str().unwrap();
                    // подставлены реальные адрес, GUID и имя решения (решение контроллера T11-c)
                    assert!(
                        block.contains(&format!(
                            "ZeroAIMemoryUrl={url}
"
                        )),
                        "{block}"
                    );
                    let guid = j["solution"]["agent_solution_id"].as_str().unwrap();
                    assert!(block.contains(guid), "{block}");
                    assert!(block.contains(shown.as_str()), "{block}");
                    assert_eq!(
                        j["solution"]["name"],
                        name.as_str(),
                        "полное имя — в solution"
                    );
                    let n = counter.count(block);
                    eprintln!("{i}: {agent}.{lang} rules_write={rules}: {n} токенов bge-m3");
                    assert!(
                        n <= 500,
                        "{agent}.{lang}: {n} токенов > 500:
{block}"
                    );
                    assert_eq!(j["warnings"], json!([]), "{j}");
                }
            }
        }
    }
}

#[tokio::test]
async fn agent_setup_contains_axiom_rule_and_rev_rule() {
    let a = api("agent-setup-rules", |_| {}).await;
    let ru = setup(&a, &a.token, "agent=claude").await;
    assert_eq!(ru["version"], 2);
    assert_eq!(ru["lang"], "ru");
    let b = ru["block"].as_str().unwrap();
    assert!(b.starts_with("<!-- zeroaimemory:begin v2 -->\n"), "{b}");
    assert!(b.ends_with("<!-- zeroaimemory:end -->\n"), "{b}");
    assert!(
        b.contains("ZeroAIMemoryUrl=http://memory.lan:7700\n"),
        "{b}"
    );
    for want in [
        "`axiom` ставь, правь, снимай только по прямому указанию пользователя",
        "`hypothesis`/`theorem`",
        "скажи пользователю",
        "Правка записи — с `rev`; на 409/428 перечитай",
        "`memory_search`",
        "`memory_cache_put`",
        "`memory_vault`",
        "`memory_recall`",
        // без решения — его создаст `zam init`; Rules — по настройке сервера (закрыты)
        "(создаёт `zam init`)",
        "Секреты → `memory_vault`. Заметке — `l0`.\n",
    ] {
        assert!(b.contains(want), "{want}: {b}");
    }
    let en = setup(&a, &a.token, "agent=codex&lang=en&rules_write=true").await;
    let b = en["block"].as_str().unwrap();
    for want in [
        "<!-- zeroaimemory:begin v2 -->",
        "`axiom` only on the user's direct instruction",
        "`hypothesis`/`theorem`",
        "Edit a record with `rev`; on 409/428 reread",
        "Shared rules go to Rules.",
    ] {
        assert!(b.contains(want), "{want}: {b}");
    }
    // токена нет нигде в ответе
    for j in [&ru, &en] {
        assert!(!j.to_string().contains(&a.token), "{j}");
        assert!(!j["block"].as_str().unwrap().contains("zam_"), "{j}");
    }
    // неверные параметры — 400; без аутентификации — 401
    for q in [
        "agent=cursor",
        "lang=de",
        "solution=xyz",
        "mcp=maybe",
        "token=zam_x",
    ] {
        let r = a
            .tok_h(
                &a.token,
                Method::GET,
                &format!("/api/v1/agent-setup?{q}"),
                None,
                &[HOST],
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
    }
    let r = a
        .send(
            Request::get("/api/v1/agent-setup")
                .header("host", "memory.lan:7700")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // полный протокол
    let r = a
        .tok(&a.token, Method::GET, "/api/v1/agent-guide", None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("content-type").starts_with("text/markdown"));
    let g = r.text();
    assert!(g.contains("memory_cache_put") && g.contains("axiom"), "{g}");
    let r = a
        .tok(&a.token, Method::GET, "/api/v1/agent-guide?lang=en", None)
        .await;
    assert!(r.text().contains("direct instruction"), "{}", r.text());
}

#[tokio::test]
async fn agent_setup_hooks_exec_form() {
    let a = api("agent-setup-hooks", |_| {}).await;
    let g = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e71";
    let j = setup(
        &a,
        &a.token,
        &format!("agent=claude&solution={g}&name=Demo"),
    )
    .await;
    let cmd = |kind: &str, t: u32| json!([{"hooks": [{"type": "command", "command": "zam", "args": ["hook", kind], "timeout": t}]}]);
    let want = json!({"hooks": {
        "SessionStart": cmd("session-start", 2),
        "UserPromptSubmit": cmd("prompt", 2),
        "PreToolUse": [{"matcher": "Write|Edit|Bash", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "pre-tool"], "timeout": 1}]}],
        "PostToolUseFailure": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "post-tool"], "timeout": 2}]}],
        "Stop": cmd("checkpoint", 2),
        "PreCompact": cmd("checkpoint", 2),
    }});
    assert_eq!(j["claude_settings"], want);
    assert!(j.get("codex_hooks").is_none(), "{j}");
    assert_eq!(
        j["mcp_json"],
        json!({"mcpServers": {"zeroaimemory": {
            "type": "http",
            "url": "http://memory.lan:7700/mcp",
            "headers": {
                "Authorization": "Bearer ${ZAM_TOKEN}",
                "X-Zam-Solution": g,
                "X-Zam-Machine": "${ZAM_MACHINE:-}",
            },
        }}})
    );
    assert_eq!(
        j["solution_toml"],
        format!("agent_solution_id = \"{g}\"\nname = \"Demo\"\n")
    );
    // решения ещё нет в реестре — с именем, в кавычках своей оболочки
    assert_eq!(
        j["init_cmd"]["linux"],
        format!(
            "zam init --url 'http://memory.lan:7700' --agent claude --id {g} --name 'Demo' --yes"
        )
    );
    assert_eq!(
        j["init_cmd"]["windows"],
        format!(
            "zam init --url 'http://memory.lan:7700' --agent claude --id {g} --name 'Demo' --yes"
        )
    );
    let install = &j["install"];
    let windows = install["windows"].as_str().unwrap();
    assert!(
        windows.contains("'http://memory.lan:7700/downloads/zam/windows-x64'")
            && windows.contains("'ZAM_TOKEN', '<токен>'")
            && windows.contains("-notcontains $d")
            && windows.contains("'ZAM_MACHINE', $env:COMPUTERNAME"),
        "{install}"
    );
    let linux = install["linux"].as_str().unwrap();
    assert!(
        linux.contains("'http://memory.lan:7700/downloads/zam/linux-x64'")
            && linux.contains("case \":$PATH:\"")
            && !linux.contains(">> ~/.profile")
            && linux.contains("export ZAM_MACHINE=$(uname -n)"),
        "{install}"
    );

    // имя с подстановками оболочки в однострочник не попадает (fix round 3): zam init возьмёт
    // имя каталога; адрес и токен — в кавычках своей оболочки (fix round 1, I-3)
    for evil in ["x$(id)`whoami`'\"", "a'; rm -rf ~; '", "a\u{2019}b"] {
        let j = setup(&a, &a.token, &format!("solution=new&name={}", enc(evil))).await;
        let id = j["solution"]["agent_solution_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(j["solution"]["name"], evil);
        for os in ["linux", "windows"] {
            assert_eq!(
                j["init_cmd"][os],
                format!("zam init --url 'http://memory.lan:7700' --agent claude --id {id} --yes"),
                "{evil}"
            );
        }
    }
    // допустимое имя с пробелом и кириллицей — в кавычках
    let j = setup(
        &a,
        &a.token,
        &format!("solution=new&name={}", enc("Мой проект v2.0")),
    )
    .await;
    let c = j["init_cmd"]["linux"].as_str().unwrap();
    assert!(c.contains(" --name 'Мой проект v2.0' --yes"), "{c}");
    // решение уже зарегистрировано — только --id
    let r = a
        .t(
            Method::POST,
            "/api/v1/solutions",
            Some(json!({"agent_solution_id": g, "name": "Demo"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = setup(&a, &a.token, &format!("solution={g}")).await;
    for os in ["linux", "windows"] {
        let c = j["init_cmd"][os].as_str().unwrap();
        assert!(
            c.contains(&format!("--id {g}")) && !c.contains("--name"),
            "{c}"
        );
    }
    assert_eq!(j["solution"]["name"], "Demo");

    // Codex: команда строкой (у Codex нет `args`), post-tool — на PostToolUse, apply_patch
    let j = setup(&a, &a.token, "agent=codex").await;
    assert!(j.get("claude_settings").is_none(), "{j}");
    let h = &j["codex_hooks"]["hooks"];
    let line = |ev: &str| {
        h[ev][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(line("SessionStart"), "zam hook session-start --agent codex");
    assert_eq!(line("UserPromptSubmit"), "zam hook prompt --agent codex");
    assert_eq!(line("PreToolUse"), "zam hook pre-tool --agent codex");
    assert_eq!(line("PostToolUse"), "zam hook post-tool --agent codex");
    assert_eq!(line("Stop"), "zam hook checkpoint --agent codex");
    assert_eq!(line("PreCompact"), "zam hook checkpoint --agent codex");
    assert_eq!(h["PreToolUse"][0]["matcher"], "Bash|apply_patch");
    assert_eq!(h["PostToolUse"][0]["matcher"], "Bash");
    assert_eq!(h["PreToolUse"][0]["hooks"][0]["timeout"], 1);
    assert_eq!(h["Stop"][0]["hooks"][0]["timeout"], 2);
    assert!(h.get("PostToolUseFailure").is_none(), "{h}");
    // без решения — без X-Zam-Solution и solution_toml
    assert!(
        j["mcp_json"]["mcpServers"]["zeroaimemory"]["headers"]
            .get("X-Zam-Solution")
            .is_none()
    );
    assert!(j.get("solution_toml").is_none(), "{j}");

    // AGENTS.md без хуков; mcp=false, hooks=false
    let j = setup(&a, &a.token, "agent=agents&mcp=false").await;
    assert!(
        j.get("codex_hooks").is_none() && j.get("mcp_json").is_none(),
        "{j}"
    );
    assert!(
        j["init_cmd"]["linux"]
            .as_str()
            .unwrap()
            .ends_with("--agent codex --no-hooks --no-mcp --yes"),
        "{j}"
    );
    let j = setup(&a, &a.token, "agent=claude&hooks=false").await;
    assert!(j.get("claude_settings").is_none(), "{j}");

    // solution=new — новый GUID; администратор с token= — токен в установке и однострочнике
    let j = setup(&a, &a.token, "solution=new").await;
    let guid = j["solution"]["agent_solution_id"].as_str().unwrap();
    assert!(guid.parse::<zam_core::SolutionGuid>().is_ok(), "{j}");
    assert_eq!(j["solution"]["name"], "Solution");
    let r = a
        .send(
            Request::get(format!("/api/v1/agent-setup?token={}", a.token))
                .header("host", "memory.lan:7700")
                .header("cookie", &a.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    for os in ["linux", "windows"] {
        assert!(
            j["init_cmd"][os]
                .as_str()
                .unwrap()
                .contains(&format!("--token '{}'", a.token))
        );
    }
    assert!(j["install"]["windows"].as_str().unwrap().contains(&a.token));
    assert!(!j["block"].as_str().unwrap().contains(&a.token));
    assert!(!j["mcp_json"].to_string().contains(&a.token));
}

async fn admin_settings(a: &Api, body: Value) -> (StatusCode, Value) {
    let r = a
        .adm(Method::PATCH, "/api/v1/admin/settings", Some(body), None)
        .await;
    let s = r.status;
    (
        s,
        if s == StatusCode::OK {
            r.json()
        } else {
            json!(r.text())
        },
    )
}

async fn reminder(a: &Api, v: u32) -> String {
    let r = a
        .t(
            Method::POST,
            "/api/v1/recall",
            Some(json!({"event": "session_start", "agent_block_version": v, "record": false})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["text"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn template_edit_bumps_version() {
    let a = api("agent-setup-template", |_| {}).await;
    let r = a
        .adm(Method::GET, "/api/v1/admin/settings", None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let s = r.json();
    assert_eq!(s["agent_setup"]["version"], 2);
    assert_eq!(s["agent_setup"]["templates"]["claude.ru"]["custom"], false);
    assert!(
        s["agent_setup"]["templates"]["codex.en"]["text"]
            .as_str()
            .unwrap()
            .contains("{url}")
    );
    assert!(!reminder(&a, 2).await.contains("устарел"));

    // свой шаблон: версия 3, блок — свой текст с маркером v3
    let tpl = "## Память\nZeroAIMemoryUrl={url}\nРешение: {solution}. {rules}\r\n";
    let (st, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": tpl}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["agent_setup"]["version"], 3);
    assert_eq!(s["agent_setup"]["templates"]["claude.ru"]["custom"], true);
    let j = setup(&a, &a.token, "agent=claude").await;
    assert_eq!(j["version"], 3);
    assert_eq!(j["template_custom"], true);
    assert_eq!(
        j["block"],
        "<!-- zeroaimemory:begin v3 -->\n## Память\nZeroAIMemoryUrl=http://memory.lan:7700\n\
         Решение: `.zeroaimemory/solution.toml` (создаёт `zam init`).\n\
         <!-- zeroaimemory:end -->\n"
    );
    // версия одна на сервер: и у других агентов и языков
    assert_eq!(
        setup(&a, &a.token, "agent=codex&lang=en").await["version"],
        3
    );
    // /recall(session_start) сравнивает с той же версией
    let t = reminder(&a, 2).await;
    assert!(
        t.contains("Блок ZeroAIMemory устарел (v2 → v3): обнови через `zam init` или WebUI"),
        "{t}"
    );
    assert!(!reminder(&a, 3).await.contains("устарел"));

    // тот же текст повторно — версия не меняется; другой — растёт; сброс — тоже растёт
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": tpl}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 3);
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": "{url} шаблон-v4-маркер", "agents.en": "{url} en"}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 4, "один запрос — одна версия");
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": null}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 5);
    assert_eq!(s["agent_setup"]["templates"]["claude.ru"]["custom"], false);
    let j = setup(&a, &a.token, "agent=claude").await;
    assert!(
        j["block"]
            .as_str()
            .unwrap()
            .starts_with("<!-- zeroaimemory:begin v5 -->\n## Память ZeroAIMemory\n")
    );
    // сброс шаблона по умолчанию — без изменения версии
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"codex.ru": null}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 5);
    // сохранение текста, совпадающего с действующим (здесь — по умолчанию), версию не меняет
    // (fix round 1, M-4)
    let default_en = s["agent_setup"]["templates"]["codex.en"]["text"].clone();
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"codex.en": default_en}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 5);
    assert_eq!(s["agent_setup"]["templates"]["codex.en"]["custom"], false);

    // неверные шаблоны и ключи — 400, токен — 403
    for bad in [
        json!({"agent_setup": {"templates": {"claude.ru": "без адреса"}}}),
        json!({"agent_setup": {"templates": {"claude.ru": "{url} <!-- zeroaimemory:end -->"}}}),
        json!({"agent_setup": {"templates": {"cursor.ru": "{url}"}}}),
        json!({"agent_setup": {"templates": {"claude.ru": 1}}}),
        json!({"agent_setup": {"x": 1}}),
    ] {
        let (st, _) = admin_settings(&a, bad.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    let r = a
        .t(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"agent_setup": {"templates": {"claude.ru": "{url}"}}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // аудит: ключ, сброс и версия, без текста шаблона
    let r = a
        .adm(Method::GET, "/api/v1/admin/audit?limit=50", None, None)
        .await;
    let items = r.json()["items"].as_array().unwrap().clone();
    let e: Vec<&Value> = items
        .iter()
        .filter(|e| e["target"] == "agent_setup.template.claude.ru")
        .collect();
    assert_eq!(e.len(), 3, "{items:?}");
    assert!(
        !r.text().contains("шаблон-v4-маркер"),
        "текст шаблона в аудите"
    );
    assert_eq!(e[0]["details"]["reset"], true);
    assert_eq!(e[0]["details"]["version"], 5);
}

#[tokio::test]
async fn downloads_zam_serves_binary_or_404() {
    let dir = common::tmp("downloads-bundle");
    let a = api("downloads", |c| c.server.downloads_dir = Some(dir.clone())).await;
    let (own, own_file, other, other_file) = if cfg!(windows) {
        ("windows-x64", "zam.exe", "linux-x64", "zam")
    } else {
        ("linux-x64", "zam", "windows-x64", "zam.exe")
    };
    let get = |p: &str| {
        Request::get(format!("/downloads/zam/{p}"))
            .body(Body::empty())
            .unwrap()
    };
    // нет файлов — 404 (problem+json, не страница WebUI)
    for p in [own, other, "macos-arm64", ""] {
        let r = a.send(get(p)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}");
        assert!(!r.text().contains("<html"), "{p}");
    }
    // свой клиент рядом с сервером, чужой — в downloads/<платформа>/; без аутентификации
    std::fs::write(dir.join(own_file), b"own-binary").unwrap();
    std::fs::create_dir_all(dir.join("downloads").join(other)).unwrap();
    std::fs::write(dir.join("downloads").join(other).join(other_file), b"other").unwrap();
    // файл своей ОС не отдаётся под именем чужой платформы
    std::fs::write(dir.join(other_file), b"wrong").unwrap();
    for (p, body, file) in [(own, "own-binary", own_file), (other, "other", other_file)] {
        let r = a.send(get(p)).await;
        assert_eq!(r.status, StatusCode::OK, "{p}");
        assert_eq!(r.text(), body);
        assert_eq!(r.header("content-type"), "application/octet-stream");
        assert_eq!(
            r.header("content-disposition"),
            format!("attachment; filename=\"{file}\"")
        );
    }
    std::fs::remove_file(dir.join("downloads").join(other).join(other_file)).unwrap();
    assert_eq!(a.send(get(other)).await.status, StatusCode::NOT_FOUND);
}

/// Предпросмотр несохранённого шаблона (задача 13): блок с маркером следующей версии,
/// проверка как у сохранения, только администратор.
#[tokio::test]
async fn template_preview_renders_unsaved() {
    let a = api("agent-setup-preview", |_| {}).await;
    let preview = |body: Value| {
        let a = &a;
        async move {
            let b = common::api::req(
                Method::POST,
                "/api/v1/admin/agent-setup/preview",
                Some(&body),
            )
            .header("cookie", &a.cookie)
            .header("x-csrf-token", &a.csrf)
            .header(HOST.0, HOST.1);
            a.send(b.body(Body::from(body.to_string())).unwrap()).await
        }
    };
    let r = preview(json!({"agent": "claude", "lang": "ru",
                           "template": "Память: {url}\nРешение: {solution}.\r\n"}))
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["version"], 2);
    assert_eq!(j["version_next"], 3);
    assert_eq!(
        j["block"],
        "<!-- zeroaimemory:begin v3 -->\nПамять: http://memory.lan:7700\n\
         Решение: «Пример» (`00000000-0000-0000-0000-000000000000`).\n\
         <!-- zeroaimemory:end -->\n"
    );
    assert!(j["tokens"].as_u64().unwrap() > 0);
    // шаблон по умолчанию без изменений — версия та же
    let s = a
        .adm(Method::GET, "/api/v1/admin/settings", None, None)
        .await
        .json();
    let def = s["agent_setup"]["templates"]["claude.ru"]["text"].clone();
    let r = preview(json!({"agent": "claude", "lang": "ru", "template": def})).await;
    assert_eq!(r.json()["version_next"], 2, "{}", r.text());
    // проверка — как у сохранения; версия не меняется
    let r = preview(json!({"agent": "claude", "lang": "ru", "template": "без адреса"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = preview(json!({"agent": "nope", "template": "{url}"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let s = a
        .adm(Method::GET, "/api/v1/admin/settings", None, None)
        .await
        .json();
    assert_eq!(s["agent_setup"]["version"], 2);
    // токен — 403
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            "/api/v1/admin/agent-setup/preview",
            Some(json!({"template": "{url}"})),
            &[HOST],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

/// Ответ `/agent-setup` не кэшируется (`no-store`); `token_placeholder=true` — `--token '<токен>'`
/// в однострочнике для подстановки в браузере (задача 13 fix round 1, I-1).
#[tokio::test]
async fn agent_setup_no_store_and_token_placeholder() {
    let a = api("agent-setup-no-store", |_| {}).await;
    let r = a
        .tok_h(&a.token, Method::GET, "/api/v1/agent-setup", None, &[HOST])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("cache-control"), "no-store");
    let j = r.json();
    for k in ["linux", "windows"] {
        assert!(
            !j["init_cmd"][k].as_str().unwrap().contains("--token"),
            "{j}"
        );
    }
    assert!(
        j["install"]["windows"]
            .as_str()
            .unwrap()
            .contains("'<токен>'")
    );
    let j = setup(&a, &a.token, "token_placeholder=true").await;
    for k in ["linux", "windows"] {
        let c = j["init_cmd"][k].as_str().unwrap();
        assert!(c.contains(" --token '<токен>' "), "{c}");
    }
}
