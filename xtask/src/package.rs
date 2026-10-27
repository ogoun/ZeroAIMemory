//! Упаковка комплектов поставки: zip (Windows) и tar.gz (Linux) + SHA256SUMS.
//!
//! Комплект `zam-server-<ver>-<платформа>-<flavor>`: cpu — ONNX Runtime CPU, gpu — ONNX
//! Runtime с CUDA EP (`onnxruntime_providers_cuda`, `onnxruntime_providers_shared`) без
//! библиотек NVIDIA (их ставит `zam-server cuda fetch` или архив `zam-cuda`, см. `bundles`).
//! На Windows рядом с `zam-server.exe` лежит app-local MSVC runtime: его импортируют
//! библиотеки ONNX Runtime (сами `zam-server.exe` и `zam.exe` — со статическим CRT).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::fetch::sha256_file;
use crate::manifest::{Manifest, PLATFORMS};

/// Сборка комплекта сервера: ONNX Runtime CPU или с CUDA EP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Flavor {
    Cpu,
    Gpu,
}

impl Flavor {
    pub fn as_str(self) -> &'static str {
        match self {
            Flavor::Cpu => "cpu",
            Flavor::Gpu => "gpu",
        }
    }
}

/// Предел размера одного файла релиза GitHub (2 ГиБ).
pub const MAX_RELEASE_FILE: u64 = 2 * 1024 * 1024 * 1024;

/// Файл не больше `limit` байт, иначе ошибка с размером и подсказкой.
pub fn check_release_size(path: &Path, limit: u64) -> Result<()> {
    let len = fs::metadata(path)
        .with_context(|| format!("размер {}", path.display()))?
        .len();
    if len >= limit {
        bail!(
            "{}: {len} байт ({:.2} ГиБ) — не меньше предела {:.2} ГиБ на файл релиза GitHub; \
             разбейте содержимое на несколько архивов (zam-models — по одной модели на \
             архив) или уменьшите состав",
            path.display(),
            len as f64 / (1u64 << 30) as f64,
            limit as f64 / (1u64 << 30) as f64
        );
    }
    Ok(())
}

/// Всё, что нужно для сборки архивов; пути задаёт вызывающий (в тестах — фиктивные).
pub struct Inputs<'a> {
    pub platform: &'a str,
    pub version: &'a str,
    pub flavor: Flavor,
    pub server_bin: &'a Path,
    pub cli_bin: &'a Path,
    /// `zam` другой платформы ([`other_platform`]) для `GET /downloads/zam/{os}-{arch}`
    /// (§15.5): кладётся в `downloads/<платформа>/zam[.exe]` комплекта сервера. `None` —
    /// сервер раздаёт только клиент своей ОС.
    pub other_cli_bin: Option<&'a Path>,
    /// `target/deps/<platform>`
    pub deps_dir: &'a Path,
    /// `deploy/` (qdrant.toml, models.toml, cuda.toml, config.example.toml,
    /// README-install.md, systemd/, licenses/)
    pub deploy_dir: &'a Path,
    /// Сгенерированные сторонние лицензии (THIRD-PARTY-*).
    pub generated_licenses_dir: &'a Path,
    /// Лицензия проекта.
    pub project_license: &'a Path,
    /// Каталог `Microsoft.VC*.CRT` с файлами [`crt_files`] (только Windows).
    pub crt_dir: Option<&'a Path>,
    pub qdrant_version: &'a str,
    pub out_dir: &'a Path,
}

#[derive(Debug)]
pub(crate) struct Entry {
    /// Путь внутри корневого каталога архива, с `/`.
    pub(crate) path: String,
    pub(crate) src: PathBuf,
    pub(crate) exec: bool,
}

impl Entry {
    pub(crate) fn file(path: impl Into<String>, src: impl Into<PathBuf>) -> Self {
        Entry {
            path: path.into(),
            src: src.into(),
            exec: false,
        }
    }
}

pub(crate) fn is_windows(platform: &str) -> bool {
    platform.starts_with("windows")
}

/// Расширение архивов платформы: zip (Windows) или tar.gz (Linux).
pub fn archive_ext(platform: &str) -> &'static str {
    if is_windows(platform) {
        "zip"
    } else {
        "tar.gz"
    }
}

/// Библиотеки ONNX Runtime в корне комплекта и каталог `target/deps/<платформа>/<…>`.
pub fn ort_files(platform: &str, flavor: Flavor) -> (&'static str, &'static [&'static str]) {
    match (is_windows(platform), flavor) {
        (true, Flavor::Cpu) => ("onnxruntime", &["onnxruntime.dll"]),
        (false, Flavor::Cpu) => ("onnxruntime", &["libonnxruntime.so"]),
        (true, Flavor::Gpu) => (
            "onnxruntime-gpu",
            &[
                "onnxruntime.dll",
                "onnxruntime_providers_cuda.dll",
                "onnxruntime_providers_shared.dll",
            ],
        ),
        (false, Flavor::Gpu) => (
            "onnxruntime-gpu",
            &[
                "libonnxruntime.so",
                "libonnxruntime_providers_cuda.so",
                "libonnxruntime_providers_shared.so",
            ],
        ),
    }
}

/// App-local MSVC runtime в корне комплекта Windows. Его импортируют библиотеки ONNX
/// Runtime (`dumpbin /dependents`): `onnxruntime.dll` — MSVCP140, MSVCP140_1, VCRUNTIME140,
/// VCRUNTIME140_1; CUDA EP ещё MSVCP140_ATOMIC_WAIT. UCRT (`api-ms-win-crt-*`) входит в
/// Windows 10 и новее. Полноту проверяет smoke по таблицам импорта.
pub fn crt_files(flavor: Flavor) -> &'static [&'static str] {
    match flavor {
        Flavor::Cpu => &[
            "msvcp140.dll",
            "msvcp140_1.dll",
            "vcruntime140.dll",
            "vcruntime140_1.dll",
        ],
        Flavor::Gpu => &[
            "msvcp140.dll",
            "msvcp140_1.dll",
            "msvcp140_atomic_wait.dll",
            "vcruntime140.dll",
            "vcruntime140_1.dll",
        ],
    }
}

