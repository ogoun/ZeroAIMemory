//! `cargo xtask clean-test-tmp [--older-than 6h] [--dry-run]` — уборка данных тестов в `target/`
//! (M6.6, задача 12).
//!
//! Каталоги данных тестов: `target/tmp` (`CARGO_TARGET_TMPDIR` сборки в `target/` и жёсткие
//! пути модульных тестов), `target/<сборка>/tmp` (сборка с `CARGO_TARGET_DIR=target/<сборка>`:
//! в `target/<сборка>/` есть метка cargo `CACHEDIR.TAG`) и `target/test-tmp`. Удаляются только
//! записи этих каталогов (их прямые потомки), в дереве которых ничего не менялось с отсечки:
//! возраст записи — самое новое время изменения или создания среди всех её файлов и каталогов
//! (создание ловит файлы, скопированные с прежним временем изменения), поэтому данные
//! запущенного теста не трогаются. Ссылки и junction не обходятся и удаляются как ссылки.
//! `target/tags`, `target/models`, `target/deps`, `target/deps-cache`, `target/dist`,
//! `target/release` (и прочие [`PROTECTED`]), сами каталоги данных, файлы `target/` и всё вне
//! `target/` — никогда.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

/// Подкаталоги `target/`, чей `tmp` не трогается, даже если в них есть метка сборки: бинарники
/// тегов, модели, зависимости, выпуски, замеры.
const PROTECTED: [&str; 12] = [
    "tags",
    "models",
    "deps",
    "deps-cache",
    "dist",
    "release",
    "debug",
    "eval",
    "bench",
    "bench-edge",
    "licenses",
    "generated",
];

/// Аргументы `cargo xtask clean-test-tmp`.
#[derive(clap::Args)]
pub struct CleanTestTmpArgs {
    /// Удалять записи, в которых ничего не менялось дольше: `<число><s|m|h|d>`, не меньше 1m
    #[arg(long, default_value = "6h", value_parser = parse_age)]
    older_than: Duration,
    /// Только показать, что было бы удалено
    #[arg(long)]
    dry_run: bool,
    /// Каталог вместо `target/` репозитория (только внутри него: проверка на копии)
    #[arg(long, value_name = "DIR")]
    target_dir: Option<PathBuf>,
}

/// Наименьший порог возраста: запись, менявшаяся меньше минуты назад, — почти наверняка данные
/// запущенного теста (на Linux удаление открытых файлов не отказывает, и тест их потерял бы).
pub const MIN_AGE: Duration = Duration::from_secs(60);

/// Порог возраста `<число><s|m|h|d>` (`90m`, `6h`, `2d`), не меньше [`MIN_AGE`].
pub fn parse_age(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let err = || format!("порог «{s}»: ожидается <целое число><s|m|h|d>, например 6h или 90m");
    let unit = s.chars().last().ok_or_else(err)?;
    let secs = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86400,
        _ => return Err(err()),
    };
    let num = &s[..s.len() - 1];
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    let n: u64 = num.parse().map_err(|_| err())?;
    let age = n
        .checked_mul(secs)
        .map(Duration::from_secs)
        .ok_or_else(err)?;
    if age < MIN_AGE {
        return Err(format!(
            "порог «{s}» меньше 1m: так можно удалить данные запущенного теста"
        ));
    }
    Ok(age)
}

/// Итог уборки.
#[derive(Debug, Default)]
pub struct Report {
    /// Удалённые (или при `dry_run` — подлежащие удалению) записи.
    pub removed: Vec<PathBuf>,
    /// Байт в них.
    pub bytes: u64,
    /// Записи, которые не удалось удалить (заняты запущенным процессом), с причиной.
    pub failed: Vec<(PathBuf, String)>,
    /// Просмотренные каталоги данных тестов.
    pub roots: Vec<PathBuf>,
}

/// Каталог (не ссылка и не junction).
fn real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
}

/// Каталоги данных тестов в `target` (см. модуль), только настоящие каталоги внутри `target`.
fn test_tmp_dirs(target: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !real_dir(target) {
        return Ok(out);
    }
    let canon = fs::canonicalize(target).with_context(|| target.display().to_string())?;
    let mut push = |d: PathBuf| {
        // каталог данных — настоящий каталог, и путь к нему не уводит из `target`
        if real_dir(&d) && fs::canonicalize(&d).is_ok_and(|c| c.starts_with(&canon)) {
            out.push(d);
        }
    };
    push(target.join("tmp"));
    push(target.join("test-tmp"));
    let mut builds: Vec<PathBuf> = fs::read_dir(target)
        .with_context(|| target.display().to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !PROTECTED.contains(&name)
                && !matches!(name, "tmp" | "test-tmp")
                && real_dir(p)
                && p.join("CACHEDIR.TAG").is_file()
        })
        .collect();
    builds.sort();
    for b in builds {
        push(b.join("tmp"));
    }
    Ok(out)
}

