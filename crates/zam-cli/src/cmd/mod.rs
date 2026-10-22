//! Команды `zam`. Общее: флаги подключения, разрешение цели, вывод в stdout.
pub mod attach;
pub mod history;
pub mod insight;
pub mod link;
pub mod log;
pub mod read;
pub mod release;
pub mod search;
pub mod solution;
pub mod status;
pub mod tree;
pub mod vault;
pub mod write;

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use serde_json::{Value, json};

use crate::client::{CliError, Client, EXIT_CONFIG, EXIT_TOKEN, read_json};
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

/// Подключение команды заметок.
pub struct Ctx {
    pub client: Client,
    pub solution: Option<Solution>,
    pub json: bool,
}

/// Отказ `--human` без интерактивного терминала.
pub const HUMAN_TTY_REQUIRED: &str = "подтверждение аксиом — только человеком: WebUI или интерактивный терминал \
     (--human требует, чтобы stdin и stderr были терминалом)";

/// Шлюз `--human` (ruling контроллера, §11.4): только при интерактивном терминале (`tty` —
/// stdin и stderr — терминал), и человек подтверждает действие, введя `expected` (id записи).
/// Агент, вызывающий CLI без терминала, получает отказ (код токена) без запроса к серверу.
pub fn human_gate(
    tty: bool,
    expected: &str,
    input: &mut dyn std::io::BufRead,
    prompt: &mut dyn std::io::Write,
) -> Result<(), CliError> {
    if !tty {
        return Err(CliError::new(EXIT_TOKEN, HUMAN_TTY_REQUIRED));
    }
    let _ = write!(
        prompt,
        "Действие от имени человека (человеческий токен). Для подтверждения введите id записи {expected}: "
    );
    let _ = prompt.flush();
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|e| CliError::new(EXIT_TOKEN, format!("чтение подтверждения: {e}")))?;
    if line.trim() != expected {
        return Err(CliError::new(
            EXIT_TOKEN,
            "подтверждение не совпало с id записи: действие отменено",
        ));
    }
    Ok(())
}

/// Подключение с человеческим токеном (`--human`, §7.5, §11.4) для действия над `confirm_id`:
/// 1. только интерактивный терминал и подтверждение вводом id ([`human_gate`]);
/// 2. адрес и токен — только из `~/.zeroaimemory/config.toml` (`url`, `human_token`,
///    [`locate::human_target`]); адрес из флага, `ZAM_URL` или файлов проекта игнорируется, при
///    расхождении — предупреждение в stderr.
pub fn connect_human(c: &Common, confirm_id: &str) -> Result<Ctx, CliError> {
    use std::io::IsTerminal;
    let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    human_gate(
        tty,
        confirm_id,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    )?;
    let (url, token) = locate::human_target(&home_dir())
        .map_err(CliError::config)?
        .ok_or_else(|| {
            CliError::new(
                EXIT_TOKEN,
                "--human: нет human_token в ~/.zeroaimemory/config.toml (человеческий токен \
                 выпускает администратор в WebUI; не кладите его в CLAUDE.md/AGENTS.md)",
            )
        })?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let flags = Flags {
        url: c.url.clone(),
        token: None,
    };
    // другие источники адреса — только для предупреждения
    if let Ok(r) = locate::resolve(&flags, &zam_env(), &cwd, &home_dir())
        && let Some(w) = locate::human_url_warning(&url.value, r.url.as_ref())
    {
        eprintln!("{w}");
    }
    let solution = match locate::find_solution(&cwd) {
        Some(p) => Some(target::load_solution(&p)?),
        None => None,
    };
    let client = Client::new(
        &url.value,
        &token.value,
        solution.as_ref().map(|s| s.id.clone()),
    );
    Ok(Ctx {
        client,
        solution,
        json: c.json,
    })
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
    let token = r.token.ok_or_else(|| {
        CliError::new(
            EXIT_TOKEN,
            "токен не задан: флаг --token, ZAM_TOKEN, блок ZeroAIMemory в CLAUDE.md/AGENTS.md или ~/.zeroaimemory/config.toml",
        )
    })?;
    let solution = match locate::find_solution(&cwd) {
        Some(p) => Some(target::load_solution(&p)?),
        None => None,
    };
    let client = Client::new(
        &url.value,
        &token.value,
        solution.as_ref().map(|s| s.id.clone()),
    );
    Ok(Ctx {
        client,
        solution,
        json: c.json,
    })
}

impl Ctx {
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
    if ctx.json {
        return emit_json(&json!({ "entry": e, "created": created }));
    }
    emit(format!("{}@{}\n", e["id"].as_str().unwrap_or("?"), e["rev"]).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_gate_requires_tty_and_confirmation() {
        let mut out = Vec::new();
        let e = human_gate(false, "01ID", &mut "01ID\n".as_bytes(), &mut out).unwrap_err();
        assert_eq!(e.code, EXIT_TOKEN);
        assert!(e.msg.contains("интерактивный терминал"), "{}", e.msg);
        assert!(out.is_empty(), "без терминала ничего не спрашивается");
        let e = human_gate(true, "01ID", &mut "01IX\n".as_bytes(), &mut out).unwrap_err();
        assert_eq!(e.code, EXIT_TOKEN);
        assert!(String::from_utf8_lossy(&out).contains("01ID"));
        assert!(human_gate(true, "01ID", &mut " 01ID \n".as_bytes(), &mut Vec::new()).is_ok());
        assert!(human_gate(true, "01ID", &mut "".as_bytes(), &mut Vec::new()).is_err());
    }
}
