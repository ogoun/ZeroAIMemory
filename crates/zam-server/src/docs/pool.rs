//! Пул рабочих процессов `zam-server doc-worker` (M5 §2).
//!
//! - `docs.workers` долгоживущих потоков-«слотов»; каждый запускает свой процесс по требованию,
//!   отдаёт ему задания по одному и перезапускает после `docs.worker_jobs` заданий или падения.
//!   Процесс запускается из потока слота: на Linux `PDEATHSIG` привязан к потоку-родителю.
//!   Очередь заданий ограничена (`2 × workers`): `submit` ждёт места, `Bytes` не копятся.
//! - Привязка к серверу и лимит памяти: Windows — свой Job Object на процесс
//!   (`KILL_ON_JOB_CLOSE`, `JOB_OBJECT_LIMIT_PROCESS_MEMORY`); Linux — `PDEATHSIG` (`SIGKILL`),
//!   `RLIMIT_AS` и своя группа процессов. Секреты сервера в окружение процесса не попадают.
//! - Дешёвая изоляция (финальное ревью M5, I-3): Windows — `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1`
//!   (процесс не запускает дочерних); Linux — `PR_SET_NO_NEW_PRIVS` и рабочий каталог `/`.
//!   Ответы процесса недоверенные: вид извлечения сверяется с запросом, HTML подсветки — с
//!   форматом `zam_markdown` (`api::previews`). Полная песочница ОС (seccomp/landlock,
//!   AppContainer/restricted token) — M7.
//! - Обмен: запрос пишет поток записи процесса, stdout читается **только под ожидающий запрос**
//!   (разрешение на один кадр): процесс, пишущий кадры без запроса, упирается в канал, память
//!   сервера не растёт. Кадр с чужим номером — нарушение протокола: процесс убивается, задание
//!   повторяется один раз на новом процессе; повторное нарушение и неверный кадр ответа —
//!   [`ParseFailure::Crash`] (процесс вёл себя не как наш рабочий процесс — как при падении,
//!   а не ошибка документа).
//! - Срок `docs.timeout_ms` отсчитывается до отправки запроса (запись тоже под сроком): по
//!   истечении процесс убивается — [`ParseFailure::Timeout`]; внутри процесса разбор получает
//!   срок на 20 % короче и отдаёт частичный результат сам.
//! - Падение — [`ParseFailure::Crash`]; нехватка памяти — [`ParseFailure::Oom`] по признакам:
//!   сообщение аллокатора Rust или C++ в stderr; Windows — код `0xE0000008` (OOM pdfium) или пик
//!   памяти процесса у лимита; Linux — пик `VmPeak` у лимита или `SIGKILL` не от пула (OOM killer).
//! - stderr процесса (JSON-журнал) пересылается в журнал сервера с `component = "doc-worker"`, не
//!   больше [`LOG_BURST`] строк за [`LOG_WINDOW`]; пропущенные считаются и сообщаются итогом.

use std::borrow::Cow;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use base64::Engine as _;
use zam_docs::{DocError, DocKind, Extracted, Limits};

use super::protocol::{
    Request, Response, WireHtml, WireLimits, WireSheetPage, read_frame, request_body,
    request_frame_max, response_frame_max, write_frame,
};
use crate::config::DocsSection;

/// Предел причины от процесса (`pong.pdfium`, `error.message`), байт.
pub const REASON_MAX: usize = 4096;
/// Строк stderr процесса в журнал сервера — не больше стольких за [`LOG_WINDOW`].
pub const LOG_BURST: u32 = 200;
/// Окно ограничения частоты журнала процесса.
pub const LOG_WINDOW: Duration = Duration::from_secs(10);

/// Сбой разбора в рабочем процессе; [`ParseFailure::reason`] — причина для `index_state`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseFailure {
    /// Процесс упал, не запустился или нарушил протокол (неверный кадр, кадр без запроса
    /// после повтора).
    #[error("рабочий процесс разбора упал")]
    Crash,
    /// Срок разбора истёк, процесс убит.
    #[error("истёк срок разбора")]
    Timeout,
    /// Процесс упёрся в лимит памяти.
    #[error("нехватка памяти при разборе")]
    Oom,
    /// Документ не разобран (повреждён, слишком велик, вид или pdfium недоступны).
    #[error(transparent)]
    Doc(DocError),
}

impl ParseFailure {
    /// Причина для `index_state = failed`: `parse_crash | parse_timeout | parse_oom`;
    /// ошибка документа — `parse_error`.
    pub fn reason(&self) -> &'static str {
        match self {
            ParseFailure::Crash => "parse_crash",
            ParseFailure::Timeout => "parse_timeout",
            ParseFailure::Oom => "parse_oom",
            ParseFailure::Doc(_) => "parse_error",
        }
    }
}

/// Тестовые задания (фича `test-hooks`).
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy)]
pub enum TestOp {
    /// Аварийное завершение процесса.
    Crash,
    /// Зависание на `ms` мс.
    Sleep(u64),
    /// Выделение `mb` МиБ.
    Alloc(u64),
    /// Ответ, затем `frames` лишних кадров по `mb` МиБ.
    Spam { frames: u32, mb: u64 },
    /// Ответ `unsupported` с причиной в `kb` КиБ.
    LongError(u64),
}