/// В дереве `p` что-то изменено или создано не раньше `cutoff` (ссылки — своё время, без
/// обхода). Не читается (нет прав, удалено во время обхода) — тоже «свежее»: такая запись не
/// удаляется. Обход останавливается на первом свежем узле.
fn fresh(p: &Path, cutoff: SystemTime) -> bool {
    let Ok(m) = fs::symlink_metadata(p) else {
        return true;
    };
    let Some(own) = [m.modified().ok(), m.created().ok()]
        .into_iter()
        .flatten()
        .max()
    else {
        return true;
    };
    if own >= cutoff {
        return true;
    }
    if !m.file_type().is_dir() {
        return false;
    }
    let Ok(rd) = fs::read_dir(p) else {
        return true;
    };
    for e in rd {
        match e {
            Ok(e) if !fresh(&e.path(), cutoff) => {}
            _ => return true,
        }
    }
    false
}

/// Байт в дереве `p` (ссылки — без обхода).
fn size(p: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else {
        return 0;
    };
    if !m.file_type().is_dir() {
        return m.len();
    }
    fs::read_dir(p).map_or(0, |rd| rd.flatten().map(|e| size(&e.path())).sum())
}

/// Удаляет запись: каталог — `remove_dir_all` (ссылки и junction внутри удаляет как ссылки),
/// ссылку на каталог (Windows) — `remove_dir`, остальное — `remove_file`.
fn remove(p: &Path) -> std::io::Result<()> {
    let ft = fs::symlink_metadata(p)?.file_type();
    if ft.is_dir() {
        fs::remove_dir_all(p)
    } else if ft.is_symlink() {
        fs::remove_file(p).or_else(|_| fs::remove_dir(p))
    } else {
        fs::remove_file(p)
    }
}

/// Убирает в каталогах данных тестов `target` записи, не менявшиеся с `cutoff`.
pub fn clean(target: &Path, cutoff: SystemTime, dry_run: bool) -> Result<Report> {
    let mut r = Report {
        roots: test_tmp_dirs(target)?,
        ..Report::default()
    };
    for root in r.roots.clone() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&root)
            .with_context(|| root.display().to_string())?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for e in entries {
            if fresh(&e, cutoff) {
                continue;
            }
            let bytes = size(&e);
            if dry_run {
                r.bytes += bytes;
                r.removed.push(e);
                continue;
            }
            match remove(&e) {
                Ok(()) => {
                    r.bytes += bytes;
                    r.removed.push(e);
                }
                Err(err) => r.failed.push((e, err.to_string())),
            }
        }
    }
    Ok(r)
}

/// Каталог уборки: `target/` репозитория или `dir` внутри него (иначе — ошибка: вне `target/`
/// команда ничего не трогает).
fn resolve_target(root: &Path, dir: Option<&Path>) -> Result<PathBuf> {
    let target = root.join("target");
    let Some(dir) = dir else {
        return Ok(target);
    };
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    let (canon, base) = (
        fs::canonicalize(&dir).with_context(|| dir.display().to_string())?,
        fs::canonicalize(&target).with_context(|| target.display().to_string())?,
    );
    if !canon.starts_with(&base) {
        anyhow::bail!(
            "--target-dir {}: только внутри {}",
            dir.display(),
            target.display()
        );
    }
    Ok(dir)
}

