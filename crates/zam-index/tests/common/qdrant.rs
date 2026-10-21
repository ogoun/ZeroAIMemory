//! Тестовый запуск настоящего Qdrant 1.19.2 из `target/deps/<платформа>/qdrant/`.
//!
//! Каждый [`TestQdrant`] — отдельный процесс в свежем каталоге под `CARGO_TARGET_TMPDIR`
//! на свободных портах 127.0.0.1, со случайным API-ключом в `QDRANT__SERVICE__API_KEY`.
//! Процесс завершается в `Drop`. Боевой запуск — `zam_index::supervisor` (тесты в `supervisor.rs`);
//! здесь — облегчённый вариант без лока, pid-файла и перезапусков для тестов адаптера.

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use rand::Rng as _;

/// Корень репозитория.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Путь к бинарнику Qdrant (`cargo xtask fetch-deps`).
pub fn qdrant_bin() -> PathBuf {
    let (platform, exe) = if cfg!(windows) {
        ("windows-x64", "qdrant.exe")
    } else {
        ("linux-x64", "qdrant")
    };
    repo_root()
        .join("target")
        .join("deps")
        .join(platform)
        .join("qdrant")
        .join(exe)
}

/// Порты, уже выданные [`free_port`] в этом процессе тестов.
static ISSUED: Mutex<Option<HashSet<u16>>> = Mutex::new(None);

/// Свободный TCP-порт на 127.0.0.1, не выдававшийся раньше в этом процессе. Порт
/// освобождается сразу, и ОС может вернуть его параллельному тесту до запуска Qdrant
/// (флейк M3: os error 10048) — реестр не выдаёт один порт дважды. Гонку с чужими
/// процессами реестр не закрывает: её ловит повтор запуска в [`TestQdrant::start`].
pub fn free_port() -> u16 {
    let mut issued = ISSUED.lock().unwrap_or_else(|p| p.into_inner());
    let issued = issued.get_or_insert_with(HashSet::new);
    loop {
        let port = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("свободный порт");
        if issued.insert(port) {
            return port;
        }
    }
}

/// Попыток запуска Qdrant на новых портах, если адрес оказался занят.
const START_ATTEMPTS: u32 = 3;

/// Хвост журнала Qdrant говорит о занятом адресе (Windows — 10048, Linux — 98).
fn addr_in_use(dir: &Path) -> bool {
    let log = std::fs::read(dir.join("qdrant.log")).unwrap_or_default();
    let tail = String::from_utf8_lossy(&log[log.len().saturating_sub(8192)..]).to_lowercase();
    [
        "os error 10048",
        "os error 98",
        "address already in use",
        "addrinuse",
    ]
    .iter()
    .any(|m| tail.contains(m))
}

/// Процесс завершился при старте, потому что адрес занят: можно повторить на других портах.
struct AddrInUse;

