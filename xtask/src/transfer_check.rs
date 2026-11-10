//! Проверка переезда сервера на другую машину (M6 §12б.6, §17 п. 24; задача 18c):
//!
//! - `seed` — корпус на старом сервере: заметки, вложения PDF и DOCX, секрет Vault, идеи,
//!   отчёт с картинкой, карта кода (`zam code sync` на фикстуре `eval/sessions-corpus/code`);
//!   ожидания — в `expect.json` (id, sha256 содержимого и вложений, топ-5 на 10 запросах, имя
//!   и sha256 секрета, `model_id`, отпечатки нарезки);
//! - `verify` — те же данные на новом сервере после `restore` (или без ключа — `--no-key`:
//!   Vault заблокирован с понятной ошибкой, остальное работает);
//! - `bindings` — ни одна текстовая ячейка баз каталога данных, кроме атрибуции `machine`,
//!   не содержит прежнего `data_dir` и имени прежней машины (§12б.1; та же проверка — тест
//!   `restore_has_no_machine_bindings`).
//!
//! Запросы поиска идут без заголовков сессии и машины: выданное сессии понижается в выдаче, а
//! неявная сессия выбирается по машине (R13) — топ-5 сравнивается без этих поправок.

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
    /// Корпус на старом сервере и ожидания в expect.json
    Seed {
        /// Адрес сервера, например http://127.0.0.1:7700
        #[arg(long)]
        server: String,
        /// Токен пользователя (rw)
        #[arg(long)]
        token: String,
        /// Файл ожиданий
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
        #[arg(long)]
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
    /// Текстовые ячейки баз без прежнего data_dir и имени прежней машины (кроме `machine`)
    Bindings {
        /// Каталог данных (system.db, users/*.db)
        #[arg(long)]
        data_dir: PathBuf,
        /// Абсолютный путь data_dir старой машины
        #[arg(long)]
        old_path: String,
        /// Имя старой машины
        #[arg(long)]
        old_machine: String,
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
            let needles = needles(&old_path, &old_machine);
            let hits = bindings(&data_dir, &needles)?;
            if hits.is_empty() {
                println!(
                    "bindings: ok — в текстовых ячейках баз {} нет {needles:?} (кроме machine)",
                    data_dir.display()
                );
                return Ok(());
            }
            for h in &hits {
                println!("{h}");
            }
            bail!("bindings: {} ячеек с привязкой к старой машине", hits.len())
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
        let body = json!({"query": q, "top_k": 6, "scope": {"solution": SOLUTION}});
        let r = self.call(
            "POST",
            "/search",
            Some(("application/json", body.to_string().into_bytes())),
            false,
            None,
        )?;
        if r.status != 200 {
            bail!("POST /search «{q}»: HTTP {}: {}", r.status, r.text());
        }
        let v = r.json();
        let items = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                // элемент: фрагмент вложения — `id` (`a:…`), запись — `entry_id`, заметка — путь
                let id = ["id", "entry_id", "path"]
                    .iter()
                    .find_map(|k| i[*k].as_str().filter(|s| !s.is_empty()))
                    .unwrap_or_default()
                    .to_string();
                (id, i["score"].as_f64().unwrap_or_default())
            })
            .collect();
        Ok((items, v))
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

