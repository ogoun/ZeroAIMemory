//! Проверка переезда сервера на другую машину (M6 §12б.6, §17 п. 24; задача 18c):
//!
//! - `seed` — корпус на старом сервере: заметки, вложения PDF и DOCX, секрет Vault, идеи,
//!   отчёт с картинкой, карта кода (`zam code sync` на фикстуре `eval/sessions-corpus/code`);
//!   ожидания — в `expect.json` (id, sha256 содержимого и вложений, топ-5 на 10 запросах, имя
//!   и sha256 секрета, `model_id`, отпечатки нарезки). **Только на чистом сервере:** решение
//!   корпуса уже зарегистрировано — отказ (топ-5 по чужим данным ничего не подтверждает);
//! - `verify` — те же данные на новом сервере после `restore` (или без ключа — `--no-key`:
//!   Vault заблокирован с понятной ошибкой, остальное работает);
//! - `bindings` — ни одна ячейка баз каталога данных, кроме атрибуции `machine`, не содержит
//!   путей прежней машины и её имени (§12б.1). Проверка — модуль [`bindings`], общий с тестом
//!   `restore_has_no_machine_bindings` (`crates/zam-server/tests/backup.rs`).
//!
//! Запросы поиска идут без заголовков сессии и машины: выданное сессии понижается в выдаче, а
//! неявная сессия выбирается по машине (R13) — топ-5 сравнивается без этих поправок.
//!
//! Токен — флаг `--token` или переменная `ZAM_TOKEN` (так он не попадает в список процессов).

pub mod bindings;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::docs_corpus::{ooxml, pdf};
use crate::smoke::{agent_with, scrub_zam_env};

/// Решение корпуса переезда.
const SOLUTION: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e18";
/// Секрет Vault корпуса (значение — только в теле запроса; в `expect.json` — sha256).
const SECRET_TITLE: &str = "transfer-deploy-key";
const SECRET_VALUE: &str = "tc18c-vault-value-9b41e7";
/// PNG 1×1 — картинка отчёта.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];
/// Заметки корпуса: путь в Knowledge и текст. Темы разнесены, у каждого запроса — свой ответ.
const NOTES: &[(&str, &str)] = &[
    (
        "transfer/build",
        "# Сборка проекта\n\nПроект собирается командой cargo build --release; артефакты лежат в target/release.\n",
    ),
    (
        "transfer/deploy",
        "# Выкладка на сервер\n\nВыкладка идёт через systemd: unit копируется в /etc/systemd/system, затем daemon-reload.\n",
    ),
    (
        "transfer/backup",
        "# Резервные копии\n\nКопия снимается командой backup без остановки сервера; мастер-ключ хранится отдельно.\n",
    ),
    (
        "transfer/vacuum",
        "# Обслуживание SQLite\n\nVACUUM INTO делает согласованный снимок базы в транзакции чтения.\n",
    ),
    (
        "transfer/qdrant",
        "# Векторный индекс\n\nQdrant пересобирается из кэша эмбеддингов без инференса модели.\n",
    ),
    (
        "transfer/photosynthesis",
        "# Photosynthesis\n\nPlants convert sunlight, water and carbon dioxide into glucose and oxygen.\n",
    ),
    (
        "transfer/volcano",
        "# Volcanoes\n\nMagma rises through the crust and erupts as lava, ash and volcanic gases.\n",
    ),
    (
        "transfer/coffee",
        "# Кофе\n\nЭспрессо готовят под давлением девять бар из мелкого помола за двадцать пять секунд.\n",
    ),
    (
        "transfer/chess",
        "# Шахматы\n\nРокировка возможна, если король и ладья не ходили и поля между ними свободны.\n",
    ),
    (
        "transfer/tides",
        "# Tides\n\nOcean tides are caused by the gravitational pull of the Moon and the Sun.\n",
    ),
    (
        "transfer/bread",
        "# Хлеб на закваске\n\nЗакваску подкармливают мукой и водой, тесто расстаивается двенадцать часов.\n",
    ),
    (
        "transfer/bicycle",
        "# Велосипед\n\nЦепь смазывают каждые триста километров, а давление в шинах проверяют перед поездкой.\n",
    ),
];
/// Запросы проверки топ-5 (10 штук).
const QUERIES: &[&str] = &[
    "как собрать проект в release",
    "systemd unit выкладка",
    "резервная копия без остановки",
    "согласованный снимок базы sqlite",
    "пересборка векторного индекса",
    "how plants make glucose from sunlight",
    "lava and ash eruption",
    "давление при варке эспрессо",
    "moon gravity ocean",
    "obsidian heron copper kettles",
];
/// Факт PDF (страница 2) и DOCX (второй абзац).
const PDF_FACT: &str = "The obsidian heron delivered nineteen copper kettles to the harbour.";
const DOCX_FACT: &str = "The vermilion otter archived twelve glass marbles in the cellar.";
/// Предел ожидания индексации и разбора на старом сервере.
const SEED_WAIT: Duration = Duration::from_secs(600);

#[derive(Args)]
pub struct TransferCheckArgs {
    #[command(subcommand)]
    cmd: TcCmd,
}

#[derive(Subcommand)]
enum TcCmd {
    /// Корпус на старом сервере и ожидания в expect.json. Только на чистом сервере: если
    /// решение корпуса уже зарегистрировано, команда отказывает
    Seed {
        /// Адрес сервера, например http://127.0.0.1:7700
        #[arg(long)]
        server: String,
        /// Токен пользователя (rw); лучше переменной `ZAM_TOKEN` — флаг виден в списке процессов
        #[arg(long, env = "ZAM_TOKEN", hide_env_values = true)]
        token: String,
        /// Файл ожиданий; рядом на время работы создаётся каталог `seed-work` (с маркером)
        #[arg(long)]
        out: PathBuf,
        /// Клиент `zam` для `zam code sync` (из комплекта)
        #[arg(long)]
        zam: PathBuf,
        /// Каталог данных сервера (только чтение): отпечатки нарезки `meta.chunker*`
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Имя машины в `X-Zam-Machine` записей (по умолчанию — имя этой машины)
        #[arg(long)]
        machine: Option<String>,
    },
    /// Проверка ожиданий на новом сервере
    Verify {
        #[arg(long)]
        server: String,
        /// Токен пользователя; лучше переменной `ZAM_TOKEN`
        #[arg(long, env = "ZAM_TOKEN", hide_env_values = true)]
        token: String,
        /// Файл ожиданий seed
        #[arg(long)]
        expect: PathBuf,
        /// Сервер без мастер-ключа: Vault заблокирован (503 vault-locked), остальное работает
        #[arg(long)]
        no_key: bool,
        /// Каталог данных нового сервера (только чтение): integrity_check, отпечатки нарезки
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Предел ожидания пересборки индекса, с
        #[arg(long, default_value_t = 3600)]
        wait_secs: u64,
        /// Итог проверки в JSON
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Ячейки баз без путей прежней машины и её имени (кроме атрибуции `machine`)
    Bindings {
        /// Каталог данных (system.db, users/*.db)
        #[arg(long)]
        data_dir: PathBuf,
        /// Абсолютный путь старой машины; флаг повторяется: data_dir, каталог комплекта (exe,
        /// pdfium), каталог моделей. Пустой и слишком общий (корень диска, `/var`) — отказ
        #[arg(long, required = true)]
        old_path: Vec<String>,
        /// Имя старой машины (не короче 3 символов; сравнение по границам слова, без учёта
        /// регистра); флаг повторяется. Имя-слово (`dev`, `server`) найдётся и в обычном
        /// тексте заметок («the dev server») — такая находка не привязка, смотрите окрестность
        #[arg(long, required = true)]
        old_machine: Vec<String>,
    },
}

pub fn main(a: TransferCheckArgs, root: &Path) -> Result<()> {
    match a.cmd {
        TcCmd::Seed {
            server,
            token,
            out,
            zam,
            data_dir,
            machine,
        } => {
            let machine = machine
                .or_else(|| zam_core::machine_name(|k| std::env::var(k).ok()))
                .unwrap_or_else(|| "old-machine".into());
            let c = Conn::new(&server, &token, Some(machine));
            let e = seed(&c, root, &zam, &out, data_dir.as_deref())?;
            fs::write(&out, serde_json::to_string_pretty(&e)?)?;
            println!("seed: ожидания — {}", out.display());
            Ok(())
        }
        TcCmd::Verify {
            server,
            token,
            expect,
            no_key,
            data_dir,
            wait_secs,
            report,
        } => {
            let e: Expect = serde_json::from_str(
                &fs::read_to_string(&expect).with_context(|| expect.display().to_string())?,
            )
            .context("expect.json")?;
            let c = Conn::new(&server, &token, None);
            let rep = verify(
                &c,
                &e,
                no_key,
                data_dir.as_deref(),
                Duration::from_secs(wait_secs),
            );
            if let Some(p) = report {
                fs::write(&p, serde_json::to_string_pretty(&rep.json())?)?;
            }
            rep.finish()
        }
        TcCmd::Bindings {
            data_dir,
            old_path,
            old_machine,
        } => {
            let needles = bindings::Needles::new(&old_path, &old_machine)?;
            let scan = bindings::scan(&data_dir, &needles)?;
            println!(
                "bindings: {} баз, {} текстовых ячеек, {} BLOB с текстом (zstd, UTF-8)",
                scan.dbs, scan.text_cells, scan.blob_cells
            );
            if scan.text_cells == 0 {
                bail!(
                    "bindings: в базах {} нет текстовых ячеек",
                    data_dir.display()
                );
            }
            if scan.hits.is_empty() {
                println!(
                    "bindings: ok — в ячейках баз {} нет путей {:?} и машин {:?} (кроме machine)",
                    data_dir.display(),
                    needles.paths(),
                    needles.machines()
                );
                return Ok(());
            }
            for h in &scan.hits {
                println!("{h}");
            }
            bail!(
                "bindings: {} ячеек с привязкой к старой машине",
                scan.hits.len()
            )
        }
    }
}

// ---------------------------------------------------------------- HTTP

/// Сервер, токен и имя машины записей.
struct Conn {
    base: String,
    token: String,
    machine: Option<String>,
}

/// Ответ: статус и тело.
struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Conn {
    fn new(base: &str, token: &str, machine: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            machine,
        }
    }

