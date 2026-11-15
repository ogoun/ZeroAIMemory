//! `zam mcp` — stdio-прокси к `/mcp` (§13.1, §13.2) для агентов без HTTP-транспорта MCP.
//!
//! - Читает stdin построчно: строка — одно сообщение JSON-RPC (запрос, уведомление, ответ или
//!   пачка). Каждое уходит отдельным `POST /mcp` с `Authorization: Bearer`, `X-Zam-Solution`
//!   (из `solution.toml`) и `X-Zam-Machine`; `Origin` не шлётся.
//! - **Сессия** (M6 §7.1, [`SessionSource`]): `X-Zam-Session` — по порядку `ZAM_SESSION_ID`,
//!   затем файл привязки хука `session-start` по своей цепочке предков
//!   ([`hook::binding::find_binding`]; цепочка — один раз при запуске, не прочиталась —
//!   снова при поиске; поиск — не чаще раза в [`BINDING_RECHECK`], так что новая сессия после
//!   `/clear` приходит со следующим вызовом). `CLAUDE_CODE_SESSION_ID` не читается (ревью задачи 3, I4): по
//!   журналу изменений Claude Code 2.1.132 она есть у подпроцессов Bash, но не у
//!   MCP-подпроцесса, и в справочнике переменных её нет. Нет ни одного источника — без
//!   заголовка, сессию выбирает сервер неявно (R13), без учёта выданного в сессии. `ZAM_SESSION` и `session.json` хуков, как и раньше, на MCP
//!   не влияют (fix round 1, I-4): `session.json` общий у параллельных агентов проекта.
//! - Ответ `application/json` пишется в stdout одной строкой; `text/event-stream` (SSE)
//!   разворачивается: каждое событие `data:` — отдельная строка. 202/204 — без вывода.
//! - После `initialize` прокси шлёт `MCP-Protocol-Version` с согласованной версией, а
//!   выданный сервером `Mcp-Session-Id` — во всех следующих запросах.
//! - Каждый запрос с `id` получает ответ (fix round 1, I-5): сетевая ошибка, HTTP-ошибка,
//!   обрыв или тайм-аут тела, превышение предела тела, SSE без ответа на `id` — ответ JSON-RPC
//!   `error` с кодом −32000 и причиной; у пачки — на каждый `id` без ответа. Уведомление без
//!   ответа — строка в stderr. Неразборчивая строка (не JSON, не UTF-8) — `error` −32700, работа
//!   продолжается (M-7).
//! - Сообщения обрабатываются параллельно, не больше [`MAX_INFLIGHT`] сразу (`initialize` —
//!   первым); stdout пишется построчно под блокировкой. Конец stdin — выход с кодом 0 после всех
//!   ответов. Токен и тексты сообщений никуда, кроме сервера, не пишутся.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Ctx, hook};
use crate::client::{CliError, Client};

/// Как часто `zam mcp` заново ищет привязку (ревью задачи 3, m2): после `/clear` и `resume`
/// `session-start` перезаписывает файлы, и новая сессия должна прийти уже со следующим
/// вызовом. Поиск — до [`hook::binding::BINDING_ANCESTORS_MAX`] файлов, не чаще раза в секунду.
pub const BINDING_RECHECK: Duration = Duration::from_secs(1);

/// Состояние привязки: цепочка предков, найденная сессия и время последнего поиска.
#[derive(Default)]
struct Bound {
    /// Цепочка — один раз при запуске (I2); не прочиталась — снова при поиске (m4).
    chain: Vec<hook::procs::Proc>,
    session: Option<String>,
    checked: Option<Instant>,
}

/// Источник `X-Zam-Session` (см. модуль): переменная окружения или файл привязки.
struct SessionSource {
    /// `ZAM_SESSION_ID` — на всё время процесса.
    fixed: Option<String>,
    /// Корень проекта: там `.zeroaimemory/cache/agent-<pid>.json`.
    root: PathBuf,
    bound: Mutex<Bound>,
}

impl SessionSource {
    fn new(get: impl Fn(&str) -> Option<String>, root: PathBuf) -> SessionSource {
        let fixed = hook::session_var(get(zam_core::MCP_SESSION_ENV));
        // с переменной цепочка не нужна
        let chain = if fixed.is_some() {
            Vec::new()
        } else {
            hook::binding::chain()
        };
        SessionSource {
            fixed,
            root,
            bound: Mutex::new(Bound {
                chain,
                ..Bound::default()
            }),
        }
    }

