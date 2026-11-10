//! Снимок сессии на сервере (спецификация M6 §8а, задача 17c): запись, чтение и показ
//! «последнего известного состояния» в дайджесте и пакете восстановления.
//!
//! ```text
//! PUT /sessions/{id}/snapshot {taken_at, trigger, agent, status, skipped, data}
//!   → 204 | 200 {warnings: ["secrets_masked"]}
//! GET /sessions/{id}/snapshot → 200 {session_id, taken_at, trigger, agent, status, skipped,
//!                                     bytes, data} | 404
//! ```
//!
//! - **Запись** — только при явной сессии: `X-Zam-Session` совпадает с `{id}` пути, иначе 400
//!   `/problems/session-required` (привязки MCP §7.1 у маршрута нет: снимок шлёт клиент `zam`,
//!   не инструмент MCP). Токен `ro` — 403. Тело — JSON клиента (`PutBody` задачи 17b:
//!   `taken_at` и поля нейтрального снимка `zam_core::snapshot`); неизвестные поля
//!   игнорируются (клиент бывает новее сервера) **[решение исполнителя]**; не JSON — 400;
//!   неизвестный `trigger`/`status`, `taken_at` не RFC 3339, `agent` длиннее
//!   [`AGENT_MAX_CHARS`] — 422. Всё тело больше `snapshot.max_bytes` — 413 (и до разбора, и
//!   после повторной маскировки). Клиент на 400/413/422 снимок отбрасывает.
//! - **Сессии нет** в базе пользователя запроса — 404 `/problems/session-not-found`
//!   **[решение исполнителя]**: снимок не регистрирует сессию — регистрация освежила бы
//!   `last_seen_at`, и оборванная сессия выглядела бы живой (§8а). Клиент 17b считает 404 «старым
//!   сервером» (до 3 попыток за сессию) — для снимка удалённой сессии это и нужно: он больше не
//!   отправляется.
//! - `taken_at` из будущего (часы клиента впереди) сводится к «сейчас» сервера: иначе снимок
//!   казался бы новее работы, сделанной после него.
//! - **Маскировка**: сервер повторяет `scan_secrets` для каждого текстового поля `data`; при
//!   находке маскирует сам, отвечает `warnings: ["secrets_masked"]` и пишет WARN без текста и без
//!   id сессии. Правил флагов команд клиента (`mask_flags`) у сервера нет — их находки уже
//!   замаскированы клиентом.
//! - **Не обращение сессии**: маршрут — вне `memory_routes` и без `touch_from_request`, поэтому
//!   `PUT` не меняет ни `last_seen_at`, ни `memory_writes` (снимок `SessionEnd` и догоняющий
//!   снимок не делают сессию живой). Работа до ответа — разбор, скан и одна запись SQLite.
//! - **Чтение** — только своя сессия (база пользователя запроса, у администратора —
//!   `X-Zam-User`); чужая, в том числе для участника общего решения, — 404, как несуществующая.
//!
//! **Показ** ([`last_state`]) — раздел
//! `### Последнее известное состояние (снимок <время>, извлечено автоматически)`:
//! - **дайджест** (`session_start` без восстановления) — последний снимок **других** сессий
//!   этого пользователя в этом решении за `recall.snapshot_digest_days` суток UTC (от начала
//!   текущих); не показывается, если его сессия жива (нет `ended_at`, `last_seen_at` моложе
//!   [`LIVE_MIN`] минут) или работа после снимка продолжилась: другая сессия решения (не
//!   субагент сессии снимка) писала в память позже снимка, ревизия задачи снимка позже снимка
//!   (любая — задача тогда не «давно не обновлялась»), действующий чужой захват задачи;
//! - **пакет восстановления** — снимок своей сессии, без конспекта (он уже в контексте агента)
//!   и без проверок живости;
//! - в обоих — только если «задача давно не обновлялась»: у сессии снимка есть задача
//!   `in_progress`/`blocked` (захваченная ею или с её ревизией), последняя ревизия которой старше
//!   снимка на `recall.snapshot_stale_min` минут, — тогда строка `задача k:<id> не обновлялась с
//!   <время>`; или задачи нет, а в снимке есть изменённые файлы;
//! - бюджет — `recall.snapshot_tokens` (0 — раздела нет) внутри бюджета дайджеста; квоты частей
//!   — файлы [`FILES_TOKENS`], команды [`COMMANDS_TOKENS`], запросы [`PROMPTS_TOKENS`], конспект
//!   — остаток (хвост: главное в конце). Из каждого списка остаются последние элементы.
//!   Выбор зависит только от базы и момента запроса — дайджест детерминирован (§8).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use serde_json::json;
use zam_core::Id;
use zam_core::snapshot::{SnapCommand, SnapStatus, SnapTrigger, SnapshotData};
use zam_search::recall::one_line;
use zam_search::truncate_marked;
use zam_store::{
    Db, SnapshotRow, StoreError, budget_cap, count_tokens, digest_snapshot, get_session,
    get_snapshot, put_snapshot, snapshot_tasks, writes_after,
};

