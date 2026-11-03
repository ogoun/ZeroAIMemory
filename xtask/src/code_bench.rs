//! Замеры карты кода M5.6 (спецификация `docs/specs/2026-11-03-m5.6-code-map.md` §17 п. 2, 3, 8):
//! `cargo xtask code-bench [--repo <путь>]`.
//!
//! release-сборки `zam` и `zam-server`, сервер без моделей (FTS) на свободном порту
//! ([`crate::smoke::Server`]), копия `HEAD` репозитория через `git archive` в
//! `.tmp/m56/bench/repo` (своя git-история из одного коммита — отпечатки по git, как у рабочей
//! копии). Сценарии: первая `zam code sync`, повтор без изменений, правка одного файла, удаление
//! одного файла, вторая копия рабочего каталога со сброшенным `code-state.json`;
//! `GET /code/map` с бюджетами 200/500/1000/2000; центральные символы против 10 ключевых типов
//! плана; холодный старт `zam --version` (p95 из 50); запуск фоновой синхронизации хуком
//! `session-start` (p95 из 20). Итог — таблица в stdout и `.tmp/m56/bench/report.json`; цели
//! п. 2 и п. 8 не достигнуты — ошибка (после записи отчёта), п. 3 (ключевые типы) — только
//! отчёт: промах разбирается в журнале и идёт вопросом владельцу (веса под список не
//! подгоняются).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::eval::run::{build_cli, build_server, qdrant_bin};
use crate::smoke::{Server, SmokeEnv, agent_with, scrub_zam_env, wait_health_for};

/// Решение замеров (GUID агента).
const SOLUTION: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9b56";
/// Ключевые типы плана (критерий §17 п. 3; список фиксирован до реализации): `(qual, путь)`.
pub const KEY_TYPES: [(&str, &str); 10] = [
    ("Id", "crates/zam-core/src/id.rs"),
    ("Db", "crates/zam-store/src/db.rs"),
    ("StoreError", "crates/zam-store/src/lib.rs"),
    ("ApiError", "crates/zam-server/src/error.rs"),
    ("AppState", "crates/zam-server/src/app.rs"),
    ("Principal", "crates/zam-server/src/auth/mod.rs"),
    ("Entry", "crates/zam-store/src/entries/mod.rs"),
    ("Node", "crates/zam-store/src/nodes/mod.rs"),
    ("SpaceKind", "crates/zam-store/src/user_db.rs"),
    ("ServerConfig", "crates/zam-server/src/config.rs"),
];
/// Порог п. 3: ключевых типов в первых [`CENTRAL_TOP`] центральных.
const KEY_TYPES_MIN: usize = 6;
const CENTRAL_TOP: usize = 15;
/// Бюджеты карты п. 3.
const BUDGETS: [usize; 4] = [200, 500, 1000, 2000];
/// Цели п. 2 и п. 8.
const FIRST_SYNC_MAX_S: f64 = 15.0;
const REPEAT_SYNC_MAX_S: f64 = 2.0;
const VERSION_P95_MAX_MS: f64 = 20.0;
const SPAWN_P95_MAX_MS: f64 = 10.0;
/// Повторов `zam --version` и запусков фоновой синхронизации.
const VERSION_RUNS: usize = 50;
const SPAWN_RUNS: usize = 20;
/// Прогревочных запусков до замера (не в выборке): первый запуск свежесобранного exe на Windows
/// включает его проверку антивирусом и чтение с HDD.
const WARMUP_RUNS: usize = 3;
/// Запусков каждого хука в замере с картой.
const HOOK_RUNS: usize = 20;
/// Эталоны экстрактора (развилка п. 3: карта без них).
const FIXTURES_GLOB: &str = "crates/zam-code/tests/fixtures/**";
/// Файл правки и удаления (есть в любом `HEAD` проекта): правка — комментарий в конце.
const EDIT_FILE: &str = "crates/zam-core/src/id.rs";
const REMOVE_FILE: &str = "xtask/src/changelog.rs";
const HEALTH_WAIT: Duration = Duration::from_secs(120);
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(clap::Args)]
pub struct CodeBenchArgs {
    /// Репозиторий для копии `HEAD` (по умолчанию — этот)
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

pub fn main(a: CodeBenchArgs, root: &Path) -> Result<()> {
    code_bench(&a.repo.unwrap_or_else(|| root.to_path_buf()))
}

/// Замеры на копии `HEAD` репозитория `repo`; сборки, каталог работы и отчёт — в этом
/// репозитории (`target/`, `.tmp/m56/bench/`).
pub fn code_bench(repo: &Path) -> Result<()> {
    code_bench_in(&crate::repo_root(), repo)
}

/// Результат одной `zam code sync --json`.
struct Sync {
    secs: f64,
    report: Value,
}

impl Sync {
    fn n(&self, k: &str) -> u64 {
        self.report[k].as_u64().unwrap_or(0)
    }
}

fn code_bench_in(root: &Path, repo: &Path) -> Result<()> {
    let work = root.join(".tmp").join("m56").join("bench");
    if work.exists() {
        fs::remove_dir_all(&work).with_context(|| format!("очистка {}", work.display()))?;
    }
    fs::create_dir_all(&work)?;
    let server_dir = build_server(root, None)?;
    let zam = build_cli(root, None)?;
    let copy = work.join("repo");
    let head = archive_head(repo, &copy, &work)?;
    let home = work.join("home");
    fs::create_dir_all(home.join(".zeroaimemory"))?;
    // фоновая синхронизация хуков замеров не нужна: только явные `zam code sync`
    fs::write(
        home.join(".zeroaimemory/config.toml"),
        "[code]\nauto = false\n",
    )?;
    project_files(&copy)?;

    // сервер без моделей (FTS-only), как `eval` без `--model`
    let mut env = SmokeEnv::for_model("none")?;
    let no_models = work.join("no-models");
    fs::create_dir_all(&no_models)?;
    env.vars.push((
        "ZAM_ML__MODELS_DIR".into(),
        no_models.to_string_lossy().into(),
    ));
    let qdrant = qdrant_bin(root);
    if qdrant.is_file() {
        env.vars
            .push(("ZAM_QDRANT__BINARY".into(), qdrant.to_string_lossy().into()));
    }
    let base = format!("http://{}", env.addr);
    let data = work.join("data");
    let mut srv = Server::start(
        &server_dir,
        &data,
        &env,
        &work.join("zam-server.stderr.log"),
        false,
    )?;
    wait_health_for(&mut srv, &base, HEALTH_WAIT)?;
    let token = srv
        .token_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| anyhow!("нет первого токена; stderr:\n{}", srv.log_tail()))?;
    let (code, v) = http(
        &base,
        &token,
        "POST",
        "/api/v1/solutions",
        Some(&json!({"agent_solution_id": SOLUTION, "name": "code-bench"})),
    )?;
    if code != 201 {
        bail!("регистрация решения: HTTP {code}: {v}");
    }
    let ctx = Ctx {
        zam: zam.clone(),
        home: home.clone(),
        base: base.clone(),
        token: token.clone(),
    };

