//! `zam hook post-tool` — два режима.
//!
//! **Ошибка** (бюджет 250 мс; §10.3): `PostToolUseFailure.error` (Claude Code) или непустой
//! `PostToolUse.tool_response.stderr` (Codex, Bash). Сначала локальный `error_regex` по кэшу
//! триггеров (первые [`ERROR_SCAN_MAX`] байт), затем `/recall(post_tool)`. Сервер сам
//! проверяет `error_regex`, поэтому при ответе сервера локальные подсказки добавляются только
//! для записей, которых нет в его `items`; сервер недоступен — остаются локальные. Повторы не
//! гасятся: подсказка об ошибке выдаётся при каждой ошибке (§10.3). Подсказки — по ключу
//! `post_tool`; в журнал сессии (ключ `session_journal`) — `Ev::Error` и, если известна
//! команда, `Ev::Cmd {ok: false, err_fp}`. Петля (§9.3, ключ `session_journal`, и при
//! выключенном `post_tool`): тот же `err_fp` в журнале [`LOOP_AT`]-й раз — строка «эта ошибка
//! повторяется N-й раз в сессии…», раз на отпечаток ([`repeat_hint`]). Ответ сервера с
//! `holdout: true` (контрольная группа A/B, M6 §12) — тишина: ни локальных `error_regex`, ни
//! строки петли, маркер сессии ставится ([`cache::note_holdout`]).
//!
//! M7.1 §4.6: ожидаемый ненулевой код ([`expected_exit`]: `grep`, `diff`, `test`… с кодом 1 без
//! stderr) — не ошибка, а успех команды (`Ev::Cmd {ok: true}`, без сети); сбой класса «не
//! повторяй» ([`dont_repeat_class`]: 401, 403, квота, диск, 429, 400/422) — к подсказкам сервера
//! (запрос `/recall(post_tool)` — как у любой ошибки) и `error_regex` добавляется строка класса,
//! построенная локально, раз за сессию на класс и отпечаток; сервер недоступен — строка всё
//! равно есть; в запрос `/recall(post_tool)` — слова путей команды `path_words` ([`zam_core::shell::path_words`],
//! после маскировки секретов): сервер не считает их предметом ошибки.
//!
//! **Успех** (M4.5 §6.3, §9.1; `PostToolUse` без ошибки; без сети, бюджет 50 мс):
//! - `Read` — `Ev::Read` и знание о файле по `file-refs.json` ([`file_context_lines`], ключ
//!   `file_context`): до 3 записей, раз за сессию на файл (`shown-<сессия>.json`, ключ
//!   `file:<путь>`), ссылка `stale` — «возможно устарело», ссылка на символ называет его
//!   ([`file_hint`], M5.6 §8);
//! - `Write|Edit|MultiEdit|NotebookEdit|apply_patch` — `Ev::Edit` по каждому пути и строка
//!   `{path, hash}` в очередь `changed-<сессия>.jsonl` (при решении; файл больше
//!   [`HASH_MAX`] не хэшируется — его поймает `zam refs check`, удалённый — хэш
//!   [`MISSING_HASH`]). M5.6 §8: у пути в `file-refs.json` есть ссылка на символ, файл кода не
//!   больше [`SYMBOLS_MAX_BYTES`] — он разбирается (`zam_code::symbol_hashes`, без запроса
//!   ссылок) и в строку идут хэши тел **всех** его символов (решение составителя плана 5: кэш
//!   ссылок может отставать); отсечка — [`SYMBOLS_PARSE_MS`] на все файлы хука; не успел,
//!   разбор неполный или файл больше — строка без `symbols` (сервер судит по хэшу файла);
//! - `Bash` — `Ev::Cmd {exe, ok: true}` (первое слово команды);
//! - MCP [`MCP_MEMORY_LOG`] — `Ev::MemoryWrite {kind}` из `tool_input.kind` (§9.3: запись
//!   insight через MCP гасит напоминание о паре «ошибка → исправление»).
//!
//! Путь — относительно корня проекта по правилам ссылок ([`crate::cmd::refs::ref_path`]);
//! файл вне корня в журнал и очередь не попадает.
use std::time::{Duration, Instant};

use regex::RegexBuilder;
use serde_json::{Value, json};

use super::cache::{self, PathRefs, Shown, Trigger};
use super::input::Input;
use super::journal::{self, Ev};
use super::output::Output;
use super::pre_tool::{is_edit_tool, patch_files};
use super::{Agent, Env, Flag, Kind, Settings, head_bytes, shown_title};
use crate::client::read_json;
use crate::cmd::refs::{self, MISSING_HASH};

/// Предел файла, который `post-tool` хэширует в бюджете 50 мс, байт.
pub const HASH_MAX: u64 = 8 * 1024 * 1024;
/// Предел файла, который `post-tool` разбирает на символы (M5.6 §10), байт.
pub const SYMBOLS_MAX_BYTES: u64 = 512 * 1024;
/// Отсечка разбора символов правленых файлов — на все файлы одного хука, мс (M5.6 §10).
pub const SYMBOLS_PARSE_MS: u64 = 20;
/// Записей в строке знания о файле.
const FILE_ITEMS_MAX: usize = 3;
/// Предел заголовка записи в строке знания о файле, символов.
const TITLE_MAX: usize = 120;
/// Сканируемая и отправляемая часть ошибки, байт (как `ERROR_SCAN_MAX` сервера).
pub const ERROR_SCAN_MAX: usize = 64 * 1024;
/// Предел размера скомпилированного regex — как у сервера.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Начало строки подсказки об ошибке (как у сервера).
const LEAD: &str = "эта ошибка уже разбиралась: ";

