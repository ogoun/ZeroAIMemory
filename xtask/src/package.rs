//! Упаковка комплектов поставки: zip (Windows) и tar.gz (Linux) + SHA256SUMS.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::fetch::sha256_file;
use crate::manifest::{Manifest, PLATFORMS};

/// Всё, что нужно для сборки архивов; пути задаёт вызывающий (в тестах — фиктивные).
pub struct Inputs<'a> {
    pub platform: &'a str,
    pub version: &'a str,
    pub server_bin: &'a Path,
    pub cli_bin: &'a Path,
    /// `target/deps/<platform>`
    pub deps_dir: &'a Path,
    /// `deploy/` (qdrant.toml, config.example.toml, README-install.md, systemd/, licenses/)
    pub deploy_dir: &'a Path,
    /// Сгенерированные сторонние лицензии (THIRD-PARTY-*).
    pub generated_licenses_dir: &'a Path,
    /// Лицензия проекта.
    pub project_license: &'a Path,
    /// vcruntime140.dll (только Windows).
    pub vcruntime: Option<&'a Path>,
    pub qdrant_version: &'a str,
    pub out_dir: &'a Path,
}

struct Entry {
    /// Путь внутри корневого каталога архива, с `/`.
    path: String,
    src: PathBuf,
    exec: bool,
}

fn is_windows(platform: &str) -> bool {
    platform.starts_with("windows")
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
    let (ort, pdf) = if win {
        ("onnxruntime.dll", "pdfium.dll")
    } else {
        ("libonnxruntime.so", "libpdfium.so")
    };
    v.push(Entry {
        path: ort.into(),
        src: i.deps_dir.join("onnxruntime").join(ort),
        exec: false,
    });
    v.push(Entry {
        path: pdf.into(),
        src: i.deps_dir.join("pdfium").join(pdf),
        exec: false,
    });
    let q = exe(p, "qdrant");
    v.push(Entry {
        path: format!("qdrant/{}/{q}", i.qdrant_version),
        src: i.deps_dir.join("qdrant").join(&q),
        exec: true,
    });
    if win {
        let vc = i
            .vcruntime
            .context("для windows-x64 нужен vcruntime140.dll")?;
        v.push(Entry {
            path: format!("qdrant/{}/vcruntime140.dll", i.qdrant_version),
            src: vc.to_path_buf(),
            exec: false,
        });
    }
    for (rel, src) in [
        ("deploy/qdrant.toml", "qdrant.toml"),
        ("config.example.toml", "config.example.toml"),
        ("README-install.md", "README-install.md"),
    ] {
        v.push(Entry {
            path: rel.into(),
            src: i.deploy_dir.join(src),
            exec: false,
        });
    }
    if !win {
        v.push(Entry {
            path: "systemd/zam-server.service".into(),
            src: i.deploy_dir.join("systemd/zam-server.service"),
            exec: false,
        });
    }
    push_dir_files(&mut v, &i.deploy_dir.join("licenses"), "LICENSES")?;
    push_dir_files(&mut v, i.generated_licenses_dir, "LICENSES")?;
    v.push(Entry {
        path: "LICENSES/ZeroAIMemory-LICENSE.txt".into(),
        src: i.project_license.to_path_buf(),
        exec: false,
    });
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

fn write_zip(dest: &Path, root: &str, entries: &[Entry]) -> Result<()> {
    let mut z = zip::ZipWriter::new(File::create(dest)?);
    for e in entries {
        let mode = if e.exec { 0o755 } else { 0o644 };
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode);
        z.start_file(format!("{root}/{}", e.path), opts)?;
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
        t.append_data(&mut h, format!("{root}/{}", e.path), &mut f)?;
    }
    t.into_inner()?.finish()?.flush()?;
    Ok(())
}