    println!("копия HEAD {head}: {}", copy.display());
    let first = ctx.sync(&copy)?;
    println!(
        "первая sync: {:.2} с, файлов {}, загружено {}",
        first.secs,
        first.n("files"),
        first.n("uploaded")
    );
    let repeat = ctx.sync(&copy)?;
    // правка: комментарий в конце файла
    let edit = copy.join(EDIT_FILE);
    let mut text = fs::read_to_string(&edit).with_context(|| format!("{}", edit.display()))?;
    text.push_str("\n// code-bench: правка одного файла\n");
    fs::write(&edit, text)?;
    let edited = ctx.sync(&copy)?;
    let status_before = ctx.get("/api/v1/code/status", SOLUTION)?;
    // положительный контроль: до удаления файл в таблице карты есть
    let present = ctx.files_named(REMOVE_FILE)?;
    fs::remove_file(copy.join(REMOVE_FILE)).with_context(|| format!("удаление {REMOVE_FILE}"))?;
    let removed = ctx.sync(&copy)?;
    let status_after = ctx.get("/api/v1/code/status", SOLUTION)?;
    let gone = ctx.files_named(REMOVE_FILE)?;
    // вторая копия рабочего каталога со сброшенным состоянием
    let copy2 = work.join("repo2");
    copy_dir(&copy, &copy2)?;
    let state = copy2.join(".zeroaimemory/cache/code-state.json");
    fs::remove_file(&state).with_context(|| format!("удаление {}", state.display()))?;
    let second = ctx.sync(&copy2)?;

