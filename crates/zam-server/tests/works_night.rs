//! Ночной исполнитель `NightRunner` (M7.3 §6.1 п. 5, задача 1): остановка первой в
//! `AppState::shutdown` при висящей работе, независимость от `[backup]`, порядок «проверка всех
//! пользователей — затем работы». Работы ночи подменяются тестовыми точками
//! (`works::night::test_hooks`): тело проверки и очереди — задачи 5 и 3.
mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp, civil, tz::TimeZone};
use zam_core::Id;
use zam_server::config::{BackupSchedule, CheckMode, ServerConfig};
use zam_server::works::night::JobFuture;
use zam_server::works::night::test_hooks::{self, JobFn, Jobs};
use zam_store::StoreError;

use common::api::{api, api_with};

/// Тестовые точки ночи общие для процесса: тесты файла идут по одному.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Снимает подмены при выходе из теста (и при панике).
struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        test_hooks::reset();
    }
}

/// Отпускает висящий блокирующий поток при выходе из теста (и при панике).
struct Unblock(Arc<AtomicBool>);

impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Окно ночи вокруг «сейчас»: `at` — минутой раньше (UTC), окно 2 ч; ночная проверка включена
/// (в `common::cfg_in` она выключена, решение 67).
fn night_now(c: &mut ServerConfig) {
    let at = (Timestamp::now() - SignedDuration::from_mins(1)).to_zoned(TimeZone::UTC);
    c.works.at = at.strftime("%H:%M").to_string();
    c.works.tz = "UTC".into();
    c.works.window_min = 120;
    c.works.check = CheckMode::Nightly;
}

/// Работа ночи из замыкания.
fn job<F, Fut>(f: F) -> JobFn
where
    F: Fn(zam_server::works::NightCtx, civil::Date, Timestamp) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), StoreError>> + Send + 'static,
{
    Arc::new(move |ctx, night, until| -> JobFuture { Box::pin(f(ctx, night, until)) })
}

