//! Ядро ZeroAIMemory: общие типы и логика.

pub mod id;
pub mod solution;
pub mod target;
pub mod token;

pub use id::{Id, ParseIdError};
pub use solution::SolutionGuid;
pub use target::{TargetKind, TargetRef};
pub use token::{NewToken, generate_token, hash_token, looks_like_token, mask_token};

/// Текущая версия блока инструкции агента ZeroAIMemory в CLAUDE.md/AGENTS.md (§14.2, §15.5;
/// v3 — M4.5 §11):
/// маркер `<!-- zeroaimemory:begin vN -->`. Нижняя граница версии сервера: действующая версия —
/// `max(AGENT_BLOCK_VERSION, версия шаблонов в settings)` (`zam_server::agent_setup`), с ней
/// сравнивают `/recall(session_start)` и `zam status`, её пишет в маркер `GET /agent-setup`.
pub const AGENT_BLOCK_VERSION: u32 = 3;

/// Предел длины имени решения, символов (§11.6): регистрация (`POST /solutions`), переименование
/// (`PATCH /solutions/{guid}`), `GET /agent-setup?name=`, однострочник `zam init` и `zam init
/// --name`. Одно число на все пути (финальное ревью M4, M-2): 128 — предел регистрации с M3,
/// поэтому ни одно зарегистрированное имя ему не противоречит; переименование до 200 символов
/// было только в невыпущенной ветке M4.
pub const SOLUTION_NAME_MAX_CHARS: usize = 128;

/// Переменная с именем машины для `X-Zam-Machine` — одна для CLI и хуков и для `.mcp.json`
/// (`"X-Zam-Machine": "${ZAM_MACHINE:-}"`): Claude Code подставляет в `.mcp.json` только одну
/// переменную без вложенных умолчаний, а `.mcp.json` бывает общим для машин в репозитории, поэтому
/// имя самой машины в файл не пишется (финальное ревью M4, M-4). Задаёт её команда установки `zam`.
pub const MACHINE_ENV: &str = "ZAM_MACHINE";

/// Предел длины `X-Zam-Machine`, символов (как на сервере).
pub const MACHINE_MAX_CHARS: usize = 128;

/// Имя машины для `X-Zam-Machine` из окружения: [`MACHINE_ENV`], затем `COMPUTERNAME` (Windows),
/// затем `HOSTNAME` (если экспортирована); без внешних вызовов. Пустое, длиннее
/// [`MACHINE_MAX_CHARS`] или с управляющими символами — `None`.
pub fn machine_name(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    [MACHINE_ENV, "COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|k| get(k))
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
        .and_then(valid_machine)
}

/// Имя машины годится для `X-Zam-Machine`: непустое, до [`MACHINE_MAX_CHARS`], без управляющих.
pub fn valid_machine(v: String) -> Option<String> {
    let v = v.trim().to_string();
    (!v.is_empty() && v.chars().count() <= MACHINE_MAX_CHARS && !v.chars().any(char::is_control))
        .then_some(v)
}

/// Переменные окружения, которые ставит агент (Claude Code, Codex) своим подпроцессам и по
/// которым `zam` понимает, что его запустил агент, и берёт сессию хуков (§10.3, задача 10 M4).
/// Тесты и smoke, запускающие `zam` изнутри сессии агента, убирают их из окружения дочернего
/// процесса — иначе окружение самого разработчика утекает в проверку (задача 16 M4, M-5).
///
/// Переменные Codex (префикс `CODEX_`) здесь же — признак запуска `zam` агентом Codex
/// (`zam-cli`, `launched_by_agent`), список один.
pub const AGENT_ENV_VARS: [&str; 6] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_THREAD_ID",
];

/// Клиентские переменные `zam` (не агента): явная сессия хуков, признак агента, имя машины.
/// Сервер не читает их как ключи конфигурации, тесты убирают из окружения дочернего `zam`.
pub const CLIENT_ENV_VARS: [&str; 3] = ["ZAM_SESSION", "ZAM_AGENT", MACHINE_ENV];

#[cfg(test)]
mod machine_tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn machine_name_order_and_rules() {
        let both = &[
            ("ZAM_MACHINE", "m"),
            ("COMPUTERNAME", "WIN"),
            ("HOSTNAME", "lin"),
        ];
        assert_eq!(machine_name(env(both)).as_deref(), Some("m"));
        let win = &[
            ("ZAM_MACHINE", " "),
            ("COMPUTERNAME", "WIN"),
            ("HOSTNAME", "lin"),
        ];
        assert_eq!(machine_name(env(win)).as_deref(), Some("WIN"));
        assert_eq!(
            machine_name(env(&[("COMPUTERNAME", " "), ("HOSTNAME", "lin")])).as_deref(),
            Some("lin")
        );
        assert_eq!(machine_name(env(&[])), None);
        assert_eq!(machine_name(env(&[("HOSTNAME", "a\tb")])), None);
        assert_eq!(valid_machine("x".repeat(MACHINE_MAX_CHARS + 1)), None);
    }
}
