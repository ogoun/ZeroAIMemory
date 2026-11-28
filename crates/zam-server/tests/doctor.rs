//! `zam-server doctor [--full] [--fix] [--json]` (M7.4 §7.5): проверки каталога данных, только
//! чтение без `--fix` (и при запущенном сервере), безопасные исправления с `--fix` на
//! остановленном, коды выхода. Каталог данных собирается сервером в процессе теста; Qdrant —
//! `edge` (без процесса и сетевых обращений).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::Value;
use zam_core::Id;
use zam_server::config::{CheckMode, QdrantMode, ServerConfig};
use zam_server::doctor::{DoctorError, DoctorOpts, Level, Report, run};

fn edge(c: &mut ServerConfig) {
    c.qdrant.mode = QdrantMode::Edge;
}

async fn stop(a: Api) {
    a.st.shutdown().await;
    drop(a);
    // фоновые задачи открытия баз отпускают `Db` чуть позже
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Остановленный сервер: заметка администратора с вложением и неразрешённой wikilink.
struct Fx {
    cfg: ServerConfig,
    user: Id,
    node: String,
    sha: String,
}

impl Fx {
    fn db(&self) -> PathBuf {
        self.cfg
            .data_dir
            .join("users")
            .join(format!("{}.db", self.user))
    }

    fn blobs(&self) -> PathBuf {
        self.cfg.data_dir.join("blobs").join(self.user.to_string())
    }

    fn blob(&self, sha: &str) -> PathBuf {
        self.blobs().join(&sha[..2]).join(sha)
    }

    fn rw(&self) -> rusqlite::Connection {
        rw(&self.db())
    }

    fn code(&self, what: &str) -> String {
        format!("{what}:{}", self.user)
    }
}

fn rw(p: &Path) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(p).unwrap();
    c.busy_timeout(Duration::from_secs(5)).unwrap();
    c
}