    // карта: первая после синхронизации (с пересчётом рангов), затем бюджеты
    let t = Instant::now();
    let (code, m) = http(
        &base,
        &token,
        "GET",
        &format!("/api/v1/code/map?solution={SOLUTION}&budget_tokens=1000"),
        None,
    )?;
    let first_map_ms = ms(t);
    if code != 200 {
        bail!("GET /code/map: HTTP {code}: {m}");
    }
    let status = ctx.wait_ranked()?;
    let mut maps = Vec::new();
    for b in BUDGETS {
        let mut times = Vec::new();
        let mut tokens = 0;
        for _ in 0..5 {
            let t = Instant::now();
            let (code, m) = http(
                &base,
                &token,
                "GET",
                &format!("/api/v1/code/map?solution={SOLUTION}&budget_tokens={b}"),
                None,
            )?;
            times.push(ms(t));
            if code != 200 {
                bail!("GET /code/map {b}: HTTP {code}: {m}");
            }
            tokens = m["tokens"].as_u64().unwrap_or(u64::MAX) as usize;
        }
        maps.push(json!({"budget": b, "tokens": tokens, "ok": tokens <= b,
                         "ms_p50": pct(&times, 50), "ms_max": pct(&times, 100)}));
    }
    let central = central_report(&ctx, &status)?;
    let hooks = hooks_with_map(&ctx, &copy, &work)?;
    // развилка для владельца (п. 3): та же карта без эталонов экстрактора — исключением
    // `[code] exclude` конфигурации проекта, без правки ранжирования; не гейт
    fs::write(
        copy.join(".zeroaimemory/config.toml"),
        format!("[code]\nexclude = [\"{FIXTURES_GLOB}\"]\n"),
    )?;
    let no_fixtures_sync = ctx.sync(&copy)?;
    let status_nf = ctx.wait_ranked()?;
    let central_nf = central_report(&ctx, &status_nf)?;
    // замеры старта клиента — без работающего рядом сервера замеров (Qdrant, фоновые задачи
    // на том же HDD): у агента сервер обычно на другой машине
    srv.stop()?;
    let version = version_p95(&zam)?;
    let spawn = spawn_p95(&zam, &work)?;

    let checks = [
        (
            "первая sync ≤ 15 с",
            first.secs <= FIRST_SYNC_MAX_S && first.n("uploaded") == first.n("files"),
        ),
        (
            "повтор ≤ 2 с и 0 загруженных",
            repeat.secs <= REPEAT_SYNC_MAX_S && repeat.n("uploaded") == 0,
        ),
        ("правка — загружен 1", edited.n("uploaded") == 1),
        (
            "удаление — файл исчез из карты",
            removed.n("removed") == 1
                && present == 1
                && gone == 0
                && status_after["files"].as_u64() == status_before["files"].as_u64().map(|n| n - 1),
        ),
        ("вторая копия — загружено 0", second.n("uploaded") == 0),
        (
            "хуки с картой — p95 в бюджете, строка карты и «### Код» в каждом выводе",
            hooks.as_array().is_some_and(|a| {
                a.iter().all(|h| {
                    h["ok"] == true
                        && (h["with_expected"].is_null() || h["with_expected"] == HOOK_RUNS)
                })
            }),
        ),
        (
            "карта ≤ бюджета (200/500/1000/2000)",
            maps.iter().all(|m| m["ok"] == true),
        ),
        (
            "zam --version p95 ≤ 20 мс",
            version["p95_ms"].as_f64().unwrap_or(f64::MAX) <= VERSION_P95_MAX_MS,
        ),
        (
            "запуск фоновой синхронизации p95 ≤ 10 мс",
            spawn["p95_ms"].as_f64().unwrap_or(f64::MAX) <= SPAWN_P95_MAX_MS,
        ),
    ];
    let row = |s: &Sync| {
        json!({"secs": round2(s.secs), "files": s.n("files"), "need": s.n("need"),
               "uploaded": s.n("uploaded"), "removed": s.n("removed")})
    };
    let report = json!({
        "repo": repo.display().to_string(),
        "head": head,
        "platform": std::env::consts::OS,
        "sync": {
            "first": row(&first),
            "repeat": row(&repeat),
            "edit_one": row(&edited),
            "remove_one": row(&removed),
            "second_copy": row(&second),
            "removed_file_rows_before": present,
            "removed_file_rows_after": gone,
        },
        "map": {"first_after_sync_ms": round2(first_map_ms), "budgets": maps},
        "status": {"files": status["files"], "symbols": status["symbols"],
                   "by_lang": status["by_lang"], "skipped_total": status["skipped_total"]},
        "central": central,
        "central_without_fixtures": {"exclude": FIXTURES_GLOB, "sync": row(&no_fixtures_sync),
                                     "files": status_nf["files"], "report": central_nf},
        "hooks_with_map": hooks,
        "zam_version": version,
        "background_spawn": spawn,
        "checks": checks.iter().map(|(k, v)| json!({"check": k, "ok": v})).collect::<Vec<_>>(),
    });
    let out = work.join("report.json");
    fs::write(&out, serde_json::to_string_pretty(&report)?)?;
    print_table(&report);
    println!("отчёт: {}", out.display());
    let failed: Vec<&str> = checks
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(k, _)| *k)
        .collect();
    if !failed.is_empty() {
        bail!("цели не достигнуты: {}", failed.join("; "));
    }
    Ok(())
}