/// Задание слоту.
enum Op {
    Extract {
        kind: DocKind,
        bytes: Bytes,
    },
    Thumb {
        bytes: Bytes,
        page: u32,
        max_px: u32,
    },
    Sheet {
        kind: DocKind,
        bytes: Bytes,
        no: u32,
        from: u32,
        rows: u32,
    },
    Image {
        bytes: Bytes,
        max_px: u32,
    },
    Highlight {
        bytes: Bytes,
        name: String,
        truncated: bool,
    },
    Ping,
    #[cfg(feature = "test-hooks")]
    Test(TestOp),
}

/// Результат задания.
enum Outcome {
    Extracted(Extracted),
    Thumb(Vec<u8>),
    Sheet(WireSheetPage),
    Image(Vec<u8>),
    Html(WireHtml),
    Pong(Option<String>),
    #[cfg(feature = "test-hooks")]
    Done,
}

struct Job {
    op: Op,
    reply: tokio::sync::oneshot::Sender<Result<Outcome, ParseFailure>>,
}

/// Неизменные настройки пула.
struct Settings {
    program: PathBuf,
    pdfium: PathBuf,
    limits: Limits,
    timeout: Duration,
    memory: u64,
    worker_jobs: u32,
    request_max: usize,
    response_max: usize,
}

/// Общее состояние слотов.
#[derive(Default)]
struct Shared {
    /// Причина недоступности pdfium (последний ответ процесса), `None` — загружен или неизвестно.
    pdfium_error: Mutex<Option<String>>,
    /// Сколько процессов запущено за жизнь пула.
    spawned: AtomicU64,
    /// pid текущего процесса каждого слота (0 — нет процесса).
    pids: Mutex<Vec<u32>>,
    /// Тест: мягкий срок разбора внутри процесса, мс (`u64::MAX` — 80 % `docs.timeout_ms`).
    #[cfg(feature = "test-hooks")]
    soft_ms: AtomicU64,
}

/// Пул рабочих процессов разбора документов.
pub struct DocPool {
    /// Очередь заданий; `None` — пул закрыт ([`DocPool::close`]).
    tx: Mutex<Option<tokio::sync::mpsc::Sender<Job>>>,
    shared: Arc<Shared>,
}

/// Имя библиотеки pdfium платформы.
pub fn pdfium_default_name() -> &'static str {
    if cfg!(windows) {
        "pdfium.dll"
    } else {
        "libpdfium.so"
    }
}

/// Путь pdfium: абсолютный — как есть, относительный и по умолчанию — от каталога `exe_dir`.
pub fn resolve_pdfium(cfg: &DocsSection, exe_dir: &Path) -> PathBuf {
    match &cfg.pdfium_dylib {
        Some(p) if p.is_absolute() => p.clone(),
        Some(p) => exe_dir.join(p),
        None => exe_dir.join(pdfium_default_name()),
    }
}