    /// Сессия запроса в момент `now`: поиск привязки не чаще [`BINDING_RECHECK`], между
    /// поисками — прежний результат.
    fn current_at(&self, now: Instant) -> Option<String> {
        if let Some(s) = &self.fixed {
            return Some(s.clone());
        }
        let mut b = self.bound.lock().unwrap_or_else(|e| e.into_inner());
        let due = b
            .checked
            .is_none_or(|t| now.saturating_duration_since(t) >= BINDING_RECHECK);
        if due {
            if b.chain.is_empty() {
                b.chain = hook::binding::chain();
            }
            b.session = hook::binding::find_binding(&self.root, &b.chain).map(|f| f.session);
            b.checked = Some(now);
        }
        b.session.clone()
    }

    fn current(&self) -> Option<String> {
        self.current_at(Instant::now())
    }
}

/// Состояние сеанса MCP, общее для потоков запросов.
#[derive(Default)]
struct Session {
    protocol: Option<String>,
    session_id: Option<String>,
}

/// Предел тела ответа, байт.
const BODY_MAX: u64 = 64 * 1024 * 1024;
/// Предел длины строки stdin, байт (fix round 3, N-5): длиннее — ошибка −32600, остаток
/// строки до `\n` пропускается.
pub const LINE_MAX: usize = 4 * 1024 * 1024;
/// Предел одновременных запросов (M-7).
pub const MAX_INFLIGHT: usize = 16;
/// Код ошибки JSON-RPC прокси (транспорт).
const PROXY_ERROR: i64 = -32000;

fn write_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out
        .write_all(line.as_bytes())
        .and_then(|_| out.write_all(b"\n"))
        .and_then(|_| out.flush());
}

/// JSON одной строкой (текст не JSON — как есть, без переводов строк).
fn compact(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    Some(match serde_json::from_str::<Value>(t) {
        Ok(v) => v.to_string(),
        Err(_) => t.replace(['\r', '\n'], " "),
    })
}

fn rpc_error(id: &Value, code: i64, msg: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}).to_string()
}

/// `id` запросов сообщения (у уведомления и ответа клиента их нет); у пачки — всех запросов.
fn request_ids(msg: &Value) -> Vec<Value> {
    let one = |m: &Value| {
        (m.get("method").is_some())
            .then(|| m.get("id").cloned())
            .flatten()
            .filter(|id| !id.is_null())
    };
    match msg {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        m => one(m).into_iter().collect(),
    }
}

/// `id` ответов в сообщении сервера (объект или пачка).
fn response_ids(text: &str) -> Vec<Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(a)) => a.iter().filter_map(|m| m.get("id").cloned()).collect(),
        Ok(m) => m.get("id").cloned().into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

/// События SSE: строки `data:` до пустой строки — одно сообщение. Ошибка чтения — `Err`
/// (обрыв, тайм-аут, предел тела).
fn sse(reader: impl Read, mut emit: impl FnMut(&str)) -> Result<(), String> {
    let mut data: Vec<String> = Vec::new();
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty() {
                emit(&data.join("\n"));
                data.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
    }
    if !data.is_empty() {
        emit(&data.join("\n"));
    }
    Ok(())
}

/// Запомнить согласованную версию протокола из ответа `initialize`.
fn note_protocol(state: &Mutex<Session>, msg: &str, was_initialize: bool) {
    if !was_initialize {
        return;
    }
    if let Ok(v) = serde_json::from_str::<Value>(msg)
        && let Some(p) = v["result"]["protocolVersion"].as_str()
    {
        state.lock().unwrap_or_else(|e| e.into_inner()).protocol = Some(p.to_string());
    }
}

/// Ошибка на каждый `id` из `ids`, на который не было ответа; без запросов — в stderr.
fn fail_unanswered(ids: &[Value], answered: &[Value], reason: &str) {
    let missing: Vec<&Value> = ids.iter().filter(|id| !answered.contains(id)).collect();
    if ids.is_empty() {
        eprintln!("zam mcp: {reason}");
    }
    for id in missing {
        write_line(&rpc_error(id, PROXY_ERROR, reason));
    }
}

