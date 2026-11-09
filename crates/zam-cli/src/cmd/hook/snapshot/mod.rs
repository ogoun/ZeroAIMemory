//! Снимок сессии из журнала агента (спецификация M6 §8а, решение владельца baton-1 = А).
//!
//! Журнал агента (`transcript_path` во входе хука) читается без LLM: с конца, блоками по
//! [`BLOCK`] байт, не больше `tail_kb` КиБ; первая, обрезанная строка отбрасывается. Строки
//! проходят предфильтр по подстроке ([`AgentKind::prefilter`]) и только потом разбираются
//! адаптером формата ([`claude`], [`codex`]). Адаптер берёт **белый список**: конспект сжатия,
//! запросы человека, изменённые файлы (относительно корня решения), команды (первая строка, исход).
//!
//! Всё — в отдельном потоке с жёстким сроком ([`take`]): открытие, чтение, разбор, маскировка.
//! Вызывающий ждёт не дольше срока; опоздавший поток брошен, результат — `None`. Поток сам
//! останавливается на 4/5 срока и отдаёт собранное как `partial`.
//!
//! Маскировка ([`mask_and_limit`]) — `zam_secrets` по каждому текстовому полю (то же, что
//! `mask_text`, плюс признак упора `depth_capped`) и правила флагов командной строки
//! ([`mask_flags`]: `-pXXX` у mysql, `--password X`, `curl -u u:p`, `Bearer X`). Затем пределы
//! §8а: поля по отдельности, потом общий размер JSON — однозначный порядок сокращения.
//!
//! Форматы агентов знают только адаптеры; ядро (`zam_core::snapshot`) — нейтральный снимок.
//! Модуль не зависит от остального клиента (`crate::…`): его исходник подключают и тесты
//! (`tests/snapshot.rs`).

mod claude;
mod codex;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Serialize;
use zam_core::snapshot::{SnapCommand, SnapStatus, SnapTrigger, SnapshotData};

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;

/// Блок чтения с конца файла.
pub const BLOCK: usize = 64 * 1024;
/// Строка длиннее — не разбирается и считается в `skipped`.
pub const LINE_MAX: usize = 1024 * 1024;
/// Запросов человека в снимке, символов в запросе.
pub const PROMPTS_MAX: usize = 5;
pub const PROMPT_CHARS: usize = 500;
/// Изменённых файлов в снимке.
pub const FILES_MAX: usize = 20;
/// Команд в снимке, символов в команде (первая строка).
pub const COMMANDS_MAX: usize = 10;
pub const COMMAND_CHARS: usize = 200;
/// Предел конспекта, байт UTF-8.
pub const SUMMARY_BYTES: usize = 8 * 1024;
/// Запас окна маскировки сверх сохраняемой части поля, символов: секрет, начатый в сохраняемой
/// части, сканер видит целым (закрытый ключ PEM — до ~3,3 тыс. символов). Скан ограничен окном,
/// а не всем полем (запрос в мегабайт на слабом стенде не съедает срок).
pub const MASK_MARGIN: usize = 4096;
/// Окно конспекта до маскировки, байт: хвост — ключевые разделы Claude в конце.
pub const SUMMARY_WINDOW: usize = 4 * SUMMARY_BYTES;
/// Первая строка журнала (Codex `session_meta`), если она вне окна хвоста: одно чтение с нуля.
pub const HEAD_LINE_MAX: usize = 64 * 1024;
/// Начало и конец длинной строки для предфильтра, байт: в пропуски идут только строки
/// своего формата (огромный вывод инструмента — не повод для `partial`).
pub const PREFIX_PROBE: usize = 4096;
/// Запас под поля тела `PUT` сверх сериализованного [`Snapshot`] (время, id сессии): тело
/// запроса не больше `max_bytes`, если в нём не больше этого запаса добавочных байт.
pub const ENVELOPE_RESERVE: usize = 256;

