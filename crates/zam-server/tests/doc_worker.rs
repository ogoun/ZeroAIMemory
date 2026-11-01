//! Рабочий процесс `zam-server doc-worker` и пул `DocPool` (M5, задача 3; Review Focus 4):
//! разбор через процесс, падение, срок, лимит памяти, перезапуск, смерть вместе с сервером,
//! недоступный pdfium.
//!
//! Рабочий процесс — настоящий бинарник `zam-server` этого пакета (тестовые запросы — фича
//! `test-hooks`). Тесты PDF требуют pdfium из `cargo xtask fetch-deps`, без него пропускаются.

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use zam_docs::test_pdf::{Page, build, pdfium_path};
use zam_docs::{DocError, DocKind, Locator};
use zam_server::config::DocsSection;
use zam_server::docs::pool::TestOp;
use zam_server::docs::{DocPool, ParseFailure};

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zam-server"))
}

/// Конфигурация с pdfium из `target/deps` (если есть) или с заведомо отсутствующим путём.
fn cfg() -> DocsSection {
    DocsSection {
        workers: 1,
        pdfium_dylib: Some(pdfium_path(env!("CARGO_MANIFEST_DIR")).unwrap_or_else(missing)),
        ..DocsSection::default()
    }
}

fn missing() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("no-such-dir/pdfium-missing.dll")
}

fn have_pdfium() -> bool {
    let ok = pdfium_path(env!("CARGO_MANIFEST_DIR")).is_some();
    if !ok {
        eprintln!("pdfium не найден (cargo xtask fetch-deps) — тест PDF пропущен");
    }
    ok
}

/// Ждёт исчезновения процесса не дольше `limit`.
fn gone(pid: u32, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if !zam_index::supervisor::pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !zam_index::supervisor::pid_alive(pid)
}

#[tokio::test]
async fn worker_roundtrip_extract() {
    let pool = DocPool::with_program(&cfg(), exe());
    // не-PDF разбирается и без pdfium
    let x = pool
        .extract(
            DocKind::Text,
            Bytes::from_static("строка текста".as_bytes()),
        )
        .await
        .unwrap();
    assert_eq!(x.units[0].text, "строка текста");
    if !have_pdfium() {
        return;
    }
    assert_eq!(pool.ping().await.unwrap(), None);
    let pdf = build(&[
        Page::default().line(72.0, 700.0, "Hello from worker"),
        Page::default().line(72.0, 700.0, "Second page").image(),
    ]);
    let x = pool
        .extract(DocKind::Pdf, Bytes::from(pdf.clone()))
        .await
        .unwrap();
    assert_eq!(x.kind, DocKind::Pdf);
    let pages: Vec<(Locator, &str)> = x
        .units
        .iter()
        .map(|u| (u.locator.clone(), u.text.as_str()))
        .collect();
    assert_eq!(
        pages,
        [
            (Locator::Page(1), "Hello from worker"),
            (Locator::Page(2), "Second page")
        ]
    );
    let png = pool.thumb(Bytes::from(pdf), 2, 128).await.unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    // повреждённый PDF — ошибка документа, процесс жив
    let e = pool
        .extract(DocKind::Pdf, Bytes::from_static(b"%PDF-1.4 garbage"))
        .await
        .unwrap_err();
    assert!(
        matches!(e, ParseFailure::Doc(DocError::Malformed(_))),
        "{e:?}"
    );
    assert_eq!(e.reason(), "parse_error");
    assert_eq!(pool.spawned(), 1, "один процесс на все задания");
    assert_eq!(pool.pdfium_error(), None);
}

#[tokio::test]
async fn worker_crash_is_parse_crash_and_restarts() {
    let pool = DocPool::with_program(&cfg(), exe());
    pool.test_op(TestOp::Sleep(1)).await.unwrap();
    let first = pool.worker_pids();
    assert_eq!(first.len(), 1);
    let e = pool.test_op(TestOp::Crash).await.unwrap_err();
    assert_eq!(e, ParseFailure::Crash);
    assert_eq!(e.reason(), "parse_crash");
    assert!(gone(first[0], Duration::from_secs(5)));
    // следующий запрос — новый процесс
    pool.test_op(TestOp::Sleep(1)).await.unwrap();
    assert_eq!(pool.spawned(), 2);
    assert_ne!(pool.worker_pids(), first);
}

