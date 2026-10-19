//! Команды `zam-server models fetch|status` и `cuda fetch` (задача 9 M2) — запуск бинарника
//! без сети: отказ без согласия с лицензией jina, состояние, отказ CPU-комплекта.
//! Загрузка с проверкой SHA-256 и размера — в тестах zam-fetch (локальный HTTP-сервер).

mod common;

use std::process::{Command, Output};

use common::tmp;

fn zam_server(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zam-server"));
    c.args(args);
    // настройки только из аргументов и `envs`
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
        c.env_remove(k);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().expect("запуск zam-server")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn jina_without_license_ack_downloads_nothing() {
    let d = tmp("models-cmd-nc");
    let data = d.join("data");
    let out = zam_server(
        &[
            "models",
            "fetch",
            "--device",
            "cpu",
            "--reranker",
            "--data-dir",
            data.to_str().unwrap(),
        ],
        &[],
    );
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(
        err.contains("CC-BY-NC-4.0") && err.contains("--accept-nc-license"),
        "{err}"
    );
    assert!(text(&out.stdout).is_empty(), "{}", text(&out.stdout));
    assert!(!data.join("models").exists());
}

#[test]
fn status_lists_manifest_models() {
    let d = tmp("models-cmd-status");
    let data = d.join("data");
    let out = zam_server(
        &["models", "status", "--data-dir", data.to_str().unwrap()],
        &[],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let s = text(&out.stdout);
    for id in [
        "bge-m3-int8",
        "bge-m3-fp16",
        "bge-reranker-v2-m3",
        "jina-reranker-v2",
    ] {
        let line = s
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap_or_else(|| panic!("{s}"));
        assert!(line.ends_with(": нет"), "{line}");
    }
    assert!(s.contains("--accept-nc-license"), "{s}");
}

#[test]
fn cuda_fetch_refuses_cpu_kit() {
    let d = tmp("cuda-cmd-cpu");
    let ort = d.join("onnxruntime.dll");
    let out = zam_server(
        &[
            "cuda",
            "fetch",
            "--data-dir",
            d.join("data").to_str().unwrap(),
        ],
        &[("ZAM_ML__ORT_DYLIB", ort.to_str().unwrap())],
    );
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("CPU-комплект"), "{err}");
    assert!(!d.join("cuda").exists());
}