/// Формат журнала агента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [AgentKind::Claude, AgentKind::Codex]
            .into_iter()
            .find(|a| a.as_str() == s)
    }

    /// Предфильтр строк по подстроке: строка без единого признака не разбирается. Свой у
    /// каждого формата (§8а перечисляет общий список; у Codex вызовы инструментов —
    /// `function_call`/`custom_tool_call`/`local_shell_call`, а не `tool_use`).
    pub fn prefilter(self) -> &'static [&'static str] {
        match self {
            AgentKind::Claude => &claude::PREFILTER,
            AgentKind::Codex => &codex::PREFILTER,
        }
    }

    /// Нужна ли первая строка файла вне окна: у Codex там `session_meta` (id и `cwd` сессии);
    /// у Claude первая строка — обычная старая запись, вне окна она не берётся.
    fn reads_head(self) -> bool {
        matches!(self, AgentKind::Codex)
    }

    /// Строка для адаптера из первой строки файла (`cut` — прочитано только начало): у Codex —
    /// только `session_meta` (иначе вне окна оказалась бы старая запись хода).
    fn head_line(self, head: &str, cut: bool) -> Option<String> {
        match self {
            AgentKind::Claude => None,
            AgentKind::Codex => codex::head_line(head, cut),
        }
    }

    fn adapter(self, root: &Path, session: &str) -> Box<dyn TranscriptAdapter> {
        match self {
            AgentKind::Claude => Box::new(ClaudeAdapter::new(root, session)),
            AgentKind::Codex => Box::new(CodexAdapter::new(root)),
        }
    }
}

/// Снимок: нейтральное содержимое и сведения о сборке.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub trigger: SnapTrigger,
    pub agent: AgentKind,
    pub status: SnapStatus,
    /// Строк пропущено: битые (прошли предфильтр, не разобрались), длиннее [`LINE_MAX`], не UTF-8.
    pub skipped: u32,
    /// Id сессии из журнала (Claude — `sessionId` записей, Codex — `session_meta.payload.id`).
    #[serde(skip)]
    pub session_hint: Option<String>,
    /// Скан секретов упёрся в `FIXPOINT_MAX` хотя бы в одном поле (`secrets_scan_depth`, без текста).
    #[serde(skip)]
    pub scan_depth_capped: bool,
    pub data: SnapshotData,
}

/// Адаптер формата журнала: строки — по порядку файла, от старых к новым.
pub trait TranscriptAdapter: Send {
    fn accept_line(&mut self, line: &str);
    /// Строк, прошедших предфильтр, но не разобранных (битый JSON, не тот тип поля).
    fn skipped(&self) -> u32;
    fn session_hint(&self) -> Option<String>;
    fn finish(self: Box<Self>) -> SnapshotData;
}

/// Параметры [`take`].
#[derive(Debug, Clone)]
pub struct TakeOpts {
    /// Корень решения: пути файлов — относительно него, вне корня — пропуск.
    pub root: PathBuf,
    pub agent: AgentKind,
    /// Id сессии агента (`session_id` входа хука); пусто — последняя сессия файла.
    pub session: String,
    pub trigger: SnapTrigger,
    /// Хвост файла, КиБ (`hooks.snapshot_tail_kb`).
    pub tail_kb: u32,
    /// Жёсткий срок всего снимка (`hooks.snapshot_ms`).
    pub deadline: Duration,
    /// Предел записи, байт (`snapshot.max_bytes`).
    pub max_bytes: usize,
    /// Конспект из входа хука (`PostCompact.compact_summary` Claude, §8а — основной источник):
    /// заменяет конспект журнала; журнала нет или он не читается — снимок из одного конспекта.
    pub summary: Option<String>,
    /// Журнал короче — снимок не нужен ([`Taken::Skipped`]): `Stop` без роста, догоняющий снимок
    /// без новых записей. Проверка — в потоке, в сроке (§8а: весь ввод-вывод в срок).
    pub min_len: Option<u64>,
}

/// Итог [`take_ext`].
#[derive(Debug)]
pub enum Taken {
    /// Снимок и размер журнала при чтении, байт.
    Snap(Snapshot, u64),
    /// Журнала нет, это не файл или ошибка чтения (и конспекта входа нет).
    Missing,
    /// Журнал короче `min_len`.
    Skipped,
    /// Поток не успел в срок (брошен и ничего не пишет).
    Late,
}

