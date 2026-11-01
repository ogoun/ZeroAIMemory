//! Пул рабочих процессов `zam-server doc-worker` (M5 §2).
//!
//! - `docs.workers` долгоживущих потоков-«слотов»; каждый запускает свой процесс по требованию,
//!   отдаёт ему задания по одному и перезапускает после `docs.worker_jobs` заданий или падения.
//!   Процесс запускается из потока слота: на Linux `PDEATHSIG` привязан к потоку-родителю.
//! - Привязка к серверу и лимит памяти: Windows — свой Job Object на процесс
//!   (`KILL_ON_JOB_CLOSE`, `JOB_OBJECT_LIMIT_PROCESS_MEMORY`); Linux — `PDEATHSIG` (`SIGKILL`),
//!   `RLIMIT_AS` и своя группа процессов. Секреты сервера в окружение процесса не попадают.
//! - Срок `docs.timeout_ms`: по истечении процесс убивается — [`ParseFailure::Timeout`]; внутри
//!   процесса разбор получает срок на 10 % короче и отдаёт частичный результат сам.
//! - Падение — [`ParseFailure::Crash`]; нехватка памяти — [`ParseFailure::Oom`] по признакам:
//!   сообщение аллокатора Rust или C++ в stderr; Windows — код `0xE0000008` (OOM pdfium) или пик
//!   памяти процесса у лимита; Linux — пик `VmPeak` у лимита или `SIGKILL` не от пула (OOM killer).
//! - stderr процесса (JSON-журнал) пересылается в журнал сервера с `component = "doc-worker"`.

use std::borrow::Cow;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use base64::Engine as _;
use zam_docs::{DocError, DocKind, Extracted, Limits};

use super::protocol::{
    Request, Response, WireLimits, read_frame, request_body, request_frame_max, response_frame_max,
    write_frame,
};
use crate::config::DocsSection;

/// Сбой разбора в рабочем процессе; [`ParseFailure::reason`] — причина для `index_state`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseFailure {
    /// Процесс упал (или не запустился).
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
    Ping,
    #[cfg(feature = "test-hooks")]
    Test(TestOp),
}

/// Результат задания.
enum Outcome {
    Extracted(Extracted),
    Thumb(Vec<u8>),
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
}