/// Текст ошибки из stdin (fix round 1, I-1): у Claude Code — только `PostToolUseFailure.error`
/// (успешный Bash со stderr — успех: предупреждения компиляторов и прогресс идут в stderr); у
/// Codex — непустой `PostToolUse.tool_response.stderr` (`tool_response` бывает строкой JSON — она
/// разбирается), а если в ответе есть код выхода (`exit_code`, `exitCode`, `metadata.exit_code`),
/// — только при ненулевом.
pub fn error_text(inp: &Input, agent: Agent) -> Option<String> {
    let s = if inp.event == "PostToolUseFailure" {
        inp.str("error").map(str::to_string)
    } else if agent == Agent::Codex {
        let tr = inp.v.get("tool_response")?;
        let parsed;
        let obj = match tr {
            Value::String(t) => {
                parsed = serde_json::from_str::<Value>(t).ok()?;
                &parsed
            }
            v => v,
        };
        let code = ["/exit_code", "/exitCode", "/metadata/exit_code"]
            .iter()
            .find_map(|p| obj.pointer(p).and_then(Value::as_i64));
        if code == Some(0) {
            return None;
        }
        obj.get("stderr")
            .and_then(Value::as_str)
            .map(str::to_string)
    } else {
        None
    };
    s.filter(|e| !e.trim().is_empty())
}

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    match error_text(inp, env.agent) {
        // ожидаемый ненулевой код (M7.1 §4.6 п. 1) — исход команды, а не сбой
        Some(error) if command_of(inp).is_some_and(|c| expected_exit(&c, &error)) => {
            success(env, inp, s)
        }
        Some(error) => failure(env, inp, s, &error),
        None if inp.event == "PostToolUse" => success(env, inp, s),
        None => Output::Empty,
    }
}

/// Программы, у которых код 1 — исход проверки, а не сбой (M7.1 §4.6 п. 1): «совпадений нет»,
/// «файлы различаются», «ложно». У `git` — только `git diff --quiet | --exit-code`.
pub const EXIT_1_PROGRAMS: [&str; 6] = ["grep", "rg", "diff", "cmp", "test", "["];
/// Начала строк-диагностик (stderr) в выводе, кроме имени самой программы: сообщения `git`
/// (`error:`, `fatal:`, `warning:`), справка `usage:` и оболочки.
const DIAG_WORDS: [&str; 9] = [
    "error", "fatal", "warning", "usage", "bash", "sh", "zsh", "dash", "ksh",
];

/// Ожидаемый ненулевой код (M7.1 §4.6 п. 1): простая команда `cmd`
/// ([`zam_core::shell::is_simple`]: без `|`, `;`, `&&`, `||`, фона, второй строки и без
/// подстановок `$(…)`, `` `…` ``, `<(…)`, `>(…)` — их сбой прячется в коде внешней программы) —
/// программа из [`EXIT_1_PROGRAMS`] или `git diff` с `--quiet`/`--exit-code` — завершилась кодом 1
/// и без stderr. Код — первая строка `error` `Exit code 1` (Claude Code; stdout и stderr идут за
/// ней одним текстом), поэтому stderr — строки-диагностики после неё ([`has_diagnostics`])
/// **[решение исполнителя, задача 8 M7.1]**; разница `diff`, счётчик `grep -c` и «a b differ»
/// `cmp` — stdout. У Codex `error` — сам непустой stderr без строки кода: не ожидаемый.
pub fn expected_exit(cmd: &str, error: &str) -> bool {
    let (head, body) = error.split_once('\n').unwrap_or((error, ""));
    if !head.trim().eq_ignore_ascii_case("exit code 1") || !zam_core::shell::is_simple(cmd) {
        return false;
    }
    let Some(prog) = program_name(cmd) else {
        return false;
    };
    let words = zam_core::shell::words(cmd);
    let args = words
        .iter()
        .skip_while(|w| zam_core::shell::is_assignment(w))
        .skip(1)
        .map(String::as_str);
    let listed = match prog.as_str() {
        "git" => git_diff_exit_code(args),
        p => EXIT_1_PROGRAMS.contains(&p),
    };
    listed && !has_diagnostics(body, &prog)
}

/// Программа команды или слова: [`journal::exe_of`] (первое слово без присваиваний и пути) без
/// регистра и суффикса `.exe`.
fn program_name(cmd: &str) -> Option<String> {
    let exe = journal::exe_of(cmd)?.to_lowercase();
    Some(match exe.strip_suffix(".exe") {
        Some(b) => b.to_string(),
        None => exe,
    })
}

/// `git [-C <путь>] [-c <ключ=значение>] [--флаги] diff … --quiet | --exit-code`.
fn git_diff_exit_code<'a>(mut args: impl Iterator<Item = &'a str>) -> bool {
    while let Some(a) = args.next() {
        match a {
            "-C" | "-c" => {
                args.next();
            }
            "diff" => return args.any(|a| a == "--quiet" || a == "--exit-code"),
            a if a.starts_with('-') => {}
            _ => return false,
        }
    }
    false
}