/// Снимок журнала `path` в отдельном потоке со сроком `opts.deadline`: чтение хвоста,
/// предфильтр, разбор, маскировка и пределы. `None` — файла нет, это не файл, ошибка чтения
/// или поток не успел (он брошен и ничего не пишет).
///
/// **Только для короткоживущих процессов** (хук `zam hook …`, `zam session snapshot`): брошенный
/// по сроку поток доживает до конца своей работы и всё это время держит файл журнала открытым.
/// Из долгоживущих процессов (`zam mcp`) не вызывать.
pub fn take(path: &Path, opts: &TakeOpts) -> Option<Snapshot> {
    match take_ext(path, opts) {
        Taken::Snap(s, _) => Some(s),
        _ => None,
    }
}

/// [`take`] с причиной, если снимка нет, и размером журнала (для `snapst-*`): весь ввод-вывод
/// журнала (`metadata`, проверка `min_len`, чтение) — в потоке со сроком.
pub fn take_ext(path: &Path, opts: &TakeOpts) -> Taken {
    let start = Instant::now();
    let (tx, rx) = mpsc::sync_channel(1);
    let path = path.to_path_buf();
    let o = opts.clone();
    let spawned = std::thread::Builder::new()
        .name("zam-snapshot".into())
        .spawn(move || {
            test_stall(&path);
            let _ = tx.send(build(&path, &o, start));
        });
    if spawned.is_err() {
        return Taken::Late;
    }
    rx.recv_timeout(opts.deadline).unwrap_or(Taken::Late)
}

/// Точка подмены для тестов срока: файл `<журнал>.stall-ms` с числом миллисекунд — поток
/// засыпает до сборки. Только отладочная сборка и фича `test-hooks` (как
/// `ZAM_TEST_BINDING_STALL_MS`); у каждого теста свой журнал — соседние тесты не задеты.
fn test_stall(path: &Path) {
    #[cfg(any(feature = "test-hooks", debug_assertions))]
    if let Some(ms) = std::fs::read_to_string(format!("{}.stall-ms", path.display()))
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(ms));
    }
    #[cfg(not(any(feature = "test-hooks", debug_assertions)))]
    let _ = path;
}

/// Тело потока [`take`]: мягкий срок — 4/5 жёсткого, после него собранное отдаётся `partial`.
fn build(path: &Path, o: &TakeOpts, start: Instant) -> Taken {
    let soft = start + o.deadline * 4 / 5;
    let summary = o.summary.as_deref().filter(|t| !t.trim().is_empty());
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file());
    if let Some(m) = &meta
        && o.min_len.is_some_and(|n| m.len() < n)
    {
        return Taken::Skipped;
    }
    let len = meta.as_ref().map_or(0, |m| m.len());
    let tail = meta
        .as_ref()
        .and_then(|_| read_tail(path, o.tail_kb as usize * 1024, soft, o.agent.reads_head()));
    let Some(tail) = tail else {
        // журнала нет — снимок из конспекта входа хука, если он есть
        let Some(summary) = summary else {
            return Taken::Missing;
        };
        let snap = Snapshot {
            trigger: o.trigger,
            agent: o.agent,
            status: SnapStatus::Complete,
            skipped: 0,
            session_hint: None,
            scan_depth_capped: false,
            data: SnapshotData {
                compact_summary: Some(summary.to_string()),
                ..SnapshotData::default()
            },
        };
        return Taken::Snap(mask_and_limit(snap, o.max_bytes), 0);
    };
    let mut late = tail.late;
    let mut adapter = o.agent.adapter(&o.root, &o.session);
    let filters = o.agent.prefilter();
    let passes = |s: &str| filters.iter().any(|f| s.contains(f));
    let mut skipped = 0u32;
    // первая строка файла (`session_meta` Codex) вне окна — до хвоста; адаптер решает, его ли она
    if let Some(line) = tail
        .head
        .as_ref()
        .and_then(|(h, cut)| o.agent.head_line(h, *cut))
    {
        adapter.accept_line(&line);
    }
    let mut lines = tail.buf.split(|&b| b == b'\n').peekable();
    let mut i = 0usize;
    while let Some(raw) = lines.next() {
        i += 1;
        if i.is_multiple_of(256) && Instant::now() >= soft {
            late = true;
            break;
        }
        // последняя строка без `\n` — агент её ещё пишет
        let unterminated = lines.peek().is_none() && !raw.is_empty();
        if raw.len() > LINE_MAX {
            // признак может стоять в начале записи (Claude: `type` до `message`) или в конце
            let head = String::from_utf8_lossy(&raw[..PREFIX_PROBE]);
            let end = String::from_utf8_lossy(&raw[raw.len() - PREFIX_PROBE..]);
            skipped += u32::from(passes(&head) || passes(&end));
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else {
            skipped += u32::from(passes(&String::from_utf8_lossy(raw)));
            continue;
        };
        let line = line.trim_end_matches('\r');
        if passes(line) {
            adapter.accept_line(line);
        } else if unterminated && serde_json::from_str::<serde::de::IgnoredAny>(line).is_err() {
            // недописанная строка могла потерять признак предфильтра — пропуск, снимок неполный
            skipped += 1;
        }
    }
    let skipped = skipped + adapter.skipped();
    let session_hint = adapter.session_hint();
    let mut data = adapter.finish();
    if let Some(t) = summary {
        data.compact_summary = Some(t.to_string());
    }
    let snap = Snapshot {
        trigger: o.trigger,
        agent: o.agent,
        status: if late || skipped > 0 {
            SnapStatus::Partial
        } else {
            SnapStatus::Complete
        },
        skipped,
        session_hint,
        scan_depth_capped: false,
        data,
    };
    Taken::Snap(mask_and_limit(snap, o.max_bytes), len)
}

