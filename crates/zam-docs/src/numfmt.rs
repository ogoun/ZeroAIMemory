//! Даты и время ячеек XLSX по стилю (`xl/styles.xml`: `numFmts`, `cellXfs`; задача 10 M5).
//!
//! Excel хранит дату числом — серийным номером дня (`45231` = 2023-11-01), а вид ячейки задаёт
//! формат числа её стиля. Без разбора стилей дата в тексте извлечения — число, и поиск по дате
//! её не находит (eval документов, `eval/results/2026-11-02-m5-docs-summary.md`).
//!
//! Текст даты — **как её показывает Excel**, если свой формат числовой (`dd.mm.yyyy`,
//! `yyyy-mm-dd hh:mm`, `d/m/yy`, литералы в кавычках): пользователь ищет то, что видит в
//! таблице, а FTS ищет слова фразой по порядку — «15.03.2021» не найдёт `2021-03-15` (замер
//! eval). Встроенные форматы (14, 22 — по локали Excel, она неизвестна), названия месяцев и
//! дней (`mmm`, `dddd`), `AM/PM` и доли секунды — ISO 8601 (`2023-11-01`, `2023-11-01 12:30`,
//! `18:00`). Проценты, валюта и прочие числовые форматы остаются числом как есть.
//!
//! Формат — дата, если в коде формата (без строк в кавычках, экранированных символов,
//! `[…]`-секций цвета и локали) есть `y` или `d`, только время — `h` или `s`; `m` без них —
//! месяц (дата). Длительности (`[h]:mm`, `[mm]:ss`) — не дата: число остаётся числом.
//! Встроенные форматы — по номерам ECMA-376 §18.8.30 (14–22, 45–47) и восточноазиатским (27–36,
//! 50–58). Система 1904 (`workbookPr date1904`) учитывается; ошибка високосного 1900 года
//! Excel — тоже (номер 60 — «1900-02-29»).

/// Вид формата числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DateKind {
    /// Не дата: число как есть.
    #[default]
    None,
    Date,
    DateTime,
    Time,
}

/// Стилей ячеек (`cellXfs`) и своих форматов (`numFmts`) не больше: Excel хранит до 64 000.
pub(crate) const MAX_STYLES: usize = 65_536;
/// Длина кода своего формата, символов (Excel — до 255).
const CODE_MAX: usize = 255;

/// Формат числа стиля ячейки: вид даты и код своего формата (для вывода как в Excel).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Style {
    pub kind: DateKind,
    pub code: Option<std::rc::Rc<str>>,
}

impl Style {
    pub fn builtin(id: u32) -> Style {
        Style {
            kind: builtin(id),
            code: None,
        }
    }

    pub fn custom(code: &str) -> Style {
        let kind = classify(code);
        Style {
            kind,
            code: (kind != DateKind::None && code.chars().count() <= CODE_MAX)
                .then(|| std::rc::Rc::from(code)),
        }
    }
}

/// Вид встроенного формата `numFmtId`.
pub(crate) fn builtin(id: u32) -> DateKind {
    match id {
        14..=17 | 27..=31 | 36 | 50..=58 => DateKind::Date,
        22 => DateKind::DateTime,
        18..=21 | 32..=35 | 45 | 47 => DateKind::Time,
        _ => DateKind::None,
    }
}

/// Вид своего формата по коду (`dd\.mm\.yyyy`, `[$-419]d mmmm yyyy`, `h:mm`, `#,##0.00 "₽"`).
pub(crate) fn classify(code: &str) -> DateKind {
    let mut plain = String::with_capacity(code.len());
    let mut chars = code.chars();
    // только первая секция (положительные числа): `0;-0;"ноль"`
    while let Some(c) = chars.next() {
        match c {
            ';' => break,
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let mut inner = String::new();
                for q in chars.by_ref() {
                    if q == ']' {
                        break;
                    }
                    inner.push(q);
                }
                // длительность: `[h]`, `[hh]`, `[m]`, `[ss]` — не дата и не время дня
                let l = inner.to_ascii_lowercase();
                if !l.is_empty() && l.chars().all(|c| matches!(c, 'h' | 'm' | 's')) {
                    return DateKind::None;
                }
            }
            c => plain.push(c.to_ascii_lowercase()),
        }
    }
    if plain.contains("general") {
        return DateKind::None;
    }
    let ymd = plain.contains('y') || plain.contains('d');
    let hs = plain.contains('h') || plain.contains('s');
    match (ymd, hs, plain.contains('m')) {
        (true, true, _) => DateKind::DateTime,
        (true, false, _) => DateKind::Date,
        (false, true, _) => DateKind::Time,
        (false, false, true) => DateKind::Date,
        _ => DateKind::None,
    }
}

/// Наибольший серийный номер Excel (9999-12-31).
const MAX_SERIAL: f64 = 2_958_465.0;

