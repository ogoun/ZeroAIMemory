//! `zam hook session-start` (`SessionStart`, бюджет 300 мс; §10.3).
//!
//! Параллельно (бюджет общий): `POST /sessions {id, agent, solution}`, `/recall(session_start)`
//! с `source` из stdin и `agent_block_version` (R2), `GET /triggers` с `If-None-Match` (кэш
//! триггеров). Вывод — дайджест `text`; `hooks` ответа запоминаются (R5).
//!
//! Выключенный в клиенте `session_start` гасит только вывод: `/recall` вызывается всё равно —
//! ради `hooks` сервера (fix round 1, m-5) и с `record: false` (без `session_seen` и `usage`,
//! fix round 2), текст отбрасывается; регистрация сессии и кэш
//! триггеров нужны `checkpoint`, MCP (R13) и `pre-tool`. Выключенный на сервере — дайджест,
//! полученный в том же ответе, не выводится.
//! На `source` `compact` и `clear` (контекст агента потерян) показанное `pre-tool` в сессии
//! сбрасывается, факт правок остаётся.
//! M6 §7.2: новую эпоху `session_seen` на `source` `compact | resume | clear` начинает сам
//! сервер в этом же `/recall` (`source` уже в теле). Выключенный в клиенте `recovery` убирает
//! `source` из тела — тогда хук шлёт отдельный `POST /sessions {id, new_epoch: true}`
//! ([`super::checkpoint::new_epoch`]) параллельно с `/recall`, в том же бюджете. Клиент
//! v0.0.10-m5.7 с выключенным `recovery` эпохи на старте не начинает (матрица 13b/13c).
//! M4.5: в том же параллельном наборе — `GET /file-refs` с `If-None-Match` (кэш знания о файле
//! `file-refs.json`; ключ `file_context` выключен — не запрашивается) и отправка очередей
//! изменённых файлов всех сессий проекта (`POST /files/changed`, [`cache::flush_changed`]).
//! Очереди уходят **до** `GET /file-refs` (задача 9, замечание ревью задачи 8): индекс берётся
//! уже с устарелостью, которую выставил сервер по отправленным хэшам.
//! M4.5 §9.2: на `source` `compact` и `resume` сервер строит пакет восстановления, а хук
//! дописывает после его текста сводку журнала сессии ([`journal::render_summary`], ключи
//! `recovery` и `session_journal`); общий предел — прежний ([`output::with_tail`]). Сервер
//! недоступен — только сводка; журнала нет — только текст сервера.
//! M5.6 §5: после запросов хука (их бюджет — не делить с синхронизацией за тот же сервер) —
//! запуск фоновой `zam code sync --background`, если пора
//! ([`crate::cmd::code::auto::maybe_spawn_sync`]; сервер для запуска не нужен).
//! M6 §12: `hooks.holdout = p` (клиент, 0..=0,5) — контрольная группа агентного A/B: сессия
//! попадает в неё с вероятностью p по хэшу своего id ([`holdout_pick`]: решение одно и то же на
//! каждом старте сессии, в том числе после compact, и не требует состояния). Такой сессии хук
//! шлёт отдельный `POST /sessions {id, holdout: true}` **до** `/recall` того же потока: сервер
//! отдаёт пустой `text` уже первому дайджесту. У метки свой срок [`HOLDOUT_MS`], `/recall` после
//! неё — с новым клиентом по остатку бюджета (ревью задачи 12, I2: общий бюджет не
//! превышается). Старый сервер отвечает 400 (неизвестное поле) — хук молча продолжает без
//! контрольной группы. Метка прошла или ответ `/recall` несёт `holdout: true` (метка на сервере —
//! в том числе поставленная раньше или унаследованная) — маркер кэша сессии
//! ([`cache::mark_holdout`]: тишина всех хуков сессии, и без сводки журнала); ответ без неё —
//! маркер снимается.
//! M6 §7.1: каждый старт с `session_id` (новая сессия, `/clear`, `resume`, `compact`) пишет
//! привязку `zam mcp` к сессии — `cache/agent-<pid>.json` ([`binding::write_binding`]) без сети.
//! M6, задача 5: локальная запись идёт в своём потоке параллельно запросам и не тратит их
//! бюджет. Сначала обязательное — `session.json`, привязка, сброс показанного ([`critical`]):
//! хук ждёт его после запросов до [`CRITICAL_WAIT_MAX`] (неверная сессия у `zam mcp` хуже
//! опоздания хука); затем уборка ([`best_effort`]: срок кэша, субагенты, уплотнение очередей) —
//! только в остатке бюджета.
use std::path::Path;
use std::time::Duration;

