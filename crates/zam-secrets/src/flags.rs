//! Правила флагов командной строки (задача 3 M7.1, §4.2 п. 5 спецификации M7): значения
//! паролей и токенов в аргументах команд, которых остальные правила сканера не знают.
//!
//! Перенесены из клиента `zam` (`snapshot::mask_flags`, задача 17a M6) без изменения поведения.
//! Применяются двумя путями:
//! - [`mask_flags`] — текст команды целиком (первая строка команды снимка сессии, клиент): в
//!   [`crate::mask_text`] эта функция не входит;
//! - [`code_flag_ranges`] — **только внутри кода** Markdown: строки fenced-блоков (```` ``` ````
//!   и `~~~`, любой язык или без него) и inline-спаны `` `…` `` (круг правок 1 задачи 5 M7.1,
//!   решение координатора). Это правило сканера ([`crate::scan_secrets`], вид
//!   [`crate::SecretKind::CommandFlag`]): чанки, L0, выдача агенту и фокус маскируются одинаково.
//!   Проза вне кода не трогается: «используйте -u для пользователя», «basic», «bearer» в тексте —
//!   не пароль. Ворота `-p` (mysql-подобные, `sshpass`) и `-u user:pass` (HTTP-клиенты) —
//!   по логической строке кода (строки с `\` в конце склеиваются) или по inline-спану.
//!
//! Правило в коде вошло в выпуск 0.0.14-m7.1 вместе с прочими правилами версии
//! [`crate::RULES_VERSION`] 3 (версия не повышалась: выпуск ещё не вышел).

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::SECRET_MARK;

/// Шаблоны значений: `--password X`, `-u user:pass`, `Authorization: X`, `-pXXX`.
static FLAGS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        // --password X, --password=X, --token "a b"
        Regex::new(
            r#"(?i)(?:^|[\s'"])--(?:password|passwd|pass|pwd|passphrase|token|api-key|apikey|api-token|secret|secret-key|client-secret|access-token|auth-token|http-password|proxy-password)(?:=|\s+)("[^"]*"|'[^']*'|[^\s"']+)"#,
        )
        .expect("regex"),
        // curl -u user:pass, --user user:pass, -uuser:pass
        Regex::new(r#"(?:^|\s)(?:-u\s*|--user(?:=|\s+))["']?([^\s:"']+):([^\s"'@]+)"#)
            .expect("regex"),
        // Authorization: Bearer X | Basic X | Token X | X; X-Api-Key: X
        Regex::new(
            r#"(?i)(?:authorization\s*:\s*(?:(?:bearer|basic|token|digest)\s+)?|x-api-key\s*:\s*)([^\s"']{6,})"#,
        )
        .expect("regex"),
        // -pXXX, -p'a b' (значение слитно с флагом)
        Regex::new(r#"(?:^|\s)-p("[^"]*"|'[^']*'|[^\s"']+)"#).expect("regex"),
    ]
});

/// Номер правила `-u user:pass` в [`FLAGS`] (ворота — HTTP-клиенты).
const USER_PASS: usize = 1;
/// Номер правила `-pXXX` в [`FLAGS`] (ворота — mysql-подобные и `sshpass`).
const GLUED_P: usize = 3;

/// Команды, у которых флаги `-p` и `-u` значат пароль: mysql-подобные и `sshpass`; HTTP-клиенты.
static GATES: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i)\b(?:mysql\w*|mariadb\w*|sshpass)\b").expect("regex"),
        Regex::new(r"(?i)\b(?:curl|wget|wget2|http|https|httpie|xh|lftp|aria2c)\b").expect("regex"),
    ]
});

/// Те же ворота с ASCII-границами слова — для буфера кода ([`code_flag_ranges`]): юникодная
/// граница на тексте с кириллицей (комментарии в коде) снимает быстрый DFA, и проход по всему
/// буферу стал бы медленным; на латинских именах команд результат тот же.
static CODE_GATES: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i-u)\b(?:mysql\w*|mariadb\w*|sshpass)\b").expect("regex"),
        Regex::new(r"(?i-u)\b(?:curl|wget|wget2|http|https|httpie|xh|lftp|aria2c)\b")
            .expect("regex"),
    ]
});