/// Пул рабочих процессов разбора документов.
pub struct DocPool {
    tx: Sender<Job>,
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
        let shared = Arc::new(Shared {
            pids: Mutex::new(vec![0; cfg.workers]),
            ..Shared::default()
        });
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for slot in 0..cfg.workers {
            let (rx, settings, shared) = (rx.clone(), settings.clone(), shared.clone());
            // поток живёт, пока жив пул: на Linux его смерть убивает процесс слота (PDEATHSIG)
            let r = std::thread::Builder::new()
                .name(format!("doc-worker-{slot}"))
                .spawn(move || slot_loop(slot, &rx, &settings, &shared));
            if let Err(e) = r {
                tracing::error!(component = "doc-worker", "поток слота не запущен: {e}");
            }
        }
        DocPool { tx, shared }
    }

    async fn submit(&self, op: Op) -> Result<Outcome, ParseFailure> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Job { op, reply })
            .map_err(|_| ParseFailure::Crash)?;
        rx.await.unwrap_or(Err(ParseFailure::Crash))
    }

    /// Извлекает структуру документа в рабочем процессе.
    pub async fn extract(&self, kind: DocKind, bytes: Bytes) -> Result<Extracted, ParseFailure> {
        match self.submit(Op::Extract { kind, bytes }).await? {
            Outcome::Extracted(x) => Ok(x),
            _ => Err(ParseFailure::Doc(DocError::Internal(
                "неожиданный ответ".into(),
            ))),
        }
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
            _ => Err(ParseFailure::Doc(DocError::Internal(
                "неожиданный ответ".into(),
            ))),
        }
    }

    /// Проверка процесса и pdfium: `Ok(None)` — pdfium загружен, `Ok(Some(причина))` — нет.
    pub async fn ping(&self) -> Result<Option<String>, ParseFailure> {
        match self.submit(Op::Ping).await? {
            Outcome::Pong(p) => Ok(p),
            _ => Err(ParseFailure::Doc(DocError::Internal(
                "неожиданный ответ".into(),
            ))),
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
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn slot_loop(slot: usize, rx: &Mutex<Receiver<Job>>, s: &Settings, shared: &Shared) {
    let mut worker: Option<Worker> = None;
    loop {
        let job = {
            let guard = lock(rx);
            match guard.recv() {
                Ok(j) => j,
                Err(_) => break,
            }
        };
        let r = run_job(slot, &mut worker, job.op, s, shared);
        let _ = job.reply.send(r);
    }
    drop(worker);
    lock(&shared.pids)[slot] = 0;
}

/// Тело кадра запроса и срок разбора внутри процесса.
fn body_of(op: &Op, s: &Settings) -> Vec<u8> {
    let soft_ms = (s.timeout.as_millis() as u64).saturating_mul(9) / 10;
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
        Op::Ping => request_body(&Request::Ping, None),
        #[cfg(feature = "test-hooks")]
        Op::Test(t) => request_body(
            &match *t {
                TestOp::Crash => Request::Crash,
                TestOp::Sleep(ms) => Request::Sleep { ms },
                TestOp::Alloc(mb) => Request::Alloc { mb },
            },
            None,
        ),
    }
}

fn run_job(
    slot: usize,
    worker: &mut Option<Worker>,
    op: Op,
    s: &Settings,
    shared: &Shared,
) -> Result<Outcome, ParseFailure> {
    if let Op::Extract { bytes, .. } | Op::Thumb { bytes, .. } = &op
        && bytes.len() as u64 > s.limits.max_bytes
    {
        return Err(ParseFailure::Doc(DocError::TooLarge));
    }
    let body = body_of(&op, s);
    drop(op);
    // процесс, умерший между заданиями, заменяется до отправки
    if worker.as_mut().is_some_and(|w| !w.alive())
        && let Some(w) = worker.take()
    {
        w.finish(true);
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
    let deadline = Instant::now() + s.timeout;
    let got = match w.send(&body) {
        Ok(()) => {
            drop(body);
            w.recv(deadline)
        }
        Err(e) => {
            tracing::warn!(
                component = "doc-worker",
                pid = w.pid,
                "запрос не отправлен: {e}"
            );
            Err(Recv::Exited)
        }
    };
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
            lock(&shared.pids)[slot] = 0;
            return Err(ParseFailure::Timeout);
        }
        Err(Recv::Exited) => {
            let w = worker.take().expect("процесс есть");
            let f = w.reap();
            lock(&shared.pids)[slot] = 0;
            return Err(f);
        }
        Err(Recv::Bad(msg)) => {
            let w = worker.take().expect("процесс есть");
            tracing::warn!(
                component = "doc-worker",
                pid = w.pid,
                "неверный ответ: {msg}"
            );
            w.kill();
            lock(&shared.pids)[slot] = 0;
            return Err(ParseFailure::Doc(DocError::Internal(msg)));
        }
    };
    if worker.as_ref().is_some_and(|w| w.jobs >= s.worker_jobs) {
        if let Some(w) = worker.take() {
            w.finish(true);
        }
        lock(&shared.pids)[slot] = 0;
    }
    result
}

/// Ответ процесса → результат задания; учитывает состояние pdfium.
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
        Response::Pong { pdfium } => {
            lock(&shared.pdfium_error).clone_from(&pdfium);
            Ok(Outcome::Pong(pdfium))
        }
        Response::Error(e) => {
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
    /// Процесс закрыл stdout (завершился или упал).
    Exited,
    /// Неверный кадр или JSON.
    Bad(String),
}

/// Признак нехватки памяти в строке stderr: аллокатор Rust (`memory allocation of N bytes
/// failed`) или C++ (`std::bad_alloc`, «out of memory»).
fn oom_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    (l.contains("memory allocation of") && l.contains("failed"))
        || l.contains("bad_alloc")
        || l.contains("out of memory")
}