/// Прочитанное [`read_tail`].
struct Tail {
    /// Хвост с начала целой строки.
    buf: Vec<u8>,
    /// Первая строка файла, если окно её не захватило: целиком (`false`) или только начало в
    /// [`HEAD_LINE_MAX`] байт без перевода строки (`true`, строка длиннее).
    head: Option<(String, bool)>,
    /// Срок вышел во время чтения.
    late: bool,
}

/// Хвост файла не больше `max` байт, начиная с целой строки; чтение с конца блоками по
/// [`BLOCK`]. Срок `soft` вышел — отдаётся уже прочитанное (новейшее) и признак опоздания.
/// Окно не с начала файла — первая строка читается отдельно, одним коротким чтением с нуля.
fn read_tail(path: &Path, max: usize, soft: Instant, want_head: bool) -> Option<Tail> {
    let mut f = File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let len = meta.len();
    let want = (max as u64).min(len);
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut pos = len;
    let mut late = false;
    while pos > len - want {
        if !blocks.is_empty() && Instant::now() >= soft {
            late = true;
            break;
        }
        let n = (BLOCK as u64).min(pos - (len - want));
        pos -= n;
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut b = vec![0u8; n as usize];
        f.read_exact(&mut b).ok()?;
        blocks.push(b);
    }
    let mut buf: Vec<u8> = Vec::with_capacity(blocks.iter().map(Vec::len).sum());
    for b in blocks.iter().rev() {
        buf.extend_from_slice(b);
    }
    // начало окна не на границе строки — первая (обрезанная) строка отбрасывается
    let mut head = None;
    if pos > 0 {
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                buf.drain(..=i);
            }
            None => buf.clear(),
        }
        if want_head && !late {
            head = read_head(&mut f, pos);
        }
    }
    Some(Tail { buf, head, late })
}

/// Первая строка файла из первых `min(HEAD_LINE_MAX, limit)` байт (`limit` — начало окна хвоста):
/// целиком, если перевод строки там есть, иначе начало строки (по границе символа) и `true`.
/// У Codex `session_meta` с `instructions` (AGENTS.md) бывает длиннее 64 КиБ; `id` и `cwd` в ней
/// идут раньше `instructions` — их достаёт [`codex::meta_from_prefix`].
fn read_head(f: &mut File, limit: u64) -> Option<(String, bool)> {
    let n = (HEAD_LINE_MAX as u64).min(limit) as usize;
    f.seek(SeekFrom::Start(0)).ok()?;
    let mut b = vec![0u8; n];
    f.read_exact(&mut b).ok()?;
    let (end, cut) = match b.iter().position(|&c| c == b'\n') {
        Some(e) => (e, false),
        None => (n, true),
    };
    b.truncate(end);
    let s = match String::from_utf8(b) {
        Ok(s) => s,
        // начало длинной строки могло оборвать символ — до последней целой границы
        Err(e) if cut => {
            let ok = e.utf8_error().valid_up_to();
            let mut b = e.into_bytes();
            b.truncate(ok);
            String::from_utf8(b).ok()?
        }
        Err(_) => return None,
    };
    Some((s.trim_end_matches('\r').to_string(), cut))
}

