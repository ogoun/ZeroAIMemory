//! Smoke-тест комплекта: распаковка, запуск сервера, проверка API, клиента и Qdrant,
//! векторный поиск на тестовой модели (M2), устройство модели, завершение Qdrant вместе с
//! сервером; для Windows — статический CRT наших exe и app-local MSVC runtime библиотек.
//! `smoke-cuda` — проверка архива `zam-cuda`.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use zam_fetch::{MODEL_LICENSE_FILE, MODEL_SPEC_FILE};

use crate::manifest::{CudaManifest, Manifest, ModelsManifest, PLATFORMS};

const TOKEN_PREFIX: &str = "ZeroAIMemoryToken=";
const WAIT: Duration = Duration::from_secs(10);
/// Предел готовности сервера при первой загрузке только что распакованного `zam-cuda`.
const CUDA_FIRST_WAIT: Duration = Duration::from_secs(120);
/// Предел для коротких вызовов (`zam status`, `qdrant --version`).
const CMD_TIMEOUT: Duration = Duration::from_secs(15);

/// Запускает команду и ждёт не дольше `timeout`; по истечении процесс убивается, это ошибка.
/// stdout/stderr читаются в потоках, чтобы переполнение канала не вешало дочерний процесс.
fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output> {
    output_with_stdin(cmd, None, timeout)
}

/// То же, со входом: `input` пишется в stdin в отдельном потоке и канал закрывается.
fn output_with_stdin(cmd: &mut Command, input: Option<&[u8]>, timeout: Duration) -> Result<Output> {
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("запуск процесса")?;
    if let Some(input) = input {
        let mut w = child.stdin.take().context("stdin")?;
        let data = input.to_vec();
        thread::spawn(move || {
            let _ = w.write_all(&data);
        });
    }
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

/// Имена переменных `ZAM_*` (без учёта регистра) из `vars`, кроме `keep`.
fn zam_vars_except<I: IntoIterator<Item = OsString>>(vars: I, keep: &[&str]) -> Vec<OsString> {
    vars.into_iter()
        .filter(|k| {
            let up = k.to_string_lossy().to_ascii_uppercase();
            up.starts_with("ZAM_") && !keep.contains(&up.as_str())
        })
        .collect()
}

/// Убирает из окружения дочернего процесса все `ZAM_*` текущего, кроме `keep`:
/// сборочные переменные CI (`ZAM_GIT_SHA` и т. п.) и настройки разработчика не должны влиять на проверку.
fn scrub_zam_env<'a>(cmd: &'a mut Command, keep: &[&str]) -> &'a mut Command {
    for k in zam_vars_except(std::env::vars_os().map(|(k, _)| k), keep) {
        cmd.env_remove(k);
    }
    cmd
}

/// Свободный порт: bind на 127.0.0.1:0 и закрытие.
pub(crate) fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Дочерний zam-server; при ошибке теста гарантированно убивается. Общий со smoke
/// `cargo xtask eval` (сервер из target/release на каталоге в target/eval).
pub(crate) struct Server {
    pub(crate) child: Child,
    log: Arc<Mutex<VecDeque<String>>>,
    pub(crate) token_rx: Receiver<String>,
}

/// Сколько последних строк stderr сервера показывать при ошибке.
const LOG_TAIL: usize = 80;

/// Строка журнала, полезная при разборе ошибки smoke: без подробностей ORT и раздачи WebUI.
fn interesting(line: &str) -> bool {
    !line.contains("\"target\":\"ort::logging\"") && !line.contains("\"target\":\"memory_serve\"")
}

