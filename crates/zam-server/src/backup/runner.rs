//! Копия в процессе сервера (M7 §5.6 п. 1): **один замок** у страницы «Администрирование →
//! Копии» (`POST /admin/backups`) и автокопии воркера обслуживания
//! ([`crate::maintenance::backup_tick`]).
//!
//! - [`BackupRunner::try_start`] берёт замок или отвечает [`Busy`] с тем, кто его держит; замок
//!   снимает [`RunGuard`] — при итоге ([`RunGuard::run`], [`RunGuard::release`]) или при
//!   уничтожении (паника потока копии, задача снята при остановке): итог тогда — ошибка
//!   «прервана»;
//! - [`RunGuard::run`] — копия тем же [`super::backup`] с [`super::BackupOpts::default`] в
//!   блокирующем потоке (обработчики запросов на CPU-сервере не ждут её), после успеха при
//!   `schedule = "daily"` — ротация `keep` ([`super::schedule::prune_list`], как у автокопии;
//!   при `off` — без ротации, решение координатора 24), аудит `server.backup {by, ok, name, bytes,
//!   ms, error}` и итог в [`RunState::last`]. Замок держится, пока поток копии жив: снятая задача
//!   (остановка сервера) не отпускает его раньше конца копии;
//! - [`BackupRunner::state`] — лёгкое чтение для опроса страницы раз в 3 с.
//!
//! Путь копии в аудите и ответе — только имя каталога копии; тексты ошибок — без путей каталога
//! данных и каталога копий ([`redact`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use jiff::Timestamp;
use serde::Serialize;
use serde_json::json;
use zam_store::SystemStore;
use zam_store::time::ser_ts;

use super::{BackupError, BackupOpts, BackupReport, CopySummary};
use crate::config::{BackupSchedule, ServerConfig};

/// Кто начал копию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunBy {
    /// Страница «Копии» (`POST /admin/backups`).
    WebUi,
    /// Автокопия по расписанию `[backup] schedule = "daily"`.
    Schedule,
}

impl RunBy {
    /// Значение поля `by` (ответ, аудит).
    pub fn as_str(self) -> &'static str {
        match self {
            RunBy::WebUi => "webui",
            RunBy::Schedule => "schedule",
        }
    }
}

/// Идущая копия (или шаг расписания под замком).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Running {
    #[serde(serialize_with = "ser_ts")]
    pub started_at: Timestamp,
    pub by: RunBy,
}

/// Итог копии без пути: имя каталога копии и числа [`BackupReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportView {
    pub name: String,
    pub dbs: usize,
    pub blobs: usize,
    pub linked: usize,
    pub copied: usize,
    pub bytes: u64,
    pub ms: u64,
    pub drift: Vec<String>,
}

impl ReportView {
    fn of(r: &BackupReport) -> ReportView {
        ReportView {
            name: copy_name_of(&r.path),
            dbs: r.dbs,
            blobs: r.blobs,
            linked: r.linked,
            copied: r.copied,
            bytes: r.bytes,
            ms: r.ms,
            drift: r.drift.clone(),
        }
    }
}

/// Последний итог копии в процессе (с запуска сервера).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastRun {
    #[serde(serialize_with = "ser_ts")]
    pub finished_at: Timestamp,
    pub by: RunBy,
    pub ok: bool,
    /// Причина неудачи (без путей), у успеха — `None`.
    pub error: Option<String>,
    pub report: Option<ReportView>,
}

/// Состояние для страницы: идущая копия и последний итог.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunState {
    pub running: Option<Running>,
    pub last: Option<LastRun>,
}

/// Замок занят: кто и с какого момента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy(pub Running);

/// Сводки копий по описи: копия неизменна, опись читается один раз — опрос страницы видит
/// размер и время изменения описи (без чтения файла).
type SummaryCache = HashMap<PathBuf, (u64, Option<SystemTime>, CopySummary)>;

#[derive(Debug, Default)]
struct Inner {
    running: Option<Running>,
    last: Option<LastRun>,
}

/// Замок и состояние копии в процессе; дёшево клонируется (общие части — `Arc`).
#[derive(Debug, Clone, Default)]
pub struct BackupRunner {
    inner: Arc<Mutex<Inner>>,
    summaries: Arc<Mutex<SummaryCache>>,
    /// Удаление копий — по одному: `DELETE /admin/backups/{name}` и ротация `keep` одной и той же
    /// копии не идут одновременно — второе видит, что копии уже нет (404), а не сбой удаления.
    /// Отдельно от замка копии: удалить готовую копию можно и во время копии.
    removal: Arc<Mutex<()>>,
}