/// Маскировка и пределы §8а. Каждое текстовое поле: `zam_secrets` (как `mask_text`) и
/// [`mask_flags`]. Затем поля ограничиваются сами (запросы 5 × 500 символов, файлы 20, команды
/// 10 × 200 символов первой строки, конспект ≤ 8 КиБ — [`summary_tail`]); если сериализованный
/// снимок с запасом [`ENVELOPE_RESERVE`] всё ещё больше `max_bytes`, сокращаются по шагам:
/// команды (с самых старых), запросы (с самых старых), конспект (с начала), файлы (с самых
/// старых, сверх §8а — иначе предел недостижим). Сокращение второго шага — статус `partial`.
pub fn mask_and_limit(mut s: Snapshot, max_bytes: usize) -> Snapshot {
    let mut capped = s.scan_depth_capped;
    let mut mask = |t: &str| -> String {
        let o = zam_secrets::scan_secrets_ext(t);
        capped |= o.depth_capped;
        if o.hits.is_empty() {
            t.to_string()
        } else {
            zam_secrets::mask_secrets(t, &o.hits)
        }
    };
    let d = &mut s.data;
    keep_last(&mut d.prompts, PROMPTS_MAX);
    d.prompts = d
        .prompts
        .iter()
        .map(|p| {
            trunc_chars(
                &mask(head_chars(p, PROMPT_CHARS + MASK_MARGIN)),
                PROMPT_CHARS,
            )
        })
        .collect();
    let mut files: Vec<String> = Vec::new();
    for f in d.files.drain(..) {
        files.retain(|x| x != &f);
        files.push(f);
    }
    keep_last(&mut files, FILES_MAX);
    d.files = files
        .iter()
        .map(|f| mask(head_chars(f, PROMPT_CHARS + MASK_MARGIN)))
        .collect();
    keep_last(&mut d.commands, COMMANDS_MAX);
    d.commands = d
        .commands
        .iter()
        .map(|c| SnapCommand {
            // флаги командной строки — только у команд: в прозе «basic»/«-u» не пароль
            cmd: trunc_chars(
                &mask_flags(first_line(&mask(head_chars(
                    first_line(&c.cmd),
                    COMMAND_CHARS + MASK_MARGIN,
                )))),
                COMMAND_CHARS,
            ),
            failed: c.failed,
        })
        .collect();
    d.compact_summary = d
        .compact_summary
        .as_deref()
        .map(|t| summary_tail(&mask(tail_bytes(t, SUMMARY_WINDOW)), SUMMARY_BYTES))
        .filter(|t| !t.is_empty());
    s.scan_depth_capped = capped;

    let budget = max_bytes.saturating_sub(ENVELOPE_RESERVE);
    let size = |s: &Snapshot| serde_json::to_vec(s).map_or(usize::MAX, |v| v.len());
    let mut cut = false;
    loop {
        let n = size(&s);
        if n <= budget {
            break;
        }
        let d = &mut s.data;
        if !d.commands.is_empty() {
            d.commands.remove(0);
        } else if !d.prompts.is_empty() {
            d.prompts.remove(0);
        } else if let Some(t) = d.compact_summary.take() {
            // JSON-экранирование не меньше байта на байт текста: срез на избыток — с начала
            let keep = t.len().saturating_sub(n - budget);
            let rest = tail_bytes(&t, keep);
            if !rest.is_empty() {
                d.compact_summary = Some(rest.to_string());
            }
        } else if !d.files.is_empty() {
            d.files.remove(0);
        } else {
            break;
        }
        cut = true;
    }
    if cut {
        s.status = SnapStatus::Partial;
    }
    s
}

