//! Единый текстовый формат времени в БД: UTC, микросекунды, `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
//! Фиксированная ширина делает лексикографический порядок строк равным хронологическому.

use jiff::Timestamp;
use rusqlite::Row;

use crate::StoreError;

/// Форматирует момент времени для хранения в TEXT-колонке.
pub fn fmt_ts(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%S.%6fZ").to_string()
}

/// Сериализация времени в JSON в том же формате, что и в БД (`serialize_with`).
pub fn ser_ts<S: serde::Serializer>(ts: &Timestamp, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&fmt_ts(*ts))
}

/// [`ser_ts`] для необязательного времени (`None` → `null`).
pub fn ser_ts_opt<S: serde::Serializer>(ts: &Option<Timestamp>, s: S) -> Result<S::Ok, S::Error> {
    match ts {
        Some(t) => s.serialize_str(&fmt_ts(*t)),
        None => s.serialize_none(),
    }
}

/// Разбирает строку времени из БД.
pub fn parse_ts(s: &str) -> Result<Timestamp, StoreError> {
    s.parse()
        .map_err(|e| StoreError::Corrupt(format!("некорректное время «{s}»: {e}")))
}

/// Ошибка преобразования значения колонки `i` для `query_map`.
pub(crate) fn conv_err<E: std::error::Error + Send + Sync + 'static>(
    i: usize,
    e: E,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
}

/// Обязательное время из колонки `i`.
pub(crate) fn ts_at(r: &Row, i: usize) -> rusqlite::Result<Timestamp> {
    let s: String = r.get(i)?;
    parse_ts(&s).map_err(|e| conv_err(i, e))
}

/// Необязательное время из колонки `i`.
pub(crate) fn ts_opt(r: &Row, i: usize) -> rusqlite::Result<Option<Timestamp>> {
    r.get::<_, Option<String>>(i)?
        .map(|s| parse_ts(&s).map_err(|e| conv_err(i, e)))
        .transpose()
}

/// Момент `verified_at:` заметки (M4.5 §7): дата `YYYY-MM-DD` — полночь UTC, полная метка
/// RFC 3339 — как есть; иное — `None` (поле во frontmatter свободное).
pub fn parse_verified_at(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    if let Ok(t) = s.parse::<Timestamp>() {
        return Some(t);
    }
    let d: jiff::civil::Date = s.parse().ok()?;
    d.to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

/// Возраст записи в днях (M4.5 §7): `floor((now − max(updated_at, verified_at)) / 86400)`;
/// будущее время — 0.
pub fn age_days(now: Timestamp, updated_at: Timestamp, verified_at: Option<&str>) -> u32 {
    let base = verified_at
        .and_then(parse_verified_at)
        .map_or(updated_at, |v| v.max(updated_at));
    let secs = now.as_second() - base.as_second();
    u32::try_from(secs.max(0) / 86_400).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_days_from_updated_or_verified() {
        let now: Timestamp = "2026-10-29T12:00:00Z".parse().unwrap();
        let upd: Timestamp = "2026-10-19T13:00:00Z".parse().unwrap();
        assert_eq!(age_days(now, upd, None), 9);
        assert_eq!(age_days(now, upd, Some("2026-10-27")), 2);
        // проверка раньше правки — от правки
        assert_eq!(age_days(now, upd, Some("2026-01-01")), 9);
        assert_eq!(age_days(now, upd, Some("2026-10-29T11:00:00Z")), 0);
        assert_eq!(age_days(now, upd, Some("мусор")), 9);
        assert_eq!(age_days(upd, now, None), 0);
    }

    #[test]
    fn fixed_width_and_roundtrip() {
        let t: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        assert_eq!(fmt_ts(t), "2026-10-18T12:00:00.000000Z");
        let t2: Timestamp = "2026-10-18T12:00:00.123456789Z".parse().unwrap();
        assert_eq!(fmt_ts(t2), "2026-10-18T12:00:00.123456Z");
        assert_eq!(
            parse_ts(&fmt_ts(t2)).unwrap().as_microsecond(),
            t2.as_microsecond()
        );
        assert!(matches!(parse_ts("мусор"), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn lexicographic_order_is_chronological() {
        let a: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        let b: Timestamp = "2026-10-18T12:00:00.5Z".parse().unwrap();
        let c: Timestamp = "2026-10-18T12:00:01Z".parse().unwrap();
        let (sa, sb, sc) = (fmt_ts(a), fmt_ts(b), fmt_ts(c));
        assert!(sa < sb && sb < sc);
    }
}