async fn started(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> (Api, String, String) {
    let a = api(name, |c| {
        edge(c);
        tweak(c);
    })
    .await;
    let n = a
        .note(
            "knowledge",
            "docs/a",
            "# Сборка\n\nКоманда cargo build, см. [[Нет такой заметки]].\n",
        )
        .await;
    let node = n["id"].as_str().unwrap().to_string();
    let data: Vec<u8> = (0..3000usize).map(|i| (i * 31 % 251) as u8).collect();
    let j = common::attach::upload(&a, &a.token, &node, "a.bin", &data).await;
    let sha = j["sha256"].as_str().unwrap().to_string();
    (a, node, sha)
}

async fn fixture(name: &str) -> Fx {
    let (a, node, sha) = started(name, |_| {}).await;
    let cfg = (*a.st.cfg).clone();
    let user = a.st.admin_id;
    stop(a).await;
    Fx {
        cfg,
        user,
        node,
        sha,
    }
}

async fn doctor(cfg: &ServerConfig, fix: bool) -> Report {
    run(cfg, DoctorOpts { full: false, fix }, None)
        .await
        .unwrap()
}

/// Есть строка уровня `level` с кодом `code`, текст которой содержит `needle`.
fn has(r: &Report, level: Level, code: &str, needle: &str) -> bool {
    r.checks
        .iter()
        .any(|c| c.level == level && c.code == code && c.text.contains(needle))
}

fn errors_of<'a>(r: &'a Report, code: &'a str) -> Vec<&'a str> {
    r.checks
        .iter()
        .filter(|c| c.code == code && c.level == Level::Error)
        .map(|c| c.text.as_str())
        .collect()
}

/// `mtime` файла — `hours` часов назад.
fn age(p: &Path, hours: f64) {
    let t = SystemTime::now() - Duration::from_secs_f64(hours * 3600.0);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn zam_server(args: &[&str]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zam-server"));
    c.args(args);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
        c.env_remove(k);
    }
    c.output().expect("запуск zam-server")
}

fn write_conf(dir: &Path, c: &ServerConfig) -> String {
    let conf = dir.join("zam.toml");
    std::fs::write(&conf, toml::to_string(c).unwrap()).unwrap();
    conf.to_string_lossy().into_owned()
}

/// Остановленный сервер без поломок: ошибок нет, код 0; все разделы проверены; `--full` —
/// `integrity_check`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_clean_ok() {
    let f = fixture("dr-clean").await;
    let r = doctor(&f.cfg, false).await;
    assert_eq!(r.errors, 0, "{}", r.text());
    assert_eq!(r.exit_code(), 0);
    for code in [
        "data_dir".to_string(),
        "disk".into(),
        "db:system".into(),
        "vault_key".into(),
        "qdrant".into(),
        "models".into(),
        "backup".into(),
        f.code("db"),
        f.code("blobs"),
        f.code("links"),
    ] {
        assert!(r.of(&code).next().is_some(), "{code}:\n{}", r.text());
    }
    assert!(
        has(&r, Level::Ok, "data_dir", "сервер остановлен"),
        "{}",
        r.text()
    );
    assert!(has(&r, Level::Ok, &f.code("db"), "v27"), "{}", r.text());
    assert!(has(&r, Level::Ok, "vault_key", "подходит"), "{}", r.text());
    assert!(has(&r, Level::Ok, &f.code("blobs"), "1"), "{}", r.text());
    assert!(
        has(&r, Level::Info, &f.code("links"), "wikilink"),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Warn, "backup", "no_recent_backup"),
        "{}",
        r.text()
    );
    let full = run(
        &f.cfg,
        DoctorOpts {
            full: true,
            fix: false,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(full.errors, 0, "{}", full.text());
    assert!(
        has(&full, Level::Ok, &f.code("db"), "integrity_check ok"),
        "{}",
        full.text()
    );
}

/// JSON — `{checks: [{code, level, text, data}], errors, warnings}`; числа сходятся со строками.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_json_shape() {
    let f = fixture("dr-json").await;
    std::fs::remove_file(f.blob(&f.sha)).unwrap();
    let r = doctor(&f.cfg, false).await;
    let v = serde_json::to_value(&r).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["checks", "errors", "warnings"]);
    let checks = v["checks"].as_array().unwrap();
    assert!(!checks.is_empty());
    let (mut e, mut w) = (0, 0);
    for c in checks {
        let o = c.as_object().unwrap();
        let mut k: Vec<&str> = o.keys().map(String::as_str).collect();
        k.sort();
        assert_eq!(k, vec!["code", "data", "level", "text"], "{c}");
        assert!(c["code"].is_string() && c["text"].is_string(), "{c}");
        match c["level"].as_str().unwrap() {
            "error" => e += 1,
            "warn" => w += 1,
            "ok" | "info" => {}
            other => panic!("уровень {other}"),
        }
    }
    assert_eq!(
        (v["errors"].as_u64(), v["warnings"].as_u64()),
        (Some(e), Some(w))
    );
    assert!(e >= 1, "строка блоба без файла — error");
}

