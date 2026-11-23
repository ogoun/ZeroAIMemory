//! Время расписаний с часовым поясом (M7.3 §6.1 пп. 3–4): общая функция слота у автокопий
//! (`[backup] at`/`tz`, [`crate::backup::schedule`]) и у ночных работ (`[works] at`/`tz`,
//! [`crate::works::schedule`]).
//!
//! - Пояс — имя IANA (`Europe/Moscow`), `UTC` или смещение `±HH:MM` ([`parse_tz`]); база поясов
//!   встроена в сервер (`jiff`, фича `tzdb-bundle-always`) — пакет `tzdata` на Linux не нужен.
//! - Слот — дата `day` в поясе `tz` и время `at`, переведённые в UTC ([`slot`]). Несуществующее
//!   местное время (переход на летнее) сдвигается вперёд на длину перехода, повторяющееся —
//!   первое из двух: правило `compatible` у `jiff`.
//! - Окно ночи — `[slot(d), slot(d) + window_min)` для `d` — вчера и сегодня в поясе `tz`
//!   ([`night_window`]): окно через полночь (`at = "23:30"`) не обрывается; ночь — дата слота.

use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp, civil};

/// Примеры пояса в тексте ошибки конфигурации.
pub const TZ_EXAMPLES: &str = "имя IANA (Europe/Moscow), UTC или смещение ±HH:MM (+03:00)";

/// Пояс конфигурации: имя IANA, `UTC` или смещение `±HH:MM` (часы 0..=18, минуты 0..=59).
/// Ошибка — текст с примерами ([`TZ_EXAMPLES`]).
pub fn parse_tz(s: &str) -> Result<TimeZone, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("utc") {
        return Ok(TimeZone::UTC);
    }
    if let Some(sign) = s.chars().next().filter(|c| *c == '+' || *c == '-') {
        return fixed_offset(&s[1..], sign == '-')
            .map(TimeZone::fixed)
            .ok_or_else(|| format!("неверное смещение «{s}»: {TZ_EXAMPLES}"));
    }
    TimeZone::get(s).map_err(|_| format!("неизвестный часовой пояс «{s}»: {TZ_EXAMPLES}"))
}

/// `HH:MM` смещения (без знака) → смещение со знаком `neg`.
fn fixed_offset(hm: &str, neg: bool) -> Option<Offset> {
    let (h, m) = hm.split_once(':')?;
    if h.len() != 2 || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    if h > 18 || m > 59 {
        return None;
    }
    let secs = h * 3600 + m * 60;
    Offset::from_seconds(if neg { -secs } else { secs }).ok()
}

