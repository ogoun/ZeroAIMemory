//! Архивы для машин без интернета (§16.3):
//!
//! - `zam-models-<ver>-<id>.zip` — по архиву на модель с `bundle = true` из
//!   `deploy/models.toml` (одна модель — один архив: вместе модели больше предела 2 ГиБ на
//!   файл релиза GitHub). Внутри `models.toml` и `<id>/` с файлами модели, `LICENSE` и
//!   `model.toml`; распаковывается в каталог моделей (`<data_dir>/models`). Модели с
//!   некоммерческой лицензией (`requires_license_ack`) не входят никогда.
//! - `zam-cuda-<ver>-<платформа>.(zip|tar.gz)` — библиотеки NVIDIA по `deploy/cuda.toml`
//!   со штампами `.<компонент>.sha256`, `cuda.toml` и лицензиями NVIDIA в каталоге `cuda/`;
//!   распаковывается в каталог `zam-server` (рядом с ONNX Runtime), после чего `zam-server
//!   cuda fetch` видит библиотеки установленными и ничего не скачивает.
//!
//! В архив попадают только перечисленные манифестом файлы (временные `*.part`, `*.tmp` и
//! прочее из каталогов — никогда); SHA-256 моделей сверяется с манифестом при упаковке.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zam_fetch::{
    CudaManifest, MODEL_LICENSE_FILE, MODEL_SPEC_FILE, ModelEntry, ModelsManifest, install_cuda,
    is_installed, sha256_file,
};

use crate::package::{Entry, archive_ext, validate_version, write_archive};

/// Каталог в архиве `zam-cuda` и в каталоге `zam-server` (`<каталог ORT>/cuda/`).
pub const CUDA_DIR: &str = "cuda";

/// Модели комплекта `zam-models`: `bundle = true` и без `requires_license_ack`.
pub fn bundle_models(m: &ModelsManifest) -> Vec<&ModelEntry> {
    m.model
        .iter()
        .filter(|e| e.bundle && !e.requires_license_ack)
        .collect()
}

/// Имя архива модели.
pub fn models_archive_name(version: &str, id: &str) -> String {
    format!("zam-models-{version}-{id}.zip")
}

/// Записи архива модели: `models.toml`, файлы манифеста (размер и SHA-256 сверяются),
/// `LICENSE` и `model.toml` (совпадают с тем, что пишет `fetch-models`).
fn model_entries(m: &ModelEntry, models_dir: &Path, manifest_path: &Path) -> Result<Vec<Entry>> {
    let dir = models_dir.join(&m.id);
    let hint = format!("cargo xtask fetch-models --id {}", m.id);
    let mut v = vec![Entry::file("models.toml", manifest_path)];
    for f in &m.files {
        let p = dir.join(&f.path);
        let len = fs::metadata(&p)
            .with_context(|| format!("{}: нет {} ({hint})", m.id, p.display()))?
            .len();
        if len != f.size {
            bail!(
                "{}: {} — {len} байт вместо {} по манифесту ({hint})",
                m.id,
                f.path,
                f.size
            );
        }
        let actual = sha256_file(&p)?;
        if !actual.eq_ignore_ascii_case(f.sha256.trim()) {
            bail!(
                "{}: SHA-256 {} не совпал с манифестом: {actual} вместо {} ({hint})",
                m.id,
                f.path,
                f.sha256
            );
        }
        v.push(Entry::file(format!("{}/{}", m.id, f.path), p));
    }
    for (name, want) in [
        (MODEL_LICENSE_FILE, m.license_text()),
        (MODEL_SPEC_FILE, m.spec_toml()?),
    ] {
        let p = dir.join(name);
        let text = fs::read_to_string(&p)
            .with_context(|| format!("{}: нет {} ({hint})", m.id, p.display()))?;
        if text != want {
            bail!("{}: {name} не совпадает с манифестом ({hint})", m.id);
        }
        v.push(Entry::file(format!("{}/{name}", m.id), p));
    }
    Ok(v)
}

/// Собирает `zam-models-<ver>-<id>.zip` для каждой модели комплекта.
pub fn package_models(
    manifest_path: &Path,
    models_dir: &Path,
    version: &str,
    out_dir: &Path,
) -> Result<Vec<PathBuf>> {
    validate_version(version)?;
    let m = ModelsManifest::load(manifest_path)?;
    m.validate()?;
    let models = bundle_models(&m);
    if models.is_empty() {
        bail!("в {} нет моделей с bundle = true", manifest_path.display());
    }
    fs::create_dir_all(out_dir)?;
    let mut out = Vec::new();
    for e in models {
        let entries = model_entries(e, models_dir, manifest_path)?;
        let dest = out_dir.join(models_archive_name(version, &e.id));
        write_archive(&dest, "", &entries).with_context(|| e.id.clone())?;
        out.push(dest);
    }
    Ok(out)
}

