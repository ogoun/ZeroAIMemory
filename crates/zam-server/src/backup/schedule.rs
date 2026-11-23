//! Автокопии по расписанию (M6 §12б.4): решение «пора ли» — чистая функция [`tick`], её
//! тест вызывает напрямую; исполняет действия воркер обслуживания (`maintenance`).

use std::path::PathBuf;

use jiff::{Timestamp, tz::TimeZone};

use crate::config::{BackupSchedule, BackupSection};

/// Действие расписания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ничего не делать.
    Skip,
    /// Снять копию (тем же кодом, что `zam-server backup`).
    Backup,
    /// Удалить эти копии (сверх `keep`, начиная с самых старых).
    Prune(Vec<PathBuf>),
}

/// Сегодняшний (UTC) момент `at` для `now`.
fn slot(now: Timestamp, cfg: &BackupSection) -> Option<Timestamp> {
    let (h, m) = cfg.at_hm();
    now.to_zoned(TimeZone::UTC)
        .date()
        .at(h, m, 0, 0)
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

/// Что делать в момент `now`: `last` — время последней копии **или попытки** (неудачная
/// попытка тоже занимает слот: повтор — в следующий, без копии каждую минуту), `existing` —
/// готовые копии каталога копий от старых к новым ([`super::list_copies`]).
///
/// - `schedule = "off"` — [`Action::Skip`];
/// - копия — если `now` не раньше сегодняшнего `at` (UTC) и сегодня после `at` копии (попытки)
///   ещё не было (пропущенный вчерашний срок не догоняется до сегодняшнего `at`); удаления в
///   этом же шаге нет: лишние копии удаляются следующим шагом **после успешной** копии, по
///   фактическому списку — неудачная копия не стоит хороших;
/// - иначе удаление копий сверх `keep`, начиная с самых старых.
pub fn tick(
    now: Timestamp,
    last: Option<Timestamp>,
    existing: &[PathBuf],
    cfg: &BackupSection,
) -> Vec<Action> {
    if cfg.schedule == BackupSchedule::Off {
        return vec![Action::Skip];
    }
    if slot(now, cfg).is_some_and(|s| now >= s && last.is_none_or(|l| l < s)) {
        return vec![Action::Backup];
    }
    let extra = prune_list(existing, cfg.keep);
    if !extra.is_empty() {
        return vec![Action::Prune(extra)];
    }
    vec![Action::Skip]
}

/// Копии сверх `keep` из `existing` (от старых к новым, [`super::list_copies`]) — самые старые;
/// не больше — пусто. Одна ротация у расписания и у копии из WebUI при `schedule = "daily"`
/// (M7 §5.6 п. 1, [`super::runner`]).
pub fn prune_list(existing: &[PathBuf], keep: u32) -> Vec<PathBuf> {
    let keep = keep as usize;
    existing[..existing.len().saturating_sub(keep)].to_vec()
}
