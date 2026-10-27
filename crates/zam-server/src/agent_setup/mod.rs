//! Подключение агента (§14, §15.5): блок инструкции для CLAUDE.md / AGENTS.md, фрагменты
//! `.mcp.json`, `.claude/settings.json`, `.codex/hooks.json`, команды установки `zam`.
//!
//! - **Шаблоны** хранятся в `settings` (`agent_setup.template.<agent>.<lang>`, строка; `null` или
//!   нет ключа — шаблон по умолчанию из [`templates`]); их правит администратор
//!   (`PATCH /admin/settings`, WebUI — задача 13).
//! - **Версия блока** — одна на сервер **[решение контроллера T11-a]**:
//!   `max(AGENT_BLOCK_VERSION, agent_setup.version)`; каждое сохранение шаблона, которое меняет
//!   его текст, увеличивает её на 1 ([`bump_version`]). Её отдаёт `GET /agent-setup`
//!   (`version`), с ней сравнивают `/recall(session_start)` и `zam status`.
//! - Токен не попадает ни в блок, ни в `.mcp.json` (`Bearer ${ZAM_TOKEN}`).
pub mod templates;

use serde_json::{Value, json};
use zam_core::AGENT_BLOCK_VERSION;
use zam_store::SystemStore;

use crate::error::ApiError;

/// Ключ сохранённой версии шаблонов.
pub const VERSION_KEY: &str = "agent_setup.version";
/// Префикс ключей шаблонов: `agent_setup.template.<agent>.<lang>`.
pub const TEMPLATE_PREFIX: &str = "agent_setup.template.";
/// Предел размера шаблона, байт.
pub const TEMPLATE_MAX_BYTES: usize = 16 * 1024;
/// Маркеры блока (§14.1).
pub const BEGIN: &str = "<!-- zeroaimemory:begin";
pub const END: &str = "<!-- zeroaimemory:end -->";

/// Для кого блок: Claude Code (CLAUDE.md), AGENTS.md (Codex, Cursor и др.) или Codex
/// (AGENTS.md и хуки Codex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Agents,
    Codex,
}

impl AgentKind {
    pub const ALL: [AgentKind; 3] = [AgentKind::Claude, AgentKind::Agents, AgentKind::Codex];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Agents => "agents",
            AgentKind::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        AgentKind::ALL.into_iter().find(|a| a.as_str() == s)
    }
}

/// Язык инструкции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Ru, Lang::En];

    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }

    pub fn parse(s: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

/// Ключ шаблона в `settings`.
pub fn template_key(agent: AgentKind, lang: Lang) -> String {
    format!("{TEMPLATE_PREFIX}{}.{}", agent.as_str(), lang.as_str())
}

/// Шаблон по умолчанию.
pub fn default_template(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => templates::BLOCK_RU,
        Lang::En => templates::BLOCK_EN,
    }
}

/// Текущая версия блока: `max(AGENT_BLOCK_VERSION, сохранённая)`.
pub async fn current_version(system: &SystemStore) -> Result<u32, ApiError> {
    let saved = system.get_setting::<u32>(VERSION_KEY).await?.unwrap_or(0);
    Ok(saved.max(AGENT_BLOCK_VERSION))
}

/// Сохранение шаблонов (`None` — сброс к шаблону по умолчанию) и рост версии — одной
/// транзакцией (fix round 1, M-4): версия растёт на 1, только если действующий текст хотя бы
/// одного шаблона изменился. Возвращает новую версию и изменённые ключи с признаком сброса.
pub async fn save_templates(
    system: &SystemStore,
    edits: Vec<(AgentKind, Lang, Option<String>)>,
) -> Result<Option<(u32, Vec<(String, bool)>)>, ApiError> {
    let edits = edits
        .into_iter()
        .map(|(a, l, t)| (template_key(a, l), t, default_template(l).to_string()))
        .collect();
    Ok(system
        .set_settings_versioned(edits, VERSION_KEY, AGENT_BLOCK_VERSION)
        .await?)
}