/// Предел одновременно запущенных [`TestQdrant`] в процессе теста (по умолчанию 4,
/// переменная `ZAM_TEST_QDRANT_MAX`). Тесты одного бинарника идут параллельно, и каждый
/// поднимает свой Qdrant: на HDD десяток процессов, одновременно создающих коллекции
/// с payload-индексами (fsync), растягивает каждую запись `wait=true` до сотен
/// миллисекунд — тесты с пределами времени падают от взаимной нагрузки, а не от
/// ошибки. Лишние тесты ждут слота до запуска своего Qdrant.
fn max_running() -> usize {
    std::env::var("ZAM_TEST_QDRANT_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

static RUNNING: Mutex<usize> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

/// Занятый слот запуска; освобождается в `Drop` (после остановки процесса).
struct Slot;

impl Slot {
    fn take() -> Slot {
        let max = max_running();
        let mut n = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        while *n >= max {
            n = FREED.wait(n).unwrap_or_else(|p| p.into_inner());
        }
        *n += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut n = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        *n -= 1;
        FREED.notify_one();
    }
}

/// Запущенный Qdrant для одного теста.
pub struct TestQdrant {
    child: Child,
    pub dir: PathBuf,
    pub http_port: u16,
    pub grpc_port: u16,
    pub api_key: String,
    /// Последним полем: слот освобождается после остановки процесса в `Drop`.
    _slot: Slot,
}

static SEQ: AtomicU32 = AtomicU32::new(0);

impl TestQdrant {
    /// Запускает Qdrant и ждёт `GET /readyz` (до 60 с: данные проекта на HDD). Если процесс
    /// завершился из-за занятого адреса, запуск повторяется на новых портах (до 3 попыток).
    pub fn start(name: &str) -> TestQdrant {
        let bin = qdrant_bin();
        assert!(
            bin.is_file(),
            "нет бинарника Qdrant {}: выполните `cargo xtask fetch-deps`",
            bin.display()
        );
        let slot = Slot::take();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("zam-index")
            .join(format!("{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).expect("каталог теста");

        let api_key = format!("{:032x}", rand::rng().random::<u128>());
        let mut attempt = 1;
        loop {
            let http_port = free_port();
            let grpc_port = free_port();
            // Пути в конфиге — от рабочего каталога процесса (исследование 05 §6.1).
            let config = format!(
                "log_level: WARN\n\
                 telemetry_disabled: true\n\
                 storage:\n  storage_path: ./storage\n  snapshots_path: ./snapshots\n  temp_path: ./temp\n  on_disk_payload: true\n\
                 service:\n  host: 127.0.0.1\n  http_port: {http_port}\n  grpc_port: {grpc_port}\n  enable_cors: false\n  enable_static_content: false\n\
                 cluster:\n  enabled: false\n"
            );
            std::fs::write(dir.join("config").join("zam.yaml"), config).expect("конфиг Qdrant");
            let mut child = spawn(&bin, &dir, &api_key);
            match wait_ready(&mut child, &dir, http_port, grpc_port) {
                Ok(()) => {
                    return TestQdrant {
                        child,
                        dir,
                        http_port,
                        grpc_port,
                        api_key,
                        _slot: slot,
                    };
                }
                Err(AddrInUse) if attempt < START_ATTEMPTS => attempt += 1,
                Err(AddrInUse) => panic!(
                    "Qdrant не запустился за {START_ATTEMPTS} попытки: адрес занят, лог: {}",
                    dir.join("qdrant.log").display()
                ),
            }
        }
    }

    /// Останавливает процесс (данные и порты сохраняются для [`TestQdrant::restart`]).
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Запускает процесс заново в том же каталоге, на тех же портах и с тем же ключом.
    pub fn restart(&mut self) {
        self.stop();
        self.child = spawn(&qdrant_bin(), &self.dir, &self.api_key);
        if wait_ready(&mut self.child, &self.dir, self.http_port, self.grpc_port).is_err() {
            panic!(
                "Qdrant не перезапустился: адрес занят, лог: {}",
                self.dir.join("qdrant.log").display()
            );
        }
    }

    /// pid процесса.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// gRPC-адрес для `QdrantIndex::connect`.
    pub fn grpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.grpc_port)
    }
}

/// Ждёт `GET /readyz` и открытый gRPC-порт (до 60 с: данные проекта на HDD). Процесс,
/// завершившийся при старте из-за занятого адреса, — [`AddrInUse`]; иначе — паника.
fn wait_ready(
    child: &mut Child,
    dir: &Path,
    http_port: u16,
    grpc_port: u16,
) -> Result<(), AddrInUse> {
    let limit = Duration::from_secs(60);
    let started = Instant::now();
    loop {
        if let Ok(Some(st)) = child.try_wait() {
            if addr_in_use(dir) {
                return Err(AddrInUse);
            }
            panic!(
                "Qdrant завершился при старте ({st}), лог: {}",
                dir.join("qdrant.log").display()
            );
        }
        // gRPC-порт Qdrant открывает отдельно от HTTP и бывает, что позже `/readyz`
        // (флейк на Linux CI: ConnectionRefused первого `health()`); супервизор по той же
        // причине ждёт gRPC health.
        if readyz(http_port) && port_open(grpc_port) {
            return Ok(());
        }
        if started.elapsed() >= limit {
            let _ = child.kill();
            panic!(
                "Qdrant не готов за {limit:?}, лог: {}",
                dir.join("qdrant.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Процесс Qdrant с конфигом `<dir>/config/zam.yaml`; журнал дописывается в `qdrant.log`.
fn spawn(bin: &Path, dir: &Path, api_key: &str) -> Child {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("qdrant.log"))
        .expect("лог Qdrant");
    Command::new(bin)
        .arg("--config-path")
        .arg(dir.join("config").join("zam.yaml"))
        .arg("--disable-telemetry")
        .current_dir(dir)
        .env("QDRANT__SERVICE__API_KEY", api_key)
        .env("QDRANT_INIT_FILE_PATH", dir.join(".qdrant-initialized"))
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("лог"))
        .stderr(log)
        .spawn()
        .expect("запуск Qdrant")
}

/// На порту 127.0.0.1 есть слушатель.
fn port_open(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// `GET /readyz` по HTTP/1.0 без внешних зависимостей; ключ не нужен.
fn readyz(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if s.write_all(b"GET /readyz HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    buf.starts_with(b"HTTP/1.0 200") || buf.starts_with(b"HTTP/1.1 200")
}

impl Drop for TestQdrant {
    fn drop(&mut self) {
        // Тестовый процесс — жёсткое завершение; штатную остановку делает супервизор (задача 6).
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Данные успешного теста не нужны; после паники каталог с логом остаётся для разбора.
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