/// Файлы каталога данных: путь → (размер, `mtime`), без `-shm` (индекс WAL читатели обновляют)
/// и журналов.
fn snapshot(root: &Path) -> BTreeMap<String, (u64, SystemTime)> {
    fn walk(d: &Path, root: &Path, out: &mut BTreeMap<String, (u64, SystemTime)>) {
        let Ok(rd) = std::fs::read_dir(d) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with("logs") || rel.ends_with("-shm") {
                continue;
            }
            if p.is_dir() {
                walk(&p, root, out);
            } else if let Ok(m) = std::fs::metadata(&p) {
                out.insert(rel, (m.len(), m.modified().unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Сервер запущен: `doctor` без `--fix` работает, ничего не записывает (файлы баз и каталога —
/// те же размеры и `mtime`), ложных ошибок нет.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_read_only_with_server_running() {
    let (a, _, _) = started("dr-running", |_| {}).await;
    let cfg = (*a.st.cfg).clone();
    // сервер затих: фоновые записи (checkpoint раз в секунду) закончились — снимок не меняется
    // два интервала подряд дольше периода checkpoint
    let mut before = snapshot(&cfg.data_dir);
    let mut same = 0;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let now = snapshot(&cfg.data_dir);
        same = if now == before { same + 1 } else { 0 };
        before = now;
        if same == 2 {
            break;
        }
    }
    assert_eq!(same, 2, "сервер не затих");
    let r = doctor(&cfg, false).await;
    let after = snapshot(&cfg.data_dir);
    assert_eq!(before, after, "doctor без --fix ничего не пишет");
    assert_eq!(r.errors, 0, "{}", r.text());
    assert!(
        has(&r, Level::Info, "data_dir", "сервер запущен"),
        "{}",
        r.text()
    );
    stop(a).await;
}

/// `--fix` при запущенном сервере — отказ «остановите сервер», ничего не тронуто.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_fix_refuses_running_server() {
    let (a, _, _) = started("dr-fix-running", |_| {}).await;
    let cfg = (*a.st.cfg).clone();
    let staging = cfg.data_dir.join(".restore-staging");
    std::fs::create_dir_all(&staging).unwrap();
    let e = run(
        &cfg,
        DoctorOpts {
            full: false,
            fix: true,
        },
        None,
    )
    .await;
    match e {
        Err(e @ DoctorError::Busy) => assert!(e.to_string().contains("остановите сервер")),
        Ok(r) => panic!("ожидался отказ: {}", r.text()),
    }
    assert!(staging.is_dir());
    stop(a).await;
}

/// Строка блоба без файла — `error`; файл в `.gc/` (сбой посреди удаления) — «вернёт --fix», и
/// `--fix` возвращает его.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_missing_blob_file_error() {
    let f = fixture("dr-missing").await;
    let gc = f.blobs().join(".gc");
    std::fs::create_dir_all(&gc).unwrap();
    std::fs::rename(f.blob(&f.sha), gc.join(&f.sha)).unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, &f.code("blobs")).join("\n");
    assert!(
        errs.contains("без файла") && errs.contains(&f.sha[..16]),
        "{}",
        r.text()
    );
    assert!(errs.contains("вернёт doctor --fix"), "{}", r.text());
    assert_eq!(r.exit_code(), 1);

    let r = doctor(&f.cfg, true).await;
    assert_eq!(r.errors, 0, "{}", r.text());
    assert!(f.blob(&f.sha).is_file());
    assert!(!gc.join(&f.sha).exists());

    std::fs::remove_file(f.blob(&f.sha)).unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, &f.code("blobs")).join("\n");
    assert!(
        errs.contains("без файла") && !errs.contains("вернёт"),
        "{}",
        r.text()
    );
}

/// Сумма записанной миграции расходится с программой — `error` с подсказкой `rollback`/`restore`;
/// `--fix` её не правит.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_checksum_changed_error() {
    let f = fixture("dr-sum").await;
    f.rw()
        .execute(
            "UPDATE zam_migrations SET sha256 = ?1 WHERE version = 3",
            [&"0".repeat(64)],
        )
        .unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, &f.code("db")).join("\n");
    assert!(
        errs.contains("v3") && errs.contains("изменились"),
        "{}",
        r.text()
    );
    assert!(errs.contains("zam-server rollback") && errs.contains("zam-server restore"));
    // у базы с изменённой миграцией `--fix` не трогает и блобы: старый `.part` остаётся
    let inc = f.blobs().join(".incoming");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::write(inc.join("old.part"), b"partial").unwrap();
    age(&inc.join("old.part"), 30.0);
    let r = doctor(&f.cfg, true).await;
    assert!(!errors_of(&r, &f.code("db")).is_empty(), "{}", r.text());
    assert!(
        has(
            &r,
            Level::Info,
            &format!("fix:blobs:{}", f.user),
            "пропущены"
        ),
        "{}",
        r.text()
    );
    assert!(inc.join("old.part").is_file());
}