use crate::client::{CliError, EXIT_UNREACHABLE};

use serde_json::{Value, json};

use super::binding;
use super::cache;
use super::input::Input;
use super::journal;
use super::output::{self, Output};
use super::{Env, Flag, HookFlags, Kind, Settings};
use crate::client::read_json;

/// Предел файла инструкции при поиске маркера блока, байт.
const MD_MAX: u64 = 1024 * 1024;
/// Маркер начала блока ZeroAIMemory в CLAUDE.md/AGENTS.md.
const BEGIN: &str = "<!-- zeroaimemory:begin";

pub fn run(env: &Env, inp: &Input, s: &Settings) -> Output {
    let out = digest(env, inp, s);
    // карта кода (M5.6 §5): фоновая синхронизация, если пора; хук её не ждёт (≤ 10 мс)
    crate::cmd::code::auto::maybe_spawn_sync(env, s);
    out
}

/// Потолок ожидания записи привязки сессии и `session.json` после запросов хука (ревью задачи
/// 5 M6, I-1): опоздание хука лучше неверной сессии у `zam mcp` (§7.1).
pub const CRITICAL_WAIT_MAX: Duration = Duration::from_millis(1500);

/// Обязательная запись старта (без сети): текущая сессия (`session.json`), привязка `zam mcp`
/// (§7.1) и сброс показанного после `compact`/`clear`. Хук ждёт её до [`CRITICAL_WAIT_MAX`].
fn critical(env: &Env, session: Option<&str>, source: Option<&str>) {
    let Some(id) = session else {
        return;
    };
    cache::write_current_session(env, id);
    // привязка `zam mcp` к сессии (M6 §7.1): на предков до процесса агента
    if let Err(e) = binding::write_binding(&env.root, id) {
        env.log(&format!("привязка MCP к сессии: {e}"));
    }
    if matches!(source, Some("compact" | "clear")) {
        let mut sh = cache::read_shown(env, id);
        if !sh.shown.is_empty() {
            sh.shown.clear();
            cache::write_shown(env, id, &sh);
        }
    }
}

/// Уборка «по возможности»: срок кэша, очередь запусков субагентов, уплотнение очередей
/// изменённых файлов больше 4 МиБ. Хук ждёт её только в остатке бюджета; не успела — процесс
/// выходит без неё (файлы пишутся атомарно, брошенные `.tmp` убирает следующий `gc_cache`).
/// Уплотнение держит замок очереди, и `flush_changed` того же старта ждёт его не дольше 50 мс
/// (`CHANGED_LOCK_WAIT`), — бюджет запросов оно не съедает.
fn best_effort(env: &Env, session: Option<&str>) {
    cache::gc_cache(env);
    if let Some(id) = session {
        // просроченные запуски субагентов (§9.4) не ждут следующего `subagent-start`
        cache::prune_spawns(env, id);
    }
    cache::compact_changed_queues(env);
}

/// Регистрация сессии, дайджест, кэши и очереди (см. модуль).
fn digest(env: &Env, inp: &Input, s: &Settings) -> Output {
    let session = inp.session_id.as_deref();
    let source = inp.str("source");
    // локальная запись — в своём потоке, параллельно запросам (M6, задача 5, A): снимок
    // процессов и запись файлов на занятом HDD съедали бюджет до запроса («бюджет исчерпан»,
    // пакет восстановления без сервера). Сначала — обязательное, затем уборка.
    let (crit_rx, rest_rx) = {
        let (crit_tx, crit_rx) = std::sync::mpsc::channel::<()>();
        let (rest_tx, rest_rx) = std::sync::mpsc::channel::<()>();
        let env2 = env.clone();
        let (sid, src) = (session.map(str::to_string), source.map(str::to_string));
        std::thread::spawn(move || {
            critical(&env2, sid.as_deref(), src.as_deref());
            let _ = crit_tx.send(());
            best_effort(&env2, sid.as_deref());
            let _ = rest_tx.send(());
        });
        (crit_rx, rest_rx)
    };
    let out = digest_net(env, inp, s, session, source);
    // привязку и `session.json` — дождаться (потолок — [`CRITICAL_WAIT_MAX`]), уборку — только
    // в остатке бюджета
    if crit_rx.recv_timeout(CRITICAL_WAIT_MAX).is_err() {
        env.log("привязка MCP к сессии: запись не завершилась за 1,5 с");
    }
    if let Some(left) = env.http_left() {
        let _ = rest_rx.recv_timeout(left);
    }
    out
}

