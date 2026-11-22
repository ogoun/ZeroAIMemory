//! Правила флагов командной строки (задача 3 M7.1, §4.2 п. 5 спецификации M7): значения
//! паролей и токенов в аргументах команд, которых остальные правила сканера не знают.
//!
//! Перенесены из клиента `zam` (`snapshot::mask_flags`, задача 17a M6). Применяются двумя путями:
//! - [`mask_flags`] — текст команды целиком (первая строка команды снимка сессии, клиент): в
//!   [`crate::mask_text`] эта функция не входит;
//! - [`code_flag_ranges`] — **только внутри кода** Markdown ([`crate::code::code_regions`]: строки
//!   блоков в ограждениях, в том числе в цитатах, и инлайн-код, в том числе многострочный; круг
//!   правок 1–2 задачи 5 M7.1, решение координатора). Это правило сканера
//!   ([`crate::scan_secrets`], вид [`crate::SecretKind::CommandFlag`]): чанки, L0, выдача агенту и
//!   фокус маскируются одинаково. Проза вне кода не трогается: «используйте -u для
//!   пользователя», «basic», «bearer» в тексте — не пароль. Ворота `-p` (mysql-подобные,
//!   `sshpass`) и `-u user:pass` (HTTP-клиенты) — по логической строке блока (строки с `\` в
//!   конце склеиваются) или по спану инлайн-кода целиком.
//!
//! Значение: без кавычек; обратная кавычка в значение не входит (`` `mysql -pX` ``), закрывающая
//! скобка без пары — тоже (`$(mysql -pX)`); ссылки на переменные (`$X`, `${X}`, `%X%`), заглушки
//! `<…>` и заглушки из заглавных букв и `_` (`TOKEN`, `PASS_WORD`, от трёх знаков) — не
//! секреты; уже замаскированное — остаётся. Заголовок `Authorization`/`X-Api-Key` узнаётся и в
//! JSON и YAML (`"Authorization": "Bearer …"`, `Authorization: "Bearer …"`).
//!
//! Время линейно по длине текста (круг правок 2: прежние ворота строили логическую строку и
//! проходили её регуляркой на каждого кандидата — квадратично, 38 с на строке в 1 МБ): флаги с
//! дефисом — один проход по знакам `-` без регулярок ([`dash_flags`]), заголовок — один поиск
//! регулярки; начала логических единиц собираются один раз, единица кандидата — двоичным
//! поиском, регулярка ворот проходит каждую единицу не больше раза на правило.
//!
//! Правило в коде вошло в выпуск 0.0.14-m7.1 вместе с прочими правилами версии
//! [`crate::RULES_VERSION`] 3 (версия не повышалась: выпуск ещё не вышел).

use std::cell::Cell;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::SECRET_MARK;
use crate::code::{CodeKind, CodeRegion, mask_code_region_sets};

/// Правила флагов (номер — в [`flag_values`], ворота — у `-u` и `-p`):
/// - [`LONG`] — `--password X`, `--password=X`, `--token "a b"` (имена — [`LONG_NAMES`]);
/// - [`USER_PASS`] — `-u user:pass`, `--user user:pass`, `-uuser:pass` (ворота — HTTP-клиенты);
/// - [`HEADER`] — `Authorization: Bearer X | Basic X | Token X | X`, `X-Api-Key: X`, в том числе
///   JSON и YAML ([`HEADER_RE`]);
/// - [`GLUED_P`] — `-pXXX`, `-p'a b'` (значение слитно с флагом; ворота — mysql-подобные и
///   `sshpass`).
///
/// Флаги с дефисом ищутся одним проходом по знакам `-` ([`dash_flags`]) — по смыслу это шаблоны
/// `(?:^|[\s'"])--(?:имена)(?:=|\s+)(?:"[^"]*"|'[^']*'|[^\s"'`]+)`,
/// `(?:^|\s)(?:-u\s*|--user(?:=|\s+))["']?[^\s:"'`]+:[^\s"'@`]+` и
/// `(?:^|\s)-p(?:"[^"]*"|'[^']*'|[^\s"'`]+)` (поиск регулярки на каждое из тысяч совпадений
/// подряд стоил ~100 нс, разбор — единицы).
const LONG: usize = 0;
const USER_PASS: usize = 1;
const HEADER: usize = 2;
const GLUED_P: usize = 3;

