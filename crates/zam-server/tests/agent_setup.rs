//! Подключение агента (§14, §15.5; задача 11 M4, задача 13 M4.5): `GET /agent-setup` (блок v4,
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

/// Фразы, которые блоки v5 (M5.6) и v6 (M5.7) перенесли в протокол `zam guide` ради бюджета
/// 500 токенов (ru и en): v5 — исследование по прошлым опытам, `scope=global` у insight,
/// канонические термины; v6 — «(409 — перечитай)», «заметке — `l0`», «перед работой», «в чате».
/// GUIDv7 с наибольшим числом токенов bge-m3 в подстановке «(`…`)» — 39 (поиск
/// восхождением по всем шестнадцатеричным позициям при фиксированных версии и варианте; у
/// случайных GUIDv7 — 23–36): тест бюджета блока детерминирован (ревью задачи 7 M5.6, I1).
const WORST_GUID: &str = "d6c5a5d9-c4c7-7f6b-a8b6-d8fd4c9fb6f6";

/// v7 (M6 §13) — «; `certainty` меняй с `reason`» и «функция — `путь#Тип::метод`» (в протоколе:
/// «Достоверность … Меняется с `reason`», «Ссылка на символ — `путь#Тип::метод`»).
const MOVED_TO_GUIDE: [&str; 18] = [
    "`certainty` меняй с `reason`",
    "change `certainty` with a `reason`",
    "функция — `путь#Тип::метод`",
    "a function — `path#Type::method`",
    "insight_kinds=[experiment,failure]",
    "scope=global",
    "каноническими терминами",
    "canonical terms",
    "не про проект",
    "not about the project",
    "(409 — перечитай)",
    "(409 — reread)",
    "заметке — `l0`",
    "give notes an `l0`",
    "перед работой",
    "before work",
    "в чате",
    "in chat",
];

