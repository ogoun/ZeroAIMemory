//! Подключение агента (§14, §15.5; задача 11 M4).
//!
//! ```text
//! GET /agent-setup?agent=claude|agents|codex&solution=<guid|new|auto>&name=&mcp=&hooks=
//!                  &rules_write=&lang=ru|en&token=
//!   → 200 {version, agent, lang, url, block, tokens, tokens_exact, solution_toml?, solution?,
//!          mcp_json?, claude_settings?, codex_hooks?, install: {windows, linux}, init_cmd}
//! GET /agent-guide?lang=ru|en → 200 text/markdown (полный протокол §9.3, §14)
//! ```
//!
//! - Доступ — токен (любой, `ro` тоже) или администратор WebUI. `token=` — только
//!   администратору: открытый текст только что выпущенного токена для `install` и `init_cmd`;
//!   в `block` и `mcp_json` токена нет никогда (`Bearer ${ZAM_TOKEN}`).
//! - `url` — `server.public_url`, иначе схема и `Host` запроса плюс `server.base_path`.
//! - `solution`: `auto` (по умолчанию) — без решения (его создаст `zam init`); `new` — новый
//!   GUID (v7); GUID — указанное решение. Имя — `name`, иначе имя из реестра (владельцу и
//!   администратору), иначе `Solution`.
//! - `mcp`, `hooks` — по умолчанию `true`; `rules_write` — по умолчанию настройка сервера
//!   `rules.agent_write`. Хуков у `agents` нет (AGENTS.md без хуков Codex).
//! - `version` — текущая версия блока сервера ([`agent_setup::current_version`]).
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_core::{SolutionGuid, looks_like_token};

use super::query;
use super::scope::RULES_AGENT_WRITE;
use crate::agent_setup::{self, AgentKind, BlockCtx, InitCmd, Lang, SolutionRef};
use crate::app::AppState;
use crate::auth::{Client, Principal, target_user_id};
use crate::error::ApiError;

/// Имя решения без указания.
const DEFAULT_NAME: &str = "Solution";
/// Предел длины имени решения, символов (как у `POST /solutions`).
const NAME_MAX_CHARS: usize = 128;
/// Предел длины `Host`, байт.
const HOST_MAX: usize = 255;

#[derive(Deserialize)]
pub struct SetupQuery {
    agent: Option<String>,
    solution: Option<String>,
    name: Option<String>,
    mcp: Option<bool>,
    hooks: Option<bool>,
    rules_write: Option<bool>,
    lang: Option<String>,
    token: Option<String>,
}

fn parse_agent(raw: Option<&str>) -> Result<AgentKind, ApiError> {
    match raw {
        None => Ok(AgentKind::Claude),
        Some(s) => AgentKind::parse(s)
            .ok_or_else(|| ApiError::BadRequest("agent: claude, agents или codex".into())),
    }
}

pub(crate) fn parse_lang(raw: Option<&str>) -> Result<Lang, ApiError> {
    match raw {
        None => Ok(Lang::Ru),
        Some(s) => Lang::parse(s).ok_or_else(|| ApiError::BadRequest("lang: ru или en".into())),
    }
}

/// Внешний адрес сервера без замыкающего `/`.
fn public_url(st: &AppState, headers: &HeaderMap, client: Client) -> Result<String, ApiError> {
    if let Some(u) = st
        .cfg
        .server
        .public_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        return Ok(u.trim_end_matches('/').to_string());
    }
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::trim)
        .filter(|h| {
            !h.is_empty()
                && h.len() <= HOST_MAX
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:[]_".contains(&b))
        })
        .ok_or_else(|| {
            ApiError::BadRequest(
                "нет корректного заголовка Host: задайте server.public_url в конфигурации".into(),
            )
        })?;
    let scheme = if client.https { "https" } else { "http" };
    Ok(format!("{scheme}://{host}{}", st.cfg.server.base_path))
}

fn valid_name(raw: &str) -> Result<String, ApiError> {
    let n = raw.trim();
    if n.is_empty() || n.chars().count() > NAME_MAX_CHARS || n.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(format!(
            "name: непустое, до {NAME_MAX_CHARS} символов, без управляющих"
        )));
    }
    Ok(n.to_string())
}