pub fn main(a: CleanTestTmpArgs, root: &Path) -> Result<()> {
    let target = resolve_target(root, a.target_dir.as_deref())?;
    let cutoff = SystemTime::now()
        .checked_sub(a.older_than)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let r = clean(&target, cutoff, a.dry_run)?;
    if r.roots.is_empty() {
        println!("каталогов данных тестов в {} нет", target.display());
        return Ok(());
    }
    for d in &r.roots {
        println!("каталог данных тестов: {}", d.display());
    }
    let verb = if a.dry_run {
        "будет удалено"
    } else {
        "удалено"
    };
    for p in &r.removed {
        println!("  {verb}: {}", p.display());
    }
    for (p, e) in &r.failed {
        eprintln!("  не удалено (занято?): {}: {e}", p.display());
    }
    println!(
        "{verb} {} записей старше {:?}, {:.1} МБ; не удалось — {}",
        r.removed.len(),
        a.older_than,
        r.bytes as f64 / 1e6,
        r.failed.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::tests::work;
    use std::fs;

    #[test]
    fn age_parses_units() {
        assert_eq!(parse_age("6h"), Ok(Duration::from_secs(6 * 3600)));
        assert_eq!(parse_age("90m"), Ok(Duration::from_secs(90 * 60)));
        assert_eq!(parse_age("60s"), Ok(MIN_AGE));
        assert_eq!(parse_age("1m"), Ok(MIN_AGE));
        assert_eq!(parse_age("2d"), Ok(Duration::from_secs(2 * 86400)));
        assert_eq!(parse_age(" 3h "), Ok(Duration::from_secs(3 * 3600)));
        for bad in ["", "6", "h", "6x", "-1h", "1.5h", "6 h"] {
            assert!(parse_age(bad).is_err(), "{bad:?} принят");
        }
        // меньше минуты — данные запущенного теста: отказ с причиной
        for small in ["0s", "45s", "59s", "0m"] {
            let e = parse_age(small).unwrap_err();
            assert!(e.contains("меньше 1m"), "{small}: {e}");
        }
    }

    /// Запись с файлом `f` (и вложенным `deep/f`).
    fn entry(dir: &Path) {
        fs::create_dir_all(dir.join("deep")).unwrap();
        fs::write(dir.join("f"), b"12345").unwrap();
        fs::write(dir.join("deep").join("f"), b"67890").unwrap();
    }

    /// Уборка трогает только записи каталогов данных тестов (`target/tmp`, `target/<сборка>/tmp`,
    /// `target/test-tmp`) и только не менявшиеся с отсечки — по самому новому времени во всём
    /// дереве записи; каталоги `tags`, `models`, `deps-cache`, `release`, `dist` и каталоги без
    /// метки сборки cargo, файлы вне каталогов данных — нет.
    #[test]
    fn removes_only_stale_entries_of_test_tmp_dirs() {
        let t = work("clean-test-tmp").join("target");
        fs::create_dir_all(&t).unwrap();
        fs::write(t.join("CACHEDIR.TAG"), b"tag").unwrap();
        fs::create_dir_all(t.join("m65")).unwrap();
        fs::write(t.join("m65").join("CACHEDIR.TAG"), b"tag").unwrap();
        // защищённые и чужие каталоги — даже с меткой сборки и вложенным tmp
        let protected = [
            "tags",
            "models",
            "deps-cache",
            "deps",
            "dist",
            "release",
            "eval",
        ];
        for p in protected {
            fs::create_dir_all(t.join(p)).unwrap();
            fs::write(t.join(p).join("CACHEDIR.TAG"), b"tag").unwrap();
            entry(&t.join(p).join("tmp").join("old"));
        }
        // без метки сборки: `target/<x>/tmp` не каталог данных тестов
        entry(&t.join("bench").join("tmp").join("old"));
        let old = [
            t.join("tmp").join("old-a"),
            t.join("m65").join("tmp").join("old-b"),
            t.join("test-tmp").join("old-c"),
        ];
        for o in &old {
            entry(o);
        }
        fs::write(t.join("m65").join("tmp").join("old-file.log"), b"log").unwrap();
        // старое дерево, в котором позже изменился вложенный файл, — свежее
        entry(&t.join("m65").join("tmp").join("touched"));
        std::thread::sleep(Duration::from_millis(300));
        let cutoff = SystemTime::now();
        std::thread::sleep(Duration::from_millis(300));
        fs::write(
            t.join("m65")
                .join("tmp")
                .join("touched")
                .join("deep")
                .join("f"),
            b"new",
        )
        .unwrap();
        let fresh = [
            t.join("tmp").join("fresh-a"),
            t.join("m65").join("tmp").join("fresh-b"),
            t.join("test-tmp").join("fresh-c"),
        ];
        for f in &fresh {
            entry(f);
        }
        fs::create_dir_all(t.join("test-tmp").join("fresh-empty")).unwrap();
        fs::write(t.join("keep.txt"), b"keep").unwrap();

        // пробный прогон ничего не удаляет
        let dry = clean(&t, cutoff, true).unwrap();
        assert_eq!(dry.removed.len(), 4, "{:?}", dry.removed);
        assert!(old.iter().all(|o| o.exists()));

        let r = clean(&t, cutoff, false).unwrap();
        let mut removed = r.removed.clone();
        removed.sort();
        let mut want = old.to_vec();
        want.push(t.join("m65").join("tmp").join("old-file.log"));
        want.sort();
        assert_eq!(removed, want);
        assert_eq!(r.bytes, 3 * 10 + 3, "{r:?}");
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        for o in &want {
            assert!(!o.exists(), "{} не удалён", o.display());
        }
        for f in &fresh {
            assert!(f.join("deep").join("f").exists(), "{} удалён", f.display());
        }
        assert!(t.join("m65").join("tmp").join("touched").join("f").exists());
        assert!(t.join("test-tmp").join("fresh-empty").is_dir());
        for p in protected {
            assert!(t.join(p).join("tmp").join("old").join("f").exists(), "{p}");
        }
        assert!(t.join("bench").join("tmp").join("old").join("f").exists());
        assert!(t.join("keep.txt").exists());
        // сами каталоги данных остаются
        assert!(t.join("tmp").is_dir() && t.join("m65").join("tmp").is_dir());
        assert!(t.join("test-tmp").is_dir());
    }

    /// `--target-dir` — только внутри `target/` репозитория.
    #[test]
    fn target_dir_only_inside_repo_target() {
        let root = crate::repo_root();
        assert_eq!(resolve_target(&root, None).unwrap(), root.join("target"));
        let inside = work("clean-test-tmp-inside");
        assert_eq!(resolve_target(&root, Some(&inside)).unwrap(), inside);
        let err = resolve_target(&root, Some(&root.join("xtask"))).unwrap_err();
        assert!(err.to_string().contains("только внутри"), "{err}");
        assert!(
            resolve_target(&root, Some(&root.join("target").join("..").join("crates"))).is_err()
        );
    }

    /// Каталога `target` нет — нечего убирать, не ошибка.
    #[test]
    fn missing_target_is_empty_report() {
        let t = work("clean-test-tmp-none").join("target");
        let r = clean(&t, SystemTime::now(), false).unwrap();
        assert!(r.removed.is_empty() && r.roots.is_empty());
    }

    /// Ссылка внутри старой записи на каталог вне `target` удаляется как ссылка: цель цела.
    #[cfg(unix)]
    #[test]
    fn symlink_target_outside_is_not_followed() {
        let w = work("clean-test-tmp-link");
        let outside = w.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious"), b"x").unwrap();
        let t = w.join("target");
        fs::create_dir_all(t.join("test-tmp").join("old")).unwrap();
        std::os::unix::fs::symlink(&outside, t.join("test-tmp").join("old").join("link")).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let cutoff = SystemTime::now();
        clean(&t, cutoff, false).unwrap();
        assert!(!t.join("test-tmp").join("old").exists());
        assert!(outside.join("precious").exists(), "цель ссылки удалена");
    }

    /// Windows: junction (`mklink /J`, без прав администратора) удаляется как ссылка — и когда
    /// он сам старая запись, и когда лежит внутри старой записи; цели (вне подопытного
    /// `target`, внутри настоящего) целы.
    #[cfg(windows)]
    #[test]
    fn junction_target_is_not_followed() {
        // `mklink` не принимает `..` и `/`: пути — абсолютные и нормализованные
        let w = std::path::absolute(work("clean-test-tmp-junction")).unwrap();
        let junction = |link: &Path, to: &Path| {
            let st = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(to)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(
                st.success(),
                "mklink /J {} {}",
                link.display(),
                to.display()
            );
            assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        };
        let (outside_a, outside_b) = (w.join("outside-a"), w.join("outside-b"));
        for o in [&outside_a, &outside_b] {
            fs::create_dir_all(o.join("deep")).unwrap();
            fs::write(o.join("deep").join("precious"), b"x").unwrap();
        }
        let tt = w.join("target").join("test-tmp");
        fs::create_dir_all(tt.join("old-dir")).unwrap();
        fs::write(tt.join("old-dir").join("f"), b"old").unwrap();
        junction(&tt.join("old-junction"), &outside_a);
        junction(&tt.join("old-dir").join("link"), &outside_b);
        std::thread::sleep(Duration::from_millis(300));
        let cutoff = SystemTime::now();
        // свежая запись в цели после отсечки: ссылку она «свежей» не делает — обхода нет
        fs::write(outside_a.join("deep").join("new"), b"new").unwrap();
        let r = clean(&w.join("target"), cutoff, false).unwrap();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        let mut removed = r.removed.clone();
        removed.sort();
        assert_eq!(removed, vec![tt.join("old-dir"), tt.join("old-junction")]);
        assert!(fs::symlink_metadata(tt.join("old-junction")).is_err());
        assert!(!tt.join("old-dir").exists());
        for o in [&outside_a, &outside_b] {
            assert!(
                o.join("deep").join("precious").exists(),
                "цель junction удалена"
            );
        }
        assert!(outside_a.join("deep").join("new").exists());
    }
}
