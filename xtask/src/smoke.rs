//! Smoke-тест комплекта: распаковка, запуск сервера, проверка API, клиента и Qdrant.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::manifest::Manifest;

const TOKEN_PREFIX: &str = "ZeroAIMemoryToken=";
const WAIT: Duration = Duration::from_secs(10);
/// Предел для коротких вызовов (`zam status`, `qdrant --version`).
const CMD_TIMEOUT: Duration = Duration::from_secs(15);

/// Запускает команду и ждёт не дольше `timeout`; по истечении процесс убивается, это ошибка.
/// stdout/stderr читаются в потоках, чтобы переполнение канала не вешало дочерний процесс.
fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("запуск процесса")?;
    let mut out = child.stdout.take().context("stdout")?;
    let mut err = child.stderr.take().context("stderr")?;
    let t_out = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let t0 = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if t0.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("процесс не завершился за {} с, убит", timeout.as_secs_f32());
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

/// Имя архива без расширения.
fn archive_stem(archive: &Path) -> Result<String> {
    let n = archive
        .file_name()
        .and_then(|n| n.to_str())
        .context("некорректное имя архива")?;
    n.strip_suffix(".zip")
        .or_else(|| n.strip_suffix(".tar.gz"))
        .map(str::to_string)
        .with_context(|| format!("ожидается .zip или .tar.gz: {n}"))
}

/// Распаковывает архив в чистый `dir`; возвращает корневой каталог комплекта.
fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("очистка {}", dir.display()))?;
    }
    fs::create_dir_all(dir)?;
    let f = File::open(archive).with_context(|| format!("открытие {}", archive.display()))?;
    if archive.to_string_lossy().ends_with(".zip") {
        zip::ZipArchive::new(f)?.extract(dir)?;
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(f)).unpack(dir)?;
    }
    let mut roots = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    if roots.len() != 1 || !roots[0].path().is_dir() {
        bail!("в архиве ожидается ровно один корневой каталог");
    }
    Ok(roots.remove(0).path())
}

/// Свободный порт: bind на 127.0.0.1:0 и закрытие.
fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Дочерний zam-server; при ошибке теста гарантированно убивается.
struct Server {
    child: Child,
    log: Arc<Mutex<Vec<String>>>,
    token_rx: Receiver<String>,
}