impl BackupRunner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Берёт замок: свободен — [`RunGuard`] (идущая — `by`, начало — сейчас), занят — [`Busy`].
    pub fn try_start(&self, by: RunBy) -> Result<RunGuard, Busy> {
        let mut g = self.lock();
        if let Some(r) = &g.running {
            return Err(Busy(r.clone()));
        }
        let running = Running {
            started_at: zam_store::time::now(),
            by,
        };
        g.running = Some(running.clone());
        Ok(RunGuard {
            inner: self.inner.clone(),
            removal: self.removal.clone(),
            running,
            done: false,
        })
    }

    /// Удаляет готовую копию `name` каталога копий `dir` ([`super::delete_copy`]) под замком
    /// удаления. Блокирующая: звать вне потоков runtime.
    pub fn delete(&self, dir: &Path, name: &str) -> Result<(), BackupError> {
        let _one = lock_removal(&self.removal);
        super::delete_copy(dir, name)
    }

    /// Удаляет копии каталога копий `dir` сверх `keep` — `Prune` расписания ([`rotate`]); число
    /// удалённых. Блокирующая: звать вне потоков runtime.
    pub fn rotate(&self, dir: &Path, keep: u32) -> usize {
        rotate(&self.removal, dir, keep)
    }

    /// Идущая копия и последний итог (снимок под мьютексом, без ввода-вывода).
    pub fn state(&self) -> RunState {
        let g = self.lock();
        RunState {
            running: g.running.clone(),
            last: g.last.clone(),
        }
    }

    /// Готовые копии каталога копий `dir` со сводками описи, **новые первыми**. Опись читается
    /// один раз на копию; дальше — по размеру и времени изменения описи из кэша. Копия с
    /// нечитаемой описью в список не входит (DEBUG). Блокирующая: звать вне потоков runtime.
    pub fn summaries(&self, dir: &Path) -> Vec<CopySummary> {
        let copies = super::list_copies(dir);
        let mut cache = self.summaries.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|p, _| copies.contains(p));
        let mut out = Vec::with_capacity(copies.len());
        for p in copies.iter().rev() {
            let Ok(md) = std::fs::metadata(p.join(super::MANIFEST_FILE)) else {
                continue;
            };
            let key = (md.len(), md.modified().ok());
            if let Some((len, mtime, s)) = cache.get(p)
                && (*len, *mtime) == key
            {
                out.push(s.clone());
                continue;
            }
            match super::copy_summary(p) {
                Some(s) => {
                    cache.insert(p.clone(), (key.0, key.1, s.clone()));
                    out.push(s);
                }
                None => tracing::debug!(path = %p.display(), "опись копии не прочитана"),
            }
        }
        out
    }
}

/// Замок удаления копий (отравленный — тот же: удаление идемпотентно).
fn lock_removal(m: &Mutex<()>) -> std::sync::MutexGuard<'_, ()> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ротация `keep` каталога копий `dir` под замком удаления: список копий читается уже под
/// замком — копия, удалённая из WebUI, в него не попадёт, и лишней не удалится (ТЗ §16.4: «по
/// фактическому списку»). Одна у ротации после копии и у `Prune` расписания; число удалённых.
fn rotate(removal: &Mutex<()>, dir: &Path, keep: u32) -> usize {
    let _one = lock_removal(removal);
    super::prune(&super::schedule::prune_list(&super::list_copies(dir), keep))
}

/// Замок взят; снимается при итоге или уничтожении (без итога — «прервана»).
#[derive(Debug)]
pub struct RunGuard {
    inner: Arc<Mutex<Inner>>,
    removal: Arc<Mutex<()>>,
    running: Running,
    done: bool,
}

/// Итог [`RunGuard::run`]: отчёт или причина (без путей) и сколько копий удалено ротацией.
#[derive(Debug)]
pub struct RunOutcome {
    pub result: Result<BackupReport, String>,
    pub pruned: usize,
}

/// Текст итога, когда поток копии упал или задачу сняли до итога.
const ABORTED: &str = "копия прервана: поток копии завершился аварийно или сервер останавливается";

impl RunGuard {
    /// Кто держит замок и с какого момента.
    pub fn running(&self) -> &Running {
        &self.running
    }

    fn close(&mut self, last: Option<LastRun>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.running = None;
        if last.is_some() {
            g.last = last;
        }
        self.done = true;
    }

    /// Снять замок без итога копии (шаг расписания без копии — только удаление сверх `keep`).
    pub fn release(mut self) {
        self.close(None);
    }