#[tokio::test]
async fn worker_timeout_killed() {
    let pool = DocPool::with_program(
        &DocsSection {
            // запуск debug-процесса под нагрузкой параллельных тестов — до секунды и больше
            timeout_ms: 3000,
            ..cfg()
        },
        exe(),
    );
    pool.test_op(TestOp::Sleep(1)).await.unwrap();
    let pid = pool.worker_pids()[0];
    let t = Instant::now();
    let e = pool.test_op(TestOp::Sleep(30_000)).await.unwrap_err();
    assert_eq!(e, ParseFailure::Timeout);
    assert_eq!(e.reason(), "parse_timeout");
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert!(gone(pid, Duration::from_secs(5)), "процесс пережил срок");
    pool.test_op(TestOp::Sleep(1)).await.unwrap();
    assert_eq!(pool.spawned(), 2);
}

#[tokio::test]
async fn worker_memory_limit_oom() {
    let pool = DocPool::with_program(
        &DocsSection {
            // Linux: RLIMIT_AS включает отображение самого бинарника (debug — около 100 МиБ)
            worker_memory_mb: 512,
            ..cfg()
        },
        exe(),
    );
    // в пределах лимита — работает
    pool.test_op(TestOp::Alloc(64)).await.unwrap();
    let e = pool.test_op(TestOp::Alloc(4096)).await.unwrap_err();
    assert_eq!(e, ParseFailure::Oom);
    assert_eq!(e.reason(), "parse_oom");
    pool.test_op(TestOp::Alloc(16)).await.unwrap();
    assert_eq!(pool.spawned(), 2);
}

#[tokio::test]
async fn worker_recycled_after_worker_jobs() {
    let pool = DocPool::with_program(
        &DocsSection {
            worker_jobs: 2,
            ..cfg()
        },
        exe(),
    );
    for _ in 0..5 {
        pool.test_op(TestOp::Sleep(1)).await.unwrap();
    }
    assert_eq!(pool.spawned(), 3, "по 2 задания на процесс");
}

#[tokio::test]
async fn missing_pdfium_unsupported() {
    let pool = DocPool::with_program(
        &DocsSection {
            pdfium_dylib: Some(missing()),
            ..cfg()
        },
        exe(),
    );
    let pdf = build(&[Page::default().line(72.0, 700.0, "x")]);
    let e = pool
        .extract(DocKind::Pdf, Bytes::from(pdf))
        .await
        .unwrap_err();
    assert_eq!(e, ParseFailure::Doc(DocError::Unsupported));
    let why = pool.pdfium_error().expect("причина для /index/status");
    assert!(why.contains("pdfium не найден"), "{why}");
    assert_eq!(pool.ping().await.unwrap(), Some(why));
    // прочие виды — работают
    let x = pool
        .extract(DocKind::Csv, Bytes::from_static(b"a,b\n1,2\n"))
        .await
        .unwrap();
    assert!(!x.units.is_empty());
}

#[tokio::test]
async fn too_large_rejected_before_worker() {
    let pool = DocPool::with_program(
        &DocsSection {
            max_bytes: 10,
            ..cfg()
        },
        exe(),
    );
    let e = pool
        .extract(DocKind::Text, Bytes::from_static(b"01234567890"))
        .await
        .unwrap_err();
    assert_eq!(e, ParseFailure::Doc(DocError::TooLarge));
    assert_eq!(pool.spawned(), 0);
}

/// Переменная, по которой копия тестового бинарника играет роль сервера.
const PROBE_ENV: &str = "DOC_WORKER_PROBE";
const PID_LINE: &str = "DOC_WORKER_PID=";