use super::parse_body;
use super::scope::require_rw;
use crate::app::AppState;
use crate::attrib::{SESSION_HEADER, header_value};
use crate::auth::Principal;
use crate::config::RecallSection;
use crate::error::ApiError;

/// Предупреждение ответа `PUT`: сервер нашёл и замаскировал секреты.
pub const WARN_SECRETS_MASKED: &str = "secrets_masked";
/// Предел `agent` (формат журнала клиента: `claude`, `codex`), символов.
pub const AGENT_MAX_CHARS: usize = 32;
/// Сессия без `ended_at` живая, если обращалась не раньше стольких минут назад (§8а).
pub const LIVE_MIN: i64 = 10;
/// Квота строки файлов, токенов.
pub const FILES_TOKENS: usize = 80;
/// Квота строки команд, токенов.
pub const COMMANDS_TOKENS: usize = 80;
/// Квота строки запросов человека, токенов.
pub const PROMPTS_TOKENS: usize = 120;
/// Задач сессии снимка с каждой базы.
const TASKS_FETCH: usize = 5;
/// Запрос в строке — не длиннее, символов (дальше квота всё равно не пустит).
const PROMPT_CHARS: usize = 300;

/// Тело `PUT` (тело клиента `PutBody`, задача 17b).
#[derive(Deserialize)]
struct PutReq {
    taken_at: String,
    trigger: String,
    #[serde(default)]
    agent: Option<String>,
    status: String,
    #[serde(default)]
    skipped: u32,
    #[serde(default)]
    data: SnapshotData,
}

/// База пользователя запроса (у администратора — `X-Zam-User`).
async fn own_db(st: &AppState, p: &Principal, headers: &HeaderMap) -> Result<Db, ApiError> {
    let user = crate::auth::target_user(st, p, headers).await?;
    Ok(st.users.get(user.id).await?)
}

/// Маскирует секреты текста; `true` — нашлись.
fn remask(t: &mut String) -> bool {
    let scan = zam_markdown::scan_secrets_ext(t);
    if scan.hits.is_empty() {
        return false;
    }
    *t = zam_markdown::mask_secrets(t, &scan.hits);
    true
}

/// Повторная маскировка всех текстовых полей снимка; число полей с находками.
fn remask_data(d: &mut SnapshotData) -> usize {
    let mut n = 0;
    if let Some(s) = d.compact_summary.as_mut() {
        n += usize::from(remask(s));
    }
    for t in d.prompts.iter_mut().chain(d.files.iter_mut()) {
        n += usize::from(remask(t));
    }
    for c in &mut d.commands {
        n += usize::from(remask(&mut c.cmd));
    }
    n
}