    /// Запрос `m` к `path` (от `/api/v1`) с телом `body` (`content_type`) и токеном; запись —
    /// с `X-Zam-Machine`, если `attrib`.
    fn call(
        &self,
        m: &str,
        path: &str,
        body: Option<(&str, Vec<u8>)>,
        attrib: bool,
        cookie: Option<&str>,
    ) -> Result<Resp> {
        let a = agent_with(Duration::from_secs(120));
        let url = format!("{}/api/v1{path}", self.base);
        let auth = format!("Bearer {}", self.token);
        let machine = self.machine.as_deref().filter(|_| attrib);
        macro_rules! hdrs {
            ($r:expr) => {{
                let mut r = $r;
                r = match cookie {
                    Some(c) => r.header("Cookie", c),
                    None => r.header("Authorization", &auth),
                };
                if let Some(mc) = machine {
                    r = r.header("X-Zam-Machine", mc);
                }
                r
            }};
        }
        let mut r = match (m, body) {
            ("GET", _) => hdrs!(a.get(&url)).call()?,
            ("DELETE", _) => hdrs!(a.delete(&url)).call()?,
            (_, Some((ct, b))) => {
                let req = match m {
                    "POST" => a.post(&url),
                    "PUT" => a.put(&url),
                    "PATCH" => a.patch(&url),
                    other => bail!("метод {other}"),
                };
                hdrs!(req).content_type(ct).send(&b[..])?
            }
            (other, None) => bail!("{other} {path}: нет тела"),
        };
        let status = r.status().as_u16();
        let body = r.body_mut().with_config().limit(64 << 20).read_to_vec()?;
        Ok(Resp { status, body })
    }

    fn get(&self, path: &str) -> Result<Resp> {
        self.call("GET", path, None, false, None)
    }

    /// POST JSON с атрибуцией машины; ответ `want`.
    fn post(&self, path: &str, body: &Value, want: u16) -> Result<Value> {
        let r = self.call(
            "POST",
            path,
            Some(("application/json", body.to_string().into_bytes())),
            true,
            None,
        )?;
        if r.status != want {
            bail!(
                "POST {path}: HTTP {} (ожидался {want}): {}",
                r.status,
                r.text()
            );
        }
        Ok(r.json())
    }

    /// Поиск без атрибуции (без сессии и машины): `(id, балл)` первых шести.
    fn top6(&self, q: &str) -> Result<(Vec<(String, f64)>, Value)> {
        let r = self.search(q)?;
        if r.status != 200 {
            bail!("POST /search «{q}»: HTTP {}: {}", r.status, r.text());
        }
        let v = r.json();
        let items = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| (item_id(i), i["score"].as_f64().unwrap_or_default()))
            .collect();
        Ok((items, v))
    }

    /// `POST /search` без атрибуции: шесть первых в области решения корпуса; ответ как есть.
    fn search(&self, q: &str) -> Result<Resp> {
        let body = json!({"query": q, "top_k": 6, "scope": {"solution": SOLUTION}});
        self.call(
            "POST",
            "/search",
            Some(("application/json", body.to_string().into_bytes())),
            false,
            None,
        )
    }

    /// Вход администратора WebUI паролем `pw`: cookie.
    fn login(&self, pw: &str) -> Result<String> {
        let a = agent_with(Duration::from_secs(30));
        let mut r = a
            .post(&format!("{}/api/v1/auth/login", self.base))
            .content_type("application/json")
            .send(json!({"login": "admin", "password": pw}).to_string())?;
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
                    .then(|| c.split(';').next().unwrap_or_default().to_string())
            })
            .context("в ответе входа нет cookie zam_session")?;
        let _ = r.body_mut().read_to_string();
        Ok(cookie)
    }
}

