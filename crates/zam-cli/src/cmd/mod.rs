//! Команды `zam`. Общее: флаги подключения, разрешение цели, вывод в stdout.
pub mod attach;
pub mod cache;
pub mod code;
pub mod glossary;
pub mod guide;
pub mod history;
pub mod hook;
pub mod idea;
pub mod init;
pub mod insight;
pub mod link;
pub mod log;
pub mod mcp;
pub mod question;
pub mod read;
pub mod refs;
pub mod release;
pub mod report;
pub mod search;
pub mod session_snapshot;
pub mod sessions;
pub mod solution;
pub mod status;
pub mod task;
pub mod transfer;
pub mod tree;
pub mod vault;
pub mod work;
pub mod write;

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use serde_json::{Value, json};

use crate::client::{CliError, Client, EXIT_CONFIG, EXIT_TOKEN, EXIT_UNREACHABLE, read_json};
use crate::locate::{self, Flags};
use crate::target::{self, Loc, Solution};

/// Общие флаги команд.
#[derive(Args, Debug, Clone)]
pub struct Common {
    /// Адрес сервера (иначе ZAM_URL, CLAUDE.md/AGENTS.md, ~/.zeroaimemory/config.toml)
    #[arg(long)]
    pub url: Option<String>,
    /// Токен доступа (иначе ZAM_TOKEN и те же источники)
    #[arg(long)]
    pub token: Option<String>,
    /// Вывод в JSON
    #[arg(long)]
    pub json: bool,
}

/// Наибольшая страница ленты записей у сервера — `zam_store::entries::LIST_LIMIT_MAX` (клиент
/// от `zam-store` не зависит: значение повторено здесь, менять вместе).
pub const LIST_LIMIT_MAX: u32 = 200;

/// Страницы ленты журнала (`zam insight|task|question|idea list`, M6.6 §9, замечание 16г). У
/// `zam release list` `--limit` — N новейших по дате после чтения всей ленты: его справку
/// переопределяет команда (`mut_arg` в `ReleaseCmd::List`).
#[derive(Args, Debug, Clone, Copy)]
pub struct Paging {
    /// Записей на странице, 1–200 (вся лента — --all)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(LIST_LIMIT_MAX)))]
    pub limit: Option<u32>,
    /// Вся лента: страницы по 200 до конца
    #[arg(long, conflicts_with = "limit")]
    pub all: bool,
}

/// Строка stderr об обрезанной ленте: показано `n` записей, на сервере есть ещё. Совет
/// `--limit` — только пока страница меньше [`LIST_LIMIT_MAX`].
fn more_hint(n: usize) -> String {
    if n >= LIST_LIMIT_MAX as usize {
        format!("показано {n}, есть ещё: --all")
    } else {
        format!("показано {n}, есть ещё: --limit {LIST_LIMIT_MAX} или --all")
    }
}

/// Лента записей журнала `GET /spaces/{space}/entries` с фильтрами `query` — общий обход
/// `zam insight|task|question|idea list` (M6.6 §9, замечание 16г). `limit` — записей на
/// странице (`None` — умолчание сервера, 50); `all` — страницы по [`LIST_LIMIT_MAX`] по
/// `next_cursor` до конца. Записи ещё есть, а `all` нет — строка [`more_hint`] в stderr (и с
/// `--json`): лента больше не обрезается молча. Ответ — как у сервера, `{items, next_cursor}`;
/// у `all` — все записи и `next_cursor: null`. Обход `all` не зацикливается: курсор, который уже
/// был (`A→A`, `A→B→A`), и пустая страница с курсором — код 2 ([`cursor_loop`]).
pub fn list_entries(
    ctx: &Ctx,
    space: &str,
    query: &[(&str, &str)],
    limit: Option<u32>,
    all: bool,
) -> Result<Value, CliError> {
    let path = format!("/spaces/{space}/entries");
    let page = if all { Some(LIST_LIMIT_MAX) } else { limit }.map(|l| l.to_string());
    let mut items: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        let mut v = {
            let mut q = query.to_vec();
            if let Some(l) = &page {
                q.push(("limit", l));
            }
            if let Some(c) = &cursor {
                q.push(("cursor", c));
            }
            read_json(ctx.client.get(&path, &q)?)?
        };
        let got = match v["items"].as_array_mut() {
            Some(a) => {
                let n = a.len();
                items.append(a);
                n
            }
            None => 0,
        };
        match v["next_cursor"].as_str().map(str::to_string) {
            Some(c) if all => {
                if let Some(e) = cursor_loop(&seen, &c, got) {
                    return Err(e);
                }
                seen.insert(c.clone());
                cursor = Some(c);
            }
            next => {
                if next.is_some() {
                    eprintln!("{}", more_hint(items.len()));
                }
                v["items"] = Value::Array(items);
                v["next_cursor"] = next.map_or(Value::Null, Value::String);
                return Ok(v);
            }
        }
    }
}