/// Ждёт `cond` не дольше `limit`.
async fn until(what: &str, limit: Duration, cond: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < limit, "не дождались: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// §6.1 п. 5, решение 56: подменённая работа ночи висит в `spawn_blocking` (не отвечает на
/// остановку); `AppState::shutdown` останавливает `NightRunner` первой строкой — не дольше 5 с
/// ожидания (+ 1 с запаса), раньше обслуживания (журнал тестовой точки); после остановки и
/// без клонов теста файл базы пользователя переименовывается, хотя блокирующий поток ещё висит:
/// ни задача, ни блокирующее замыкание соединений базы не держат.
#[tokio::test]
async fn night_runner_stops_before_dbs() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    // и при падении проверки: рантайм теста при уничтожении ждёт блокирующий поток
    let _unblock = Unblock(release.clone());
    let (e, r) = (entered.clone(), release.clone());
    test_hooks::set_tick(Duration::from_millis(50));
    test_hooks::set_jobs(Jobs {
        check: Some(job(move |ctx, _, _| {
            let (e, r) = (e.clone(), r.clone());
            async move {
                // база пользователя открыта клоном состояния, который держит работа
                ctx.st().users.get(ctx.st().admin_id).await?;
                // блокирующему замыканию — только данные запроса, не `Db` и не `AppState`
                let _ = tokio::task::spawn_blocking(move || {
                    e.store(true, Ordering::SeqCst);
                    while !r.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
                .await;
                drop(ctx);
                Ok(())
            }
        })),
        queue: None,
    });
    let a = api("night-stop", night_now).await;
    let path = a.st.data_dir.user_db(&a.st.admin_id);
    until(
        "работа ночи начата",
        Duration::from_secs(20),
        || entered.load(Ordering::SeqCst),
    )
    .await;
    assert!(path.exists());
    let serial = a.st.night.serial();
    let t0 = Instant::now();
    a.st.shutdown().await;
    let took = t0.elapsed();
    assert!(
        took <= Duration::from_secs(6),
        "shutdown {took:?}: остановка ночи — не дольше 5 с ожидания"
    );
    assert_eq!(
        test_hooks::stop_log(serial),
        ["night", "maintenance"],
        "NightRunner — раньше обслуживания"
    );
    drop(a);
    // блокирующий поток всё ещё висит, а база уже закрыта (Windows не переименует открытый файл);
    // фоновые задачи открытия базы отпускают `Db` чуть позже — ждём ограниченно
    assert!(!release.load(Ordering::SeqCst));
    let moved = path.with_extension("db.moved");
    let t0 = Instant::now();
    loop {
        match std::fs::rename(&path, &moved) {
            Ok(()) => break,
            Err(e) => assert!(
                t0.elapsed() < Duration::from_secs(3),
                "база пользователя открыта после shutdown: {e}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    release.store(true, Ordering::SeqCst);
    std::fs::rename(&moved, &path).unwrap();
}

/// §6.1 п. 5: ночной исполнитель не зависит от `[backup]` (`schedule = "off"` — задачи автокопий
/// нет); часы заморожены в окне, тик 50 мс — работа ночи вызывается один раз за ночь: после
/// первого вызова ещё несколько тиков без второго.
#[tokio::test]
async fn night_runner_independent_of_backup() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    let calls = Arc::new(AtomicUsize::new(0));
    let nights = Arc::new(Mutex::new(Vec::new()));
    let (c, n) = (calls.clone(), nights.clone());
    test_hooks::set_tick(Duration::from_millis(50));
    test_hooks::set_jobs(Jobs {
        check: None,
        queue: Some(job(move |_, night, until| {
            let (c, n) = (c.clone(), n.clone());
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                n.lock().unwrap().push((night, until));
                Ok(())
            }
        })),
    });
    // 05:00 UTC — в окне 04:30–06:30 (умолчание `at`, `tz`, `window_min`)
    let frozen: Timestamp = "2026-11-23T05:00:00Z".parse().unwrap();
    let a = {
        let _clock = zam_store::time::freeze_now(frozen);
        let a = api("night-backup-off", |c| {
            c.backup.schedule = BackupSchedule::Off;
            c.works.check = CheckMode::Nightly;
        })
        .await;
        assert_eq!(a.st.cfg.works.at, "04:30");
        until("ночь", Duration::from_secs(20), || {
            calls.load(Ordering::SeqCst) >= 1
        })
        .await;
        // ещё несколько тиков: вторая ночь не начинается
        tokio::time::sleep(Duration::from_millis(400)).await;
        a
    };
    assert_eq!(calls.load(Ordering::SeqCst), 1, "одна ночь — один вызов");
    let (night, until) = nights.lock().unwrap()[0];
    assert_eq!(night, civil::date(2026, 11, 23));
    assert_eq!(until, "2026-11-23T06:30:00Z".parse::<Timestamp>().unwrap());
    a.st.shutdown().await;
}

/// §6.3 п. 2: ночь — сначала проверка всех пользователей (`check::night`), затем работы
/// (`works::queue::night`); подменённые функции пишут порядок вызовов по пользователям.
#[tokio::test]
async fn night_runner_checks_before_works() {
    let _serial = SERIAL.lock().await;
    let _reset = Reset;
    let order: Arc<Mutex<Vec<(&'static str, Id)>>> = Arc::default();
    let record = |what: &'static str| {
        let order = order.clone();
        job(move |ctx, _, _| {
            let order = order.clone();
            async move {
                for u in ctx.st().system.list_users().await? {
                    // между пользователями — ожидание: порядок не держится на одном шаге
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    order.lock().unwrap().push((what, u.id));
                }
                Ok(())
            }
        })
    };
    // второй пользователь — до старта ночного исполнителя: первый запуск без `[works]`, затем
    // перезапуск на том же каталоге с окном ночи вокруг «сейчас»
    let a = api("night-order", |c| c.works.enabled = false).await;
    let other = a.st.system.create_user("Мира", false).await.unwrap();
    let mut cfg = a.st.cfg.as_ref().clone();
    a.st.shutdown().await;
    drop(a);
    cfg.works.enabled = true;
    night_now(&mut cfg);
    test_hooks::set_tick(Duration::from_millis(50));
    test_hooks::set_jobs(Jobs {
        check: Some(record("check")),
        queue: Some(record("queue")),
    });
    let a = api_with(cfg).await;
    until("ночь", Duration::from_secs(20), || {
        order
            .lock()
            .unwrap()
            .iter()
            .filter(|(w, _)| *w == "queue")
            .count()
            >= 2
    })
    .await;
    a.st.shutdown().await;
    let got = order.lock().unwrap().clone();
    let first_queue = got.iter().position(|(w, _)| *w == "queue").unwrap();
    let checks: Vec<Id> = got[..first_queue].iter().map(|(_, u)| *u).collect();
    assert!(
        got[..first_queue].iter().all(|(w, _)| *w == "check"),
        "{got:?}"
    );
    assert!(
        checks.contains(&a.st.admin_id) && checks.contains(&other.id),
        "проверка обоих пользователей до первой работы: {got:?}"
    );
    assert!(
        got[first_queue..].iter().all(|(w, _)| *w == "queue"),
        "{got:?}"
    );
}