/// Идентификатор элемента выдачи: фрагмент вложения — `id` (`a:<чанк>`), запись журнала —
/// `entry_id`, заметка — `node_id` (не путь: путь совпал бы и у другой заметки на том же месте).
/// Пусто — у элемента нет ни одного из них.
fn item_id(i: &Value) -> String {
    ["id", "entry_id", "node_id"]
        .iter()
        .find_map(|k| i[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or_default()
        .to_string()
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn multipart(boundary: &str, parts: &[(&str, Option<&str>, &str, &[u8])]) -> Vec<u8> {
    let mut b = Vec::new();
    for (name, file, ct, data) in parts {
        let disp = match file {
            Some(f) => format!("form-data; name=\"{name}\"; filename=\"{f}\""),
            None => format!("form-data; name=\"{name}\""),
        };
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: {disp}\r\nContent-Type: {ct}\r\n\r\n")
                .as_bytes(),
        );
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}

/// Ждёт `ok()` не дольше `limit`, опрос раз в `step`; `what` — для ошибки. Транспортная
/// ошибка запроса (сервер ещё поднимается, перезапускается, обрыв) — не отказ: опрос
/// повторяется до предела, последняя такая ошибка попадает в сообщение. Прочие ошибки `ok()`
/// (состояние, которое уже не изменится) прерывают ожидание сразу.
fn until(
    what: &str,
    limit: Duration,
    step: Duration,
    mut ok: impl FnMut() -> Result<bool>,
) -> Result<Duration> {
    let t0 = Instant::now();
    let mut last = None;
    loop {
        match ok() {
            Ok(true) => return Ok(t0.elapsed()),
            Ok(false) => {}
            Err(e) if e.downcast_ref::<ureq::Error>().is_some() => last = Some(format!("{e:#}")),
            Err(e) => return Err(e),
        }
        if t0.elapsed() > limit {
            let tail = last.map(|e| format!("; последняя ошибка связи: {e}"));
            bail!(
                "{what}: не дождались за {} с{}",
                limit.as_secs(),
                tail.unwrap_or_default()
            );
        }
        thread::sleep(step);
    }
}

// ---------------------------------------------------------------- ожидания

#[derive(Serialize, Deserialize, Debug, Clone)]
struct NoteExp {
    id: String,
    path: String,
    sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AttachExp {
    file: String,
    sha256: String,
    size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EntryExp {
    id: String,
    kind: String,
    status: Option<String>,
    title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct QueryExp {
    query: String,
    top: Vec<(String, f64)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Expect {
    created_at: String,
    server_version: String,
    machine: String,
    solution: String,
    model_id: Option<String>,
    /// `meta.chunker*` по базам (`--data-dir`), иначе пусто.
    chunkers: BTreeMap<String, BTreeMap<String, String>>,
    notes: Vec<NoteExp>,
    attach_node: String,
    attachments: Vec<AttachExp>,
    vault_id: String,
    vault_title: String,
    vault_sha256: String,
    entries: Vec<EntryExp>,
    report_id: String,
    report_image_sha256: String,
    code_files: u64,
    code_path: String,
    queries: Vec<QueryExp>,
}

fn chunkers(data_dir: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut out = BTreeMap::new();
    for db in user_dbs(data_dir)? {
        let c =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut st = c.prepare("SELECT key, value FROM meta WHERE key LIKE 'chunker%'")?;
        let m: BTreeMap<String, String> = st
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        out.insert(db.file_name().unwrap().to_string_lossy().into_owned(), m);
    }
    Ok(out)
}

fn user_dbs(data_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = fs::read_dir(data_dir.join("users"))
        .with_context(|| format!("{}/users", data_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .collect();
    v.sort();
    Ok(v)
}

// ---------------------------------------------------------------- seed

fn seed(c: &Conn, root: &Path, zam: &Path, out: &Path, data_dir: Option<&Path>) -> Result<Expect> {
    let health = c.get("/health")?.json();
    let r = c.call(
        "POST",
        "/solutions",
        Some((
            "application/json",
            json!({"agent_solution_id": SOLUTION, "name": "transfer-check"})
                .to_string()
                .into_bytes(),
        )),
        true,
        None,
    )?;
    if r.status == 200 {
        bail!(
            "решение корпуса {SOLUTION} на сервере уже зарегистрировано: seed работает только \
             на чистом сервере (свежий каталог данных) — поверх прежнего корпуса id и топ-5 \
             ожиданий были бы другими"
        );
    }
    if r.status != 201 {
        bail!("POST /solutions: HTTP {}: {}", r.status, r.text());
    }
    // заметки
    let mut notes = Vec::new();
    for (path, text) in NOTES {
        let v = c.post(
            "/nodes",
            &json!({"space": "knowledge", "path": path, "kind": "note", "content": text}),
            201,
        )?;
        notes.push(NoteExp {
            id: v["id"].as_str().context("нет id заметки")?.to_string(),
            path: (*path).to_string(),
            sha256: sha_hex(text.as_bytes()),
        });
    }
    // вложения PDF и DOCX
    let v = c.post(
        "/nodes",
        &json!({"space": "knowledge", "path": "transfer/docs", "kind": "note",
                "content": "# Документы переезда\n\nВложения PDF и DOCX.\n"}),
        201,
    )?;
    let node = v["id"].as_str().context("нет id заметки")?.to_string();
    let page = |lines: &[&str]| pdf::Page {
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, t)| pdf::Line {
                x: 72.0,
                y: pdf::PAGE_H - 120.0 - 18.0 * i as f32,
                size: 12.0,
                text: (*t).to_string(),
            })
            .collect(),
        ..Default::default()
    };
    let pdf_bytes = pdf::build(&pdf::Doc {
        pages: vec![
            page(&["Transfer document, first page.", "Nothing to see here."]),
            page(&["Transfer document, second page.", PDF_FACT]),
        ],
        open_action_js: None,
    });
    let (docx_bytes, _) = ooxml::docx(&[
        ooxml::Block::Heading(1, "Transfer handbook".into()),
        ooxml::Block::Para("Introductory paragraph without the marker.".into()),
        ooxml::Block::Para(DOCX_FACT.into()),
    ]);
    let mut attachments = Vec::new();
    for (file, data) in [("transfer.pdf", &pdf_bytes), ("transfer.docx", &docx_bytes)] {
        let boundary = "zam-tc18c-attach";
        let body = multipart(
            boundary,
            &[
                ("node", None, "text/plain", node.as_bytes()),
                ("file", Some(file), "application/octet-stream", data),
            ],
        );
        let r = c.call(
            "POST",
            "/blobs",
            Some((&format!("multipart/form-data; boundary={boundary}"), body)),
            true,
            None,
        )?;
        if r.status != 201 {
            bail!("POST /blobs {file}: HTTP {}: {}", r.status, r.text());
        }
        attachments.push(AttachExp {
            file: file.to_string(),
            sha256: sha_hex(data),
            size: data.len() as u64,
        });
    }
    // секрет Vault решения
    let v = c.post(
        &format!("/spaces/{SOLUTION}/vault"),
        &json!({"title": SECRET_TITLE, "data": {"type": "token"}, "secret": SECRET_VALUE}),
        201,
    )?;
    let vault_id = v["id"].as_str().context("нет id секрета")?.to_string();
    // идеи (одна принята — задача), insight, вопрос
    let feed = format!("/spaces/{SOLUTION}/entries");
    let mut entries = Vec::new();
    let mut add = |kind: &str, title: &str, body: Value| -> Result<String> {
        let v = c.post(&feed, &body, 201)?;
        let id = v["id"].as_str().context("нет id записи")?.to_string();
        entries.push(EntryExp {
            id: id.clone(),
            kind: kind.to_string(),
            status: None,
            title: title.to_string(),
        });
        Ok(id)
    };
    for t in [
        "Идея: проверять переезд каждый релиз",
        "Идея: копия на другой диск",
    ] {
        add(
            "idea",
            t,
            json!({"kind": "idea", "title": t, "body": "Предложение корпуса переезда.",
                   "data": {"origin": "agent", "priority": "normal"}}),
        )?;
    }
    let accepted = add(
        "idea",
        "Идея: журнал стенда",
        json!({"kind": "idea", "title": "Идея: журнал стенда", "body": "Вести журнал.",
               "data": {"origin": "human"}}),
    )?;
    add(
        "insight",
        "Мастер-ключ хранится отдельно от копии",
        json!({"kind": "insight", "title": "Мастер-ключ хранится отдельно от копии",
               "body": "Без master.key секреты Vault на новой машине не раскрыть.",
               "data": {"kind": "gotcha"}, "force": true}),
    )?;
    c.post(
        &format!("/entries/{accepted}/decide"),
        &json!({"decision": "accept", "by": "human"}),
        200,
    )?;
    // статусы — после всех переходов
    for e in &mut entries {
        let r = c.get(&format!("/entries/{}", e.id))?;
        if r.status != 200 {
            bail!("GET /entries/{}: HTTP {}", e.id, r.status);
        }
        e.status = r.json()["status"].as_str().map(String::from);
    }
    // отчёт с картинкой (как `zam report`)
    let boundary = "zam-tc18c-report";
    let meta = json!({"title": "Отчёт переезда", "tags": ["transfer"],
                      "body": "# Отчёт переезда\n\nКартинка:\n\n![dot](figs/dot.png)\n"});
    let body = multipart(
        boundary,
        &[
            (
                "meta",
                None,
                "application/json",
                meta.to_string().as_bytes(),
            ),
            (
                "file",
                Some("figs/dot.png"),
                "application/octet-stream",
                PNG,
            ),
        ],
    );
    let r = c.call(
        "POST",
        &feed,
        Some((&format!("multipart/form-data; boundary={boundary}"), body)),
        true,
        None,
    )?;
    if r.status != 201 {
        bail!("отчёт: HTTP {}: {}", r.status, r.text());
    }
    let report_id = r.json()["id"]
        .as_str()
        .context("нет id отчёта")?
        .to_string();
    // карта кода: `zam code sync` на фикстуре
    let work = SeedWork::create(
        &out.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .join("seed-work"),
    )?;
    code_sync(
        c,
        zam,
        &root.join("eval").join("sessions-corpus").join("code"),
        &work.0,
    )?;
    drop(work);
    let st = c.get(&format!("/code/status?solution={SOLUTION}"))?.json();
    let code_files = st["files"].as_u64().unwrap_or(0);
    if code_files == 0 {
        bail!("карта кода пуста после zam code sync: {st}");
    }
    // разбор вложений и индексация
    wait_attachments(c, &node, &attachments, SEED_WAIT)?;
    wait_index(c, SEED_WAIT)?;
    let mut queries = Vec::new();
    for q in QUERIES {
        let (top, _) = c.top6(q)?;
        full_top5(q, &top)?;
        queries.push(QueryExp {
            query: (*q).to_string(),
            top,
        });
    }
    let ist = c.get("/index/status")?.json();
    let model_id = ist["model"]["id"]
        .as_str()
        .filter(|m| !m.is_empty())
        .context(
            "у сервера нет модели эмбеддингов (index/status: model.id): без векторного канала \
             топ-5 не подтверждает перенос индекса — установите модель (models fetch)",
        )?
        .to_string();
    Ok(Expect {
        created_at: jiff::Timestamp::now().to_string(),
        server_version: health["version"].as_str().unwrap_or_default().to_string(),
        machine: c.machine.clone().unwrap_or_default(),
        solution: SOLUTION.to_string(),
        model_id: Some(model_id),
        chunkers: match data_dir {
            Some(d) => chunkers(d)?,
            None => BTreeMap::new(),
        },
        notes,
        attach_node: node,
        attachments,
        vault_id,
        vault_title: SECRET_TITLE.to_string(),
        vault_sha256: sha_hex(SECRET_VALUE.as_bytes()),
        entries,
        report_id,
        report_image_sha256: sha_hex(PNG),
        code_files,
        code_path: "services/orders/src/export.rs".into(),
        queries,
    })
}

/// Фикстура карты кода → проект с `solution.toml` в `work`, `zam code sync --json`.
fn code_sync(c: &Conn, zam: &Path, fixture: &Path, work: &Path) -> Result<()> {
    let project = work.join("project");
    let home = work.join("home");
    copy_dir(fixture, &project)?;
    fs::create_dir_all(project.join(".zeroaimemory"))?;
    fs::write(
        project.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{SOLUTION}\"\nname = \"transfer-check\"\n"),
    )?;
    fs::create_dir_all(home.join(".zeroaimemory"))?;
    fs::write(
        home.join(".zeroaimemory").join("config.toml"),
        "[code]\nauto = false\n",
    )?;
    let mut cmd = Command::new(zam);
    scrub_zam_env(&mut cmd, &[]);
    for k in zam_core::AGENT_ENV_VARS {
        cmd.env_remove(k);
    }
    if let Some(m) = &c.machine {
        cmd.env("ZAM_MACHINE", m);
    }
    let out = cmd
        .args(["code", "sync", "--json"])
        .current_dir(&project)
        .env("ZAM_URL", &c.base)
        .env("ZAM_TOKEN", &c.token)
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .output()
        .with_context(|| format!("запуск {}", zam.display()))?;
    if !out.status.success() {
        bail!(
            "zam code sync: код {:?}: {}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    println!(
        "zam code sync: {}",
        String::from_utf8_lossy(&out.stdout).trim()
    );
    Ok(())
}

/// Маркер рабочего каталога `seed`: чужой каталог с тем же именем команда не трогает.
const SEED_MARK: &str = ".zam-transfer-seed";

/// Рабочий каталог `seed` (проект фикстуры карты кода и домашний каталог клиента): создаётся с
/// маркером [`SEED_MARK`] и удаляется при выходе из области. Существующий каталог удаляется,
/// только если маркер в нём есть (остаток прерванного `seed`), иначе — отказ.
struct SeedWork(PathBuf);

impl SeedWork {
    fn create(dir: &Path) -> Result<Self> {
        if dir.exists() {
            if !dir.join(SEED_MARK).is_file() {
                bail!(
                    "каталог {} уже есть и создан не командой seed (нет маркера {SEED_MARK}): \
                     уберите его или задайте --out в другом каталоге",
                    dir.display()
                );
            }
            fs::remove_dir_all(dir).with_context(|| dir.display().to_string())?;
        }
        fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
        fs::write(dir.join(SEED_MARK), "cargo xtask transfer-check seed\n")?;
        Ok(Self(dir.to_path_buf()))
    }
}

impl Drop for SeedWork {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from).with_context(|| from.display().to_string())? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()))?;
        } else {
            fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Документы вложений `node` разобраны (`done`).
fn wait_attachments(c: &Conn, node: &str, want: &[AttachExp], limit: Duration) -> Result<()> {
    until(
        "разбор вложений",
        limit,
        Duration::from_millis(500),
        || {
            let v = c.get(&format!("/nodes/{node}/attachments"))?.json();
            let docs = v.as_array().cloned().unwrap_or_default();
            if let Some(d) = docs
                .iter()
                .find(|d| matches!(d["extract_state"].as_str(), Some("failed" | "unsupported")))
            {
                bail!(
                    "{}: extract_state = {}, extract_error = {}",
                    d["filename"],
                    d["extract_state"],
                    d["extract_error"]
                );
            }
            Ok(want.iter().all(|w| {
                docs.iter()
                    .any(|d| d["filename"] == w.file.as_str() && d["extract_state"] == "done")
            }))
        },
    )?;
    Ok(())
}

/// Индекс готов: векторный канал есть, очередь пуста, reindex не идёт, поиск без
/// `pending_count` и `degraded`. Возвращает время ожидания.
fn wait_index(c: &Conn, limit: Duration) -> Result<Duration> {
    until(
        "индексация",
        limit,
        Duration::from_secs(2),
        || {
            let st = c.get("/index/status")?.json();
            if st["vector"]["available"] != true
                || st["queue"]["queued"].as_u64().unwrap_or(1) > 0
                || st["queue"]["running"].as_u64().unwrap_or(1) > 0
                || st["reindex"]["running"] == true
            {
                return Ok(false);
            }
            search_ready(&c.search(QUERIES[0])?)
        },
    )
}

/// Ответ `/search` при ожидании индекса: 200 без `pending_count` и `degraded` — готово; 200 с
/// ними и любой 5xx (Qdrant ещё поднимается, сервер занят) — «ещё не готово», ожидание
/// продолжается; прочее (4xx: токен, запрос) не изменится — ошибка.
fn search_ready(r: &Resp) -> Result<bool> {
    match r.status {
        200 => {
            let v = r.json();
            Ok(v["pending_count"] == 0 && v["degraded"] == false)
        }
        500..=599 => Ok(false),
        s => bail!("POST /search: HTTP {s}: {}", r.text()),
    }
}

// ---------------------------------------------------------------- verify

/// Итог проверки: пункты (имя, ok, подробности) и числа.
#[derive(Default)]
struct Report {
    checks: Vec<(String, bool, String)>,
    numbers: BTreeMap<String, Value>,
}

impl Report {
    fn check(&mut self, name: &str, r: Result<String>) {
        let (ok, msg) = match r {
            Ok(m) => (true, m),
            Err(e) => (false, format!("{e:#}")),
        };
        println!("[{}] {name}: {msg}", if ok { "ok" } else { "FAIL" });
        self.checks.push((name.to_string(), ok, msg));
    }

    fn json(&self) -> Value {
        json!({
            "ok": self.checks.iter().all(|c| c.1),
            "checks": self.checks.iter().map(|(n, ok, m)| json!({"name": n, "ok": ok, "detail": m}))
                .collect::<Vec<_>>(),
            "numbers": self.numbers,
        })
    }

    fn finish(&self) -> Result<()> {
        let bad: Vec<&str> = self
            .checks
            .iter()
            .filter(|c| !c.1)
            .map(|c| c.0.as_str())
            .collect();
        if bad.is_empty() {
            println!("verify: ok ({} проверок)", self.checks.len());
            Ok(())
        } else {
            bail!("verify: не прошли {}: {}", bad.len(), bad.join(", "))
        }
    }
}

fn verify(c: &Conn, e: &Expect, no_key: bool, data_dir: Option<&Path>, wait: Duration) -> Report {
    let mut rep = Report::default();
    rep.check("health", check_health(c));
    // индекс: пересборка из кэша завершилась, модель та же
    let waited = wait_index(c, wait);
    rep.check(
        "индекс готов",
        waited
            .as_ref()
            .map(|d| format!("ожидание {:.1} с", d.as_secs_f64()))
            .map_err(|e| anyhow::anyhow!("{e:#}")),
    );
    if let Ok(d) = &waited {
        rep.numbers
            .insert("index_wait_secs".into(), json!(d.as_secs_f64()));
    }
    rep.check("модель", check_model(c, e));
    if let Ok(st) = c.get("/index/status").map(|r| r.json()) {
        rep.numbers.insert("index_status".into(), st);
    }
    rep.check("заметки", check_notes(c, e));
    rep.check("поиск: топ-5 на 10 запросах", check_top5(c, e));
    rep.check("вложения", check_attachments(c, e));
    rep.check(
        "Vault",
        if no_key {
            check_vault_locked(c, e)
        } else {
            check_vault(c, e)
        },
    );
    rep.check("идеи и записи", check_entries(c, e));
    rep.check("отчёт", check_report(c, e));
    rep.check("карта кода", check_code(c, e));
    if let Some(d) = data_dir {
        rep.check("integrity_check", check_integrity(d));
        rep.check("отпечатки нарезки", check_chunkers(d, e));
    }
    rep
}

fn check_health(c: &Conn) -> Result<String> {
    let h = c.get("/health")?;
    if h.status != 200 {
        bail!("HTTP {}", h.status);
    }
    Ok(format!("версия {}", h.json()["version"]))
}

/// Модель нового сервера — та же, что в ожиданиях. Ожидания без `model_id` — отказ: «нет
/// модели там и тут» совпадением не считается.
fn check_model(c: &Conn, e: &Expect) -> Result<String> {
    let want = e
        .model_id
        .as_deref()
        .filter(|m| !m.is_empty())
        .context("в expect.json нет model_id: seed снят без модели, сверять нечего")?;
    let st = c.get("/index/status")?.json();
    let got = st["model"]["id"].as_str();
    if got != Some(want) {
        bail!("model_id {got:?}, ожидался {want:?}");
    }
    Ok(format!(
        "{want}; reindex.last = {}; worker = {}",
        st["reindex"]["last"], st["worker"]
    ))
}

fn check_notes(c: &Conn, e: &Expect) -> Result<String> {
    if e.notes.is_empty() {
        bail!("в expect.json нет заметок");
    }
    for n in &e.notes {
        let r = c.get(&format!("/nodes/{}", n.id))?;
        if r.status != 200 {
            bail!("{} {}: HTTP {}", n.id, n.path, r.status);
        }
        let v = r.json();
        let content = v["content"].as_str().unwrap_or_default();
        if sha_hex(content.as_bytes()) != n.sha256
            || v["node"]["path"].as_str() != Some(n.path.as_str())
        {
            bail!("{} {}: содержимое или путь другие", n.id, n.path);
        }
    }
    Ok(format!("{} заметок, содержимое совпадает", e.notes.len()))
}

fn check_top5(c: &Conn, e: &Expect) -> Result<String> {
    if e.queries.len() != QUERIES.len() {
        bail!(
            "в expect.json {} запросов, нужно {}",
            e.queries.len(),
            QUERIES.len()
        );
    }
    for q in &e.queries {
        let (got, _) = c.top6(&q.query)?;
        same_top5(&q.query, &q.top, &got)?;
    }
    Ok(format!(
        "{} запросов: в топ-5 те же id (заметки — node_id, вложения — a:<чанк>) на тех же местах",
        e.queries.len()
    ))
}

fn check_attachments(c: &Conn, e: &Expect) -> Result<String> {
    if e.attachments.is_empty() {
        bail!("в expect.json нет вложений");
    }
    let v = c
        .get(&format!("/nodes/{}/attachments", e.attach_node))?
        .json();
    let docs = v.as_array().cloned().unwrap_or_default();
    for a in &e.attachments {
        let d = docs
            .iter()
            .find(|d| d["filename"] == a.file.as_str())
            .with_context(|| format!("нет вложения {}", a.file))?;
        if d["sha256"] != a.sha256.as_str() || d["extract_state"] != "done" {
            bail!(
                "{}: sha256 = {}, extract_state = {}",
                a.file,
                d["sha256"],
                d["extract_state"]
            );
        }
        let r = c.get(&format!("/blobs/{}/{}", a.sha256, a.file))?;
        if r.status != 200 || sha_hex(&r.body) != a.sha256 {
            bail!(
                "GET /blobs/{}/{}: HTTP {}, {} байт",
                a.sha256,
                a.file,
                r.status,
                r.body.len()
            );
        }
    }
    let pdf = &e
        .attachments
        .iter()
        .find(|a| a.file.ends_with(".pdf"))
        .context("нет PDF")?
        .sha256;
    let r = c.get(&format!("/previews/{pdf}?kind=thumb&page=1&px=128"))?;
    if r.status != 200 || !r.body.starts_with(b"\x89PNG\r\n\x1a\n") {
        bail!("превью PDF: HTTP {}, {} байт", r.status, r.body.len());
    }
    Ok(format!(
        "{} файлов открываются, превью PDF — PNG {} байт",
        e.attachments.len(),
        r.body.len()
    ))
}

/// С ключом: секрет раскрывается, значение то же.
fn check_vault(c: &Conn, e: &Expect) -> Result<String> {
    let r = c.get(&format!("/vault/{}?reveal=1", e.vault_id))?;
    if r.status != 200 {
        bail!("HTTP {}: {}", r.status, r.text());
    }
    let v = r.json();
    let secret = v["secret"]
        .as_str()
        .or(v["value"].as_str())
        .unwrap_or_default();
    if sha_hex(secret.as_bytes()) != e.vault_sha256 || v["title"] != e.vault_title.as_str() {
        bail!("секрет {} другой", e.vault_title);
    }
    Ok(format!(
        "секрет «{}» раскрыт, значение совпадает",
        e.vault_title
    ))
}

/// Без ключа (`--no-key`): раскрытие — 503 `vault-locked`, список отвечает с `locked: true`.
fn check_vault_locked(c: &Conn, e: &Expect) -> Result<String> {
    let r = c.get(&format!("/vault/{}?reveal=1", e.vault_id))?;
    let p = r.json();
    let kind = p["type"].as_str().unwrap_or_default();
    if r.status != 503 || !kind.ends_with("vault-locked") {
        bail!(
            "без ключа ожидался 503 vault-locked: HTTP {}: {}",
            r.status,
            r.text()
        );
    }
    let l = c.get(&format!("/spaces/{}/vault", e.solution))?;
    if l.status != 200 || l.json()["locked"] != true {
        bail!("список Vault без ключа: HTTP {}: {}", l.status, l.text());
    }
    Ok(format!(
        "заблокирован, как ожидалось: {} «{}»; список отвечает (locked: true)",
        p["type"],
        p["detail"]
            .as_str()
            .or(p["title"].as_str())
            .unwrap_or_default()
    ))
}

fn check_entries(c: &Conn, e: &Expect) -> Result<String> {
    if e.entries.is_empty() {
        bail!("в expect.json нет записей");
    }
    for x in &e.entries {
        let r = c.get(&format!("/entries/{}", x.id))?;
        let v = r.json();
        if r.status != 200
            || v["title"] != x.title.as_str()
            || v["status"].as_str().map(String::from) != x.status
        {
            bail!(
                "{} {}: HTTP {}, status {}",
                x.kind,
                x.id,
                r.status,
                v["status"]
            );
        }
    }
    Ok(format!(
        "{} записей (идеи, insight) со статусами",
        e.entries.len()
    ))
}

/// Отчёт в ленте и его картинка — глазами человека: вход администратора паролем из
/// `ZAM_ADMIN_PASSWORD` (агентскому токену отчёты не выдаются).
fn check_report(c: &Conn, e: &Expect) -> Result<String> {
    let Ok(pw) = std::env::var("ZAM_ADMIN_PASSWORD") else {
        bail!(
            "нужен ZAM_ADMIN_PASSWORD (пароль администратора нового сервера): \
             отчёт видит только человек"
        );
    };
    let cookie = c.login(&pw)?;
    let feed = format!("/spaces/{}/entries?kind=report", e.solution);
    let r = c.call("GET", &feed, None, false, Some(&cookie))?;
    if r.status != 200 {
        bail!("лента отчётов: HTTP {}", r.status);
    }
    let v = r.json();
    let item = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["id"] == e.report_id.as_str())
        .with_context(|| format!("отчёта {} нет в ленте", e.report_id))?;
    let url = item["first_image"]["url"]
        .as_str()
        .context("нет first_image")?;
    let url = format!("{}/{}", c.base, url.trim_start_matches('/'));
    let mut r = agent_with(Duration::from_secs(30))
        .get(&url)
        .header("Cookie", &cookie)
        .call()?;
    let st = r.status().as_u16();
    let b = r.body_mut().read_to_vec()?;
    if st != 200 || sha_hex(&b) != e.report_image_sha256 {
        bail!("картинка отчёта: HTTP {st}, {} байт", b.len());
    }
    Ok(format!("отчёт {} в ленте, картинка совпадает", e.report_id))
}

fn check_code(c: &Conn, e: &Expect) -> Result<String> {
    if e.code_files == 0 {
        bail!("в expect.json карта кода пуста");
    }
    let st = c
        .get(&format!("/code/status?solution={}", e.solution))?
        .json();
    if st["files"].as_u64() != Some(e.code_files) {
        bail!("files = {}, ожидалось {}", st["files"], e.code_files);
    }
    let m = c.get(&format!("/code/map?solution={}", e.solution))?;
    if m.status != 200 || !m.text().contains(&e.code_path) {
        bail!("карта: HTTP {}, нет {}", m.status, e.code_path);
    }
    Ok(format!("{} файлов, карта отдаётся", e.code_files))
}

fn check_integrity(data_dir: &Path) -> Result<String> {
    let dbs = bindings::databases(data_dir)?;
    for db in &dbs {
        let c =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if ok != "ok" {
            bail!("{}: {ok}", db.display());
        }
    }
    Ok(format!("{} баз — ok", dbs.len()))
}

/// Отпечатки нарезки баз — как в ожиданиях. Ожидания без отпечатков (seed без `--data-dir`) —
/// отказ, а не пропуск.
fn check_chunkers(data_dir: &Path, e: &Expect) -> Result<String> {
    if e.chunkers.is_empty() || e.chunkers.values().any(BTreeMap::is_empty) {
        bail!("в expect.json нет отпечатков нарезки: seed запущен без --data-dir");
    }
    let got = chunkers(data_dir)?;
    for (db, m) in &e.chunkers {
        if got.get(db) != Some(m) {
            bail!("{db}: {:?}, ожидалось {m:?}", got.get(db));
        }
    }
    Ok("meta.chunker* совпадают".to_string())
}

/// В выдаче не меньше пяти элементов, и у каждого из первых пяти есть id. Иначе сравнение
/// топ-5 было бы пустым: «ничего не нашли там и тут» — не совпадение.
fn full_top5(q: &str, top: &[(String, f64)]) -> Result<()> {
    if top.len() < 5 {
        bail!("«{q}»: в выдаче {} элементов, для топ-5 нужно 5", top.len());
    }
    if let Some(i) = top.iter().take(5).position(|x| x.0.is_empty()) {
        bail!("«{q}»: у элемента на месте {} нет id", i + 1);
    }
    Ok(())
}

/// Топ-5 совпадают: те же пять id на тех же местах (§12б.6); id заметки — `node_id`,
/// фрагмента вложения — `a:<чанк>`, записи — `entry_id` ([`item_id`]). Баллы не сравниваются:
/// это нормированные места слияния каналов (1,0; 0,984; 0,968 …), а не косинусы.
fn same_top5(q: &str, before: &[(String, f64)], after: &[(String, f64)]) -> Result<()> {
    full_top5(q, before).context("ожидания (expect.json)")?;
    let ids = |v: &[(String, f64)]| v.iter().take(5).map(|x| x.0.clone()).collect::<Vec<_>>();
    if ids(before) != ids(after) {
        bail!("«{q}»: топ-5 {:?} вместо {:?}", ids(after), ids(before));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::bindings::{Needles, machine_needle, norm, path_needle, percent_decode, scan};
    use super::*;

    /// Свежий каталог теста в `target/test-tmp` (как у остальных тестов xtask).
    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/xtask")
            .join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn win() -> Needles {
        Needles::new(&[r"H:\Zam Data\Данные"], &["WIN-BOX"]).unwrap()
    }

    fn top(ids: &[&str]) -> Vec<(String, f64)> {
        ids.iter().map(|i| (i.to_string(), 0.0)).collect()
    }

    #[test]
    fn top5_same_ids_same_places() {
        let a = top(&["a", "b", "c", "d", "e", "f"]);
        assert!(same_top5("q", &a, &a).is_ok());
        // шестое место в сравнение не входит
        assert!(same_top5("q", &a, &top(&["a", "b", "c", "d", "e", "z"])).is_ok());
        assert!(same_top5("q", &a, &top(&["a", "c", "b", "d", "e"])).is_err());
        assert!(same_top5("q", &a, &top(&["a", "b", "c", "d", "f"])).is_err());
        assert!(same_top5("q", &a, &top(&["a", "b", "c", "d"])).is_err());
    }

    /// Пустой зелёный: короткая выдача и пустые id — отказ, даже когда «до» и «после» равны.
    #[test]
    fn top5_refuses_short_and_empty() {
        let short = top(&["a", "b", "c"]);
        assert!(same_top5("q", &short, &short).is_err());
        assert!(same_top5("q", &[], &[]).is_err());
        let blank = top(&["a", "", "c", "d", "e"]);
        assert!(same_top5("q", &blank, &blank).is_err());
        assert!(full_top5("q", &short).is_err());
        assert!(full_top5("q", &blank).is_err());
        assert!(full_top5("q", &top(&["a", "b", "c", "d", "e"])).is_ok());
    }

    /// Заметка сравнивается по `node_id`, не по пути; вложение — по `a:<чанк>`, запись — по
    /// `entry_id`.
    #[test]
    fn item_id_prefers_ids_over_path() {
        let note = json!({"node_id": "01N", "path": "transfer/build"});
        assert_eq!(item_id(&note), "01N");
        let att = json!({"id": "a:c1", "node_id": "01OWNER", "path": "transfer/docs"});
        assert_eq!(item_id(&att), "a:c1");
        assert_eq!(item_id(&json!({"entry_id": "01E", "kind": "idea"})), "01E");
        assert_eq!(item_id(&json!({"path": "transfer/build"})), "");
    }

    /// Проверка без `model_id` и без отпечатков в ожиданиях — отказ, а не «None == None».
    #[test]
    fn verify_refuses_empty_expectations() {
        let c = Conn::new("http://127.0.0.1:9", "t", None);
        let e: Expect = serde_json::from_value(json!({
            "created_at": "", "server_version": "", "machine": "", "solution": SOLUTION,
            "model_id": null, "chunkers": {}, "notes": [], "attach_node": "", "attachments": [],
            "vault_id": "", "vault_title": "", "vault_sha256": "", "entries": [],
            "report_id": "", "report_image_sha256": "", "code_files": 0, "code_path": "",
            "queries": []
        }))
        .unwrap();
        // отказы — до первого запроса к серверу (порт 9 не слушает)
        let msg = |r: Result<String>| format!("{:#}", r.unwrap_err());
        assert!(msg(check_model(&c, &e)).contains("нет model_id"));
        assert!(msg(check_top5(&c, &e)).contains("запросов"));
        assert!(msg(check_notes(&c, &e)).contains("нет заметок"));
        assert!(msg(check_attachments(&c, &e)).contains("нет вложений"));
        assert!(msg(check_entries(&c, &e)).contains("нет записей"));
        assert!(msg(check_code(&c, &e)).contains("пуста"));
        assert!(msg(check_chunkers(Path::new("нет"), &e)).contains("нет отпечатков"));
    }

    /// Транспортная ошибка опроса повторяется до предела; прочая — прерывает сразу.
    #[test]
    fn until_retries_transport_errors() {
        let step = Duration::from_millis(1);
        let mut n = 0;
        let r = until("опрос", Duration::from_secs(5), step, || {
            n += 1;
            if n < 3 {
                return Err(ureq::Error::ConnectionFailed.into());
            }
            Ok(true)
        });
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(n, 3);
        // предел: в сообщении — последняя ошибка связи
        let r = until("опрос", Duration::ZERO, step, || {
            Err(ureq::Error::ConnectionFailed.into())
        });
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains("не дождались") && msg.contains("ошибка связи"),
            "{msg}"
        );
        // ошибка состояния — сразу, без повторов
        let mut n = 0;
        let r = until("опрос", Duration::from_secs(5), step, || {
            n += 1;
            bail!("extract_state = failed")
        });
        assert!(format!("{:#}", r.unwrap_err()).contains("failed"));
        assert_eq!(n, 1);
    }

    /// Рабочий каталог seed: чужой каталог не удаляется, свой (с маркером) — пересоздаётся и
    /// убирается при выходе.
    #[test]
    fn seed_work_only_own_dir() {
        let root = tmp("tc-seed-work");
        let foreign = root.join("foreign").join("seed-work");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("notes.txt"), "чужое").unwrap();
        let e = SeedWork::create(&foreign)
            .err()
            .expect("чужой каталог принят");
        assert!(format!("{e:#}").contains(SEED_MARK), "{e:#}");
        assert!(foreign.join("notes.txt").is_file(), "чужой каталог тронут");

        let own = root.join("own").join("seed-work");
        let w = SeedWork::create(&own).unwrap();
        assert!(own.join(SEED_MARK).is_file());
        fs::write(own.join("left.txt"), "остаток").unwrap();
        std::mem::forget(w); // прерванный seed: каталог остался
        let w = SeedWork::create(&own).unwrap();
        assert!(
            !own.join("left.txt").exists(),
            "остаток прерванного seed не убран"
        );
        drop(w);
        assert!(!own.exists(), "seed не убрал за собой");
        let _ = fs::remove_dir_all(&root);
    }

    // ------------------------------------------------------------ привязки: формы пути

    #[test]
    fn norm_and_percent_decode() {
        assert_eq!(norm(r"H:\\Zam//Data\x"), "h:/zam/data/x");
        assert_eq!(percent_decode("a%20b%D0%B4%zz%2"), "a bд%zz%2");
        assert_eq!(path_needle(r"\\?\H:\zam\data\").unwrap(), "h:/zam/data");
        assert_eq!(path_needle("/var/lib/zam//").unwrap(), "/var/lib/zam");
    }

    #[test]
    fn path_as_is_and_case_insensitive() {
        let n = win();
        assert!(n.find(r"см. H:\Zam Data\Данные\blobs").is_some());
        // регистр ячейки и иглы не важен, в том числе кириллица
        assert!(n.find(r"см. h:\zam data\данные").is_some());
        assert!(n.find(r"см. H:\ZAM DATA\ДАННЫЕ").is_some());
        let upper = Needles::new(&[r"H:\ZAM DATA\ДАННЫЕ"], &["win-box"]).unwrap();
        assert!(upper.find(r"h:\zam data\данные").is_some());
        assert!(upper.find("host WIN-BOX here").is_some());
    }

    #[test]
    fn path_forward_slashes() {
        assert!(win().find("см. H:/Zam Data/Данные/blobs").is_some());
    }

    #[test]
    fn path_json_escaped_backslashes() {
        // текст вне JSON с экранированием JSON: `\\`
        assert!(win().find(r"path=H:\\Zam Data\\Данные").is_some());
        // ячейка JSON: разбор сам снимает экранирование
        assert!(win().find(r#"{"p":"H:\\Zam Data\\Данные\\x"}"#).is_some());
    }

    #[test]
    fn path_double_escaped_backslashes() {
        // JSON в JSON: `\\\\`
        assert!(win().find(r"log: H:\\\\Zam Data\\\\Данные").is_some());
        let inner = json!({"p": r"H:\Zam Data\Данные"}).to_string();
        let outer = json!({"payload": inner}).to_string();
        assert!(outer.contains(r"H:\\\\Zam"), "{outer}");
        assert!(win().find(&outer).is_some());
    }

    #[test]
    fn path_mixed_slashes() {
        assert!(win().find(r"см. H:\Zam Data/Данные").is_some());
        assert!(win().find(r"см. H:/Zam Data\\Данные/x").is_some());
    }

    #[test]
    fn path_percent_encoded_file_url() {
        // пробел и кириллица в file://
        let url = "file:///H:/Zam%20Data/%D0%94%D0%B0%D0%BD%D0%BD%D1%8B%D0%B5/a.md";
        assert!(win().find(url).is_some());
        // шестнадцатеричные цифры в нижнем регистре, двоеточие и слэши кодированы
        let url = "file:///H%3a%5cZam%20Data%5c%d0%94%d0%b0%d0%bd%d0%bd%d1%8b%d0%b5";
        assert!(win().find(url).is_some());
        // и внутри JSON
        let cell = json!({"href": "file:///H:/Zam%20Data/%D0%94%D0%B0%D0%BD%D0%BD%D1%8B%D0%B5"});
        assert!(win().find(&cell.to_string()).is_some());
    }

    #[test]
    fn path_boundary_and_trailing_slash() {
        let n = Needles::new(&[r"H:\zam\data\"], &["WIN-BOX"]).unwrap();
        assert_eq!(n.paths(), ["h:/zam/data"]);
        assert!(n.find(r"H:\zam\data").is_some());
        assert!(n.find(r"H:\zam\data\blobs").is_some());
        // другой каталог с тем же началом имени — не привязка
        assert!(n.find(r"H:\zam\data2\blobs").is_none());
        assert!(n.find(r"H:\zam\database").is_none());
        assert!(n.find("ничего похожего").is_none());
    }

    #[test]
    fn path_empty_and_too_general_refused() {
        for p in ["", "  ", r"C:\", "/", "/var", r"\\?\D:\", "a/b"] {
            assert!(path_needle(p).is_err(), "{p:?}");
            assert!(Needles::new(&[p], &["WIN-BOX"]).is_err(), "{p:?}");
        }
        assert!(Needles::new::<&str, &str>(&[], &["WIN-BOX"]).is_err());
        let mut n = win();
        assert!(!n.add_path_if_specific(r"C:\"));
        assert!(n.add_path_if_specific(r"C:\ZeroAIMemory"));
        assert_eq!(n.paths().len(), 2);
    }

    // ------------------------------------------------------------ привязки: имя машины

    #[test]
    fn machine_word_boundaries() {
        let n = win();
        assert!(n.find("host=WIN-BOX;").is_some());
        assert!(n.find("win-box").is_some());
        assert!(n.find("с машины win-box.local").is_some());
        // часть другого слова — не привязка
        assert!(n.find("darwin-boxes").is_none());
        assert!(n.find("win-box-2").is_none());
        assert!(n.find("my_win-box").is_none());
    }

    #[test]
    fn machine_short_name_refused() {
        for m in ["", " ", "pc", "ПК"] {
            let e = machine_needle(m).expect_err(m);
            assert!(format!("{e:#}").contains("короче"), "{e:#}");
            assert!(Needles::new(&[r"H:\zam\data"], &[m]).is_err(), "{m:?}");
        }
        assert_eq!(machine_needle(" Box ").unwrap(), "box");
        let mut n = win();
        // само добавляется только приметное имя: от 4 символов и не из одних букв
        for m in ["pc", "dev", "runner", "Сервер", "a-1"] {
            assert!(!n.add_machine_if_distinct(m), "{m}");
        }
        assert!(n.add_machine_if_distinct("OGOUN-PC"));
        assert!(n.add_machine_if_distinct("fv-az1774-221"));
        assert!(n.add_machine_if_distinct("node7"));
        assert_eq!(
            n.machines(),
            ["fv-az1774-221", "node7", "ogoun-pc", "win-box"]
        );
    }

    /// Литеральные `\n`, `\t`, `\r` перед именем машины в сыром тексте — граница слова.
    #[test]
    fn machine_after_literal_escape() {
        let n = win();
        assert!(n.find(r"log: line1\nwin-box started").is_some());
        assert!(n.find(r"a\twin-box").is_some());
        assert!(n.find(r"a\rWIN-BOX b").is_some());
        // обрывок JSON (не разбирается): экранирование осталось литеральным
        assert!(n.find(r#""msg":"up\nwin-box ok", ..."#).is_some());
        // без обратной косой буква — часть слова
        assert!(n.find("nwin-box").is_none());
        assert!(n.find(r"a\xwin-box").is_none());
    }

    /// `\uXXXX` в неразобранном JSON и двойное процентное кодирование.
    #[test]
    fn raw_unicode_escapes_and_double_percent() {
        use super::bindings::unescape_unicode;
        assert_eq!(
            unescape_unicode(r"a\u005cb\u0414\uZZZZ\u00"),
            "a\\bД\\uZZZZ\\u00"
        );
        assert_eq!(unescape_unicode(r"\ud83d\ude00!"), "\u{1F600}!");
        let n = win();
        // обрывок JSON и JSONL: `\` как \u005c, кириллица как \uXXXX
        let frag =
            r#""p":"H:\u005cZam Data\u005c\u0414\u0430\u043d\u043d\u044b\u0435\u005cx", ..."#;
        assert!(n.find(frag).is_some());
        let jsonl = "{\"a\":1}\n{\"p\":\"H:\\u005CZam Data\\u005CДанные\"}";
        assert!(n.find(jsonl).is_some());
        assert!(n.find(r"host \u0057IN-BOX up").is_some());
        // двойное и тройное процентное кодирование
        assert!(n.find("file:///H:/Zam%2520Data/%25D0%2594анные").is_some());
        assert!(n.find("H%253A%255CZam%252520Data%255CДанные").is_some());
        assert!(n.find("win%252Dbox").is_some());
        // `\u` и процент вместе
        assert!(n.find(r"H:\u005cZam%2520Data\u005cДанные").is_some());
        assert!(n.find(r"H:\u005cZam%2520Other\u005cДанные").is_none());
    }

    /// Ожидание индекса: 5xx от `/search` — «ещё не готово», 4xx — ошибка.
    #[test]
    fn search_ready_treats_5xx_as_not_ready() {
        let r = |status, body: &str| Resp {
            status,
            body: body.as_bytes().to_vec(),
        };
        let ok = r#"{"items":[],"pending_count":0,"degraded":false}"#;
        assert!(search_ready(&r(200, ok)).unwrap());
        assert!(!search_ready(&r(200, r#"{"pending_count":3,"degraded":false}"#)).unwrap());
        assert!(!search_ready(&r(200, r#"{"pending_count":0,"degraded":"qdrant_down"}"#)).unwrap());
        for s in [500, 502, 503, 504] {
            assert!(!search_ready(&r(s, "busy")).unwrap(), "{s}");
        }
        for s in [400, 401, 403, 404] {
            assert!(search_ready(&r(s, "no")).is_err(), "{s}");
        }
    }

    /// Поля `machine` JSON — атрибуция на любой глубине, в том числе в JSON внутри строки;
    /// то же имя в другом поле или ключе — находка.
    #[test]
    fn json_machine_fields_are_attribution() {
        let n = win();
        assert!(n.find(r#"{"machine":"WIN-BOX","x":1}"#).is_none());
        assert!(n.find(r#"[{"a":{"machine":"win-box"}}]"#).is_none());
        let inner = json!({"machine": "WIN-BOX"}).to_string();
        assert!(n.find(&json!({"payload": inner}).to_string()).is_none());
        assert!(
            n.find(r#"{"machine":"WIN-BOX","host":"WIN-BOX"}"#)
                .is_some()
        );
        assert!(n.find(r#"{"WIN-BOX":1}"#).is_some());
        // экранированный перевод строки перед именем: в сыром тексте `\nwin-box` — не граница
        assert!(n.find(r#"{"note":"first\nWIN-BOX"}"#).is_some());
        // не JSON — сырой текст, атрибуции в нём нет
        assert!(n.find(r#"machine":"WIN-BOX" (обрывок)"#).is_some());
    }

    /// Находка показывает окрестность совпадения, а не начало ячейки.
    #[test]
    fn hit_shows_neighbourhood() {
        let text = format!(
            "{} перед H:\\Zam Data\\Данные\\blobs после {}",
            "начало ".repeat(100),
            "хвост ".repeat(100)
        );
        let (needle, ctx) = win().find(&text).unwrap();
        assert_eq!(needle, "h:/zam data/данные");
        assert!(
            ctx.contains("перед h:/zam data/данные/blobs после"),
            "{ctx}"
        );
        assert!(ctx.starts_with('…') && ctx.ends_with('…'), "{ctx}");
        assert!(ctx.chars().count() < 200, "{ctx}");
        // короткая ячейка — целиком, без многоточий
        let (_, ctx) = win().find("host WIN-BOX").unwrap();
        assert_eq!(ctx, "host win-box");
    }

    // ------------------------------------------------------------ привязки: обход баз

    #[test]
    fn bindings_skip_machine_and_find_paths() {
        let root = tmp("tc-bindings");
        fs::create_dir_all(root.join("users")).unwrap();
        let sys = rusqlite::Connection::open(root.join("system.db")).unwrap();
        sys.execute_batch(
            "CREATE TABLE audit(detail TEXT, machine TEXT);
             INSERT INTO audit VALUES ('{\"machine\":\"WIN-BOX\",\"x\":1}', 'WIN-BOX');",
        )
        .unwrap();
        let u = rusqlite::Connection::open(root.join("users").join("u.db")).unwrap();
        u.execute_batch(
            "CREATE TABLE nodes(path TEXT, note TEXT);
             INSERT INTO nodes VALUES ('a', 'see h:\\ZAM\\data\\blobs');
             INSERT INTO nodes VALUES ('b', 'чисто');
             CREATE TABLE blob_extract(sha TEXT, units BLOB);",
        )
        .unwrap();
        // BLOB: zstd(JSON) с путём — находка; zstd без пути, двоичный и UTF-8 с именем машины
        let packed = |t: &str| zstd::bulk::compress(t.as_bytes(), 3).unwrap();
        let ins = "INSERT INTO blob_extract VALUES (?1, ?2)";
        let unit = json!([{"text": r"opened H:\zam\data\blobs\ab\cd"}]).to_string();
        u.execute(ins, rusqlite::params!["z-hit", packed(&unit)])
            .unwrap();
        u.execute(ins, rusqlite::params!["z-clean", packed("[]")])
            .unwrap();
        u.execute(
            ins,
            rusqlite::params!["bin", vec![0xFFu8, 0xFE, 0x00, 0x80]],
        )
        .unwrap();
        u.execute(ins, rusqlite::params!["utf8", b"from win-box".to_vec()])
            .unwrap();
        drop((sys, u));
        let n = Needles::new(&[r"H:\zam\data"], &["WIN-BOX"]).unwrap();
        let s = scan(&root, &n).unwrap();
        assert_eq!(s.dbs, 2);
        assert_eq!(s.blob_cells, 3, "zstd ×2 и UTF-8; двоичный пропущен");
        assert_eq!(s.hits.len(), 3, "{:?}", s.hits);
        assert!(s.hits.iter().any(|h| h.contains("u.db nodes.note")));
        let z = s.hits.iter().find(|h| h.contains("blob_extract.units"));
        assert!(z.is_some_and(|h| h.contains("h:/zam/data")), "{:?}", s.hits);
        assert_eq!(
            s.hits
                .iter()
                .filter(|h| h.contains("blob_extract.units"))
                .count(),
            2,
            "{:?}",
            s.hits
        );
        assert!(!s.hits.iter().any(|h| h.contains("audit")), "{:?}", s.hits);
        let _ = fs::remove_dir_all(&root);
    }
}