/// Строка не длиннее [`REASON_MAX`] байт (по границе символа, с «…»).
fn cap_reason(mut s: String) -> String {
    if s.len() > REASON_MAX {
        let mut cut = REASON_MAX - '…'.len_utf8();
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

impl DocPool {
    /// Пул с рабочими процессами из исполняемого файла сервера.
    pub fn new(cfg: &DocsSection) -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zam-server"));
        Self::with_program(cfg, exe)
    }

    /// Пул с рабочими процессами из `program` (подкоманда `doc-worker`); pdfium по
    /// умолчанию ищется рядом с исполняемым файлом текущего процесса.
    pub fn with_program(cfg: &DocsSection, program: PathBuf) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let limits = cfg.limits();
        let settings = Arc::new(Settings {
            program,
            pdfium: resolve_pdfium(cfg, &exe_dir),
            timeout: Duration::from_millis(cfg.timeout_ms),
            memory: cfg.worker_memory_mb * 1024 * 1024,
            worker_jobs: cfg.worker_jobs,
            request_max: request_frame_max(limits.max_bytes),
            response_max: response_frame_max(limits.max_chars),
            limits,
        });
        let workers = cfg.workers.max(1);
        let shared = Arc::new(Shared {
            pids: Mutex::new(vec![0; workers]),
            #[cfg(feature = "test-hooks")]
            soft_ms: AtomicU64::new(u64::MAX),
            ..Shared::default()
        });
        let (tx, rx) = tokio::sync::mpsc::channel::<Job>(workers * 2);
        let rx = Arc::new(Mutex::new(rx));
        for slot in 0..workers {
            let (rx, settings, shared) = (rx.clone(), settings.clone(), shared.clone());
            // поток живёт, пока жив пул: на Linux его смерть убивает процесс слота (PDEATHSIG)
            let r = std::thread::Builder::new()
                .name(format!("doc-worker-{slot}"))
                .spawn(move || slot_loop(slot, &rx, &settings, &shared));
            if let Err(e) = r {
                tracing::error!(component = "doc-worker", "поток слота не запущен: {e}");
            }
        }
        DocPool {
            tx: Mutex::new(Some(tx)),
            shared,
        }
    }

    async fn submit(&self, op: Op) -> Result<Outcome, ParseFailure> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let tx = lock(&self.tx).clone().ok_or(ParseFailure::Crash)?;
        // очередь ограничена: ждём места, а не копим файлы без предела
        tx.send(Job { op, reply })
            .await
            .map_err(|_| ParseFailure::Crash)?;
        drop(tx);
        rx.await.unwrap_or(Err(ParseFailure::Crash))
    }

    /// Закрывает пул: новые задания — [`ParseFailure::Crash`], слоты дорабатывают текущее
    /// задание и завершают свои процессы (pid исчезают из [`DocPool::worker_pids`]).
    pub fn close(&self) {
        lock(&self.tx).take();
    }

    /// Извлекает структуру документа в рабочем процессе.
    pub async fn extract(&self, kind: DocKind, bytes: Bytes) -> Result<Extracted, ParseFailure> {
        extracted_of(kind, self.submit(Op::Extract { kind, bytes }).await?)
    }

    /// Миниатюра страницы `page` (с 1) PDF в PNG.
    pub async fn thumb(
        &self,
        bytes: Bytes,
        page: u32,
        max_px: u32,
    ) -> Result<Vec<u8>, ParseFailure> {
        match self
            .submit(Op::Thumb {
                bytes,
                page,
                max_px,
            })
            .await?
        {
            Outcome::Thumb(png) => Ok(png),
            _ => Err(unexpected()),
        }
    }

    /// Страница листа `no` (с 1, порядок книги) таблицы `kind`: `rows` строк со строки `from`
    /// (превью, задача 8). Срок разбора внутри процесса — как у извлечения.
    pub async fn sheet_page(
        &self,
        kind: DocKind,
        bytes: Bytes,
        no: u32,
        from: u32,
        rows: u32,
    ) -> Result<WireSheetPage, ParseFailure> {
        match self
            .submit(Op::Sheet {
                kind,
                bytes,
                no,
                from,
                rows,
            })
            .await?
        {
            Outcome::Sheet(p) => Ok(p),
            _ => Err(unexpected()),
        }
    }

    /// Миниатюра изображения (PNG, JPEG, GIF, WebP) в PNG (превью, задача 8).
    pub async fn image_thumb(&self, bytes: Bytes, max_px: u32) -> Result<Vec<u8>, ParseFailure> {
        match self.submit(Op::Image { bytes, max_px }).await? {
            Outcome::Image(png) => Ok(png),
            _ => Err(unexpected()),
        }
    }

    /// HTML syntect начала файла `name` (превью, задача 8); `truncated` — файл длиннее `bytes`.
    pub async fn highlight(
        &self,
        bytes: Bytes,
        name: String,
        truncated: bool,
    ) -> Result<WireHtml, ParseFailure> {
        match self
            .submit(Op::Highlight {
                bytes,
                name,
                truncated,
            })
            .await?
        {
            Outcome::Html(h) => Ok(h),
            _ => Err(unexpected()),
        }
    }

    /// Проверка процесса и pdfium: `Ok(None)` — pdfium загружен, `Ok(Some(причина))` — нет.
    pub async fn ping(&self) -> Result<Option<String>, ParseFailure> {
        match self.submit(Op::Ping).await? {
            Outcome::Pong(p) => Ok(p),
            _ => Err(unexpected()),
        }
    }

    /// Причина недоступности pdfium по последнему ответу процесса (для `/index/status`).
    pub fn pdfium_error(&self) -> Option<String> {
        lock(&self.shared.pdfium_error).clone()
    }

    /// Сколько процессов запущено за жизнь пула.
    pub fn spawned(&self) -> u64 {
        self.shared.spawned.load(Ordering::Relaxed)
    }

    /// pid живых процессов слотов.
    pub fn worker_pids(&self) -> Vec<u32> {
        lock(&self.shared.pids)
            .iter()
            .copied()
            .filter(|p| *p != 0)
            .collect()
    }

    /// Тестовое задание.
    #[cfg(feature = "test-hooks")]
    pub async fn test_op(&self, op: TestOp) -> Result<(), ParseFailure> {
        self.submit(Op::Test(op)).await.map(|_| ())
    }

    /// Тест: мягкий срок разбора внутри процесса `ms` (`None` — 80 % `docs.timeout_ms`):
    /// `Some(0)` даёт частичный по сроку результат (`truncated: timeout_ms`) на первой
    /// проверке срока.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn set_soft_ms(&self, ms: Option<u64>) {
        self.shared
            .soft_ms
            .store(ms.unwrap_or(u64::MAX), Ordering::SeqCst);
    }
}

/// Ответ на `Extract { kind }`: извлечение того же вида. Другой вид — процесс ответил не на
/// этот запрос (как неверный кадр: сбой, не свойство файла и не в кэш; финальное ревью M5, M-3).
fn extracted_of(kind: DocKind, out: Outcome) -> Result<Extracted, ParseFailure> {
    match out {
        Outcome::Extracted(x) if x.kind == kind => Ok(x),
        _ => Err(unexpected()),
    }
}

