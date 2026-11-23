//! «Пора ли» ночной части (M7.3 §6.1 п. 4): чистая функция [`tick`] на окне ночи
//! [`crate::schedule::night_window`]; исполняет её [`super::night::NightRunner`] раз в тик.
//!
//! Отдельного состояния нет: `last` — ночь, уже начатая **этим процессом**. Перезапуск сервера в
//! окне начинает ту же ночь снова, а функции ночи сами по базе видят, что уже сделано (ночной
//! прогон проверки, ночная постановка работ). Вне окна — ничего: пропущенная ночь днём не
//! догоняется (решение координатора 40).

use jiff::tz::TimeZone;
use jiff::{Timestamp, civil};

use crate::config::WorksSection;

/// Ночь и конец её окна, если `now` в окне ночи (`[works] at`, `window_min`, пояс `tz`) и эта
/// ночь в процессе ещё не начиналась (`last` — последняя начатая); иначе `None`. `[works]
/// enabled = false` — `None`.
pub fn tick(
    now: Timestamp,
    last: Option<civil::Date>,
    cfg: &WorksSection,
    tz: &TimeZone,
) -> Option<(civil::Date, Timestamp)> {
    if !cfg.enabled {
        return None;
    }
    let (night, _, until) = crate::schedule::night_window(now, cfg.at_hm(), tz, cfg.window_min)?;
    last.is_none_or(|l| l < night).then_some((night, until))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::parse_tz;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn cfg(at: &str, window_min: u32) -> WorksSection {
        WorksSection {
            at: at.into(),
            window_min,
            ..WorksSection::default()
        }
    }

    /// Вне окна — ничего, и пропущенная ночь днём не догоняется (решение 40): сервер, поднятый в
    /// 10:00 без прошлой ночи, ждёт следующего окна; в окне — ночь и конец окна; конец окна
    /// исключён; `enabled = false` — никогда.
    #[test]
    fn works_tick_window_no_catch_up() {
        let c = cfg("04:30", 120);
        let utc = TimeZone::UTC;
        let d = civil::date(2026, 11, 23);
        for now in [
            "2026-11-23T10:00:00Z",
            "2026-11-23T04:29:59Z",
            "2026-11-23T06:30:00Z",
            "2026-11-23T23:59:00Z",
        ] {
            assert_eq!(tick(ts(now), None, &c, &utc), None, "{now}");
            assert_eq!(tick(ts(now), d.yesterday().ok(), &c, &utc), None, "{now}");
        }
        assert_eq!(
            tick(ts("2026-11-23T04:30:00Z"), None, &c, &utc),
            Some((d, ts("2026-11-23T06:30:00Z")))
        );
        assert_eq!(
            tick(ts("2026-11-23T06:29:59Z"), d.yesterday().ok(), &c, &utc),
            Some((d, ts("2026-11-23T06:30:00Z")))
        );
        let off = WorksSection {
            enabled: false,
            ..c.clone()
        };
        assert_eq!(tick(ts("2026-11-23T05:00:00Z"), None, &off, &utc), None);
        // в поясе: 04:30 по Москве — 01:30 UTC
        let msk = parse_tz("Europe/Moscow").unwrap();
        assert_eq!(tick(ts("2026-11-23T04:30:00Z"), None, &c, &msk), None);
        assert_eq!(
            tick(ts("2026-11-23T01:30:00Z"), None, &c, &msk),
            Some((d, ts("2026-11-23T03:30:00Z")))
        );
    }

    /// Перезапуск в окне продолжает ночь (`last` нового процесса — `None`); в том же процессе
    /// начатая ночь второй раз не начинается; следующая ночь — снова.
    #[test]
    fn works_tick_restart_in_window_continues() {
        let c = cfg("04:30", 120);
        let utc = TimeZone::UTC;
        let d = civil::date(2026, 11, 23);
        let now = ts("2026-11-23T05:45:00Z");
        assert_eq!(
            tick(now, None, &c, &utc),
            Some((d, ts("2026-11-23T06:30:00Z")))
        );
        assert_eq!(tick(now, Some(d), &c, &utc), None);
        assert_eq!(tick(ts("2026-11-23T06:00:00Z"), Some(d), &c, &utc), None);
        let next = d.tomorrow().unwrap();
        assert_eq!(
            tick(ts("2026-11-24T04:31:00Z"), Some(d), &c, &utc),
            Some((next, ts("2026-11-24T06:30:00Z")))
        );
    }

    /// Окно через полночь (`at = "23:30"`, 120 мин): в 00:30 — ночь вчерашней даты, конец — 01:30;
    /// с переходом на летнее время внутри окна (`America/New_York`, 7→8 марта 2026, 02:00 →
    /// 03:00): окно 23:30 EST + 240 мин кончается в 04:30 EDT, в 03:15 EDT — ночь 7 марта.
    #[test]
    fn works_tick_window_crosses_midnight() {
        let c = cfg("23:30", 120);
        let utc = TimeZone::UTC;
        let prev = civil::date(2026, 11, 22);
        assert_eq!(
            tick(ts("2026-11-23T00:30:00Z"), None, &c, &utc),
            Some((prev, ts("2026-11-23T01:30:00Z")))
        );
        assert_eq!(
            tick(ts("2026-11-22T23:30:00Z"), None, &c, &utc),
            Some((prev, ts("2026-11-23T01:30:00Z")))
        );
        assert_eq!(tick(ts("2026-11-23T01:30:00Z"), None, &c, &utc), None);
        assert_eq!(tick(ts("2026-11-23T00:30:00Z"), Some(prev), &c, &utc), None);

        let ny = parse_tz("America/New_York").unwrap();
        let c = cfg("23:30", 240);
        let night = civil::date(2026, 3, 7);
        // 23:30 EST 7 марта = 04:30 UTC 8 марта; +240 мин = 08:30 UTC = 04:30 EDT
        let until = ts("2026-03-08T08:30:00Z");
        assert_eq!(
            until.to_zoned(ny.clone()).datetime(),
            civil::date(2026, 3, 8).at(4, 30, 0, 0)
        );
        // 03:15 EDT 8 марта = 07:15 UTC
        assert_eq!(
            tick(ts("2026-03-08T07:15:00Z"), None, &c, &ny),
            Some((night, until))
        );
        assert_eq!(
            tick(ts("2026-03-08T04:30:00Z"), None, &c, &ny),
            Some((night, until))
        );
        assert_eq!(tick(until, None, &c, &ny), None);
    }
}