fn keep_last<T>(v: &mut Vec<T>, n: usize) {
    if v.len() > n {
        v.drain(..v.len() - n);
    }
}

/// Первая непустая строка (без `\r` и краевых пробелов).
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Первые не больше `n` символов (срез по границе символа, без копии).
fn head_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// Не больше `n` символов: длиннее — `n - 1` символ и `…`. Срез не режет метку секрета.
fn trunc_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut cut = s
        .char_indices()
        .nth(n.saturating_sub(1))
        .map_or(s.len(), |(i, _)| i);
    if let Some((i, _)) = s
        .match_indices(zam_secrets::SECRET_MARK)
        .find(|(i, m)| *i < cut && cut < i + m.len())
    {
        cut = i;
    }
    format!("{}…", &s[..cut])
}

/// Последние не больше `n` байт по границе символа; если в срезе есть перевод строки в первых
/// 256 байтах — с начала следующей строки.
fn tail_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = s.len() - n;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    // срез внутри метки `‹secret›` — начало после метки (как `trunc_chars`, но с хвоста)
    if let Some((j, m)) = s
        .match_indices(zam_secrets::SECRET_MARK)
        .find(|(j, m)| *j < i && i < j + m.len())
    {
        i = j + m.len();
    }
    let rest = &s[i..];
    match rest.find('\n') {
        Some(j) if j < 256 => &rest[j + 1..],
        _ => rest,
    }
}

/// Ключевые разделы конспекта — в конце конспекта сжатия Claude (живой журнал 2.1.314:
/// «7. Pending Tasks:», «8. Current Work:», «9. Optional Next Step:»; варианты с `**…**` и
/// русские «Ожидающие задачи», «Текущая работа», «Следующий шаг»). Порядок — приоритет при
/// нехватке места.
const KEY_SECTIONS: [&[&str]; 3] = [
    &["optional next step", "next step", "следующий шаг"],
    &["current work", "текущая работа"],
    &["pending tasks", "ожидающие задачи"],
];

/// Прочие разделы шаблона конспекта Claude (живой журнал 2.1.314, английские и русские).
const OTHER_SECTIONS: [&str; 13] = [
    "summary",
    "primary request and intent",
    "key technical concepts",
    "files and code sections",
    "errors and fixes",
    "problem solving",
    "all user messages",
    "основной запрос и намерение",
    "ключевые технические решения",
    "файлы и разделы",
    "ошибки и их исправление",
    "решённые задачи",
    "все сообщения пользователя",
];

/// Заголовок раздела: строка с начала (`#…`, `N.` или `**…**`) и имя до `:`/`(`/`**`.
/// Заголовок — Markdown `#…`, известное имя ([`KEY_SECTIONS`], [`OTHER_SECTIONS`]) или
/// нумерованная строка с двоеточием в конце (`N. Имя:`, `N. **Имя:**`). Пункт списка без отступа
/// («1. сделать X») раздел не обрывает.
/// `Some(Some(k))` — ключевой раздел `k` из [`KEY_SECTIONS`], `Some(None)` — прочий.
fn heading(line: &str) -> Option<Option<usize>> {
    let l = line.trim_end();
    if l.starts_with([' ', '\t']) {
        return None;
    }
    let mut rest = l.trim_start_matches('#').trim_start();
    let hashed = rest.len() != l.len();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let numbered = digits > 0 && rest[digits..].starts_with(". ");
    if numbered {
        rest = rest[digits + 2..].trim_start();
    }
    let bold = rest.starts_with("**");
    if !(hashed || numbered || bold) {
        return None;
    }
    let name = rest.trim_start_matches('*');
    let end = name.find([':', '(', '*']).unwrap_or(name.len());
    let name = name[..end].trim().to_lowercase();
    if name.is_empty() || name.chars().count() > 60 {
        return None;
    }
    let key = KEY_SECTIONS
        .iter()
        .position(|names| names.contains(&name.as_str()));
    let known = key.is_some() || OTHER_SECTIONS.contains(&name.as_str());
    let colon = l.trim_end_matches('*').ends_with(':');
    (known || hashed || (numbered && colon)).then_some(key)
}

