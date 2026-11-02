//! Превью вложений (M5, задача 8): свой пул рабочих процессов и кэш в `previews/`.
//!
//! **Где разбирается файл.** Как и индексация (задача 5), превью разбирает недоверенный файл
//! только в рабочем процессе `doc-worker`: страница листа (`sheet_page_no`), миниатюра
//! изображения (`image` + `fast_image_resize`), миниатюра PDF (pdfium), подсветка syntect
//! (`fancy-regex` с возвратами). Лимит памяти процесса (`docs.worker_memory_mb`), срок
//! (`docs.timeout_ms`, внутри процесса — 80 %), паника — выход процесса; сервер держит только
//! файл (≤ `docs.max_bytes`) и кадр запроса. Пул **свой** (`docs.preview_workers`): разбор PDF
//! индексации на десятки секунд не задерживает первый экран листа (ТЗ §17). Одновременно
//! строится не больше `2 × preview_workers` превью ([`Previews::permit`]): ожидающий запрос файл
//! в память не читает.
//!
//! **Кэш** — `previews/<владелец>/<sha256>/<ключ>.<json|png>`: путь собирается только из id
//! владельца, hex sha256 (проверен) и чисел параметров ([`PreviewKey::file_name`]) — строки
//! запроса (имя листа, имя файла) в путь не попадают. Ключ содержит параметры,
//! `EXTRACTOR_VERSION`, [`PREVIEW_VERSION`] и отпечаток лимитов разбора: смена любого —
//! новый файл. Запись атомарна (временный файл в том же каталоге и `rename`); размер кэша
//! ограничен `docs.preview_cache_mb`: сверх него фоновая чистка удаляет самые давние по времени
//! изменения файлы до 3/4 предела (чтение из кэша обновляет время раз в час — LRU с точностью
//! до часа). Кэш — производные данные, его можно удалить целиком.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{Semaphore, SemaphorePermit};
use zam_core::Id;
use zam_docs::Limits;
use zam_store::blobs::is_sha256;

use super::pool::DocPool;
use crate::config::DocsSection;

/// Версия построения превью: смена правил (масштаб, формат ответа) — новые файлы кэша.
pub const PREVIEW_VERSION: u32 = 1;
/// Файл кэша больше этого не пишется (кэш не раздувается редкими огромными страницами).
const ENTRY_MAX: usize = 16 * 1024 * 1024;
/// Чтение из кэша обновляет время файла не чаще раза в столько.
const TOUCH_AFTER: Duration = Duration::from_secs(3600);
/// Незаконченный временный файл старше этого удаляется чисткой.
const TMP_STALE: Duration = Duration::from_secs(600);

/// Что строится: параметры превью (только числа).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKey {
    /// Лист `no` (с 1), окно `rows` строк со строки `from`.
    Sheet { no: u32, from: u32, rows: u32 },
    /// Миниатюра страницы `page` (PDF; у изображения — 1), большая сторона `px`.
    Thumb { page: u32, px: u32 },
    /// Текст: слайды PPTX или HTML syntect.
    Text,
}

impl PreviewKey {
    /// Имя файла кэша: вид, числа параметров, версии, отпечаток лимитов `tag` (8 hex) и — у листа
    /// и текста — 8 hex blake3 расширения имени файла `filename` в нижнем регистре: вид CSV/текст и
    /// синтаксис подсветки выбираются по нему (один файл под `.csv` и `.txt`, `.rs` и `.py` — разные
    /// превью; ревью, M-2). Миниатюра от имени не зависит (вид — по сигнатуре).
    pub fn file_name(&self, tag: &str, filename: &str) -> String {
        let v = format!("v{}.{PREVIEW_VERSION}-{tag}", zam_docs::EXTRACTOR_VERSION);
        let ext = filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        let e = &blake3::hash(ext.as_bytes()).to_hex()[..8];
        match self {
            PreviewKey::Sheet { no, from, rows } => {
                format!("sheet-{no}-{from}-{rows}-e{e}-{v}.json")
            }
            PreviewKey::Thumb { page, px } => format!("thumb-{page}-{px}-{v}.png"),
            PreviewKey::Text => format!("text-e{e}-{v}.json"),
        }
    }
}

