//! Сканер секретов перед индексацией (ТЗ §7.6).
//!
//! [`scan_secrets`] находит в тексте значения, похожие на ключи, токены, пароли и закрытые
//! ключи; [`mask_secrets`] заменяет их на [`SECRET_MARK`]. Нарезка заметок и записей строит
//! чанки из замаскированного текста: значения не попадают ни в FTS, ни в эмбеддинги, ни в
//! Qdrant. Исходник заметки не меняется.
//!
//! Принцип правил — «лучше пропустить, чем сломать поиск»: идентификаторы (git SHA, sha256,
//! UUID, base32-id `Id`, длинные URL с query, хэши в логах) не маскируются никогда, поэтому
//! правило высокой энтропии узкое: нужны строчные, заглавные и цифры одновременно, а строки
//! только из hex-символов и дефисов (хэши, UUID) отбрасываются.
//!
//! Диапазоны результата отсортированы, не пересекаются и лежат на границах символов UTF-8.
//! Бюджет — не больше 2 мс на 100 КБ (release).
//!
//! Отдельный крейт с одной зависимостью (`regex`) — с задачи 15b M6: сканер нужен не только
//! серверу (через `zam-markdown`, реэкспорт `zam_markdown::secrets` и прежние имена), но и
//! клиенту `zam` без comrak (маскировка снимка сессии, задача 17a). [`scan_secrets_ext`]
//! дополнительно сообщает число повторов скана и упор в их предел ([`ScanOutcome`]).

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

/// Замена секрета в тексте чанка.
pub const SECRET_MARK: &str = "‹secret›";

/// Тип сработавшего правила.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// Известный префикс ключа (метка — семейство: `aws`, `github`, …).
    KnownPrefix(&'static str),
    /// Блок `-----BEGIN … PRIVATE KEY-----`.
    PrivateKey,
    /// `password = значение`, `api_key: значение` и подобное; маскируется только значение.
    KeyValue,
    /// Пароль в URL: `scheme://user:пароль@host`.
    UrlPassword,
    /// JWT.
    Jwt,
    /// Длинная случайная строка с высокой энтропией.
    HighEntropy,
}

impl SecretKind {
    /// Короткое имя вида для ответа API (без значения секрета).
    pub fn as_str(self) -> &'static str {
        match self {
            SecretKind::KnownPrefix(_) => "known_prefix",
            SecretKind::PrivateKey => "private_key",
            SecretKind::KeyValue => "key_value",
            SecretKind::UrlPassword => "url_password",
            SecretKind::Jwt => "jwt",
            SecretKind::HighEntropy => "high_entropy",
        }
    }
}

/// Найденный секрет: диапазон байтов значения и строка (с 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub kind: SecretKind,
    pub range: Range<usize>,
    pub line: usize,
}

struct Prefix {
    family: &'static str,
    re: Regex,
    /// Тот же шаблон без границы слова, с якорем `^`: ключ вплотную к найденному секрету
    /// ([`glued_prefixes`]).
    glued: Regex,
}

fn rx(p: &str) -> Regex {
    Regex::new(p).expect("шаблон сканера секретов")
}