/// В выводе есть строка-диагностика ([`expected_exit`]). Диагностика начинается с первой
/// колонки (строки контекста `diff -u` с отступом — stdout) и это одно из:
/// - слово до `:` без пробелов — программа `prog`, оболочка (`bash:`, `/usr/bin/bash:`) или
///   слово из [`DIAG_WORDS`];
/// - `<имя>:<число>:` — сообщение скрипта или оболочки с номером строки, в том числе zsh
///   `(eval):1: no such file or directory: …` (у программ списка с кодом 1 такого stdout нет:
///   `grep -n` без совпадений ничего не печатает);
/// - Python: `Traceback (most recent call last):` и `<Имя>Error: …` (с модулем —
///   `requests.exceptions.ConnectionError: …`).
fn has_diagnostics(body: &str, prog: &str) -> bool {
    body.lines().any(|l| {
        if l == "Traceback (most recent call last):" {
            return true;
        }
        let Some((word, rest)) = l.split_once(':') else {
            return false;
        };
        if word.is_empty() || word.contains(char::is_whitespace) {
            return false;
        }
        let numbered = rest
            .split_once(':')
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let python_error = word.rsplit('.').next().is_some_and(|w| {
            w.len() > "Error".len()
                && w.ends_with("Error")
                && w.starts_with(|c: char| c.is_ascii_uppercase())
                && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        let named =
            program_name(word).is_some_and(|w| w == prog || DIAG_WORDS.contains(&w.as_str()));
        numbered || python_error || named
    })
}

/// Класс сбоя, который повтор той же команды не исправит (M7.1 §4.6 п. 2): строка «не
/// повторяй» ([`DontRepeat::line`]) строится локально, без сети, и добавляется к подсказкам
/// сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DontRepeat {
    /// 401, `invalid_grant` — учётные данные.
    Auth,
    /// 403, scope — права.
    Scope,
    /// Квота, кредиты.
    Quota,
    /// `ENOSPC`, «No space left on device» — диск.
    NoSpace,
    /// 429 — лимит запросов (`Retry-After`).
    RateLimit,
    /// 400, 422 — неверный запрос.
    BadRequest,
}

/// Код HTTP в тексте ошибки: `HTTP 401`, `HTTP/1.1 401`, `status: 401`, `status code 401`,
/// `returned error: 401`, `response code 401` — или `401 Unauthorized` (имена `names`).
fn http_status(code: &str, names: &str) -> String {
    format!(
        r"\b(?:HTTP(?:/\d(?:\.\d)?)?[ :]+|status(?:[ _]?code)?\s*[:=]?\s*|returned error:?\s*|response code:?\s*){code}\b|\b{code}\s+(?:{names}|client error)\b"
    )
}

impl DontRepeat {
    /// Порядок проверки: точные признаки раньше общих (`insufficient_quota` приходит с 429, «API
    /// rate limit exceeded» — с 403).
    pub const ALL: [DontRepeat; 6] = [
        DontRepeat::NoSpace,
        DontRepeat::Quota,
        DontRepeat::RateLimit,
        DontRepeat::Auth,
        DontRepeat::Scope,
        DontRepeat::BadRequest,
    ];

    /// Имя класса в ключе показа (`shown-<сессия>.json`).
    pub fn key(self) -> &'static str {
        match self {
            DontRepeat::Auth => "auth",
            DontRepeat::Scope => "scope",
            DontRepeat::Quota => "quota",
            DontRepeat::NoSpace => "no_space",
            DontRepeat::RateLimit => "rate_limit",
            DontRepeat::BadRequest => "bad_request",
        }
    }

    /// Шаблон класса (без учёта регистра).
    fn pattern(self) -> String {
        match self {
            DontRepeat::NoSpace => r"\bENOSPC\b|no space left on device|there is not enough space on the disk|недостаточно места на диске".into(),
            DontRepeat::Quota => r"\binsufficient_quota\b|\bquota\b[^\n]{0,40}\b(?:exceeded|exhausted)\b|\b(?:exceeded|exhausted)\b[^\n]{0,40}\bquota\b|\bcredit balance is too low\b".into(),
            DontRepeat::RateLimit => format!(
                r"{}|\btoo many requests\b|\brate[ _-]?limit(?:ed)?\b[^\n]{{0,30}}\b(?:exceeded|reached)\b",
                http_status("429", "too many requests")
            ),
            DontRepeat::Auth => format!(r"{}|\binvalid_grant\b", http_status("401", "unauthorized")),
            DontRepeat::Scope => format!(
                r"{}|\binsufficient[_ ]scopes?\b|\b(?:missing|required) scopes?\b",
                http_status("403", "forbidden")
            ),
            DontRepeat::BadRequest => format!(
                "{}|{}",
                http_status("400", "bad request"),
                http_status("422", r"unprocessable(?: entity| content)?")
            ),
        }
    }

    /// Строка класса, до 30 токенов: что проверить вместо повтора (без готового рецепта); у 429 —
    /// значение `Retry-After`, если оно есть в ошибке.
    pub fn line(self, error: &str) -> String {
        match self {
            DontRepeat::Auth => {
                "ZeroAIMemory: доступ отклонён (401/invalid_grant) — проверь ключ, не повторяй тот \
                 же запрос"
                    .into()
            }
            DontRepeat::Scope => {
                "ZeroAIMemory: нет прав (403/scope) — проверь права ключа, повтор не поможет".into()
            }
            DontRepeat::Quota => {
                "ZeroAIMemory: квота исчерпана — проверь лимит и баланс, повтор не поможет".into()
            }
            DontRepeat::NoSpace => {
                "ZeroAIMemory: нет места на диске — освободи место, повтор не поможет".into()
            }
            DontRepeat::RateLimit => {
                let wait = retry_after(error).map_or(String::new(), |s| format!(" ({s} с)"));
                format!(
                    "ZeroAIMemory: лимит запросов (429) — выжди Retry-After{wait}, не повторяй сразу"
                )
            }
            DontRepeat::BadRequest => {
                "ZeroAIMemory: запрос отклонён (400/422) — проверь запрос, а не повторяй".into()
            }
        }
    }
}

/// Тест-раннер среди простых команд `cmd` ([`zam_core::shell::segments`]; ревью задачи 8 M7.1,
/// M-4): в выводе тестов коды HTTP — чаще текст проверок (`400 Bad Request` в ожидании теста),
/// чем сбой самой команды, поэтому классы «не повторяй» к ним не применяются. Раннеры: `cargo
/// test | nextest`, `go test`, `npm | pnpm | yarn | bun test` (и `run test`), `pytest`, `jest`,
/// `vitest` (и через `npx`/`pnpx`/`bunx`, `python -m pytest`).
fn is_test_runner(cmd: &str) -> bool {
    zam_core::shell::segments(cmd).iter().any(|seg| {
        let words = zam_core::shell::words(seg);
        let mut w = words
            .iter()
            .skip_while(|w| zam_core::shell::is_assignment(w))
            .map(|w| program_name(w).unwrap_or_default());
        let Some(prog) = w.next() else {
            return false;
        };
        let rest: Vec<String> = w.collect();
        // первое слово-подкоманда: без флагов `-…` и `+toolchain`
        let sub = || {
            rest.iter()
                .find(|a| !a.starts_with(['-', '+']))
                .map(String::as_str)
        };
        match prog.as_str() {
            "pytest" | "jest" | "vitest" => true,
            "cargo" => matches!(sub(), Some("test" | "nextest")),
            "go" => sub() == Some("test"),
            "npm" | "pnpm" | "yarn" | "bun" => {
                let mut subs = rest.iter().filter(|a| !a.starts_with('-'));
                match subs.next().map(String::as_str) {
                    Some("test") => true,
                    Some("run") => subs.next().is_some_and(|s| s == "test"),
                    _ => false,
                }
            }
            "npx" | "pnpx" | "bunx" => matches!(sub(), Some("jest" | "vitest" | "pytest")),
            "python" | "python3" | "py" => {
                rest.windows(2).any(|p| p[0] == "-m" && p[1] == "pytest")
            }
            _ => false,
        }
    })
}