/// Одно сообщение: запрос к серверу и вывод ответа.
fn forward(client: &Client, state: &Mutex<Session>, source: &SessionSource, line: &str) {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            write_line(&rpc_error(
                &Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ));
            return;
        }
    };
    let ids = request_ids(&msg);
    let initialize = msg.get("method").and_then(Value::as_str) == Some("initialize");
    let (protocol, sid) = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        (s.protocol.clone(), s.session_id.clone())
    };
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(p) = protocol.as_deref().filter(|_| !initialize) {
        headers.push(("MCP-Protocol-Version", p));
    }
    if let Some(s) = sid.as_deref() {
        headers.push(("Mcp-Session-Id", s));
    }
    let agent_session = source.current();
    if let Some(s) = agent_session.as_deref() {
        headers.push(("X-Zam-Session", s));
    }
    let body = msg.to_string();
    let mut resp = match client.post_mcp(body.as_bytes(), &headers) {
        Ok(r) => r,
        Err(e) => return fail_unanswered(&ids, &[], &e.msg),
    };
    let status = resp.status().as_u16();
    if let Some(s) = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        state.lock().unwrap_or_else(|e| e.into_inner()).session_id = Some(s.to_string());
    }
    let sse_body = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/event-stream"));
    if !(200..300).contains(&status) {
        let text = resp
            .body_mut()
            .with_config()
            .limit(BODY_MAX)
            .read_to_string()
            .unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["detail"].as_str().map(String::from))
            .unwrap_or_else(|| text.chars().take(200).collect());
        return fail_unanswered(&ids, &[], &format!("HTTP {status}: {detail}"));
    }
    let mut answered: Vec<Value> = Vec::new();
    if sse_body {
        let reader = resp.into_body().into_with_config().limit(BODY_MAX).reader();
        let res = sse(reader, |data| {
            note_protocol(state, data, initialize);
            answered.extend(response_ids(data));
            if let Some(l) = compact(data) {
                write_line(&l);
            }
        });
        let reason = match res {
            Ok(()) => "поток SSE закончился без ответа на запрос".to_string(),
            Err(e) => format!("обрыв потока SSE: {e}"),
        };
        return fail_unanswered_quiet(&ids, &answered, &reason);
    }
    match resp
        .body_mut()
        .with_config()
        .limit(BODY_MAX)
        .read_to_string()
    {
        Ok(text) => {
            note_protocol(state, &text, initialize);
            answered.extend(response_ids(&text));
            if let Some(l) = compact(&text) {
                write_line(&l);
            }
            fail_unanswered_quiet(&ids, &answered, "сервер не ответил на запрос");
        }
        Err(e) => fail_unanswered(&ids, &[], &format!("чтение ответа сервера: {e}")),
    }
}

/// Как [`fail_unanswered`], но без строки в stderr, когда запросов не было (уведомление).
fn fail_unanswered_quiet(ids: &[Value], answered: &[Value], reason: &str) {
    if !ids.is_empty() {
        fail_unanswered(ids, answered, reason);
    }
}

pub fn run(ctx: Ctx) -> Result<(), CliError> {
    let mut client = ctx.client;
    // не `session.json` хуков (I-4): сессия — из [`SessionSource`] (M6 §7.1)
    client.set_session(None);
    // долгие инструменты (вложения, реранк на CPU): ответ ждётся до 10 мин (m1)
    client.set_timeouts(crate::client::Timeouts::Mcp);
    let client = Arc::new(client);
    let state = Arc::new(Mutex::new(Session::default()));
    let cwd = std::env::current_dir().unwrap_or_default();
    let source = Arc::new(SessionSource::new(
        |k| std::env::var(k).ok(),
        hook::project_root(&cwd, &super::home_dir()),
    ));
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut stdin = std::io::stdin().lock();
    let mut buf = Vec::new();
    loop {
        match read_line_limited(&mut stdin, &mut buf, LINE_MAX) {
            Ok(Line::Eof) | Err(_) => break,
            Ok(Line::TooLong) => {
                write_line(&rpc_error(
                    &Value::Null,
                    -32600,
                    &format!("сообщение длиннее {LINE_MAX} байт — пропущено"),
                ));
                continue;
            }
            Ok(Line::Ok) => {}
        }
        let line = match std::str::from_utf8(&buf) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                write_line(&rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("parse error: не UTF-8 ({e})"),
                ));
                continue;
            }
        };
        if line.is_empty() {
            continue;
        }
        // `initialize` — до остальных: от него зависят версия протокола и сеанс
        let first = serde_json::from_str::<Value>(&line)
            .is_ok_and(|v| v.get("method").and_then(Value::as_str) == Some("initialize"));
        // не больше MAX_INFLIGHT запросов сразу
        workers.retain(|w| !w.is_finished());
        while workers.len() >= MAX_INFLIGHT {
            let _ = workers.remove(0).join();
            workers.retain(|w| !w.is_finished());
        }
        let (c, s, src) = (client.clone(), state.clone(), source.clone());
        let h = std::thread::spawn(move || forward(&c, &s, &src, &line));
        if first {
            let _ = h.join();
        } else {
            workers.push(h);
        }
    }
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

/// Итог чтения строки.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    Ok,
    /// Длиннее предела: прочитано и отброшено до `\n` включительно.
    TooLong,
    Eof,
}