// ASCII-границы слова `(?-u:\b)`: быстрее юникодных и не срабатывают на кириллице.
static PREFIXES: LazyLock<Vec<Prefix>> = LazyLock::new(|| {
    [
        ("aws", r"(?-u:\b)(?:AKIA|ASIA)[0-9A-Z]{16}"),
        ("github", r"(?-u:\b)gh[pousr]_[A-Za-z0-9]{36}"),
        ("github", r"(?-u:\b)github_pat_[A-Za-z0-9_]{60,}"),
        ("gitlab", r"(?-u:\b)glpat-[A-Za-z0-9_-]{20,}"),
        ("slack", r"(?-u:\b)xox[abprs]-[A-Za-z0-9-]{10,}"),
        ("api", r"(?-u:\b)sk-(?:ant-)?[A-Za-z0-9_-]{20,}"),
        ("google", r"(?-u:\b)AIza[0-9A-Za-z_-]{35}"),
        // ревью задачи 5 M5.6, M8: Hugging Face, npm, SendGrid (ASIA и пароль в URL — выше)
        // ровно 34: иначе жадный повтор съел бы `hf` вплотную следующего ключа
        ("huggingface", r"(?-u:\b)hf_[A-Za-z0-9]{34}"),
        ("npm", r"(?-u:\b)npm_[A-Za-z0-9]{36}"),
        (
            "sendgrid",
            r"(?-u:\b)SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}",
        ),
        // формат zam_core::token: `zam_` + 40 символов base62
        ("zeroaimemory", r"(?-u:\b)zam_[A-Za-z0-9]{40}"),
    ]
    .into_iter()
    .map(|(family, p)| Prefix {
        family,
        re: rx(p),
        glued: rx(&format!(
            "^(?:{})",
            p.strip_prefix(r"(?-u:\b)").unwrap_or(p)
        )),
    })
    .collect()
});

/// Окно проверки ключа внутри конца находки, байт ([`glued_prefixes`]): правило с жадным
/// хвостом (`xox…-[A-Za-z0-9-]{10,}`, `sk-…`, `glpat-…`, JWT) поглощает начало вплотную
/// следующего ключа (`xoxb-…hf` + `_<34>`); его начало — не дальше окна от конца находки
/// (самый длинный литерал префикса — `github_pat_`, заголовок JWT — десятки символов).
const GLUED_INNER_WINDOW: usize = 256;

/// Первые байты шаблонов [`PREFIXES`] и JWT: проверка внутри находки — только с них.
const GLUED_FIRST: &[u8] = b"AghxsSnze";

/// Ключи (префиксные и JWT) с позиции `at` — якорем, без границы слова.
fn anchored_at(text: &str, at: usize, out: &mut Vec<(Range<usize>, SecretKind)>) {
    let rest = &text[at..];
    for p in PREFIXES.iter() {
        if let Some(m) = p.glued.find(rest) {
            out.push((at..at + m.end(), SecretKind::KnownPrefix(p.family)));
        }
    }
    if let Some(m) = JWT_GLUED.find(rest) {
        out.push((at..at + m.end(), SecretKind::Jwt));
    }
}

/// Ключи вплотную к найденным секретам (`SG.<22>.<43>hf_<34>`, `ghp_…ghp_…`, `ghp_…eyJ…`,
/// `xoxb-…hf_…`): правило префикса требует границы слова слева, а её у слипшегося ключа нет —
/// без этого шага второй ключ уходил бы открытым в чанки и индекс, а маскировка не была бы
/// идемпотентной (ревью задачи 9A, M2). Для каждой находки: от её конца шаблоны префиксов и JWT
/// проверяются якорем; у префиксной и JWT — ещё и с позиций последних [`GLUED_INNER_WINDOW`]
/// байт внутри неё (жадный хвост съел начало следующего ключа; берутся ключи, выходящие за
/// конец находки). Найденное — снова в работу. Повторы отсекает множество `(начало, конец)`:
/// цепочка из 50 000 ключей — линейно (ревью повторное 9A).
fn glued_prefixes(text: &str, raw: &mut Vec<(Range<usize>, SecretKind)>) {
    let mut seen: std::collections::HashSet<(usize, usize)> =
        raw.iter().map(|(r, _)| (r.start, r.end)).collect();
    let mut ends: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut work: Vec<(Range<usize>, SecretKind)> = raw.clone();
    let mut found = Vec::new();
    while let Some((h, kind)) = work.pop() {
        found.clear();
        if h.end < text.len() && text.is_char_boundary(h.end) && ends.insert(h.end) {
            anchored_at(text, h.end, &mut found);
        }
        if matches!(kind, SecretKind::KnownPrefix(_) | SecretKind::Jwt) {
            let from = (h.start + 1).max(h.end.saturating_sub(GLUED_INNER_WINDOW));
            let mut inner = Vec::new();
            for pos in from..h.end {
                if GLUED_FIRST.contains(&text.as_bytes()[pos]) && text.is_char_boundary(pos) {
                    anchored_at(text, pos, &mut inner);
                }
            }
            found.extend(inner.into_iter().filter(|(r, _)| r.end > h.end));
        }
        for (r, k) in found.drain(..) {
            if seen.insert((r.start, r.end)) {
                raw.push((r.clone(), k));
                work.push((r, k));
            }
        }
    }
}