/// Скомпилированные шаблоны классов ([`DontRepeat::ALL`]) — один раз на процесс.
fn dont_repeat_regexes() -> &'static [(DontRepeat, regex::Regex)] {
    static RE: std::sync::OnceLock<Vec<(DontRepeat, regex::Regex)>> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        DontRepeat::ALL
            .iter()
            .filter_map(|&c| {
                RegexBuilder::new(&c.pattern())
                    .case_insensitive(true)
                    .build()
                    .ok()
                    .map(|r| (c, r))
            })
            .collect()
    })
}

/// Класс «не повторяй» ошибки (M7.1 §4.6 п. 2) — первый совпавший по порядку
/// [`DontRepeat::ALL`]; не совпал ни один — `None`.
pub fn dont_repeat_class(error: &str) -> Option<DontRepeat> {
    dont_repeat_regexes()
        .iter()
        .find(|(_, r)| r.is_match(error))
        .map(|(c, _)| *c)
}

/// Секунды `Retry-After: N` в тексте ошибки.
fn retry_after(error: &str) -> Option<u64> {
    static RE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        RegexBuilder::new(r"\bretry-after\s*[:=]?\s*(\d{1,6})\b")
            .case_insensitive(true)
            .build()
            .ok()
    })
    .as_ref()?
    .captures(error)?
    .get(1)?
    .as_str()
    .parse()
    .ok()
}

/// Строка класса `class` ошибки с отпечатком `fp`: раз за сессию на класс и отпечаток (`shown`,
/// ключ `dont_repeat:<класс>:<отпечаток>`); без сессии — каждый раз.
fn dont_repeat_line(
    env: &Env,
    session: Option<&str>,
    class: DontRepeat,
    fp: &str,
    error: &str,
) -> Option<String> {
    if let Some(id) = session {
        let key = format!("dont_repeat:{}:{fp}", class.key());
        let mut sh = cache::read_shown(env, id);
        if sh.shown.contains(&key) {
            return None;
        }
        sh.shown.push(key);
        cache::write_shown(env, id, &sh);
    }
    Some(class.line(error))
}