impl Server {
    fn start(root: &Path, data: &Path, addr: SocketAddr) -> Result<Self> {
        let mut child =
            Command::new(root.join(crate::package::exe(host_platform(root), "zam-server")))
                .args(["serve", "--data-dir"])
                .arg(data)
                .args(["--bind", &addr.to_string()])
                .env("ZAM_ADMIN_PASSWORD", "smoke")
                .current_dir(root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .context("запуск zam-server")?;
        let stderr = child.stderr.take().context("stderr сервера")?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, token_rx) = mpsc::channel();
        let log2 = Arc::clone(&log);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(t) = line.strip_prefix(TOKEN_PREFIX) {
                    let _ = tx.send(t.trim().to_string());
                } else if let Ok(mut l) = log2.lock()
                    && l.len() < 200
                {
                    l.push(line);
                }
            }
        });
        Ok(Self {
            child,
            log,
            token_rx,
        })
    }

    fn log_tail(&self) -> String {
        self.log.lock().map(|l| l.join("\n")).unwrap_or_default()
    }

    /// Остановка: Unix — SIGTERM, затем kill через 10 с; Windows — TerminateProcess.
    fn stop(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(self.child.id().to_string())
                .status();
            let t0 = Instant::now();
            while self.child.try_wait()?.is_none() {
                if t0.elapsed() > WAIT {
                    self.child.kill()?;
                    self.child.wait()?;
                    bail!("сервер не завершился по SIGTERM за 10 с, убит принудительно");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        #[cfg(not(unix))]
        {
            // Ctrl+C дочернему процессу без консоли послать нельзя; жёсткое завершение.
            let _ = self.child.kill();
        }
        let st = self.child.wait()?;
        #[cfg(unix)]
        if !st.success() {
            bail!("сервер завершился с кодом {st} после SIGTERM");
        }
        let _ = st;
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// По наличию `.exe` определяем, какого вида комплект распакован.
fn host_platform(root: &Path) -> &'static str {
    if root.join("zam-server.exe").is_file() {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
}

fn wait_health(srv: &mut Server, base: &str) -> Result<()> {
    let a = agent();
    let t0 = Instant::now();
    loop {
        if let Some(st) = srv.child.try_wait()? {
            bail!(
                "сервер завершился до готовности ({st}); stderr:\n{}",
                srv.log_tail()
            );
        }
        if let Ok(r) = a.get(&format!("{base}/api/v1/health")).call()
            && r.status().as_u16() == 200
        {
            return Ok(());
        }
        if t0.elapsed() > WAIT {
            bail!(
                "/api/v1/health не ответил за 10 с; stderr:\n{}",
                srv.log_tail()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Вход администратора и выпуск токена `ro`; возвращает plaintext.
fn admin_issue_ro(base: &str) -> Result<String> {
    let a = agent();
    let mut r = a
        .post(&format!("{base}/api/v1/auth/login"))
        .content_type("application/json")
        .send(r#"{"login":"admin","password":"smoke"}"#)?;
    if r.status().as_u16() != 200 {
        bail!("вход администратора: HTTP {}", r.status());
    }
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| {
            c.starts_with("zam_session=")
                .then(|| c.split(';').next().unwrap().to_string())
        })
        .context("в ответе входа нет cookie zam_session")?;
    let body: Value = serde_json::from_str(&r.body_mut().read_to_string()?)?;
    let csrf = body["csrf"]
        .as_str()
        .context("в ответе входа нет csrf")?
        .to_string();

    let mut r = a
        .get(&format!("{base}/api/v1/auth/me"))
        .header("Cookie", &cookie)
        .call()?;
    if r.status().as_u16() != 200 {
        bail!("/auth/me: HTTP {}", r.status());
    }
    let me: Value = serde_json::from_str(&r.body_mut().read_to_string()?)?;
    let admin_id = me["user_id"]
        .as_str()
        .context("в /auth/me нет user_id")?
        .to_string();

    let mut r = a
        .post(&format!("{base}/api/v1/admin/users/{admin_id}/tokens"))
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", &csrf)
        .content_type("application/json")
        .send(r#"{"name":"smoke-ro","access":"ro"}"#)?;
    let status = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    if status != 201 {
        bail!("выпуск токена: HTTP {status}");
    }
    let v: Value = serde_json::from_str(&text)?;
    if v["info"]["access"] != "ro" {
        bail!("выпущенный токен не ro");
    }
    let plain = v["plaintext"].as_str().context("нет plaintext токена")?;
    if plain.is_empty() {
        bail!("пустой plaintext токена");
    }
    Ok(plain.to_string())
}

fn step<T>(n: u32, what: &str, r: Result<T>) -> Result<T> {
    r.map_err(|e| anyhow!("шаг {n} ({what}): {e:#}"))
}

/// Выполняет smoke-тест; при успехе печатает `SMOKE OK`.
pub fn run(archive: &Path, target: &Path) -> Result<()> {
    let name = archive_stem(archive)?;
    let dir = target.join("smoke").join(&name);
    let root = step(1, "распаковка", unpack(archive, &dir))?;
    let data = dir.join("data");
    let port = free_port()?;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let base = format!("http://{addr}");

    let mut srv = step(2, "запуск zam-server", Server::start(&root, &data, addr))?;
    step(3, "ожидание /api/v1/health", wait_health(&mut srv, &base))?;
    let token = step(
        4,
        "первый токен в stderr",
        srv.token_rx.recv_timeout(WAIT).map_err(|_| {
            anyhow!(
                "строка {TOKEN_PREFIX}… не найдена; stderr:\n{}",
                srv.log_tail()
            )
        }),
    )?;

    let zam = root.join(crate::package::exe(host_platform(&root), "zam"));
    let out = step(
        5,
        "zam status",
        output_with_timeout(
            Command::new(&zam)
                .args(["status", "--url", &base, "--token", &token])
                .current_dir(&dir),
            CMD_TIMEOUT,
        ),
    )?;
    if out.status.code() != Some(0) {
        bail!(
            "шаг 5 (zam status): код {:?}\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    step(6, "вход администратора и токен ro", admin_issue_ro(&base))?;

    let qv = Manifest::load(&root.join("deploy").join("qdrant.toml"))?.version;
    let qdir = root.join("qdrant").join(&qv);
    let qbin = qdir.join(crate::package::exe(host_platform(&root), "qdrant"));
    let out = step(
        7,
        "qdrant --version",
        output_with_timeout(
            Command::new(&qbin).arg("--version").current_dir(&qdir),
            CMD_TIMEOUT,
        ),
    )?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() || !text.contains(&qv) {
        bail!(
            "шаг 7 (qdrant --version): код {:?}, вывод: {text}",
            out.status.code()
        );
    }

    step(8, "остановка сервера", srv.stop())?;
    if srv.child.try_wait()?.is_none() {
        bail!("шаг 8: процесс zam-server ещё жив");
    }
    if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
        bail!("шаг 8: порт {port} всё ещё принимает соединения");
    }
    println!("SMOKE OK");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn echo() -> Command {
        let mut c = Command::new("cmd");
        c.args(["/C", "echo", "hi"]);
        c
    }
    #[cfg(not(windows))]
    fn echo() -> Command {
        let mut c = Command::new("echo");
        c.arg("hi");
        c
    }
    // Долгая команда без обёртки-оболочки: kill гарантированно закрывает её каналы.
    #[cfg(windows)]
    fn sleeper() -> Command {
        let mut c = Command::new("ping");
        c.args(["-n", "30", "127.0.0.1"]);
        c
    }
    #[cfg(not(windows))]
    fn sleeper() -> Command {
        let mut c = Command::new("sleep");
        c.arg("30");
        c
    }

    #[test]
    fn output_with_timeout_returns_output() {
        let o = output_with_timeout(&mut echo(), Duration::from_secs(10)).unwrap();
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stdout).contains("hi"));
    }

    #[test]
    fn output_with_timeout_kills_slow_command() {
        let t0 = Instant::now();
        let e = output_with_timeout(&mut sleeper(), Duration::from_millis(300)).unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(10));
        assert!(format!("{e:#}").contains("убит"), "{e:#}");
    }
}