/// Момент `at` (часы, минуты) даты `day` в поясе `tz`, в UTC: несуществующее местное время
/// (переход на летнее) — вперёд на длину перехода, повторяющееся — первое из двух
/// (`compatible`). Дата вне диапазона `jiff` (недостижимо для календаря сервера) — тот же момент
/// в UTC.
pub fn slot(day: civil::Date, at: (i8, i8), tz: &TimeZone) -> Timestamp {
    let dt = day.at(at.0, at.1, 0, 0);
    tz.to_ambiguous_timestamp(dt)
        .compatible()
        .or_else(|_| TimeZone::UTC.to_ambiguous_timestamp(dt).compatible())
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

/// Окно ночи, в котором лежит `now`: `(ночь — дата слота в поясе tz, начало окна, конец окна)`;
/// окно — `[slot(d), slot(d) + window_min)` для `d` — вчера и сегодня в поясе `tz`. `now` вне
/// обоих окон — `None`.
pub fn night_window(
    now: Timestamp,
    at: (i8, i8),
    tz: &TimeZone,
    window_min: u32,
) -> Option<(civil::Date, Timestamp, Timestamp)> {
    let today = now.to_zoned(tz.clone()).date();
    let len = SignedDuration::from_mins(i64::from(window_min));
    [today.yesterday().ok(), Some(today)]
        .into_iter()
        .flatten()
        .find_map(|d| {
            let start = slot(d, at, tz);
            let end = start.checked_add(len).ok()?;
            (start <= now && now < end).then_some((d, start, end))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Рабочий сервер памяти (§6.1 п. 1): 04:30 по Москве — 01:30 UTC сервера.
    #[test]
    fn slot_tz_moscow_0430_is_0130_utc() {
        let tz = parse_tz("Europe/Moscow").unwrap();
        assert_eq!(
            slot(civil::date(2026, 11, 23), (4, 30), &tz),
            ts("2026-11-23T01:30:00Z")
        );
        // летом смещение то же (без перехода с 2014 года)
        assert_eq!(
            slot(civil::date(2026, 7, 1), (4, 30), &tz),
            ts("2026-07-01T01:30:00Z")
        );
        // ночь — дата в поясе: 02:00 UTC 23-го — по Москве 05:00 23-го, в окне 04:30–06:30
        let (night, start, end) =
            night_window(ts("2026-11-23T02:00:00Z"), (4, 30), &tz, 120).unwrap();
        assert_eq!(night, civil::date(2026, 11, 23));
        assert_eq!(
            (start, end),
            (ts("2026-11-23T01:30:00Z"), ts("2026-11-23T03:30:00Z"))
        );
    }

    /// `America/New_York`: 02:30 в день перехода на летнее время (8 марта 2026, 02:00 → 03:00)
    /// не существует — слот 03:30 EDT (вперёд на длину перехода); в день возврата (1 ноября
    /// 2026, 02:00 → 01:00) 01:30 бывает дважды — первое (EDT).
    #[test]
    fn slot_dst_gap_and_fold() {
        let tz = parse_tz("America/New_York").unwrap();
        let gap = slot(civil::date(2026, 3, 8), (2, 30), &tz);
        assert_eq!(gap, ts("2026-03-08T07:30:00Z"));
        assert_eq!(
            gap.to_zoned(tz.clone()).datetime(),
            civil::date(2026, 3, 8).at(3, 30, 0, 0)
        );
        let fold = slot(civil::date(2026, 11, 1), (1, 30), &tz);
        // первое 01:30 — летнее (UTC−4), второе было бы 06:30 UTC
        assert_eq!(fold, ts("2026-11-01T05:30:00Z"));
        // обычный день — без сдвига
        assert_eq!(
            slot(civil::date(2026, 1, 15), (2, 30), &tz),
            ts("2026-01-15T07:30:00Z")
        );
    }

    /// Смещение `±HH:MM` — фиксированный пояс без переходов; `UTC` — в любом регистре; неверные
    /// смещения и имена — ошибка с примерами.
    #[test]
    fn slot_fixed_offset() {
        let tz = parse_tz("+03:00").unwrap();
        assert_eq!(
            slot(civil::date(2026, 11, 23), (4, 30), &tz),
            ts("2026-11-23T01:30:00Z")
        );
        let tz = parse_tz("-05:30").unwrap();
        assert_eq!(
            slot(civil::date(2026, 11, 23), (0, 0), &tz),
            ts("2026-11-23T05:30:00Z")
        );
        assert_eq!(parse_tz("UTC").unwrap(), TimeZone::UTC);
        assert_eq!(parse_tz("utc").unwrap(), TimeZone::UTC);
        assert_eq!(
            slot(civil::date(2026, 11, 23), (4, 30), &TimeZone::UTC),
            ts("2026-11-23T04:30:00Z")
        );
        for bad in [
            "Mars/Base",
            "+3:00",
            "+03",
            "+19:00",
            "+03:60",
            "03:00",
            "",
            "+0a:00",
        ] {
            let e = parse_tz(bad).unwrap_err();
            assert!(
                e.contains("Europe/Moscow") && e.contains("+03:00"),
                "{bad}: {e}"
            );
        }
    }

    /// `[backup] tz` по умолчанию — `UTC`: слот автокопии прежний (сегодняшний `at` в UTC), шаг
    /// расписания — те же действия, что до M7.3; с поясом слот — в нём.
    #[test]
    fn backup_tz_default_utc_unchanged() {
        use crate::backup::schedule::{Action, tick};
        use crate::config::{BackupSchedule, BackupSection};
        let mut b = BackupSection {
            schedule: BackupSchedule::Daily,
            ..BackupSection::default()
        };
        assert_eq!(b.tz, "UTC");
        assert_eq!(b.time_zone(), TimeZone::UTC);
        let before = ts("2026-11-23T03:29:00Z");
        let after = ts("2026-11-23T03:30:00Z");
        assert_eq!(tick(before, None, &[], &b), vec![Action::Skip]);
        assert_eq!(tick(after, None, &[], &b), vec![Action::Backup]);
        // попытка после сегодняшнего слота — до завтра ничего
        assert_eq!(
            tick(ts("2026-11-23T12:00:00Z"), Some(after), &[], &b),
            vec![Action::Skip]
        );
        // 03:30 по Москве — 00:30 UTC
        b.tz = "Europe/Moscow".into();
        assert_eq!(
            tick(ts("2026-11-23T00:29:00Z"), None, &[], &b),
            vec![Action::Skip]
        );
        assert_eq!(
            tick(ts("2026-11-23T00:30:00Z"), None, &[], &b),
            vec![Action::Backup]
        );
    }
}