fn unexpected() -> ParseFailure {
    ParseFailure::Doc(DocError::Internal("неожиданный ответ".into()))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn slot_loop(
    slot: usize,
    rx: &Mutex<tokio::sync::mpsc::Receiver<Job>>,
    s: &Settings,
    shared: &Shared,
) {
    let mut worker: Option<Worker> = None;
    let mut next_id = 0u64;
    loop {
        let job = {
            let mut guard = lock(rx);
            match guard.blocking_recv() {
                Some(j) => j,
                None => break,
            }
        };
        let r = run_job(slot, &mut worker, &mut next_id, job.op, s, shared);
        let _ = job.reply.send(r);
    }
    drop(worker);
    lock(&shared.pids)[slot] = 0;
}

/// Тело кадра запроса и срок разбора внутри процесса.
fn body_of(op: &Op, s: &Settings, shared: &Shared) -> Vec<u8> {
    let soft_ms = (s.timeout.as_millis() as u64).saturating_mul(8) / 10;
    #[cfg(feature = "test-hooks")]
    let soft_ms = match shared.soft_ms.load(Ordering::SeqCst) {
        u64::MAX => soft_ms,
        ms => ms,
    };
    #[cfg(not(feature = "test-hooks"))]
    let _ = shared;
    match op {
        Op::Extract { kind, bytes } => request_body(
            &Request::Extract {
                kind: kind.as_str().into(),
                limits: WireLimits::from(&s.limits),
                soft_ms,
                bytes: Cow::Borrowed(""),
            },
            Some(bytes),
        ),
        Op::Thumb {
            bytes,
            page,
            max_px,
        } => request_body(
            &Request::Thumb {
                page: *page,
                max_px: *max_px,
                bytes: Cow::Borrowed(""),
            },
            Some(bytes),
        ),
        Op::Sheet {
            kind,
            bytes,
            no,
            from,
            rows,
        } => request_body(
            &Request::Sheet {
                kind: kind.as_str().into(),
                no: *no,
                from: *from,
                rows: *rows,
                limits: WireLimits::from(&s.limits),
                soft_ms,
                bytes: Cow::Borrowed(""),
            },
            Some(bytes),
        ),
        Op::Image { bytes, max_px } => request_body(
            &Request::Image {
                max_px: *max_px,
                bytes: Cow::Borrowed(""),
            },
            Some(bytes),
        ),
        Op::Highlight {
            bytes,
            name,
            truncated,
        } => request_body(
            &Request::Highlight {
                name: name.clone(),
                truncated: *truncated,
                bytes: Cow::Borrowed(""),
            },
            Some(bytes),
        ),
        Op::Ping => request_body(&Request::Ping, None),
        #[cfg(feature = "test-hooks")]
        Op::Test(t) => request_body(
            &match *t {
                TestOp::Crash => Request::Crash,
                TestOp::Sleep(ms) => Request::Sleep { ms },
                TestOp::Alloc(mb) => Request::Alloc { mb },
                TestOp::Spam { frames, mb } => Request::Spam { frames, mb },
                TestOp::LongError(kb) => Request::LongError { kb },
            },
            None,
        ),
    }
}

fn clear_pid(shared: &Shared, slot: usize) {
    lock(&shared.pids)[slot] = 0;
}

fn run_job(
    slot: usize,
    worker: &mut Option<Worker>,
    next_id: &mut u64,
    op: Op,
    s: &Settings,
    shared: &Shared,
) -> Result<Outcome, ParseFailure> {
    if let Op::Extract { bytes, .. }
    | Op::Thumb { bytes, .. }
    | Op::Sheet { bytes, .. }
    | Op::Image { bytes, .. }
    | Op::Highlight { bytes, .. } = &op
        && bytes.len() as u64 > s.limits.max_bytes
    {
        return Err(ParseFailure::Doc(DocError::TooLarge));
    }
    let body = Arc::new(body_of(&op, s, shared));
    drop(op);
    // нарушение протокола (кадр с чужим номером) — один повтор на новом процессе
    for attempt in 0..2 {
        // процесс, умерший между заданиями, заменяется до отправки
        if worker.as_mut().is_some_and(|w| !w.alive())
            && let Some(w) = worker.take()
        {
            w.finish(true);
            clear_pid(shared, slot);
        }
        if worker.is_none() {
            match Worker::spawn(s) {
                Ok(w) => {
                    shared.spawned.fetch_add(1, Ordering::Relaxed);
                    lock(&shared.pids)[slot] = w.pid;
                    *worker = Some(w);
                }
                Err(e) => {
                    tracing::error!(component = "doc-worker", "рабочий процесс не запущен: {e}");
                    return Err(ParseFailure::Crash);
                }
            }
        }
        let w = worker.as_mut().expect("процесс запущен");
        *next_id += 1;
        let id = *next_id;
        // срок — до отправки: запись большого запроса тоже под сроком
        let deadline = Instant::now() + s.timeout;
        let got = w.exchange(id, body.clone(), deadline);
        let result = match got {
            Ok(resp) => {
                w.jobs += 1;
                outcome(resp, shared)
            }
            Err(Recv::Timeout) => {
                let w = worker.take().expect("процесс есть");
                tracing::warn!(
                    component = "doc-worker",
                    pid = w.pid,
                    "срок разбора истёк — процесс убит"
                );
                w.kill();
                clear_pid(shared, slot);
                return Err(ParseFailure::Timeout);
            }
            Err(Recv::Exited) => {
                let w = worker.take().expect("процесс есть");
                let f = w.reap();
                clear_pid(shared, slot);
                return Err(f);
            }
            Err(Recv::Stale(got)) => {
                let w = worker.take().expect("процесс есть");
                tracing::warn!(
                    component = "doc-worker",
                    pid = w.pid,
                    "кадр без запроса (номер {got}, ждали {id}) — процесс убит"
                );
                w.kill();
                clear_pid(shared, slot);
                if attempt == 0 {
                    continue;
                }
                tracing::warn!(
                    component = "doc-worker",
                    "повторное нарушение протокола рабочим процессом — сбой разбора"
                );
                return Err(ParseFailure::Crash);
            }
            Err(Recv::Bad(msg)) => {
                let w = worker.take().expect("процесс есть");
                tracing::warn!(
                    component = "doc-worker",
                    pid = w.pid,
                    "неверный ответ: {msg}"
                );
                w.kill();
                clear_pid(shared, slot);
                return Err(ParseFailure::Crash);
            }
        };
        if worker.as_ref().is_some_and(|w| w.jobs >= s.worker_jobs) {
            if let Some(w) = worker.take() {
                w.finish(true);
            }
            clear_pid(shared, slot);
        }
        return result;
    }
    Err(ParseFailure::Crash)
}

/// Ответ процесса → результат задания; учитывает состояние pdfium. Причины от процесса — не
/// длиннее [`REASON_MAX`].
fn outcome(resp: Response, shared: &Shared) -> Result<Outcome, ParseFailure> {
    let b64 = base64::engine::general_purpose::STANDARD;
    match resp {
        Response::Extracted(x) => {
            let pdf = x.kind == DocKind::Pdf.as_str();
            let x = Extracted::try_from(x).map_err(ParseFailure::Doc)?;
            if pdf {
                *lock(&shared.pdfium_error) = None;
            }
            Ok(Outcome::Extracted(x))
        }
        Response::Thumb { png } => {
            *lock(&shared.pdfium_error) = None;
            b64.decode(png)
                .map(Outcome::Thumb)
                .map_err(|e| ParseFailure::Doc(DocError::Internal(format!("base64: {e}"))))
        }
        Response::Sheet(p) => Ok(Outcome::Sheet(p)),
        Response::Image { png } => b64
            .decode(png)
            .map(Outcome::Image)
            .map_err(|e| ParseFailure::Doc(DocError::Internal(format!("base64: {e}")))),
        Response::Html(h) => Ok(Outcome::Html(h)),
        Response::Pong { pdfium } => {
            let pdfium = pdfium.map(cap_reason);
            lock(&shared.pdfium_error).clone_from(&pdfium);
            Ok(Outcome::Pong(pdfium))
        }
        Response::Error(mut e) => {
            e.message = cap_reason(e.message);
            if e.kind == "unsupported" && !e.message.is_empty() {
                tracing::warn!(component = "doc-worker", "PDF недоступен: {}", e.message);
                *lock(&shared.pdfium_error) = Some(e.message.clone());
            }
            Err(ParseFailure::Doc(DocError::from(e)))
        }
        #[cfg(feature = "test-hooks")]
        Response::Done => Ok(Outcome::Done),
    }
}

/// Итог ожидания ответа.
enum Recv {
    Timeout,
    /// Процесс закрыл stdout или stdin (завершился или упал).
    Exited,
    /// Кадр с чужим номером — лишний кадр без запроса.
    Stale(u64),
    /// Неверный кадр или JSON.
    Bad(String),
}

/// События потоков процесса для слота.
enum Event {
    Frame(u64, Vec<u8>),
    /// Запрос не записан (процесс закрыл stdin).
    WriteFailed(String),
    /// stdout закрыт.
    Eof,
    /// Неверный кадр ответа.
    BadFrame(String),
}

/// Признак нехватки памяти в строке stderr: аллокатор Rust (`memory allocation of N bytes
/// failed`) или C++ (`std::bad_alloc`, «out of memory»).
fn oom_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    (l.contains("memory allocation of") && l.contains("failed"))
        || l.contains("bad_alloc")
        || l.contains("out of memory")
}