/// Запросы к серверу старта и дайджест (см. модуль).
fn digest_net(
    env: &Env,
    inp: &Input,
    s: &Settings,
    session: Option<&str>,
    source: Option<&str>,
) -> Output {
    // сводка журнала до сжатия (§9.2): только после `compact`/`resume`
    let summary = |st: &Settings| -> String {
        match session {
            Some(id)
                if matches!(source, Some("compact" | "resume"))
                    && st.flag_on(Flag::Recovery)
                    && st.flag_on(Flag::SessionJournal) =>
            {
                journal::render_summary(&journal::summarize(&journal::read(&env.root, id)))
            }
            _ => String::new(),
        }
    };
    // сервер недоступен — только сводка
    let local = |st: &Settings| -> Output {
        if !st.on(Kind::SessionStart) {
            return Output::Empty;
        }
        Output::context(&inp.event, &summary(st))
    };
    let Some(client) = env.client() else {
        return local(s);
    };
    // сводка журнала — заранее: её место вычитается из предела канала (ревью 8a M6, C1)
    let tail_pre = summary(s);
    let solution = env.solution.as_ref().map(|x| x.id.clone());
    // `recovery` выключен в клиенте: `source` к серверу не уходит — эпоху начинает хук сам
    let epoch_source = matches!(source, Some("compact" | "resume" | "clear"))
        && s.client.get(Flag::Recovery) == Some(false);
    let (reg, recall, trig) = std::thread::scope(|sc| {
        let bump = session
            .filter(|_| epoch_source)
            .map(|id| sc.spawn(|| super::checkpoint::new_epoch(env, &client, id)));
        let reg = session.map(|id| {
            let body = json!({"id": id, "agent": env.agent.name(), "solution": solution});
            let c = &client;
            sc.spawn(move || c.post_json("/sessions", &body).map(|_| ()))
        });
        // контрольная группа A/B (M6 §12): метка — до `/recall`, в том же потоке
        let hold = session.filter(|id| holdout_pick(id, s.holdout()));
        let recall = {
            let mut body = json!({
                "event": "session_start",
                "session_id": session,
                "solution": solution,
                "source": source,
                // строка карты кода — по `[hooks] code_map` клиента (M5.6 §10)
                "code_map": s.client.get(Flag::CodeMap) != Some(false),
                // предел канала минус сводка журнала: весь текст укладывает сервер
                "output_limit": output::output_limit(env.agent, &tail_pre),
            });
            // `recovery` выключен в клиенте — сервер не строит пакет (по `source` он его и
            // выбирает; иначе `source` только в журнал DEBUG)
            if s.client.get(Flag::Recovery) == Some(false) {
                body["source"] = Value::Null;
            }
            if !s.client_on(Kind::SessionStart) {
                body["record"] = json!(false);
            }
            if let Some(v) = block_version(&env.cwd, &env.root, env.agent) {
                body["agent_block_version"] = json!(v);
            }
            let c = &client;
            sc.spawn(move || {
                let Some(id) = hold else {
                    return (c.post_json("/recall", &body).and_then(read_json), false);
                };
                // метка — со своим сроком, `/recall` — новым клиентом по остатку бюджета
                let held = env
                    .client_within(Duration::from_millis(HOLDOUT_MS))
                    .is_some_and(|h| set_holdout(env, &h, id));
                let r = match env.client() {
                    Some(fresh) => fresh.post_json("/recall", &body).and_then(read_json),
                    None => Err(CliError::new(EXIT_UNREACHABLE, zam_core::HOOK_BUDGET_SPENT)),
                };
                (r, held)
            })
        };
        let trig = sc.spawn(|| cache::refresh_triggers(env, &client, false));
        // очередь учёта локального вывода прошлой сессии хуков (M4.5 §9.5)
        let tokens = session
            .filter(|_| cache::tokens_queued(env, session))
            .map(|id| sc.spawn(|| cache::flush_tokens(env, &client, id)));
        // сначала очереди изменённых файлов, затем индекс ссылок в остатке бюджета: иначе
        // индекс может прийти без устарелости, которую выставит эта же отправка
        let want_refs = s.flag_on(Flag::FileContext) && env.solution.is_some();
        let c = &client;
        let refs = sc.spawn(move || {
            cache::flush_changed(env, c, None);
            want_refs.then(|| cache::refresh_file_refs(env, c))
        });
        match refs.join() {
            Ok(Some(Err(e))) => env.log(&format!("кэш file-refs не обновлён: {e}")),
            Err(_) => env.log("files/changed, file-refs: паника"),
            _ => {}
        }
        if let Some(h) = tokens
            && h.join().is_err()
        {
            env.log("sessions(tokens): паника");
        }
        if let Some(h) = bump
            && h.join().is_err()
        {
            env.log("sessions(new_epoch): паника");
        }
        (reg.map(|h| h.join()), recall.join(), trig.join())
    });
    match reg {
        Some(Ok(Err(e))) => env.log(&format!("регистрация сессии: {e}")),
        Some(Err(_)) => env.log("регистрация сессии: паника"),
        _ => {}
    }
    match trig {
        Ok(Ok((_, warnings))) => {
            for w in warnings {
                env.log(&format!("триггеры: предупреждение сервера: {w}"));
            }
        }
        Ok(Err(e)) => env.log(&format!("кэш триггеров не обновлён: {e}")),
        Err(_) => env.log("кэш триггеров: паника"),
    }
    let v: Value = match recall {
        Ok((Ok(v), held)) => {
            // маркер контрольной группы (M6 §12): по метке и по ответу сервера
            if let Some(id) = session {
                cache::mark_holdout(env, id, held || v["holdout"].as_bool() == Some(true));
            }
            v
        }
        Ok((Err(e), held)) => {
            if let (Some(id), true) = (session, held) {
                cache::mark_holdout(env, id, true);
            }
            env.log(&format!("recall(session_start): {e}"));
            return local(s);
        }
        Err(_) => {
            env.log("recall(session_start): паника");
            return local(s);
        }
    };
    for w in cache::strings(&v["warnings"]) {
        env.log(&format!(
            "recall(session_start): предупреждение сервера: {w}"
        ));
    }
    let mut server = s.server;
    if let Ok(h) = serde_json::from_value::<HookFlags>(v["hooks"].clone())
        && v["hooks"].is_object()
    {
        cache::write_server_hooks(env, client.base(), h);
        server = h;
    }
    let now = Settings {
        client: s.client,
        server,
        keys: s.keys,
    };
    // контрольная группа A/B (M6 §12): ни дайджеста, ни сводки журнала
    if !now.on(Kind::SessionStart) || v["holdout"].as_bool() == Some(true) {
        return Output::Empty;
    }
    // сводка — та же, если флаги сервера её не изменили (журнал читается один раз)
    let same = |f: Flag| now.flag_on(f) == s.flag_on(f);
    let tail = if same(Flag::Recovery) && same(Flag::SessionJournal) {
        tail_pre
    } else {
        summary(&now)
    };
    let head = v["text"].as_str().unwrap_or_default();
    let joined = if tail.trim().is_empty() {
        head.to_string()
    } else {
        format!("{}\n\n{}", head.trim_end(), tail.trim())
    };
    if let Some(m) = output::overflow(&joined, env.agent) {
        env.log(&format!("session-start: {m}"));
    }
    let text = output::with_tail(head, &tail, env.agent);
    Output::context(&inp.event, &text)
}