/// Команда `tool_input.command` (строка или массив аргументов).
fn command_of(inp: &Input) -> Option<String> {
    let v = inp.v.get("tool_input")?.get("command")?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = v.as_array()?.iter().filter_map(|x| x.as_str()).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Режим ошибки (M4) и события журнала.
fn failure(env: &Env, inp: &Input, s: &Settings, error: &str) -> Output {
    let error = head_bytes(error, ERROR_SCAN_MAX);
    let tool = inp.str("tool_name").filter(|t| !t.is_empty());
    if let (Some(t), Some(id)) = (tool, inp.session_id.as_deref())
        && is_edit_tool(t)
    {
        record_edits(env, s, inp, t, id, false);
    }
    let norm = journal::mask_secrets(&journal::normalize_error(error));
    let fp = journal::err_fingerprint(&norm);
    let mut loop_line = None;
    if let Some(session) = inp.session_id.as_deref()
        && s.flag_on(Flag::SessionJournal)
    {
        journal::note(
            env,
            session,
            &Ev::Error {
                tool: tool.unwrap_or("").to_string(),
                err_fp: fp.clone(),
                text: journal::error_text(&norm),
            },
        );
        loop_line = repeat_hint(env, session, &fp);
        if tool != Some("apply_patch")
            && let Some(exe) = command_of(inp).as_deref().and_then(journal::exe_of)
        {
            journal::note(
                env,
                session,
                &Ev::Cmd {
                    exe,
                    ok: false,
                    err_fp: Some(fp.clone()),
                },
            );
        }
    }
    if !s.on(Kind::PostTool) {
        return Output::context(&inp.event, loop_line.as_deref().unwrap_or_default());
    }
    let server = env.server_url();
    let cached = cache::usable_triggers(env, server.as_deref());
    let local = cached
        .as_ref()
        .map(|c| local_matches(&c.triggers, tool, error, env))
        .unwrap_or_default();
    // слова путей команды — не предмет ошибки (M7.1 §4.6 п. 3); значения секретов и флагов с
    // паролем не уходят (маскировка — как у команд снимка сессии)
    let path_words: Vec<String> = match (tool, command_of(inp)) {
        (Some("apply_patch"), _) | (_, None) => Vec::new(),
        (_, Some(c)) => {
            let masked = zam_secrets::mask_flags(&zam_secrets::mask_text(&c));
            zam_core::shell::path_words(&masked, |a| env.cwd.join(a).is_dir())
        }
    };
    let remote = env.client().and_then(|client| {
        let body = json!({
            "event": "post_tool",
            "session_id": inp.session_id,
            "solution": env.solution.as_ref().map(|x| x.id.clone()),
            "error": error,
            "tool": tool,
            // предел канала агента (ревью 8a M6, C1)
            "output_limit": super::output::output_limit(env.agent, ""),
            "path_words": path_words,
        });
        let session = inp.session_id.as_deref();
        std::thread::scope(|sc| {
            // очередь учёта локального вывода — параллельно, в том же бюджете (M4.5 §9.5)
            let flush = session
                .filter(|_| cache::tokens_queued(env, session))
                .map(|id| sc.spawn(|| cache::flush_tokens(env, &client, id)));
            let r = match client.post_json("/recall", &body).and_then(read_json) {
                Ok(v) => Some(v),
                Err(e) => {
                    env.log(&format!("recall(post_tool): {e}"));
                    None
                }
            };
            if let Some(h) = flush
                && h.join().is_err()
            {
                env.log("sessions(tokens): паника");
            }
            r
        })
    });
    let mut lines: Vec<String> = Vec::new();
    let mut known: Vec<String> = Vec::new();
    if let Some(v) = &remote {
        // контрольная группа A/B (M6 §12): маркер — по ответу; в группе — ни локальных
        // совпадений, ни строки петли
        cache::note_holdout(env, inp.session_id.as_deref(), v);
        if v["holdout"].as_bool() == Some(true) {
            return Output::Empty;
        }
        for w in cache::strings(&v["warnings"]) {
            env.log(&format!("recall(post_tool): предупреждение сервера: {w}"));
        }
        if let Some(t) = v["text"].as_str().filter(|t| !t.trim().is_empty()) {
            lines.push(t.trim_end().to_string());
        }
        if let Some(items) = v["items"].as_array() {
            known.extend(
                items
                    .iter()
                    .filter_map(|i| i["id"].as_str())
                    .map(str::to_string),
            );
        }
    }
    for t in local {
        let r = format!("i:{}", t.entry_id);
        if !known.contains(&r) {
            known.push(r);
            lines.push(t.hint(LEAD));
        }
    }
    // сбой, который повтор не исправит (M7.1 §4.6 п. 2; решение координатора): строка класса
    // строится локально и идёт после подсказок сервера и `error_regex` (сервер недоступен — и
    // без них); сервер её не видел — она в очередь учёта (M4.5 §9.5)
    // у тест-раннера коды HTTP в выводе — текст проверок, не сбой команды (M-4 ревью)
    let runner = command_of(inp).is_some_and(|c| is_test_runner(&c));
    if !runner
        && let Some(class) = dont_repeat_class(error)
        && let Some(line) = dont_repeat_line(env, inp.session_id.as_deref(), class, &fp, error)
    {
        if let Some(id) = inp.session_id.as_deref() {
            cache::note_tokens(env, id, cache::TOKENS_POST_TOOL_LOCAL, &line);
        }
        lines.push(line);
    }
    lines.extend(loop_line);
    Output::context(&inp.event, &lines.join("\n"))
}

/// Повтор ошибки, с которого `post-tool` советует остановиться (§9.3).
pub const LOOP_AT: usize = 3;

/// Петля (§9.3): ошибка с отпечатком `fp` в журнале сессии [`LOOP_AT`]-й раз и больше — строка
/// «эта ошибка повторяется N-й раз…», раз на отпечаток (`shown`, ключ `loop:<fp>`).
fn repeat_hint(env: &Env, session: &str, fp: &str) -> Option<String> {
    let n = journal::read(&env.root, session)
        .iter()
        .filter(|(_, e)| matches!(e, Ev::Error { err_fp, .. } if err_fp == fp))
        .count();
    if n < LOOP_AT {
        return None;
    }
    let key = format!("loop:{fp}");
    let mut sh = cache::read_shown(env, session);
    if sh.shown.contains(&key) {
        return None;
    }
    sh.shown.push(key);
    cache::write_shown(env, session, &sh);
    Some(format!(
        "ZeroAIMemory: эта ошибка повторяется {n}-й раз в сессии — проверь память \
         (memory_search) или запиши разбор"
    ))
}

/// Имя MCP-инструмента записи в память (Claude Code: `mcp__<сервер>__<инструмент>`).
pub const MCP_MEMORY_LOG: &str = "mcp__zeroaimemory__memory_log";

/// Успешный ответ MCP `memory_log` (§9.1): `Ev::MemoryWrite {kind}` из `tool_input.kind`
/// (до 32 символов; нет — `entry`). Ответ с `isError: true` — не запись.
fn mcp_memory_write(env: &Env, inp: &Input, session: &str) {
    let failed = inp
        .v
        .get("tool_response")
        .and_then(|r| r.get("isError"))
        .and_then(Value::as_bool)
        == Some(true);
    if failed {
        return;
    }
    let kind: String = inp
        .tool_input("kind")
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or("entry")
        .chars()
        .take(32)
        .collect();
    journal::note(env, session, &Ev::MemoryWrite { kind });
}

/// Пути файлов инструмента: `file_path`/`notebook_path`, у `apply_patch` — заголовки патча.
pub fn tool_files(inp: &Input, tool: &str) -> Vec<String> {
    if tool == "apply_patch" {
        return patch_files(&command_of(inp).unwrap_or_default());
    }
    inp.tool_input("file_path")
        .or_else(|| inp.tool_input("notebook_path"))
        .map(str::to_string)
        .into_iter()
        .collect()
}

/// Режим успеха: журнал, знание о файле, очередь изменённых (без сети).
fn success(env: &Env, inp: &Input, s: &Settings) -> Output {
    let tool = inp.str("tool_name").unwrap_or_default();
    let session = inp.session_id.as_deref();
    let journal_on = s.flag_on(Flag::SessionJournal);
    let rels = || -> Vec<String> {
        tool_files(inp, tool)
            .iter()
            .filter_map(|f| refs::ref_path(f, &env.cwd, &env.root).ok())
            .collect()
    };
    match tool {
        "Read" => {
            let rels = rels();
            if let (Some(id), true) = (session, journal_on) {
                for p in &rels {
                    journal::note(env, id, &Ev::Read { path: p.clone() });
                }
            }
            if !s.flag_on(Flag::FileContext) {
                return Output::Empty;
            }
            let mut shown = session.map(|id| cache::read_shown(env, id));
            let lines = file_context_lines(env, shown.as_mut(), &rels);
            if let (Some(id), Some(sh), false) = (session, shown.as_ref(), lines.is_empty()) {
                cache::write_shown(env, id, sh);
            }
            let text = lines.join("\n");
            // вывод без сети — в очередь учёта (M4.5 §9.5)
            if let Some(id) = session {
                cache::note_tokens(env, id, cache::TOKENS_POST_TOOL_LOCAL, &text);
            }
            Output::context(&inp.event, &text)
        }
        t if is_edit_tool(t) => {
            if let Some(id) = session {
                record_edits(env, s, inp, tool, id, journal_on);
            }
            Output::Empty
        }
        "Bash" => {
            if let (Some(id), true) = (session, journal_on)
                && let Some(exe) = command_of(inp).as_deref().and_then(journal::exe_of)
            {
                journal::note(
                    env,
                    id,
                    &Ev::Cmd {
                        exe,
                        ok: true,
                        err_fp: None,
                    },
                );
            }
            Output::Empty
        }
        t if t.eq_ignore_ascii_case(MCP_MEMORY_LOG) => {
            if let (Some(id), true) = (session, journal_on) {
                mcp_memory_write(env, inp, id);
            }
            Output::Empty
        }
        _ => Output::Empty,
    }
}

/// Правка файлов инструментом `tool`: маркер правок, `Ev::Edit` по каждому пути (`journal_on`)
/// и очередь `{path, hash, symbols?}` (при решении). Вызывается и на отказ правки (fix round 1,
/// M-8): патч мог примениться частично.
fn record_edits(env: &Env, s: &Settings, inp: &Input, tool: &str, id: &str, journal_on: bool) {
    if s.on(Kind::Checkpoint) {
        cache::mark_edits(env, id);
    }
    let rels: Vec<String> = tool_files(inp, tool)
        .iter()
        .filter_map(|f| refs::ref_path(f, &env.cwd, &env.root).ok())
        .collect();
    // кэш ссылок читается, только если есть что ставить в очередь
    let refs_cache = if env.solution.is_some() && !rels.is_empty() {
        cache::usable_file_refs(env, env.server_url().as_deref())
    } else {
        None
    };
    let parse_until = Instant::now() + Duration::from_millis(SYMBOLS_PARSE_MS);
    let mut queue = Vec::new();
    for p in &rels {
        if journal_on {
            journal::note(env, id, &Ev::Edit { path: p.clone() });
        }
        if env.solution.is_none() {
            continue;
        }
        let symbolic = refs_cache.as_ref().is_some_and(|c| c.has_symbol_refs(p));
        queue.extend(edited(env, p, symbolic.then_some(parse_until)));
    }
    cache::queue_changed(env, id, &queue);
}

/// Строка очереди правленого файла: нет файла — [`MISSING_HASH`] без символов, больше
/// [`HASH_MAX`] или не читается — `None` (строка в журнал хуков). `parse_until` задан (у пути
/// ссылки на символы), файл кода не больше [`SYMBOLS_MAX_BYTES`] — читается один раз: хэш и
/// хэши тел символов, если полный разбор успел до срока.
fn edited(env: &Env, rel: &str, parse_until: Option<Instant>) -> Option<cache::Changed> {
    let abs = env.root.join(rel);
    let line = |hash: String, symbols| cache::Changed {
        path: rel.to_string(),
        hash,
        symbols,
    };
    let meta = match std::fs::metadata(&abs) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(line(MISSING_HASH.to_string(), None));
        }
        Err(e) => {
            env.log(&format!("хэш правленого файла: {e}"));
            return None;
        }
        Ok(m) => m,
    };
    if meta.len() > HASH_MAX {
        env.log("правленый файл больше 8 МБ — не хэшируется (zam refs check)");
        return None;
    }
    let lang = parse_until.and_then(|_| zam_code::lang_of(rel));
    if let (Some(until), Some(lang), true) = (
        parse_until,
        lang,
        meta.is_file() && meta.len() <= SYMBOLS_MAX_BYTES,
    ) {
        let bytes = std::fs::read(&abs)
            .inspect_err(|e| env.log(&format!("хэш правленого файла: {e}")))
            .ok()?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let left = until.saturating_duration_since(Instant::now());
        let symbols = (!left.is_zero())
            .then(|| {
                let lim = zam_code::Limits {
                    max_file_bytes: SYMBOLS_MAX_BYTES,
                    parse_timeout: left,
                    ..zam_code::Limits::default()
                };
                zam_code::symbol_hashes(lang, &bytes, &lim)
            })
            .flatten();
        return Some(line(hash, symbols));
    }
    refs::hash_file(&abs)
        .inspect_err(|e| env.log(&format!("хэш правленого файла: {e}")))
        .ok()
        .map(|h| line(h, None))
}