/// Клиент замеров: `zam` с адресом и токеном сервера, «домашний» каталог замеров.
struct Ctx {
    zam: PathBuf,
    home: PathBuf,
    base: String,
    token: String,
}

impl Ctx {
    fn zam(&self, dir: &Path) -> Command {
        let mut c = Command::new(&self.zam);
        scrub_zam_env(&mut c, &[])
            .env("ZAM_URL", &self.base)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("CLAUDECODE")
            .current_dir(dir)
            .stdin(Stdio::null());
        c
    }

    /// `zam code sync --json` в каталоге `dir`: время и отчёт.
    fn sync(&self, dir: &Path) -> Result<Sync> {
        let t = Instant::now();
        let mut child = self
            .zam(dir)
            .args(["code", "sync", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("запуск zam code sync")?;
        crate::procjob::adopt(&child);
        let deadline = Instant::now() + SYNC_TIMEOUT;
        while child.try_wait()?.is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                bail!("zam code sync дольше {} с", SYNC_TIMEOUT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let secs = t.elapsed().as_secs_f64();
        let o = child.wait_with_output()?;
        if !o.status.success() {
            bail!(
                "zam code sync: {}\n{}{}",
                o.status,
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
        }
        let report: Value = serde_json::from_slice(&o.stdout).with_context(|| {
            format!(
                "zam code sync --json: не JSON: {}",
                String::from_utf8_lossy(&o.stdout)
            )
        })?;
        Ok(Sync { secs, report })
    }

    fn get(&self, path: &str, solution: &str) -> Result<Value> {
        let (code, v) = http(
            &self.base,
            &self.token,
            "GET",
            &format!("{path}?solution={solution}"),
            None,
        )?;
        if code != 200 {
            bail!("GET {path}: HTTP {code}: {v}");
        }
        Ok(v)
    }

    /// Строк файла `path` в таблице файлов карты (`GET /code/files`, поле `files`): 0 или 1.
    fn files_named(&self, path: &str) -> Result<usize> {
        let (code, v) = http(
            &self.base,
            &self.token,
            "GET",
            &format!(
                "/api/v1/code/files?solution={SOLUTION}&q={}&limit=50",
                path.replace('/', "%2F")
            ),
            None,
        )?;
        if code != 200 {
            bail!("GET /code/files: HTTP {code}: {v}");
        }
        let files = v["files"]
            .as_array()
            .with_context(|| format!("GET /code/files: нет массива files: {v}"))?;
        Ok(files.iter().filter(|f| f["path"] == path).count())
    }

    /// Состояние карты после пересчёта рангов.
    fn wait_ranked(&self) -> Result<Value> {
        let t = Instant::now();
        loop {
            let s = self.get("/api/v1/code/status", SOLUTION)?;
            if s["rank_state"] == "ok" {
                return Ok(s);
            }
            if t.elapsed() > Duration::from_secs(120) {
                bail!("ранги не пересчитаны за 120 с: {s}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Копия `HEAD` репозитория `repo` в `dest` (`git archive`) и git-история из одного коммита;
/// возвращает короткий `HEAD` исходного репозитория.
fn archive_head(repo: &Path, dest: &Path, work: &Path) -> Result<String> {
    let head = git(repo, &["rev-parse", "--short=12", "HEAD"])?;
    let tar = work.join("head.tar");
    git(
        repo,
        &[
            "archive",
            "--format=tar",
            "-o",
            &tar.to_string_lossy(),
            "HEAD",
        ],
    )?;
    fs::create_dir_all(dest)?;
    tar::Archive::new(fs::File::open(&tar)?).unpack(dest)?;
    fs::remove_file(&tar)?;
    git(dest, &["init", "-q"])?;
    git(dest, &["add", "-A"])?;
    git(
        dest,
        &[
            "-c",
            "user.name=code-bench",
            "-c",
            "user.email=code-bench@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "HEAD",
        ],
    )?;
    Ok(head)
}

/// Файлы проекта клиента: решение замеров, локальный кэш вне git.
fn project_files(dir: &Path) -> Result<()> {
    let z = dir.join(".zeroaimemory");
    fs::create_dir_all(&z)?;
    fs::write(
        z.join("solution.toml"),
        format!("agent_solution_id = \"{SOLUTION}\"\nname = \"code-bench\"\n"),
    )?;
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !o.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        fs::create_dir_all(&dst)?;
        for e in fs::read_dir(&src)? {
            let e = e?;
            let t = e.file_type()?;
            if t.is_dir() {
                stack.push((e.path(), dst.join(e.file_name())));
            } else if t.is_file() {
                fs::copy(e.path(), dst.join(e.file_name()))?;
            }
        }
    }
    Ok(())
}

/// Запрос к API: (код, JSON или `Null`).
fn http(
    base: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let a = agent_with(Duration::from_secs(120));
    let url = format!("{base}{path}");
    let auth = format!("Bearer {token}");
    let mut r = match (method, body) {
        ("GET", _) => a.get(&url).header("Authorization", &auth).call()?,
        (_, b) => a
            .post(&url)
            .header("Authorization", &auth)
            .content_type("application/json")
            .send(b.map(Value::to_string).unwrap_or_default())?,
    };
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    let v = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).with_context(|| format!("{path}: не JSON: {text}"))?
    };
    Ok((code, v))
}

/// Центральные символы против ключевых типов: первые 10 `GET /code/status` (не больше трёх из
/// файла — ответ сервера) и первые 15 по рангу символа среди всех символов карты в JSON
/// (`budget_tokens = 20000`, без ограничения на файл).
fn central_report(ctx: &Ctx, status: &Value) -> Result<Value> {
    let key = |qual: &str, path: &str| KEY_TYPES.iter().any(|(q, p)| *q == qual && *p == path);
    let status_top: Vec<Value> = status["central"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            let (q, p) = (
                c["qual"].as_str().unwrap_or(""),
                c["path"].as_str().unwrap_or(""),
            );
            json!({"qual": q, "path": p, "kind": c["kind"], "rank": c["rank"], "key": key(q, p)})
        })
        .collect();
    let (code, m) = http(
        &ctx.base,
        &ctx.token,
        "GET",
        &format!("/api/v1/code/map?solution={SOLUTION}&format=json&budget_tokens=20000"),
        None,
    )?;
    if code != 200 {
        bail!("GET /code/map json: HTTP {code}: {m}");
    }
    let mut all: Vec<(f64, String, String, String)> = Vec::new();
    for f in m["files"].as_array().into_iter().flatten() {
        let path = f["path"].as_str().unwrap_or("").to_string();
        for s in f["symbols"].as_array().into_iter().flatten() {
            all.push((
                s["rank"].as_f64().unwrap_or(0.0),
                s["qual"].as_str().unwrap_or("").to_string(),
                path.clone(),
                s["kind"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    all.sort_by(|a, b| b.0.total_cmp(&a.0));
    let top: Vec<Value> = all
        .iter()
        .take(CENTRAL_TOP)
        .map(|(r, q, p, k)| json!({"qual": q, "path": p, "kind": k, "rank": r, "key": key(q, p)}))
        .collect();
    let hits = |v: &[Value]| v.iter().filter(|c| c["key"] == true).count();
    // место каждого ключевого типа среди всех символов карты
    let places: Vec<Value> = KEY_TYPES
        .iter()
        .map(|(q, p)| {
            let place = all.iter().position(|(_, aq, ap, _)| aq == q && ap == p);
            json!({"qual": q, "path": p, "place": place.map(|i| i + 1)})
        })
        .collect();
    let top_hits = hits(&top);
    Ok(json!({
        "status_top10": status_top,
        "status_hits": hits(&status_top),
        "top15": top,
        "top15_hits": top_hits,
        "symbols_in_map_json": all.len(),
        "key_type_places": places,
        "threshold": KEY_TYPES_MIN,
        "ok": top_hits >= KEY_TYPES_MIN,
    }))
}

/// Холодный старт `zam --version`: p50/p95 из [`VERSION_RUNS`].
fn version_p95(zam: &Path) -> Result<Value> {
    let mut t = Vec::new();
    for i in 0..WARMUP_RUNS + VERSION_RUNS {
        let t0 = Instant::now();
        let o = Command::new(zam)
            .arg("--version")
            .stdin(Stdio::null())
            .output()?;
        if i >= WARMUP_RUNS {
            t.push(ms(t0));
        }
        if !o.status.success() {
            bail!("zam --version: {}", o.status);
        }
    }
    let size = fs::metadata(zam)?.len();
    Ok(
        json!({"runs": VERSION_RUNS, "warmup": WARMUP_RUNS, "p50_ms": pct(&t, 50),
              "p95_ms": pct(&t, 95), "max_ms": pct(&t, 100), "binary_bytes": size}),
    )
}

/// Хуки с загруженной картой этого репозитория против живого сервера замеров (§17 п. 8
/// «`hook_budgets` зелёный с включённой картой»): `session-start` (строка карты в дайджесте),
/// `prompt`, `pre-tool` и `post-tool` (успех) на `Read`, `subagent-start` с задачей про
/// существующую функцию (раздел `### Код`). Конфигурация клиента — по умолчанию (`code_map`
/// и `code.auto` включены; `HEAD` копии не меняется — фоновая синхронизация стартует только
/// при первом `session-start`, он — прогревочный). Время — весь процесс хука, с прогревом.
fn hooks_with_map(ctx: &Ctx, proj: &Path, work: &Path) -> Result<Value> {
    let home = work.join("hooks-home");
    fs::create_dir_all(home.join(".zeroaimemory"))?;
    let run = |kind: &str, ev: &Value| -> Result<(f64, String)> {
        let mut c = Command::new(&ctx.zam);
        scrub_zam_env(&mut c, &[])
            .args(["hook", kind, "--agent", "claude"])
            .env("ZAM_URL", &ctx.base)
            .env("ZAM_TOKEN", &ctx.token)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("CLAUDECODE")
            .current_dir(proj)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let t0 = Instant::now();
        let mut child = c.spawn().with_context(|| format!("zam hook {kind}"))?;
        {
            use std::io::Write as _;
            let mut si = child.stdin.take().context("stdin хука")?;
            si.write_all(ev.to_string().as_bytes())?;
        }
        let o = child.wait_with_output()?;
        let d = ms(t0);
        let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
        let ctx_text = serde_json::from_str::<Value>(&out)
            .ok()
            .and_then(|v| {
                v["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Ok((d, ctx_text))
    };
    let ev = |event: &str, session: &str, extra: Value| {
        let mut v = json!({"session_id": session, "cwd": proj,
                           "transcript_path": proj.join("t.jsonl"), "hook_event_name": event});
        for (k, x) in extra.as_object().into_iter().flatten() {
            v[k] = x.clone();
        }
        v
    };
    let read_file = proj.join("crates/zam-store/src/db.rs");
    let task = "Почини code_sync_manifest в zam-store: падает на пустом манифесте";
    let mut rows = Vec::new();
    for (kind, budget, want) in [
        ("session-start", 300.0, Some("ZeroAIMemory: карта кода:")),
        ("prompt", 250.0, None),
        ("pre-tool", 50.0, None),
        ("post-tool", 50.0, None),
        ("subagent-start", 300.0, Some("### Код")),
    ] {
        let (mut times, mut hits) = (Vec::new(), 0usize);
        for i in 0..WARMUP_RUNS + HOOK_RUNS {
            let sess = format!("cb-{kind}-{i}");
            let e = match kind {
                "session-start" => ev("SessionStart", &sess, json!({"source": "startup"})),
                "prompt" => ev(
                    "UserPromptSubmit",
                    &sess,
                    json!({"prompt": "почему падает синхронизация карты"}),
                ),
                "pre-tool" => ev(
                    "PreToolUse",
                    &sess,
                    json!({"tool_name": "Read", "tool_input": {"file_path": read_file}}),
                ),
                "post-tool" => ev(
                    "PostToolUse",
                    &sess,
                    json!({"tool_name": "Read", "tool_use_id": "u1",
                           "tool_input": {"file_path": read_file},
                           "tool_response": {"success": true}}),
                ),
                _ => {
                    // очередь запуска субагента — `pre-tool` на `Agent` (вне замера)
                    let spawn = ev(
                        "PreToolUse",
                        &sess,
                        json!({"tool_name": "Agent", "tool_use_id": "u2",
                               "tool_input": {"description": "d", "prompt": task,
                                              "subagent_type": "Plan"}}),
                    );
                    run("pre-tool", &spawn)?;
                    ev(
                        "SubagentStart",
                        &sess,
                        json!({"agent_id": format!("ag-{i}"), "agent_type": "Plan"}),
                    )
                }
            };
            let (d, text) = run(kind, &e)?;
            if i >= WARMUP_RUNS {
                times.push(d);
                if want.is_some_and(|w| text.contains(w)) {
                    hits += 1;
                }
            }
        }
        let p95 = pct(&times, 95);
        rows.push(
            json!({"hook": kind, "runs": HOOK_RUNS, "p50_ms": pct(&times, 50),
                         "p95_ms": p95, "max_ms": pct(&times, 100), "budget_ms": budget,
                         "ok": p95 <= budget, "expect": want,
                         "with_expected": want.map(|_| hits)}),
        );
    }
    Ok(Value::from(rows))
}

/// Запуск фоновой синхронизации хуком `session-start`: время запуска из `hooks.log` (строка
/// «фоновая синхронизация карты запущена за N мс»), каждый раз со сменой `HEAD`; адрес сервера
/// не настроен — запущенный процесс сразу выходит, без замка и сети (как тест хуков).
fn spawn_p95(zam: &Path, work: &Path) -> Result<Value> {
    let proj = work.join("hook").join("repo");
    let home = work.join("hook").join("home");
    fs::create_dir_all(proj.join(".git/refs/heads"))?;
    fs::create_dir_all(home.join(".zeroaimemory"))?;
    project_files(&proj)?;
    let mut hook_ms = Vec::new();
    for i in 0..WARMUP_RUNS + SPAWN_RUNS {
        fs::write(
            proj.join(".git/HEAD"),
            format!("{:040x}\n", 0x5eed_0000 + i),
        )?;
        let ev = json!({"session_id": format!("bench-{i}"), "cwd": proj,
                        "transcript_path": proj.join("t.jsonl"),
                        "hook_event_name": "SessionStart", "source": "startup"});
        let mut c = Command::new(zam);
        scrub_zam_env(&mut c, &[])
            .args(["hook", "session-start", "--agent", "claude"])
            .env("ZAM_URL", "no-url")
            .env("ZAM_TOKEN", "zam_bench")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .current_dir(&proj)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let t0 = Instant::now();
        let mut child = c.spawn()?;
        {
            use std::io::Write as _;
            let mut si = child.stdin.take().context("stdin хука")?;
            si.write_all(ev.to_string().as_bytes())?;
        }
        child.wait()?;
        if i >= WARMUP_RUNS {
            hook_ms.push(ms(t0));
        }
    }
    let log = fs::read_to_string(proj.join(".zeroaimemory/logs/hooks.log")).unwrap_or_default();
    let all: Vec<f64> = log
        .lines()
        .filter_map(|l| {
            let rest = l.split("фоновая синхронизация карты запущена за ").nth(1)?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .collect();
    let spawns: Vec<f64> = all.iter().skip(WARMUP_RUNS).copied().collect();
    if all.len() != WARMUP_RUNS + SPAWN_RUNS {
        bail!(
            "запусков фоновой синхронизации {} из {}; hooks.log:\n{log}",
            all.len(),
            WARMUP_RUNS + SPAWN_RUNS
        );
    }
    Ok(
        json!({"runs": SPAWN_RUNS, "warmup": WARMUP_RUNS, "p50_ms": pct(&spawns, 50),
              "p95_ms": pct(&spawns, 95), "max_ms": pct(&spawns, 100),
              "hook_p95_ms": pct(&hook_ms, 95)}),
    )
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Процентиль `p` (ближайший ранг) в мс, округлённый до сотых.
fn pct(v: &[f64], p: usize) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    if s.is_empty() {
        return 0.0;
    }
    let i = (s.len() * p).div_ceil(100).clamp(1, s.len()) - 1;
    round2(s[i])
}

fn print_table(r: &Value) {
    let s = &r["sync"];
    println!();
    println!("| Замер | Значение | Цель |");
    println!("|---|---|---|");
    let sync_row = |name: &str, v: &Value, goal: &str| {
        println!(
            "| {name} | {} с; файлов {}, need {}, загружено {}, удалено {} | {goal} |",
            v["secs"], v["files"], v["need"], v["uploaded"], v["removed"]
        );
    };
    sync_row("первая `zam code sync`", &s["first"], "≤ 15 с");
    sync_row("повтор без изменений", &s["repeat"], "≤ 2 с, 0 загружено");
    sync_row("правка одного файла", &s["edit_one"], "загружен 1");
    sync_row("удаление одного файла", &s["remove_one"], "исчез из карты");
    sync_row(
        "вторая копия, сброшен `code-state.json`",
        &s["second_copy"],
        "0 загружено",
    );
    println!(
        "| первая карта после sync (пересчёт рангов) | {} мс | — |",
        r["map"]["first_after_sync_ms"]
    );
    for m in r["map"]["budgets"].as_array().into_iter().flatten() {
        println!(
            "| `GET /code/map` бюджет {} | {} ток., p50 {} мс | ≤ бюджета |",
            m["budget"], m["tokens"], m["ms_p50"]
        );
    }
    let c = &r["central"];
    println!(
        "| ключевые типы в первых 15 центральных (ранг символа) | {}/10 | ≥ 6 |",
        c["top15_hits"]
    );
    println!(
        "| ключевые типы в 10 центральных `GET /code/status` | {}/10 | — |",
        c["status_hits"]
    );
    for h in r["hooks_with_map"].as_array().into_iter().flatten() {
        let expect = match h["expect"].as_str() {
            Some(w) => format!("; «{w}» в {}/{}", h["with_expected"], h["runs"]),
            None => String::new(),
        };
        println!(
            "| хук `{}` с картой ({}) | p50 {} мс, p95 {} мс{expect} | ≤ {} мс |",
            h["hook"].as_str().unwrap_or(""),
            h["runs"],
            h["p50_ms"],
            h["p95_ms"],
            h["budget_ms"]
        );
    }
    let nf = &r["central_without_fixtures"];
    println!(
        "| развилка: без `{}` (файлов {}) — ключевые типы в первых 15 | {}/10 | ≥ 6 |",
        nf["exclude"].as_str().unwrap_or(""),
        nf["files"],
        nf["report"]["top15_hits"]
    );
    let v = &r["zam_version"];
    println!(
        "| `zam --version` (50) | p50 {} мс, p95 {} мс, бинарник {} байт | p95 ≤ 20 мс |",
        v["p50_ms"], v["p95_ms"], v["binary_bytes"]
    );
    let b = &r["background_spawn"];
    println!(
        "| запуск фоновой синхронизации (20) | p50 {} мс, p95 {} мс (хук p95 {} мс) | p95 ≤ 10 мс |",
        b["p50_ms"], b["p95_ms"], b["hook_p95_ms"]
    );
    println!();
    println!("первые 15 центральных по рангу символа:");
    for (i, t) in c["top15"].as_array().into_iter().flatten().enumerate() {
        let mark = if t["key"] == true {
            " ← ключевой"
        } else {
            ""
        };
        println!(
            "{:>2}. {} ({}, {}){mark}",
            i + 1,
            t["qual"].as_str().unwrap_or(""),
            t["kind"].as_str().unwrap_or(""),
            t["path"].as_str().unwrap_or("")
        );
    }
    println!("развилка без эталонов — первые 15:");
    for (i, t) in nf["report"]["top15"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let mark = if t["key"] == true {
            " ← ключевой"
        } else {
            ""
        };
        println!(
            "{:>2}. {} ({}, {}){mark}",
            i + 1,
            t["qual"].as_str().unwrap_or(""),
            t["kind"].as_str().unwrap_or(""),
            t["path"].as_str().unwrap_or("")
        );
    }
    println!(
        "места ключевых типов среди {} символов JSON-карты (бюджет 20000; null — за её пределами):",
        c["symbols_in_map_json"]
    );
    for k in c["key_type_places"].as_array().into_iter().flatten() {
        println!("  {} — {}", k["qual"].as_str().unwrap_or(""), k["place"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(pct(&v, 95), 19.0);
        assert_eq!(pct(&v, 50), 10.0);
        assert_eq!(pct(&v, 100), 20.0);
        assert_eq!(pct(&[], 95), 0.0);
    }
}