/// Значения флагов в `text`: диапазоны байт значений (по правилам, без сортировки). Правило `i`
/// с воротами проверяется, только если `enabled[i]`, и значение берётся, только если
/// `gate(i, совпадение)` (весь диапазон совпадения; ворота — последней, самой дорогой проверкой).
/// Значение — без кавычек; ссылки на переменные, заглушки, схема заголовка без значения и уже
/// замаскированное — не значения.
fn flag_values(
    text: &str,
    enabled: [bool; 4],
    gate: &dyn Fn(usize, Range<usize>) -> bool,
) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for (i, r) in FLAGS.iter().enumerate() {
        if !enabled[i] {
            continue;
        }
        // одни и те же ячейки групп на все совпадения правила: без выделения памяти на каждое
        let mut locs = r.capture_locations();
        let mut from = 0usize;
        while from < text.len() {
            let Some(found) = r.captures_read_at(&mut locs, text, from) else {
                break;
            };
            let whole = found.range();
            // совпадения непустые; на всякий случай — шаг вперёд на символ
            from = if whole.end > from {
                whole.end
            } else {
                from + text[from..].chars().next().map_or(1, char::len_utf8)
            };
            let group = |k: usize| locs.get(k).map(|(s, e)| s..e);
            // `-u user:pass` — значение во второй группе; числовые `uid:gid` — не пароль
            let m = if i == USER_PASS {
                let (Some(u), Some(p)) = (group(1), group(2)) else {
                    continue;
                };
                let digits = |r: &Range<usize>| text[r.clone()].bytes().all(|b| b.is_ascii_digit());
                if digits(&u) && digits(&p) {
                    continue;
                }
                p
            } else {
                let Some(m) = group(1) else { continue };
                m
            };
            let v = &text[m.clone()];
            let (mut b, mut e) = (m.start, m.end);
            if (v.starts_with('"') || v.starts_with('\'')) && v.len() >= 2 {
                b += 1;
                e -= 1;
            }
            let inner = &text[b..e];
            // значение целиком — ссылка на переменную (`$NAME`, `${NAME}`, `%NAME%`) или заглушка
            // `<…>` ([`is_reference`]); у заголовка без значения длиной ≥ 6 регулярка берёт
            // слово-схему — его не трогаем
            let scheme = i == 2
                && ["bearer", "basic", "token", "digest"]
                    .iter()
                    .any(|w| inner.eq_ignore_ascii_case(w));
            // уже замаскированное: сначала дешёвый поиск первого знака метки
            let masked = inner.contains('\u{2039}') && inner.contains(SECRET_MARK);
            if inner.is_empty() || scheme || masked || is_reference(inner) {
                continue;
            }
            if (i == USER_PASS || i == GLUED_P) && !gate(i, whole) {
                continue;
            }
            out.push(b..e);
        }
    }
    out
}