/// Блок v7 (M6 §13) в худшем случае теста — не больше 500 токенов bge-m3 (ru и en): строка поиска
/// с `focus`, строка кода без «функция — …», строка `axiom` без «`certainty` меняй …».
#[tokio::test]
async fn agent_setup_block_v7_under_500_tokens_ru_en() {
    let Some(counter) = common::ml::bge_counter() else {
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
                            "agent={agent}&lang={lang}&solution={WORST_GUID}&name={}&rules_write={rules}",
                            enc(name)
                        ),
                    )
                    .await;
                    let block = j["block"].as_str().unwrap();
                    assert!(
                        block.starts_with("<!-- zeroaimemory:begin v7 -->\n"),
                        "{block}"
                    );
                    // строка v7 (M6 §6, §13): когда звать фокус — длинное и дорогая модель
                    let search = if lang == "ru" {
                        "- Перед задачей — `memory_search` → `[cache]` или `memory_read` пачкой \
                         (длинное и дорогая модель — один `focus`) → сводку в `memory_cache_put`.\n"
                    } else {
                        "- Before a task — `memory_search` → `[cache]` or `memory_read` in one \
                         call (long text and an expensive model — one `focus`) → summary into \
                         `memory_cache_put`.\n"
                    };
                    assert!(block.contains(search), "{block}");
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
                    // строка v4 (M5.5): итог для пользователя — отчёт, выводы для себя — insight;
                    // ru и en синхронны (финальное ревью M5.5: en без `<file.md>`)
                    let report = if lang == "ru" {
                        "сравнения — пользователю `zam report`, себе — `insight`."
                    } else {
                        "a comparison — for the user `zam report`, for yourself — `insight`."
                    };
                    assert!(block.contains(report), "{block}");
                    // строка v5 (M5.6) — карта и карточка символа; адрес символа
                    // `путь#Тип::метод` с v7 — в протоколе (раздел «Карта кода»)
                    let code = if lang == "ru" {
                        "- Код — `refs.files`; карта — `zam code map`, символ — `zam code sym`; \
                         новое понятие → `term`.\n"
                    } else {
                        "- Code — `refs.files`; map — `zam code map`, symbol — `zam code sym`; \
                         new concept → `term`.\n"
                    };
                    assert!(block.contains(code), "{block}");
                    // строка v6 (M5.7): идея — отдельный вид, начинать — только после «начать»
                    let idea = if lang == "ru" {
                        "- Идея → `idea`; начинать её — только после ответа человека «начать».\n"
                    } else {
                        "- Idea → `idea`; start it only after the human answers “start”.\n"
                    };
                    assert!(block.contains(idea), "{block}");
                    // строка `axiom` (v7): без «`certainty` меняй с `reason`» — она в протоколе
                    let axiom = if lang == "ru" {
                        "несогласен — `hypothesis`/`theorem` с `contradicts` и скажи ему.\n"
                    } else {
                        "if you disagree — `hypothesis`/`theorem` with `contradicts` and tell \
                         the user.\n"
                    };
                    assert!(block.contains(axiom), "{block}");
                    // перенесены в протокол (M5.6, M5.7, M6): их в блоке нет
                    for moved in MOVED_TO_GUIDE {
                        assert!(!block.contains(moved), "{moved}: {block}");
                    }
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
    assert_eq!(ru["version"], 7);
    assert_eq!(ru["lang"], "ru");
    let b = ru["block"].as_str().unwrap();
    assert!(b.starts_with("<!-- zeroaimemory:begin v7 -->\n"), "{b}");
    assert!(b.ends_with("<!-- zeroaimemory:end -->\n"), "{b}");
    assert!(
        b.contains("ZeroAIMemoryUrl=http://memory.lan:7700\n"),
        "{b}"
    );
    for want in [
        "`axiom` — только по прямому указанию пользователя",
        "`hypothesis`/`theorem` с `contradicts`",
        "`memory_update_entry` с `rev` правит.\n",
        // правило v7 (M6 §13): фокус — для длинного и дорогой модели
        "(длинное и дорогая модель — один `focus`)",
        "`memory_search`",
        "`memory_cache_put`",
        "`memory_vault`",
        "`memory_recall`",
        // правила v3 (M4.5 §11), строки задач и вопросов слиты в v6 (M5.7)
        "- Задача → `task` (claim, `status` с `next_step`); вопрос человеку → `question`, его \
         ответ → `answer`.\n",
        // правило v6 (M5.7): идеи
        "- Идея → `idea`; начинать её — только после ответа человека «начать».\n",
        "новое понятие → `term`.",
        // правило v5 (M5.6): карта и карточка символа (адрес символа с v7 — в протоколе)
        "Код — `refs.files`; карта — `zam code map`, символ — `zam code sym`",
        "после сжатия — `recovery`; субагенту — `brief`",
        // без решения — его создаст `zam init`; Rules — по настройке сервера (закрыты)
        "(создаёт `zam init`)",
        "- Секреты, хосты → `memory_vault`.\n",
    ] {
        assert!(b.contains(want), "{want}: {b}");
    }
    let en = setup(&a, &a.token, "agent=codex&lang=en&rules_write=true").await;
    let b = en["block"].as_str().unwrap();
    for want in [
        "<!-- zeroaimemory:begin v7 -->",
        "`axiom` — only on the user's direct instruction",
        "`hypothesis`/`theorem` with `contradicts`",
        "(long text and an expensive model — one `focus`)",
        "`memory_update_entry` with `rev` edits.\n",
        "- Task → `task` (claim, `status` with `next_step`); question for the human → \
         `question`, their answer → `answer`.\n",
        "- Idea → `idea`; start it only after the human answers “start”.\n",
        "- Secrets, hosts → `memory_vault`. Shared rules go to Rules.\n",
        "new concept → `term`",
        "Code — `refs.files`; map — `zam code map`, symbol — `zam code sym`",
        "after compaction — `recovery`; for a subagent — `brief`",
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
    // протокол рабочей памяти (M4.5 §11)
    for section in [
        "## Задачи и передача работы",
        "## Вопросы человеку",
        "## Словарь",
        "## Решения и противоречия",
        "## Ссылки на файлы",
        "## После сжатия и для субагентов (без хуков)",
        "## Окружение в Vault",
    ] {
        assert!(g.contains(section), "{section}");
    }
    assert!(g.contains("`by: human` — только если человек действительно ответил"));
    assert!(
        g.contains("verified_at") && g.contains("contradicts"),
        "{g}"
    );
    let r = a
        .tok(&a.token, Method::GET, "/api/v1/agent-guide?lang=en", None)
        .await;
    let g = r.text();
    assert!(g.contains("direct instruction"), "{g}");
    for section in [
        "## Tasks and handing work over",
        "## Questions for the human",
        "## Glossary",
        "## Decisions and contradictions",
        "## File references",
        "## After compaction and for subagents (without hooks)",
        "## Environment in Vault",
    ] {
        assert!(g.contains(section), "{section}");
    }
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
        "PreToolUse": [{"matcher": "Write|Edit|MultiEdit|NotebookEdit|Bash|Agent|Task", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "pre-tool"], "timeout": 1}]}],
        "PostToolUse": [{"matcher": "Read|Write|Edit|MultiEdit|NotebookEdit|Bash|mcp__zeroaimemory__memory_log", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "post-tool"], "timeout": 1}]}],
        "PostToolUseFailure": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "zam", "args": ["hook", "post-tool"], "timeout": 2}]}],
        "SubagentStart": cmd("subagent-start", 2),
        "Stop": cmd("checkpoint", 2),
        "PreCompact": cmd("checkpoint", 2),
        "PostCompact": cmd("checkpoint", 2),
        "SessionEnd": cmd("checkpoint", 2),
    }});
    assert_eq!(j["claude_settings"], want);
    assert!(j.get("codex_hooks").is_none(), "{j}");
    // M6 §7.1 (ревью задачи 3, I4): stdio `zam mcp` — общий предок с хуками, привязка к сессии
    assert_eq!(
        j["mcp_json"],
        json!({"mcpServers": {"zeroaimemory": {
            "type": "stdio",
            "command": "zam",
            "args": ["mcp"],
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
    assert_eq!(h["PostToolUse"][0]["matcher"], "Bash|apply_patch");
    assert_eq!(h["PreToolUse"][0]["hooks"][0]["timeout"], 1);
    assert_eq!(h["Stop"][0]["hooks"][0]["timeout"], 2);
    assert!(h.get("PostToolUseFailure").is_none(), "{h}");
    // без решения — без solution_toml; фрагмент .mcp.json от решения не зависит
    assert_eq!(
        j["mcp_json"]["mcpServers"]["zeroaimemory"]["command"],
        "zam"
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
    assert_eq!(s["agent_setup"]["version"], 7);
    assert_eq!(s["agent_setup"]["templates"]["claude.ru"]["custom"], false);
    assert!(
        s["agent_setup"]["templates"]["codex.en"]["text"]
            .as_str()
            .unwrap()
            .contains("{url}")
    );
    assert!(!reminder(&a, 7).await.contains("устарел"));

    // свой шаблон: версия 8, блок — свой текст с маркером v8
    let tpl = "## Память\nZeroAIMemoryUrl={url}\nРешение: {solution}. {rules}\r\n";
    let (st, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": tpl}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["agent_setup"]["version"], 8);
    assert_eq!(s["agent_setup"]["templates"]["claude.ru"]["custom"], true);
    let j = setup(&a, &a.token, "agent=claude").await;
    assert_eq!(j["version"], 8);
    assert_eq!(j["template_custom"], true);
    assert_eq!(
        j["block"],
        "<!-- zeroaimemory:begin v8 -->\n## Память\nZeroAIMemoryUrl=http://memory.lan:7700\n\
         Решение: `.zeroaimemory/solution.toml` (создаёт `zam init`).\n\
         <!-- zeroaimemory:end -->\n"
    );
    // версия одна на сервер: и у других агентов и языков
    assert_eq!(
        setup(&a, &a.token, "agent=codex&lang=en").await["version"],
        8
    );
    // /recall(session_start) сравнивает с той же версией
    let t = reminder(&a, 7).await;
    assert!(
        t.contains("Блок ZeroAIMemory устарел (v7 → v8): обнови через `zam init` или WebUI"),
        "{t}"
    );
    assert!(!reminder(&a, 8).await.contains("устарел"));

    // тот же текст повторно — версия не меняется; другой — растёт; сброс — тоже растёт
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": tpl}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 8);
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": "{url} шаблон-v4-маркер", "agents.en": "{url} en"}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 9, "один запрос — одна версия");
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"claude.ru": null}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 10);
    assert_eq!(s["agent_setup"]["templates"]["claude.ru"]["custom"], false);
    let j = setup(&a, &a.token, "agent=claude").await;
    assert!(
        j["block"]
            .as_str()
            .unwrap()
            .starts_with("<!-- zeroaimemory:begin v10 -->\n## Память ZeroAIMemory\n")
    );
    // сброс шаблона по умолчанию — без изменения версии
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"codex.ru": null}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 10);
    // сохранение текста, совпадающего с действующим (здесь — по умолчанию), версию не меняет
    // (fix round 1, M-4)
    let default_en = s["agent_setup"]["templates"]["codex.en"]["text"].clone();
    let (_, s) = admin_settings(
        &a,
        json!({"agent_setup": {"templates": {"codex.en": default_en}}}),
    )
    .await;
    assert_eq!(s["agent_setup"]["version"], 10);
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
    assert_eq!(e[0]["details"]["version"], 10);
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
    assert_eq!(j["version"], 7);
    assert_eq!(j["version_next"], 8);
    assert_eq!(
        j["block"],
        "<!-- zeroaimemory:begin v8 -->\nПамять: http://memory.lan:7700\n\
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
    assert_eq!(r.json()["version_next"], 7, "{}", r.text());
    // проверка — как у сохранения; версия не меняется
    let r = preview(json!({"agent": "claude", "lang": "ru", "template": "без адреса"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = preview(json!({"agent": "nope", "template": "{url}"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let s = a
        .adm(Method::GET, "/api/v1/admin/settings", None, None)
        .await
        .json();
    assert_eq!(s["agent_setup"]["version"], 7);
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

/// Claude Code (M4.5 §11): `PostToolUse` успеха (чтение, правки, команды, `memory_log` MCP),
/// `pre-tool` и на запуске субагента, `SubagentStart`; прежние события — без изменений.
#[tokio::test]
async fn agent_setup_claude_settings_new_events() {
    let a = api("agent-setup-claude-events", |_| {}).await;
    let j = setup(&a, &a.token, "agent=claude").await;
    let h = &j["claude_settings"]["hooks"];
    let one = |ev: &str| {
        let g = h[ev].as_array().unwrap_or_else(|| panic!("{ev}: {h}"));
        assert_eq!(g.len(), 1, "{ev}: {h}");
        g[0].clone()
    };
    let pre = one("PreToolUse");
    assert_eq!(
        pre["matcher"],
        "Write|Edit|MultiEdit|NotebookEdit|Bash|Agent|Task"
    );
    assert_eq!(pre["hooks"][0]["args"], json!(["hook", "pre-tool"]));
    assert_eq!(pre["hooks"][0]["timeout"], 1);
    let post = one("PostToolUse");
    let m = post["matcher"].as_str().unwrap();
    for tool in [
        "Read",
        "Write",
        "Edit",
        "MultiEdit",
        "NotebookEdit",
        "Bash",
        "mcp__zeroaimemory__memory_log",
    ] {
        assert!(m.split('|').any(|x| x == tool), "{tool}: {m}");
    }
    assert_eq!(post["hooks"][0]["args"], json!(["hook", "post-tool"]));
    assert_eq!(post["hooks"][0]["timeout"], 1);
    let sub = one("SubagentStart");
    assert!(sub.get("matcher").is_none(), "{sub}");
    assert_eq!(sub["hooks"][0]["args"], json!(["hook", "subagent-start"]));
    assert_eq!(sub["hooks"][0]["timeout"], 2);
    // прежние события — без изменений
    assert_eq!(one("PostToolUseFailure")["matcher"], "Bash");
    for (ev, kind) in [
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "prompt"),
        ("PostToolUseFailure", "post-tool"),
        ("Stop", "checkpoint"),
        ("PreCompact", "checkpoint"),
    ] {
        let g = one(ev);
        assert_eq!(g["hooks"][0]["args"], json!(["hook", kind]), "{ev}");
        assert_eq!(g["hooks"][0]["timeout"], 2, "{ev}");
    }
    assert_eq!(h.as_object().unwrap().len(), 10, "{h}");
}

/// Codex (M4.5 §11): `PostToolUse` на `Bash|apply_patch` (ошибка и успех — одна команда),
/// `SubagentStart` с `--agent codex`.
#[tokio::test]
async fn agent_setup_codex_hooks_new_events() {
    let a = api("agent-setup-codex-events", |_| {}).await;
    let j = setup(&a, &a.token, "agent=codex").await;
    let h = &j["codex_hooks"]["hooks"];
    assert_eq!(h["PostToolUse"][0]["matcher"], "Bash|apply_patch");
    assert_eq!(
        h["PostToolUse"][0]["hooks"][0]["command"],
        "zam hook post-tool --agent codex"
    );
    assert_eq!(
        h["SubagentStart"][0]["hooks"][0]["command"],
        "zam hook subagent-start --agent codex"
    );
    assert_eq!(h["SubagentStart"][0]["hooks"][0]["timeout"], 2);
    assert!(h["SubagentStart"][0].get("matcher").is_none(), "{h}");
    assert_eq!(h["PreToolUse"][0]["matcher"], "Bash|apply_patch");
    assert!(h.get("PostToolUseFailure").is_none(), "{h}");
    assert_eq!(h.as_object().unwrap().len(), 8, "{h}");
}

/// Снимок сессии (M6 §8а, задача 17b): Claude Code — `PostCompact` (конспект сжатия) и
/// `SessionEnd` на `zam hook checkpoint` без matcher, `timeout` 2; Codex — `SessionEnd`
/// (`PostCompact` у Codex без конспекта — не подключается).
#[tokio::test]
async fn agent_setup_fragments_have_session_end() {
    let a = api("agent-setup-session-end", |_| {}).await;
    let j = setup(&a, &a.token, "agent=claude").await;
    let h = &j["claude_settings"]["hooks"];
    for ev in ["PostCompact", "SessionEnd"] {
        assert_eq!(
            h[ev],
            json!([{"hooks": [{"type": "command", "command": "zam", "args": ["hook", "checkpoint"], "timeout": 2}]}]),
            "{ev}: {h}"
        );
    }
    let j = setup(&a, &a.token, "agent=codex").await;
    let h = &j["codex_hooks"]["hooks"];
    assert_eq!(
        h["SessionEnd"],
        json!([{"hooks": [{"type": "command", "command": "zam hook checkpoint --agent codex", "timeout": 2}]}]),
        "{h}"
    );
    assert!(h.get("PostCompact").is_none(), "{h}");
}

/// Блок v6 из M5.7 устарел (M6 — строка `focus`, две фразы перенесены в протокол):
/// `/recall(session_start)` напоминает обновить его до v7 — одним уведомлением; более старый
/// блок (v5) — тоже одним, с номером текущей версии; v7 — без напоминания.
#[tokio::test]
async fn block_version_reminder_v6_to_v7() {
    let a = api("agent-setup-v6-v7", |_| {}).await;
    for old in [6, 5] {
        let t = reminder(&a, old).await;
        let want =
            format!("Блок ZeroAIMemory устарел (v{old} → v7): обнови через `zam init` или WebUI");
        assert_eq!(t.matches(want.as_str()).count(), 1, "{t}");
        assert_eq!(t.matches("устарел").count(), 1, "{t}");
    }
    assert!(!reminder(&a, 7).await.contains("устарел"));
    assert_eq!(zam_core::AGENT_BLOCK_VERSION, 7);
    assert_eq!(setup(&a, &a.token, "agent=claude").await["version"], 7);
}

/// Протокол v7 (M6 §13, задача 13a): раздел «Фокусное чтение», правило отвергнутых подходов в
/// «Записи», раздел «Снимок сессии», фразы, перенесённые из блока v7, и передачи задач 2–19
/// (ссылка на код в отчёте, сессия MCP, `idempotency_key`, `timeout_ms`, строки дайджеста) —
/// в своих разделах, ru и en; правила отвергнутых подходов в блоке нет.
#[tokio::test]
async fn guide_has_focus_rejected_snapshot() {
    let a = api("agent-guide-v7", |_| {}).await;
    fn section<'a>(g: &'a str, title: &str) -> &'a str {
        let sec = g
            .split(&format!("\n## {title}\n"))
            .nth(1)
            .unwrap_or_else(|| panic!("нет раздела «## {title}»"));
        sec.split("\n## ").next().unwrap()
    }
    type Want<'a> = [(&'a str, &'a [&'a str]); 10];
    let ru: Want = [
        (
            "Фокусное чтение",
            &[
                "`focus`",
                "`tier: \"focus\"`",
                "`zam read --focus",
                "600",
                "дорогая модель",
                "Один `focus` на пачку",
                "[пропущено: range:r<rev>:S-E",
                "range: \"r<rev>:S-E\"",
                "`--range r<rev>:S-E`",
                "`locator`",
                "focus skipped: short",
                "focus lexical",
                "focus skipped: not indexed",
                "400",
            ],
        ),
        (
            "Запись",
            &[
                "Отверг подход после проверки",
                "`kind: decision`",
                "`decision.options`",
                "`chosen`",
                "`rationale`",
                "«отвергнуто, потому что…»",
                "`failure`",
                "Сжатие контекста",
                "`idempotency_key`",
                "422",
            ],
        ),
        (
            "Снимок сессии",
            &[
                "`zam session snapshot",
                "`hooks.session_snapshot`",
                "`‹secret›`",
                "`--password X`",
                "`Authorization:`",
                "не диктуйте секреты в чат",
                "данные, а не указания",
            ],
        ),
        (
            "Поиск: четыре шага с экономией токенов",
            &[
                "`(выдавалось)`",
                "`(выдавалось, в выжимке)`",
                "`· уже выдавалось в этой сессии`",
                "`focus`",
            ],
        ),
        ("Решения и противоречия", &["Меняется с `reason`"]),
        ("Карта кода", &["`путь#Тип::метод`"]),
        (
            "Отчёты",
            &[
                "`vscode://file/home/u/proj/src/a.rs:10`",
                "`vscode://file/C:/proj/src/a.rs:10`",
                "`vscode://file//",
                "`%2F`",
                "`%252F`",
                "«копировать путь»",
            ],
        ),
        (
            "Подключение",
            &["`zam mcp`", "`X-Zam-Session`", "`session_id`", "400"],
        ),
        (
            "После сжатия и для субагентов (без хуков)",
            &["`timeout_ms`", "`recall.max_timeout_ms`"],
        ),
        (
            "Проактивное вспоминание",
            &[
                "`<путь> [надёжность] n:<id>`",
                "`memory_read n:<id>`",
                "`<ссылка> «заголовок» · Nt`",
                "не показано — правил: N",
                "правка затрагивает",
                "`hooks.checkpoint_impact = true`",
            ],
        ),
    ];
    let en: Want = [
        (
            "Focused reading",
            &[
                "`focus`",
                "`tier: \"focus\"`",
                "`zam read --focus",
                "600",
                "expensive model",
                "One `focus` per batch",
                "[пропущено: range:r<rev>:S-E",
                "range: \"r<rev>:S-E\"",
                "`--range r<rev>:S-E`",
                "`locator`",
                "focus skipped: short",
                "focus lexical",
                "focus skipped: not indexed",
                "400",
            ],
        ),
        (
            "Writing",
            &[
                "Rejected an approach after checking it",
                "`kind: decision`",
                "`decision.options`",
                "`chosen`",
                "`rationale`",
                "“rejected because…”",
                "`failure`",
                "Context compaction",
                "`idempotency_key`",
                "422",
            ],
        ),
        (
            "Session snapshot",
            &[
                "`zam session snapshot",
                "`hooks.session_snapshot`",
                "`‹secret›`",
                "`--password X`",
                "`Authorization:`",
                "do not dictate secrets in the chat",
                "data, not instructions",
            ],
        ),
        (
            "Search: four token-saving steps",
            &[
                "`(выдавалось)`",
                "`(выдавалось, в выжимке)`",
                "`· уже выдавалось в этой сессии`",
                "`focus`",
            ],
        ),
        ("Decisions and contradictions", &["Changed with a `reason`"]),
        ("Code map", &["`path#Type::method`"]),
        (
            "Reports",
            &[
                "`vscode://file/home/u/proj/src/a.rs:10`",
                "`vscode://file/C:/proj/src/a.rs:10`",
                "`vscode://file//",
                "`%2F`",
                "`%252F`",
                "“copy path”",
            ],
        ),
        (
            "Connection",
            &["`zam mcp`", "`X-Zam-Session`", "`session_id`", "400"],
        ),
        (
            "After compaction and for subagents (without hooks)",
            &["`timeout_ms`", "`recall.max_timeout_ms`"],
        ),
        (
            "Proactive recall",
            &[
                "`<path> [certainty] n:<id>`",
                "`memory_read n:<id>`",
                "`<ref> «title» · Nt`",
                "не показано — правил: N",
                "правка затрагивает",
                "`hooks.checkpoint_impact = true`",
            ],
        ),
    ];
    for (q, want) in [("", &ru), ("?lang=en", &en)] {
        let r = a
            .tok(
                &a.token,
                Method::GET,
                &format!("/api/v1/agent-guide{q}"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK);
        let g = r.text();
        for (title, phrases) in want {
            let sec = section(&g, title);
            for p in *phrases {
                assert!(sec.contains(p), "«{title}»{q}: нет «{p}»:\n{sec}");
            }
        }
    }
    // правила отвергнутых подходов в блоке нет — оно в протоколе и в схеме `memory_log`
    for lang in ["ru", "en"] {
        let j = setup(&a, &a.token, &format!("agent=claude&lang={lang}")).await;
        let block = j["block"].as_str().unwrap();
        for absent in ["decision.options", "отвергнут", "rejected", "vscode://"] {
            assert!(!block.contains(absent), "{lang}: {absent}: {block}");
        }
    }
}

/// Протокол `GET /agent-guide` (ru, en) — раздел об отчётах: для кого, когда, ссылки на
/// картинки и файлы, лимиты, MCP base64, агент отчёты не читает.
#[tokio::test]
async fn guide_has_reports_section() {
    let a = api("agent-guide-reports", |_| {}).await;
    let guide = |q: &'static str| {
        let a = &a;
        async move {
            let r = a
                .tok(
                    &a.token,
                    Method::GET,
                    &format!("/api/v1/agent-guide{q}"),
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK);
            r.text()
        }
    };
    let ru = guide("").await;
    let sec = ru.split("## Отчёты\n").nth(1).expect("раздел «## Отчёты»");
    let sec = sec.split("\n## ").next().unwrap();
    for want in [
        "для пользователя",
        "исследования, эксперимента, обучения",
        "`zam report <файл.md>",
        "![кривая](figs/loss.png)",
        "ссылка на каталог",
        "до 64 файлов, всего до 200 МиБ",
        "base64",
        "16 МиБ",
        "Неизменяем",
        "Агент отчёты не читает",
        "`insight`",
    ] {
        assert!(sec.contains(want), "{want}: {sec}");
    }
    let en = guide("?lang=en").await;
    let sec = en
        .split("## Reports\n")
        .nth(1)
        .expect("section «## Reports»");
    let sec = sec.split("\n## ").next().unwrap();
    for want in [
        "for the user",
        "research, an experiment, model training",
        "`zam report <file.md>",
        "up to 64 files, 200 MiB in total",
        "a link to a directory",
        "base64",
        "16 MiB",
        "Immutable",
        "Agents do not read reports",
        "`insight`",
    ] {
        assert!(sec.contains(want), "{want}: {sec}");
    }
}

/// Протокол (M5.6): раздел о карте кода (ru, en) и три фразы, перенесённые из блока v5 ради
/// бюджета 500 токенов, — в своих разделах протокола; в блоке их нет.
#[tokio::test]
async fn guide_has_code_map_section_and_moved_phrases() {
    let a = api("agent-guide-code-map", |_| {}).await;
    let guide = |q: &'static str| {
        let a = &a;
        async move {
            let r = a
                .tok(
                    &a.token,
                    Method::GET,
                    &format!("/api/v1/agent-guide{q}"),
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK);
            r.text()
        }
    };
    /// Раздел `## <title>` протокола до следующего `## `.
    fn section<'a>(g: &'a str, title: &str) -> &'a str {
        let sec = g
            .split(&format!("\n## {title}\n"))
            .nth(1)
            .unwrap_or_else(|| panic!("нет раздела «## {title}»"));
        sec.split("\n## ").next().unwrap()
    }
    let ru = guide("").await;
    let code = section(&ru, "Карта кода");
    for want in [
        "`memory_code`",
        "map",
        "focus",
        "`путь#Тип::метод`",
        "symbol_changed",
        "symbol_removed",
        "`zam code sync",
        "`zam code map",
        "`zam code sym",
        "`zam code refs",
        "`zam code impact",
        "`code_map`",
        "`code.send_signatures",
        "Тела",
        "~N tok",
    ] {
        assert!(code.contains(want), "{want}: {code}");
    }
    assert!(
        section(&ru, "Поиск: четыре шага с экономией токенов")
            .contains("`insight_kinds=[experiment,failure]`")
    );
    assert!(section(&ru, "Запись").contains("`scope=global`"));
    assert!(section(&ru, "Словарь").contains("Пиши каноническими терминами"));
    let en = guide("?lang=en").await;
    let code = section(&en, "Code map");
    for want in [
        "`memory_code`",
        "map",
        "focus",
        "`path#Type::method`",
        "symbol_changed",
        "symbol_removed",
        "`zam code sync",
        "`zam code map",
        "`zam code sym",
        "`zam code refs",
        "`zam code impact",
        "`code_map`",
        "`code.send_signatures",
        "bodies",
        "~N tok",
    ] {
        assert!(code.contains(want), "{want}: {code}");
    }
    assert!(
        section(&en, "Search: four token-saving steps")
            .contains("`insight_kinds=[experiment,failure]`")
    );
    assert!(section(&en, "Writing").contains("`scope=global`"));
    assert!(section(&en, "Glossary").contains("Write with canonical terms"));
    // в блоке перенесённых фраз нет (ru и en)
    for lang in ["ru", "en"] {
        let j = setup(&a, &a.token, &format!("agent=claude&lang={lang}")).await;
        let block = j["block"].as_str().unwrap();
        for moved in MOVED_TO_GUIDE {
            assert!(!block.contains(moved), "{lang}: {moved}: {block}");
        }
    }
}

/// Протокол (M5.7): раздел «Идеи» / «Ideas» (ru, en) и четыре фразы, перенесённые из блока v6
/// ради бюджета 500 токенов, — в своих разделах протокола; в блоке их нет.
#[tokio::test]
async fn guide_has_ideas_section_and_moved_phrases() {
    let a = api("agent-guide-ideas", |_| {}).await;
    let guide = |q: &'static str| {
        let a = &a;
        async move {
            let r = a
                .tok(
                    &a.token,
                    Method::GET,
                    &format!("/api/v1/agent-guide{q}"),
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK);
            r.text()
        }
    };
    fn section<'a>(g: &'a str, title: &str) -> &'a str {
        let sec = g
            .split(&format!("\n## {title}\n"))
            .nth(1)
            .unwrap_or_else(|| panic!("нет раздела «## {title}»"));
        sec.split("\n## ").next().unwrap()
    }
    let ru = guide("").await;
    let ideas = section(&ru, "Идеи");
    for want in [
        "`idea`",
        "`origin: human`",
        "`origin: agent`",
        "### Идеи",
        "action=propose",
        "начать / отложить / отказаться",
        "`option`",
        "`by: human`",
        "`horizon`",
        "Без ответа «начать» не начинай",
        "`k:`",
        "`derived_from`",
        "Knowledge",
        "`ideas.proactive`",
        "`w:`",
    ] {
        assert!(ideas.contains(want), "{want}: {ideas}");
    }
    assert!(section(&ru, "Запись").contains("(409 — перечитай)"));
    assert!(section(&ru, "Запись").contains("заметке — `l0`"));
    assert!(section(&ru, "Задачи и передача работы").contains("перед работой"));
    assert!(section(&ru, "Вопросы человеку").contains("в чате"));
    // решение человека из чата через CLI — с `--channel chat` (ревью задачи 4)
    assert!(ideas.contains("zam idea accept|defer|decline w:<id> --by human --channel chat"));
    assert!(section(&ru, "Вопросы человеку").contains("--by human|agent --channel chat"));
    let en = guide("?lang=en").await;
    let ideas = section(&en, "Ideas");
    for want in [
        "`idea`",
        "`origin: human`",
        "`origin: agent`",
        "### Идеи",
        "action=propose",
        "start / postpone / decline",
        "`option`",
        "`by: human`",
        "`horizon`",
        "Without the answer “start”, do not start",
        "`k:`",
        "`derived_from`",
        "Knowledge",
        "`ideas.proactive`",
        "`w:`",
    ] {
        assert!(ideas.contains(want), "{want}: {ideas}");
    }
    assert!(section(&en, "Writing").contains("(409 — reread)"));
    assert!(section(&en, "Writing").contains("give notes an `l0`"));
    assert!(section(&en, "Tasks and handing work over").contains("before work"));
    assert!(section(&en, "Questions for the human").contains("in chat"));
    assert!(ideas.contains("zam idea accept|defer|decline w:<id> --by human --channel chat"));
    assert!(section(&en, "Questions for the human").contains("--by human|agent --channel chat"));
    for lang in ["ru", "en"] {
        let j = setup(&a, &a.token, &format!("agent=claude&lang={lang}")).await;
        let block = j["block"].as_str().unwrap();
        for moved in MOVED_TO_GUIDE {
            assert!(!block.contains(moved), "{lang}: {moved}: {block}");
        }
    }
}