/// Конспект не больше `max` байт: хвост, а не начало (§8а). Ключевые разделы берутся целиком
/// (при нехватке места — по приоритету [`KEY_SECTIONS`], не влезший — его хвост), остальное — с
/// конца до предела. Несмежные части соединяются `\n…\n`, порядок — как в тексте.
pub fn summary_tail(text: &str, max: usize) -> String {
    const SEP: &str = "\n…\n";
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let mut heads: Vec<(usize, Option<usize>)> = Vec::new();
    let mut off = 0;
    for line in text.split_inclusive('\n') {
        if let Some(k) = heading(line) {
            heads.push((off, k));
        }
        off += line.len();
    }
    // каждая часть оплачивается с разделителем — сумма не больше `max`
    let mut chosen: Vec<(usize, usize)> = Vec::new();
    let mut used = 0usize;
    let mut take = |chosen: &mut Vec<(usize, usize)>, b: usize, e: usize| -> bool {
        let room = max.saturating_sub(used + SEP.len());
        let t = tail_bytes(&text[b..e], room);
        if t.is_empty() {
            return false;
        }
        chosen.push((e - t.len(), e));
        used += t.len() + SEP.len();
        e - t.len() == b
    };
    for k in 0..KEY_SECTIONS.len() {
        for (i, &(b, key)) in heads.iter().enumerate() {
            if key == Some(k) {
                let e = heads.get(i + 1).map_or(text.len(), |n| n.0);
                take(&mut chosen, b, e);
            }
        }
    }
    chosen.sort();
    let mut gaps: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for &(b, e) in &chosen {
        if b > at {
            gaps.push((at, b));
        }
        at = at.max(e);
    }
    if at < text.len() {
        gaps.push((at, text.len()));
    }
    for &(b, e) in gaps.iter().rev() {
        // промежуток взят не целиком — ближе к началу места уже нет
        if !take(&mut chosen, b, e) {
            break;
        }
    }
    chosen.sort();
    let mut out = String::with_capacity(max);
    let mut at: Option<usize> = None;
    for (b, e) in chosen {
        if at.is_some_and(|a| a != b) {
            out.push_str(SEP);
        }
        out.push_str(&text[b..e]);
        at = Some(e);
    }
    let out = out.trim();
    tail_bytes(out, max).to_string()
}