/// Маскирует значения флагов командной строки с паролем или токеном (текст команды; в прозе
/// «basic»/«bearer» и `-u` не пароль — там не применять):
/// - `-pXXX`/`-p'a b'` у mysql-подобных команд и `sshpass`;
/// - `--password X`/`--password=X` (и `--passphrase`, `--secret-key`, `--client-secret`, `--token`,
///   `--api-key`, `--secret`, …);
/// - `-u user:pass`/`--user user:pass` у curl, wget и подобных (числовые `uid:gid` — нет);
/// - значение заголовка `Authorization:` (`Bearer X`, `Basic X`, `Token X`) и `X-Api-Key:`;
///   ссылка на переменную (`Bearer $TOKEN`) и сама схема не маскируются.
///
/// Значение заменяется меткой [`SECRET_MARK`]; ссылки на переменные (`$X`, `%X%`), заглушки
/// (`<…>`) и уже замаскированные значения остаются — повторный вызов ничего не меняет. Ворота
/// `-p` и `-u` — по всему `text` (это одна команда).
pub fn mask_flags(text: &str) -> String {
    let [mysql, curl] = &*GATES;
    let enabled = [true, curl.is_match(text), true, mysql.is_match(text)];
    let mut ranges = flag_values(text, enabled, &|_, _| true);
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for r in ranges {
        if r.start < pos {
            continue;
        }
        out.push_str(&text[pos..r.start]);
        out.push_str(SECRET_MARK);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Значения флагов внутри кода Markdown `text` (см. модуль): диапазоны байт `text`. Участки кода —
/// [`code_segments`]; они собираются в один буфер через `\n`, и каждое правило и каждые ворота
/// проходят его один раз (линейно по объёму кода, без вызова регулярок на каждый спан или
/// совпадение); значение берётся, только если целиком лежит в одном участке, а слово ворот
/// `-p`/`-u` есть в его логической строке (поиск по отсортированным началам слов ворот).
pub(crate) fn code_flag_ranges(text: &str) -> Vec<Range<usize>> {
    // нет ни обратной кавычки, ни тильды — кода нет (обычная проза — без разбора строк)
    if !text.bytes().any(|b| b == b'`' || b == b'~') {
        return Vec::new();
    }
    let segs = code_segments(text);
    if segs.is_empty() {
        return Vec::new();
    }
    // (начало в буфере, начало в тексте, длина)
    let mut map: Vec<(usize, usize, usize)> = Vec::with_capacity(segs.len());
    let mut buf = String::with_capacity(segs.iter().map(|s| s.len() + 1).sum());
    for s in &segs {
        map.push((buf.len(), s.start, s.len()));
        buf.push_str(&text[s.clone()]);
        buf.push('\n');
    }
    let seg_of = |at: usize| map.partition_point(|m| m.0 <= at) - 1;
    // ворота — по логической строке участка вокруг последнего байта совпадения (начало совпадения
    // может быть разделителем `\n` перед участком — `(?:^|\s)` шаблона); проверяются только у
    // кандидатов, прошедших прочие проверки
    let [mysql, curl] = &*CODE_GATES;
    let gate = |i: usize, r: Range<usize>| {
        let mut at = r.end.saturating_sub(1).max(r.start);
        while !buf.is_char_boundary(at) {
            at -= 1;
        }
        let (bs, _, len) = map[seg_of(at)];
        let line = &buf[logical_line(&buf, bs..bs + len, at)];
        if i == GLUED_P {
            mysql.is_match(line)
        } else {
            curl.is_match(line)
        }
    };
    flag_values(&buf, [true; 4], &gate)
        .into_iter()
        .filter_map(|r| {
            let (bs, ts, len) = map[seg_of(r.start)];
            (r.end <= bs + len).then(|| ts + (r.start - bs)..ts + (r.end - bs))
        })
        .collect()
}

/// Логическая строка участка `seg` буфера `buf` вокруг позиции `at` (диапазон в буфере): от
/// начала строки до её конца; строка, которая кончается на `\` (перенос команды), продолжается
/// следующей — и в обе стороны.
fn logical_line(buf: &str, seg: Range<usize>, at: usize) -> Range<usize> {
    let s = &buf[seg.clone()];
    let a = at.saturating_sub(seg.start).min(s.len());
    let continued = |end: usize| s[..end].trim_end_matches([' ', '\t', '\r']).ends_with('\\');
    let mut start = s[..a].rfind('\n').map_or(0, |i| i + 1);
    while start > 0 && continued(start - 1) {
        start = s[..start - 1].rfind('\n').map_or(0, |i| i + 1);
    }
    let mut end = s[a..].find('\n').map_or(s.len(), |i| a + i);
    while end < s.len() && continued(end) {
        end = s[end + 1..].find('\n').map_or(s.len(), |i| end + 1 + i);
    }
    seg.start + start..seg.start + end
}

/// Участки кода Markdown в `text` (диапазоны байт): содержимое fenced-блоков — от строки после
/// открывающей ```` ``` ````/`~~~` (три и больше одинаковых знаков; у ```` ` ```` — без обратных
/// кавычек в строке информации) до закрывающей того же знака не короче открывающей (нет её —
/// до конца текста), и содержимое inline-спанов вне блоков — между двумя сериями обратных
/// кавычек одной длины в одной строке. Отступ перед забором не ограничивается (блоки в списках).
pub(crate) fn code_segments(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    // открытый блок: знак забора, длина серии, начало содержимого
    let mut fence: Option<(u8, usize, usize)> = None;
    let mut pos = 0usize;
    while pos <= text.len() {
        let end = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
        let line = &text[pos..end];
        let t = line.trim_start_matches([' ', '\t']);
        match fence {
            Some((ch, n, start)) => {
                if is_fence_close(t, ch, n) {
                    out.push(start..pos);
                    fence = None;
                }
            }
            None => {
                if let Some((ch, n)) = fence_open(t) {
                    fence = Some((ch, n, (end + 1).min(text.len())));
                } else if line.contains('`') {
                    inline_spans(line, pos, &mut out);
                }
            }
        }
        if end == text.len() {
            break;
        }
        pos = end + 1;
    }
    if let Some((_, _, start)) = fence
        && start < text.len()
    {
        out.push(start..text.len());
    }
    out
}

/// Длина серии знака `ch` в начале `t`.
fn run_of(t: &[u8], ch: u8) -> usize {
    t.iter().take_while(|&&b| b == ch).count()
}

/// Открывающий забор: три и больше ```` ` ```` (без ```` ` ```` дальше в строке) или `~`.
fn fence_open(t: &str) -> Option<(u8, usize)> {
    let b = t.as_bytes();
    let ch = *b.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let n = run_of(b, ch);
    if n < 3 || (ch == b'`' && t[n..].contains('`')) {
        return None;
    }
    Some((ch, n))
}

/// Закрывающий забор блока `(ch, n)`: серия `ch` не короче `n`, дальше — только пробелы.
fn is_fence_close(t: &str, ch: u8, n: usize) -> bool {
    let k = run_of(t.as_bytes(), ch);
    k >= n && t[k..].trim().is_empty()
}

/// Inline-спаны строки `line` (её начало в тексте — `base`): серия из `n` обратных кавычек и
/// следующая серия ровно из `n`; между ними — код. Непарная серия — просто знаки.
fn inline_spans(line: &str, base: usize, out: &mut Vec<Range<usize>>) {
    let b = line.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run_of(&b[i..], b'`');
        let open_end = i + n;
        let mut j = open_end;
        let mut close = None;
        while j < b.len() {
            if b[j] == b'`' {
                let m = run_of(&b[j..], b'`');
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(j) => {
                if j > open_end {
                    out.push(base + open_end..base + j);
                }
                i = j + n;
            }
            None => i = open_end,
        }
    }
}

/// Значение — целиком ссылка на переменную или заглушка, а не секрет: `$NAME`, `${NAME}`,
/// `%NAME%` (имя — `[A-Za-z_][A-Za-z0-9_]*`) или `<…>`. `%2Fabc…`, `$2b$10$…` (хэш bcrypt) —
/// не ссылки.
fn is_reference(v: &str) -> bool {
    let ident = |s: &str| {
        let mut c = s.chars();
        c.next()
            .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
    };
    if let Some(r) = v.strip_prefix("${") {
        return r.strip_suffix('}').is_some_and(ident);
    }
    if let Some(r) = v.strip_prefix('$') {
        return ident(r);
    }
    if let Some(r) = v.strip_prefix('%') {
        return r.strip_suffix('%').is_some_and(ident);
    }
    v.len() >= 2 && v.starts_with('<') && v.ends_with('>')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Участки кода: блоки ```` ``` ````/`~~~` (с языком и без, в списке, незакрытый — до
    /// конца), inline-спаны любой длины серии; забор с ```` ` ```` в строке информации — не
    /// забор; непарная кавычка — не код.
    #[test]
    fn code_segments_cases() {
        let seg = |t: &str| -> Vec<String> {
            code_segments(t)
                .into_iter()
                .map(|r| t[r].to_string())
                .collect()
        };
        assert_eq!(seg("a\n```sh\nx -y\nz\n```\nb"), ["x -y\nz\n"]);
        assert_eq!(seg("~~~\nk\n~~~~\n"), ["k\n"]);
        assert_eq!(seg("- пункт\n  ```\n  y\n  ```\n"), ["  y\n"]);
        assert_eq!(seg("```\nоткрыт\nдо конца"), ["открыт\nдо конца"]);
        assert_eq!(seg("```` ``` внутри ````\n"), [" ``` внутри "]);
        assert_eq!(seg("а `b` и ``c ` d`` и `"), ["b", "c ` d"]);
        // не забор: обратная кавычка в строке информации — строка разбирается как inline
        assert_eq!(seg("```js `x`\nтекст\n"), ["x"]);
        assert!(seg("просто текст без кода").is_empty());
        assert!(seg("одна ` кавычка").is_empty());
    }
}
