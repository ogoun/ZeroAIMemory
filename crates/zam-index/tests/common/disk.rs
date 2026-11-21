//! Место на диске и каталоги данных тестов zam-index (M6.6, задача 12).
//!
//! Наборы с настоящим Qdrant пишут сотни мегабайт на прогон (`supervisor.rs` — ≈ 2,4 ГБ): на
//! почти полном диске Qdrant не успевает подтвердить WAL или отказывает с ENOSPC, и тест падает
//! от среды, а не от ошибки. [`require_free_disk`] проверяет место до старта; [`TestDir`]
//! удаляет данные успешного теста и подметает каталоги завершившихся прогонов ([`sweep_once`];
//! так же — `TestQdrant` в своей базе `zam-index`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Свободного места перед тестом, пишущим в Qdrant, по умолчанию, МБ.
pub const MIN_FREE_MB: u64 = 4096;

/// Порог свободного места: `ZAM_TEST_MIN_FREE_MB` или [`MIN_FREE_MB`].
pub fn min_free_mb() -> u64 {
    threshold_mb(std::env::var("ZAM_TEST_MIN_FREE_MB").ok().as_deref())
}

/// Порог по значению переменной `ZAM_TEST_MIN_FREE_MB` (нет или не число — [`MIN_FREE_MB`]).
pub fn threshold_mb(var: Option<&str>) -> u64 {
    var.and_then(|v| v.trim().parse().ok())
        .unwrap_or(MIN_FREE_MB)
}

/// Переменная задана и не выключена (`""`, `0`, `false`).
fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false"))
}

/// Нехватка места — падение, а не пропуск: в CI (`CI`) и с `ZAM_REQUIRE_DISK=1`, как
/// `ZAM_REQUIRE_ML` у тестов ML.
pub fn strict() -> bool {
    flag("CI") || flag("ZAM_REQUIRE_DISK")
}

/// Свободное место тома `dir`, МБ (`None` — не удалось узнать).
pub fn free_mb(dir: &Path) -> Option<u64> {
    fs4::available_space(dir).ok().map(|b| b / (1024 * 1024))
}

/// Текст пропуска, если на томе `dir` свободно меньше `need_mb`; места хватает или его не
/// удалось узнать — `None` (тест идёт: проверка не должна сама ронять прогон).
pub fn disk_shortage(dir: &Path, need_mb: u64, test: &str) -> Option<String> {
    let free = free_mb(dir)?;
    (free < need_mb).then(|| {
        format!(
            "SKIP {test}: свободно {free} МБ < {need_mb} МБ на {} — освободите место \
             (cargo xtask clean-test-tmp)",
            dir.display()
        )
    })
}

/// Хватает ли места на томе `dir` (`need_mb`, обычно [`min_free_mb`]): нет — `SKIP …` в stderr
/// и `false` (тест возвращается), в строгом режиме ([`strict`]) — паника с тем же текстом.
pub fn require_free_disk(dir: &Path, need_mb: u64, test: &str) -> bool {
    match disk_shortage(dir, need_mb, test) {
        None => true,
        Some(msg) => {
            assert!(!strict(), "{msg}");
            eprintln!("{msg}");
            false
        }
    }
}

/// [`require_free_disk`] для каталога данных тестов (`CARGO_TARGET_TMPDIR`) с порогом
/// [`min_free_mb`].
pub fn enough_disk(test: &str) -> bool {
    require_free_disk(Path::new(env!("CARGO_TARGET_TMPDIR")), min_free_mb(), test)
}

/// Каталог данных теста `<CARGO_TARGET_TMPDIR>/<base>/<name>-<pid>`: создаётся пустым и
/// удаляется в `Drop`, если тест не паникует (после паники остаётся для разбора — как у
/// `TestQdrant`). Первое создание в процессе под базой `base` сначала удаляет каталоги
/// `<base>/*-<pid>` завершившихся процессов (упавшие прогоны), и пока оно идёт, остальные тесты
/// ждут: удаление гигабайтов на HDD не должно совпасть с замерами времени соседей.
pub struct TestDir {
    path: PathBuf,
}

/// Базы, уже подметённые этим процессом; блокировка — на всё время подметания.
static SWEPT: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Подметает `root` ([`sweep_dead`]) один раз за процесс; пока подметание идёт, остальные
/// вызовы ждут.
pub fn sweep_once(root: &Path, pid_of: fn(&str) -> Option<u32>) {
    let mut swept = SWEPT.lock().unwrap_or_else(|p| p.into_inner());
    if swept
        .get_or_insert_with(HashSet::new)
        .insert(root.to_path_buf())
    {
        sweep_dead(root, pid_of);
    }
}

impl TestDir {
    pub fn new(base: &str, name: &str) -> TestDir {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(base);
        sweep_once(&root, pid_suffix);
        let path = root.join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("каталог теста");
        TestDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// `pid` из имени каталога `<имя>-<pid>` ([`TestDir`]).
pub fn pid_suffix(name: &str) -> Option<u32> {
    name.rsplit_once('-').and_then(|(_, p)| p.parse().ok())
}

/// `pid` из имени каталога `<имя>-<pid>-<номер>` (`TestQdrant`).
pub fn pid_before_seq(name: &str) -> Option<u32> {
    let (head, seq) = name.rsplit_once('-')?;
    seq.parse::<u32>().ok()?;
    pid_suffix(head)
}

/// Удаляет каталоги `<root>/*` процессов, которых уже нет (не текущего; `pid` из имени —
/// `pid_of`): данные прогонов, упавших или прерванных до `Drop`. Каталог живого процесса
/// (параллельный прогон другого бинарника) не трогается; не удалось удалить — остаётся до
/// следующего раза.
pub fn sweep_dead(root: &Path, pid_of: fn(&str) -> Option<u32>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let me = std::process::id();
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(pid_of) else {
            continue;
        };
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && pid != me && !zam_index::supervisor::pid_alive(pid) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