    /// Копия под этим замком целиком (см. модуль): блокирующий поток, ротация при `daily`,
    /// аудит `server.backup` (`actor` — `admin` у WebUI, `system` у расписания; `ip` — адрес
    /// клиента), итог в состоянии; замок снимается в конце. Сбой аудита — WARN, не итог копии.
    pub async fn run(
        self,
        cfg: &ServerConfig,
        system: &SystemStore,
        ip: Option<&str>,
    ) -> RunOutcome {
        let by = self.running.by;
        let c = cfg.clone();
        let rotate_keep = cfg.backup.schedule == BackupSchedule::Daily;
        // замок — в потоке копии: снятая задача не отпускает его раньше конца копии, паника
        // копии снимает его с итогом «прервана»
        let joined = tokio::task::spawn_blocking(move || {
            let r = super::backup(&c, &BackupOpts::default());
            let mut pruned = 0;
            if rotate_keep && r.is_ok() {
                pruned = rotate(&self.removal, &c.backup.dir(&c.data_dir), c.backup.keep);
            }
            (self, r, pruned)
        })
        .await;
        let (mut guard, result, pruned) = match joined {
            Ok((g, Ok(rep), n)) => (Some(g), Ok(rep), n),
            Ok((g, Err(e), _)) => {
                // полный текст (с путями) — только в журнал сервера
                tracing::warn!(by = by.as_str(), error = %e, "копия не снята");
                (Some(g), Err(redact(&e.to_string(), cfg)), 0)
            }
            // паника потока: замок снят уничтожением, итог — «прервана»
            Err(e) => {
                tracing::warn!(
                    panic = e.is_panic(),
                    "копия завершилась аварийно (поток копии)"
                );
                (None, Err(ABORTED.to_string()), 0)
            }
        };
        let report = result.as_ref().ok().map(ReportView::of);
        let actor = match by {
            RunBy::WebUi => "admin",
            RunBy::Schedule => "system",
        };
        let target = report.as_ref().map(|r| r.name.clone());
        let details = json!({
            "by": by.as_str(),
            "ok": result.is_ok(),
            "name": report.as_ref().map(|r| r.name.as_str()),
            "bytes": report.as_ref().map(|r| r.bytes),
            "ms": report.as_ref().map(|r| r.ms),
            "error": result.as_ref().err(),
        });
        if let Err(e) = system
            .audit(actor, None, "server.backup", target.as_deref(), ip, details)
            .await
        {
            tracing::warn!(error = %e, "аудит server.backup не записан");
        }
        if let Some(g) = guard.as_mut() {
            g.close(Some(LastRun {
                finished_at: zam_store::time::now(),
                by,
                ok: result.is_ok(),
                error: result.as_ref().err().cloned(),
                report,
            }));
        }
        RunOutcome { result, pruned }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let by = self.running.by;
        self.close(Some(LastRun {
            finished_at: zam_store::time::now(),
            by,
            ok: false,
            error: Some(ABORTED.to_string()),
            report: None,
        }));
    }
}

/// Имя каталога (архива) копии — последняя часть пути.
fn copy_name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Текст ошибки копии без путей: каталог копий — `<backup.dir>`, каталог данных —
/// `<data_dir>` (ответ и аудит несут только имя копии; полный текст — в журнале сервера).
pub fn redact(text: &str, cfg: &ServerConfig) -> String {
    let dir = cfg.backup.dir(&cfg.data_dir);
    let mut out = text.to_string();
    // каждый путь — в записи ОС и с другим разделителем (`/` и `\` в одном пути на Windows);
    // длинный — первым: каталог копий по умолчанию лежит внутри каталога данных
    let mut paths: Vec<(String, &str)> = Vec::new();
    for (p, mark) in [(dir, "<backup.dir>"), (cfg.data_dir.clone(), "<data_dir>")] {
        let s = p.display().to_string();
        for v in [s.replace('\\', "/"), s.replace('/', "\\"), s] {
            if !v.is_empty() && !paths.iter().any(|(x, _)| *x == v) {
                paths.push((v, mark));
            }
        }
    }
    paths.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    for (p, mark) in paths {
        out = out.replace(&p, mark);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Замок один: второй `try_start` — `Busy` с первым; итог без вызова `run` (паника, снятая
    /// задача) — «прервана», `release` итога не пишет.
    #[test]
    fn runner_lock_and_abort() {
        let r = BackupRunner::default();
        let g = r.try_start(RunBy::WebUi).unwrap();
        let busy = r.try_start(RunBy::Schedule).unwrap_err();
        assert_eq!(busy.0, *g.running());
        assert_eq!(r.state().running.as_ref().map(|x| x.by), Some(RunBy::WebUi));
        drop(g);
        let st = r.state();
        assert_eq!(st.running, None);
        let last = st.last.expect("итог прерванной копии");
        assert!(!last.ok);
        assert_eq!(last.error.as_deref(), Some(ABORTED));
        assert_eq!(last.by, RunBy::WebUi);
        // снятие без итога — прежний итог остаётся
        let g = r.try_start(RunBy::Schedule).unwrap();
        g.release();
        let st = r.state();
        assert_eq!(st.running, None);
        assert_eq!(st.last.unwrap().by, RunBy::WebUi);
        // паника в потоке с замком снимает его
        let r2 = r.clone();
        let g = r2.try_start(RunBy::Schedule).unwrap();
        let h = std::thread::spawn(move || {
            let _g = g;
            panic!("сбой копии");
        });
        assert!(h.join().is_err());
        let st = r.state();
        assert_eq!(st.running, None);
        assert_eq!(st.last.unwrap().by, RunBy::Schedule);
    }

    #[test]
    fn redact_paths_of_config() {
        let cfg = ServerConfig {
            data_dir: PathBuf::from("/srv/zam/data"),
            ..ServerConfig::default()
        };
        let t = "базу /srv/zam/data/users/a.db не открыть; копия в /srv/zam/data/backups/x";
        assert_eq!(
            redact(t, &cfg),
            "базу <data_dir>/users/a.db не открыть; копия в <backup.dir>/x"
        );
    }
}