/// Имена длинных флагов с паролем или токеном (строчными; сравнение без учёта регистра).
const LONG_NAMES: &[&str] = &[
    "password",
    "passwd",
    "pass",
    "pwd",
    "passphrase",
    "token",
    "api-key",
    "apikey",
    "api-token",
    "secret",
    "secret-key",
    "client-secret",
    "access-token",
    "auth-token",
    "http-password",
    "proxy-password",
];

/// Заголовок: значение — в конце совпадения, его выделяет [`header_value`].
static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:authorization|x-api-key)["']?\s*:\s*["']?(?:(?:bearer|basic|token|digest)\s+)?[^\s"'`]{6,}"#,
    )
    .expect("regex")
});

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

/// Позиция после пробельных символов с `p` (как `\s` регулярок: Unicode White_Space).
fn skip_ws(m: &str, p: usize) -> usize {
    p + m[p..]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(m.len() - p, |(i, _)| i)
}

/// Позиция после одной кавычки (`"` или `'`) с `p`, если она там есть.
fn skip_quote(m: &str, p: usize) -> usize {
    p + usize::from(m[p..].starts_with(['"', '\'']))
}

/// Значение в совпадении заголовка `m` ([`HEADER_RE`], диапазон внутри `m`): после `:`, кавычки
/// и схемы (`Bearer`) — после последнего пробела.
fn header_value(m: &str) -> Option<Range<usize>> {
    let kw = if m
        .as_bytes()
        .get(..13)
        .is_some_and(|k| k.eq_ignore_ascii_case(b"authorization"))
    {
        13
    } else {
        "x-api-key".len()
    };
    let mut p = skip_ws(m, skip_quote(m, kw));
    p = m[p..].starts_with(':').then_some(p + 1)?;
    p = skip_quote(m, skip_ws(m, p));
    let start = m[p..]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(p, |(j, c)| p + j + c.len_utf8());
    Some(start..m.len())
}

/// Флаг с дефисом: правило, всё совпадение, значение (в кавычках — с ними), у `-u` — имя.
struct DashFlag {
    rule: usize,
    /// Начало самого флага (первый `-`).
    flag: usize,
    whole: Range<usize>,
    value: Range<usize>,
    user: Option<Range<usize>>,
}

/// Начало совпадения перед позицией `i` по `(?:^|\s)` (или `(?:^|[\s'"])` при `quotes`): начало
/// текста или начало предшествующего пробельного знака (кавычки).
fn lead_at(text: &str, i: usize, quotes: bool) -> Option<usize> {
    if i == 0 {
        return Some(0);
    }
    let c = text[..i].chars().next_back()?;
    (c.is_whitespace() || (quotes && matches!(c, '"' | '\''))).then(|| i - c.len_utf8())
}

/// Длина серии знаков с `p`, для которых `keep` истинно.
fn run_len(text: &str, p: usize, keep: impl Fn(char) -> bool) -> usize {
    let rest = &text[p..];
    rest.char_indices()
        .find(|(_, c)| !keep(*c))
        .map_or(rest.len(), |(j, _)| j)
}

/// Конец значения `(?:"[^"]*"|'[^']*'|[^\s"'`]+)` с позиции `p`.
fn value_end(text: &str, p: usize) -> Option<usize> {
    match *text.as_bytes().get(p)? {
        q @ (b'"' | b'\'') => text[p + 1..].find(q as char).map(|j| p + 1 + j + 1),
        _ => {
            let n = run_len(text, p, |c| {
                !c.is_whitespace() && !matches!(c, '"' | '\'' | '`')
            });
            (n > 0).then_some(p + n)
        }
    }
}

/// Флаги с дефисом в `text` одним проходом по знакам `-` (шаблоны — у [`LONG`]), слева направо.
/// `f` решает, взято ли значение: взятое совпадение закрывает собой следующие того же правила,
/// начатые внутри него (они — внутри маскируемого значения), а отвергнутое — нет (круг правок 3
/// задачи 5 M7.1: иначе отвергнутое совпадение прятало соседнее, а маска другого правила его
/// убирала — и повторная маскировка находила новое).
fn dash_flags(text: &str, enabled: [bool; 4], f: &mut dyn FnMut(DashFlag) -> bool) {
    let b = text.as_bytes();
    let mut last_end = [0usize; 4];
    let mut emit = |d: DashFlag, last_end: &mut [usize; 4]| {
        // знак перед флагом (пробел, кавычка) может быть концом прежнего значения: закрывает
        // только сам флаг, начатый внутри взятого совпадения
        let (rule, flag, end) = (d.rule, d.flag, d.whole.end);
        if flag >= last_end[rule] && f(d) {
            last_end[rule] = end;
        }
    };
    let mut i = 0;
    while let Some(j) = b[i..].iter().position(|&c| c == b'-') {
        let at = i + j;
        i = at + 1;
        match b.get(at + 1) {
            Some(b'p') if enabled[GLUED_P] => {
                if let Some(start) = lead_at(text, at, false)
                    && let Some(end) = value_end(text, at + 2)
                {
                    let d = DashFlag {
                        rule: GLUED_P,
                        flag: at,
                        whole: start..end,
                        value: at + 2..end,
                        user: None,
                    };
                    emit(d, &mut last_end);
                }
            }
            Some(b'u') if enabled[USER_PASS] => {
                // `--user` — с первого дефиса; `-u` — с этого
                let (flag, after) = if at >= 1 && b[at - 1..].starts_with(b"--user") {
                    let p = at - 1 + "--user".len();
                    let after = if b.get(p) == Some(&b'=') {
                        Some(p + 1)
                    } else {
                        let w = skip_ws(text, p);
                        (w > p).then_some(w)
                    };
                    (at - 1, after)
                } else {
                    (at, Some(skip_ws(text, at + 2)))
                };
                let Some(start) = lead_at(text, flag, false) else {
                    continue;
                };
                let Some(p) = after.map(|p| skip_quote(text, p)) else {
                    continue;
                };
                let un = run_len(text, p, |c| {
                    !c.is_whitespace() && !matches!(c, ':' | '"' | '\'' | '`')
                });
                if un == 0 || b.get(p + un) != Some(&b':') {
                    continue;
                }
                let q = p + un + 1;
                let pn = run_len(text, q, |c| {
                    !c.is_whitespace() && !matches!(c, '"' | '\'' | '@' | '`')
                });
                if pn == 0 {
                    continue;
                }
                let d = DashFlag {
                    rule: USER_PASS,
                    flag,
                    whole: start..q + pn,
                    value: q..q + pn,
                    user: Some(p..p + un),
                };
                emit(d, &mut last_end);
            }
            Some(b'-') if enabled[LONG] => {
                let Some(start) = lead_at(text, at, true) else {
                    continue;
                };
                let name = at + 2;
                let nn = text[name..]
                    .bytes()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == b'-')
                    .count();
                let n = &text[name..name + nn];
                if !LONG_NAMES.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                    continue;
                }
                let p = name + nn;
                let p = if b.get(p) == Some(&b'=') {
                    p + 1
                } else {
                    let w = skip_ws(text, p);
                    if w == p {
                        continue;
                    }
                    w
                };
                if let Some(end) = value_end(text, p) {
                    let d = DashFlag {
                        rule: LONG,
                        flag: at,
                        whole: start..end,
                        value: p..end,
                        user: None,
                    };
                    emit(d, &mut last_end);
                }
            }
            _ => {}
        }
    }
}