/// Строка до `\n` в `buf` (без накопления больше `max` байт).
fn read_line_limited(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> std::io::Result<Line> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(match (any, too_long) {
                (false, _) => Line::Eof,
                (true, true) => Line::TooLong,
                (true, false) => Line::Ok,
            });
        }
        any = true;
        let (part, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (&chunk[..=i], true),
            None => (chunk, false),
        };
        let n = part.len();
        if !too_long {
            if buf.len() + n > max + 1 {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(part);
            }
        }
        r.consume(n);
        if done {
            return Ok(if too_long { Line::TooLong } else { Line::Ok });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-mcp")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn session_env_beats_binding_claude_var_ignored() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let root = tmp_root("env");
        hook::procs::set_fake_ancestors(vec![hook::procs::Proc {
            pid: 801,
            name: "claude".into(),
        }]);
        hook::binding::write_binding(&root, "bound").unwrap();
        let s = SessionSource::new(env(&[("ZAM_SESSION_ID", "a")]), root.clone());
        assert_eq!(s.current().as_deref(), Some("a"));
        // CLAUDE_CODE_SESSION_ID MCP-подпроцессу не передаётся (I4) — не читается
        let s = SessionSource::new(
            env(&[("ZAM_SESSION_ID", " "), ("CLAUDE_CODE_SESSION_ID", "b")]),
            root,
        );
        assert_eq!(s.current().as_deref(), Some("bound"));
    }

    /// I2, m2, m4: поиск привязки не чаще раза в секунду; новая сессия после `/clear` — со
    /// следующей проверкой; цепочка, не прочитанная при запуске, читается снова.
    #[test]
    fn binding_refresh_schedule() {
        use hook::procs::{Proc, set_fake_ancestors};
        let root = tmp_root("refresh");
        // при запуске цепочка не прочиталась
        set_fake_ancestors(Vec::new());
        let s = SessionSource::new(|_| None, root.clone());
        set_fake_ancestors(vec![Proc {
            pid: 901,
            name: "claude".into(),
        }]);
        let t0 = Instant::now();
        assert_eq!(s.current_at(t0), None);
        hook::binding::write_binding(&root, "s1").unwrap();
        assert_eq!(s.current_at(t0 + Duration::from_millis(500)), None);
        let t1 = t0 + BINDING_RECHECK;
        assert_eq!(s.current_at(t1).as_deref(), Some("s1"));
        // `/clear`: файл перезаписан новой сессией — со следующей проверкой
        hook::binding::write_binding(&root, "s2").unwrap();
        assert_eq!(
            s.current_at(t1 + Duration::from_millis(500)).as_deref(),
            Some("s1")
        );
        let t2 = t1 + BINDING_RECHECK;
        assert_eq!(s.current_at(t2).as_deref(), Some("s2"));
        // файл пропал — без привязки
        std::fs::remove_file(hook::binding::binding_path(&root, 901)).unwrap();
        assert_eq!(s.current_at(t2 + BINDING_RECHECK), None);
    }

    #[test]
    fn sse_events_become_lines() {
        let body = "event: message\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n: comment\ndata: {\"b\": 2}\n\ndata: x";
        let mut got = Vec::new();
        sse(body.as_bytes(), |d| got.push(compact(d).unwrap())).unwrap();
        assert_eq!(got, ["{\"a\":1}", "{\"b\":2}", "x"]);
    }

    #[test]
    fn compact_single_line() {
        assert_eq!(
            compact("{\n \"a\": [1,\n2]\n}").as_deref(),
            Some("{\"a\":[1,2]}")
        );
        assert_eq!(compact("  "), None);
    }

    #[test]
    fn long_line_skipped_to_newline() {
        let data = format!("{}\nok\n{}", "x".repeat(20), "y".repeat(3));
        let mut r = std::io::BufReader::with_capacity(4, data.as_bytes());
        let mut buf = Vec::new();
        assert_eq!(
            read_line_limited(&mut r, &mut buf, 8).unwrap(),
            Line::TooLong
        );
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Line::Ok);
        assert_eq!(buf, b"ok\n");
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Line::Ok);
        assert_eq!(buf, b"yyy");
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Line::Eof);
    }

    #[test]
    fn ids_of_requests_and_batches() {
        let m: Value = serde_json::from_str(
            r#"[{"jsonrpc":"2.0","id":1,"method":"a"},{"jsonrpc":"2.0","method":"n"},
                {"jsonrpc":"2.0","id":"x","method":"b"},{"jsonrpc":"2.0","id":5,"result":{}}]"#,
        )
        .unwrap();
        assert_eq!(request_ids(&m), [json!(1), json!("x")]);
        assert!(request_ids(&json!({"jsonrpc":"2.0","method":"n"})).is_empty());
        assert_eq!(
            response_ids(r#"[{"id":1,"result":{}},{"id":"x","error":{}}]"#),
            [json!(1), json!("x")]
        );
    }
}