/// Гражданская дата из дней от 1970-01-01 (алгоритм Хиннанта).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Дни от 1970-01-01 до 1899-12-30 (нулевой день серийных номеров Excel после 1900-02-28).
const EPOCH_1900: i64 = -25_569;
/// До 1904-01-01 (нулевой день системы 1904).
const EPOCH_1904: i64 = -24_107;

/// Текст по числовому коду формата (`dd.mm.yyyy`); `None` — в коде есть то, что здесь не
/// выводится (названия месяцев и дней, `AM/PM`, доли секунды, числовые знаки) — тогда ISO.
fn render(code: &str, (y, mo, d): (i64, u32, u32), (h, mi, s): (i64, i64, i64)) -> Option<String> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // последний выведенный элемент — часы (следующее `m` — минуты)
    let mut after_hour = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ';' => break,
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    out.push(n);
                }
                i += 1;
            }
            '_' | '*' => i += 1,
            '[' => {
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
            }
            c if c.is_ascii_alphabetic() => {
                let l = c.to_ascii_lowercase();
                let mut n = 0;
                while i + n < chars.len() && chars[i + n].to_ascii_lowercase() == l {
                    n += 1;
                }
                // `m` — минуты сразу после часов или перед секундами
                let minutes = l == 'm' && n <= 2 && {
                    let rest = chars[i + n..].iter().find(|c| c.is_ascii_alphabetic());
                    after_hour || rest.is_some_and(|c| c.eq_ignore_ascii_case(&'s'))
                };
                let piece = match (l, n) {
                    ('y', 1 | 2) => format!("{:02}", y.rem_euclid(100)),
                    ('y', _) => format!("{y:04}"),
                    ('d', 1) => d.to_string(),
                    ('d', 2) => format!("{d:02}"),
                    ('m', 1) if minutes => mi.to_string(),
                    ('m', 2) if minutes => format!("{mi:02}"),
                    ('m', 1) => mo.to_string(),
                    ('m', 2) => format!("{mo:02}"),
                    ('h', 1) => h.to_string(),
                    ('h', _) => format!("{h:02}"),
                    ('s', 1) => s.to_string(),
                    ('s', 2) => format!("{s:02}"),
                    _ => return None,
                };
                after_hour = l == 'h';
                out.push_str(&piece);
                i += n - 1;
            }
            '0' | '#' | '?' | '@' | '%' => return None,
            c if c.is_alphabetic() => return None,
            c => out.push(c),
        }
        i += 1;
    }
    let t = out.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Текст серийного номера `v` по стилю `st` в `out`: свой числовой формат — как в Excel, иначе