/// «Сервер» для [`worker_dies_with_server`]: пул, один процесс, pid в stdout, ожидание.
#[test]
#[ignore = "запускается только из worker_dies_with_server"]
fn probe_server_holding_worker() {
    if std::env::var_os(PROBE_ENV).is_none() {
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = std::sync::Arc::new(DocPool::with_program(&cfg(), exe()));
        pool.test_op(TestOp::Sleep(1)).await.unwrap();
        // процесс занят долгим заданием: сам по EOF stdin он не выйдет (ревью, I-3)
        let busy = pool.clone();
        tokio::spawn(async move { busy.test_op(TestOp::Sleep(120_000)).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        println!("{PID_LINE}{}", pool.worker_pids()[0]);
        tokio::time::sleep(Duration::from_secs(120)).await;
    });
}

#[test]
fn worker_dies_with_server() {
    let mut probe = Command::new(std::env::current_exe().unwrap())
        .args([
            "probe_server_holding_worker",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = BufReader::new(probe.stdout.take().unwrap());
    let mut pid = None;
    for line in out.lines() {
        let line = line.unwrap();
        // libtest может напечатать перед строкой имя теста: «test … ... DOC_WORKER_PID=…»
        if let Some((_, p)) = line.trim().split_once(PID_LINE) {
            pid = Some(p.trim().parse::<u32>().unwrap());
            break;
        }
    }
    let pid = pid.expect("pid рабочего процесса от «сервера»");
    assert!(zam_index::supervisor::pid_alive(pid));
    // жёсткое завершение «сервера»: Windows — TerminateProcess, Linux — SIGKILL
    let t = Instant::now();
    probe.kill().unwrap();
    probe.wait().unwrap();
    // занятый процесс (Sleep 120 с) исчезает только по Job Object / PDEATHSIG — быстро
    assert!(
        gone(pid, Duration::from_secs(10)),
        "рабочий процесс {pid} пережил сервер"
    );
    eprintln!("рабочий процесс исчез через {:?}", t.elapsed());
}

/// Резидентная память этого процесса, МиБ.
fn rss_mib() -> u64 {
    zam_index::supervisor::rss_bytes(std::process::id()).unwrap_or(0) / (1024 * 1024)
}

#[tokio::test]
async fn worker_unsolicited_frames_killed_not_buffered() {
    // ревью задачи 3 (I-1): процесс после ответа пишет 10 лишних кадров по 64 МиБ; пул не читает
    // stdout без запроса (память не растёт), лишний кадр не выдаётся следующему запросу
    let pool = DocPool::with_program(&cfg(), exe());
    pool.test_op(TestOp::Sleep(1)).await.unwrap();
    let before = rss_mib();
    pool.test_op(TestOp::Spam { frames: 10, mb: 64 })
        .await
        .unwrap();
    let first = pool.worker_pids();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let idle = rss_mib();
    assert!(
        idle < before + 200,
        "память простаивающего пула: {before} → {idle} МиБ"
    );
    // следующий запрос: лишний кадр — нарушение протокола, процесс убит, ответ — свежий
    let p = pool.ping().await.unwrap();
    assert!(p.as_ref().is_none_or(|s| !s.starts_with("stale")), "{p:?}");
    assert!(p.as_ref().is_none_or(|s| s.len() <= 4096));
    assert_eq!(pool.spawned(), 2, "процесс с лишними кадрами заменён");
    assert!(gone(first[0], Duration::from_secs(5)));
    assert!(pool.pdfium_error().is_none_or(|s| !s.starts_with("stale")));
    let after = rss_mib();
    eprintln!("RSS: до {before}, простой {idle}, после {after} МиБ");
    assert!(after < before + 200, "{before} → {after} МиБ");
}

#[tokio::test]
async fn worker_long_reason_capped() {
    let pool = DocPool::with_program(&cfg(), exe());
    let e = pool.test_op(TestOp::LongError(1024)).await.unwrap_err();
    assert_eq!(e, ParseFailure::Doc(DocError::Unsupported));
    let why = pool.pdfium_error().unwrap();
    assert!(
        why.len() <= zam_server::docs::pool::REASON_MAX,
        "{}",
        why.len()
    );
}