/// Строки знания о файле (§6.3) по `file-refs.json` для путей `rels` (относительно корня):
/// `ZeroAIMemory: о файле <path> в памяти: <title> [<kind>] <ref>; …` (до 3 записей; вид v2, как у
/// подсказок сервера и `Trigger::hint`: теги через «·», ссылка без « — »; M6 §4.3, решение
/// координатора задачи 13a).
/// `shown` задан — каждый файл раз за сессию (ключ `file:<путь>` добавляется в `shown`).
pub fn file_context_lines(
    env: &Env,
    mut shown: Option<&mut Shown>,
    rels: &[String],
) -> Vec<String> {
    if rels.is_empty() {
        return Vec::new();
    }
    let server = env.server_url();
    let Some(c) = cache::usable_file_refs(env, server.as_deref()) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for rel in rels {
        let Some(pr) = c.find(rel).filter(|p| !p.items.is_empty()) else {
            continue;
        };
        let key = format!("file:{}", pr.path);
        if let Some(sh) = shown.as_deref_mut() {
            if sh.shown.contains(&key) {
                continue;
            }
            sh.shown.push(key);
        }
        lines.push(file_hint(pr));
    }
    lines
}

/// Строка знания о файле: до [`FILE_ITEMS_MAX`] записей, `stale` — тег «возможно устарело».
/// Ссылки на символ (M5.6 §8; решение составителя плана 13) — первыми; все записи о одном
/// символе — «`ZeroAIMemory: о Sym в <путь> в памяти: <title> [<kind>] <ref>; …`»; смесь
/// символьных и файловых (или разных символов) — «`ZeroAIMemory: о файле <путь> в памяти:
/// Sym: <title> [<kind>] <ref>; <title> [<kind>·возможно устарело] <ref>`».
pub fn file_hint(pr: &PathRefs) -> String {
    let mut items: Vec<&cache::RefItem> = pr.items.iter().collect();
    // устойчиво: символьные первыми, порядок сервера внутри групп
    items.sort_by_key(|i| i.symbol.is_none());
    items.truncate(FILE_ITEMS_MAX);
    let one = items
        .first()
        .and_then(|i| i.symbol.as_deref())
        .filter(|s| items.iter().all(|i| i.symbol.as_deref() == Some(*s)));
    let parts: Vec<String> = items
        .iter()
        .map(|i| {
            // заголовок — с маской секретов до обрезки ([`shown_title`]): сервер M7.1 маскирует
            // его для токена сам, старый — нет
            let one_line = i.title.split_whitespace().collect::<Vec<_>>().join(" ");
            let title = shown_title(&one_line, TITLE_MAX);
            // теги как у `hint_line`: пустые опускаются, без тегов — без скобок
            let stale = if i.state == "stale" {
                "возможно устарело"
            } else {
                ""
            };
            let tags: Vec<&str> = [i.kind.trim(), stale]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect();
            let tag = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join("·"))
            };
            match (&i.symbol, one) {
                (Some(sym), None) => format!("`{sym}`: {title}{tag} {}", i.id),
                _ => format!("{title}{tag} {}", i.id),
            }
        })
        .collect();
    match one {
        Some(sym) => format!(
            "ZeroAIMemory: о `{sym}` в {} в памяти: {}",
            pr.path,
            parts.join("; ")
        ),
        None => format!(
            "ZeroAIMemory: о файле {} в памяти: {}",
            pr.path,
            parts.join("; ")
        ),
    }
}