/// Сколько помнится неудача построения превью, свойство файла (`parse_error`, `too_large`,
/// `unsupported`, не тот вид): повтор не поднимает процесс заново.
pub const FAILURE_TTL: Duration = Duration::from_secs(300);
/// Сколько помнится сбой процесса (`parse_timeout`, `parse_crash`, `parse_oom`): он может быть
/// следствием нагрузки или остановки сервера, а не файла (ревью задачи 8 M5, раунд 2).
pub const TRANSIENT_FAILURE_TTL: Duration = Duration::from_secs(45);

/// Срок памяти неудачи по пояснению ответа 422.
pub fn failure_ttl(detail: &str) -> Duration {
    if ["parse_timeout", "parse_crash", "parse_oom"]
        .iter()
        .any(|c| detail.contains(c))
    {
        TRANSIENT_FAILURE_TTL
    } else {
        FAILURE_TTL
    }
}
/// Записей в памяти неудач — не больше.
const FAILURES_MAX: usize = 4096;

/// Ключ неудачи: владелец, sha и имя файла кэша.
type FailureKey = (Id, String, String);

/// Пул превью, кэш и ограничение одновременных построений.
pub struct Previews {
    pool: DocPool,
    cache: PreviewCache,
    builds: Semaphore,
    limits: Limits,
    /// Отпечаток лимитов разбора в ключе кэша.
    tag: String,
    /// Недавние неудачи построения: срок и пояснение ответа 422 (ревью, M-1).
    failures: Mutex<std::collections::HashMap<FailureKey, (Instant, String)>>,
}

impl std::fmt::Debug for Previews {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Previews")
            .field("tag", &self.tag)
            .finish_non_exhaustive()
    }
}

impl Previews {
    /// Пул из `docs.preview_workers` процессов (по требованию) и кэш в `dir`.
    pub fn new(cfg: &DocsSection, dir: PathBuf) -> Self {
        let pc = DocsSection {
            workers: cfg.preview_workers.max(1),
            ..cfg.clone()
        };
        let pool = match &cfg.worker_program {
            Some(p) => DocPool::with_program(&pc, p.clone()),
            None => DocPool::new(&pc),
        };
        let limits = cfg.limits();
        let tag = super::limits_fingerprint(&limits);
        Previews {
            pool,
            cache: PreviewCache::new(dir, cfg.preview_cache_mb * 1024 * 1024),
            builds: Semaphore::new(2 * pc.workers),
            limits,
            tag,
            failures: Mutex::default(),
        }
    }

    /// Пояснение недавней (моложе [`FAILURE_TTL`]) неудачи того же построения.
    pub fn failed_recently(&self, owner: Id, sha: &str, name: &str) -> Option<String> {
        let mut f = lock(&self.failures);
        let k = (owner, sha.to_string(), name.to_string());
        match f.get(&k) {
            Some((until, d)) if *until > Instant::now() => Some(d.clone()),
            Some(_) => {
                f.remove(&k);
                None
            }
            None => None,
        }
    }

    /// Запоминает неудачу построения на [`failure_ttl`] (свойство файла — 5 мин, сбой процесса —
    /// 45 с): повтор того же запроса отвечает сразу, без нового процесса.
    pub fn remember_failure(&self, owner: Id, sha: &str, name: &str, detail: String) {
        let mut f = lock(&self.failures);
        let now = Instant::now();
        if f.len() >= FAILURES_MAX {
            f.retain(|_, (until, _)| *until > now);
            if f.len() >= FAILURES_MAX {
                f.clear();
            }
        }
        let ttl = failure_ttl(&detail);
        f.insert(
            (owner, sha.to_string(), name.to_string()),
            (now + ttl, detail),
        );
    }