/// Ограничение частоты журнала процесса: не больше [`LOG_BURST`] строк за [`LOG_WINDOW`].
struct RateLimit {
    start: Instant,
    used: u32,
    dropped: u64,
}

impl RateLimit {
    fn new() -> Self {
        RateLimit {
            start: Instant::now(),
            used: 0,
            dropped: 0,
        }
    }

    /// `true` — строку можно журналировать; при смене окна сообщает о пропущенных.
    fn allow(&mut self, pid: u32) -> bool {
        if self.start.elapsed() >= LOG_WINDOW {
            self.flush(pid);
            self.start = Instant::now();
            self.used = 0;
        }
        if self.used < LOG_BURST {
            self.used += 1;
            true
        } else {
            self.dropped += 1;
            false
        }
    }

    fn flush(&mut self, pid: u32) {
        if self.dropped > 0 {
            tracing::warn!(
                component = "doc-worker",
                pid,
                dropped = self.dropped,
                "журнал рабочего процесса: строки пропущены (ограничение частоты)"
            );
            self.dropped = 0;
        }
    }
}

/// Пересылает stderr процесса в журнал сервера построчно (строка — не длиннее 16 КиБ).
fn forward_stderr(r: impl std::io::Read, pid: u32, oom: &AtomicBool) {
    const LINE_MAX: u64 = 16 * 1024;
    let mut reader = BufReader::new(r);
    let mut buf = Vec::with_capacity(512);
    let mut rate = RateLimit::new();
    loop {
        buf.clear();
        match (&mut reader).take(LINE_MAX).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !buf.ends_with(b"\n") && buf.len() as u64 >= LINE_MAX {
            // хвост длинной строки пропускается
            let mut skip = Vec::new();
            loop {
                skip.clear();
                match (&mut reader).take(LINE_MAX).read_until(b'\n', &mut skip) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if skip.ends_with(b"\n") => break,
                    Ok(_) => {}
                }
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // признак OOM ищется во всех строках, журналируются — в пределах частоты
        if oom_line(line) {
            oom.store(true, Ordering::SeqCst);
        }
        if !rate.allow(pid) {
            continue;
        }
        match zam_index::child::parse_json_line(line) {
            Some(l) => {
                let m = l.message.as_str();
                match l.level {
                    tracing::Level::ERROR => tracing::error!(component = "doc-worker", pid, "{m}"),
                    tracing::Level::WARN => tracing::warn!(component = "doc-worker", pid, "{m}"),
                    tracing::Level::INFO => tracing::info!(component = "doc-worker", pid, "{m}"),
                    _ => tracing::debug!(component = "doc-worker", pid, "{m}"),
                }
            }
            // не JSON — паника, сообщение аллокатора, вывод pdfium
            None => tracing::warn!(component = "doc-worker", pid, "{line}"),
        }
    }
    rate.flush(pid);
}

/// Поток записи: запросы `(номер, тело)` в stdin; после записи — разрешение читателю на один
/// кадр. Ошибка записи — событие слоту.
fn writer_loop(
    mut stdin: ChildStdin,
    jobs: Receiver<(u64, Arc<Vec<u8>>)>,
    permits: SyncSender<()>,
    events: SyncSender<Event>,
) {
    while let Ok((id, body)) = jobs.recv() {
        if let Err(e) = write_frame(&mut stdin, id, &body) {
            let _ = events.send(Event::WriteFailed(e.to_string()));
            break;
        }
        drop(body);
        if permits.send(()).is_err() {
            break;
        }
    }
    // stdin закрывается — процесс выходит сам по EOF
}

/// Поток чтения: один кадр stdout на одно разрешение; без запроса stdout не читается.
fn reader_loop(
    stdout: std::process::ChildStdout,
    max: usize,
    permits: Receiver<()>,
    events: SyncSender<Event>,
) {
    let mut r = BufReader::with_capacity(64 * 1024, stdout);
    while permits.recv().is_ok() {
        let ev = match read_frame(&mut r, max) {
            Ok(Some((id, f))) => Event::Frame(id, f),
            Ok(None) => Event::Eof,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Event::Eof,
            Err(e) => Event::BadFrame(e.to_string()),
        };
        let last = !matches!(ev, Event::Frame(..));
        if events.send(ev).is_err() || last {
            break;
        }
    }
}

/// Один рабочий процесс.
struct Worker {
    child: Child,
    pid: u32,
    /// Запросы потоку записи; `None` — закрыт (stdin процесса закрывается).
    jobs_tx: Option<SyncSender<(u64, Arc<Vec<u8>>)>>,
    events: Option<Receiver<Event>>,
    threads: Vec<JoinHandle<()>>,
    oom: Arc<AtomicBool>,
    jobs: u32,
    memory: u64,
    /// Наибольший замеченный `VmPeak` (Linux), байт.
    #[cfg(unix)]
    vm_peak: u64,
    #[cfg(windows)]
    job: zam_index::child::KillJob,
    /// Процесс убит пулом (сигнал смерти — не OOM killer).
    killed: bool,
    done: bool,
}

impl Worker {
    fn spawn(s: &Settings) -> Result<Self, String> {
        let mut cmd = Command::new(&s.program);
        cmd.arg("doc-worker")
            .arg("--pdfium")
            .arg(&s.pdfium)
            .arg("--max-frame")
            .arg(s.request_max.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, _) in std::env::vars_os() {
            if zam_index::child::is_secret_env(&k.to_string_lossy()) {
                cmd.env_remove(&k);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // CREATE_NO_WINDOW: без консольного окна у службы и GUI
            cmd.creation_flags(0x0800_0000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
            // рабочий каталог — корень: не каталог данных сервера (относительные пути процесса
            // не ведут в данные), `/` есть всегда и не доступен на запись службе
            cmd.current_dir("/");
            let hook =
                zam_index::child::pre_exec_hook(std::process::id() as _, Some(s.memory), true);
            // SAFETY: замыкание исполняется между fork и exec и вызывает только
            // async-signal-safe функции (prctl, getppid, setrlimit, _exit).
            unsafe {
                cmd.pre_exec(hook);
            }
        }
        // один процесс в job: рабочий процесс не запустит дочерних. Назначение — после запуска,
        // но до первого запроса: недоверенный файл приходит, когда предел уже действует
        #[cfg(windows)]
        let job = zam_index::child::KillJob::new(Some(s.memory), Some(1))?;
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: {e}", s.program.display()))?;
        #[cfg(windows)]
        if let Err(e) = job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Job Object: {e}"));
        }
        let pid = child.id();
        let stdin = child.stdin.take().ok_or("нет stdin")?;
        let stdout = child.stdout.take().ok_or("нет stdout")?;
        let stderr = child.stderr.take().ok_or("нет stderr")?;
        // каналы на один элемент: ни запросы, ни кадры не копятся
        let (jobs_tx, jobs_rx) = mpsc::sync_channel(1);
        let (permit_tx, permit_rx) = mpsc::sync_channel(1);
        let (ev_tx, events) = mpsc::sync_channel(1);
        let oom = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(3);
        let max = s.response_max;
        let ev2 = ev_tx.clone();
        let spawn = |name: String, f: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name)
                .spawn(f)
                .map_err(|e| e.to_string())
        };
        threads.push(spawn(
            format!("doc-worker-in-{pid}"),
            Box::new(move || writer_loop(stdin, jobs_rx, permit_tx, ev_tx)),
        )?);
        threads.push(spawn(
            format!("doc-worker-out-{pid}"),
            Box::new(move || reader_loop(stdout, max, permit_rx, ev2)),
        )?);
        let oom2 = oom.clone();
        threads.push(spawn(
            format!("doc-worker-err-{pid}"),
            Box::new(move || forward_stderr(stderr, pid, &oom2)),
        )?);
        tracing::debug!(component = "doc-worker", pid, "рабочий процесс запущен");
        Ok(Worker {
            child,
            pid,
            jobs_tx: Some(jobs_tx),
            events: Some(events),
            threads,
            oom,
            jobs: 0,
            memory: s.memory,
            #[cfg(unix)]
            vm_peak: 0,
            #[cfg(windows)]
            job,
            killed: false,
            done: false,
        })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Отправляет запрос `id` и ждёт ответа с тем же номером до `deadline`.
    fn exchange(
        &mut self,
        id: u64,
        body: Arc<Vec<u8>>,
        deadline: Instant,
    ) -> Result<Response, Recv> {
        let Some(tx) = &self.jobs_tx else {
            return Err(Recv::Exited);
        };
        // канал на один запрос, предыдущий уже записан: отправка не ждёт
        if tx.try_send((id, body)).is_err() {
            return Err(Recv::Exited);
        }
        let Some(events) = &self.events else {
            return Err(Recv::Exited);
        };
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(Recv::Timeout);
            }
            let step = (deadline - now).min(Duration::from_millis(100));
            match events.recv_timeout(step) {
                Ok(Event::Frame(got, _)) if got != id => return Err(Recv::Stale(got)),
                Ok(Event::Frame(_, frame)) => {
                    return serde_json::from_slice(&frame)
                        .map_err(|e| Recv::Bad(format!("ответ: {e}")));
                }
                Ok(Event::BadFrame(e)) => return Err(Recv::Bad(e)),
                Ok(Event::WriteFailed(e)) => {
                    tracing::warn!(
                        component = "doc-worker",
                        pid = self.pid,
                        "запрос не отправлен: {e}"
                    );
                    return Err(Recv::Exited);
                }
                Ok(Event::Eof) | Err(RecvTimeoutError::Disconnected) => return Err(Recv::Exited),
                Err(RecvTimeoutError::Timeout) => {
                    // Linux: пик адресного пространства для признака OOM
                    #[cfg(unix)]
                    if let Some(p) = zam_index::child::vm_peak_bytes(self.pid) {
                        self.vm_peak = self.vm_peak.max(p);
                    }
                }
            }
        }
    }

    /// Ждёт выхода не дольше `limit`; не вышел — `None`.
    fn wait_for(&mut self, limit: Duration) -> Option<ExitStatus> {
        let t = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(st)) => return Some(st),
                Ok(None) if t.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
                _ => return None,
            }
        }
    }

    fn kill_now(&mut self) {
        self.killed = true;
        #[cfg(windows)]
        self.job.terminate(1);
        let _ = self.child.kill();
    }

    /// Закрывает каналы потоков: поток записи закрывает stdin, читатель и запись не ждут слота.
    fn close_channels(&mut self) {
        self.jobs_tx.take();
        self.events.take();
    }

    /// Закрывает процесс: `graceful` — закрыть stdin и дать выйти самому (2 с), затем убить.
    fn close(&mut self, graceful: bool) -> Option<ExitStatus> {
        if self.done {
            return None;
        }
        self.done = true;
        // сначала каналы (потоки не застрянут на отправке слоту), затем процесс, затем join
        self.close_channels();
        let mut st = if graceful {
            self.wait_for(Duration::from_secs(2))
        } else {
            None
        };
        if st.is_none() {
            self.kill_now();
            st = self.child.wait().ok();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        st
    }

    /// Штатно закрывает процесс (исчерпан `worker_jobs`, умер между заданиями).
    fn finish(mut self, graceful: bool) {
        let _ = self.close(graceful);
    }

    /// Убивает процесс (срок, неверный ответ, нарушение протокола).
    fn kill(mut self) {
        let _ = self.close(false);
    }

    /// Процесс закрыл stdout посреди задания: дожидается выхода и определяет причину.
    fn reap(mut self) -> ParseFailure {
        self.close_channels();
        let st = self.wait_for(Duration::from_secs(5));
        let st = st.or_else(|| self.close(false));
        // stderr дочитан до конца: признак OOM из последних строк учтён
        self.done = true;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        let oom = self.oom.load(Ordering::SeqCst) || self.oom_by_status(st);
        tracing::warn!(
            component = "doc-worker",
            pid = self.pid,
            status = ?st,
            oom,
            "рабочий процесс завершился посреди разбора"
        );
        if oom {
            ParseFailure::Oom
        } else {
            ParseFailure::Crash
        }
    }

    fn near_limit(&self, peak: u64) -> bool {
        peak >= self.memory / 100 * 95
    }

    #[cfg(windows)]
    fn oom_by_status(&self, st: Option<ExitStatus>) -> bool {
        // 0xE0000008 — исключение OOM pdfium (`FX_OutOfMemoryTerminate`)
        let code = st.and_then(|s| s.code()).map(|c| c as u32);
        code == Some(0xE000_0008)
            || self
                .job
                .peak_process_memory()
                .is_some_and(|p| self.near_limit(p))
    }

    #[cfg(unix)]
    fn oom_by_status(&self, st: Option<ExitStatus>) -> bool {
        use std::os::unix::process::ExitStatusExt as _;
        let sigkill = st.and_then(|s| s.signal()) == Some(libc::SIGKILL);
        self.near_limit(self.vm_peak) || (sigkill && !self.killed)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.close(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracted_kind_must_match_request() {
        let x = |kind| Extracted {
            kind,
            units: Vec::new(),
            warnings: Vec::new(),
            ocr_needed: false,
        };
        assert!(extracted_of(DocKind::Csv, Outcome::Extracted(x(DocKind::Csv))).is_ok());
        assert_eq!(
            extracted_of(DocKind::Csv, Outcome::Extracted(x(DocKind::Text))),
            Err(unexpected())
        );
        assert_eq!(
            extracted_of(DocKind::Csv, Outcome::Pong(None)),
            Err(unexpected())
        );
    }

    #[test]
    fn oom_lines() {
        assert!(oom_line("memory allocation of 16777216 bytes failed"));
        assert!(oom_line(
            "terminate called after throwing an instance of 'std::bad_alloc'"
        ));
        assert!(oom_line("Out of memory"));
        assert!(!oom_line(r#"{"level":"INFO","fields":{"message":"ok"}}"#));
    }

    #[test]
    fn reasons_and_pdfium_path() {
        assert_eq!(ParseFailure::Crash.reason(), "parse_crash");
        assert_eq!(ParseFailure::Timeout.reason(), "parse_timeout");
        assert_eq!(ParseFailure::Oom.reason(), "parse_oom");
        let mut cfg = DocsSection::default();
        let dir = Path::new("exe-dir");
        assert_eq!(resolve_pdfium(&cfg, dir), dir.join(pdfium_default_name()));
        cfg.pdfium_dylib = Some("lib/p.so".into());
        assert_eq!(resolve_pdfium(&cfg, dir), dir.join("lib/p.so"));
        let abs = std::env::current_dir().unwrap().join("p.dll");
        cfg.pdfium_dylib = Some(abs.clone());
        assert_eq!(resolve_pdfium(&cfg, dir), abs);
    }

    #[test]
    fn reasons_capped() {
        assert_eq!(cap_reason("кратко".into()), "кратко");
        let long = cap_reason("я".repeat(10_000));
        assert!(
            long.len() <= REASON_MAX && long.ends_with('…'),
            "{}",
            long.len()
        );
    }

    #[test]
    fn rate_limit_counts_dropped() {
        let mut r = RateLimit::new();
        let allowed = (0..LOG_BURST + 50).filter(|_| r.allow(1)).count();
        assert_eq!(allowed, LOG_BURST as usize);
        assert_eq!(r.dropped, 50);
    }
}
