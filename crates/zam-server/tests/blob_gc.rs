//! Сборщик мусора блобов (M7.4 §7.4): что удаляется и что нет, возраст по `mtime` файла, замок
//! блобов пользователя, двухфазное удаление через `.gc/`, пропуск во время копии, копия без
//! мусора. Возраст — `set_modified` файлов и момент `now` прохода, без ожидания.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use axum::http::{Method, StatusCode};
use common::api::{Api, api, enc};
use jiff::{SignedDuration, Timestamp};
use zam_core::Id;
use zam_server::backup::runner::RunBy;
use zam_server::backup::{BackupOpts, RestoreOpts, backup, restore};
use zam_server::maintenance::gc_blobs;
use zam_store::blob_gc::{self, GC_DIR};
use zam_store::test_hooks::{pause_gc_after_plan, set_gc_fail};
use zam_store::{BlobStore, Db, GcReport, USER_MIGRATIONS};

const DAYS: u32 = 7;

/// Сервер с заметкой администратора; блобы — его.
struct W {
    a: Api,
    user: Id,
    store: BlobStore,
    node: String,
}

async fn world_cfg(name: &str, tweak: impl FnOnce(&mut zam_server::config::ServerConfig)) -> W {
    let a = api(name, tweak).await;
    let user = a.st.admin_id;
    let store = BlobStore::new(&a.st.data_dir.blobs_dir(), user);
    let n = a.note("knowledge", "docs/gc", "# Сборщик\n").await;
    let node = n["id"].as_str().unwrap().to_string();
    W {
        a,
        user,
        store,
        node,
    }
}

async fn world(name: &str) -> W {
    world_cfg(name, |_| {}).await
}

impl W {
    async fn db(&self) -> Db {
        self.a.st.users.get(self.user).await.unwrap()
    }

    async fn upload(&self, name: &str, data: &[u8]) -> String {
        let j = common::attach::upload(&self.a, &self.a.token, &self.node, name, data).await;
        j["sha256"].as_str().unwrap().to_string()
    }

    async fn detach(&self, sha: &str, name: &str) {
        let r = self
            .a
            .t(
                Method::DELETE,
                &format!(
                    "/api/v1/nodes/{}/attachments/{sha}/{}",
                    self.node,
                    enc(name)
                ),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    }

    async fn refcount(&self, sha: &str) -> Option<i64> {
        refcount(&self.db().await, sha).await
    }

    fn file(&self, sha: &str) -> PathBuf {
        self.store.path_of(sha)
    }

    fn root(&self) -> &Path {
        self.store.root()
    }

    async fn sweep_at(&self, now: Timestamp) -> GcReport {
        blob_gc::sweep(&self.db().await, self.root(), now, DAYS)
            .await
            .unwrap()
    }

    async fn sweep(&self) -> GcReport {
        self.sweep_at(Timestamp::now()).await
    }

    /// Загруженный и откреплённый блоб, файл которого не менялся `days` суток.
    async fn garbage(&self, name: &str, data: &[u8], days: f64) -> String {
        let sha = self.upload(name, data).await;
        self.detach(&sha, name).await;
        age(&self.file(&sha), days);
        sha
    }
}

async fn refcount(db: &Db, sha: &str) -> Option<i64> {
    let sha = sha.to_string();
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            "SELECT refcount FROM blobs WHERE sha256 = ?1",
            [&sha],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
    .unwrap()
}

/// `mtime` файла — `days` суток назад.
fn age(p: &Path, days: f64) {
    let t = SystemTime::now() - Duration::from_secs_f64(days * 86_400.0);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn bytes(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed))
        .collect()
}

fn in_days(d: i64) -> Timestamp {
    Timestamp::now() + SignedDuration::from_hours(24 * d)
}

fn files_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Строка `refcount = 0` с файлом старше `blob_gc_days` — строка, файл и документ открепленной
/// пары удаляются; свежий мусор остаётся.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_removes_zero_refcount_after_age() {
    let w = world("gc-zero").await;
    let d = bytes(1, 3000);
    let sha = w.upload("a.bin", &d).await;
    w.detach(&sha, "a.bin").await;
    assert_eq!(w.refcount(&sha).await, Some(0));
    let r = w.sweep().await;
    assert_eq!((r.rows, r.files), (0, 0), "{r:?}");
    assert!(w.file(&sha).is_file());

    age(&w.file(&sha), 8.0);
    let r = w.sweep().await;
    assert_eq!((r.rows, r.files, r.bytes), (1, 1, d.len() as u64), "{r:?}");
    assert_eq!(w.refcount(&sha).await, None);
    assert!(!w.file(&sha).exists());
    assert!(files_in(&w.root().join(GC_DIR)).is_empty());
    let s = sha.clone();
    let docs: i64 = w
        .db()
        .await
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM attachment_docs WHERE blob_sha256 = ?1",
                [&s],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(docs, 0, "документ открепленной пары удалён вместе с блобом");
}