/// Решение запроса (`auto` — нет).
async fn solution(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    q: &SetupQuery,
) -> Result<Option<SolutionRef>, ApiError> {
    let name = q.name.as_deref().map(valid_name).transpose()?;
    let guid = match q.solution.as_deref().map(str::trim) {
        None | Some("auto") => return Ok(None),
        Some("new") => SolutionGuid::new_v7(),
        Some(g) => g.parse::<SolutionGuid>().map_err(|_| {
            ApiError::BadRequest("solution: GUID решения (UUID с дефисами), new или auto".into())
        })?,
    };
    let name = match name {
        Some(n) => n,
        None => {
            let user = target_user_id(st, p, headers)?;
            let admin = matches!(p, Principal::Admin { .. });
            match st.system.get_solution(&guid).await? {
                Some(i) if admin || i.owner_id == user => i.name,
                _ => DEFAULT_NAME.into(),
            }
        }
    };
    Ok(Some(SolutionRef {
        guid: guid.to_string(),
        name,
    }))
}

pub async fn setup(
    State(st): State<AppState>,
    p: Principal,
    client: Client,
    headers: HeaderMap,
    q: Result<Query<SetupQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let agent = parse_agent(q.agent.as_deref())?;
    let lang = parse_lang(q.lang.as_deref())?;
    let token = match (q.token.as_deref().map(str::trim), &p) {
        (None | Some(""), _) => None,
        (Some(t), Principal::Admin { .. }) if looks_like_token(t) => Some(t.to_string()),
        (Some(_), Principal::Admin { .. }) => {
            return Err(ApiError::BadRequest("token: токен вида zam_…".into()));
        }
        (Some(_), Principal::Token(_)) => {
            return Err(ApiError::BadRequest(
                "token= передаёт только администратор при выпуске токена".into(),
            ));
        }
    };
    let url = public_url(&st, &headers, client)?;
    let sol = solution(&st, &p, &headers, &q).await?;
    let rules_write = match q.rules_write {
        Some(v) => v,
        None => st
            .system
            .get_setting::<bool>(RULES_AGENT_WRITE)
            .await?
            .unwrap_or(false),
    };
    let mcp = q.mcp.unwrap_or(true);
    let hooks = q.hooks.unwrap_or(true);
    let version = agent_setup::current_version(&st.system).await?;
    let (template, custom) = agent_setup::template(&st.system, agent, lang).await?;
    let block = agent_setup::render_block(
        &template,
        version,
        &BlockCtx {
            url: &url,
            solution: sol.as_ref(),
            rules_write,
            lang,
        },
    );
    let mut out = json!({
        "version": version,
        "agent": agent.as_str(),
        "lang": lang.as_str(),
        "url": url,
        "tokens": zam_store::count_tokens(&block),
        "tokens_exact": zam_store::token_counter_exact(),
        "template_custom": custom,
        "block": block,
        "install": agent_setup::install(&url, token.as_deref()),
        "init_cmd": agent_setup::init_cmd(&InitCmd {
            url: &url,
            token: token.as_deref(),
            agent,
            solution: sol.as_ref(),
            hooks,
            mcp,
            lang,
        }),
    });
    if let Some(s) = &sol {
        out["solution"] = json!({"agent_solution_id": s.guid, "name": s.name});
        out["solution_toml"] = json!(agent_setup::solution_toml(s));
    }
    if mcp {
        out["mcp_json"] = agent_setup::mcp_json(&url, sol.as_ref());
    }
    if hooks {
        match agent {
            AgentKind::Claude => out["claude_settings"] = agent_setup::claude_settings(),
            AgentKind::Codex => out["codex_hooks"] = agent_setup::codex_hooks(),
            AgentKind::Agents => {}
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct GuideQuery {
    lang: Option<String>,
}

/// `GET /agent-guide` — Markdown.
pub async fn guide(
    _: Principal,
    q: Result<Query<GuideQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let lang = parse_lang(query(q)?.lang.as_deref())?;
    let mut r = agent_setup::guide(lang).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    Ok(r)
}