/// Шаблон агента и языка: `(текст, свой ли)`.
pub async fn template(
    system: &SystemStore,
    agent: AgentKind,
    lang: Lang,
) -> Result<(String, bool), ApiError> {
    let saved = system
        .get_setting::<Option<String>>(&template_key(agent, lang))
        .await?
        .flatten();
    Ok(match saved {
        Some(t) => (t, true),
        None => (default_template(lang).to_string(), false),
    })
}

/// Проверка шаблона перед сохранением; возвращает нормализованный текст (LF, без
/// хвостовых пустых строк, с переводом строки в конце).
pub fn validate_template(raw: &str) -> Result<String, String> {
    let t = raw.replace("\r\n", "\n");
    let t = t.trim_end();
    if t.trim().is_empty() {
        return Err("шаблон пуст".into());
    }
    if t.len() > TEMPLATE_MAX_BYTES {
        return Err(format!("шаблон больше {TEMPLATE_MAX_BYTES} байт"));
    }
    if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("в шаблоне управляющие символы".into());
    }
    if t.contains("<!-- zeroaimemory:") {
        return Err("маркеры блока добавляет сервер: уберите <!-- zeroaimemory:… -->".into());
    }
    if !t.contains("{url}") {
        return Err("в шаблоне нет {url}: без адреса сервера zam и агент не найдут память".into());
    }
    Ok(format!("{t}\n"))
}

/// Решение для блока и фрагментов.
#[derive(Debug, Clone)]
pub struct SolutionRef {
    pub guid: String,
    pub name: String,
}

/// Что подставить в шаблон.
pub struct BlockCtx<'a> {
    pub url: &'a str,
    pub solution: Option<&'a SolutionRef>,
    pub rules_write: bool,
    pub lang: Lang,
}

/// Предел длины имени решения в блоке, символов (fix round 1, I-2): длиннее — обрезка с «…»;
/// решение однозначно задаёт GUID рядом.
pub const BLOCK_NAME_MAX_CHARS: usize = 40;

/// Имя решения для блока: не длиннее [`BLOCK_NAME_MAX_CHARS`] символов.
pub fn block_name(name: &str) -> String {
    if name.chars().count() <= BLOCK_NAME_MAX_CHARS {
        return name.to_string();
    }
    let mut s: String = name.chars().take(BLOCK_NAME_MAX_CHARS).collect();
    s.push('…');
    s
}

/// Блок с маркерами версии `version`.
pub fn render_block(template: &str, version: u32, c: &BlockCtx<'_>) -> String {
    let solution = match (c.solution, c.lang) {
        (Some(s), Lang::Ru) => format!("«{}» (`{}`)", block_name(&s.name), s.guid),
        (Some(s), Lang::En) => format!("\"{}\" (`{}`)", block_name(&s.name), s.guid),
        (None, Lang::Ru) => templates::SOLUTION_AUTO_RU.into(),
        (None, Lang::En) => templates::SOLUTION_AUTO_EN.into(),
    };
    let rules = match (c.rules_write, c.lang) {
        (true, Lang::Ru) => templates::RULES_WRITE_RU,
        (false, Lang::Ru) => templates::RULES_READONLY_RU,
        (true, Lang::En) => templates::RULES_WRITE_EN,
        (false, Lang::En) => templates::RULES_READONLY_EN,
    };
    let body = template
        .replace("{url}", c.url)
        .replace("{solution}", &solution)
        .replace("{rules}", rules);
    // пустая подстановка не оставляет пробелов в конце строк
    let body: Vec<&str> = body.trim_end().lines().map(str::trim_end).collect();
    format!("{BEGIN} v{version} -->\n{}\n{END}\n", body.join("\n"))
}

/// Обработчик хука в exec form Claude Code: `command: "zam"` + `args` (без shell).
fn claude_hook(kind: &str, timeout: u32) -> Value {
    json!({"type": "command", "command": "zam", "args": ["hook", kind], "timeout": timeout})
}

/// Обработчик хука Codex: у Codex нет `args` — команда одной строкой (через shell).
fn codex_hook(kind: &str, timeout: u32) -> Value {
    json!({"type": "command", "command": format!("zam hook {kind} --agent codex"), "timeout": timeout})
}

/// Группа хуков события.
fn group(matcher: Option<&str>, hook: Value) -> Value {
    match matcher {
        Some(m) => json!([{"matcher": m, "hooks": [hook]}]),
        None => json!([{"hooks": [hook]}]),
    }
}