/// ISO 8601; `false` — не дата (вне диапазона или отрицательное), `out` не тронут.
pub(crate) fn format_serial(v: f64, st: &Style, date1904: bool, out: &mut String) -> bool {
    use std::fmt::Write;
    let kind = st.kind;
    if kind == DateKind::None || !v.is_finite() || v < 0.0 || v > MAX_SERIAL {
        return false;
    }
    let mut days = v.trunc() as i64;
    let mut secs = ((v - v.trunc()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        secs -= 86_400;
        days += 1;
    }
    let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let time = |out: &mut String| {
        if s == 0 {
            let _ = write!(out, "{h:02}:{mi:02}");
        } else {
            let _ = write!(out, "{h:02}:{mi:02}:{s:02}");
        }
    };
    out.clear();
    if kind == DateKind::Time {
        if let Some(t) = st
            .code
            .as_deref()
            .and_then(|c| render(c, (1900, 1, 1), (h, mi, s)))
        {
            out.push_str(&t);
        } else {
            time(out);
        }
        return true;
    }
    let date = if date1904 {
        civil(EPOCH_1904 + days)
    } else if days == 60 {
        // несуществующее 1900-02-29 Excel (совместимость с Lotus 1-2-3)
        (1900, 2, 29)
    } else if days < 60 {
        if days == 0 {
            return false;
        }
        civil(EPOCH_1900 + days + 1)
    } else {
        civil(EPOCH_1900 + days)
    };
    if let Some(t) = st.code.as_deref().and_then(|c| render(c, date, (h, mi, s))) {
        out.push_str(&t);
        return true;
    }
    let _ = write!(out, "{:04}-{:02}-{:02}", date.0, date.1, date.2);
    if kind == DateKind::DateTime || (kind == DateKind::Date && secs != 0) {
        out.push(' ');
        time(out);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64, k: DateKind, d1904: bool) -> Option<String> {
        let mut s = String::new();
        let st = Style {
            kind: k,
            code: None,
        };
        format_serial(v, &st, d1904, &mut s).then_some(s)
    }

    fn c(v: f64, code: &str) -> Option<String> {
        let mut s = String::new();
        format_serial(v, &Style::custom(code), false, &mut s).then_some(s)
    }

    /// Свой числовой формат — как в Excel; названия месяцев, `AM/PM` — ISO.
    #[test]
    fn custom_codes_render_like_excel() {
        let v = 44_270.0 + (14.0 * 3600.0 + 30.0 * 60.0 + 5.0) / 86_400.0; // 2021-03-15 14:30:05
        assert_eq!(c(44_270.0, r"dd\.mm\.yyyy").as_deref(), Some("15.03.2021"));
        assert_eq!(c(44_270.0, "d/m/yy").as_deref(), Some("15/3/21"));
        assert_eq!(
            c(v, "yyyy-mm-dd hh:mm").as_deref(),
            Some("2021-03-15 14:30")
        );
        assert_eq!(c(v, "hh:mm:ss").as_deref(), Some("14:30:05"));
        assert_eq!(c(v, "h:mm").as_deref(), Some("14:30"));
        assert_eq!(
            c(44_270.0, r#"dd.mm.yyyy" г.""#).as_deref(),
            Some("15.03.2021 г.")
        );
        assert_eq!(
            c(44_270.0, "[$-419]dd.mm.yyyy").as_deref(),
            Some("15.03.2021")
        );
        assert_eq!(c(44_270.0, "mmm-yy").as_deref(), Some("2021-03-15"));
        assert_eq!(
            c(44_270.0, "dddd, d mmmm yyyy").as_deref(),
            Some("2021-03-15")
        );
        assert_eq!(c(v, "h:mm AM/PM").as_deref(), Some("14:30:05"));
        assert_eq!(c(v, "mm:ss.0").as_deref(), Some("14:30:05"));
        assert_eq!(c(44_270.0, "0.00"), None);
    }

    #[test]
    fn classify_codes() {
        assert_eq!(classify(r"dd\.mm\.yyyy"), DateKind::Date);
        assert_eq!(classify("[$-419]d mmmm yyyy"), DateKind::Date);
        assert_eq!(classify("mmm-yy"), DateKind::Date);
        assert_eq!(classify("yyyy-mm-dd hh:mm"), DateKind::DateTime);
        assert_eq!(classify("h:mm AM/PM"), DateKind::Time);
        assert_eq!(classify("[h]:mm:ss"), DateKind::None);
        assert_eq!(classify(r#"#,##0.00 "руб.""#), DateKind::None);
        assert_eq!(classify(r#"0 "days""#), DateKind::None);
        assert_eq!(classify("0.00%"), DateKind::None);
        assert_eq!(classify("General"), DateKind::None);
        assert_eq!(classify("[Red]0.00;[Blue]-0.00"), DateKind::None);
        assert_eq!(builtin(14), DateKind::Date);
        assert_eq!(builtin(22), DateKind::DateTime);
        assert_eq!(builtin(20), DateKind::Time);
        assert_eq!(builtin(46), DateKind::None);
        assert_eq!(builtin(10), DateKind::None);
    }

    #[test]
    fn serials() {
        assert_eq!(
            f(45231.0, DateKind::Date, false).as_deref(),
            Some("2023-11-01")
        );
        assert_eq!(
            f(45231.5, DateKind::DateTime, false).as_deref(),
            Some("2023-11-01 12:00")
        );
        assert_eq!(
            f(45231.25, DateKind::Date, false).as_deref(),
            Some("2023-11-01 06:00")
        );
        assert_eq!(f(0.75, DateKind::Time, false).as_deref(), Some("18:00"));
        assert_eq!(
            f(0.5 + 1.0 / 86_400.0, DateKind::Time, false).as_deref(),
            Some("12:00:01")
        );
        assert_eq!(
            f(61.0, DateKind::Date, false).as_deref(),
            Some("1900-03-01")
        );
        assert_eq!(
            f(60.0, DateKind::Date, false).as_deref(),
            Some("1900-02-29")
        );
        assert_eq!(
            f(59.0, DateKind::Date, false).as_deref(),
            Some("1900-02-28")
        );
        assert_eq!(f(1.0, DateKind::Date, false).as_deref(), Some("1900-01-01"));
        assert_eq!(f(0.0, DateKind::Date, true).as_deref(), Some("1904-01-01"));
        assert_eq!(
            f(43_830.0, DateKind::Date, true).as_deref(),
            Some("2024-01-01")
        );
        assert_eq!(
            f(2_958_465.0, DateKind::Date, false).as_deref(),
            Some("9999-12-31")
        );
        assert_eq!(f(0.0, DateKind::Date, false), None);
        assert_eq!(f(-1.0, DateKind::Date, false), None);
        assert_eq!(f(3e6, DateKind::Date, false), None);
        assert_eq!(f(45231.0, DateKind::None, false), None);
        // 23:59:59.9 → следующий день, 00:00
        assert_eq!(
            f(45231.0 + 86_399.9 / 86_400.0, DateKind::DateTime, false).as_deref(),
            Some("2023-11-02 00:00")
        );
    }
}
