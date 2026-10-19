//! Общие помощники интеграционных тестов zam-ml: пути к ORT и моделям из `target/`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use zam_ml::{DevicePref, MlConfig};

/// Корень репозитория.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Платформа в `target/deps/<платформа>/`.
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

/// Имя библиотеки ORT.
pub fn ort_name() -> &'static str {
    if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    }
}

/// CPU-сборка ORT (`cargo xtask fetch-deps`).
pub fn ort_cpu() -> PathBuf {
    repo_root()
        .join("target")
        .join("deps")
        .join(platform())
        .join("onnxruntime")
        .join(ort_name())
}

/// GPU-сборка ORT (с CUDA EP, без библиотек NVIDIA).
pub fn ort_gpu() -> PathBuf {
    repo_root()
        .join("target")
        .join("deps")
        .join(platform())
        .join("onnxruntime-gpu")
        .join(ort_name())
}

/// `target/models` (`cargo xtask fetch-models`).
pub fn models_dir() -> PathBuf {
    repo_root().join("target").join("models")
}

/// Отдельный каталог для временных данных теста (внутри `target/`).
pub fn tmp_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("zam-ml")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Есть ли ORT и модель. Без них тест пропускается, но в CI (`ZAM_REQUIRE_ML=1`) падает.
pub fn have(ort: &Path, model: &str) -> bool {
    let ok = ort.is_file() && models_dir().join(model).join("model.toml").is_file();
    if !ok {
        assert!(
            std::env::var_os("ZAM_REQUIRE_ML").is_none(),
            "нет ORT ({}) или модели {model}: cargo xtask fetch-deps && cargo xtask fetch-models --test",
            ort.display()
        );
        eprintln!("пропуск: нет ORT или модели {model}");
    }
    ok
}

pub fn config(ort: PathBuf, models: PathBuf, device: DevicePref) -> MlConfig {
    MlConfig {
        device,
        models_dir: models,
        ort_dylib: ort,
        index_threads: 2,
        query_threads: 2,
        unload_after: Some(Duration::from_secs(600)),
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

pub fn norm(a: &[f32]) -> f32 {
    a.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Синтетический `sparse_linear.pt`: zip без сжатия с `<name>/data/0` (fp16 веса) и `<name>/data/1` (fp16 bias).
pub fn write_sparse_pt(path: &Path, w: &[f32], b: f32) {
    use std::io::Write as _;
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opt =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    // pickle не читается — кладём заглушку, как в настоящем файле есть data.pkl
    z.start_file("sparse_linear/data.pkl", opt).unwrap();
    z.write_all(b"\x80\x02}q\x00.").unwrap();
    z.start_file("sparse_linear/data/0", opt).unwrap();
    for x in w {
        z.write_all(&half::f16::from_f32(*x).to_le_bytes()).unwrap();
    }
    z.start_file("sparse_linear/data/1", opt).unwrap();
    z.write_all(&half::f16::from_f32(b).to_le_bytes()).unwrap();
    z.finish().unwrap();
}

/// RSS процесса в байтах (Linux: /proc/self/status; Windows: WorkingSet64 через PowerShell).
pub fn rss_bytes() -> Option<u64> {
    if cfg!(windows) {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {}).WorkingSet64", std::process::id()),
            ])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    } else {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

/// Каталог моделей с копией MiniLM под именем `name` и своим `model.toml` (`extra` — доп. строки
/// спеки; если в них есть `sparse_head`, рядом кладётся синтетическая голова). Возвращает корень.
pub fn minilm_copy(name: &str, extra: &str) -> PathBuf {
    let root = tmp_dir(name);
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let src = models_dir().join("minilm-l6-int8");
    for f in ["model.onnx", "tokenizer.json"] {
        std::fs::copy(src.join(f), dir.join(f)).unwrap();
    }
    if extra.contains("sparse_head") {
        let w: Vec<f32> = (0..384)
            .map(|i| ((i * 37 % 17) as f32 - 8.0) / 8.0)
            .collect();
        write_sparse_pt(&dir.join("sparse_linear.pt"), &w, 0.5);
    }
    std::fs::write(
        dir.join("model.toml"),
        format!(
            "id = \"{name}\"\ndim = 384\npooling = \"mean\"\nmax_tokens_passage = 256\n\
             max_tokens_query = 128\n{extra}"
        ),
    )
    .unwrap();
    root
}