/// Открепление последней ссылки ставит файлу `mtime` «сейчас»: давно загруженный блоб,
/// откреплённый только что, ждёт `blob_gc_days` от открепления.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_keeps_recently_detached() {
    let w = world("gc-recent").await;
    let d = bytes(2, 2000);
    let sha = w.upload("old.bin", &d).await;
    age(&w.file(&sha), 30.0);
    let before = SystemTime::now() - Duration::from_secs(5);
    w.detach(&sha, "old.bin").await;
    let mtime = std::fs::metadata(w.file(&sha)).unwrap().modified().unwrap();
    assert!(mtime >= before, "mtime обновлён при отвязке");
    let r = w.sweep().await;
    assert_eq!((r.rows, r.files), (0, 0), "{r:?}");
    assert_eq!(w.refcount(&sha).await, Some(0));
    assert!(w.file(&sha).is_file());
    // через восемь суток — мусор
    let r = w.sweep_at(in_days(8)).await;
    assert_eq!((r.rows, r.files), (1, 1), "{r:?}");
    assert!(!w.file(&sha).exists());
}

/// Файл без строки (недописанная загрузка) удаляется по возрасту; запись того же содержимого
/// обновляет `mtime` уже бывшего файла.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_orphan_file_by_age() {
    let w = world("gc-orphan").await;
    let put = |d: Vec<u8>| {
        let s = w.store.clone();
        async move {
            let _share = s.share().await;
            s.put(&d[..], 1 << 20).await.unwrap().0
        }
    };
    let sha = put(bytes(3, 1500)).await;
    assert_eq!(w.refcount(&sha).await, None);
    let r = w.sweep().await;
    assert_eq!(r.files, 0, "{r:?}");
    age(&w.file(&sha), 8.0);
    let r = w.sweep().await;
    assert_eq!((r.rows, r.files, r.bytes), (0, 1, 1500), "{r:?}");
    assert!(!w.file(&sha).exists());

    let again = put(bytes(4, 900)).await;
    age(&w.file(&again), 8.0);
    assert_eq!(put(bytes(4, 900)).await, again);
    let r = w.sweep().await;
    assert_eq!(
        r.files, 0,
        "повторная запись того же содержимого — файл свежий: {r:?}"
    );
    assert!(w.file(&again).is_file());
}

/// `.incoming/*.part` старше суток удаляются, свежие и не `.part` — нет.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_incoming_parts_after_day() {
    let w = world("gc-parts").await;
    let inc = w.root().join(".incoming");
    std::fs::create_dir_all(&inc).unwrap();
    for (n, d) in [("old.part", 1.1), ("fresh.part", 0.5), ("keep.tmp", 3.0)] {
        std::fs::write(inc.join(n), b"partial").unwrap();
        age(&inc.join(n), d);
    }
    let r = w.sweep().await;
    assert_eq!((r.parts, r.files, r.bytes), (1, 1, 7), "{r:?}");
    assert_eq!(files_in(&inc), vec!["fresh.part", "keep.tmp"]);
}

/// Строка `refcount = 0` без файла (восстановленная копия) удаляется; строка со ссылкой без файла
/// — нет (её показывает `doctor`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_zero_row_without_file_removed() {
    let w = world("gc-dangling").await;
    let zero = w.upload("zero.bin", &bytes(5, 800)).await;
    w.detach(&zero, "zero.bin").await;
    std::fs::remove_file(w.file(&zero)).unwrap();
    let live = w.upload("live.bin", &bytes(6, 700)).await;
    std::fs::remove_file(w.file(&live)).unwrap();
    let r = w.sweep().await;
    assert_eq!((r.rows, r.files), (1, 0), "{r:?}");
    assert_eq!(w.refcount(&zero).await, None);
    assert_eq!(w.refcount(&live).await, Some(1));
}