/// Имя архива библиотек NVIDIA.
pub fn cuda_archive_name(version: &str, platform: &str) -> String {
    format!("zam-cuda-{version}-{platform}.{}", archive_ext(platform))
}

/// Записи `cuda/` архива: библиотеки и штамп каждого архива NVIDIA (штамп должен совпасть
/// с манифестом, файлы — на месте), `cuda.toml`, лицензии NVIDIA (`LICENSES/`).
fn cuda_entries(
    m: &CudaManifest,
    platform: &str,
    dir: &Path,
    manifest_path: &Path,
    licenses_dir: &Path,
) -> Result<Vec<Entry>> {
    let mut v = Vec::new();
    let mut any = false;
    for l in m.libs(platform) {
        any = true;
        let e = l.entry();
        if !is_installed(&e, dir, &l.component)? {
            bail!(
                "{} {} [{platform}]: в {} нет библиотек или штамп не совпал с манифестом",
                l.component,
                l.version,
                dir.display()
            );
        }
        for n in zam_fetch::dest_names(&e)? {
            v.push(Entry::file(n.clone(), dir.join(&n)));
        }
        let stamp = zam_fetch::stamp_path(dir, &l.component)?;
        let name = stamp
            .file_name()
            .and_then(|n| n.to_str())
            .context("имя штампа")?
            .to_string();
        v.push(Entry::file(name, stamp));
    }
    if !any {
        bail!("в манифесте CUDA нет библиотек для {platform}");
    }
    v.push(Entry::file("cuda.toml", manifest_path));
    let mut lic: Vec<PathBuf> = fs::read_dir(licenses_dir)
        .with_context(|| format!("нет каталога лицензий NVIDIA {}", licenses_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    lic.sort();
    if lic.is_empty() {
        bail!("в {} нет текстов лицензий NVIDIA", licenses_dir.display());
    }
    for p in lic {
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .context("имя файла лицензии")?
            .to_string();
        v.push(Entry::file(format!("LICENSES/{n}"), p));
    }
    Ok(v)
}

/// Ставит библиотеки NVIDIA платформы в `dir` (как `zam-server cuda fetch`: пропуск по
/// штампу, иначе загрузка с проверкой размера и SHA-256) и собирает архив `zam-cuda`.
pub fn package_cuda(
    manifest_path: &Path,
    licenses_dir: &Path,
    platform: &str,
    dir: &Path,
    version: &str,
    out_dir: &Path,
) -> Result<PathBuf> {
    validate_version(version)?;
    let m = CudaManifest::load(manifest_path)?;
    m.validate()?;
    println!(
        "CUDA {}, cuDNN {} [{platform}]: {:.0} МБ архивов NVIDIA -> {}",
        m.cuda,
        m.cudnn,
        m.total_size(platform) as f64 / 1e6,
        dir.display()
    );
    install_cuda(&m, platform, dir, &mut |l, done| {
        println!(
            "  {} {}: {}",
            l.component,
            l.version,
            if done {
                "уже установлено (штамп совпал)"
            } else {
                "загрузка"
            }
        )
    })?;
    let entries = cuda_entries(&m, platform, dir, manifest_path, licenses_dir)?;
    fs::create_dir_all(out_dir)?;
    let dest = out_dir.join(cuda_archive_name(version, platform));
    write_archive(&dest, CUDA_DIR, &entries)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::tests::{tar_entries, touch, work, zip_names};
    use std::io::Read as _;

    fn sha(b: &[u8]) -> String {
        let p = work("sha-helper").join("x");
        fs::write(&p, b).unwrap();
        sha256_file(&p).unwrap()
    }

    fn hf(file: &str) -> String {
        format!(
            "https://huggingface.co/o/r/resolve/0123456789abcdef0123456789abcdef01234567/{file}"
        )
    }

    /// Манифест: модель комплекта, модель вне комплекта и NC-модель.
    fn models_manifest(a: &[u8], b: &[u8]) -> String {
        let spec = "[model.spec]\ndim = 4\npooling = \"mean\"\nmax_tokens_passage = 8\n\
                    max_tokens_query = 8\n";
        let file = |name: &str, data: &[u8]| {
            format!(
                "{{ url = \"{}\", path = \"{name}\", size = {}, sha256 = \"{}\" }}",
                hf(name),
                data.len(),
                sha(data)
            )
        };
        format!(
            "[[model]]\nid = \"m1\"\nrole = \"embed\"\ndevice = \"cpu\"\nlicense = \"MIT\"\n\
             bundle = true\nfiles = [{}, {}]\n{spec}\n\
             [[model]]\nid = \"t1\"\nrole = \"test\"\ndevice = \"any\"\nlicense = \"MIT\"\n\
             bundle = false\nfiles = [{}, {}]\n{spec}\n\
             [[model]]\nid = \"nc1\"\nrole = \"embed\"\ndevice = \"cpu\"\n\
             license = \"CC-BY-NC-4.0\"\nbundle = false\nrequires_license_ack = true\n\
             files = [{}, {}]\n{spec}",
            file("model.onnx", a),
            file("tokenizer.json", b),
            file("model.onnx", a),
            file("tokenizer.json", b),
            file("model.onnx", a),
            file("tokenizer.json", b),
        )
    }

    /// Каталог модели как после `fetch-models`, плюс мусор: `*.part` и `*.tmp`.
    fn install_fake(models: &Path, m: &ModelEntry, a: &[u8], b: &[u8]) {
        let dir = models.join(&m.id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("model.onnx"), a).unwrap();
        fs::write(dir.join("tokenizer.json"), b).unwrap();
        fs::write(dir.join(MODEL_LICENSE_FILE), m.license_text()).unwrap();
        fs::write(dir.join(MODEL_SPEC_FILE), m.spec_toml().unwrap()).unwrap();
        fs::write(dir.join("model.onnx.4242.part"), b"partial").unwrap();
        fs::write(dir.join("junk.tmp"), b"tmp").unwrap();
    }

    #[test]
    fn zam_models_has_manifest_and_matching_sha() {
        let w = work("zam-models");
        let (a, b) = (b"ONNX-WEIGHTS".as_slice(), b"{\"tok\":1}".as_slice());
        let manifest = w.join("models.toml");
        fs::write(&manifest, models_manifest(a, b)).unwrap();
        let m = ModelsManifest::load(&manifest).unwrap();
        m.validate().unwrap();
        let models = w.join("models");
        for e in &m.model {
            install_fake(&models, e, a, b);
        }
        let dist = w.join("dist");
        let got = package_models(&manifest, &models, "1.2.3", &dist).unwrap();
        // только m1: t1 — bundle = false, nc1 — NC-лицензия
        assert_eq!(got, [dist.join("zam-models-1.2.3-m1.zip")]);
        assert_eq!(
            zip_names(&got[0]),
            [
                "m1/LICENSE",
                "m1/model.onnx",
                "m1/model.toml",
                "m1/tokenizer.json",
                "models.toml"
            ]
        );
        // models.toml в архиве — тот же манифест, хэши файлов архива совпадают с ним
        let mut z = zip::ZipArchive::new(fs::File::open(&got[0]).unwrap()).unwrap();
        let mut text = String::new();
        z.by_name("models.toml")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let inner = ModelsManifest::parse(&text).unwrap();
        let e = inner.get("m1").unwrap();
        for f in &e.files {
            let mut data = Vec::new();
            z.by_name(&format!("m1/{}", f.path))
                .unwrap()
                .read_to_end(&mut data)
                .unwrap();
            assert_eq!(data.len() as u64, f.size);
            assert_eq!(sha(&data), f.sha256, "{}", f.path);
        }

        // испорченный файл модели — ошибка с подсказкой, архив не пишется
        fs::write(models.join("m1").join("model.onnx"), b"ONNX-WEIGHTZ").unwrap();
        fs::remove_file(&got[0]).unwrap();
        let e = format!(
            "{:#}",
            package_models(&manifest, &models, "1.2.3", &dist).unwrap_err()
        );
        assert!(e.contains("SHA-256") && e.contains("fetch-models"), "{e}");
        assert!(!got[0].exists());
    }

    #[test]
    fn real_bundle_excludes_jina_and_each_archive_fits_release_limit() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/models.toml");
        let m = ModelsManifest::load(&p).unwrap();
        let ids: Vec<&str> = bundle_models(&m).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["bge-m3-int8", "bge-m3-fp16", "bge-reranker-v2-m3"]);
        assert!(!ids.iter().any(|id| id.contains("jina")));
        for e in bundle_models(&m) {
            // zip со сжатием не больше исходных данных (+ заголовки): запас до 2 ГиБ
            assert!(
                e.total_size() < crate::package::MAX_RELEASE_FILE - (64 << 20),
                "{}",
                e.id
            );
        }
    }

    /// Каталог `cuda/` как после `cuda fetch`: библиотеки и штампы для манифеста.
    fn cuda_fixture(w: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let ones = "1".repeat(64);
        let twos = "2".repeat(64);
        let text = format!(
            "cuda = \"13.4.3\"\ncudnn = \"9.27.0\"\n\
             [[lib]]\nplatform = \"linux-x64\"\ncomponent = \"cuda_cudart\"\nversion = \"1\"\n\
             url = \"https://developer.download.nvidia.com/compute/cuda/redist/a.tar.xz\"\n\
             size = 10\nsha256 = \"{ones}\"\nextract = [\"a/lib/libcudart.so.13\"]\n\
             [[lib]]\nplatform = \"linux-x64\"\ncomponent = \"cudnn\"\nversion = \"9\"\n\
             url = \"https://developer.download.nvidia.com/compute/cudnn/redist/b.tar.xz\"\n\
             size = 10\nsha256 = \"{twos}\"\n\
             extract = [\"b/lib/libcudnn.so.9\", \"b/lib/libcudnn_ops.so.9\"]\n\
             [[lib]]\nplatform = \"windows-x64\"\ncomponent = \"cuda_cudart\"\nversion = \"1\"\n\
             url = \"https://developer.download.nvidia.com/compute/cuda/redist/a.zip\"\n\
             size = 10\nsha256 = \"{ones}\"\nextract = [\"a/bin/x64/cudart64_13.dll\"]\n"
        );
        let manifest = w.join("cuda.toml");
        fs::write(&manifest, text).unwrap();
        let dir = w.join("ort").join(CUDA_DIR);
        for n in ["libcudart.so.13", "libcudnn.so.9", "libcudnn_ops.so.9"] {
            touch(&dir.join(n));
        }
        fs::write(dir.join(".cuda_cudart.sha256"), format!("{ones}\n")).unwrap();
        fs::write(dir.join(".cudnn.sha256"), format!("{twos}\n")).unwrap();
        // мусор прерванной загрузки и чужой файл в архив не попадают
        touch(&dir.join("libcudnn.so.9.4242.part"));
        touch(&dir.join("other.so"));
        let lic = w.join("licenses-nvidia");
        touch(&lic.join("NVIDIA-CUDA-EULA.txt"));
        (manifest, dir, lic)
    }

    #[test]
    fn zam_cuda_contains_stamps_and_unpacks_as_installed() {
        let w = work("zam-cuda");
        let (manifest, dir, lic) = cuda_fixture(&w);
        // штампы и файлы на месте — package_cuda ничего не скачивает
        let dist = w.join("dist");
        let got = package_cuda(&manifest, &lic, "linux-x64", &dir, "1.2.3", &dist).unwrap();
        assert_eq!(got, dist.join("zam-cuda-1.2.3-linux-x64.tar.gz"));
        let names: Vec<String> = tar_entries(&got).into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                "cuda/.cuda_cudart.sha256",
                "cuda/.cudnn.sha256",
                "cuda/LICENSES/NVIDIA-CUDA-EULA.txt",
                "cuda/cuda.toml",
                "cuda/libcudart.so.13",
                "cuda/libcudnn.so.9",
                "cuda/libcudnn_ops.so.9",
            ]
        );
        // распаковка в каталог zam-server: cuda fetch видит всё установленным
        let kit = w.join("kit");
        fs::create_dir_all(&kit).unwrap();
        tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(&got).unwrap()))
            .unpack(&kit)
            .unwrap();
        let m = CudaManifest::load(&manifest).unwrap();
        for l in m.libs("linux-x64") {
            assert!(
                is_installed(&l.entry(), &kit.join(CUDA_DIR), &l.component).unwrap(),
                "{}",
                l.component
            );
        }
        let r = install_cuda(&m, "linux-x64", &kit.join(CUDA_DIR), &mut |_, done| {
            assert!(done)
        })
        .unwrap();
        assert!(r.iter().all(|(_, cached)| *cached));
    }

    #[test]
    fn zam_cuda_refuses_stale_stamp() {
        let w = work("zam-cuda-stale");
        let (manifest, dir, lic) = cuda_fixture(&w);
        fs::write(dir.join(".cudnn.sha256"), format!("{}\n", "3".repeat(64))).unwrap();
        let m = CudaManifest::load(&manifest).unwrap();
        let e = format!(
            "{:#}",
            cuda_entries(&m, "linux-x64", &dir, &manifest, &lic).unwrap_err()
        );
        assert!(e.contains("cudnn") && e.contains("штамп"), "{e}");
        // нет лицензий NVIDIA — ошибка
        fs::write(dir.join(".cudnn.sha256"), format!("{}\n", "2".repeat(64))).unwrap();
        let empty = w.join("no-lic");
        fs::create_dir_all(&empty).unwrap();
        assert!(cuda_entries(&m, "linux-x64", &dir, &manifest, &empty).is_err());
    }

    #[test]
    fn archive_names() {
        assert_eq!(
            cuda_archive_name("1.2.3", "windows-x64"),
            "zam-cuda-1.2.3-windows-x64.zip"
        );
        assert_eq!(
            cuda_archive_name("1.2.3", "linux-x64"),
            "zam-cuda-1.2.3-linux-x64.tar.gz"
        );
        assert_eq!(
            models_archive_name("1.2.3", "bge-m3-int8"),
            "zam-models-1.2.3-bge-m3-int8.zip"
        );
    }
}