/// Фрагмент `.claude/settings.json` (§15.5): exec form, `timeout` в секундах — 1 у
/// `pre-tool`, 2 у остальных.
pub fn claude_settings() -> Value {
    let h = claude_hook;
    json!({"hooks": {
        "SessionStart": group(None, h("session-start", 2)),
        "UserPromptSubmit": group(None, h("prompt", 2)),
        "PreToolUse": group(Some("Write|Edit|Bash"), h("pre-tool", 1)),
        "PostToolUseFailure": group(Some("Bash"), h("post-tool", 2)),
        "Stop": group(None, h("checkpoint", 2)),
        "PreCompact": group(None, h("checkpoint", 2)),
    }})
}

/// Фрагмент `.codex/hooks.json`: тот же состав с `--agent codex`; `post-tool` — на
/// `PostToolUse` (у Codex нет `PostToolUseFailure`), правки файлов Codex — инструмент
/// `apply_patch` (matcher — regex) **[решение контроллера T11-b]**.
pub fn codex_hooks() -> Value {
    let h = codex_hook;
    json!({"hooks": {
        "SessionStart": group(None, h("session-start", 2)),
        "UserPromptSubmit": group(None, h("prompt", 2)),
        "PreToolUse": group(Some("Bash|apply_patch"), h("pre-tool", 1)),
        "PostToolUse": group(Some("Bash"), h("post-tool", 2)),
        "Stop": group(None, h("checkpoint", 2)),
        "PreCompact": group(None, h("checkpoint", 2)),
    }})
}

/// Фрагмент `.mcp.json`: токен — из переменной `ZAM_TOKEN`, машина — из `COMPUTERNAME`
/// (Windows; пусто — сервер берёт последнюю сессию пользователя без учёта машины, R13).
pub fn mcp_json(url: &str, solution: Option<&SolutionRef>) -> Value {
    let mut headers = json!({
        "Authorization": "Bearer ${ZAM_TOKEN}",
        "X-Zam-Machine": "${COMPUTERNAME:-}",
    });
    if let Some(s) = solution {
        headers["X-Zam-Solution"] = json!(s.guid);
    }
    json!({"mcpServers": {"zeroaimemory": {
        "type": "http",
        "url": format!("{url}/mcp"),
        "headers": headers,
    }}})
}

/// Строка TOML в кавычках (имя проверено: без управляющих символов).
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `.zeroaimemory/solution.toml` (§11.1).
pub fn solution_toml(s: &SolutionRef) -> String {
    format!(
        "agent_solution_id = {}\nname = {}\n",
        toml_str(&s.guid),
        toml_str(&s.name)
    )
}

