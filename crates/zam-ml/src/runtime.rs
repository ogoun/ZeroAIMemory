//! Загрузка ONNX Runtime (`load-dynamic`) и окружение ORT.
//!
//! Библиотека грузится только [`ort::init_from`] по явному пути (`ml.ort_dylib`); `ORT_DYLIB_PATH`
//! и системный поиск не используются — иначе первый же вызов ORT без библиотеки паникует.
//! И библиотека, и окружение ORT существуют в одном экземпляре на процесс (ограничение `ort`:
//! `G_ORT_LIB` и «only one environment is allowed per process»), поэтому окружение хранится здесь
//! и раздаётся сессиям клоном (`Environment` — это `Arc`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::environment::Environment;

use crate::MlError;

/// Загруженная библиотека и окружение.
struct Runtime {
    path: PathBuf,
    env: Environment,
}

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Абсолютный путь к библиотеке ORT: относительный отсчитывается от каталога исполняемого
/// файла (так же поступает `ort`, но там `current_exe().expect` — здесь ошибка вместо паники).
pub(crate) fn resolve_dylib(dylib: &Path) -> Result<PathBuf, MlError> {
    if dylib.is_absolute() {
        return Ok(dylib.to_path_buf());
    }
    let exe = std::env::current_exe()
        .map_err(|e| MlError::NoRuntime(format!("не определить каталог программы: {e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| MlError::NoRuntime("у исполняемого файла нет каталога".into()))?;
    Ok(dir.join(dylib))
}

/// Загружает ONNX Runtime из `dylib` и создаёт окружение. До загрузки версия проверяется
/// `ort::util::probe`: нужна 1.[`ort::MINOR_VERSION`] или новее. Повторный вызов с тем же путём — `Ok`;
/// с другим путём — `NoRuntime` (вторую библиотеку в процесс не загрузить, нужен перезапуск).
/// Относительный путь отсчитывается от каталога исполняемого файла.
pub fn init_runtime(dylib: &Path) -> Result<(), MlError> {
    environment(dylib).map(|_| ())
}

/// Окружение ORT для `dylib` (загружает библиотеку при первом вызове).
pub(crate) fn environment(dylib: &Path) -> Result<Environment, MlError> {
    let dylib = &resolve_dylib(dylib)?;
    let mut guard = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(rt) = guard.as_ref() {
        if same_file(&rt.path, dylib) {
            return Ok(rt.env.clone());
        }
        return Err(MlError::NoRuntime(format!(
            "ONNX Runtime уже загружен из {}, библиотека {} подключится только после перезапуска",
            rt.path.display(),
            dylib.display()
        )));
    }
    if !dylib.is_file() {
        return Err(MlError::NoRuntime(format!("нет файла {}", dylib.display())));
    }
    let minor = ort::util::probe(dylib).ok_or_else(|| {
        MlError::NoRuntime(format!(
            "{} не загружается или не сообщает версию 1.x",
            dylib.display()
        ))
    })?;
    if minor < ort::MINOR_VERSION {
        return Err(MlError::NoRuntime(format!(
            "{}: версия 1.{minor}, нужна 1.{} или новее",
            dylib.display(),
            ort::MINOR_VERSION
        )));
    }
    let builder = ort::init_from(dylib).map_err(|e| MlError::NoRuntime(e.to_string()))?;
    let env = builder
        .with_name("zam")
        .with_telemetry(false)
        .build()
        .map_err(|e| MlError::NoRuntime(format!("окружение ORT: {e}")))?;
    tracing::info!(
        path = %dylib.display(),
        version = format!("1.{minor}"),
        "ONNX Runtime загружен"
    );
    *guard = Some(Runtime {
        path: dylib.to_path_buf(),
        env: env.clone(),
    });
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_dylib_resolves_from_exe_dir() {
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let r = resolve_dylib(Path::new("onnxruntime.dll")).unwrap();
        assert!(r.is_absolute());
        assert_eq!(r, exe_dir.join("onnxruntime.dll"));
        let abs = exe_dir.join("x");
        assert_eq!(resolve_dylib(&abs).unwrap(), abs);
    }
}