impl Server {
    /// Запускает `zam-server` из каталога `root` (комплект или target/release); stderr
    /// целиком (кроме строки с токеном) пишется в `log_file`.
    pub(crate) fn start(root: &Path, data: &Path, env: &SmokeEnv, log_file: &Path) -> Result<Self> {
        let mut cmd =
            Command::new(root.join(crate::package::exe(host_platform(root), "zam-server")));
        let mut child = scrub_zam_env(&mut cmd, &["ZAM_ADMIN_PASSWORD"])
            .args(["serve", "--data-dir"])
            .arg(data)
            .args(["--bind", &env.addr.to_string()])
            .env("ZAM_ADMIN_PASSWORD", "smoke")
            .envs(env.vars.iter().map(|(k, v)| (k, v)))
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("запуск zam-server")?;
        let stderr = child.stderr.take().context("stderr сервера")?;
        let mut file =
            File::create(log_file).with_context(|| format!("создание {}", log_file.display()))?;
        let log = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, token_rx) = mpsc::channel();
        let log2 = Arc::clone(&log);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(t) = line.strip_prefix(TOKEN_PREFIX) {
                    let _ = tx.send(t.trim().to_string());
                    continue;
                }
                let _ = writeln!(file, "{line}");
                if interesting(&line)
                    && let Ok(mut l) = log2.lock()
                {
                    if l.len() == LOG_TAIL {
                        l.pop_front();
                    }
                    l.push_back(line);
                }
            }
        });
        Ok(Self {
            child,
            log,
            token_rx,
        })
    }

    /// Последние строки stderr (без журнала ORT и раздачи WebUI).
    pub(crate) fn log_tail(&self) -> String {
        self.log
            .lock()
            .map(|l| l.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    /// Остановка: Unix — SIGTERM, затем kill через 10 с; Windows — TerminateProcess.
    pub(crate) fn stop(&mut self) -> Result<()> {
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
    agent_with(Duration::from_secs(10))
}

/// HTTP-клиент без прокси, коды ошибок не превращаются в `Err`, общий предел `timeout`.
pub(crate) fn agent_with(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn wait_health(srv: &mut Server, base: &str) -> Result<()> {
    wait_health_for(srv, base, WAIT)
}

/// Ждёт `/api/v1/health` не дольше `wait` (сервер отвечает после загрузки модели).
pub(crate) fn wait_health_for(srv: &mut Server, base: &str, wait: Duration) -> Result<()> {
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
        if t0.elapsed() > wait {
            bail!(
                "/api/v1/health не ответил за {} с; stderr:\n{}",
                wait.as_secs(),
                srv.log_tail()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Сессия администратора WebUI: cookie `zam_session` и CSRF-токен.
pub(crate) struct AdminSession {
    pub(crate) cookie: String,
    pub(crate) csrf: String,
}

/// Вход администратора (`admin` / пароль smoke из `ZAM_ADMIN_PASSWORD`).
pub(crate) fn admin_login(base: &str) -> Result<AdminSession> {
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
    Ok(AdminSession { cookie, csrf })
}

/// Вход администратора и выпуск токена `ro`; возвращает plaintext.
fn admin_issue_ro(base: &str) -> Result<String> {
    let a = agent();
    let AdminSession { cookie, csrf } = admin_login(base)?;
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

/// Критерий M0: `zam status` без флагов находит URL и токен в `CLAUDE.md` выше cwd.
/// Окружение без `ZAM_*`, домашний каталог — пустой временный (без ~/.zeroaimemory). Токен не печатается.
fn status_from_claude_md(zam: &Path, dir: &Path, base: &str, token: &str) -> Result<()> {
    let proj = dir.join("proj");
    let sub = proj.join("sub");
    let home = dir.join("home");
    fs::create_dir_all(&sub)?;
    fs::create_dir_all(&home)?;
    let claude_md = proj.join("CLAUDE.md");
    fs::write(
        &claude_md,
        format!(
            "# smoke\n<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl={base}\nZeroAIMemoryToken={token}\n<!-- zeroaimemory:end -->\n"
        ),
    )?;
    let mut cmd = Command::new(zam);
    scrub_zam_env(&mut cmd, &[])
        .args(["status", "--json"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(&sub);
    let out = output_with_timeout(&mut cmd, CMD_TIMEOUT)?;
    // В выводе только маска токена.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if text.contains(token) {
        bail!("вывод zam status содержит токен целиком");
    }
    if out.status.code() != Some(0) {
        bail!("код {:?}\n{text}", out.status.code());
    }
    let v: Value = serde_json::from_slice(&out.stdout).context("разбор JSON zam status")?;
    let expect = format!("файл {}", claude_md.display());
    for key in ["url_source", "token_source"] {
        if v[key].as_str() != Some(expect.as_str()) {
            bail!("{key} = {}, ожидалось «{expect}»", v[key]);
        }
    }
    Ok(())
}

/// Текст заметки smoke-теста.
const SMOKE_NOTE: &[u8] = b"# Hello\nsmoke note\n";

/// `zam` из комплекта с чистым окружением: только адрес и токен в `ZAM_URL`/`ZAM_TOKEN`.
fn zam_cmd(zam: &Path, dir: &Path, base: &str, token: &str) -> Command {
    let mut cmd = Command::new(zam);
    scrub_zam_env(&mut cmd, &[])
        .env("ZAM_URL", base)
        .env("ZAM_TOKEN", token)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .current_dir(dir);
    cmd
}

/// `zam <args>` с проверкой: код 0, токен в выводе не появляется целиком.
fn zam_ok(
    zam: &Path,
    home: &Path,
    base: &str,
    token: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<Output> {
    let out = output_with_stdin(
        zam_cmd(zam, home, base, token).args(args),
        input,
        CMD_TIMEOUT,
    )?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if text.contains(token) {
        bail!("вывод zam {} содержит токен целиком", args.join(" "));
    }
    if out.status.code() != Some(0) {
        bail!(
            "zam {}: код {:?}\n{text}",
            args.join(" "),
            out.status.code()
        );
    }
    Ok(out)
}

/// Критерий M1: `zam write` из stdin → `zam search` находит путь → `zam read` возвращает текст.
fn note_roundtrip(zam: &Path, dir: &Path, base: &str, token: &str) -> Result<()> {
    let home = dir.join("notes-home");
    fs::create_dir_all(&home)?;
    let run = |args: &[&str], input: Option<&[u8]>| zam_ok(zam, &home, base, token, args, input);
    run(&["write", "knowledge:smoke/hello"], Some(SMOKE_NOTE))?;
    let found = run(&["search", "hello"], None)?;
    if !String::from_utf8_lossy(&found.stdout).contains("smoke/hello") {
        bail!(
            "zam search hello не нашёл smoke/hello:\n{}",
            String::from_utf8_lossy(&found.stdout)
        );
    }
    let read = run(&["read", "knowledge:smoke/hello"], None)?;
    if read.stdout != SMOKE_NOTE {
        bail!(
            "zam read вернул другой текст: {:?}",
            String::from_utf8_lossy(&read.stdout)
        );
    }
    Ok(())
}

/// В комплекте есть тексты лицензий пакетов бандла WebUI (минимум svelte).
fn check_web_notices(root: &Path) -> Result<()> {
    let p = root.join("LICENSES").join("THIRD-PARTY-WEB-NOTICES.txt");
    let text = fs::read_to_string(&p).with_context(|| format!("чтение {}", p.display()))?;
    if !text.lines().any(|l| l.starts_with("svelte@")) {
        bail!("в {} нет лицензии svelte", p.display());
    }
    Ok(())
}

/// Windows: exe комплекта не должны импортировать динамический CRT (VCRUNTIME/MSVCP/UCRT),
/// иначе на чистой системе без VC Redistributable они не запустятся.
fn check_static_crt(root: &Path) -> Result<()> {
    let dumpbin = crate::package::find_dumpbin()?;
    for exe in ["zam-server.exe", "zam.exe"] {
        let out = output_with_timeout(
            Command::new(&dumpbin)
                .arg("/dependents")
                .arg(root.join(exe)),
            CMD_TIMEOUT,
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!(
                "dumpbin /dependents {exe}: код {:?}\n{text}",
                out.status.code()
            );
        }
        let bad = crate::package::forbidden_imports(&text);
        if !bad.is_empty() {
            bail!("{exe} импортирует динамический CRT: {}", bad.join(", "));
        }
    }
    Ok(())
}

/// Нумерация шагов smoke по порядку выполнения: ошибка — «шаг N (что): причина».
struct Steps(u32);

impl Steps {
    fn run<T>(&mut self, what: &str, r: Result<T>) -> Result<T> {
        self.0 += 1;
        let n = self.0;
        r.map_err(|e| anyhow!("шаг {n} ({what}): {e:#}"))
    }
}

/// Команда завершилась с кодом 0, иначе ошибка с выводом.
fn ensure_success(out: Output) -> Result<Output> {
    if out.status.code() != Some(0) {
        bail!(
            "код {:?}\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out)
}

/// `qdrant --version` из комплекта отвечает закреплённой версией.
fn qdrant_version_ok(qdir: &Path, qbin: &Path, qv: &str) -> Result<()> {
    let out = output_with_timeout(
        Command::new(qbin).arg("--version").current_dir(qdir),
        CMD_TIMEOUT,
    )?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() || !text.contains(qv) {
        bail!("код {:?}, вывод: {text}", out.status.code());
    }
    Ok(())
}

/// Сервер остановлен: процесса нет, порт закрыт.
fn server_stopped(srv: &mut Server, addr: SocketAddr) -> Result<()> {
    srv.stop()?;
    if srv.child.try_wait()?.is_none() {
        bail!("процесс zam-server ещё жив");
    }
    if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
        bail!("порт {} всё ещё принимает соединения", addr.port());
    }
    Ok(())
}

/// Выполняет smoke-тест комплекта; при успехе печатает `SMOKE OK`. `repo` — корень
/// репозитория (тестовая модель — из `target/models/`, распаковка — в `target/smoke/`).
pub fn run(
    archive: &Path,
    cuda_archive: Option<&Path>,
    expect_device: Option<DeviceKind>,
    repo: &Path,
) -> Result<()> {
    let mut s = Steps(0);
    let name = archive_stem(archive)?;
    let dir = repo.join("target").join("smoke").join(&name);
    let root = s.run("распаковка", unpack(archive, &dir))?;
    s.run(
        "тексты лицензий WebUI в LICENSES/",
        check_web_notices(&root),
    )?;
    let qv = Manifest::load(&root.join("deploy").join("qdrant.toml"))?.version;
    let qdir = root.join("qdrant").join(&qv);
    let platform = host_platform(&root);
    if platform == "windows-x64" {
        s.run(
            "статический CRT zam-server.exe и zam.exe (dumpbin /dependents)",
            check_static_crt(&root),
        )?;
        s.run(
            "app-local MSVC runtime для ONNX Runtime и Qdrant (dumpbin /dependents)",
            check_app_local_crt(&root, &qdir),
        )?;
    }
    let qbin = qdir.join(crate::package::exe(platform, "qdrant"));
    s.run("qdrant --version", qdrant_version_ok(&qdir, &qbin, &qv))?;
    let data = dir.join("data");
    let env = SmokeEnv::new()?;
    let addr = env.addr;
    let base = format!("http://{addr}");
    s.run(
        "тестовая модель в каталоге данных",
        install_test_model(repo, &data),
    )?;
    if let Some(ca) = cuda_archive {
        s.run(
            "распаковка zam-cuda в комплект и cuda fetch без загрузки",
            cuda_from_archive(&env, ca, &root, &data, repo),
        )?;
    }

    let server_log = dir.join("zam-server.stderr.log");
    let mut srv = s.run(
        "запуск zam-server",
        Server::start(&root, &data, &env, &server_log),
    )?;
    if cuda_archive.is_some() {
        // сервер отвечает на /health после загрузки модели, а с CUDA — после загрузки
        // библиотек NVIDIA (~1,4 ГБ, cuDNN). Задача 17 M2 (Windows, HDD, VRAM почти
        // занята сторонним процессом): два прогона подряд не уложились в 10 с, следующие —
        // 3,9 и 4,8 с; причина не установлена (кандидаты — проверка антивирусом свежих DLL,
        // конкуренция за GPU). Предел шире, время печатается.
        let t0 = Instant::now();
        s.run(
            "ожидание /api/v1/health (первая загрузка CUDA)",
            wait_health_for(&mut srv, &base, CUDA_FIRST_WAIT),
        )?;
        println!(
            "первый старт с zam-cuda до /health: {:.1} с",
            t0.elapsed().as_secs_f64()
        );
    } else {
        s.run("ожидание /api/v1/health", wait_health(&mut srv, &base))?;
    }
    let token = s.run(
        "первый токен в stderr",
        srv.token_rx.recv_timeout(WAIT).map_err(|_| {
            anyhow!(
                "строка {TOKEN_PREFIX}… не найдена; stderr:\n{}",
                srv.log_tail()
            )
        }),
    )?;

    let zam = root.join(crate::package::exe(platform, "zam"));
    s.run(
        "zam status",
        output_with_timeout(
            Command::new(&zam)
                .args(["status", "--url", &base, "--token", &token])
                .current_dir(&dir),
            CMD_TIMEOUT,
        )
        .and_then(ensure_success),
    )?;
    s.run(
        "zam status из CLAUDE.md без флагов",
        status_from_claude_md(&zam, &dir, &base, &token),
    )?;
    s.run(
        "zam write → search → read заметки",
        note_roundtrip(&zam, &dir, &base, &token),
    )?;
    s.run("вход администратора и токен ro", admin_issue_ro(&base))?;
    let vr = s.run(
        "векторный поиск: 5 заметок, индексация, перефраз в top-3",
        vector_search(&zam, &dir, &base, &token, expect_device).map_err(|e| {
            anyhow!(
                "{e:#}\nstderr сервера (целиком — {}):\n{}",
                server_log.display(),
                srv.log_tail()
            )
        }),
    )?;
    s.run(
        "Qdrant запущен сервером (pid-файл, qdrant status --json)",
        qdrant_alive(&env, &root, &data).and_then(|(alive, j)| {
            if alive {
                Ok(())
            } else {
                Err(anyhow!("pid-файл не указывает на живой Qdrant: {j}"))
            }
        }),
    )?;
    s.run("остановка сервера", server_stopped(&mut srv, addr))?;
    s.run(
        "процесс Qdrant завершён вместе с сервером",
        qdrant_gone(&env, &root, &data),
    )?;
    let reason = if vr.reason.is_empty() {
        String::new()
    } else {
        format!(" ({})", vr.reason)
    };
    println!(
        "векторный поиск: {VECTOR_TARGET} — №{} в top-3, каналы {}, индексация {:.1} с; \
         модель {}, device={}{reason}",
        vr.rank,
        vr.sources,
        vr.waited.as_secs_f32(),
        vr.model,
        vr.device
    );
    println!("шагов: {}", s.0);
    println!("SMOKE OK");
    Ok(())
}

// ---- M2: модель, векторный поиск, устройство, Qdrant, CUDA ----

/// Устройство модели в `/index/status` (`device.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DeviceKind {
    Cpu,
    Cuda,
}

impl DeviceKind {
    fn as_str(self) -> &'static str {
        match self {
            DeviceKind::Cpu => "cpu",
            DeviceKind::Cuda => "cuda",
        }
    }
}

/// Тестовая модель векторного шага (`role = "test"`, `cargo xtask fetch-models --test`):
/// маленькая и англоязычная, поэтому заметки и запрос — на английском.
const SMOKE_MODEL: &str = "minilm-l6-int8";

/// Предел ожидания векторной индексации пяти заметок.
const INDEX_WAIT: Duration = Duration::from_secs(60);

/// Заметки векторного шага: (путь в Knowledge, текст).
const VECTOR_NOTES: [(&str, &str); 5] = [
    (
        "smoke/vec/bread",
        "# Sourdough bread\n\nMix flour, water and a starter, leave the dough to rise \
         overnight, then bake it in a hot oven until the crust is golden.\n",
    ),
    (
        "smoke/vec/cluster",
        "# Container orchestration\n\nKubernetes schedules pods across cluster nodes and \
         restarts crashed containers automatically.\n",
    ),
    (
        "smoke/vec/marathon",
        "# Marathon training\n\nRunners build endurance with long weekly runs, tempo \
         sessions and a careful taper before race day.\n",
    ),
    (
        "smoke/vec/photosynthesis",
        "# Photosynthesis\n\nPlants convert sunlight, carbon dioxide and water into glucose \
         and oxygen inside their chloroplasts.\n",
    ),
    (
        "smoke/vec/taxes",
        "# Personal income tax\n\nFile the annual return before the deadline and keep \
         receipts for deductible expenses.\n",
    ),
];

/// Перефраз целевой заметки без общих подстрок из 3 букв (тест
/// `vector_query_shares_no_trigram_with_target`): ни FTS по словам, ни trigram его не
/// найдут — только векторный канал; smoke проверяет, что у цели нет каналов `fts` и `trigram`.
const VECTOR_QUERY: &str = "how vegetation makes food using solar energy";
const VECTOR_TARGET: &str = "smoke/vec/photosynthesis";

/// Порты и окружение сервера smoke: модель — тестовая, Qdrant — на свободных портах
/// (не мешает Qdrant разработчика и параллельным прогонам).
pub(crate) struct SmokeEnv {
    pub(crate) addr: SocketAddr,
    qdrant_http: u16,
    pub(crate) vars: Vec<(String, String)>,
}

impl SmokeEnv {
    fn new() -> Result<Self> {
        Self::for_model(SMOKE_MODEL)
    }

    /// Свободные порты сервера и Qdrant, модель эмбеддингов `model` (`ml.embed_model`).
    pub(crate) fn for_model(model: &str) -> Result<Self> {
        let addr: SocketAddr = ([127, 0, 0, 1], free_port()?).into();
        let qdrant_http = free_port()?;
        let qdrant_grpc = free_port()?;
        Ok(Self {
            addr,
            qdrant_http,
            vars: vec![
                ("ZAM_ML__EMBED_MODEL".into(), model.into()),
                ("ZAM_QDRANT__HTTP_PORT".into(), qdrant_http.to_string()),
                ("ZAM_QDRANT__GRPC_PORT".into(), qdrant_grpc.to_string()),
            ],
        })
    }

    /// `zam-server <args> --data-dir <data>` с окружением smoke.
    fn server_cmd(&self, root: &Path, data: &Path, args: &[&str]) -> Command {
        let mut cmd =
            Command::new(root.join(crate::package::exe(host_platform(root), "zam-server")));
        scrub_zam_env(&mut cmd, &[])
            .args(args)
            .arg("--data-dir")
            .arg(data)
            .envs(self.vars.iter().map(|(k, v)| (k, v)))
            .current_dir(root);
        cmd
    }
}

/// Копирует тестовую модель из `target/models/` в `<data>/models/`: только файлы манифеста,
/// `LICENSE` и `model.toml` (без `*.part` и прочего).
fn install_test_model(repo: &Path, data: &Path) -> Result<()> {
    let manifest = ModelsManifest::load(&repo.join("deploy").join("models.toml"))?;
    let m = manifest.get(SMOKE_MODEL)?;
    let src = repo.join("target").join("models").join(SMOKE_MODEL);
    let dst = data.join("models").join(SMOKE_MODEL);
    fs::create_dir_all(&dst)?;
    let names = m
        .files
        .iter()
        .map(|f| f.path.as_str())
        .chain([MODEL_LICENSE_FILE, MODEL_SPEC_FILE]);
    for n in names {
        let s = src.join(n);
        if !s.is_file() {
            bail!(
                "нет {}: скачайте тестовую модель (cargo xtask fetch-models --test)",
                s.display()
            );
        }
        fs::copy(&s, dst.join(n)).with_context(|| format!("копирование {}", s.display()))?;
    }
    Ok(())
}

/// Запрос к API с токеном; возвращает статус и JSON ответа.
pub(crate) fn api(
    base: &str,
    token: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    api_timeout(base, token, path, body, Duration::from_secs(10))
}

/// То же с пределом времени запроса (реранк на CPU, загрузка реранкера).
pub(crate) fn api_timeout(
    base: &str,
    token: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<(u16, Value)> {
    let a = agent_with(timeout);
    let url = format!("{base}{path}");
    let auth = format!("Bearer {token}");
    let mut r = match body {
        Some(b) => a
            .post(&url)
            .header("Authorization", &auth)
            .content_type("application/json")
            .send(b.to_string())?,
        None => a.get(&url).header("Authorization", &auth).call()?,
    };
    let status = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    let v = serde_json::from_str(&text)
        .with_context(|| format!("{path}: HTTP {status}, ответ не JSON"))?;
    Ok((status, v))
}

/// Пути и каналы выдачи (без текстов заметок).
fn describe_items(v: &Value) -> String {
    v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| format!("{} {}", i["path"], i["sources"]))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// Итог векторного шага для печати.
struct VectorReport {
    rank: usize,
    sources: String,
    device: String,
    reason: String,
    model: String,
    waited: Duration,
}

/// Критерий M2: 5 заметок → индексация (≤ 60 с) → перефраз находит нужную в top-3 через
/// векторный канал без деградации; устройство и модель — по `/index/status`.
fn vector_search(
    zam: &Path,
    dir: &Path,
    base: &str,
    token: &str,
    expect: Option<DeviceKind>,
) -> Result<VectorReport> {
    let home = dir.join("vector-home");
    fs::create_dir_all(&home)?;
    for (path, text) in VECTOR_NOTES {
        zam_ok(
            zam,
            &home,
            base,
            token,
            &["write", &format!("knowledge:{path}")],
            Some(text.as_bytes()),
        )?;
    }
    let body = serde_json::json!({ "query": VECTOR_QUERY, "mode": "notes", "top_k": 3 });
    let t0 = Instant::now();
    let found = loop {
        let (status, v) = api(base, token, "/api/v1/search", Some(&body))?;
        if status != 200 {
            bail!("POST /api/v1/search: HTTP {status}");
        }
        if v["degraded"] == false && v["pending_count"] == 0 {
            break v;
        }
        if t0.elapsed() > INDEX_WAIT {
            let (_, st) = api(base, token, "/api/v1/index/status", None)?;
            bail!(
                "за {} с заметки не проиндексированы или поиск деградирован: degraded = {}, \
                 degraded_reason = {}, pending_count = {}; /index/status: vector = {}, \
                 device = {}, model = {}, qdrant = {}, queue = {}",
                INDEX_WAIT.as_secs(),
                v["degraded"],
                v["degraded_reason"],
                v["pending_count"],
                st["vector"],
                st["device"],
                st["model"],
                st["qdrant"],
                st["queue"]
            );
        }
        thread::sleep(Duration::from_millis(500));
    };
    let waited = t0.elapsed();
    let items = found["items"]
        .as_array()
        .context("в ответе поиска нет items")?;
    let (rank, item) = items
        .iter()
        .take(3)
        .enumerate()
        .find(|(_, i)| {
            i["path"]
                .as_str()
                .is_some_and(|p| p.ends_with(VECTOR_TARGET))
        })
        .with_context(|| {
            format!(
                "перефраз «{VECTOR_QUERY}» не нашёл {VECTOR_TARGET} в top-3: {}",
                describe_items(&found)
            )
        })?;
    let sources = item["sources"].to_string();
    let channels: Vec<&str> = item["sources"]
        .as_array()
        .map(|s| s.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if channels != ["vector"] {
        bail!(
            "{VECTOR_TARGET} должен найтись только векторным каналом (у перефраза нет общих \
             с заметкой подстрок для fts и trigram): sources = {sources}"
        );
    }
    let (status, st) = api(base, token, "/api/v1/index/status", None)?;
    if status != 200 {
        bail!("GET /api/v1/index/status: HTTP {status}");
    }
    if st["vector"]["available"] != true {
        bail!("/index/status: векторный канал выключен: {}", st["vector"]);
    }
    let model = st["model"]["id"].as_str().unwrap_or_default().to_string();
    if !model.starts_with(SMOKE_MODEL) {
        bail!("/index/status: модель {model}, ожидалась {SMOKE_MODEL}");
    }
    let device = st["device"]["kind"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let reason = st["device"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if let Some(want) = expect
        && device != want.as_str()
    {
        bail!(
            "устройство {device}, ожидалось {} (причина: {reason})",
            want.as_str()
        );
    }
    if device.is_empty() {
        bail!("/index/status: устройство не определено: {}", st["device"]);
    }
    Ok(VectorReport {
        rank: rank + 1,
        sources,
        device,
        reason,
        model,
        waited,
    })
}

/// `zam-server qdrant status --json` по каталогу данных: pid-файл указывает на живой процесс
/// Qdrant (`pid_alive`). Возвращает признак и JSON целиком (для текста ошибки).
fn qdrant_alive(env: &SmokeEnv, root: &Path, data: &Path) -> Result<(bool, Value)> {
    let out = output_with_timeout(
        &mut env.server_cmd(root, data, &["qdrant", "status", "--json"]),
        CMD_TIMEOUT,
    )?;
    let out = ensure_success(out).context("zam-server qdrant status --json")?;
    let v: Value =
        serde_json::from_slice(&out.stdout).context("разбор JSON qdrant status --json")?;
    let alive = v["pid_alive"]
        .as_bool()
        .with_context(|| format!("в qdrant status --json нет pid_alive: {v}"))?;
    Ok((alive, v))
}

/// После остановки сервера процесса Qdrant нет (pid-файл не указывает на живой процесс,
/// порт HTTP закрыт); ждём не дольше 10 с.
fn qdrant_gone(env: &SmokeEnv, root: &Path, data: &Path) -> Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], env.qdrant_http).into();
    let t0 = Instant::now();
    loop {
        let (alive, v) = qdrant_alive(env, root, data)?;
        let port = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok();
        if !alive && !port {
            return Ok(());
        }
        if t0.elapsed() > WAIT {
            bail!(
                "Qdrant работает через 10 с после остановки сервера (порт {} {}): {v}",
                env.qdrant_http,
                if port { "открыт" } else { "закрыт" }
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Распаковывает архив в существующий каталог (без очистки).
fn extract_into(archive: &Path, dir: &Path) -> Result<()> {
    let f = File::open(archive).with_context(|| format!("открытие {}", archive.display()))?;
    if archive.to_string_lossy().ends_with(".zip") {
        zip::ZipArchive::new(f)?.extract(dir)?;
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(f)).unpack(dir)?;
    }
    Ok(())
}

/// Пользователь распаковал `zam-cuda` в каталог `zam-server`: `cuda fetch --json` видит все
/// архивы NVIDIA установленными по штампам (`installed` = число архивов платформы в
/// `deploy/cuda.toml`, `downloaded` = 0) и ничего не скачивает.
fn cuda_from_archive(
    env: &SmokeEnv,
    archive: &Path,
    root: &Path,
    data: &Path,
    repo: &Path,
) -> Result<()> {
    extract_into(archive, root)?;
    let m = CudaManifest::load(&repo.join("deploy").join("cuda.toml"))?;
    let libs = m.libs(host_platform(root)).count() as u64;
    let out = output_with_timeout(
        &mut env.server_cmd(root, data, &["cuda", "fetch", "--json"]),
        Duration::from_secs(120),
    )?;
    let out = ensure_success(out).context("zam-server cuda fetch --json")?;
    let v: Value = serde_json::from_slice(&out.stdout).context("разбор JSON cuda fetch --json")?;
    if v["downloaded"].as_u64() != Some(0) || v["installed"].as_u64() != Some(libs) {
        bail!("после распаковки zam-cuda ожидалось downloaded = 0, installed = {libs}: {v}");
    }
    Ok(())
}

/// Windows: импорты MSVC runtime у DLL комплекта и у `qdrant.exe` лежат рядом (app-local):
/// на чистой системе без VC++ Redistributable ONNX Runtime и Qdrant загрузятся.
fn check_app_local_crt(root: &Path, qdrant_dir: &Path) -> Result<()> {
    let dumpbin = crate::package::find_dumpbin()?;
    let files_in = |dir: &Path| -> Result<Vec<String>> {
        Ok(fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect())
    };
    let mut targets: Vec<PathBuf> = files_in(root)?
        .into_iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".dll"))
        .map(|n| root.join(n))
        .collect();
    targets.push(qdrant_dir.join("qdrant.exe"));
    let mut bad = Vec::new();
    for t in &targets {
        let out = output_with_timeout(
            Command::new(&dumpbin).arg("/dependents").arg(t),
            CMD_TIMEOUT,
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!(
                "dumpbin /dependents {}: код {:?}",
                t.display(),
                out.status.code()
            );
        }
        let dir = t.parent().context("каталог файла")?;
        let missing = crate::package::missing_app_local(&text, &files_in(dir)?);
        if !missing.is_empty() {
            bad.push(format!(
                "{}: нет рядом {}",
                t.strip_prefix(root).unwrap_or(t).display(),
                missing.join(", ")
            ));
        }
    }
    if !bad.is_empty() {
        bail!(
            "библиотеки MSVC runtime не найдутся на чистой Windows:\n  {}",
            bad.join("\n  ")
        );
    }
    Ok(())
}

/// Проверка архива `zam-cuda`: размер меньше 2 ГиБ, корень `cuda/`, `cuda.toml` совпадает с
/// `deploy/cuda.toml`, штамп каждого архива NVIDIA совпадает с манифестом и файлы на месте
/// (`cuda fetch` после распаковки ничего не скачает), библиотеки — PE (Windows) или ELF
/// (Linux), есть лицензии NVIDIA. При успехе печатает `SMOKE-CUDA OK`.
pub fn run_cuda(archive: &Path, repo: &Path) -> Result<()> {
    let name = archive_stem(archive)?;
    let platform = *PLATFORMS
        .iter()
        .find(|p| name.starts_with("zam-cuda-") && name.ends_with(*p))
        .with_context(|| format!("ожидается архив zam-cuda-<версия>-<платформа>: {name}"))?;
    crate::package::check_release_size(archive, crate::package::MAX_RELEASE_FILE)?;
    let dir = repo.join("target").join("smoke").join(&name);
    let root = Steps(0).run("распаковка", unpack(archive, &dir))?;
    if root.file_name().and_then(|n| n.to_str()) != Some(crate::bundles::CUDA_DIR) {
        bail!(
            "корневой каталог архива — {}, ожидался cuda/",
            root.display()
        );
    }
    let manifest_path = repo.join("deploy").join("cuda.toml");
    let m = CudaManifest::load(&manifest_path)?;
    m.validate()?;
    let inner = fs::read(root.join("cuda.toml")).context("в архиве нет cuda/cuda.toml")?;
    if inner != fs::read(&manifest_path)? {
        bail!("cuda/cuda.toml в архиве отличается от deploy/cuda.toml");
    }
    for l in m.libs(platform) {
        if !zam_fetch::is_installed(&l.entry(), &root, &l.component)? {
            bail!(
                "{} {}: штамп не совпал с манифестом или нет файлов — cuda fetch стал бы \
                 скачивать",
                l.component,
                l.version
            );
        }
    }
    let names = m.dest_names(platform)?;
    let mut total = 0u64;
    for n in &names {
        let p = root.join(n);
        let mut head = [0u8; 4];
        File::open(&p)?.read_exact(&mut head)?;
        let ok = if platform == "linux-x64" {
            head == *b"\x7fELF"
        } else {
            head[..2] == *b"MZ"
        };
        if !ok {
            bail!("{n}: не библиотека {platform}");
        }
        total += fs::metadata(&p)?.len();
    }
    let lic = root.join("LICENSES");
    let has_lic = fs::read_dir(&lic)
        .map(|d| d.filter_map(|e| e.ok()).any(|e| e.path().is_file()))
        .unwrap_or(false);
    if !has_lic {
        bail!("в архиве нет cuda/LICENSES с лицензиями NVIDIA");
    }
    println!(
        "{name}: {} библиотек ({:.0} МБ распакованными), архив {:.0} МБ; штампы совпадают с \
         deploy/cuda.toml",
        names.len(),
        total as f64 / 1e6,
        fs::metadata(archive)?.len() as f64 / 1e6
    );
    println!("SMOKE-CUDA OK");
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

    /// Подстроки из 3 символов в нижнем регистре (как у токенизатора trigram FTS5), по
    /// каждому слову из букв и цифр.
    fn trigrams(text: &str) -> std::collections::HashSet<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .flat_map(|w| {
                let ch: Vec<char> = w.chars().collect();
                ch.windows(3)
                    .map(|t| t.iter().collect::<String>())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn vector_query_shares_no_trigram_with_target() {
        let (_, text) = VECTOR_NOTES
            .iter()
            .find(|(p, _)| *p == VECTOR_TARGET)
            .unwrap();
        let note = trigrams(&format!("{VECTOR_TARGET} {text}"));
        let common: Vec<String> = trigrams(VECTOR_QUERY)
            .into_iter()
            .filter(|t| note.contains(t))
            .collect();
        assert!(common.is_empty(), "общие подстроки: {common:?}");
        // а прежний перефраз trigram находил: light ⊂ sunlight
        assert!(trigrams("from light").contains("lig") && note.contains("lig"));
    }

    #[test]
    fn zam_vars_except_filters_case_insensitively() {
        let vars = [
            "ZAM_GIT_SHA",
            "zam_web_rebuild",
            "ZAM_ADMIN_PASSWORD",
            "PATH",
            "XZAM_A",
        ]
        .map(OsString::from);
        let got = zam_vars_except(vars, &["ZAM_ADMIN_PASSWORD"]);
        assert_eq!(got, ["ZAM_GIT_SHA", "zam_web_rebuild"].map(OsString::from));
    }

    #[test]
    fn output_with_timeout_returns_output() {
        let o = output_with_timeout(&mut echo(), Duration::from_secs(10)).unwrap();
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stdout).contains("hi"));
    }

    #[cfg(windows)]
    fn cat() -> Command {
        let mut c = Command::new("findstr");
        c.arg("^");
        c
    }
    #[cfg(not(windows))]
    fn cat() -> Command {
        Command::new("cat")
    }

    #[test]
    fn output_with_stdin_passes_input() {
        let o = output_with_stdin(&mut cat(), Some(b"abc\n"), Duration::from_secs(10)).unwrap();
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stdout).contains("abc"));
    }

    #[test]
    fn output_with_timeout_kills_slow_command() {
        let t0 = Instant::now();
        let e = output_with_timeout(&mut sleeper(), Duration::from_millis(300)).unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(10));
        assert!(format!("{e:#}").contains("убит"), "{e:#}");
    }
}