/// Аргумент в одинарных кавычках POSIX shell: `'` → `'\''` (fix round 1, I-3).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Аргумент в одинарных кавычках PowerShell: каждая одинарная кавычка удваивается — `'` и
/// типографские U+2018, U+2019, U+201A, U+201B, которые PowerShell тоже считает кавычками
/// (fix round 1, I-3; fix round 3).
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Имя решения годится для `--name` однострочника (fix round 3): 1–128 символов из букв,
/// цифр, пробела, `.`, `_`, `-` (`^[\p{L}\p{N} ._-]{1,128}$`), не с `-` (иначе `zam` прочтёт
/// его как флаг; fix round 4, F5). Иное имя в однострочник не
/// попадает — `zam init` возьмёт имя каталога, переименование — `zam solution rename`;
/// экранирование остаётся второй защитой.
pub fn safe_cli_name(name: &str) -> bool {
    let n = name.chars().count();
    (1..=128).contains(&n)
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_alphabetic() || c.is_numeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// Команды установки `zam` с сервера (`GET /downloads/zam/{os}-{arch}`) (fix round 1, M-1, M-2):
/// каталог в `Path` пользователя Windows добавляется один раз; на Linux — подсказка, если
/// `~/.local/bin` нет в `PATH`. Токен `zam` и хуки берут из `~/.zeroaimemory/config.toml`
/// (его пишет `zam init --token`, права 0600); `ZAM_TOKEN` нужна только MCP по HTTP — на
/// Windows она задаётся в окружении пользователя (`token` — открытый текст только при выпуске
/// токена администратором, иначе заглушка).
pub fn install(url: &str, token: Option<&str>) -> Value {
    let t = ps_quote(token.unwrap_or("<токен>"));
    let win_url = ps_quote(&format!("{url}/downloads/zam/windows-x64"));
    let windows = format!(
        "$d = \"$env:LOCALAPPDATA\\Programs\\zam\"; New-Item -ItemType Directory -Force $d | Out-Null\n\
         Invoke-WebRequest {win_url} -OutFile \"$d\\zam.exe\"\n\
         $p = [Environment]::GetEnvironmentVariable('Path', 'User'); if (($p -split ';') -notcontains $d) {{ [Environment]::SetEnvironmentVariable('Path', $(if ($p) {{ \"$p;$d\" }} else {{ $d }}), 'User') }}\n\
         # MCP по HTTP (.mcp.json) берёт токен из ZAM_TOKEN; zam и хуки — из ~/.zeroaimemory/config.toml (zam init --token)\n\
         [Environment]::SetEnvironmentVariable('ZAM_TOKEN', {t}, 'User')\n"
    );
    let lin_url = sh_quote(&format!("{url}/downloads/zam/linux-x64"));
    let linux = format!(
        "mkdir -p ~/.local/bin && curl -fsSL {lin_url} -o ~/.local/bin/zam && chmod +x ~/.local/bin/zam\n\
         case \":$PATH:\" in *\":$HOME/.local/bin:\"*) ;; *) echo 'Добавьте ~/.local/bin в PATH' ;; esac\n\
         # токен: zam init --token сохранит его в ~/.zeroaimemory/config.toml (права 0600); MCP по HTTP\n\
         # (.mcp.json) берёт его из ZAM_TOKEN — задайте её в окружении агента в файле, доступном только вам\n"
    );
    json!({"windows": windows, "linux": linux})
}

/// Параметры однострочника `zam init`.
pub struct InitCmd<'a> {
    pub url: &'a str,
    pub token: Option<&'a str>,
    pub agent: AgentKind,
    pub solution: Option<&'a SolutionRef>,
    /// Решение уже есть в реестре: только `--id` (имя берётся с сервера и из `solution.toml`).
    pub registered: bool,
    pub hooks: bool,
    pub mcp: bool,
    pub lang: Lang,
}

/// Однострочник `zam init …` (§15.5, п. 6) для оболочки с правилом кавычек `quote`.
/// AGENTS.md без хуков — `--agent codex --no-hooks`.
fn init_line(c: &InitCmd<'_>, quote: fn(&str) -> String) -> String {
    let mut s = format!("zam init --url {}", quote(c.url));
    if let Some(t) = c.token {
        s.push_str(&format!(" --token {}", quote(t)));
    }
    let agent = match c.agent {
        AgentKind::Claude => "claude",
        AgentKind::Agents | AgentKind::Codex => "codex",
    };
    s.push_str(&format!(" --agent {agent}"));
    if let Some(sol) = c.solution {
        s.push_str(&format!(" --id {}", sol.guid));
        if !c.registered && safe_cli_name(&sol.name) {
            s.push_str(&format!(" --name {}", quote(&sol.name)));
        }
    }
    if !c.hooks || c.agent == AgentKind::Agents {
        s.push_str(" --no-hooks");
    }
    if !c.mcp {
        s.push_str(" --no-mcp");
    }
    if c.lang != Lang::Ru {
        s.push_str(&format!(" --lang {}", c.lang.as_str()));
    }
    s.push_str(" --yes");
    s
}

/// Однострочники `zam init` для Linux (POSIX shell) и Windows (PowerShell).
pub fn init_cmd(c: &InitCmd<'_>) -> Value {
    json!({"linux": init_line(c, sh_quote), "windows": init_line(c, ps_quote)})
}

