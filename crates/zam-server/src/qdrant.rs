//! Команды `zam-server qdrant fetch|status|reset` (§4.3).
//!
//! Супервизор работает в составе сервера (`index_state::IndexRuntime`); здесь — только
//! обслуживание установки: загрузка бинарника по манифесту, диагностика и стирание хранилища.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use zam_fetch::{DepManifest, host_platform, install_archive};
use zam_index::supervisor::{QDRANT_VERSION, SupervisorConfig, inspect, qdrant_exe_name};
use zam_index::{QdrantIndex, VectorIndex};

use crate::config::ServerConfig;

/// Манифест, с которым собран сервер (`deploy/qdrant.toml`): его версию ждёт супервизор.
const BUILT_IN_MANIFEST: &str = include_str!("../../../deploy/qdrant.toml");

/// Ключ штампа архива Qdrant в каталоге версии (`.qdrant.sha256`).
const STAMP_KEY: &str = "qdrant";

/// Каталог исполняемого файла сервера (корень комплекта).
pub fn exe_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("путь к исполняемому файлу")?;
    Ok(exe
        .parent()
        .context("у исполняемого файла нет каталога")?
        .to_path_buf())
}

/// Манифест: явный путь, иначе встроенный при сборке. Версия обязана совпасть с
/// закреплённой — бинарник другой версии супервизор не примет (`Broken`).
pub fn load_manifest(path: Option<&Path>) -> anyhow::Result<DepManifest> {
    let m = match path {
        Some(p) => DepManifest::load(p)?,
        None => DepManifest::parse(BUILT_IN_MANIFEST)?,
    };
    if m.version != QDRANT_VERSION {
        bail!(
            "манифест описывает Qdrant {}, а сервер собран под {QDRANT_VERSION}",
            m.version
        );
    }
    Ok(m)
}

/// Загрузка по сети допускается только по https (локальная петля — для тестов).
fn check_url(url: &str) -> anyhow::Result<()> {
    if url.starts_with("https://") || url.starts_with("http://127.0.0.1:") {
        Ok(())
    } else {
        bail!("{url}: загрузка допускается только по https://")
    }
}

/// `qdrant fetch`: ставит бинарник в каталог `binary`, если его там нет.
/// Возвращает строку отчёта.
///
/// В отличие от `cuda fetch`, бинарник без штампа не перекачивается: каталог версии
/// (`qdrant/<версия>/`) уже фиксирует выпуск, а комплект кладёт бинарник без штампа; версию
/// запущенного процесса сверяет супервизор (чужая — `Broken`). Установка этой командой
/// пишет штамп `.qdrant.sha256` (как у `install_archive` везде).
pub fn fetch(manifest: &DepManifest, binary: &Path) -> anyhow::Result<String> {
    let entry = manifest.entry(host_platform())?;
    let dir = binary
        .parent()
        .with_context(|| format!("у пути {} нет каталога", binary.display()))?;
    if binary.is_file() {
        let how = if zam_fetch::is_installed(entry, dir, STAMP_KEY)? {
            "штамп совпал"
        } else {
            "из комплекта; версию проверяет супервизор при старте"
        };
        return Ok(format!(
            "Qdrant уже установлен ({how}): {}",
            binary.display()
        ));
    }
    check_url(&entry.url)?;
    let wanted = binary.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !zam_fetch::dest_names(entry)?.iter().any(|n| n == wanted) {
        bail!(
            "архив манифеста не содержит {wanted}: задайте qdrant.binary с именем {}",
            qdrant_exe_name()
        );
    }
    install_archive(entry, dir, STAMP_KEY)
        .with_context(|| format!("qdrant {}", manifest.version))?;
    if !binary.is_file() {
        bail!("после установки нет {}", binary.display());
    }
    let mut out = format!(
        "Qdrant {} установлен: {} (SHA-256 архива сверен)",
        manifest.version,
        binary.display()
    );
    if cfg!(windows) && !dir.join("vcruntime140.dll").is_file() {
        out.push_str(
            "\nВнимание: рядом нет vcruntime140.dll. Если не установлен Microsoft Visual C++ \
             Redistributable, скопируйте vcruntime140.dll в этот каталог.",
        );
    }
    Ok(out)
}