/// JWT: заголовок `eyJ…` (base64 от `{"`) и ещё две части, каждая от 10 символов
/// [решение исполнителя: требование `eyJ` отсекает имена вида `a.b.c`].
static JWT: LazyLock<Regex> =
    LazyLock::new(|| rx(r"(?-u:\b)eyJ[A-Za-z0-9_-]{7,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"));

/// JWT без границы слова, с якорем `^` — для [`glued_prefixes`] (`ghp_<36>eyJ…`).
static JWT_GLUED: LazyLock<Regex> =
    LazyLock::new(|| rx(r"^eyJ[A-Za-z0-9_-]{7,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"));

/// Блок закрытого ключа целиком; без `END` — заголовок и следующие строки base64.
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    rx(
        r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----(?:.*?-----END [A-Z ]*PRIVATE KEY-----|(?:\s*[A-Za-z0-9+/=]{20,})*)",
    )
});

/// Ключ-значение; необязательная кавычка перед разделителем покрывает JSON (`"password": "…"`)
/// [решение исполнителя]. Значение — группа 2.
///
/// Значение в кавычках берётся до закрывающей кавычки (пароль-фраза с пробелами); без кавычек —
/// до пробела. Словарь ключей расширен `secret_key`, `signing_key`, `private_key`
/// [решение исполнителя]. Значение — группа 1 (в `"`), 2 (в `'`) или 3 (без кавычек).
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    rx(
        r#"(?i)(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret|secret[_-]?key|signing[_-]?key|private[_-]?key)["']?\s*[:=]\s*(?:"([^"\r\n]{6,})"|'([^'\r\n]{6,})'|([^\s"'`]{6,}))"#,
    )
});

/// Пароль в URL; пароль — группа 1.
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| rx(r"[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@]*:([^\s/@]+)@"));

static ENTROPY_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| rx(r"[A-Za-z0-9+/_=-]{32,}"));

/// Порог энтропии Шеннона, бит на символ.
const ENTROPY_MIN: f64 = 4.5;

/// Значение — заведомая заглушка, а не секрет.
fn is_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    v.starts_with('<')
        || v.starts_with(SECRET_MARK) // уже замаскировано: повторный проход идемпотентен
        || v.starts_with("${")
        || v.starts_with("{{")
        // ссылка на переменную окружения: $TOKEN, $(cmd), %TOKEN%
        || v.starts_with('$')
        || (v.starts_with('%') && v.ends_with('%'))
        || v.bytes().all(|b| b == b'*' || b == b'x' || b == b'X')
        // пути, а не пароли; короткие числа (порты, счётчики, PIN)
        || v.starts_with('/')
        || v.starts_with('~')
        || (v.len() < 12 && v.bytes().all(|b| b.is_ascii_digit()))
        || l == "changeme"
        || l == "example"
}