/// Дрейф схемы (нет индекса) — `error` с различием.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_schema_drift_error() {
    let f = fixture("dr-drift").await;
    f.rw().execute_batch("DROP INDEX edges_src").unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, &f.code("db")).join("\n");
    assert!(errs.contains("нет индекса edges_src"), "{}", r.text());
}

/// Индекс FTS5 расходится с хранимым текстом (правка теневой таблицы мимо индекса) — `error`;
/// `--fix` пересобирает индекс (`rebuild`), ошибок нет, поиск видит хранимый текст.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_fts_corrupt_fixed() {
    let f = fixture("dr-fts").await;
    let count = |word: &str| -> i64 {
        f.rw()
            .query_row(
                "SELECT count(*) FROM vault_fts WHERE vault_fts MATCH ?1",
                [word],
                |r| r.get(0),
            )
            .unwrap()
    };
    let c = f.rw();
    c.execute(
        "INSERT INTO vault_fts (item_id, title, open_text) VALUES ('v-1', 'заголовок', 'текст')",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE vault_fts_content SET c1 = 'подмена' WHERE c0 = 'v-1'",
        [],
    )
    .unwrap();
    drop(c);
    assert_eq!(count("подмена"), 0);
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, &f.code("db")).join("\n");
    assert!(
        errs.contains("FTS5") && errs.contains("vault_fts"),
        "{}",
        r.text()
    );
    let r = doctor(&f.cfg, true).await;
    assert!(
        has(&r, Level::Info, &format!("fix:db:{}", f.user), "vault_fts"),
        "{}",
        r.text()
    );
    assert_eq!(r.errors, 0, "{}", r.text());
    let r = doctor(&f.cfg, false).await;
    assert_eq!(r.errors, 0, "{}", r.text());
    assert_eq!(count("подмена"), 1);
}

/// Мастер-ключа нет — «мастер-ключа нет»; другой ключ — «не подходит»; оба `error`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_vault_key_missing_vs_mismatch() {
    use base64::Engine;
    let f = fixture("dr-vault").await;
    let key = f.cfg.data_dir.join("master.key");
    let saved = std::fs::read(&key).unwrap();
    std::fs::remove_file(&key).unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, "vault_key").join("\n");
    assert!(errs.contains("мастер-ключа нет"), "{}", r.text());

    let other = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    std::fs::write(&key, other).unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, "vault_key").join("\n");
    assert!(errs.contains("не подходит"), "{}", r.text());

    std::fs::write(&key, saved).unwrap();
    let r = doctor(&f.cfg, false).await;
    assert!(has(&r, Level::Ok, "vault_key", "подходит"), "{}", r.text());
}

/// Признак незавершённого восстановления — `error` с `restore`; отката — с `rollback`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_restore_incomplete_error() {
    let f = fixture("dr-incomplete").await;
    let flag = f.cfg.data_dir.join(zam_store::RESTORE_INCOMPLETE_FILE);
    std::fs::write(&flag, "restore 4242 2026-11-28T00:00:00Z\n").unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, "data_dir").join("\n");
    assert!(errs.contains("zam-server restore"), "{}", r.text());
    assert_eq!(r.exit_code(), 1);

    std::fs::write(
        &flag,
        "rollback 4242 2026-11-28T00:00:00Z\nmoved_to pre-rollback-x\n",
    )
    .unwrap();
    let r = doctor(&f.cfg, false).await;
    let errs = errors_of(&r, "data_dir").join("\n");
    assert!(errs.contains("zam-server rollback"), "{}", r.text());
    assert!(!errs.contains("zam-server restore"), "{}", r.text());
}

