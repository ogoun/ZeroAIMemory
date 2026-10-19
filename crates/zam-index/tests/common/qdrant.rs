//! Тестовый запуск настоящего Qdrant 1.19.2 из `target/deps/<платформа>/qdrant/`.
//!
//! Каждый [`TestQdrant`] — отдельный процесс в свежем каталоге под `CARGO_TARGET_TMPDIR`
//! на свободных портах 127.0.0.1, со случайным API-ключом в `QDRANT__SERVICE__API_KEY`.
//! Процесс завершается в `Drop`. Боевой супервизор (Job Object / PDEATHSIG, pid-файл) — задача 6.

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Свободный TCP-порт на 127.0.0.1 (порт освобождается сразу, гонка допустима для тестов).
fn free_port() -> u16 {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("свободный порт")
}

/// Запущенный Qdrant для одного теста.
pub struct TestQdrant {
    child: Child,
    pub dir: PathBuf,
    pub http_port: u16,
    pub grpc_port: u16,
    pub api_key: String,
}

static SEQ: AtomicU32 = AtomicU32::new(0);

impl TestQdrant {
    /// Запускает Qdrant и ждёт `GET /readyz` (до 60 с: данные проекта на HDD).
    pub fn start(name: &str) -> TestQdrant {
        let bin = qdrant_bin();
        assert!(
            bin.is_file(),
            "нет бинарника Qdrant {}: выполните `cargo xtask fetch-deps`",
            bin.display()
        );
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("zam-index")
            .join(format!("{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).expect("каталог теста");

        let http_port = free_port();
        let grpc_port = free_port();
        let api_key = format!("{:032x}", rand::rng().random::<u128>());
        // Пути в конфиге — от рабочего каталога процесса (исследование 05 §6.1).
        let config = format!(
            "log_level: WARN\n\
             telemetry_disabled: true\n\
             storage:\n  storage_path: ./storage\n  snapshots_path: ./snapshots\n  temp_path: ./temp\n  on_disk_payload: true\n\
             service:\n  host: 127.0.0.1\n  http_port: {http_port}\n  grpc_port: {grpc_port}\n  enable_cors: false\n  enable_static_content: false\n\
             cluster:\n  enabled: false\n"
        );
        let config_path = dir.join("config").join("zam.yaml");
        std::fs::write(&config_path, config).expect("конфиг Qdrant");
        let log = std::fs::File::create(dir.join("qdrant.log")).expect("лог Qdrant");
        let child = Command::new(&bin)
            .arg("--config-path")
            .arg(&config_path)
            .arg("--disable-telemetry")
            .current_dir(&dir)
            .env("QDRANT__SERVICE__API_KEY", &api_key)
            .env("QDRANT_INIT_FILE_PATH", dir.join(".qdrant-initialized"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("лог"))
            .stderr(log)
            .spawn()
            .expect("запуск Qdrant");
        let mut q = TestQdrant {
            child,
            dir,
            http_port,
            grpc_port,
            api_key,
        };
        q.wait_ready(Duration::from_secs(60));
        q
    }

    /// gRPC-адрес для `QdrantIndex::connect`.
    pub fn grpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.grpc_port)
    }

    fn wait_ready(&mut self, limit: Duration) {
        let started = Instant::now();
        loop {
            if let Ok(Some(st)) = self.child.try_wait() {
                panic!(
                    "Qdrant завершился при старте ({st}), лог: {}",
                    self.dir.join("qdrant.log").display()
                );
            }
            if readyz(self.http_port) {
                return;
            }
            assert!(
                started.elapsed() < limit,
                "Qdrant не готов за {limit:?}, лог: {}",
                self.dir.join("qdrant.log").display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
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