/// Вторая поддерживаемая платформа (клиент `zam` которой кладётся в комплект сервера).
pub fn other_platform(platform: &str) -> &'static str {
    if is_windows(platform) {
        "linux-x64"
    } else {
        "windows-x64"
    }
}

pub fn exe(platform: &str, name: &str) -> String {
    if is_windows(platform) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Версия вида 1.2.3 или 1.2.3-dev: цифра в начале, дальше буквы/цифры/`.-+`.
pub fn validate_version(v: &str) -> Result<()> {
    let ok = v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if !ok {
        bail!("недопустимая версия '{v}', ожидается X.Y.Z[-суффикс]");
    }
    Ok(())
}

fn push_dir_files(out: &mut Vec<Entry>, dir: &Path, prefix: &str) -> Result<()> {
    let mut names = Vec::new();
    for e in fs::read_dir(dir).with_context(|| format!("не удалось прочитать {}", dir.display()))?
    {
        let e = e?;
        if e.file_type()?.is_file() {
            names.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    for n in names {
        out.push(Entry {
            path: format!("{prefix}/{n}"),
            src: dir.join(&n),
            exec: false,
        });
    }
    Ok(())
}

fn server_entries(i: &Inputs) -> Result<Vec<Entry>> {
    let p = i.platform;
    let win = is_windows(p);
    let mut v = vec![
        Entry {
            path: exe(p, "zam-server"),
            src: i.server_bin.to_path_buf(),
            exec: true,
        },
        Entry {
            path: exe(p, "zam"),
            src: i.cli_bin.to_path_buf(),
            exec: true,
        },
    ];
    if let Some(other) = i.other_cli_bin {
        let op = other_platform(p);
        v.push(Entry {
            path: format!("downloads/{op}/{}", exe(op, "zam")),
            src: other.to_path_buf(),
            exec: true,
        });
    }
    let (ort_dir, ort) = ort_files(p, i.flavor);
    for n in ort {
        v.push(Entry::file(*n, i.deps_dir.join(ort_dir).join(n)));
    }
    let pdf = if win { "pdfium.dll" } else { "libpdfium.so" };
    v.push(Entry::file(pdf, i.deps_dir.join("pdfium").join(pdf)));
    let q = exe(p, "qdrant");
    v.push(Entry {
        path: format!("qdrant/{}/{q}", i.qdrant_version),
        src: i.deps_dir.join("qdrant").join(&q),
        exec: true,
    });
    if win {
        let crt = i
            .crt_dir
            .context("для windows-x64 нужен каталог MSVC runtime (Microsoft.VC*.CRT)")?;
        v.push(Entry::file(
            format!("qdrant/{}/vcruntime140.dll", i.qdrant_version),
            crt.join("vcruntime140.dll"),
        ));
        for n in crt_files(i.flavor) {
            v.push(Entry::file(*n, crt.join(n)));
        }
    }
    // Манифесты встроены в zam-server; копии в комплекте — для прозрачности (что и откуда
    // скачивают `models fetch` и `cuda fetch`).
    let mut copies = vec![
        ("deploy/qdrant.toml", "qdrant.toml"),
        ("deploy/models.toml", "models.toml"),
        ("config.example.toml", "config.example.toml"),
        ("README-install.md", "README-install.md"),
    ];
    if i.flavor == Flavor::Gpu {
        copies.push(("deploy/cuda.toml", "cuda.toml"));
    }
    for (rel, src) in copies {
        v.push(Entry::file(rel, i.deploy_dir.join(src)));
    }
    if !win {
        v.push(Entry::file(
            "systemd/zam-server.service",
            i.deploy_dir.join("systemd/zam-server.service"),
        ));
    }
    push_dir_files(&mut v, &i.deploy_dir.join("licenses"), "LICENSES")?;
    push_dir_files(&mut v, i.generated_licenses_dir, "LICENSES")?;
    v.push(Entry::file(
        "LICENSES/ZeroAIMemory-LICENSE.txt",
        i.project_license,
    ));
    Ok(v)
}

fn cli_entries(i: &Inputs) -> Vec<Entry> {
    vec![
        Entry {
            path: exe(i.platform, "zam"),
            src: i.cli_bin.to_path_buf(),
            exec: true,
        },
        Entry {
            path: "LICENSE.txt".into(),
            src: i.project_license.to_path_buf(),
            exec: false,
        },
    ]
}

fn check_sources(entries: &[Entry]) -> Result<()> {
    let missing: Vec<String> = entries
        .iter()
        .filter(|e| !e.src.is_file())
        .map(|e| format!("{} ({})", e.path, e.src.display()))
        .collect();
    if !missing.is_empty() {
        bail!("не найдены исходные файлы:\n  {}", missing.join("\n  "));
    }
    Ok(())
}

/// Путь записи в архиве: `<root>/<path>` или `<path>` при пустом `root`.
fn in_root(root: &str, path: &str) -> String {
    if root.is_empty() {
        path.to_string()
    } else {
        format!("{root}/{path}")
    }
}

fn write_zip(dest: &Path, root: &str, entries: &[Entry]) -> Result<()> {
    let mut z = zip::ZipWriter::new(File::create(dest)?);
    for e in entries {
        let mode = if e.exec { 0o755 } else { 0o644 };
        let len = fs::metadata(&e.src)?.len();
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode)
            .large_file(len >= u32::MAX as u64);
        z.start_file(in_root(root, &e.path), opts)?;
        io::copy(&mut File::open(&e.src)?, &mut z)?;
    }
    z.finish()?.flush()?;
    Ok(())
}

fn write_targz(dest: &Path, root: &str, entries: &[Entry]) -> Result<()> {
    let gz = flate2::write::GzEncoder::new(File::create(dest)?, flate2::Compression::default());
    let mut t = tar::Builder::new(gz);
    for e in entries {
        let mut f = File::open(&e.src)?;
        let mut h = tar::Header::new_gnu();
        h.set_size(f.metadata()?.len());
        h.set_mode(if e.exec { 0o755 } else { 0o644 });
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        h.set_entry_type(tar::EntryType::Regular);
        t.append_data(&mut h, in_root(root, &e.path), &mut f)?;
    }
    t.into_inner()?.finish()?.flush()?;
    Ok(())
}

/// Пишет архив `dest` (zip, если имя оканчивается на `.zip`, иначе tar.gz) через `*.part`.
/// Архив не меньше [`MAX_RELEASE_FILE`] — ошибка, файл удаляется.
pub(crate) fn write_archive(dest: &Path, root: &str, entries: &[Entry]) -> Result<()> {
    write_archive_limited(dest, root, entries, MAX_RELEASE_FILE)
}

pub(crate) fn write_archive_limited(
    dest: &Path,
    root: &str,
    entries: &[Entry],
    limit: u64,
) -> Result<()> {
    check_sources(entries)?;
    let part = dest.with_extension("part");
    let zip = dest.to_string_lossy().ends_with(".zip");
    let r = if zip {
        write_zip(&part, root, entries)
    } else {
        write_targz(&part, root, entries)
    }
    .and_then(|()| {
        check_release_size(&part, limit).map_err(|e| anyhow::anyhow!("{}: {e:#}", dest.display()))
    });
    if let Err(e) = r {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    if dest.exists() {
        fs::remove_file(dest)?;
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Собирает комплект сервера и (для cpu) архив клиента; возвращает пути архивов.
/// Архив клиента от flavor не зависит, поэтому собирается только с cpu.
pub fn package(i: &Inputs) -> Result<Vec<PathBuf>> {
    if !PLATFORMS.contains(&i.platform) {
        bail!(
            "неизвестная платформа '{}', допустимы: {}",
            i.platform,
            PLATFORMS.join(", ")
        );
    }
    validate_version(i.version)?;
    fs::create_dir_all(i.out_dir)?;
    let ext = archive_ext(i.platform);
    let server_root = format!(
        "zam-server-{}-{}-{}",
        i.version,
        i.platform,
        i.flavor.as_str()
    );
    let server = i.out_dir.join(format!("{server_root}.{ext}"));
    write_archive(&server, &server_root, &server_entries(i)?)?;
    let mut out = vec![server];
    if i.flavor == Flavor::Cpu {
        let cli_root = format!("zam-{}-{}", i.version, i.platform);
        let cli = i.out_dir.join(format!("{cli_root}.{ext}"));
        write_archive(&cli, &cli_root, &cli_entries(i))?;
        out.push(cli);
    }
    Ok(out)
}

/// SHA256SUMS в формате `sha256sum` строго для перечисленных архивов (лежат в `dist`).
/// Чужие архивы в каталоге (другая платформа) в файл не попадают.
pub fn write_sha256sums(dist: &Path, archives: &[PathBuf]) -> Result<PathBuf> {
    let mut names: Vec<String> = Vec::new();
    for a in archives {
        let n = a
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("некорректный путь архива {}", a.display()))?;
        names.push(n.to_string());
    }
    names.sort();
    let mut text = String::new();
    for n in &names {
        text.push_str(&format!("{}  {n}\n", sha256_file(&dist.join(n))?));
    }
    let out = dist.join("SHA256SUMS");
    fs::write(&out, text)?;
    Ok(out)
}

// ---- поиск MSVC runtime (Microsoft.VC*.CRT) ----

fn version_key(name: &str) -> Vec<u64> {
    name.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Каталог `<vs>\VC\Redist\MSVC\<версия>\x64\Microsoft.VC*.CRT` с наибольшей версией, в
/// котором есть все файлы `need`.
fn crt_dir_in(vs_root: &Path, need: &[&str]) -> Result<PathBuf> {
    let msvc = vs_root.join("VC").join("Redist").join("MSVC");
    let mut vers: Vec<String> = fs::read_dir(&msvc)
        .with_context(|| format!("нет каталога {}", msvc.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("x64").is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    vers.sort_by_key(|v| version_key(v));
    for v in vers.iter().rev() {
        let x64 = msvc.join(v).join("x64");
        let mut crts: Vec<PathBuf> = fs::read_dir(&x64)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("Microsoft.VC") && n.ends_with(".CRT"))
            })
            .collect();
        crts.sort();
        for c in crts.iter().rev() {
            if need.iter().all(|n| c.join(n).is_file()) {
                return Ok(c.clone());
            }
        }
    }
    bail!(
        "в {} нет каталога Microsoft.VC*.CRT со всеми файлами: {}",
        msvc.display(),
        need.join(", ")
    )
}

/// Каталог установки Visual Studio с VC Tools (vswhere).
fn vs_install_path() -> Result<PathBuf> {
    let vswhere =
        Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    if !vswhere.is_file() {
        bail!("не найден vswhere (Visual Studio не установлена)");
    }
    let out = Command::new(vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .context("запуск vswhere")?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        bail!("vswhere не нашёл Visual Studio с компонентом VC Tools");
    }
    Ok(PathBuf::from(path))
}

/// Каталог MSVC runtime с файлами [`crt_files`] (и `vcruntime140.dll` для Qdrant):
/// `ZAM_VC_CRT_DIR` или Visual Studio через vswhere.
pub fn find_crt_dir(flavor: Flavor) -> Result<PathBuf> {
    let need = crt_files(flavor);
    if let Some(p) = std::env::var_os("ZAM_VC_CRT_DIR") {
        let p = PathBuf::from(p);
        let missing: Vec<&str> = need
            .iter()
            .copied()
            .filter(|n| !p.join(n).is_file())
            .collect();
        if !missing.is_empty() {
            bail!(
                "ZAM_VC_CRT_DIR={}: нет файлов {}",
                p.display(),
                missing.join(", ")
            );
        }
        return Ok(p);
    }
    let vs = vs_install_path().context("MSVC runtime не найден; задайте ZAM_VC_CRT_DIR")?;
    crt_dir_in(&vs, need)
}

/// `<vs>\VC\Tools\MSVC\<наибольшая версия>\bin\Hostx64\x64\dumpbin.exe`
fn dumpbin_in(vs_root: &Path) -> Result<PathBuf> {
    let msvc = vs_root.join("VC").join("Tools").join("MSVC");
    let mut vers: Vec<String> = fs::read_dir(&msvc)
        .with_context(|| format!("нет каталога {}", msvc.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    vers.sort_by_key(|v| version_key(v));
    for v in vers.iter().rev() {
        let p = msvc
            .join(v)
            .join("bin")
            .join("Hostx64")
            .join("x64")
            .join("dumpbin.exe");
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("dumpbin.exe не найден в {}", msvc.display())
}

pub fn find_dumpbin() -> Result<PathBuf> {
    dumpbin_in(&vs_install_path()?)
}

/// Имена DLL из вывода `dumpbin /dependents`, которых не должно быть у exe со статическим CRT.
pub fn forbidden_imports(dumpbin_output: &str) -> Vec<String> {
    dumpbin_output
        .lines()
        .map(str::trim)
        .filter(|l| l.to_ascii_lowercase().ends_with(".dll") && !l.contains(' '))
        .filter(|l| {
            let n = l.to_ascii_lowercase();
            ["vcruntime", "msvcp", "msvcr", "api-ms-win-crt", "ucrtbase"]
                .iter()
                .any(|p| n.starts_with(p))
        })
        .map(str::to_string)
        .collect()
}

/// Импорты MSVC runtime (`vcruntime*`, `msvcp*`, `concrt*`) из вывода `dumpbin /dependents`,
/// которых нет среди `present` (имена файлов рядом, без учёта регистра). Такая библиотека на
/// чистой Windows без VC++ Redistributable не загрузится. UCRT (`api-ms-win-crt-*`) — часть
/// Windows 10 и новее, его не требуем.
pub fn missing_app_local(dumpbin_output: &str, present: &[String]) -> Vec<String> {
    let have: Vec<String> = present.iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut v: Vec<String> = dumpbin_output
        .lines()
        .map(str::trim)
        .filter(|l| l.to_ascii_lowercase().ends_with(".dll") && !l.contains(' '))
        .map(str::to_ascii_lowercase)
        .filter(|n| {
            ["vcruntime", "msvcp", "concrt"]
                .iter()
                .any(|p| n.starts_with(p))
        })
        .filter(|n| !have.contains(n))
        .collect();
    v.sort();
    v.dedup();
    v
}

// ---- лицензии ----

fn run(cmd: &mut Command, what: &str) -> Result<std::process::Output> {
    let out = cmd.output().with_context(|| format!("запуск {what}"))?;
    if !out.status.success() {
        bail!(
            "{what} завершился с ошибкой {}:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out)
}

/// Удаляет из вывода license-checker локальные пути (ключи `path`, `licenseFile`).
fn strip_local_paths(json: &str) -> Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(json).context("вывод license-checker")?;
    if let Some(map) = v.as_object_mut() {
        for pkg in map.values_mut() {
            if let Some(o) = pkg.as_object_mut() {
                o.remove("path");
                o.remove("licenseFile");
            }
        }
    }
    Ok(serde_json::to_string_pretty(&v)?)
}

/// Закреплённая версия license-checker (воспроизводимость сборки лицензий).
const LICENSE_CHECKER: &str = "license-checker-rseidelsohn@5.0.1";

/// `name@version` самого web-пакета из package.json (исключается из отчёта о лицензиях).
fn web_package_id(package_json: &Path) -> Result<String> {
    let text = fs::read_to_string(package_json)
        .with_context(|| format!("чтение {}", package_json.display()))?;
    package_id_from_json(&text).with_context(|| format!("разбор {}", package_json.display()))
}

fn package_id_from_json(json: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let name = v["name"].as_str().context("нет поля name")?;
    let version = v["version"].as_str().context("нет поля version")?;
    Ok(format!("{name}@{version}"))
}

/// Тексты лицензий пакетов, попавших в бандл WebUI (rollup-plugin-license при `npm run build`).
const WEB_NOTICES: &str = "THIRD-PARTY-NOTICES.txt";
/// Имя этого файла в `LICENSES/` комплекта.
const WEB_NOTICES_OUT: &str = "THIRD-PARTY-WEB-NOTICES.txt";

/// Копирует `web/dist/THIRD-PARTY-NOTICES.txt` в каталог сгенерированных лицензий.
fn copy_web_notices(web_dist: &Path, out: &Path) -> Result<()> {
    let src = web_dist.join(WEB_NOTICES);
    if !src.is_file() {
        bail!(
            "нет {}: соберите WebUI (npm run build в web/)",
            src.display()
        );
    }
    fs::copy(&src, out.join(WEB_NOTICES_OUT))
        .with_context(|| format!("копирование {}", src.display()))?;
    Ok(())
}

/// Генерирует THIRD-PARTY-RUST.html (cargo-about), THIRD-PARTY-WEB.json (license-checker)
/// и копирует THIRD-PARTY-WEB-NOTICES.txt (тексты лицензий бандла WebUI).
pub fn generate_licenses(root: &Path, out: &Path) -> Result<()> {
    if out.exists() {
        fs::remove_dir_all(out)?;
    }
    fs::create_dir_all(out)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run(
        Command::new(cargo)
            .current_dir(root)
            .arg("about")
            .arg("generate")
            .arg("about.hbs")
            .arg("-o")
            .arg(out.join("THIRD-PARTY-RUST.html")),
        "cargo about (установка: cargo install cargo-about --locked --features cli)",
    )?;
    // В бандл WebUI входят пакеты из devDependencies (сборка vite), поэтому проверяем всё дерево.
    let npx = if cfg!(windows) { "npx.cmd" } else { "npx" };
    let web_pkg = web_package_id(&root.join("web").join("package.json"))?;
    let o = run(
        Command::new(npx).current_dir(root.join("web")).args([
            "--yes",
            LICENSE_CHECKER,
            "--json",
            "--excludePackages",
            &web_pkg,
        ]),
        "npx license-checker-rseidelsohn",
    )?;
    fs::write(
        out.join("THIRD-PARTY-WEB.json"),
        strip_local_paths(&String::from_utf8_lossy(&o.stdout))?,
    )?;
    copy_web_notices(&root.join("web").join("dist"), out)
}

/// Версия Qdrant из `deploy/qdrant.toml`.
pub fn qdrant_version(deploy: &Path) -> Result<String> {
    Ok(Manifest::load(&deploy.join("qdrant.toml"))?.version)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn work(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/xtask")
            .join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    pub(crate) fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    /// Фиктивное дерево исходников: (deps, deploy, generated, crt, dist).
    fn fixture(w: &Path, platform: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let deps = w.join("deps");
        for flavor in [Flavor::Cpu, Flavor::Gpu] {
            let (dir, names) = ort_files(platform, flavor);
            for n in names {
                touch(&deps.join(dir).join(n));
            }
        }
        // библиотеки NVIDIA рядом с ORT GPU (после cuda fetch) в комплект не попадают
        touch(&deps.join("onnxruntime-gpu/cuda/cudnn64_9.dll"));
        touch(&deps.join("onnxruntime-gpu/cuda/libcudnn.so.9"));
        let pdf = if is_windows(platform) {
            "pdfium.dll"
        } else {
            "libpdfium.so"
        };
        touch(&deps.join("pdfium").join(pdf));
        touch(&deps.join("qdrant").join(exe(platform, "qdrant")));
        let deploy = w.join("deploy");
        for f in [
            "qdrant.toml",
            "models.toml",
            "cuda.toml",
            "config.example.toml",
            "README-install.md",
            "systemd/zam-server.service",
            "licenses/A.txt",
            "licenses/B.txt",
            // подкаталог (лицензии NVIDIA для zam-cuda) в комплект сервера не входит
            "licenses/nvidia/NVIDIA-CUDA-EULA.txt",
        ] {
            touch(&deploy.join(f));
        }
        let gen_dir = w.join("gen");
        touch(&gen_dir.join("THIRD-PARTY-RUST.html"));
        touch(&gen_dir.join("THIRD-PARTY-WEB.json"));
        for f in ["zam-server.bin", "zam.bin", "LICENSE"] {
            touch(&w.join(f));
        }
        let crt = w.join("crt");
        for n in crt_files(Flavor::Gpu) {
            touch(&crt.join(n));
        }
        (deps, deploy, gen_dir, crt, w.join("dist"))
    }

    fn run_package(
        w: &Path,
        platform: &str,
        flavor: Flavor,
        with_crt: bool,
    ) -> Result<Vec<PathBuf>> {
        run_package_with(w, platform, flavor, with_crt, None)
    }

    fn run_package_with(
        w: &Path,
        platform: &str,
        flavor: Flavor,
        with_crt: bool,
        other_cli_bin: Option<&Path>,
    ) -> Result<Vec<PathBuf>> {
        let (deps, deploy, gen_dir, crt, dist) = fixture(w, platform);
        let server_bin = w.join("zam-server.bin");
        let cli_bin = w.join("zam.bin");
        let lic = w.join("LICENSE");
        package(&Inputs {
            platform,
            version: "1.2.3",
            flavor,
            server_bin: &server_bin,
            cli_bin: &cli_bin,
            other_cli_bin,
            deps_dir: &deps,
            deploy_dir: &deploy,
            generated_licenses_dir: &gen_dir,
            project_license: &lic,
            crt_dir: with_crt.then_some(crt.as_path()),
            qdrant_version: "1.19.2",
            out_dir: &dist,
        })
    }

    pub(crate) fn zip_names(p: &Path) -> Vec<String> {
        let mut z = zip::ZipArchive::new(File::open(p).unwrap()).unwrap();
        let mut v: Vec<String> = (0..z.len())
            .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        v.sort();
        v
    }

    pub(crate) fn tar_entries(p: &Path) -> Vec<(String, u32)> {
        let gz = flate2::read::GzDecoder::new(File::open(p).unwrap());
        let mut t = tar::Archive::new(gz);
        let mut v: Vec<(String, u32)> = t
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.path().unwrap().to_string_lossy().into_owned(),
                    e.header().mode().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    }

    fn sorted(r: &str, items: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = items.iter().map(|p| format!("{r}/{p}")).collect();
        v.sort();
        v
    }

    const WIN_COMMON: &[&str] = &[
        "zam-server.exe",
        "zam.exe",
        "pdfium.dll",
        "qdrant/1.19.2/qdrant.exe",
        "qdrant/1.19.2/vcruntime140.dll",
        "msvcp140.dll",
        "msvcp140_1.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "deploy/qdrant.toml",
        "deploy/models.toml",
        "config.example.toml",
        "README-install.md",
        "LICENSES/A.txt",
        "LICENSES/B.txt",
        "LICENSES/THIRD-PARTY-RUST.html",
        "LICENSES/THIRD-PARTY-WEB.json",
        "LICENSES/ZeroAIMemory-LICENSE.txt",
    ];

    #[test]
    fn package_includes_other_platform_cli_for_downloads() {
        let w = work("layout-cli-other");
        let other = w.join("zam-linux.bin");
        touch(&other);
        let got = run_package_with(&w, "windows-x64", Flavor::Cpu, true, Some(&other)).unwrap();
        let names = zip_names(&got[0]);
        assert!(
            names.contains(&"zam-server-1.2.3-windows-x64-cpu/downloads/linux-x64/zam".to_string()),
            "{names:?}"
        );
        // архив клиента — только своей платформы
        assert_eq!(
            zip_names(&got[1]),
            sorted("zam-1.2.3-windows-x64", &["LICENSE.txt", "zam.exe"])
        );
        let w = work("layout-cli-other-linux");
        let other = w.join("zam-win.bin");
        touch(&other);
        let got = run_package_with(&w, "linux-x64", Flavor::Cpu, false, Some(&other)).unwrap();
        assert!(
            tar_entries(&got[0]).contains(&(
                "zam-server-1.2.3-linux-x64-cpu/downloads/windows-x64/zam.exe".to_string(),
                0o755
            )),
            "{:?}",
            tar_entries(&got[0])
        );
        // нет файла — ошибка упаковки, а не комплект без клиента
        let w = work("layout-cli-other-missing");
        let missing = w.join("nope.bin");
        assert!(run_package_with(&w, "linux-x64", Flavor::Cpu, false, Some(&missing)).is_err());
    }

    #[test]
    fn package_layout_windows_cpu_and_gpu() {
        let w = work("layout-win");
        let got = run_package(&w, "windows-x64", Flavor::Cpu, true).unwrap();
        assert_eq!(got.len(), 2);
        let (server, cli) = (&got[0], &got[1]);
        assert_eq!(
            server.file_name().unwrap(),
            "zam-server-1.2.3-windows-x64-cpu.zip"
        );
        let mut items = WIN_COMMON.to_vec();
        items.push("onnxruntime.dll");
        assert_eq!(
            zip_names(server),
            sorted("zam-server-1.2.3-windows-x64-cpu", &items)
        );
        assert_eq!(
            zip_names(cli),
            sorted("zam-1.2.3-windows-x64", &["LICENSE.txt", "zam.exe"])
        );

        // gpu: ORT с CUDA EP, msvcp140_atomic_wait.dll и cuda.toml; без библиотек NVIDIA;
        // архив клиента — только из cpu
        let w = work("layout-win-gpu");
        let got = run_package(&w, "windows-x64", Flavor::Gpu, true).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].file_name().unwrap(),
            "zam-server-1.2.3-windows-x64-gpu.zip"
        );
        let mut items = WIN_COMMON.to_vec();
        items.extend([
            "onnxruntime.dll",
            "onnxruntime_providers_cuda.dll",
            "onnxruntime_providers_shared.dll",
            "msvcp140_atomic_wait.dll",
            "deploy/cuda.toml",
        ]);
        let names = zip_names(&got[0]);
        assert_eq!(names, sorted("zam-server-1.2.3-windows-x64-gpu", &items));
        assert!(
            names
                .iter()
                .any(|n| n.ends_with("/onnxruntime_providers_cuda.dll"))
        );
        for bad in ["cudnn", "cublas", "cudart", "cufft"] {
            assert!(!names.iter().any(|n| n.contains(bad)), "{bad}");
        }
    }

    #[test]
    fn package_layout_linux_cpu_and_gpu() {
        let common: &[(&str, u32)] = &[
            ("zam-server", 0o755),
            ("zam", 0o755),
            ("libpdfium.so", 0o644),
            ("qdrant/1.19.2/qdrant", 0o755),
            ("deploy/qdrant.toml", 0o644),
            ("deploy/models.toml", 0o644),
            ("config.example.toml", 0o644),
            ("README-install.md", 0o644),
            ("systemd/zam-server.service", 0o644),
            ("LICENSES/A.txt", 0o644),
            ("LICENSES/B.txt", 0o644),
            ("LICENSES/THIRD-PARTY-RUST.html", 0o644),
            ("LICENSES/THIRD-PARTY-WEB.json", 0o644),
            ("LICENSES/ZeroAIMemory-LICENSE.txt", 0o644),
        ];
        let want = |r: &str, extra: &[(&str, u32)]| {
            let mut v: Vec<(String, u32)> = common
                .iter()
                .chain(extra)
                .map(|(p, m)| (format!("{r}/{p}"), *m))
                .collect();
            v.sort();
            v
        };
        let w = work("layout-linux");
        let got = run_package(&w, "linux-x64", Flavor::Cpu, false).unwrap();
        let (server, cli) = (&got[0], &got[1]);
        assert_eq!(
            server.file_name().unwrap(),
            "zam-server-1.2.3-linux-x64-cpu.tar.gz"
        );
        assert_eq!(
            tar_entries(server),
            want(
                "zam-server-1.2.3-linux-x64-cpu",
                &[("libonnxruntime.so", 0o644)]
            )
        );
        assert_eq!(
            tar_entries(cli),
            vec![
                ("zam-1.2.3-linux-x64/LICENSE.txt".to_string(), 0o644),
                ("zam-1.2.3-linux-x64/zam".to_string(), 0o755)
            ]
        );

        let w = work("layout-linux-gpu");
        let got = run_package(&w, "linux-x64", Flavor::Gpu, false).unwrap();
        assert_eq!(got.len(), 1);
        let entries = tar_entries(&got[0]);
        assert_eq!(
            entries,
            want(
                "zam-server-1.2.3-linux-x64-gpu",
                &[
                    ("libonnxruntime.so", 0o644),
                    ("libonnxruntime_providers_cuda.so", 0o644),
                    ("libonnxruntime_providers_shared.so", 0o644),
                    ("deploy/cuda.toml", 0o644),
                ]
            )
        );
        assert!(!entries.iter().any(|(n, _)| n.contains("cudnn")));
    }

    #[test]
    fn sha256sums_and_windows_requires_crt() {
        let w = work("sums");
        let got = run_package(&w, "linux-x64", Flavor::Cpu, false).unwrap();
        // Архив другой платформы в том же dist в SHA256SUMS не попадает.
        let foreign = w.join("dist").join("zam-9.9.9-windows-x64.zip");
        fs::write(&foreign, b"x").unwrap();
        let sums = write_sha256sums(&w.join("dist"), &got).unwrap();
        let text = fs::read_to_string(sums).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for l in lines {
            let (h, n) = l.split_once("  ").unwrap();
            assert_eq!(h.len(), 64);
            assert!(n.starts_with("zam") && !n.contains("9.9.9"), "{n}");
        }
        let w = work("nocrt");
        let e = run_package(&w, "windows-x64", Flavor::Cpu, false).unwrap_err();
        assert!(format!("{e:#}").contains("Microsoft.VC"), "{e:#}");
    }

    #[test]
    fn release_size_limit_is_enforced() {
        let w = work("size-limit");
        let f = w.join("a.bin");
        fs::write(&f, vec![0u8; 100]).unwrap();
        check_release_size(&f, 101).unwrap();
        let e = format!("{:#}", check_release_size(&f, 100).unwrap_err());
        assert!(e.contains("ГиБ") && e.contains("разбейте"), "{e}");
        // архив не меньше предела не остаётся в dist (и *.part тоже)
        let src = w.join("big.bin");
        let data: Vec<u8> = (0..200_000u32).flat_map(|x| x.to_le_bytes()).collect();
        fs::write(&src, data).unwrap();
        let dest = w.join("out.zip");
        let entries = [Entry::file("big.bin", &src)];
        let e = write_archive_limited(&dest, "r", &entries, 1000).unwrap_err();
        assert!(format!("{e:#}").contains("out.zip"), "{e:#}");
        assert!(!dest.exists());
        assert!(!w.join("out.part").exists());
        write_archive_limited(&dest, "r", &entries, MAX_RELEASE_FILE).unwrap();
        assert_eq!(zip_names(&dest), ["r/big.bin"]);
    }

    #[test]
    fn web_package_id_reads_name_and_version() {
        assert_eq!(
            package_id_from_json(r#"{"name":"zam-web","version":"0.1.0"}"#).unwrap(),
            "zam-web@0.1.0"
        );
        assert!(package_id_from_json(r#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn web_notices_copied_or_missing_is_error() {
        let w = work("web-notices");
        let (dist, out) = (w.join("dist"), w.join("out"));
        fs::create_dir_all(&out).unwrap();
        let e = copy_web_notices(&dist, &out).unwrap_err();
        assert!(format!("{e:#}").contains("npm run build"), "{e:#}");
        touch(&dist.join(WEB_NOTICES));
        copy_web_notices(&dist, &out).unwrap();
        assert!(out.join(WEB_NOTICES_OUT).is_file());
    }

    #[test]
    fn forbidden_imports_detects_dynamic_crt() {
        let ok = "Image has the following dependencies:\n\n    KERNEL32.dll\n    bcrypt.dll\n\n  Summary\n";
        assert!(forbidden_imports(ok).is_empty());
        let bad = concat!(
            "    KERNEL32.dll\n",
            "    VCRUNTIME140.dll\n",
            "    api-ms-win-crt-runtime-l1-1-0.dll\n",
            "    MSVCP140.dll\n",
        );
        assert_eq!(forbidden_imports(bad).len(), 3);
    }

    #[test]
    fn missing_app_local_lists_absent_msvc_runtime() {
        // фрагмент dumpbin /dependents onnxruntime_providers_cuda.dll (ORT 1.31 gpu_cuda13)
        let out = concat!(
            "Dump of file onnxruntime_providers_cuda.dll\n\n",
            "  Image has the following dependencies:\n\n",
            "    cublasLt64_13.dll\n",
            "    KERNEL32.dll\n",
            "    MSVCP140.dll\n",
            "    MSVCP140_ATOMIC_WAIT.dll\n",
            "    VCRUNTIME140.dll\n",
            "    VCRUNTIME140_1.dll\n",
            "    api-ms-win-crt-runtime-l1-1-0.dll\n",
        );
        let names =
            |f: Flavor| -> Vec<String> { crt_files(f).iter().map(|s| s.to_string()).collect() };
        assert_eq!(
            missing_app_local(out, &names(Flavor::Cpu)),
            ["msvcp140_atomic_wait.dll"]
        );
        assert!(missing_app_local(out, &names(Flavor::Gpu)).is_empty());
    }

    #[test]
    fn dumpbin_picks_highest_version() {
        let w = work("dumpbin");
        for v in ["14.9.1", "14.44.35207"] {
            touch(&w.join(format!("VC/Tools/MSVC/{v}/bin/Hostx64/x64/dumpbin.exe")));
        }
        assert!(
            dumpbin_in(&w)
                .unwrap()
                .to_string_lossy()
                .contains("14.44.35207")
        );
    }

    #[test]
    fn version_validation() {
        assert!(validate_version("0.0.0-dev").is_ok());
        assert!(validate_version("1.2.3").is_ok());
        assert!(validate_version("").is_err());
        assert!(validate_version("../1").is_err());
        assert!(validate_version("v1.2.3").is_err());
    }

    #[test]
    fn crt_dir_picks_highest_complete_version() {
        let w = work("vc");
        for v in ["14.9.1", "14.44.35208", "14.8.9"] {
            for n in crt_files(Flavor::Cpu) {
                touch(&w.join(format!("VC/Redist/MSVC/{v}/x64/Microsoft.VC143.CRT/{n}")));
            }
        }
        // msvcp140_atomic_wait.dll есть только в 14.9.1
        touch(&w.join("VC/Redist/MSVC/14.9.1/x64/Microsoft.VC143.CRT/msvcp140_atomic_wait.dll"));
        let p = crt_dir_in(&w, crt_files(Flavor::Cpu)).unwrap();
        assert!(p.to_string_lossy().contains("14.44.35208"), "{p:?}");
        let p = crt_dir_in(&w, crt_files(Flavor::Gpu)).unwrap();
        assert!(p.to_string_lossy().contains("14.9.1"), "{p:?}");
        let e = crt_dir_in(&w, &["concrt140.dll"]).unwrap_err();
        assert!(format!("{e:#}").contains("concrt140.dll"), "{e:#}");
    }
}