/// Обход `--all` зациклился бы: следующий курсор `next` уже был (`seen`) или страница из `got`
/// записей пуста, а курсор есть (у keyset-ленты сервера так не бывает) — ошибка с кодом 2, как
/// у прочих неверных ответов сервера.
fn cursor_loop(
    seen: &std::collections::HashSet<String>,
    next: &str,
    got: usize,
) -> Option<CliError> {
    if seen.contains(next) {
        Some(CliError::new(
            EXIT_UNREACHABLE,
            format!("сервер повторил курсор ленты «{next}»"),
        ))
    } else if got == 0 {
        Some(CliError::new(
            EXIT_UNREACHABLE,
            format!("сервер вернул пустую страницу ленты с курсором «{next}»"),
        ))
    } else {
        None
    }
}

pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent-home"))
}

/// Переменные окружения, которые читает поиск настроек.
pub fn zam_env() -> Vec<(String, String)> {
    ["ZAM_URL", "ZAM_TOKEN"]
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect()
}

/// Нет токена для адреса `url` (M7 §4.4) — подсказка `zam init --url <адрес> --token`. В домашнем
/// конфиге есть токены только других адресов (`elsewhere`, [`locate::Resolved::other_server_keys`])
/// — код 1 (настройка): на этот сервер они не отправляются; токена нет вовсе — код 3, как раньше.
pub fn missing_token(url: &str, elsewhere: bool) -> CliError {
    let init = format!("zam init --url {url} --token <токен>");
    if elsewhere {
        CliError::config(format!(
            "в ~/.zeroaimemory/config.toml нет токена для {url} (токен другого адреса на этот сервер \
             не отправляется): {init}"
        ))
    } else {
        CliError::new(
            EXIT_TOKEN,
            format!("токен не задан для {url}: {init} (или флаг --token, ZAM_TOKEN)"),
        )
    }
}

/// Ключ без адреса в домашнем конфиге (`token` без `url`, [`locate::UnboundToken`]) и команда
/// привязки: `used_for` — адрес, на который он сейчас уходит (записей `[servers]` нет, как в
/// 0.0.13-m6.6); `None` — он больше не используется. `mask` — маска ключа, сам ключ не печатается.
pub fn unbound_token_note(mask: &str, used_for: Option<&str>) -> String {
    match used_for {
        Some(url) => format!(
            "в ~/.zeroaimemory/config.toml ключ без адреса ({mask}): пока ключей [servers] нет, он \
             уходит на любой адрес — привяжите: zam init --url {url} --token <токен>"
        ),
        None => format!(
            "в ~/.zeroaimemory/config.toml ключ без адреса ({mask}) больше не используется (ключи \
             хранятся по адресам) — привяжите: zam init --url <адрес его сервера> --token <токен>"
        ),
    }
}

/// Подключение команды заметок.
pub struct Ctx {
    pub client: Client,
    pub solution: Option<Solution>,
    pub json: bool,
    /// Сессия агента (`X-Zam-Session`): запись в память попадает в журнал этой сессии.
    pub session: Option<String>,
}

/// URL и токен (`locate` M0), `solution.toml` — и клиент API.
pub fn connect(c: &Common) -> Result<Ctx, CliError> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let flags = Flags {
        url: c.url.clone(),
        token: c.token.clone(),
    };
    let r = locate::resolve(&flags, &zam_env(), &cwd, &home_dir()).map_err(CliError::config)?;
    let url = r.url.ok_or_else(|| {
        CliError::config(
            "не задан адрес сервера: флаг --url, ZAM_URL, блок ZeroAIMemory в CLAUDE.md/AGENTS.md или ~/.zeroaimemory/config.toml",
        )
    })?;
    locate::check_url(&url.value).map_err(CliError::config)?;
    let elsewhere = r.other_server_keys;
    let token = r
        .token
        .ok_or_else(|| missing_token(&url.value, elsewhere))?;
    let solution = match locate::find_solution(&cwd) {
        Some(p) => Some(target::load_solution(&p)?),
        None => None,
    };
    let mut client = Client::new(
        &url.value,
        &token.value,
        solution.as_ref().map(|s| s.id.clone()),
    );
    let session = agent_session(&cwd);
    client.set_session(session.clone());
    Ok(Ctx {
        client,
        solution,
        json: c.json,
        session,
    })
}