fn shannon(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

/// Кандидат правила энтропии — настоящий секрет, а не идентификатор.
fn is_high_entropy_secret(s: &str, url_token: &str) -> bool {
    let b = s.as_bytes();
    // хэши, UUID: только hex-символы и дефисы
    if b.iter().all(|c| c.is_ascii_hexdigit() || *c == b'-') {
        return false;
    }
    if !(b.iter().any(u8::is_ascii_lowercase)
        && b.iter().any(u8::is_ascii_uppercase)
        && b.iter().any(u8::is_ascii_digit))
    {
        return false;
    }
    // SRI-хэши (`sha512-…`) — не секреты
    if ["sha256-", "sha384-", "sha512-"]
        .iter()
        .any(|p| s.starts_with(p))
    {
        return false;
    }
    // кандидат внутри URL-токена: пути и query-значения идентификаторами не считаются;
    // секреты в query ловит правило «ключ-значение»
    if url_token.contains("://") {
        return false;
    }
    // путь файловой системы: сегменты-слова, а не случайная строка
    if s.matches('/').count() >= 2 && s.split('/').filter(|p| is_wordlike(p)).count() >= 2 {
        return false;
    }
    shannon(s) >= ENTROPY_MIN
}

/// Сегмент пути из букв, `_` и `-`, без цифр (имя каталога или файла).
fn is_wordlike(p: &str) -> bool {
    !p.is_empty()
        && p.bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'-')
}

/// Граница URL-токена: пробел и `" ' , ) > ] \``.
fn is_url_delim(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b',' | b')' | b'>' | b']' | b'`')
}

/// URL-токен вокруг диапазона: от ближайшей границы слева до ближайшей справа. В однострочном
/// JSON соседний ключ за кавычкой в токен не попадает.
fn url_token_around<'a>(text: &'a str, r: &Range<usize>) -> &'a str {
    let b = text.as_bytes();
    let mut s = r.start;
    while s > 0 && !is_url_delim(b[s - 1]) {
        s -= 1;
    }
    let mut e = r.end;
    while e < b.len() && !is_url_delim(b[e]) {
        e += 1;
    }
    // границы — ASCII-байты, не бывающие продолжением многобайтового символа
    text.get(s..e).unwrap_or("")
}

/// Конец маски, которая покроет позицию `at`: цепочка пересекающихся диапазонов (как при их
/// объединении в [`merge_ranges`]) — находок энтропии текущего кандидата `local` и находок
/// остальных правил `others` (объединены, по возрастанию: из них цепочку продолжает только
/// последний с началом не правее конца — двоичный поиск).
fn masked_span_end(
    local: &[(Range<usize>, SecretKind)],
    others: &[Range<usize>],
    at: usize,
) -> usize {
    let joins = |h: &Range<usize>, end: usize| {
        h.start <= end && (h.start <= at || h.start < end) && h.end > end
    };
    let mut end = at;
    let mut grown = true;
    while grown {
        grown = false;
        for (h, _) in local {
            if joins(h, end) {
                end = h.end;
                grown = true;
            }
        }
        let i = others.partition_point(|h| h.start <= end);
        if i > 0 && joins(&others[i - 1], end) {
            end = others[i - 1].end;
            grown = true;
        }
    }
    end
}

/// Позиция `at` внутри одной из находок `others` (объединены, по возрастанию).
fn covered_by(others: &[Range<usize>], at: usize) -> bool {
    let i = others.partition_point(|h| h.start <= at);
    i > 0 && at < others[i - 1].end
}

/// URL-токен вокруг `r`, если текст от `r.end` до `masked_end` заменён маской: левая часть — как
/// в [`url_token_around`], правая продолжается за маской до границы.
fn url_token_masked(text: &str, r: &Range<usize>, masked_end: usize) -> String {
    let b = text.as_bytes();
    let mut s = r.start;
    while s > 0 && !is_url_delim(b[s - 1]) {
        s -= 1;
    }
    let mut e = masked_end;
    while e < b.len() && !is_url_delim(b[e]) {
        e += 1;
    }
    let mut out = String::new();
    out.push_str(text.get(s..r.end).unwrap_or(""));
    out.push_str(SECRET_MARK);
    out.push_str(text.get(masked_end..e).unwrap_or(""));
    out
}

/// Предел повторных сканов вида с масками в [`scan_secrets`]: каждый скан линеен, их число —
/// константа. На практике хватает одного-двух (маска одного правила открывает находку другого).
pub const FIXPOINT_MAX: u32 = 4;

