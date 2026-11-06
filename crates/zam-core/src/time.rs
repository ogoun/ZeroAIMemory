//! Дата и время в тексте агенту (M6 §4.4): `yyyy-MM-dd HH:mm:ss` UTC, без `T`, `Z` и смещения.
//! Без `jiff`: вход — строка RFC 3339 (так время приходит из JSON API и из `fmt_ts` хранилища);
//! сервер вызывает `agent_ts(&fmt_ts(t))`, CLI — на строках JSON ответа.

/// Момент RFC 3339 → `yyyy-MM-dd HH:mm:ss` UTC (смещение пересчитывается, доли секунды
/// отбрасываются). Не RFC 3339 (в том числе дата без времени) — `None`.
pub fn agent_ts(rfc3339: &str) -> Option<String> {
    let u = parse_utc(rfc3339)?;
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        u.year, u.month, u.day, u.hour, u.minute, u.second
    ))
}

/// Дата момента RFC 3339 по UTC, `yyyy-MM-dd`; не RFC 3339 — `None`.
pub fn agent_date(rfc3339: &str) -> Option<String> {
    let u = parse_utc(rfc3339)?;
    Some(format!("{:04}-{:02}-{:02}", u.year, u.month, u.day))
}

/// Разобранный момент, уже в UTC (с точностью до секунды).
struct Utc {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// `YYYY-MM-DDTHH:MM:SS[.доли](Z|±HH:MM)` → UTC; иное — `None`.
fn parse_utc(s: &str) -> Option<Utc> {
    let b = s.as_bytes();
    if b.len() < 20 || !b.is_ascii() {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = &b[from..from + len];
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0u32, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let sep = |at: usize, ok: &[u8]| ok.contains(&b[at]);
    if !(sep(4, b"-") && sep(7, b"-") && sep(10, b"Tt") && sep(13, b":") && sep(16, b":")) {
        return None;
    }
    let (year, month, day) = (i64::from(num(0, 4)?), num(5, 2)?, num(8, 2)?);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut i = 19;
    if b[i] == b'.' || b[i] == b',' {
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        i += 1 + digits;
    }
    let offset_min: i64 = match &b[i..] {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let m = i64::from(oh * 60 + om);
            if *sign == b'+' { m } else { -m }
        }
        _ => return None,
    };
    let local =
        days_from_civil(year, month, day) * 86_400 + i64::from(hour * 3600 + minute * 60 + second);
    let utc = local - offset_min * 60;
    let (days, secs) = (utc.div_euclid(86_400), utc.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let secs = u32::try_from(secs).ok()?;
    Some(Utc {
        year,
        month,
        day,
        hour: secs / 3600,
        minute: secs / 60 % 60,
        second: secs % 60,
    })
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Дней от 1970-01-01 (алгоритм Хиннанта, пролептический григорианский календарь).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Обратное к [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ts_cases() {
        let ts = |s: &str| agent_ts(s);
        // дробные секунды — отбрасываются (как `%S`), не округляются
        assert_eq!(
            ts("2026-10-10T14:03:22.123Z").as_deref(),
            Some("2026-10-10 14:03:22")
        );
        assert_eq!(
            ts("2026-10-10T14:03:22.999999Z").as_deref(),
            Some("2026-10-10 14:03:22")
        );
        // без долей, строчные `t`/`z`
        assert_eq!(
            ts("2026-10-10T14:03:22Z").as_deref(),
            Some("2026-10-10 14:03:22")
        );
        assert_eq!(
            ts("2026-10-10t14:03:22z").as_deref(),
            Some("2026-10-10 14:03:22")
        );
        // смещение пересчитывается в UTC, в том числе через границу суток, месяца и года
        assert_eq!(
            ts("2026-10-10T14:03:22+03:00").as_deref(),
            Some("2026-10-10 11:03:22")
        );
        assert_eq!(
            ts("2026-01-01T01:00:00+02:00").as_deref(),
            Some("2025-12-31 23:00:00")
        );
        assert_eq!(
            ts("2024-02-28T23:30:00-01:00").as_deref(),
            Some("2024-02-29 00:30:00")
        );
        assert_eq!(
            ts("2026-10-10T14:03:22-00:00").as_deref(),
            Some("2026-10-10 14:03:22")
        );
        // мусор и не RFC 3339 — `None`
        for bad in [
            "",
            "мусор",
            "2026-10-10",
            "2026-10-10 14:03:22",
            "2026-10-10T14:03Z",
            "2026-13-10T14:03:22Z",
            "2026-02-30T14:03:22Z",
            "2026-10-10T24:00:00Z",
            "2026-10-10T14:60:00Z",
            "2026-10-10T14:03:22.Z",
            "2026-10-10T14:03:22+3:00",
            "2026-10-10T14:03:22+24:00",
            "2026-10-10T14:03:22Zx",
            "２０２６-10-10T14:03:22Z",
        ] {
            assert_eq!(ts(bad), None, "{bad}");
        }
        // дата
        assert_eq!(
            agent_date("2026-10-10T14:03:22.123456Z").as_deref(),
            Some("2026-10-10")
        );
        assert_eq!(
            agent_date("2026-10-10T01:00:00+03:00").as_deref(),
            Some("2026-10-09")
        );
        assert_eq!(agent_date("2026-10-10"), None);
    }
}
