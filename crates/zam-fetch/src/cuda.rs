//! Манифест библиотек NVIDIA `deploy/cuda.toml` (CUDA 13 и cuDNN 9 для CUDA EP ONNX Runtime)
//! и их установка (`zam-server cuda fetch`).
//!
//! Библиотеки кладутся плоско в один каталог — `<каталог ORT>/cuda/`, откуда их
//! предзагружает zam-ml (`ort::ep::cuda::preload_dylibs`). Windows: DLL из zip-архивов.
//! Linux: tar.xz; имена SONAME (`libcudart.so.13`) в архиве — ссылки на файл с полной
//! версией, под именем SONAME кладётся сам файл (см. [`crate::extract_selected`]).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::models::{NVIDIA_REDIST, check_plain_name, check_sha256};
use crate::{FetchError, PLATFORMS, PlatformEntry, install_archive, is_installed};

/// Манифест библиотек NVIDIA (CUDA и cuDNN) для CUDA EP.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CudaManifest {
    /// Выпуск CUDA redist (`redistrib_<cuda>.json`).
    pub cuda: String,
    /// Выпуск cuDNN redist (`redistrib_<cudnn>.json`).
    pub cudnn: String,
    pub lib: Vec<CudaLib>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CudaLib {
    pub platform: String,
    /// Компонент redist (`cuda_cudart`, `libcublas`, `cudnn`, …).
    pub component: String,
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// Пути внутри архива; файл кладётся в `<каталог ORT>/cuda/` под последним компонентом пути.
    pub extract: Vec<String>,
}

impl CudaLib {
    /// Архив как запись [`PlatformEntry`] для [`install_archive`].
    pub fn entry(&self) -> PlatformEntry {
        PlatformEntry {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            size: Some(self.size),
            extract: self.extract.clone(),
            rename: BTreeMap::new(),
        }
    }
}

impl CudaManifest {
    pub fn parse(text: &str) -> Result<Self, FetchError> {
        toml::from_str(text)
            .map_err(|e| FetchError::Invalid(format!("не удалось разобрать манифест CUDA: {e}")))
    }

    pub fn load(path: &Path) -> Result<Self, FetchError> {
        let text = std::fs::read_to_string(path).map_err(FetchError::io(format!(
            "не удалось прочитать {}",
            path.display()
        )))?;
        Self::parse(&text).map_err(|e| FetchError::Invalid(format!("{}: {e}", path.display())))
    }

    pub fn validate(&self) -> Result<(), FetchError> {
        if self.lib.is_empty() {
            return Err(FetchError::Invalid(
                "в манифесте CUDA нет ни одной записи [[lib]]".into(),
            ));
        }
        for l in &self.lib {
            let ctx = format!("{} {} [{}]", l.component, l.version, l.platform);
            let bad = |m: String| FetchError::Invalid(format!("{ctx}: {m}"));
            if !PLATFORMS.contains(&l.platform.as_str()) {
                return Err(bad(format!(
                    "неизвестная платформа '{}', допустимы: {}",
                    l.platform,
                    PLATFORMS.join(", ")
                )));
            }
            if !l.url.starts_with("https://") {
                return Err(bad("url должен начинаться с https://".into()));
            }
            if !l.url.starts_with(NVIDIA_REDIST) {
                return Err(bad(format!("url должен вести на {NVIDIA_REDIST}")));
            }
            check_sha256(l.sha256.trim()).map_err(bad)?;
            if l.size == 0 {
                return Err(bad("size должен быть больше нуля".into()));
            }
            if l.extract.is_empty() {
                return Err(bad("пустой список extract".into()));
            }
        }
        for p in PLATFORMS {
            self.dest_names(p)?;
        }
        Ok(())
    }