/// `qdrant status`: режим, бинарник, порты, pid, лок, метка, ответ процесса.
/// Ничего не запускает.
pub async fn status(cfg: &ServerConfig, exe_dir: &Path) -> anyhow::Result<String> {
    let q = &cfg.qdrant;
    let mut out = Vec::new();
    match q.supervisor_config(exe_dir)? {
        None => {
            out.push("режим: edge (экспериментальный, процесс не запускается)".to_string());
        }
        Some(SupervisorConfig::External(x)) => {
            out.push(format!("режим: external, gRPC {}", x.grpc_url));
            let health = match QdrantIndex::connect(&x.grpc_url, x.api_key.as_deref()) {
                Ok(c) => c.health().await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            out.push(match health {
                Ok(()) => "доступность: ok".into(),
                Err(e) => format!("доступность: нет ({e})"),
            });
        }
        Some(SupervisorConfig::Managed(m)) => {
            out.push("режим: managed".into());
            out.push(format!(
                "бинарник: {} ({})",
                m.binary.display(),
                if m.binary.is_file() {
                    "есть"
                } else {
                    "нет — zam-server qdrant fetch"
                }
            ));
            out.push(format!("закреплённая версия: {QDRANT_VERSION}"));
            out.push(format!(
                "порты: http {}, grpc {} (127.0.0.1)",
                m.http_port, m.grpc_port
            ));
            let st = inspect(&cfg.data_dir, m.http_port).await;
            out.push(format!("каталог: {}", st.qdir.display()));
            out.push(format!(
                "лок: {}",
                if st.locked {
                    "занят (супервизор работает)"
                } else {
                    "свободен"
                }
            ));
            out.push(match st.pid {
                Some(p) if st.pid_alive => format!("pid: {p} (жив)"),
                Some(p) => format!("pid: {p} (процесса нет — устаревший pid-файл)"),
                None => "pid: нет".into(),
            });
            out.push(format!(
                "хранилище: {}, метка: {}",
                if st.storage_present {
                    "есть"
                } else {
                    "нет"
                },
                st.stamp_version.as_deref().unwrap_or("нет")
            ));
            if st.reindex_pending {
                out.push("пересборка индекса: требуется (при следующем старте)".into());
            }
            out.push(match (&st.running_version, st.ready) {
                (Some(v), true) => format!("процесс: отвечает, версия {v}, готов"),
                (Some(v), false) => format!("процесс: отвечает, версия {v}, не готов"),
                (None, _) => format!("процесс: нет ответа на 127.0.0.1:{}", m.http_port),
            });
        }
    }
    Ok(out.join("\n"))
}

/// `qdrant status --json`: то же машиночитаемо (для скриптов и smoke). Поля managed:
/// `mode`, `binary`, `binary_present`, `pinned_version`, `http_port`, `grpc_port`, `dir`,
/// `locked`, `pid`, `pid_alive`, `storage_present`, `stamp_version`, `reindex_pending`,
/// `running_version`, `ready`; external — `mode`, `grpc_url`, `healthy`, `error`; edge — `mode`.
pub async fn status_json(cfg: &ServerConfig, exe_dir: &Path) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    Ok(match cfg.qdrant.supervisor_config(exe_dir)? {
        None => json!({ "mode": "edge" }),
        Some(SupervisorConfig::External(x)) => {
            let health = match QdrantIndex::connect(&x.grpc_url, x.api_key.as_deref()) {
                Ok(c) => c.health().await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            json!({
                "mode": "external",
                "grpc_url": x.grpc_url,
                "healthy": health.is_ok(),
                "error": health.err(),
            })
        }
        Some(SupervisorConfig::Managed(m)) => {
            let st = inspect(&cfg.data_dir, m.http_port).await;
            json!({
                "mode": "managed",
                "binary": m.binary,
                "binary_present": m.binary.is_file(),
                "pinned_version": QDRANT_VERSION,
                "http_port": m.http_port,
                "grpc_port": m.grpc_port,
                "dir": st.qdir,
                "locked": st.locked,
                "pid": st.pid,
                "pid_alive": st.pid_alive,
                "storage_present": st.storage_present,
                "stamp_version": st.stamp_version,
                "reindex_pending": st.reindex_pending,
                "running_version": st.running_version,
                "ready": st.ready,
            })
        }
    })
}

/// `qdrant reset --yes`: стирает хранилище управляемого Qdrant (сервер должен быть
/// остановлен); при следующем старте индекс пересобирается из `embeddings`.
/// Осиротевший процесс (сервер убит) завершается перед стиранием.
pub fn reset(cfg: &ServerConfig, exe_dir: &Path, yes: bool) -> anyhow::Result<String> {
    let Some(SupervisorConfig::Managed(m)) = cfg.qdrant.supervisor_config(exe_dir)? else {
        bail!("reset применим только к mode = \"managed\"");
    };
    if !yes {
        bail!(
            "команда стирает {}; подтвердите флагом --yes",
            cfg.data_dir.join("qdrant").join("storage").display()
        );
    }
    zam_index::supervisor::reset_storage(&cfg.data_dir, &m)?;
    Ok(
        "хранилище Qdrant стёрто; индекс будет пересобран из embeddings при следующем старте"
            .into(),
    )
}