/// Итог скана [`scan_secrets_ext`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Находки — те же, что у [`scan_secrets`].
    pub hits: Vec<SecretHit>,
    /// Повторных сканов по виду с масками ([`MaskedView`]), от 0 до [`FIXPOINT_MAX`]: 0 —
    /// находок нет или вид из одних масок; 1 — первый повтор уже ничего не нашёл (обычный
    /// текст с секретами).
    pub passes: u32,
    /// Скан упёрся в [`FIXPOINT_MAX`]: все повторы давали находки, и ещё один повтор по виду
    /// находит новые. Неподвижная точка не достигнута — [`mask_text`] на таком тексте не
    /// идемпотентна (повторная маскировка заменит ещё что-то). Проверочный повтор в находки не
    /// идёт: они те же, что у [`scan_secrets`].
    pub depth_capped: bool,
}

/// Находит секреты. Результат отсортирован по началу, диапазоны не пересекаются
/// (пересёкшиеся объединяются, вид берётся у первого), `line` считается с 1.
///
/// Скан — до неподвижной точки (полный прогон перед слиянием M5.6, `mask_is_idempotent`):
/// после первого прохода по тексту все правила повторяются по [`MaskedView`] — тексту, каким его
/// увидит повторная маскировка. Маска одного правила меняет контекст другого: убирает `://` из
/// URL-токена кандидата энтропии (`Ahf_<34>‹secret›password=…postgres://…`), `/` и `"` из имени
/// пользователя в URL (`s://x,Ab/<40>:пароль@h`), — и находка, которую иначе открыл бы только
/// второй проход, маскируется сразу. Находки первого прохода остаются: вид их не отменяет
/// (энтропия по тексту видит длинного кандидата целиком, по виду — обрывки между масками).
/// Скан останавливается, когда вид не даёт находок, — тогда `mask_text` идемпотентна; не дольше
/// [`FIXPOINT_MAX`] повторов (упор — [`scan_secrets_ext`]).
pub fn scan_secrets(text: &str) -> Vec<SecretHit> {
    scan_secrets_ext(text).hits
}