    /// Архивы платформы.
    pub fn libs<'a>(&'a self, platform: &'a str) -> impl Iterator<Item = &'a CudaLib> + 'a {
        self.lib.iter().filter(move |l| l.platform == platform)
    }

    /// Суммарный размер архивов платформы, байт.
    pub fn total_size(&self, platform: &str) -> u64 {
        self.libs(platform).map(|l| l.size).sum()
    }

    /// Имена файлов в `<каталог ORT>/cuda/` для платформы; дубликаты и пустые имена — ошибка.
    pub fn dest_names(&self, platform: &str) -> Result<Vec<String>, FetchError> {
        let mut names: Vec<String> = Vec::new();
        for l in self.libs(platform) {
            for p in &l.extract {
                let name = p.rsplit(['/', '\\']).next().unwrap_or("");
                check_plain_name("файл", name).map_err(|e| {
                    FetchError::Invalid(format!("{} [{platform}]: '{p}': {e}", l.component))
                })?;
                if names.iter().any(|n| n == name) {
                    return Err(FetchError::Invalid(format!(
                        "[{platform}]: два файла извлекаются под одним именем '{name}'"
                    )));
                }
                names.push(name.to_string());
            }
        }
        Ok(names)
    }
}

/// Ставит библиотеки платформы в `dir` (плоско): архив за архивом через [`install_archive`]
/// со штампом `.<компонент>.sha256` (размер и SHA-256 сверяются; архив пропускается, только
/// если штамп совпал с манифестом и файлы на месте, иначе файлы заменяются). `on_lib`
/// вызывается перед каждым архивом с признаком «уже установлен» (прогресс в CLI).
/// Возвращает (компонент, был ли уже установлен — штамп совпал).
pub fn install_cuda(
    manifest: &CudaManifest,
    platform: &str,
    dir: &Path,
    on_lib: &mut dyn FnMut(&CudaLib, bool),
) -> Result<Vec<(String, bool)>, FetchError> {
    let libs: Vec<&CudaLib> = manifest.libs(platform).collect();
    if libs.is_empty() {
        return Err(FetchError::Invalid(format!(
            "в манифесте CUDA нет библиотек для {platform}"
        )));
    }
    let mut out = Vec::with_capacity(libs.len());
    for l in libs {
        on_lib(l, is_installed(&l.entry(), dir, &l.component)?);
        // штамп `.<компонент>.sha256`: новый выпуск в манифесте заменяет старые библиотеки
        let cached = install_archive(&l.entry(), dir, &l.component)
            .map_err(|e| e.context(&format!("{} {}", l.component, l.version)))?;
        out.push((l.component.clone(), cached));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Библиотеки, которые предзагружает `ort::ep::cuda::preload_dylibs`
    /// (ort 2.0.0-rc.14, `src/ep/cuda.rs`: `CUDA_DYLIBS` и `CUDNN_DYLIBS`).
    const ORT_DYLIBS_WINDOWS: &[&str] = &[
        "cudart64_13.dll",
        "cublasLt64_13.dll",
        "cublas64_13.dll",
        "cufft64_12.dll",
        "cudnn64_9.dll",
        "cudnn_graph64_9.dll",
        "cudnn_ops64_9.dll",
        "cudnn_heuristic64_9.dll",
        "cudnn_adv64_9.dll",
        "cudnn_cnn64_9.dll",
        "cudnn_engines_precompiled64_9.dll",
        "cudnn_engines_runtime_compiled64_9.dll",
    ];
    const ORT_DYLIBS_LINUX: &[&str] = &[
        "libcudart.so.13",
        "libcublasLt.so.13",
        "libcublas.so.13",
        "libnvrtc.so.13",
        "libcurand.so.10",
        "libcufft.so.12",
        "libcudnn.so.9",
        "libcudnn_graph.so.9",
        "libcudnn_ops.so.9",
        "libcudnn_heuristic.so.9",
        "libcudnn_adv.so.9",
        "libcudnn_cnn.so.9",
        "libcudnn_engines_precompiled.so.9",
        "libcudnn_engines_runtime_compiled.so.9",
    ];

    const CUDA_SAMPLE: &str = r#"
cuda = "13.4.3"
cudnn = "9.27.0"
[[lib]]
platform = "windows-x64"
component = "cuda_cudart"
version = "13.4.92"
url = "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/a.zip"
size = 10
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
extract = ["a/bin/x64/cudart64_13.dll"]
[[lib]]
platform = "linux-x64"
component = "cuda_cudart"
version = "13.4.92"
url = "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/linux-x86_64/a.tar.xz"
size = 10
sha256 = "2222222222222222222222222222222222222222222222222222222222222222"
extract = ["a/lib/libcudart.so.13"]
"#;

    #[test]
    fn cuda_manifest_parses_and_validates() {
        let m = CudaManifest::parse(CUDA_SAMPLE).unwrap();
        m.validate().unwrap();
        assert_eq!(m.cuda, "13.4.3");
        assert_eq!(m.cudnn, "9.27.0");
        assert_eq!(m.lib.len(), 2);
        assert_eq!(m.lib[0].component, "cuda_cudart");
        assert_eq!(m.lib[0].size, 10);
        assert_eq!(m.lib[0].entry().size, Some(10));
        assert_eq!(m.total_size("linux-x64"), 10);
        assert_eq!(m.dest_names("linux-x64").unwrap(), ["libcudart.so.13"]);
        let ones = "1".repeat(64);
        for (from, to, needle) in [
            ("https://developer", "http://developer", "https://"),
            (
                "https://developer.download.nvidia.com",
                "https://example.com",
                "developer.download.nvidia.com",
            ),
            (ones.as_str(), "123", "sha256"),
            ("platform = \"linux-x64\"", "platform = \"mac\"", "mac"),
            ("size = 10", "size = 0", "size"),
            (
                "extract = [\"a/lib/libcudart.so.13\"]",
                "extract = []",
                "extract",
            ),
            ("a/bin/x64/cudart64_13.dll", "a/bin/x64/", "a/bin/x64/"),
        ] {
            let bad = CUDA_SAMPLE.replacen(from, to, 1);
            let err = match CudaManifest::parse(&bad) {
                Ok(m) => m.validate().expect_err(from).to_string(),
                Err(e) => e.to_string(),
            };
            assert!(err.contains(needle), "'{from}' -> '{to}': {err}");
        }
        // одно имя назначения из двух архивов одной платформы
        let win = CUDA_SAMPLE.split("[[lib]]").nth(1).unwrap();
        let dup = format!("{CUDA_SAMPLE}[[lib]]{win}");
        let err = CudaManifest::parse(&dup)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("cudart64_13.dll"), "{err}");
    }

    #[test]
    fn real_cuda_manifest_covers_ort_dylibs() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("cuda.toml");
        let m = CudaManifest::load(&p).unwrap();
        m.validate().unwrap();
        for (plat, need) in [
            ("windows-x64", ORT_DYLIBS_WINDOWS),
            ("linux-x64", ORT_DYLIBS_LINUX),
        ] {
            let names = m.dest_names(plat).unwrap();
            for n in need {
                assert!(names.iter().any(|x| x == n), "{plat}: нет {n}");
            }
            // Linux — только tar.xz, Windows — только zip
            for l in m.libs(plat) {
                let ext = if plat == "linux-x64" {
                    ".tar.xz"
                } else {
                    ".zip"
                };
                assert!(l.url.ends_with(ext), "{}", l.url);
            }
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("zam-fetch")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Отдаёт `body` на `n` запросов.
    fn serve(body: Vec<u8>, name: &str, n: usize) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/{name}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match std::io::Read::read(&mut s, &mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => req.extend_from_slice(&b[..k]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        });
        url
    }

    #[test]
    fn install_cuda_puts_soname_files_flat() {
        let d = tmp("install_cuda");
        // tar.xz как у NVIDIA: SONAME — ссылка на файл с полной версией
        let mut t = tar::Builder::new(Vec::new());
        let mut add = |n: &str, link: Option<&str>, c: &[u8]| {
            let mut h = tar::Header::new_gnu();
            match link {
                Some(target) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    t.append_link(&mut h, n, target).unwrap();
                }
                None => {
                    h.set_size(c.len() as u64);
                    h.set_mode(0o755);
                    t.append_data(&mut h, n, c).unwrap();
                }
            }
        };
        add("r/lib/libcudart.so.13.4.92", None, b"CUDART");
        add("r/lib/libcudart.so.13", Some("libcudart.so.13.4.92"), b"");
        add("r/lib/libcudart.so", Some("libcudart.so.13"), b"");
        add("r/include/cuda.h", None, b"H");
        let mut xz = Vec::new();
        let mut w =
            lzma_rust2::XzWriter::new(&mut xz, lzma_rust2::XzOptions::with_preset(1)).unwrap();
        w.write_all(&t.into_inner().unwrap()).unwrap();
        w.finish().unwrap();
        let sha = {
            use sha2::Digest as _;
            sha2::Sha256::digest(&xz)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        };
        let size = xz.len();
        let url = serve(xz, "cudart.tar.xz", 1);
        let text = format!(
            "cuda = \"13.4.3\"\ncudnn = \"9.27.0\"\n[[lib]]\nplatform = \"linux-x64\"\n\
             component = \"cuda_cudart\"\nversion = \"13.4.92\"\nurl = \"{url}\"\nsize = {size}\n\
             sha256 = \"{sha}\"\nextract = [\"r/lib/libcudart.so.13\"]\n"
        );
        let m = CudaManifest::parse(&text).unwrap();
        let dir = d.join("ort").join("cuda");
        let mut seen = Vec::new();
        let r = install_cuda(&m, "linux-x64", &dir, &mut |l, done| {
            seen.push((l.component.clone(), done))
        })
        .unwrap();
        assert_eq!(r, [("cuda_cudart".to_string(), false)]);
        assert_eq!(seen, [("cuda_cudart".to_string(), false)]);
        assert!(dir.join(".cuda_cudart.sha256").is_file());
        assert_eq!(
            std::fs::read(dir.join("libcudart.so.13")).unwrap(),
            b"CUDART"
        );
        // библиотека и штамп
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        // рядом с каталогом не осталось *.part и *.tmp
        let left: Vec<_> = std::fs::read_dir(d.join("ort"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["cuda"]);
        // повторно — без сети (сервер обслужил единственный запрос)
        let r = install_cuda(&m, "linux-x64", &dir, &mut |_, _| {}).unwrap();
        assert_eq!(r, [("cuda_cudart".to_string(), true)]);
        // платформы нет в манифесте — ошибка
        assert!(install_cuda(&m, "windows-x64", &dir, &mut |_, _| {}).is_err());
    }

    /// Настоящие архивы NVIDIA по `deploy/cuda.toml` (сеть, ~2,2 ГБ для Linux, ~1,9 ГБ для
    /// Windows): все файлы платформы ставятся, имена SONAME — обычные файлы ELF. Платформа —
    /// `ZAM_CUDA_PLATFORM` (по умолчанию текущая; Linux-набор проверяется и на Windows).
    /// Запуск: `cargo test --release -p zam-fetch real_cuda_archives -- --ignored --nocapture`.
    #[test]
    #[ignore = "сеть: скачивает redist-архивы NVIDIA"]
    fn real_cuda_archives_install() {
        let platform =
            std::env::var("ZAM_CUDA_PLATFORM").unwrap_or_else(|_| crate::host_platform().into());
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("cuda.toml");
        let m = CudaManifest::load(&p).unwrap();
        // каталог не стирается между запусками: архивы не перекачиваются
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("zam-fetch")
            .join(format!("cuda-{platform}"))
            .join("cuda");
        let t = std::time::Instant::now();
        install_cuda(&m, &platform, &dir, &mut |l, _| {
            eprintln!("{} {} ({} МБ)", l.component, l.version, l.size / 1_000_000)
        })
        .unwrap();
        eprintln!("{platform}: {:?}", t.elapsed());
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        let mut want = m.dest_names(&platform).unwrap();
        want.sort();
        assert_eq!(names, want);
        for n in &names {
            let f = dir.join(n);
            let mut head = [0u8; 4];
            std::io::Read::read_exact(&mut std::fs::File::open(&f).unwrap(), &mut head).unwrap();
            let len = std::fs::metadata(&f).unwrap().len();
            eprintln!("  {n}: {:.1} МБ", len as f64 / 1e6);
            if platform == "linux-x64" {
                assert_eq!(&head, b"ELF", "{n}");
            } else {
                assert_eq!(&head[..2], b"MZ", "{n}");
            }
        }
    }
}