/// Пока идёт копия (признак копии или замок копий процесса), проход пропускается целиком; потом
/// удаляет и пишет аудит `blobs.gc`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_skips_while_backup_flag() {
    let w = world("gc-backup").await;
    let sha = w.garbage("g.bin", &bytes(7, 1200), 8.0).await;
    let dir = w.a.st.cfg.data_dir.clone();
    let hold = zam_store::hold_backup_flag(&dir).unwrap();
    let p = gc_blobs(&w.a.st, Timestamp::now()).await;
    assert_eq!(p.skipped, Some("backup"), "{p:?}");
    assert!(w.file(&sha).is_file());
    drop(hold);

    let g = w.a.st.backups.try_start(RunBy::Schedule).unwrap();
    let p = gc_blobs(&w.a.st, Timestamp::now()).await;
    assert_eq!(p.skipped, Some("backup"), "{p:?}");
    assert!(w.file(&sha).is_file());
    g.release();

    let p = gc_blobs(&w.a.st, Timestamp::now()).await;
    assert_eq!(
        (p.skipped, p.users, p.rows, p.files),
        (None, 1, 1, 1),
        "{p:?}"
    );
    assert!(!w.file(&sha).exists());
    let sys = rusqlite::Connection::open_with_flags(
        dir.join("system.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let details: String = sys
        .query_row(
            "SELECT details FROM audit WHERE action = 'blobs.gc' ORDER BY ts DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&details).unwrap();
    assert_eq!(
        (&v["users"], &v["rows"], &v["files"], &v["bytes"]),
        (&1.into(), &1.into(), &1.into(), &1200.into()),
        "{v}"
    );
}

/// Разделяемый замок блобов у писателя — пользователь пропускается до следующего прохода.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_skips_user_while_writer_holds_gate() {
    let w = world("gc-gate").await;
    let sha = w.garbage("g.bin", &bytes(8, 600), 8.0).await;
    let share = w.store.share().await;
    let r = w.sweep().await;
    assert!(r.busy, "{r:?}");
    assert_eq!((r.rows, r.files), (0, 0));
    assert!(w.file(&sha).is_file());
    let p = gc_blobs(&w.a.st, Timestamp::now()).await;
    assert_eq!((p.busy, p.rows), (1, 0), "{p:?}");
    drop(share);
    let r = w.sweep().await;
    assert_eq!((r.busy, r.rows, r.files), (false, 1, 1), "{r:?}");
}

/// Между отбором и удалением тот же блоб привязан снова: файл на месте, `refcount = 1`, отдаётся.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_attach_race_keeps_file() {
    let w = world("gc-race").await;
    let d = bytes(9, 2500);
    let sha = w.garbage("a.bin", &d, 8.0).await;
    let pause = pause_gc_after_plan(w.root());
    let (db, root) = (w.db().await, w.root().to_path_buf());
    let task =
        tokio::spawn(async move { blob_gc::sweep(&db, &root, Timestamp::now(), DAYS).await });
    pause.reached.await.unwrap();
    assert_eq!(w.upload("b.bin", &d).await, sha);
    pause.release.send(()).unwrap();
    let r = task.await.unwrap().unwrap();
    assert_eq!((r.rows, r.files), (0, 0), "{r:?}");
    assert!(r.kept >= 1, "{r:?}");
    assert!(w.file(&sha).is_file());
    assert_eq!(w.refcount(&sha).await, Some(1));
    let r =
        w.a.t(Method::GET, &format!("/api/v1/blobs/{sha}/b.bin"), None)
            .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body.as_ref(), &d[..]);
}

/// «Сбой процесса» между переименованием в `.gc/` и `COMMIT`: строка жива, файл в `.gc/` —
/// следующий проход возвращает его на место. Сбой после `COMMIT`: строки нет — файл удаляется.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_crash_between_rename_and_commit_recovers() {
    let w = world("gc-crash").await;
    let d = bytes(10, 1800);
    let sha = w.garbage("c.bin", &d, 8.0).await;
    let parked = w.root().join(GC_DIR).join(&sha);
    let db = w.db().await;

    set_gc_fail(w.root(), Some("after_rename"));
    let e = blob_gc::sweep(&db, w.root(), Timestamp::now(), DAYS).await;
    set_gc_fail(w.root(), None);
    assert!(e.is_err(), "{e:?}");
    assert_eq!(
        w.refcount(&sha).await,
        Some(0),
        "транзакция не зафиксирована"
    );
    assert!(!w.file(&sha).exists());
    assert!(parked.is_file());
    let p = db
        .read({
            let root = w.root().to_path_buf();
            move |c| blob_gc::plan(c, &root, Timestamp::now(), DAYS)
        })
        .await
        .unwrap();
    assert_eq!(p.leftovers.len(), 1);
    assert!(p.leftovers[0].restore);
    // следующий проход (блоб ещё не созрел) — файл на месте
    let r = w
        .sweep_at(Timestamp::now() - SignedDuration::from_hours(48))
        .await;
    assert_eq!((r.restored, r.rows, r.files), (1, 0, 0), "{r:?}");
    assert!(w.file(&sha).is_file());
    assert!(!parked.exists());
    assert_eq!(std::fs::read(w.file(&sha)).unwrap(), d);

    set_gc_fail(w.root(), Some("after_commit"));
    let e = blob_gc::sweep(&db, w.root(), Timestamp::now(), DAYS).await;
    set_gc_fail(w.root(), None);
    assert!(e.is_err(), "{e:?}");
    assert_eq!(w.refcount(&sha).await, None);
    assert!(parked.is_file());
    let r = w.sweep().await;
    assert_eq!((r.restored, r.files), (0, 1), "{r:?}");
    assert!(!parked.exists());
    assert!(!w.file(&sha).exists());
}