/// Срок `POST /sessions {id, holdout: true}` внутри бюджета `session-start`, мс.
pub const HOLDOUT_MS: u64 = 100;

/// Соль хэша контрольной группы A/B (смена — новое разбиение сессий).
const HOLDOUT_SALT: &[u8] = b"zam-holdout-v1\0";

/// Сессия `id` — в контрольной группе A/B при доле `p` (M6 §12): первые 8 байт
/// blake3(соль ‖ id) как доля от 2^64 меньше `p`. Детерминировано: повторный старт той же сессии
/// (compact, resume) даёт то же решение; `p = 0` — никогда.
pub fn holdout_pick(id: &str, p: f64) -> bool {
    if p <= 0.0 {
        return false;
    }
    let mut h = blake3::Hasher::new();
    h.update(HOLDOUT_SALT);
    h.update(id.as_bytes());
    let b = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&b.as_bytes()[..8]);
    (u64::from_le_bytes(first) as f64 / 18_446_744_073_709_551_616.0) < p
}

/// `POST /sessions {id, holdout: true}` (M6 §12) — отдельный запрос: старый сервер
/// (`deny_unknown_fields`) отвечает 400 про неизвестное поле `holdout` — молча; прочее — в
/// `hooks.log` (`sessions(holdout): …`). `true` — метка поставлена.
fn set_holdout(env: &Env, client: &crate::client::Client, id: &str) -> bool {
    let body = json!({"id": id, "holdout": true});
    match client.post_json("/sessions", &body) {
        Ok(_) => true,
        Err(e)
            if e.http_status() == Some(400)
                && e.problem
                    .as_ref()
                    .and_then(|p| p["detail"].as_str())
                    .is_some_and(|d| d.contains("holdout")) =>
        {
            false
        }
        Err(e) => {
            env.log(&format!("sessions(holdout): {e}"));
            false
        }
    }
}