/// Сессия агента для `X-Zam-Session` (fix round 1, I-1): `ZAM_SESSION` (`-` — без сессии)
/// сильнее всего; иначе — только если команду запустил агент ([`hook::launched_by_agent`]):
/// текущая сессия хуков проекта (`.zeroaimemory/cache/session.json`, не старше 12 ч), затем
/// запасной `CLAUDE_CODE_SESSION_ID`. Человек в обычном терминале пишет без сессии
/// **[решение контроллера, задача 10 M4]**.
fn agent_session(cwd: &std::path::Path) -> Option<String> {
    hook::agent_session_from(
        |k| std::env::var(k).ok(),
        &std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .collect::<Vec<_>>(),
        || hook::cache::current_session(&hook::project_root(cwd, &home_dir())),
    )
}

impl Ctx {
    /// Запись в память командой `zam` агента → `Ev::MemoryWrite {kind}` в журнал его сессии
    /// (M4.5 §9.1: из ответов MCP этого не видно, пишет только CLI); без сессии, без журнала
    /// сессии в корне проекта (его заводят хуки) или при выключенном `session_journal` —
    /// ничего. Сбой журнала команду не валит.
    pub fn note_write(&self, kind: &str) {
        let Some(session) = self.session.as_deref() else {
            return;
        };
        let cwd = std::env::current_dir().unwrap_or_default();
        // только дописывание в уже начатый хуками журнал: команда не создаёт `.zeroaimemory/`
        // (корень без разметки — корень git, иначе журнал появился бы в чужом репозитории)
        let root = hook::project_root(&cwd, &home_dir());
        if !hook::journal::path(&root, session).is_file() {
            return;
        }
        let env = hook::Env::new(
            hook::Kind::Prompt,
            hook::Agent::Claude,
            std::time::Instant::now(),
            cwd,
            home_dir(),
        );
        if env.settings().flag_on(hook::Flag::SessionJournal) {
            let ev = hook::journal::Ev::MemoryWrite {
                kind: kind.chars().take(32).collect(),
            };
            let _ = hook::journal::append(&env.root, session, &ev);
        }
    }

    pub fn locate(&self, raw: &str) -> Result<Loc, CliError> {
        target::parse(raw).resolve(self.solution.as_ref())
    }

    /// GET узла (`/nodes/{id}` или `/nodes/by-path`) с параметрами.
    pub fn get_node(
        &self,
        loc: &Loc,
        query: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>, CliError> {
        match loc {
            Loc::Node(id) => self.client.get(&format!("/nodes/{id}"), query),
            Loc::Path { space, path, .. } => {
                let mut q = vec![("space", space.as_str()), ("path", path.as_str())];
                q.extend_from_slice(query);
                self.client.get("/nodes/by-path", &q)
            }
        }
    }

    /// Узел цели: `{node, content, rev}`.
    pub fn node_json(&self, loc: &Loc) -> Result<Value, CliError> {
        read_json(self.get_node(loc, &[("format", "json")])?)
    }

    /// Id узла цели.
    pub fn node_id(&self, loc: &Loc) -> Result<String, CliError> {
        if let Loc::Node(id) = loc {
            return Ok(id.clone());
        }
        let v = self.node_json(loc)?;
        v["node"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| CliError::new(EXIT_CONFIG, "в ответе сервера нет node.id"))
    }

    /// Регистрация решения из `solution.toml` (идемпотентно) перед записью в него.
    /// Чужое решение: сервер создаёт запрос доступа (202) — запись затем получит 403
    /// `solution-access`, пока владелец не подтвердит (§11.6).
    pub fn ensure_solution(&self, loc: &Loc) -> Result<(), CliError> {
        if let (
            Loc::Path {
                own_solution: true, ..
            },
            Some(s),
        ) = (loc, &self.solution)
        {
            let r = self
                .client
                .post_json(
                    "/solutions",
                    &json!({ "agent_solution_id": s.id, "name": s.name }),
                )
                .map_err(|e| e.context("регистрация решения"))?;
            if r.status().as_u16() == 202 {
                eprintln!(
                    "решение {} принадлежит другому пользователю: запрос доступа ждёт подтверждения владельца",
                    s.id
                );
            }
        }
        Ok(())
    }
}

/// Время из JSON ответа в тексте агенту (M6 §4.4): RFC 3339 → `yyyy-MM-dd HH:mm:ss` UTC, без
/// `T`/`Z`; не время — как есть. `--json` печатает ответ API без изменений (RFC 3339).
pub fn agent_time(s: &str) -> String {
    zam_core::time::agent_ts(s).unwrap_or_else(|| s.to_string())
}

/// Запись в stdout; закрытый канал (`| head`) — не ошибка.
pub fn emit(bytes: &[u8]) -> Result<(), CliError> {
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|_| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(CliError::config(format!("запись в stdout: {e}"))),
    }
}

/// JSON в stdout (одна структура, с отступами, перевод строки в конце).
pub fn emit_json(v: &Value) -> Result<(), CliError> {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    emit(s.as_bytes())
}