/// Явная связь живой заметки к несуществующей цели — `warn` с примером; неразрешённая wikilink —
/// `info`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_broken_edges_warn() {
    let f = fixture("dr-edges").await;
    let c = f.rw();
    let space: String = c
        .query_row("SELECT space_id FROM nodes WHERE id = ?1", [&f.node], |r| {
            r.get(0)
        })
        .unwrap();
    let ghost = Id::new().to_string();
    c.execute(
        "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id, type, origin,
                            created_at)
         VALUES (?1, ?2, 'node', ?3, 'node', ?4, 'relates_to', 'explicit', ?5)",
        rusqlite::params![
            Id::new().to_string(),
            space,
            f.node,
            ghost,
            zam_store::time::fmt_ts(zam_store::time::now())
        ],
    )
    .unwrap();
    let r = doctor(&f.cfg, false).await;
    assert!(
        has(&r, Level::Warn, &f.code("links"), &ghost),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Warn, &f.code("links"), ": 1"),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Info, &f.code("links"), "wikilink"),
        "{}",
        r.text()
    );
    assert_eq!(r.errors, 0, "{}", r.text());
}

/// Открытая регрессия последнего ночного прогона — `warn` с вердиктом; при `check = nightly` и
/// активных пробах без законченного прогона двое суток — `warn`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_check_regression_warn() {
    let mut f = fixture("dr-check").await;
    let c = f.rw();
    let now = zam_store::time::now();
    let at = |h: i64| zam_store::time::fmt_ts(now - jiff::SignedDuration::from_hours(h));
    let run_id = Id::new().to_string();
    c.execute(
        "INSERT INTO check_runs (id, trigger_kind, night, status, started_at, finished_at,
                                 verdict, flags)
         VALUES (?1, 'nightly', '2026-11-27', 'done', ?2, ?3, ?4, '[]')",
        rusqlite::params![
            run_id,
            at(2),
            at(1),
            r#"{"kind":"regression","metric":"search_hit","delta":-0.2}"#
        ],
    )
    .unwrap();
    let r = doctor(&f.cfg, false).await;
    assert!(
        has(&r, Level::Warn, &f.code("check"), "регрессия")
            && has(&r, Level::Warn, &f.code("check"), &run_id),
        "{}",
        r.text()
    );

    // принятая норма три дня назад, проба активна, ночная проверка включена — прогона нет
    c.execute(
        "UPDATE check_runs SET started_at = ?1, accepted_at = ?1, accepted_by = 'admin'",
        [at(72)],
    )
    .unwrap();
    let space: String = c
        .query_row("SELECT space_id FROM nodes WHERE id = ?1", [&f.node], |r| {
            r.get(0)
        })
        .unwrap();
    c.execute(
        "INSERT INTO probes (id, space_id, question, kind, split, author_kind, created_at)
         VALUES (?1, ?2, 'как собрать проект?', 'manual', 'dev', 'human', ?3)",
        rusqlite::params![Id::new().to_string(), space, at(100)],
    )
    .unwrap();
    f.cfg.works.check = CheckMode::Nightly;
    let r = doctor(&f.cfg, false).await;
    assert!(
        has(&r, Level::Warn, &f.code("check"), "двое суток"),
        "{}",
        r.text()
    );
    assert!(
        !has(&r, Level::Warn, &f.code("check"), "регрессия"),
        "{}",
        r.text()
    );
    f.cfg.works.check = CheckMode::Off;
    let r = doctor(&f.cfg, false).await;
    assert!(
        !has(&r, Level::Warn, &f.code("check"), "двое суток"),
        "{}",
        r.text()
    );
}