/// Ждёт `ok()` не дольше `limit`, опрос раз в `step`; `what` — для ошибки.
fn until(
    what: &str,
    limit: Duration,
    step: Duration,
    mut ok: impl FnMut() -> Result<bool>,
) -> Result<Duration> {
    let t0 = Instant::now();
    loop {
        if ok()? {
            return Ok(t0.elapsed());
        }
        if t0.elapsed() > limit {
            bail!("{what}: не дождались за {} с", limit.as_secs());
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
    if !matches!(r.status, 200 | 201) {
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
    let work = out.parent().unwrap_or(Path::new(".")).join("seed-work");
    let _ = fs::remove_dir_all(&work);
    code_sync(
        c,
        zam,
        &root.join("eval").join("sessions-corpus").join("code"),
        &work,
    )?;
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
        queries.push(QueryExp {
            query: (*q).to_string(),
            top,
        });
    }
    let ist = c.get("/index/status")?.json();
    Ok(Expect {
        created_at: jiff::Timestamp::now().to_string(),
        server_version: health["version"].as_str().unwrap_or_default().to_string(),
        machine: c.machine.clone().unwrap_or_default(),
        solution: SOLUTION.to_string(),
        model_id: ist["model"]["id"].as_str().map(String::from),
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
            let (_, v) = c.top6(QUERIES[0])?;
            Ok(v["pending_count"] == 0 && v["degraded"] == false)
        },
    )
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
    rep.check(
        "health",
        (|| {
            let h = c.get("/health")?;
            if h.status != 200 {
                bail!("HTTP {}", h.status);
            }
            Ok(format!("версия {}", h.json()["version"]))
        })(),
    );
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
    rep.check(
        "модель",
        (|| {
            let st = c.get("/index/status")?.json();
            let got = st["model"]["id"].as_str().map(String::from);
            if got != e.model_id {
                bail!("model_id {got:?}, ожидался {:?}", e.model_id);
            }
            Ok(format!(
                "{got:?}; reindex.last = {}; worker = {}",
                st["reindex"]["last"], st["worker"]
            ))
        })(),
    );
    if let Ok(st) = c.get("/index/status").map(|r| r.json()) {
        rep.numbers.insert("index_status".into(), st);
    }
    rep.check(
        "заметки",
        (|| {
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
        })(),
    );
    rep.check(
        "поиск: топ-5 на 10 запросах",
        (|| {
            for q in &e.queries {
                let (got, _) = c.top6(&q.query)?;
                same_top5(&q.query, &q.top, &got)?;
            }
            Ok(format!(
                "{} запросов: топ-5 те же, на тех же местах",
                e.queries.len()
            ))
        })(),
    );
    rep.check(
        "вложения",
        (|| {
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
        })(),
    );
    rep.check(
        "Vault",
        (|| {
            let r = c.get(&format!("/vault/{}?reveal=1", e.vault_id))?;
            if no_key {
                let p = r.json();
                if r.status != 503
                    || !p["type"]
                        .as_str()
                        .unwrap_or_default()
                        .ends_with("vault-locked")
                {
                    bail!(
                        "без ключа ожидался 503 vault-locked: HTTP {}: {}",
                        r.status,
                        r.text()
                    );
                }
                let l = c.get(&format!("/spaces/{}/vault", e.solution))?;
                let lv = l.json();
                if l.status != 200 || lv["locked"] != true {
                    bail!("список Vault без ключа: HTTP {}: {}", l.status, l.text());
                }
                return Ok(format!(
                    "заблокирован, как ожидалось: {} «{}»; список отвечает (locked: true)",
                    p["type"],
                    p["detail"]
                        .as_str()
                        .or(p["title"].as_str())
                        .unwrap_or_default()
                ));
            }
            if r.status != 200 {
                bail!("HTTP {}: {}", r.status, r.text());
            }
            let v = r.json();
            let secret = v["secret"]
                .as_str()
                .or(v["value"].as_str())
                .unwrap_or_default();
            if sha_hex(secret.as_bytes()) != e.vault_sha256 || v["title"] != e.vault_title.as_str()
            {
                bail!("секрет {} другой", e.vault_title);
            }
            Ok(format!(
                "секрет «{}» раскрыт, значение совпадает",
                e.vault_title
            ))
        })(),
    );
    rep.check(
        "идеи и записи",
        (|| {
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
        })(),
    );
    rep.check("отчёт", (|| {
        let Ok(pw) = std::env::var("ZAM_ADMIN_PASSWORD") else {
            bail!("нужен ZAM_ADMIN_PASSWORD (пароль администратора нового сервера): отчёт видит только человек");
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
        let url = item["first_image"]["url"].as_str().context("нет first_image")?;
        let a = agent_with(Duration::from_secs(30));
        let mut r = a.get(&format!("{}/{}", c.base, url.trim_start_matches('/'))).header("Cookie", &cookie).call()?;
        let st = r.status().as_u16();
        let b = r.body_mut().read_to_vec()?;
        if st != 200 || sha_hex(&b) != e.report_image_sha256 {
            bail!("картинка отчёта: HTTP {st}, {} байт", b.len());
        }
        Ok(format!("отчёт {} в ленте, картинка совпадает", e.report_id))
    })());
    rep.check(
        "карта кода",
        (|| {
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
        })(),
    );
    if let Some(d) = data_dir {
        rep.check(
            "integrity_check",
            (|| {
                let mut dbs = vec![d.join("system.db")];
                dbs.extend(user_dbs(d)?);
                for db in &dbs {
                    let c = rusqlite::Connection::open_with_flags(
                        db,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )?;
                    let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
                    if ok != "ok" {
                        bail!("{}: {ok}", db.display());
                    }
                }
                Ok(format!("{} баз — ok", dbs.len()))
            })(),
        );
        if !e.chunkers.is_empty() {
            rep.check(
                "отпечатки нарезки",
                (|| {
                    let got = chunkers(d)?;
                    for (db, m) in &e.chunkers {
                        if got.get(db) != Some(m) {
                            bail!("{db}: {:?}, ожидалось {m:?}", got.get(db));
                        }
                    }
                    Ok("meta.chunker* совпадают".to_string())
                })(),
            );
        }
    }
    rep
}

/// Топ-5 совпадают: те же пять элементов на тех же местах (§12б.6). Баллы не сравниваются:
/// это нормированные места слияния каналов (1,0; 0,984; 0,968 …), а не косинусы.
fn same_top5(q: &str, before: &[(String, f64)], after: &[(String, f64)]) -> Result<()> {
    let ids = |v: &[(String, f64)]| v.iter().take(5).map(|x| x.0.clone()).collect::<Vec<_>>();
    if ids(before) != ids(after) {
        bail!("«{q}»: топ-5 {:?} вместо {:?}", ids(after), ids(before));
    }
    Ok(())
}

// ---------------------------------------------------------------- bindings

/// Иглы привязки: путь (как есть, через `/`, с удвоенным `\` — экранирование JSON) и имя машины.
fn needles(old_path: &str, old_machine: &str) -> Vec<String> {
    let p = old_path.strip_prefix(r"\\?\").unwrap_or(old_path);
    let p = p.trim_end_matches(['\\', '/']);
    let mut v = vec![
        p.to_string(),
        p.replace('\\', "/"),
        p.replace('\\', r"\\"),
        old_machine.trim().to_string(),
    ];
    v.retain(|s| !s.is_empty());
    v.sort();
    v.dedup();
    v
}

/// Убирает из JSON все поля `machine` (атрибуция, как столбцы `machine`).
fn strip_machine(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("machine");
            m.values_mut().for_each(strip_machine);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_machine),
        _ => {}
    }
}

/// Текстовые ячейки `system.db` и `users/*.db` каталога `data_dir` с одной из `needles`,
/// кроме столбцов `machine` и полей `machine` JSON-ячеек; виртуальные таблицы пропускаются
/// (их данные — в теневых таблицах).
fn bindings(data_dir: &Path, needles: &[String]) -> Result<Vec<String>> {
    let mut dbs = vec![data_dir.join("system.db")];
    dbs.extend(user_dbs(data_dir)?);
    let found = |s: &str| needles.iter().any(|n| s.contains(n.as_str()));
    let mut hits = Vec::new();
    let mut cells = 0u64;
    for db in &dbs {
        let c =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| db.display().to_string())?;
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND coalesce(sql, '') NOT LIKE 'CREATE VIRTUAL%'",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for t in tables {
            let cols: Vec<String> = c
                .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for col in cols.iter().filter(|c| c.as_str() != "machine") {
                let mut st = c.prepare(&format!(
                    "SELECT \"{col}\" FROM \"{t}\" WHERE typeof(\"{col}\") = 'text'"
                ))?;
                let vals: Vec<String> = st
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                cells += vals.len() as u64;
                for s in vals.into_iter().filter(|s| found(s)) {
                    if let Ok(mut v) = serde_json::from_str::<Value>(&s) {
                        strip_machine(&mut v);
                        if !found(&v.to_string()) {
                            continue;
                        }
                    }
                    let head: String = s.chars().take(160).collect();
                    hits.push(format!(
                        "{} {t}.{col}: {head}",
                        db.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
        }
    }
    println!("bindings: {} баз, {cells} текстовых ячеек", dbs.len());
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needles_forms() {
        let n = needles(r"\\?\H:\zam\data\", "WIN-BOX");
        assert!(n.contains(&r"H:\zam\data".to_string()));
        assert!(n.contains(&"H:/zam/data".to_string()));
        assert!(n.contains(&r"H:\\zam\\data".to_string()));
        assert!(n.contains(&"WIN-BOX".to_string()));
    }

    #[test]
    fn top5_same_ids_same_places() {
        let r = |ids: &[&str]| ids.iter().map(|i| (i.to_string(), 0.0)).collect::<Vec<_>>();
        let a = r(&["a", "b", "c", "d", "e", "f"]);
        assert!(same_top5("q", &a, &a).is_ok());
        // шестое место в сравнение не входит
        assert!(same_top5("q", &a, &r(&["a", "b", "c", "d", "e", "z"])).is_ok());
        assert!(same_top5("q", &a, &r(&["a", "c", "b", "d", "e"])).is_err());
        assert!(same_top5("q", &a, &r(&["a", "b", "c", "d", "f"])).is_err());
        assert!(same_top5("q", &a, &r(&["a", "b", "c", "d"])).is_err());
    }

    #[test]
    fn bindings_skip_machine_and_find_paths() {
        // каталог теста — внутри target репозитория (CARGO_TARGET_TMPDIR у юнит-тестов нет)
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("xtask-tc-bindings");
        let _ = fs::remove_dir_all(&root);
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
             INSERT INTO nodes VALUES ('a', 'see H:\\zam\\data\\blobs');",
        )
        .unwrap();
        drop((sys, u));
        let hits = bindings(&root, &needles(r"H:\zam\data", "WIN-BOX")).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].contains("nodes.note"), "{hits:?}");
        let _ = fs::remove_dir_all(&root);
    }
}