/// Совпадения `error_regex` кэша (триггер с `tools` — только для них).
fn local_matches<'a>(
    triggers: &'a [Trigger],
    tool: Option<&str>,
    error: &str,
    env: &Env,
) -> Vec<&'a Trigger> {
    let mut out: Vec<&Trigger> = Vec::new();
    for t in triggers
        .iter()
        .filter(|t| t.kind == "error_regex" && t.for_tool(tool))
    {
        if out.iter().any(|o| o.entry_id == t.entry_id) {
            continue;
        }
        match RegexBuilder::new(&t.pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
        {
            Ok(r) if r.is_match(error) => out.push(t),
            Ok(_) => {}
            Err(_) => env.log(&format!("триггер {}: неверный regex", t.id)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hook::input::{Input, from_bytes};

    fn inp(s: &str) -> Input {
        Input::parse(&from_bytes(s.as_bytes().to_vec())).unwrap()
    }

    #[test]
    fn file_hint_format() {
        let item = |id: &str, kind: &str, title: &str, state: &str| cache::RefItem {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            state: state.into(),
            symbol: None,
        };
        let pr = PathRefs {
            path: "src/pool.rs".into(),
            hash: None,
            items: vec![
                item("i:01A", "gotcha", "Пул  соединений\nтечёт", "ok"),
                item("n:01B", "note", "Архитектура", "stale"),
                item("c:01C", "changelog", "Пул v2", "ok"),
                item("c:01D", "changelog", "лишняя", "ok"),
            ],
        };
        assert_eq!(
            file_hint(&pr),
            "ZeroAIMemory: о файле src/pool.rs в памяти: Пул соединений течёт [gotcha] i:01A; \
             Архитектура [note·возможно устарело] n:01B; Пул v2 [changelog] c:01C"
        );
        // пустой вид — без пустого тега, как у `hint_line` (ревью задачи 13a, M9)
        let pr = PathRefs {
            path: "a.rs".into(),
            hash: None,
            items: vec![
                item("i:01E", "", "Без вида", "ok"),
                item("i:01F", " ", "Без вида, устарело", "stale"),
            ],
        };
        assert_eq!(
            file_hint(&pr),
            "ZeroAIMemory: о файле a.rs в памяти: Без вида i:01E; \
             Без вида, устарело [возможно устарело] i:01F"
        );
    }

    /// Заголовок с секретом (старый сервер его не маскирует) — меткой `‹secret›` до обрезки:
    /// ключ на границе [`TITLE_MAX`] не выходит префиксом (круг правок 1 задачи 5 M7.1).
    #[test]
    fn file_hint_title_masked() {
        // ключ из частей: целиком его в исходнике нет (push protection)
        let key = format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5");
        let pad = "п".repeat(TITLE_MAX - 10);
        let item = |title: String| cache::RefItem {
            id: "i:01A".into(),
            kind: "gotcha".into(),
            title,
            state: "ok".into(),
            symbol: None,
        };
        let pr = PathRefs {
            path: "src/a.rs".into(),
            hash: None,
            items: vec![item(format!("ключ {key}")), item(format!("{pad} {key}"))],
        };
        let line = file_hint(&pr);
        assert!(!line.contains(&key) && !line.contains("ghp_aB3"), "{line}");
        assert!(line.contains("ключ ‹secret› [gotcha] i:01A"), "{line}");
    }

    #[test]
    fn error_from_failure_and_stderr() {
        let (cl, cx) = (Agent::Claude, Agent::Codex);
        let f = inp(r#"{"hook_event_name":"PostToolUseFailure","error":"Exit code 1\nbad"}"#);
        assert_eq!(error_text(&f, cl).as_deref(), Some("Exit code 1\nbad"));
        let c = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stdout":"","stderr":"boom"}}"#,
        );
        assert_eq!(error_text(&c, cx).as_deref(), Some("boom"));
        // у Claude Code stderr успешной команды — не ошибка
        assert_eq!(error_text(&c, cl), None);
        let st = inp(r#"{"hook_event_name":"PostToolUse","tool_response":"{\"stderr\":\"x\"}"}"#);
        assert_eq!(error_text(&st, cx).as_deref(), Some("x"));
        let ok = inp(r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"  "}}"#);
        assert_eq!(error_text(&ok, cx), None);
        // код выхода 0 — успех и со stderr, ненулевой — ошибка
        let z = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"warn","exit_code":0}}"#,
        );
        assert_eq!(error_text(&z, cx), None);
        let n = inp(
            r#"{"hook_event_name":"PostToolUse","tool_response":{"stderr":"e","metadata":{"exit_code":2}}}"#,
        );
        assert_eq!(error_text(&n, cx).as_deref(), Some("e"));
    }

    /// M7.1 §4.6 п. 1: код 1 простой команды из списка без stderr — ожидаемый; иначе — ошибка.
    #[test]
    fn expected_exit_cases() {
        for (cmd, err) in [
            ("grep -c x f", "Exit code 1"),
            ("grep -c x a b", "Exit code 1\na:0\nb:0"),
            ("rg foo src", "exit code 1\n"),
            ("diff a b", "Exit code 1\n1c1\n< a\n---\n> b"),
            ("cmp a b", "Exit code 1\na b differ: byte 1, line 1"),
            ("test -f x", "Exit code 1"),
            ("[ -f x ]", "Exit code 1"),
            ("git diff --quiet", "Exit code 1"),
            (
                "git -C repo diff --exit-code -- a.rs",
                "Exit code 1\ndiff --git a/a.rs b/a.rs\n+error: x",
            ),
            ("grep 'a|b' f", "Exit code 1"),
            ("LC_ALL=C /usr/bin/grep x f", "Exit code 1"),
            (r"C:\Git\usr\bin\grep.exe x f", "Exit code 1"),
            ("grep x f", "Exit code 1\nShell cwd was reset to /w"),
            // перенаправления — не составная команда (M-2 ревью)
            ("grep x f 2>&1", "Exit code 1"),
            ("grep x f >out.txt 2>&1", "Exit code 1"),
            ("diff a b &>diff.txt", "Exit code 1"),
            ("diff a b &>>diff.txt", "Exit code 1"),
            // строка контекста `diff -u` с отступом — stdout (M-1 ревью)
            (
                "diff -u a.rs b.rs",
                "Exit code 1\n--- a.rs\n+++ b.rs\n@@ -1,3 +1,3 @@\n struct E {\n     error: String,\n-    code: u8,\n+    code: u16,",
            ),
            ("grep -q '$(x)' f", "Exit code 1"),
        ] {
            assert!(expected_exit(cmd, err), "{cmd} / {err:?}");
        }
        for (cmd, err) in [
            // составная команда
            ("grep x f | wc -l", "Exit code 1"),
            ("grep x f; echo", "Exit code 1"),
            ("grep x f && echo", "Exit code 1"),
            ("grep x f || exit 1", "Exit code 1"),
            ("cd a\ngrep x f", "Exit code 1"),
            // другой код и stderr
            (
                "grep x nofile",
                "Exit code 2\ngrep: nofile: No such file or directory",
            ),
            ("cmp a b", "Exit code 1\ncmp: EOF on a after byte 3, line 1"),
            (
                "git diff --exit-code",
                "Exit code 1\nwarning: in the working copy of 'a.sh', LF will be replaced by CRLF",
            ),
            (
                "test 1 -gt",
                "Exit code 1\nbash: test: 1: unary operator expected",
            ),
            (
                "[ a -gt 1 ]",
                "Exit code 1\n/usr/bin/bash: line 1: [: a: integer expression expected",
            ),
            // сбой вложенной программы прячется в коде внешней (I-1 ревью): подстановки
            (
                "diff <(sort a) <(sort b)",
                "Exit code 1\nsort: cannot read: a: No such file or directory\n0a1\n> x",
            ),
            (
                "diff <(jq . a.json) <(jq . b.json)",
                "Exit code 1\n1c1\n< 1\n---\n> 2",
            ),
            ("cmp <(xxd a) <(xxd b)", "Exit code 1\n- 1 differ: byte 3"),
            ("diff a >(tee log)", "Exit code 1"),
            ("grep -n foo $(find src -name '*.rs')", "Exit code 1"),
            ("grep -n foo `find src -name '*.rs'`", "Exit code 1"),
            (
                "test -n \"$(python find.py)\"",
                "Exit code 1\nTraceback (most recent call last):\n  File \"find.py\", line 3, in \
                 <module>\nFileNotFoundError: [Errno 2] No such file or directory: 'x'",
            ),
            ("test -n \"`cat x`\"", "Exit code 1"),
            // диагностики (I-1 ревью): zsh `(eval)`, `<имя>:<число>:`, Python
            (
                "grep x f",
                "Exit code 1\n(eval):1: no such file or directory: ./tools/x",
            ),
            (
                "test -f x",
                "Exit code 1\n./check.sh:3: test: too many arguments",
            ),
            ("cmp a b", "Exit code 1\nTraceback (most recent call last):"),
            ("diff a b", "Exit code 1\nValueError: bad input"),
            (
                "diff a b",
                "Exit code 1\nrequests.exceptions.ConnectionError: refused",
            ),
            // не из списка
            ("cargo test", "Exit code 1"),
            ("git diff", "Exit code 1"),
            ("git log --exit-code", "Exit code 1"),
            ("! grep x f", "Exit code 1"),
            ("", "Exit code 1"),
            // Codex: stderr без строки кода
            ("diff a b", "diff: b: No such file or directory"),
            ("grep x f", "Error: Exit code 1"),
        ] {
            assert!(!expected_exit(cmd, err), "{cmd} / {err:?}");
        }
    }

    /// M7.1 §4.6 п. 2: классы «не повторяй» — по точным признакам; строка — до 30 токенов.
    #[test]
    fn dont_repeat_classes_and_lines() {
        use DontRepeat::*;
        for (err, class) in [
            ("HTTP 401 invalid_grant", Auth),
            ("gh: HTTP/1.1 401 (https://api.github.com/user)", Auth),
            ("error: invalid_grant: token expired", Auth),
            ("npm ERR! 401 Unauthorized - GET https://registry/x", Auth),
            ("curl: (22) The requested URL returned error: 401", Auth),
            ("HTTPError: 403 Client Error: Forbidden for url", Scope),
            ("error: insufficient_scope: requires repo scope", Scope),
            ("HTTP 403: API rate limit exceeded for user", RateLimit),
            ("429 Too Many Requests; Retry-After: 30", RateLimit),
            ("error: insufficient_quota (HTTP 429)", Quota),
            ("Monthly quota exceeded for this key", Quota),
            ("No space left on device (os error 28)", NoSpace),
            ("ENOSPC: no space left, write", NoSpace),
            ("Недостаточно места на диске. (os error 112)", NoSpace),
            ("status: 422 Unprocessable Entity", BadRequest),
            ("HTTP 400 Bad Request: field `x`", BadRequest),
        ] {
            assert_eq!(dont_repeat_class(err), Some(class), "{err}");
        }
        for err in [
            "assertion failed: left: 401, right: 200",
            "error[E0401]: can't use generic parameters",
            "test result: FAILED. 401 passed; 3 failed",
            "permission denied",
            "error: could not find `Cargo.toml`",
            "HTTP 4010 weird",
        ] {
            assert_eq!(dont_repeat_class(err), None, "{err}");
        }
        assert_eq!(
            RateLimit.line("429 Too Many Requests\nRetry-After: 30"),
            "ZeroAIMemory: лимит запросов (429) — выжди Retry-After (30 с), не повторяй сразу"
        );
        for c in DontRepeat::ALL {
            let line = c.line("Retry-After: 120");
            assert!(line.starts_with("ZeroAIMemory: "), "{line}");
            assert!(cache::estimate_tokens(&line) <= 30, "{line}");
        }
    }

    /// M-4 ревью задачи 8: тест-раннеры — по программе любой простой команды строки.
    #[test]
    fn test_runners_by_program() {
        for cmd in [
            "cargo test -p zam-server --test recall",
            "cargo +nightly nextest run",
            "cd crates && cargo test",
            "RUST_LOG=debug cargo test",
            "pytest -x tests/",
            "python -m pytest -q",
            "npm test",
            "pnpm run test -- --watch=false",
            "npx jest src/a.test.ts",
            "npx vitest run",
            "vitest",
            "go test ./...",
            r"C:\Python312\python.exe -m pytest",
        ] {
            assert!(is_test_runner(cmd), "{cmd}");
        }
        for cmd in [
            "cargo build",
            "cargo run -- test",
            "gh api user",
            "curl https://x/test",
            "npm install",
            "go build ./...",
            "python test.py",
            "echo cargo test",
        ] {
            assert!(!is_test_runner(cmd), "{cmd}");
        }
    }
}