/// [`scan_secrets`] с числом повторов скана и признаком упора в [`FIXPOINT_MAX`]
/// ([`ScanOutcome`]). Упор стоит одного лишнего скана — только на таком тексте.
pub fn scan_secrets_ext(text: &str) -> ScanOutcome {
    let mut raw = scan_rules(text);
    let mut passes = 0u32;
    let mut depth_capped = false;
    // повторы `1..=FIXPOINT_MAX` дают находки; повтор `FIXPOINT_MAX + 1` — только проверка упора
    while !raw.is_empty() {
        let view = MaskedView::new(text, &raw);
        // вид из одних масок находок не даст (ни одно правило не совпадает внутри `‹secret›`):
        // цепочка из десятков тысяч ключей не сканируется второй раз
        if view.all_masked() {
            break;
        }
        let found = scan_rules(&view.text);
        if passes == FIXPOINT_MAX {
            depth_capped = !found.is_empty();
            break;
        }
        passes += 1;
        if found.is_empty() {
            break;
        }
        raw.extend(
            found
                .into_iter()
                .map(|(r, k)| (view.start_in_text(r.start)..view.end_in_text(r.end), k)),
        );
    }
    let merged = merge_ranges(raw);
    // номера строк одним проходом
    let (mut line, mut pos) = (1usize, 0usize);
    let hits = merged
        .into_iter()
        .map(|(range, kind)| {
            line += text.as_bytes()[pos..range.start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            pos = range.start;
            SecretHit { kind, range, line }
        })
        .collect();
    ScanOutcome {
        hits,
        passes,
        depth_capped,
    }
}

/// Все правила по `text` (один проход, без объединения).
fn scan_rules(text: &str) -> Vec<(Range<usize>, SecretKind)> {
    let mut raw: Vec<(Range<usize>, SecretKind)> = Vec::new();
    for p in PREFIXES.iter() {
        for m in p.re.find_iter(text) {
            raw.push((m.range(), SecretKind::KnownPrefix(p.family)));
        }
    }
    for m in JWT.find_iter(text) {
        raw.push((m.range(), SecretKind::Jwt));
    }
    for m in PRIVATE_KEY.find_iter(text) {
        raw.push((m.range(), SecretKind::PrivateKey));
    }
    for c in KEY_VALUE.captures_iter(text) {
        if let Some(v) = c.get(1).or_else(|| c.get(2)).or_else(|| c.get(3))
            && !is_placeholder(v.as_str())
        {
            raw.push((v.range(), SecretKind::KeyValue));
        }
    }
    for c in URL_PASSWORD.captures_iter(text) {
        if let Some(v) = c.get(1)
            && !is_placeholder(v.as_str())
        {
            raw.push((v.range(), SecretKind::UrlPassword));
        }
    }
    glued_prefixes(text, &mut raw);
    let others: Vec<Range<usize>> = merge_ranges(raw.clone())
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    raw.extend(
        entropy_hits(text, &others)
            .into_iter()
            .map(|r| (r, SecretKind::HighEntropy)),
    );
    raw
}

/// Объединение находок: по началу (при равном — длинная первой), пересёкшиеся — в один
/// диапазон с видом первого, смежные остаются раздельными (две маски подряд).
fn merge_ranges(mut raw: Vec<(Range<usize>, SecretKind)>) -> Vec<(Range<usize>, SecretKind)> {
    raw.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut merged: Vec<(Range<usize>, SecretKind)> = Vec::with_capacity(raw.len());
    for (r, k) in raw {
        match merged.last_mut() {
            Some((last, _)) if r.start < last.end => last.end = last.end.max(r.end),
            _ => merged.push((r, k)),
        }
    }
    merged
}

/// Текст, каким его увидит повторный проход маскировки: находки заменены [`SECRET_MARK`]
/// (объединение — [`merge_ranges`], как в [`mask_secrets`] над результатом [`scan_secrets`]).
struct MaskedView {
    text: String,
    /// Куски без маски: (начало в виде, начало в тексте), по возрастанию; за куском `i` —
    /// маска `marks[i]` (последний кусок — без маски).
    segs: Vec<(usize, usize)>,
    /// Диапазоны текста под масками.
    marks: Vec<Range<usize>>,
    /// Длина исходного текста.
    src_len: usize,
}

impl MaskedView {
    fn new(text: &str, raw: &[(Range<usize>, SecretKind)]) -> Self {
        let marks: Vec<Range<usize>> = merge_ranges(raw.to_vec())
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        let mut out = String::with_capacity(text.len());
        let mut segs = vec![(0, 0)];
        let mut pos = 0;
        for r in &marks {
            out.push_str(&text[pos..r.start]);
            out.push_str(SECRET_MARK);
            pos = r.end;
            segs.push((out.len(), pos));
        }
        out.push_str(&text[pos..]);
        MaskedView {
            text: out,
            segs,
            marks,
            src_len: text.len(),
        }
    }

    /// Вне масок нет ни байта.
    fn all_masked(&self) -> bool {
        self.text.len() == self.marks.len() * SECRET_MARK.len()
    }

    /// Кусок, в котором лежит позиция вида `at`, и смещение от его начала.
    fn locate(&self, at: usize) -> (usize, usize) {
        debug_assert!(at <= self.text.len(), "позиция вне вида: {at}");
        let i = self.segs.partition_point(|s| s.0 <= at) - 1;
        (i, at - self.segs[i].0)
    }

    /// Начало находки вида в тексте: внутри маски — начало замаскированного диапазона.
    fn start_in_text(&self, at: usize) -> usize {
        let (i, off) = self.locate(at);
        let t = self.segs[i].1;
        let r = match self.marks.get(i) {
            Some(m) if t + off > m.start => m.start,
            _ => t + off,
        };
        debug_assert!(r <= self.src_len, "начало вне текста: {r}");
        r
    }

    /// Конец находки вида в тексте: внутри маски — конец замаскированного диапазона.
    fn end_in_text(&self, at: usize) -> usize {
        let (i, off) = self.locate(at);
        let t = self.segs[i].1;
        let r = match self.marks.get(i) {
            Some(m) if t + off > m.start => m.end,
            _ => t + off,
        };
        debug_assert!(r <= self.src_len, "конец вне текста: {r}");
        r
    }
}

/// Находки правила энтропии в `text`; `others` — объединённые находки остальных правил того же
/// текста (по возрастанию): имя перед замаскированным ими значением проверяется сразу.
fn entropy_hits(text: &str, others: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    // находки текущего кандидата: находки прежних кончаются до его начала (кандидаты не
    // пересекаются, идут слева направо) и на проверки ниже не влияют — перебор только их
    // держит цепочку `<имя>=<значение> …` линейной (ревью b854b22, M1)
    let mut raw: Vec<(Range<usize>, SecretKind)> = Vec::new();
    for m in ENTROPY_CANDIDATE.find_iter(text) {
        out.extend(raw.drain(..).map(|(r, _)| r));
        let start = m.start();
        let mut end = m.end();
        // конец замаскированного значения справа от проверяемого имени (см. ниже)
        let mut masked_end: Option<usize> = None;
        loop {
            // `имя=значение`: `=` внутри строки — разделитель, а не base64-дополнение (оно в
            // конце); проверяется значение
            let inner = text[start..end].trim_end_matches('=').rfind('=');
            let r = inner.map_or(start..end, |i| start + i + 1..end);
            let token = match masked_end {
                Some(me) => url_token_masked(text, &r, me),
                None => url_token_around(text, &r).to_string(),
            };
            if r.len() >= 32 && is_high_entropy_secret(&text[r.clone()], &token) {
                raw.push((r.clone(), SecretKind::HighEntropy));
            }
            // значение замаскировано (этим или другим правилом): повторный проход увидит
            // `имя=` перед маской отдельным кандидатом — он проверяется сразу, иначе маскировка
            // не идемпотентна. Плата: длинное (от 32 символов) имя с высокой энтропией и всеми
            // классами символов маскируется вместе с `=` уже в первом проходе (раньше — во
            // втором); `UPPER_SNAKE_CASE` и CamelCase без цифр не маскируются (тест
            // `long_name_left_of_masked_value`)
            match inner {
                Some(_)
                    if covered_by(others, r.start)
                        || raw
                            .iter()
                            .any(|(h, _)| h.start <= r.start && r.start < h.end) =>
                {
                    // окрестность имени — как во втором проходе, где значение уже заменено
                    // маской: иначе `://` замаскированного значения скрывает имя только в первом
                    // проходе (CI 41605984321, `mask_is_idempotent`)
                    masked_end = Some(masked_span_end(&raw, others, r.start));
                    end = r.start;
                }
                _ => break,
            }
        }
    }
    out.extend(raw.into_iter().map(|(r, _)| r));
    out
}

/// Заменяет диапазоны `hits` на [`SECRET_MARK`]; остальные байты не меняются.
/// Некорректные диапазоны (не по порядку, за пределами, не на границах символов)
/// пропускаются.
pub fn mask_secrets(text: &str, hits: &[SecretHit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for h in hits {
        let r = &h.range;
        if r.start < pos
            || r.end < r.start
            || r.end > text.len()
            || !text.is_char_boundary(r.start)
            || !text.is_char_boundary(r.end)
        {
            continue;
        }
        out.push_str(&text[pos..r.start]);
        out.push_str(SECRET_MARK);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Сканирует и маскирует за один вызов (текст без секретов возвращается копией).
pub fn mask_text(text: &str) -> String {
    let hits = scan_secrets(text);
    if hits.is_empty() {
        text.to_string()
    } else {
        mask_secrets(text, &hits)
    }
}