fn write_archive(platform: &str, dest: &Path, root: &str, entries: &[Entry]) -> Result<()> {
    check_sources(entries)?;
    let part = dest.with_extension("part");
    let r = if is_windows(platform) {
        write_zip(&part, root, entries)
    } else {
        write_targz(&part, root, entries)
    };
    if let Err(e) = r {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Собирает оба архива; возвращает их пути (сервер, клиент).
pub fn package(i: &Inputs) -> Result<(PathBuf, PathBuf)> {
    if !PLATFORMS.contains(&i.platform) {
        bail!(
            "неизвестная платформа '{}', допустимы: {}",
            i.platform,
            PLATFORMS.join(", ")
        );
    }
    validate_version(i.version)?;
    fs::create_dir_all(i.out_dir)?;
    let ext = if is_windows(i.platform) {
        "zip"
    } else {
        "tar.gz"
    };
    let server_root = format!("zam-server-{}-{}-cpu", i.version, i.platform);
    let cli_root = format!("zam-{}-{}", i.version, i.platform);
    let server = i.out_dir.join(format!("{server_root}.{ext}"));
    let cli = i.out_dir.join(format!("{cli_root}.{ext}"));
    write_archive(i.platform, &server, &server_root, &server_entries(i)?)?;
    write_archive(i.platform, &cli, &cli_root, &cli_entries(i))?;
    Ok((server, cli))
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

// ---- поиск vcruntime140.dll ----

fn version_key(name: &str) -> Vec<u64> {
    name.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// `<vs>\VC\Redist\MSVC\<наибольшая версия>\x64\Microsoft.VC*.CRT\vcruntime140.dll`
fn vcruntime_in(vs_root: &Path) -> Result<PathBuf> {
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
            let dll = c.join("vcruntime140.dll");
            if dll.is_file() {
                return Ok(dll);
            }
        }
    }
    bail!("vcruntime140.dll не найден в {}", msvc.display())
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

/// `ZAM_VCRUNTIME_DLL` или Visual Studio через vswhere.
pub fn find_vcruntime() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("ZAM_VCRUNTIME_DLL") {
        let p = PathBuf::from(p);
        if !p.is_file() {
            bail!(
                "ZAM_VCRUNTIME_DLL указывает на несуществующий файл: {}",
                p.display()
            );
        }
        return Ok(p);
    }
    let vs = vs_install_path().context("vcruntime140.dll не найден; задайте ZAM_VCRUNTIME_DLL")?;
    vcruntime_in(&vs)
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
        let p = msvc.join(v).join(r"bin\Hostx64\x64\dumpbin.exe");
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
mod tests {
    use super::*;

    fn work(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/xtask")
            .join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    /// Фиктивное дерево исходников: (deps, deploy, generated, dist).
    fn fixture(w: &Path, platform: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let win = is_windows(platform);
        let deps = w.join("deps");
        let (ort, pdf) = if win {
            ("onnxruntime.dll", "pdfium.dll")
        } else {
            ("libonnxruntime.so", "libpdfium.so")
        };
        touch(&deps.join("onnxruntime").join(ort));
        touch(&deps.join("pdfium").join(pdf));
        touch(&deps.join("qdrant").join(exe(platform, "qdrant")));
        let deploy = w.join("deploy");
        for f in [
            "qdrant.toml",
            "config.example.toml",
            "README-install.md",
            "systemd/zam-server.service",
            "licenses/A.txt",
            "licenses/B.txt",
        ] {
            touch(&deploy.join(f));
        }
        let gen_dir = w.join("gen");
        touch(&gen_dir.join("THIRD-PARTY-RUST.html"));
        touch(&gen_dir.join("THIRD-PARTY-WEB.json"));
        for f in ["zam-server.bin", "zam.bin", "LICENSE", "vcruntime140.dll"] {
            touch(&w.join(f));
        }
        (deps, deploy, gen_dir, w.join("dist"))
    }

    fn run_package(w: &Path, platform: &str, with_vc: bool) -> Result<(PathBuf, PathBuf)> {
        let (deps, deploy, gen_dir, dist) = fixture(w, platform);
        let vc = w.join("vcruntime140.dll");
        let server_bin = w.join("zam-server.bin");
        let cli_bin = w.join("zam.bin");
        let lic = w.join("LICENSE");
        package(&Inputs {
            platform,
            version: "1.2.3",
            server_bin: &server_bin,
            cli_bin: &cli_bin,
            deps_dir: &deps,
            deploy_dir: &deploy,
            generated_licenses_dir: &gen_dir,
            project_license: &lic,
            vcruntime: with_vc.then_some(vc.as_path()),
            qdrant_version: "1.19.2",
            out_dir: &dist,
        })
    }

    fn zip_names(p: &Path) -> Vec<String> {
        let mut z = zip::ZipArchive::new(File::open(p).unwrap()).unwrap();
        let mut v: Vec<String> = (0..z.len())
            .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        v.sort();
        v
    }

    fn tar_entries(p: &Path) -> Vec<(String, u32)> {
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

    #[test]
    fn package_layout_windows_and_linux() {
        let w = work("layout-win");
        let (server, cli) = run_package(&w, "windows-x64", true).unwrap();
        assert_eq!(
            server.file_name().unwrap(),
            "zam-server-1.2.3-windows-x64-cpu.zip"
        );
        let want = sorted(
            "zam-server-1.2.3-windows-x64-cpu",
            &[
                "zam-server.exe",
                "zam.exe",
                "onnxruntime.dll",
                "pdfium.dll",
                "qdrant/1.19.2/qdrant.exe",
                "qdrant/1.19.2/vcruntime140.dll",
                "deploy/qdrant.toml",
                "config.example.toml",
                "README-install.md",
                "LICENSES/A.txt",
                "LICENSES/B.txt",
                "LICENSES/THIRD-PARTY-RUST.html",
                "LICENSES/THIRD-PARTY-WEB.json",
                "LICENSES/ZeroAIMemory-LICENSE.txt",
            ],
        );
        assert_eq!(zip_names(&server), want);
        assert_eq!(
            zip_names(&cli),
            sorted("zam-1.2.3-windows-x64", &["LICENSE.txt", "zam.exe"])
        );

        let w = work("layout-linux");
        let (server, cli) = run_package(&w, "linux-x64", false).unwrap();
        assert_eq!(
            server.file_name().unwrap(),
            "zam-server-1.2.3-linux-x64-cpu.tar.gz"
        );
        let r = "zam-server-1.2.3-linux-x64-cpu";
        let mut want: Vec<(String, u32)> = [
            ("zam-server", 0o755),
            ("zam", 0o755),
            ("libonnxruntime.so", 0o644),
            ("libpdfium.so", 0o644),
            ("qdrant/1.19.2/qdrant", 0o755),
            ("deploy/qdrant.toml", 0o644),
            ("config.example.toml", 0o644),
            ("README-install.md", 0o644),
            ("systemd/zam-server.service", 0o644),
            ("LICENSES/A.txt", 0o644),
            ("LICENSES/B.txt", 0o644),
            ("LICENSES/THIRD-PARTY-RUST.html", 0o644),
            ("LICENSES/THIRD-PARTY-WEB.json", 0o644),
            ("LICENSES/ZeroAIMemory-LICENSE.txt", 0o644),
        ]
        .iter()
        .map(|(p, m)| (format!("{r}/{p}"), *m))
        .collect();
        want.sort();
        assert_eq!(tar_entries(&server), want);
        assert_eq!(
            tar_entries(&cli),
            vec![
                ("zam-1.2.3-linux-x64/LICENSE.txt".to_string(), 0o644),
                ("zam-1.2.3-linux-x64/zam".to_string(), 0o755)
            ]
        );
    }

    #[test]
    fn sha256sums_and_windows_requires_vcruntime() {
        let w = work("sums");
        let (a, b) = run_package(&w, "linux-x64", false).unwrap();
        // Архив другой платформы в том же dist в SHA256SUMS не попадает.
        let foreign = w.join("dist").join("zam-9.9.9-windows-x64.zip");
        fs::write(&foreign, b"x").unwrap();
        let sums = write_sha256sums(&w.join("dist"), &[a, b]).unwrap();
        let text = fs::read_to_string(sums).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for l in lines {
            let (h, n) = l.split_once("  ").unwrap();
            assert_eq!(h.len(), 64);
            assert!(n.starts_with("zam") && !n.contains("9.9.9"), "{n}");
        }
        let w = work("novc");
        assert!(run_package(&w, "windows-x64", false).is_err());
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
    fn vcruntime_picks_highest_version() {
        let w = work("vc");
        for v in ["14.9.1", "14.44.35208", "14.8.9"] {
            touch(&w.join(format!(
                "VC/Redist/MSVC/{v}/x64/Microsoft.VC143.CRT/vcruntime140.dll"
            )));
        }
        let p = vcruntime_in(&w).unwrap();
        assert!(p.to_string_lossy().contains("14.44.35208"), "{p:?}");
    }
}