/// `PUT /sessions/{id}/snapshot` (см. модуль).
pub async fn put(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    require_rw(&p)?;
    if header_value(&headers, SESSION_HEADER)?.as_deref() != Some(id.as_str()) {
        return Err(ApiError::SessionRequired(
            "снимок пишется только при явной сессии: X-Zam-Session = id сессии пути".into(),
        ));
    }
    zam_store::check_session_id(&id)?;
    let max = st.cfg.snapshot.max_bytes;
    let too_large = || ApiError::PayloadTooLarge(format!("снимок больше {max} байт"));
    let body = match body {
        Ok(b) => b,
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => return Err(too_large()),
        Err(e) => return Err(ApiError::BadRequest(format!("некорректное тело: {e}"))),
    };
    if body.len() > max {
        return Err(too_large());
    }
    let req: PutReq = parse_body(&body)?;
    let trigger = SnapTrigger::parse(&req.trigger).ok_or_else(|| {
        ApiError::Unprocessable(format!("trigger: неизвестный «{}»", req.trigger))
    })?;
    let status = SnapStatus::parse(&req.status)
        .ok_or_else(|| ApiError::Unprocessable("status: complete или partial".into()))?;
    let taken_at: Timestamp = req
        .taken_at
        .trim()
        .parse()
        .map_err(|_| ApiError::Unprocessable("taken_at: момент RFC 3339".into()))?;
    let agent = req
        .agent
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    if agent
        .as_deref()
        .is_some_and(|a| a.chars().count() > AGENT_MAX_CHARS || a.chars().any(char::is_control))
    {
        return Err(ApiError::Unprocessable(format!(
            "agent: до {AGENT_MAX_CHARS} печатных символов"
        )));
    }
    let mut data = req.data;
    let masked = remask_data(&mut data);
    let mut row = SnapshotRow {
        session_id: id,
        taken_at: taken_at.min(zam_store::time::now()),
        trigger,
        agent,
        status,
        skipped: req.skipped,
        bytes: 0,
        data,
    };
    // размер записи — всё тело в том виде, как его хранит сервер (после маскировки)
    let bytes = serde_json::to_vec(&json!({
        "taken_at": zam_store::time::fmt_ts(row.taken_at),
        "trigger": row.trigger,
        "agent": row.agent,
        "status": row.status,
        "skipped": row.skipped,
        "data": row.data,
    }))
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .len();
    if bytes > max {
        return Err(too_large());
    }
    row.bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
    let db = own_db(&st, &p, &headers).await?;
    match put_snapshot(&db, &row).await {
        Ok(()) => {}
        Err(StoreError::NotFound) => return Err(ApiError::SessionNotFound),
        Err(e) => return Err(e.into()),
    }
    if masked == 0 {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    // без текста находок и без id сессии
    tracing::warn!(
        fields = masked,
        "снимок сессии: сервер замаскировал секреты (secrets_masked)"
    );
    Ok((
        StatusCode::OK,
        Json(json!({ "warnings": [WARN_SECRETS_MASKED] })),
    )
        .into_response())
}

/// `GET /sessions/{id}/snapshot` (см. модуль).
pub async fn get(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SnapshotRow>, ApiError> {
    let db = own_db(&st, &p, &headers).await?;
    get_snapshot(&db, &id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

// ---- показ «последнего известного состояния» (§8а) ----

/// Что нужно показу.
pub(crate) struct ShowReq<'a> {
    /// База пользователя запроса: сессии и снимки.
    pub own: &'a Db,
    /// GUID решения запроса (в области поиска).
    pub solution: &'a str,
    /// Сессия запроса.
    pub session: Option<&'a str>,
    /// Базы и пространства задач (решение и Knowledge).
    pub task_dbs: Vec<(Db, Vec<Id>)>,
    /// Пакет восстановления: свой снимок, без конспекта и проверок живости.
    pub recovery: bool,
    pub now: Timestamp,
    pub cfg: &'a RecallSection,
}

/// Раздел снимка: заголовок и блоки (строка задачи, конспект, запросы, файлы, команды).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    pub title: String,
    pub blocks: Vec<String>,
}

/// Раздел «Последнее известное состояние» или `None` (см. модуль).
pub(crate) async fn last_state(q: ShowReq<'_>) -> Result<Option<Shown>, StoreError> {
    if q.cfg.snapshot_tokens == 0 {
        return Ok(None);
    }
    let snap = if q.recovery {
        match q.session {
            Some(s) => get_snapshot(q.own, s).await?,
            None => None,
        }
    } else {
        let days = i64::from(q.cfg.snapshot_digest_days.max(1)) - 1;
        let since = zam_store::time::day_start_utc(q.now) - SignedDuration::from_hours(24 * days);
        digest_snapshot(q.own, q.solution, q.session, since).await?
    };
    let Some(snap) = snap else {
        return Ok(None);
    };
    if !q.recovery {
        let Some(s) = get_session(q.own, &snap.session_id).await? else {
            return Ok(None);
        };
        let live_since = q.now - SignedDuration::from_mins(LIVE_MIN);
        if s.ended_at.is_none() && s.last_seen_at > live_since {
            return Ok(None);
        }
        if writes_after(q.own, q.solution, &snap.session_id, snap.taken_at).await? {
            return Ok(None);
        }
    }
    let mut tasks = Vec::new();
    for (db, spaces) in &q.task_dbs {
        tasks.extend(snapshot_tasks(db, spaces, &snap.session_id, q.now, TASKS_FETCH).await?);
    }
    // (в) действующий чужой захват; (б) ревизия задачи другой сессией после снимка отдельной
    // проверки не требует: такая задача «обновлялась» — условие давности ниже раздел скрывает
    if !q.recovery && tasks.iter().any(|t| t.claimed_by_other) {
        return Ok(None);
    }
    // задача — недавно менявшаяся; порядок однозначен (момент ревизии, затем id)
    tasks.sort_by_key(|t| std::cmp::Reverse((t.last_rev_at, t.id)));
    let stale = SignedDuration::from_mins(i64::from(q.cfg.snapshot_stale_min));
    let task_line = match tasks.first() {
        Some(t) => {
            let at = t.last_rev_at.unwrap_or(snap.taken_at);
            if at + stale > snap.taken_at {
                return Ok(None);
            }
            Some(format!(
                "задача k:{} не обновлялась с {}",
                t.id,
                agent_time(at)
            ))
        }
        None if snap.data.files.is_empty() => return Ok(None),
        None => None,
    };
    Ok(Some(render(
        &snap,
        task_line,
        !q.recovery,
        q.cfg.snapshot_tokens,
    )))
}

/// Время в тексте агенту (M6 §4.4): `yyyy-MM-dd HH:mm:ss` UTC.
fn agent_time(t: Timestamp) -> String {
    zam_core::time::agent_ts(&zam_store::time::fmt_ts(t)).unwrap_or_default()
}

/// Стоимость блока в упаковке разделов (`zam_search::recall::pack_sections`).
fn block_cost(b: &str) -> usize {
    count_tokens(&format!("{b}\n"))
}

/// Раздел снимка в пределе `tokens`: заголовок и строка задачи — первыми, затем квоты файлов,
/// команд и запросов, конспект (`summary`) — в остатке; вывод — задача, конспект, запросы,
/// файлы, команды.
pub(crate) fn render(
    snap: &SnapshotRow,
    task_line: Option<String>,
    summary: bool,
    tokens: usize,
) -> Shown {
    let title = format!(
        "### Последнее известное состояние (снимок {}, извлечено автоматически)",
        agent_time(snap.taken_at)
    );
    let cap = budget_cap(tokens);
    let mut used = count_tokens(&format!("\n{title}\n"));
    let task = take(&mut used, cap, task_line);
    let left = |used: usize| cap.saturating_sub(used);
    let d = &snap.data;
    let files: Vec<String> = d.files.iter().map(|f| one_line(f)).collect();
    let files = list_line(
        "Файлы",
        &files,
        ", ",
        budget_cap(FILES_TOKENS).min(left(used)),
    );
    let files = take(&mut used, cap, files);
    let commands: Vec<String> = d.commands.iter().map(command_text).collect();
    let commands = list_line(
        "Команды",
        &commands,
        " · ",
        budget_cap(COMMANDS_TOKENS).min(left(used)),
    );
    let commands = take(&mut used, cap, commands);
    let prompts: Vec<String> = d
        .prompts
        .iter()
        .map(|p| format!("«{}»", clip(&one_line(p), PROMPT_CHARS)))
        .collect();
    let prompts = list_line(
        "Запросы",
        &prompts,
        " · ",
        budget_cap(PROMPTS_TOKENS).min(left(used)),
    );
    let prompts = take(&mut used, cap, prompts);
    let summary = d
        .compact_summary
        .as_deref()
        .filter(|s| summary && !s.trim().is_empty())
        .and_then(|s| summary_block(s, left(used)));
    let blocks = [task, summary, prompts, files, commands]
        .into_iter()
        .flatten()
        .collect();
    Shown { title, blocks }
}

/// Блок `line`, если он влезает в остаток `cap − used` (его стоимость прибавляется к `used`).
fn take(used: &mut usize, cap: usize, line: Option<String>) -> Option<String> {
    let line = line?;
    let c = block_cost(&line);
    (*used + c <= cap).then(|| {
        *used += c;
        line
    })
}

/// Команда строки: `` `cmd` ``, с ошибкой — ещё ` (ошибка)`.
fn command_text(c: &SnapCommand) -> String {
    let cmd = clip(&one_line(&c.cmd).replace('`', "'"), PROMPT_CHARS);
    match c.failed {
        Some(true) => format!("`{cmd}` (ошибка)"),
        _ => format!("`{cmd}`"),
    }
}

/// Строка до `max` символов (длиннее — с «…»).
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Строка списка `<имя>: a<sep>b (+N)` из последних элементов `items` (старые — первыми), чья
/// стоимость блока не больше `cap`; не влез ни один целиком — последний, обрезанный с «…».
fn list_line(name: &str, items: &[String], sep: &str, cap: usize) -> Option<String> {
    if items.is_empty() || cap == 0 {
        return None;
    }
    let line = |from: usize| {
        let mut s = format!("{name}: {}", items[from..].join(sep));
        if from > 0 {
            s.push_str(&format!(" (+{from})"));
        }
        s
    };
    // больше элементов — дороже: первая стартовая позиция, при которой строка влезает
    let fit = (0..items.len()).find(|&from| block_cost(&line(from)) <= cap);
    if let Some(from) = fit {
        return Some(line(from));
    }
    let last = items.len() - 1;
    let head = format!("{name}: ");
    let tail = if last > 0 {
        format!(" (+{last})")
    } else {
        String::new()
    };
    let room = cap.saturating_sub(block_cost(&format!("{head}{tail}")));
    let (cut, _) = truncate_marked(&items[last], room, &count_tokens);
    let s = format!("{head}{cut}{tail}");
    (!cut.is_empty() && block_cost(&s) <= cap).then_some(s)
}

/// Блок конспекта: `Конспект (хвост):` и строки текста с отступом (строка не начинается с `#`
/// и не выглядит строкой подсказки); не влезает — хвост с «…» в начале.
fn summary_block(text: &str, cap: usize) -> Option<String> {
    let show = |t: &str| -> String {
        let mut s = String::from("Конспект (хвост):");
        for l in t
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
        {
            s.push_str("\n  ");
            s.push_str(l);
        }
        s
    };
    let text = text.trim();
    let whole = show(text);
    if block_cost(&whole) <= cap {
        return Some(whole);
    }
    // наименьшее начало хвоста, при котором блок влезает (длиннее хвост — дороже)
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let fits = |k: usize| block_cost(&show(&format!("…{}", &text[starts[k]..]))) <= cap;
    let (mut lo, mut hi) = (1usize, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(mid) { hi = mid } else { lo = mid + 1 }
    }
    (lo < starts.len() && fits(lo)).then(|| show(&format!("…{}", &text[starts[lo]..])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(data: SnapshotData) -> SnapshotRow {
        SnapshotRow {
            session_id: "s".into(),
            taken_at: "2026-11-10T08:00:00Z".parse().unwrap(),
            trigger: SnapTrigger::Stop,
            agent: Some("claude".into()),
            status: SnapStatus::Complete,
            skipped: 0,
            bytes: 0,
            data,
        }
    }

    /// Списки — последние элементы с числом пропущенных; конспект — хвост.
    #[test]
    fn lists_keep_newest_and_summary_keeps_tail() {
        let files: Vec<String> = (0..40)
            .map(|i| format!("src/module_{i}/file_{i}.rs"))
            .collect();
        let s = snap(SnapshotData {
            compact_summary: Some(format!("{}\nNext Step: финал", "начало ".repeat(800))),
            prompts: vec!["первый".into(), "последний".into()],
            files,
            commands: vec![SnapCommand {
                cmd: "cargo test".into(),
                failed: Some(true),
            }],
        });
        let out = render(&s, None, true, 400);
        let all = out.blocks.join("\n");
        assert!(
            all.contains("file_39.rs") && !all.contains("file_0.rs"),
            "{all}"
        );
        assert!(all.contains("(+"), "{all}");
        assert!(all.contains("Next Step: финал"), "{all}");
        assert!(all.contains("`cargo test` (ошибка)"), "{all}");
        assert!(
            out.title.contains("снимок 2026-11-10 08:00:00"),
            "{}",
            out.title
        );
        // без конспекта (восстановление)
        let out = render(&s, None, false, 400);
        assert!(!out.blocks.join("\n").contains("Конспект"));
    }
}