/// `<id>@<rev>` или JSON-описание записанного узла.
pub fn emit_written(ctx: &Ctx, node: &Value, created: bool) -> Result<(), CliError> {
    let id = node["id"].as_str().unwrap_or("?");
    let rev = &node["current_rev"];
    if ctx.json {
        return emit_json(&json!({
            "id": id,
            "rev": rev,
            "path": node["path"],
            "space_id": node["space_id"],
            "created": created,
        }));
    }
    emit(format!("{id}@{rev}\n").as_bytes())
}

/// Флаг `f` в `features` ответа `GET /health` (`docs/api-compat.md`).
pub fn has_feature(health: &Value, f: &str) -> bool {
    health["features"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x == f))
}

/// Ошибка `e` запроса возможности сервера с флагом `feature`: HTTP-статус из `statuses` — так
/// старый сервер отвечает на неизвестный маршрут или значение. В `GET /health` `features` нет
/// `feature` (у сервера до M6 нет и самого поля) — код 1 с текстом `old`; флаг есть или `/health`
/// не ответил — `e` как есть (например, 404 — нет такой записи, код 5).
pub fn old_server(ctx: &Ctx, e: CliError, statuses: &[u16], feature: &str, old: &str) -> CliError {
    if !e.http_status().is_some_and(|s| statuses.contains(&s)) {
        return e;
    }
    match ctx.client.get("/health", &[]).and_then(read_json) {
        Ok(h) if !has_feature(&h, feature) => CliError::config(old),
        _ => e,
    }
}

/// GUID решения для команд журналов: флаг `--solution` или `solution.toml`.
pub fn solution_guid(ctx: &Ctx, flag: Option<&str>) -> Result<String, CliError> {
    match flag {
        Some(g) if crate::target::is_guid(g.trim()) => Ok(g.trim().to_ascii_lowercase()),
        Some(_) => Err(CliError::input("--solution: UUID с дефисами")),
        None => ctx.solution.as_ref().map(|s| s.id.clone()).ok_or_else(|| {
            CliError::config(
                "решение не задано: --solution GUID или .zeroaimemory/solution.toml в текущем каталоге или выше",
            )
        }),
    }
}

/// Создаёт запись журнала в пространстве решения (`POST /spaces/{guid}/entries`); решение
/// из `solution.toml` регистрируется (идемпотентно). Печатает `<id>@<rev>` или JSON записи.
pub fn post_entry(
    ctx: &Ctx,
    guid: &str,
    body: &Value,
    key: Option<&str>,
    area: Option<&str>,
) -> Result<(), CliError> {
    let own = ctx.solution.as_ref().is_some_and(|s| s.id == guid);
    ctx.ensure_solution(&Loc::Path {
        space: guid.to_string(),
        path: String::new(),
        own_solution: own,
    })?;
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(k) = key {
        headers.push(("Idempotency-Key", k));
    }
    if let Some(a) = area {
        headers.push(("X-Zam-Area", a));
    }
    let r = ctx
        .client
        .post_json_with(&format!("/spaces/{guid}/entries"), body, &headers)?;
    let created = r.status().as_u16() == 201;
    let e = read_json(r)?;
    if created {
        ctx.note_write(body["kind"].as_str().unwrap_or("entry"));
    }
    if ctx.json {
        return emit_json(&json!({ "entry": e, "created": created }));
    }
    emit(format!("{}@{}\n", e["id"].as_str().unwrap_or("?"), e["rev"]).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M6.6 §9 (16г): совет `--limit` — пока страница меньше наибольшей, дальше — только `--all`.
    #[test]
    fn more_hint_forms() {
        assert_eq!(
            more_hint(50),
            "показано 50, есть ещё: --limit 200 или --all"
        );
        assert_eq!(
            more_hint(199),
            "показано 199, есть ещё: --limit 200 или --all"
        );
        assert_eq!(more_hint(200), "показано 200, есть ещё: --all");
    }

    /// Ревью задачи 8, M2: обход `--all` останавливается на любом уже виденном курсоре и на
    /// пустой странице с курсором — код 2.
    #[test]
    fn cursor_loop_detected() {
        let seen: std::collections::HashSet<String> =
            ["A".to_string(), "B".to_string()].into_iter().collect();
        assert!(cursor_loop(&seen, "C", 1).is_none());
        let e = cursor_loop(&seen, "A", 1).expect("A уже был");
        assert_eq!(
            (e.code, e.msg.as_str()),
            (EXIT_UNREACHABLE, "сервер повторил курсор ленты «A»")
        );
        let e = cursor_loop(&seen, "C", 0).expect("пустая страница с курсором");
        assert_eq!(
            (e.code, e.msg.as_str()),
            (
                EXIT_UNREACHABLE,
                "сервер вернул пустую страницу ленты с курсором «C»"
            )
        );
    }
}