/// Версия блока инструкции (R2): маркер `<!-- zeroaimemory:begin vN -->` ближайшего
/// CLAUDE.md/AGENTS.md вверх от `cwd` до корня проекта включительно (Codex — сначала
/// AGENTS.md). Нет маркера или версии — `None`.
pub fn block_version(cwd: &Path, root: &Path, agent: super::Agent) -> Option<u32> {
    let names: [&str; 2] = match agent {
        super::Agent::Claude => ["CLAUDE.md", "AGENTS.md"],
        super::Agent::Codex => ["AGENTS.md", "CLAUDE.md"],
    };
    let stop = if cwd.starts_with(root) {
        Some(root)
    } else {
        None
    };
    for dir in cwd.ancestors() {
        for n in names {
            if let Some(v) = marker_version(&dir.join(n)) {
                return Some(v);
            }
        }
        if stop == Some(dir) {
            break;
        }
    }
    None
}

/// Версия из маркера файла: `<!-- zeroaimemory:begin v2 -->` → 2.
fn marker_version(path: &Path) -> Option<u32> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > MD_MAX {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    // BOM в начале файла (fix round 1, I-1)
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(BEGIN)?;
        let rest = rest.trim_start().strip_prefix('v')?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli-hook")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Доля контрольной группы по хэшу близка к p, решение детерминировано, `p = 0` — никого.
    #[test]
    fn holdout_pick_share_and_determinism() {
        let ids: Vec<String> = (0..4000).map(|i| format!("s-{i}")).collect();
        for p in [0.1, 0.25, 0.5] {
            let n = ids.iter().filter(|id| holdout_pick(id, p)).count() as f64;
            let share = n / ids.len() as f64;
            assert!((share - p).abs() < 0.03, "p={p}: {share}");
        }
        assert!(ids.iter().all(|id| !holdout_pick(id, 0.0)));
        // меньшая доля — подмножество большей (одна и та же сессия не «перескакивает»)
        assert!(
            ids.iter()
                .filter(|id| holdout_pick(id, 0.1))
                .all(|id| holdout_pick(id, 0.5))
        );
        assert_eq!(holdout_pick("s-7", 0.5), holdout_pick("s-7", 0.5));
        // закреплённые значения: та же функция повторена в тестах `zam-cli`
        // (`tests/common::holdout_pick`) — смена соли или формулы должна сломать оба
        let pinned: Vec<bool> = (0..8)
            .map(|i| holdout_pick(&format!("pin-{i}"), 0.5))
            .collect();
        assert_eq!(pinned, PINNED_HOLDOUT, "{pinned:?}");
    }

    /// `holdout_pick("pin-0".."pin-7", 0.5)`.
    const PINNED_HOLDOUT: [bool; 8] = [false, true, true, true, false, true, true, false];

    #[test]
    fn block_version_nearest_file() {
        let root = tmp("block-version");
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            root.join("CLAUDE.md"),
            "# x\n<!-- zeroaimemory:begin v2 -->\nZeroAIMemoryUrl=http://h\n<!-- zeroaimemory:end -->\n",
        )
        .unwrap();
        assert_eq!(
            block_version(&sub, &root, super::super::Agent::Claude),
            Some(2)
        );
        std::fs::write(root.join("a/AGENTS.md"), "<!-- zeroaimemory:begin v1 -->\n").unwrap();
        assert_eq!(
            block_version(&sub, &root, super::super::Agent::Codex),
            Some(1)
        );
        assert_eq!(
            block_version(&sub, &root, super::super::Agent::Claude),
            Some(1)
        );
        // выше корня проекта не ищем
        assert_eq!(
            block_version(&sub, &root.join("a/b"), super::super::Agent::Claude),
            None
        );
        std::fs::write(root.join("a/b/CLAUDE.md"), "<!-- zeroaimemory:begin -->\n").unwrap();
        assert_eq!(
            block_version(&sub, &root.join("a/b"), super::super::Agent::Claude),
            None
        );
    }
}