    /// Останавливает пул превью: новые задания не принимаются, процессы завершаются (ждём не
    /// дольше 3 с).
    pub async fn shutdown(&self) {
        self.pool.close();
        let t = Instant::now();
        while !self.pool.worker_pids().is_empty() && t.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn pool(&self) -> &DocPool {
        &self.pool
    }

    pub fn cache(&self) -> &PreviewCache {
        &self.cache
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Имя файла кэша для `key` файла `filename`.
    pub fn file_name(&self, key: &PreviewKey, filename: &str) -> String {
        key.file_name(&self.tag, filename)
    }

    /// Разрешение на построение превью (не больше `2 × preview_workers` одновременно).
    pub async fn permit(&self) -> SemaphorePermit<'_> {
        self.builds
            .acquire()
            .await
            .expect("семафор превью не закрывается")
    }
}

/// Кэш превью на диске с ограничением размера.
#[derive(Clone)]
pub struct PreviewCache {
    inner: Arc<CacheInner>,
}

struct CacheInner {
    root: PathBuf,
    cap: u64,
    /// Занято байт (`None` — ещё не подсчитано: подсчёт при первой записи).
    used: Mutex<Option<u64>>,
    sweeping: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl PreviewCache {
    pub fn new(root: PathBuf, cap: u64) -> Self {
        PreviewCache {
            inner: Arc::new(CacheInner {
                root,
                cap,
                used: Mutex::new(None),
                sweeping: AtomicBool::new(false),
            }),
        }
    }

    /// Путь файла кэша; `None` — sha не hex или имя не из разрешённых символов (защита в глубину:
    /// вызывающий уже проверил оба).
    fn path(&self, owner: Id, sha: &str, name: &str) -> Option<PathBuf> {
        let safe = !name.is_empty()
            && !name.starts_with('.')
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-.".contains(c));
        (is_sha256(sha) && safe)
            .then(|| self.inner.root.join(owner.to_string()).join(sha).join(name))
    }

    /// Содержимое файла кэша, если есть; давнее чтение обновляет время файла (LRU).
    pub async fn get(&self, owner: Id, sha: &str, name: &str) -> Option<Vec<u8>> {
        let path = self.path(owner, sha, name)?;
        let data = tokio::fs::read(&path).await.ok()?;
        tokio::task::spawn_blocking(move || touch(&path));
        Some(data)
    }

    /// Удаляет файл кэша (испорченный).
    pub async fn remove(&self, owner: Id, sha: &str, name: &str) {
        if let Some(path) = self.path(owner, sha, name)
            && let Ok(m) = tokio::fs::metadata(&path).await
            && tokio::fs::remove_file(&path).await.is_ok()
            && let Some(u) = lock(&self.inner.used).as_mut()
        {
            *u = u.saturating_sub(m.len());
        }
    }

    /// Пишет файл кэша атомарно в фоне (ответ не ждёт диска); сверх предела — чистка.
    pub fn put(&self, owner: Id, sha: &str, name: &str, data: Vec<u8>) {
        if data.len() > ENTRY_MAX {
            return;
        }
        let Some(path) = self.path(owner, sha, name) else {
            return;
        };
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let len = data.len() as u64;
            // перезапись: учитывается разница размеров (ревью, M-5)
            let old = match write_atomic(&path, &data) {
                Ok(old) => old,
                Err(e) => {
                    tracing::warn!(error = %e, "кэш превью не записан");
                    return;
                }
            };
            let over = {
                let mut used = lock(&inner.used);
                // первый подсчёт уже видит записанный файл
                let cur = match *used {
                    Some(u) => u.saturating_add(len).saturating_sub(old),
                    None => sweep(&inner.root, u64::MAX),
                };
                *used = Some(cur);
                cur > inner.cap
            };
            if over && !inner.sweeping.swap(true, Ordering::AcqRel) {
                let left = sweep(&inner.root, inner.cap);
                *lock(&inner.used) = Some(left);
                inner.sweeping.store(false, Ordering::Release);
            }
        });
    }
}

/// Обновляет время изменения файла, если оно старше [`TOUCH_AFTER`].
fn touch(path: &Path) {
    let old = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > TOUCH_AFTER);
    if old && let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Запись через временный файл в том же каталоге и `rename`: читатель видит файл целиком или
/// не видит вовсе. Возвращает размер заменённого файла (0 — его не было).
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<u64> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("нет каталога"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{:016x}.tmp", rand::random::<u64>()));
    let old = std::fs::metadata(path).map_or(0, |m| m.len());
    let r = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map(|()| old)
}