/// Флаги командной строки с паролем или токеном, которых не знает сканер `zam_secrets`
/// (только поле команд — в прозе «basic»/«bearer» и `-u` не пароль):
/// - `-pXXX`/`-p'a b'` у mysql-подобных команд и `sshpass`;
/// - `--password X`/`--password=X` (и `--passphrase`, `--secret-key`, `--client-secret`, `--token`,
///   `--api-key`, `--secret`, …);
/// - `-u user:pass`/`--user user:pass` у curl, wget и подобных (числовые `uid:gid` — нет);
/// - значение заголовка `Authorization:` (`Bearer X`, `Basic X`, `Token X`) и `X-Api-Key:`;
///   ссылка на переменную (`Bearer $TOKEN`) и сама схема не маскируются.
///
/// Значение заменяется меткой `‹secret›`; ссылки на переменные (`$X`, `%X%`) и заглушки (`<…>`)
/// остаются.
pub fn mask_flags(text: &str) -> String {
    static RE: OnceLock<[Regex; 4]> = OnceLock::new();
    let re = RE.get_or_init(|| {
        [
            // --password X, --password=X, --token "a b"
            Regex::new(
                r#"(?i)(?:^|[\s'"])--(?:password|passwd|pass|pwd|passphrase|token|api-key|apikey|api-token|secret|secret-key|client-secret|access-token|auth-token|http-password|proxy-password)(?:=|\s+)("[^"]*"|'[^']*'|[^\s"']+)"#,
            )
            .expect("regex"),
            // curl -u user:pass, --user user:pass, -uuser:pass
            Regex::new(r#"(?:^|\s)(?:-u\s*|--user(?:=|\s+))["']?([^\s:"']+):([^\s"'@]+)"#)
                .expect("regex"),
            // Authorization: Bearer X | Basic X | Token X | X; X-Api-Key: X
            Regex::new(
                r#"(?i)(?:authorization\s*:\s*(?:(?:bearer|basic|token|digest)\s+)?|x-api-key\s*:\s*)([^\s"']{6,})"#,
            )
            .expect("regex"),
            // -pXXX, -p'a b' (значение слитно с флагом)
            Regex::new(r#"(?:^|\s)-p("[^"]*"|'[^']*'|[^\s"']+)"#).expect("regex"),
        ]
    });
    static GATES: OnceLock<[Regex; 2]> = OnceLock::new();
    let [mysql, curl] = GATES.get_or_init(|| {
        [
            Regex::new(r"(?i)\b(?:mysql\w*|mariadb\w*|sshpass)\b").expect("regex"),
            Regex::new(r"(?i)\b(?:curl|wget|wget2|http|https|httpie|xh|lftp|aria2c)\b")
                .expect("regex"),
        ]
    });
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, r) in re.iter().enumerate() {
        if (i == 3 && !mysql.is_match(text)) || (i == 1 && !curl.is_match(text)) {
            continue;
        }
        for c in r.captures_iter(text) {
            // `-u user:pass` — значение во второй группе; числовые `uid:gid` — не пароль
            let m = if i == 1 {
                let (Some(u), Some(p)) = (c.get(1), c.get(2)) else {
                    continue;
                };
                let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
                if digits(u.as_str()) && digits(p.as_str()) {
                    continue;
                }
                p
            } else {
                let Some(m) = c.get(1) else { continue };
                m
            };
            let v = m.as_str();
            let (mut b, mut e) = (m.start(), m.end());
            if (v.starts_with('"') || v.starts_with('\'')) && v.len() >= 2 {
                b += 1;
                e -= 1;
            }
            let inner = &text[b..e];
            // значение целиком — ссылка на переменную (`$NAME`, `${NAME}`, `%NAME%`) или заглушка
            // `<…>` ([`is_reference`]); у заголовка без значения длиной ≥ 6 регулярка берёт
            // слово-схему — его не трогаем
            let scheme = i == 2
                && ["bearer", "basic", "token", "digest"]
                    .iter()
                    .any(|w| inner.eq_ignore_ascii_case(w));
            if inner.is_empty()
                || scheme
                || inner.contains(zam_secrets::SECRET_MARK)
                || is_reference(inner)
            {
                continue;
            }
            ranges.push((b, e));
        }
    }
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (b, e) in ranges {
        if b < pos {
            continue;
        }
        out.push_str(&text[pos..b]);
        out.push_str(zam_secrets::SECRET_MARK);
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

/// Значение — целиком ссылка на переменную или заглушка, а не секрет: `$NAME`, `${NAME}`,
/// `%NAME%` (имя — `[A-Za-z_][A-Za-z0-9_]*`) или `<…>`. `%2Fabc…`, `$2b$10$…` (хэш bcrypt) —
/// не ссылки.
fn is_reference(v: &str) -> bool {
    let ident = |s: &str| {
        let mut c = s.chars();
        c.next()
            .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
    };
    if let Some(r) = v.strip_prefix("${") {
        return r.strip_suffix('}').is_some_and(ident);
    }
    if let Some(r) = v.strip_prefix('$') {
        return ident(r);
    }
    if let Some(r) = v.strip_prefix('%') {
        return r.strip_suffix('%').is_some_and(ident);
    }
    v.len() >= 2 && v.starts_with('<') && v.ends_with('>')
}

/// Текст без вставок `<тег>…</тег>`; пробелы по краям сняты. Незакрытая вставка (`<тег>` без
/// `</тег>`) вырезается до конца текста: это вывод, обрезанный клиентом или самим журналом, и
/// слова человека после него не идут (лучше потерять хвост запроса, чем взять вывод команды).
pub(crate) fn strip_tags(text: &str, tags: &[&str]) -> String {
    let mut s = text.to_string();
    loop {
        let found = tags
            .iter()
            .filter_map(|t| s.find(&format!("<{t}>")).map(|i| (i, *t)))
            .min();
        let Some((i, t)) = found else { break };
        let close = format!("</{t}>");
        let end = s[i..].find(&close).map_or(s.len(), |j| i + j + close.len());
        s.replace_range(i..end, "");
    }
    s.trim().to_string()
}