/// Пересылает stderr процесса в журнал сервера построчно (строка — не длиннее 16 КиБ).
fn forward_stderr(r: impl std::io::Read, pid: u32, oom: &AtomicBool) {
    const LINE_MAX: u64 = 16 * 1024;
    let mut reader = BufReader::new(r);
    let mut buf = Vec::with_capacity(512);
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
        if oom_line(line) {
            oom.store(true, Ordering::SeqCst);
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
}

/// Один рабочий процесс.
struct Worker {
    child: Child,
    pid: u32,
    stdin: Option<ChildStdin>,
    frames: Receiver<Result<Vec<u8>, String>>,
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
            let hook = zam_index::child::pre_exec_hook(std::process::id() as _, Some(s.memory));
            // SAFETY: замыкание исполняется между fork и exec и вызывает только
            // async-signal-safe функции (prctl, getppid, setrlimit, _exit).
            unsafe {
                cmd.pre_exec(hook);
            }
        }
        #[cfg(windows)]
        let job = zam_index::child::KillJob::new(Some(s.memory))?;
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
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("нет stdout")?;
        let stderr = child.stderr.take().ok_or("нет stderr")?;
        let (ftx, frames) = mpsc::channel();
        let max = s.response_max;
        let oom = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(2);
        let out = std::thread::Builder::new()
            .name(format!("doc-worker-out-{pid}"))
            .spawn(move || {
                let mut r = BufReader::with_capacity(64 * 1024, stdout);
                loop {
                    match read_frame(&mut r, max) {
                        Ok(Some(f)) => {
                            if ftx.send(Ok(f)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Err(e) => {
                            let _ = ftx.send(Err(e.to_string()));
                            break;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        threads.push(out);
        let oom2 = oom.clone();
        let err = std::thread::Builder::new()
            .name(format!("doc-worker-err-{pid}"))
            .spawn(move || forward_stderr(stderr, pid, &oom2))
            .map_err(|e| e.to_string())?;
        threads.push(err);
        tracing::debug!(component = "doc-worker", pid, "рабочий процесс запущен");
        Ok(Worker {
            child,
            pid,
            stdin,
            frames,
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

    fn send(&mut self, body: &[u8]) -> std::io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        write_frame(stdin, body)
    }

    #[cfg(unix)]
    fn sample_memory(&mut self) {
        if let Some(p) = zam_index::child::vm_peak_bytes(self.pid) {
            self.vm_peak = self.vm_peak.max(p);
        }
    }

    #[cfg(windows)]
    fn sample_memory(&mut self) {}

    fn recv(&mut self, deadline: Instant) -> Result<Response, Recv> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(Recv::Timeout);
            }
            let step = (deadline - now).min(Duration::from_millis(100));
            match self.frames.recv_timeout(step) {
                Ok(Ok(frame)) => {
                    return serde_json::from_slice(&frame)
                        .map_err(|e| Recv::Bad(format!("ответ: {e}")));
                }
                Ok(Err(e)) => return Err(Recv::Bad(e)),
                Err(RecvTimeoutError::Timeout) => self.sample_memory(),
                Err(RecvTimeoutError::Disconnected) => return Err(Recv::Exited),
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

    /// Закрывает процесс: `graceful` — закрыть stdin и дать выйти самому (2 с), затем убить.
    fn close(&mut self, graceful: bool) -> Option<ExitStatus> {
        if self.done {
            return None;
        }
        self.done = true;
        self.stdin.take();
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

    /// Убивает процесс (срок, неверный ответ).
    fn kill(mut self) {
        let _ = self.close(false);
    }

    /// Процесс закрыл stdout посреди задания: дожидается выхода и определяет причину.
    fn reap(mut self) -> ParseFailure {
        self.stdin.take();
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
        let sigkill = st.and_then(|s| s.signal()) == Some(SIGKILL);
        self.near_limit(self.vm_peak) || (sigkill && !self.killed)
    }
}

/// Номер `SIGKILL` (Linux, POSIX).
#[cfg(unix)]
const SIGKILL: i32 = 9;

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.close(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