/// Чистка кэша: файлы `root/<владелец>/<sha>/*`; давние временные — удаляются; если сумма больше
/// `cap`, самые давние по времени изменения удаляются, пока не останется ≤ 3/4 `cap`; пустые
/// каталоги — удаляются. Возвращает занятый объём.
fn sweep(root: &Path, cap: u64) -> u64 {
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for owner in read_dirs(root) {
        for sha in read_dirs(&owner) {
            for e in std::fs::read_dir(&sha).into_iter().flatten().flatten() {
                let Ok(m) = e.metadata() else { continue };
                if !m.is_file() {
                    continue;
                }
                let t = m.modified().unwrap_or(now);
                let p = e.path();
                let tmp = p.extension().is_some_and(|x| x == "tmp");
                if tmp && now.duration_since(t).unwrap_or_default() > TMP_STALE {
                    let _ = std::fs::remove_file(&p);
                    continue;
                }
                files.push((t, m.len(), p));
            }
            dirs.push(sha);
        }
        dirs.push(owner);
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total > cap {
        let target = cap / 4 * 3;
        files.sort_by_key(|f| f.0);
        for (_, len, p) in &files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(p).is_ok() {
                total = total.saturating_sub(*len);
            }
        }
        // пустые каталоги sha, затем владельцев (непустые не удаляются)
        for d in &dirs {
            let _ = std::fs::remove_dir(d);
        }
    }
    total
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_numbers_and_versions() {
        let s = PreviewKey::Sheet {
            no: 2,
            from: 10,
            rows: 50,
        }
        .file_name("ab12cd34", "Книга.CSV");
        let e = &blake3::hash(b"csv").to_hex()[..8];
        assert_eq!(
            s,
            format!(
                "sheet-2-10-50-e{e}-v{}.{PREVIEW_VERSION}-ab12cd34.json",
                zam_docs::EXTRACTOR_VERSION
            )
        );
        // расширение различает ключи текста и листа, миниатюры — нет
        let t = |n: &str| PreviewKey::Text.file_name("x", n);
        assert_eq!(t("a.rs"), t("b.RS"));
        assert_ne!(t("a.rs"), t("a.py"));
        assert_ne!(t("a.rs"), t("a"));
        let th = |n: &str| PreviewKey::Thumb { page: 1, px: 256 }.file_name("x", n);
        assert_eq!(th("a.png"), th("a.jpg"));
        assert!(th("a").starts_with("thumb-1-256-"));
        let c = PreviewCache::new(PathBuf::from("root"), 1);
        let owner = Id::new();
        let sha = "ab".repeat(32);
        assert!(c.path(owner, &sha, "text-v2.1-x.json").is_some());
        for bad in ["../x", "a/b", "..", ".hidden", "A.json", ""] {
            assert!(c.path(owner, &sha, bad).is_none(), "{bad}");
        }
        assert!(c.path(owner, "../..", "text.json").is_none());
    }

    #[test]
    fn failure_ttl_by_reason() {
        for d in [
            "превью не построено: parse_timeout",
            "превью не построено: parse_crash",
            "превью не построено: parse_oom",
        ] {
            assert_eq!(failure_ttl(d), TRANSIENT_FAILURE_TTL, "{d}");
        }
        for d in [
            "превью не построено: parse_error: xml (word/document.xml)",
            "превью не построено: too_large",
            "превью не построено: unsupported",
            "превью листа недоступно для этого файла",
        ] {
            assert_eq!(failure_ttl(d), FAILURE_TTL, "{d}");
        }
    }

    #[test]
    fn sweep_removes_oldest_to_three_quarters() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/preview-sweep-unit");
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("u").join("s");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..10u64 {
            let p = dir.join(format!("f{i}.png"));
            std::fs::write(&p, vec![0u8; 100]).unwrap();
            let t = SystemTime::now() - Duration::from_secs(1000 - i * 10);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        assert_eq!(sweep(&root, u64::MAX), 1000);
        let left = sweep(&root, 800);
        assert_eq!(left, 600);
        // удалены самые давние
        assert!(!dir.join("f0.png").exists() && !dir.join("f3.png").exists());
        assert!(dir.join("f4.png").exists() && dir.join("f9.png").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
