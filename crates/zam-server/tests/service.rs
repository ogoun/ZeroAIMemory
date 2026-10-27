//! Режим службы (§13.3, задача 14 M4): юнит systemd по конфигу, `service run` вне SCM,
//! ошибки вызова. Службы не регистрируются и не запускаются — только `--print` и проверки до
//! обращения к менеджеру служб.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use zam_server::config::CliOverrides;
use zam_server::service::{self, systemd};

const BIN: &str = env!("CARGO_BIN_EXE_zam-server");

fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("service-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Конфиг с `qdrant.shutdown_flush_wait_ms = wait_ms` и каталогом данных в `d`.
fn config(d: &Path, wait_ms: u64) -> PathBuf {
    let data = d.join("data");
    let p = d.join("config.toml");
    std::fs::write(
        &p,
        format!(
            "data_dir = {data:?}\n\n[qdrant]\nshutdown_flush_wait_ms = {wait_ms}\n",
            data = data.display().to_string()
        ),
    )
    .unwrap();
    p
}

fn unit_for(cfg_path: &Path) -> String {
    // без окружения теста: в CI задана `ZAM_REQUIRE_ML`, а `ZAM_*` — ключи конфигурации
    let cfg = zam_server::config::load(Some(cfg_path), &[], &CliOverrides::default()).unwrap();
    systemd::render_unit(&systemd::UnitParams {
        exe: PathBuf::from("/opt/zeroaimemory/zam-server"),
        config: PathBuf::from("/etc/zeroaimemory/config.toml"),
        user: None,
        rw_paths: vec![PathBuf::from("/var/lib/zeroaimemory")],
        timeout: service::stop_timeout(cfg.qdrant.shutdown_flush_wait_ms),
    })
    .unwrap()
}

fn zam(args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    // ZAM_* окружения (разработчика, `ZAM_REQUIRE_ML` в CI) — ключи конфигурации сервера
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("ZAM_") {
            cmd.env_remove(k);
        }
    }
    cmd.output().unwrap()
}

/// `TimeoutStopSec` — по `qdrant.shutdown_flush_wait_ms` конфига (R4): 94 с при 14 000,
/// 140 с при 60 000, не меньше 90 с. На Linux — через `service install --print`, на Windows
/// `--print` показывает параметры SCM с тем же таймаутом `Preshutdown`.
#[test]
fn systemd_unit_timeout_from_config() {
    let d = dir("timeout");
    for (wait, secs) in [(14_000u64, 94u64), (60_000, 140), (0, 90), (10_500, 91)] {
        let c = config(&d, wait);
        let unit = unit_for(&c);
        assert!(
            unit.lines().any(|l| l == format!("TimeoutStopSec={secs}")),
            "{wait}: {unit}"
        );
        let out = zam(&[
            "service",
            "install",
            "--print",
            "--config",
            c.to_str().unwrap(),
        ]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{wait}: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if cfg!(windows) {
            assert!(
                stdout.contains(&format!("таймаут остановки (Preshutdown): {secs} с")),
                "{stdout}"
            );
            assert!(stdout.contains("service run --config"), "{stdout}");
            assert!(
                stdout.contains("wait_hint 10 с, checkpoint каждые 2 с"),
                "{stdout}"
            );
        } else {
            assert!(
                stdout
                    .lines()
                    .any(|l| l == format!("TimeoutStopSec={secs}")),
                "{stdout}"
            );
            let abs = std::path::absolute(&c).unwrap();
            assert!(
                stdout.lines().any(|l| l.starts_with("ExecStart=")
                    && l.ends_with(&format!("serve --config {}", abs.display()))),
                "{stdout}"
            );
            assert!(
                stdout
                    .lines()
                    .any(|l| l == format!("ReadWritePaths={}", d.join("data").display())),
                "{stdout}"
            );
        }
    }
    assert_eq!(
        service::stop_timeout(60_000),
        Duration::from_secs(140),
        "максимум ожидания WAL"
    );
}

/// Остановка юнита — SIGTERM (сервер понимает и SIGINT; оба — в общий сигнал остановки),
/// перезапуск при сбое, `User=` по `--user`.
#[test]
fn systemd_unit_kill_signal_sigterm() {
    let d = dir("kill");
    let unit = unit_for(&config(&d, 14_000));
    assert!(unit.lines().any(|l| l == "KillSignal=SIGTERM"), "{unit}");
    assert!(!unit.contains("SIGINT"), "{unit}");
    assert!(unit.lines().any(|l| l == "Restart=on-failure"), "{unit}");
    assert!(unit.lines().any(|l| l == "User=zam"), "{unit}");
    if cfg!(unix) {
        let c = config(&d, 14_000);
        let out = zam(&[
            "service",
            "install",
            "--print",
            "--user",
            "alice",
            "--config",
            c.to_str().unwrap(),
        ]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(stdout.lines().any(|l| l == "User=alice"), "{stdout}");
        assert!(
            stdout.lines().any(|l| l == "KillSignal=SIGTERM"),
            "{stdout}"
        );
    }
}

/// `service run` из консоли (не диспетчером служб) — понятная ошибка и код 2.
#[test]
fn service_run_outside_scm_errors_cleanly() {
    let out = zam(&["service", "run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    if cfg!(windows) {
        assert!(
            stderr.contains("запускается только менеджером служб"),
            "{stderr}"
        );
    } else {
        assert!(stderr.contains("systemd"), "{stderr}");
    }
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// Ошибки вызова до обращения к менеджеру служб: нет конфига, недопустимое имя, `--user` на
/// Windows — код 2.
#[test]
fn service_install_usage_errors() {
    let d = dir("usage");
    let missing = d.join("нет.toml");
    let out = zam(&[
        "service",
        "install",
        "--print",
        "--config",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("файл конфигурации не найден"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let c = config(&d, 14_000);
    let out = zam(&[
        "service",
        "install",
        "--print",
        "--name",
        "zam server",
        "--config",
        c.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    if cfg!(windows) {
        let out = zam(&[
            "service",
            "install",
            "--print",
            "--user",
            "alice",
            "--config",
            c.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("только для Linux"));
    }
}