/// `blob_gc_days = 0` — сборщик не работает: ни прохода воркера, ни удаления.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_disabled_with_zero_days() {
    let w = world_cfg("gc-off", |c| c.limits.blob_gc_days = 0).await;
    let sha = w.garbage("g.bin", &bytes(11, 500), 30.0).await;
    let inc = w.root().join(".incoming");
    std::fs::write(inc.join("old.part"), b"x").unwrap();
    age(&inc.join("old.part"), 3.0);
    let r = blob_gc::sweep(&w.db().await, w.root(), Timestamp::now(), 0)
        .await
        .unwrap();
    assert_eq!(r, GcReport::default());
    let p = gc_blobs(&w.a.st, Timestamp::now()).await;
    assert_eq!(p.skipped, Some("disabled"));
    assert!(w.file(&sha).is_file());
    assert_eq!(w.refcount(&sha).await, Some(0));
    assert!(inc.join("old.part").is_file());
}

/// Файлы отчёта и вложения заметки в корзине (`refcount > 0`) не трогаются при любом возрасте.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gc_report_and_note_in_trash_kept() {
    let w = common::reports::world("gc-trash").await;
    let report = w.report("Итог прогона").await;
    let note = w.note("notes/вложение").await;
    common::attach::upload(&w.a, &w.o.tok, &note, "n.bin", &bytes(12, 640)).await;
    let r =
        w.a.tok(
            &w.o.tok,
            Method::DELETE,
            &format!("/api/v1/nodes/{note}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // отчёт в корзину — только владелец решения в своей сессии WebUI
    let uri = format!("/api/v1/entries/{report}");
    let cur = common::reports::web(&w.a, &w.o.web, Method::GET, &uri, None).await;
    let rev = cur.json()["rev"].to_string();
    let r = common::reports::web_h(
        &w.a,
        &w.o.web,
        Method::DELETE,
        &uri,
        None,
        &[("if-match", &rev)],
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    let owner: Id = w.o.id.parse().unwrap();
    let store = BlobStore::new(&w.a.st.data_dir.blobs_dir(), owner);
    let mut files = Vec::new();
    for aa in files_in(store.root()) {
        if aa.len() == 2 {
            for f in files_in(&store.root().join(&aa)) {
                let p = store.root().join(&aa).join(&f);
                age(&p, 400.0);
                files.push(p);
            }
        }
    }
    assert_eq!(files.len(), 4, "три файла отчёта и вложение заметки");
    let db = w.db().await;
    let r = blob_gc::sweep(&db, store.root(), Timestamp::now(), DAYS)
        .await
        .unwrap();
    assert_eq!((r.rows, r.files), (0, 0), "{r:?}");
    for p in &files {
        assert!(p.is_file(), "{}", p.display());
        let sha = p.file_name().unwrap().to_str().unwrap();
        assert!(refcount(&db, sha).await.unwrap() > 0);
    }
}

/// Копия, снятая после открепления, несёт строку `refcount = 0` без файла (опись — только блобы
/// со ссылками); после восстановления такую строку убирает сборщик.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_backup_with_zero_rows_then_gc() {
    let w = world("gc-restore").await;
    let kept = w.upload("kept.bin", &bytes(13, 1000)).await;
    let gone = w.upload("gone.bin", &bytes(14, 1100)).await;
    w.detach(&gone, "gone.bin").await;
    let cfg = (*w.a.st.cfg).clone();
    let rep = {
        let c = cfg.clone();
        tokio::task::spawn_blocking(move || backup(&c, &BackupOpts::default()))
            .await
            .unwrap()
            .unwrap()
    };
    let user = w.user;
    w.a.st.shutdown().await;
    drop(w);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let dst = common::cfg_in(common::tmp("gc-restore-dst").join("d"));
    restore(&dst, &rep.path, &RestoreOpts::default())
        .await
        .unwrap();
    let store = BlobStore::new(&dst.data_dir.join("blobs"), user);
    assert!(store.path_of(&kept).is_file());
    assert!(!store.path_of(&gone).exists());
    let db = Db::open(
        &dst.data_dir.join("users").join(format!("{user}.db")),
        USER_MIGRATIONS,
    )
    .unwrap();
    assert_eq!(refcount(&db, &gone).await, Some(0));
    let r = blob_gc::sweep(&db, store.root(), Timestamp::now(), DAYS)
        .await
        .unwrap();
    assert_eq!((r.rows, r.files), (1, 0), "{r:?}");
    assert_eq!(refcount(&db, &gone).await, None);
    assert_eq!(refcount(&db, &kept).await, Some(1));
    assert!(store.path_of(&kept).is_file());
}