/// `--fix`: файл из `.gc/` — строке, мусор старше `blob_gc_days` и `.part` старше суток удалены,
/// остатки `.restore-staging` и брошенный признак копии убраны, аудит `blobs.gc`; после — без
/// ошибок.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_fix_gc_and_gc_dir() {
    let (a, node, sha) = started("dr-fix", |_| {}).await;
    let junk: Vec<u8> = (0..1234usize).map(|i| (i * 7 % 253) as u8).collect();
    let j = common::attach::upload(&a, &a.token, &node, "junk.bin", &junk).await;
    let junk_sha = j["sha256"].as_str().unwrap().to_string();
    let r = a
        .t(
            Method::DELETE,
            &format!("/api/v1/nodes/{node}/attachments/{junk_sha}/junk.bin"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let f = Fx {
        cfg: (*a.st.cfg).clone(),
        user: a.st.admin_id,
        node,
        sha,
    };
    stop(a).await;
    let root = f.cfg.data_dir.clone();
    age(&f.blob(&junk_sha), 24.0 * 8.0);
    let gc = f.blobs().join(".gc");
    std::fs::create_dir_all(&gc).unwrap();
    std::fs::rename(f.blob(&f.sha), gc.join(&f.sha)).unwrap();
    let inc = f.blobs().join(".incoming");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::write(inc.join("old.part"), b"partial").unwrap();
    age(&inc.join("old.part"), 30.0);
    std::fs::create_dir_all(root.join(".restore-staging").join("users")).unwrap();
    let flag = zam_store::backup_flag_path(&root);
    std::fs::write(&flag, r#"{"pid":4242,"started_at":"2026-11-27T00:00:00Z"}"#).unwrap();
    age(&flag, 10.0);

    let r = doctor(&f.cfg, false).await;
    assert!(!errors_of(&r, &f.code("blobs")).is_empty(), "{}", r.text());
    assert!(
        has(&r, Level::Info, &f.code("blobs"), "к удалению сборщиком: 1"),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Warn, "data_dir", ".restore-staging"),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Warn, "data_dir", "брошенный признак"),
        "{}",
        r.text()
    );

    let r = doctor(&f.cfg, true).await;
    assert_eq!(r.errors, 0, "{}", r.text());
    let fixed = format!("fix:blobs:{}", f.user);
    assert!(has(&r, Level::Info, &fixed, "строк 1"), "{}", r.text());
    assert!(
        has(&r, Level::Info, &fixed, "возвращено из .gc/: 1"),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Info, "fix:data_dir", ".restore-staging"),
        "{}",
        r.text()
    );
    assert!(
        has(&r, Level::Info, "fix:data_dir", "признак копии"),
        "{}",
        r.text()
    );
    assert!(f.blob(&f.sha).is_file());
    assert!(!f.blob(&junk_sha).exists());
    assert!(!inc.join("old.part").exists());
    assert!(!root.join(".restore-staging").exists());
    assert!(!flag.exists());
    let n: i64 = f
        .rw()
        .query_row(
            "SELECT count(*) FROM blobs WHERE sha256 = ?1",
            [&junk_sha],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
    let details: String = rw(&root.join("system.db"))
        .query_row(
            "SELECT details FROM audit WHERE action = 'blobs.gc' ORDER BY ts DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: Value = serde_json::from_str(&details).unwrap();
    assert_eq!(
        (v["rows"].as_u64(), v["by"].as_str()),
        (Some(1), Some("doctor")),
        "{v}"
    );
    let r = doctor(&f.cfg, false).await;
    assert_eq!(r.errors, 0, "{}", r.text());
}

/// Коды выхода команды: 0 — без ошибок (текст и `--json`), 1 — есть ошибки, 2 — конфигурация не
/// прочитана.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_exit_codes() {
    let f = fixture("dr-codes").await;
    let dir = f.cfg.data_dir.parent().unwrap().to_path_buf();
    let conf = write_conf(&dir, &f.cfg);
    let out = zam_server(&["doctor", "--config", &conf]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("итого: ошибок 0"), "{text}");
    assert!(
        text.lines().any(|l| l.starts_with("ok data_dir: ")),
        "{text}"
    );

    let out = zam_server(&["doctor", "--config", &conf, "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["errors"], 0, "{v}");

    std::fs::write(
        f.cfg.data_dir.join(zam_store::RESTORE_INCOMPLETE_FILE),
        "restore 1 2026-11-28T00:00:00Z\n",
    )
    .unwrap();
    let out = zam_server(&["doctor", "--config", &conf]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let bad = dir.join("bad.toml");
    std::fs::write(&bad, "[server\nbind = ").unwrap();
    let out = zam_server(&["doctor", "--config", &bad.to_string_lossy()]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("конфигурация не прочитана"));
}