/// Значение — заглушка из заглавных латинских букв и `_` (`TOKEN`, `PASSWORD`, `PASS_WORD`), от
/// трёх до [`CAPS_PLACEHOLDER_MAX`] знаков, хотя бы одна буква: так пишут место для значения, а не
/// само значение; длинная строка заглавных (`XKQZJWPRMVNBTLHGFDSAYUEIOC`) — уже похожа на ключ.
fn is_caps_placeholder(v: &str) -> bool {
    (3..=CAPS_PLACEHOLDER_MAX).contains(&v.len())
        && v.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        && v.bytes().any(|b| b.is_ascii_uppercase())
}

/// Наибольшая длина заглушки [`is_caps_placeholder`] (круг правок 3 задачи 5 M7.1).
const CAPS_PLACEHOLDER_MAX: usize = 24;

/// Значения флагов в `text`: диапазоны байт значений (по правилам, без сортировки). Правило `i`
/// с воротами проверяется, только если `enabled[i]`, и значение берётся, только если
/// `gate(i, совпадение)` (весь диапазон совпадения; ворота — последней проверкой). Значение — без
/// кавычек и без закрывающей скобки без пары; ссылки на переменные, заглушки, схема заголовка без
/// значения, числовые `uid:gid` и уже замаскированное — не значения.
fn flag_values(
    text: &str,
    enabled: [bool; 4],
    gate: &dyn Fn(usize, Range<usize>) -> bool,
) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    // `true` — значение взято
    let mut take = |rule: usize, whole: Range<usize>, value: Range<usize>, user: Option<&str>| {
        let (mut b, mut e) = (value.start, value.end);
        // `-u user:pass` — числовые `uid:gid` — не пароль
        if let Some(u) = user {
            let digits = |s: &str| s.bytes().all(|x| x.is_ascii_digit());
            if digits(u) && digits(&text[b..e]) {
                return false;
            }
        }
        let raw = &text[b..e];
        let quoted = (raw.starts_with('"') || raw.starts_with('\'')) && raw.len() >= 2;
        if quoted {
            b += 1;
            e -= 1;
        } else if raw.ends_with(')') {
            // закрывающие скобки без пары — конец подстановки `$(…)`, не значение (у значения в
            // кавычках скобка — его часть); один подсчёт, снятие — с уменьшением счётчика
            let open = raw.bytes().filter(|&c| c == b'(').count();
            let mut close = raw.bytes().filter(|&c| c == b')').count();
            while open < close && e > b && text.as_bytes()[e - 1] == b')' {
                e -= 1;
                close -= 1;
            }
        }
        let inner = &text[b..e];
        // у заголовка без значения длиной ≥ 6 шаблон берёт слово-схему — его не трогаем
        let scheme = rule == HEADER
            && ["bearer", "basic", "token", "digest"]
                .iter()
                .any(|w| inner.eq_ignore_ascii_case(w));
        // уже замаскированное (и меткой с номером `‹secret-N›`): сначала дешёвый поиск первого
        // знака метки
        let masked = inner.contains('\u{2039}') && crate::has_any_mark(inner);
        if inner.is_empty() || scheme || masked || is_reference(inner) || is_caps_placeholder(inner)
        {
            return false;
        }
        if (rule == USER_PASS || rule == GLUED_P) && !gate(rule, whole) {
            return false;
        }
        out.push(b..e);
        true
    };
    dash_flags(text, enabled, &mut |d| {
        let user = d.user.map(|u| &text[u]);
        take(d.rule, d.whole, d.value, user)
    });
    if enabled[HEADER] {
        // отвергнутое совпадение заголовка следующих не прячет (см. [`dash_flags`]): поиск — с
        // позиции за его началом, а если в значении метка — сразу за последней меткой (все
        // заголовки до неё отвергнуты бы по той же метке; так поиск остаётся линейным)
        let mut from = 0;
        while let Some(m) = HEADER_RE.find_at(text, from) {
            let v = header_value(m.as_str()).map(|v| m.start() + v.start..m.start() + v.end);
            let taken = v.clone().is_some_and(|v| take(HEADER, m.range(), v, None));
            from = if taken {
                m.end()
            } else {
                let next = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
                v.and_then(|v| {
                    crate::marks(&text[v.clone()])
                        .last()
                        .map(|(m, _)| v.start + m.end)
                })
                .map_or(next, |after_mark| after_mark.max(next))
            };
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
/// (`<…>`, `TOKEN`) и уже замаскированные значения остаются — повторный вызов ничего не меняет
/// (отвергнутое совпадение соседнее не прячет, пересёкшиеся значения правил маскируются одной
/// меткой; proptest `mask_flags_idempotent`, круг правок 3 задачи 5 M7.1). Ворота `-p` и `-u` —
/// по всему `text` (это одна команда).
pub fn mask_flags(text: &str) -> String {
    let [mysql, curl] = &*GATES;
    let enabled = [true, curl.is_match(text), true, mysql.is_match(text)];
    let mut ranges = flag_values(text, enabled, &|_, _| true);
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    // пересёкшиеся значения разных правил — одной меткой: иначе хвост второго остался бы открытым
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start < last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for r in merged {
        out.push_str(&text[pos..r.start]);
        out.push_str(SECRET_MARK);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Значения флагов внутри кода Markdown `text` (см. модуль): диапазоны байт `text`.
///
/// Код для маски — [`crate::code::mask_code_regions`], объединение участков по CommonMark, по
/// правилам круга 2 и широкого правила (круг правок 5 задачи 5 M7.1). Правило проходит наборы
/// ([`mask_code_region_sets`]) **по отдельности** и объединяет значения: ворота и знак перед
/// флагом каждого набора — по его собственным участкам, так что маска — надмножество маски
/// каждого (слитый участок мог бы поставить перед флагом обратную кавычку соседнего и потерять
/// значение). Совпадают наборы (текст без списков) — проход один.
pub(crate) fn code_flag_ranges(text: &str) -> Vec<Range<usize>> {
    // нет ни обратной кавычки, ни тильды — кода нет (обычная проза — без разбора строк)
    if !text.bytes().any(|b| b == b'`' || b == b'~') {
        return Vec::new();
    }
    mask_code_region_sets(text)
        .iter()
        .flat_map(|set| flags_in_regions(text, set))
        .collect()
}

/// Значения флагов в участках кода `regions` (по порядку, без пересечений).
///
/// Участки собираются в один буфер через `\n`: каждое правило проходит его одним поиском.
/// Значение берётся, только если совпадение целиком лежит в одном участке, а для `-p`/`-u` в его
/// логической единице есть слово ворот. Логические единицы — строки блока (строка, кончающаяся
/// `\`, продолжается следующей) и спан инлайн-кода целиком; их начала собираются один раз,
/// единица кандидата — двоичным поиском, регулярка ворот проходит каждую единицу не больше раза
/// на правило (ответ запоминается: кандидаты идут слева направо).
fn flags_in_regions(text: &str, regions: &[CodeRegion]) -> Vec<Range<usize>> {
    if regions.is_empty() {
        return Vec::new();
    }
    // (начало в буфере, начало в тексте, длина) и начала логических единиц в буфере
    let mut map: Vec<(usize, usize, usize)> = Vec::with_capacity(regions.len());
    let mut units: Vec<usize> = Vec::with_capacity(regions.len());
    let mut buf = String::with_capacity(regions.iter().map(|r| r.range.len() + 1).sum());
    for r in regions {
        let s = &text[r.range.clone()];
        let base = buf.len();
        map.push((base, r.range.start, s.len()));
        units.push(base);
        if r.kind == CodeKind::Fenced {
            line_units(s, base, &mut units);
        }
        buf.push_str(s);
        buf.push('\n');
    }
    let region_of = |at: usize| map.partition_point(|m| m.0 <= at) - 1;
    let unit_of = |at: usize| {
        let k = units.partition_point(|&u| u <= at) - 1;
        units[k]..units.get(k + 1).copied().unwrap_or(buf.len())
    };
    // ворота — по единице последнего байта совпадения (начало совпадения может быть
    // разделителем `\n` перед участком — `(?:^|\s)` шаблона): регулярка ворот проходит единицу
    // один раз — ответ запоминается до следующей единицы (кандидаты идут слева направо), так что
    // всего проходится не больше буфера на правило, сколько бы кандидатов ни было
    let [mysql, curl] = &*CODE_GATES;
    let cache: [Cell<(usize, bool)>; 2] = [
        Cell::new((usize::MAX, false)),
        Cell::new((usize::MAX, false)),
    ];
    let gate = |i: usize, r: Range<usize>| {
        let mut at = r.end.saturating_sub(1).max(r.start);
        while !buf.is_char_boundary(at) {
            at -= 1;
        }
        let unit = unit_of(at);
        let (k, re) = if i == GLUED_P { (0, mysql) } else { (1, curl) };
        let (start, open) = cache[k].get();
        if start == unit.start {
            return open;
        }
        let open = re.is_match(&buf[unit.clone()]);
        cache[k].set((unit.start, open));
        open
    };
    flag_values(&buf, [true; 4], &gate)
        .into_iter()
        .filter_map(|r| {
            let (bs, ts, len) = map[region_of(r.start)];
            (r.start >= bs && r.end <= bs + len).then(|| ts + (r.start - bs)..ts + (r.end - bs))
        })
        .collect()
}

/// Начала логических строк блока `s` (в буфере — со смещения `base`) после первой: новая строка
/// начинается за переводом строки (`\n`, `\r\n`, одиночный `\r`), если предыдущая не кончается
/// `\` (перенос команды; пробелы и табуляции после него не мешают).
fn line_units(s: &str, base: usize, units: &mut Vec<usize>) {
    let b = s.as_bytes();
    let mut line_start = 0;
    let mut i = 0;
    while i < b.len() {
        let eol = match b[i] {
            b'\n' => 1,
            b'\r' if b.get(i + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => {
                i += 1;
                continue;
            }
        };
        let continued = s[line_start..i]
            .trim_end_matches([' ', '\t'])
            .ends_with('\\');
        i += eol;
        if !continued {
            units.push(base + i);
        }
        line_start = i;
    }
}

/// Значение — целиком ссылка на переменную или заглушка, а не секрет: `$NAME`, `${NAME}`,
/// `%NAME%` (имя — `[A-Za-z_][A-Za-z0-9_]*`) или `<…>` без `:` внутри (`<TOKEN>`, `<api-key>`;
/// с двоеточием — уже не заглушка, а в значении заголовка — ещё и соседний заголовок, круг правок
/// 3 задачи 5 M7.1). `%2Fabc…`, `$2b$10$…` (хэш bcrypt) — не ссылки.
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
    v.len() >= 2 && v.starts_with('<') && v.ends_with('>') && !v.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Флаги с дефисом: (правило, совпадение, значение, имя) — как у прежних шаблонов
    /// (`(?:^|\s)` и `(?:^|[\s'"])` перед флагом, `--user=`/`--user ` и `-u`/`-uX`, имена длинных
    /// флагов без учёта регистра, значение в кавычках — с ними, без обратной кавычки), совпадения
    /// одного правила не пересекаются.
    fn dash(text: &str) -> Vec<(usize, String, String, Option<String>)> {
        let mut out = Vec::new();
        dash_flags(text, [true; 4], &mut |d| {
            out.push((
                d.rule,
                text[d.whole].to_string(),
                text[d.value].to_string(),
                d.user.map(|u| text[u].to_string()),
            ));
            true
        });
        out
    }

    #[test]
    fn dash_flags_cases() {
        let one = |t: &str| {
            let v = dash(t);
            assert_eq!(v.len(), 1, "{t:?}: {v:?}");
            (v[0].0, v[0].2.clone(), v[0].3.clone())
        };
        assert_eq!(one(" --password=abc"), (LONG, "abc".into(), None));
        assert_eq!(one("--TOKEN  \"a b\""), (LONG, "\"a b\"".into(), None));
        assert_eq!(one("'--api-key=k1"), (LONG, "k1".into(), None));
        assert_eq!(
            one(" -u admin:pw"),
            (USER_PASS, "pw".into(), Some("admin".into()))
        );
        assert_eq!(
            one(" --user='a:b"),
            (USER_PASS, "b".into(), Some("a".into()))
        );
        assert_eq!(
            one(" --user x:y"),
            (USER_PASS, "y".into(), Some("x".into()))
        );
        assert_eq!(
            one("-uroot:x"),
            (USER_PASS, "x".into(), Some("root".into()))
        );
        assert_eq!(one("\u{a0}-pS3cret"), (GLUED_P, "S3cret".into(), None));
        assert_eq!(one(" -p'a b'"), (GLUED_P, "'a b'".into(), None));
        assert_eq!(one("`mysql -pX`"), (GLUED_P, "X".into(), None));
        for t in [
            "x-pS3cret",
            "--password-file=x",
            "--username=a:b",
            "--pass",
            " -p",
            " -p\"open",
            " -uroot",
            "ё-u a:b",
        ] {
            assert!(dash(t).is_empty(), "{t:?}: {:?}", dash(t));
        }
        assert_eq!(dash(" -pa -pb -u x:y").len(), 3);
    }

    /// Значение заголовка — после `:`, кавычки и схемы; JSON и YAML.
    #[test]
    fn header_value_cases() {
        let v = |m: &str| header_value(m).map(|r| m[r].to_string());
        assert_eq!(v("Authorization: Bearer abcdef").as_deref(), Some("abcdef"));
        assert_eq!(v("authorization:abcdef").as_deref(), Some("abcdef"));
        assert_eq!(
            v("Authorization\": \"Bearer abcdef").as_deref(),
            Some("abcdef")
        );
        assert_eq!(v("X-Api-Key: k1k2k3").as_deref(), Some("k1k2k3"));
    }

    /// Логические единицы блока: перенос `\` (в том числе с CRLF и одиночным `\r`) склеивает
    /// строки.
    #[test]
    fn line_units_continuation() {
        let mut u = vec![0];
        // «a \» + CRLF склеена с «b»; «c» + одиночный `\r`; «d \» склеена с «e»
        line_units("a \\\r\nb\nc\rd \\\ne", 0, &mut u);
        assert_eq!(u, [0, 7, 9]);
    }
}