/// Полный протокол (`GET /agent-guide`).
pub fn guide(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => templates::GUIDE_RU,
        Lang::En => templates::GUIDE_EN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sol() -> SolutionRef {
        SolutionRef {
            guid: "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11".into(),
            name: "Trading \"X\"".into(),
        }
    }

    #[test]
    fn block_substitutes_and_wraps_markers() {
        let s = sol();
        for lang in Lang::ALL {
            let b = render_block(
                default_template(lang),
                7,
                &BlockCtx {
                    url: "http://h:1",
                    solution: Some(&s),
                    rules_write: false,
                    lang,
                },
            );
            assert!(b.starts_with("<!-- zeroaimemory:begin v7 -->\n"), "{b}");
            assert!(b.ends_with("\n<!-- zeroaimemory:end -->\n"), "{b}");
            assert!(b.contains("ZeroAIMemoryUrl=http://h:1\n"), "{b}");
            assert!(b.contains(&s.guid), "{b}");
            assert!(!b.contains('{'), "подстановки не остались: {b}");
        }
    }

    #[test]
    fn template_validation() {
        assert!(validate_template("  \n").is_err());
        assert!(validate_template("без адреса").is_err());
        assert!(validate_template("{url}\n<!-- zeroaimemory:end -->").is_err());
        assert!(validate_template("{url}\u{7}").is_err());
        assert_eq!(
            validate_template("a {url}\r\nb\r\n\r\n").unwrap(),
            "a {url}\nb\n"
        );
    }

    #[test]
    fn solution_toml_escapes() {
        let t = solution_toml(&sol());
        let v: toml::Table = toml::from_str(&t).unwrap();
        assert_eq!(v["name"].as_str(), Some("Trading \"X\""));
    }

    #[test]
    fn init_cmd_maps_agents_to_codex_without_hooks() {
        let s = sol();
        let c = init_cmd(&InitCmd {
            url: "http://h:1",
            token: None,
            agent: AgentKind::Agents,
            solution: Some(&s),
            registered: false,
            hooks: true,
            mcp: false,
            lang: Lang::En,
        });
        assert_eq!(
            c["linux"],
            "zam init --url 'http://h:1' --agent codex --id 0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11 \
             --no-hooks --no-mcp --lang en --yes",
            "имя с кавычками в однострочник не попадает"
        );
        let s = SolutionRef {
            name: "Trading X".into(),
            ..sol()
        };
        let c = init_cmd(&InitCmd {
            url: "http://h:1",
            token: None,
            agent: AgentKind::Claude,
            solution: Some(&s),
            registered: false,
            hooks: true,
            mcp: true,
            lang: Lang::Ru,
        });
        assert!(
            c["windows"]
                .as_str()
                .unwrap()
                .contains(" --name 'Trading X' --yes"),
            "{c}"
        );
    }

    #[test]
    fn shell_quoting() {
        let n = "x$(id)`whoami`'\"";
        assert_eq!(sh_quote(n), r#"'x$(id)`whoami`'\''"'"#);
        assert_eq!(ps_quote(n), r#"'x$(id)`whoami`''"'"#);
        assert_eq!(
            ps_quote("a\u{2019}b\u{2018}"),
            "'a\u{2019}\u{2019}b\u{2018}\u{2018}'"
        );
        assert!(safe_cli_name("Платформа v2.0 core_x-1"));
        for bad in [
            "",
            "-",
            "-x",
            "--yes",
            "a'b",
            "a;b",
            "x$(id)",
            "a\"b",
            "a`b",
            &"я".repeat(129),
        ] {
            assert!(!safe_cli_name(bad), "{bad}");
        }
    }

    /// Строка в кавычках POSIX shell доходит до команды байт в байт (настоящий `sh`).
    #[cfg(unix)]
    #[test]
    fn sh_quote_roundtrip_real_shell() {
        for v in [
            "a'; rm -rf ~; '",
            "x$(id)`whoami`'\"",
            "''",
            "'",
            "пробел и \\ обратная черта",
            "$HOME ${PATH} !! *",
        ] {
            let o = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", sh_quote(v)))
                .output()
                .unwrap();
            assert!(o.status.success(), "{v}");
            assert_eq!(String::from_utf8(o.stdout).unwrap(), v);
        }
    }

    #[test]
    fn long_name_truncated() {
        let n = "я".repeat(128);
        let b = block_name(&n);
        assert_eq!(b.chars().count(), BLOCK_NAME_MAX_CHARS + 1);
        assert!(b.ends_with('…'));
        assert_eq!(block_name("короткое"), "короткое");
    }
}
