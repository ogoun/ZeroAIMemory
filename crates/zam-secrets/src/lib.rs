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
//!
//! [`mask_text`] идемпотентна для любого текста (задача 3 M7.1): скан идёт до неподвижной
//! точки, а упор в [`FIXPOINT_MAX`] расширяет неустойчивые находки, пока вид не перестанет
//! меняться ([`ScanOutcome::depth_capped`]). Правила флагов командной строки — отдельно,
//! [`mask_flags`] (только текст команд).

mod flags;

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

pub use flags::mask_flags;

/// Замена секрета в тексте чанка.
pub const SECRET_MARK: &str = "‹secret›";

/// Версия правил сканера. Увеличивается при каждой правке правил, после которой сканер находит
/// больше: хранилище (`zam-store`, ключи `meta` `secrets_scanned_v2` и `secrets_scanned_v3`)
/// записывает версию, с которой база перемаскирована, и при открытии базы с меньшей версией один
/// раз перенарезает заметки и записи, где теперь есть находки, и проверяет теги payload и папки.
/// 1 — правила M3–M5.7 (значение `'1'` старых баз); 2 — значение в обратных кавычках (задача 15b
/// M6, I1); 3 — русские слова-ключи (значение и тогда, когда оно только из hex), `response`
/// заголовка `Authorization: Digest`, идемпотентность на цепочках «пароль в URL ↔ ключ-значение»
/// (задача 3 M7.1).
pub const RULES_VERSION: u32 = 3;

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

/// Значение в кавычках правил «ключ-значение»: в `"`, в `'` или в обратных кавычках — до
/// закрывающей кавычки (пароль-фраза с пробелами), от 6 символов.
///
/// Задача 3 M7.1 — значение устойчиво к маскам ([`MaskedView`]), чтобы по виду с масками правило
/// не находило того, чего не видело в тексте (иначе находки открывались по одной за повтор, и
/// цепочка повторов не кончалась — `tests/fixtures/scan_depth_capped.txt`):
/// - значение не заходит в метку `‹`: иначе короткий остаток склеивался с маской другого правила
///   (`pwd=:‹secret›@…` — в тексте `:` упирался в `` ` ``); остаток до метки — начало значения,
///   которое правило уже видело в тексте целиком;
/// - значение не начинается с метки и с признаков заглушки — пути и ссылки (`/`, `~`), переменной
///   (`$`), шаблона (`<`, `{`): такое совпадение раньше отвергалось проверкой
///   [`is_placeholder`] уже после поиска и поглощало текст до конца значения вместе с ключами
///   внутри него, а по виду (значение начинается с метки — совпадения нет) эти ключи
///   открывались. Теперь совпадения нет ни в тексте, ни в виде, и вложенный ключ виден сразу
///   (`pwd=/путь,password=…`).
const KV_QUOTED: &str = r#""([^"\r\n‹/~$<{][^"\r\n‹]{5,})"|'([^'\r\n‹/~$<{][^'\r\n‹]{5,})'|`([^`\r\n‹/~$<{][^`\r\n‹]{5,})`"#;

/// Значение без кавычек — до пробела, кавычки или метки `‹`, от 6 символов; первый символ — не
/// признак заглушки ([`KV_QUOTED`]).
const KV_BARE: &str = r#"([^\s"'`‹/~$<{][^\s"'`‹]{5,})"#;

/// Ключ-значение; необязательная кавычка перед разделителем покрывает JSON (`"password": "…"`)
/// [решение исполнителя].
///
/// Значение в кавычках берётся до закрывающей кавычки (пароль-фраза с пробелами); без кавычек —
/// до пробела ([`KV_QUOTED`], [`KV_BARE`]). Словарь ключей расширен `secret_key`, `signing_key`,
/// `private_key` [решение исполнителя]. Значение — группа 1 (в `"`), 2 (в `'`), 3 (в обратных
/// кавычках, `` password=`…` ``, ревью задачи 15b M6, I1) или 4 (без кавычек). Обычный inline
/// code без ключа (`` `cargo build` ``) правило не задевает. Значение только из hex (64 hex —
/// типичный токен) — тоже значение: исключение hex есть только у правила энтропии.
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    rx(&format!(
        r#"(?i)(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|client[_-]?secret|secret[_-]?key|signing[_-]?key|private[_-]?key)["']?\s*[:=]\s*(?:{KV_QUOTED}|{KV_BARE})"#
    ))
});

/// Русские слова-ключи (задача 3 M7.1, `i:01M7Z9C3FAE8ARBCX00WSZJ6SD`): `токен`, `пароль`,
/// `секрет`, `ключ API`, `api-ключ` и падежные формы, после которых естественно стоит значение, —
/// `токена`, `токеном`, `пароля`, `паролем`, `секрета` **[решение координатора]**. Слово «ключ»
/// само по себе — не ключ: «ключ идемпотентности: abc123» не маскируется.
///
/// Разделитель — `:` или `=`, как у [`KEY_VALUE`] (значение — группы 1–4), но значение — на той
/// же строке: «нашёлся токен:» в конце строки вводит блок ниже, и следующая строка
/// (`API_TOKEN=ghp_…`) — не значение этого ключа **[решение исполнителя, задача 3 M7.1]**. У форм
/// «с паролем», «токеном» значение стоит и без разделителя, но только в кавычках (группы 5–7:
/// «войти с паролем `…`»); без кавычек это проза («с паролем администратора»). Граница слова
/// справа — сам разделитель («токенов: 1500» не совпадает), слева — по Unicode: перед словом начало
/// текста или символ не буква и не цифра («автотокен: …» — не ключ).
///
/// Левая граница — часть шаблона (символ перед словом входит в совпадение), а не проверка после
/// него: отвергнутое проверкой совпадение всё равно поглощало бы текст до конца значения вместе
/// с ключами внутри него, а по виду с масками (значение короче — до метки) эти ключи
/// открывались бы, и цепочка повторов росла. Конец метки `›` — тоже не граница: ключ вплотную за
/// замаскированным значением (`<ключ>токен: …`) в тексте границы не имел, и вид с масками
/// ([`MaskedView`]) не должен её получать (задача 3 M7.1, находки генератора
/// `generate_depth_capped`).
static KEY_VALUE_RU: LazyLock<Regex> = LazyLock::new(|| {
    rx(&format!(
        r#"(?i)(?:^|[^\p{{L}}\p{{N}}›])(?:(?:токен(?:а|ом)?|парол(?:ь|я|ем)|секрет(?:а)?|ключ[ \t]+api|api[-_ ]?ключ)["']?[ \t]*[:=][ \t]*(?:{KV_QUOTED}|{KV_BARE})|(?:токеном|паролем)[ \t]+(?:{KV_QUOTED}))"#
    ))
});

/// Значение без кавычек после русского слова-ключа — слово прозы, а не секрет: только буквы
/// (и дефис), есть кириллица, знаки препинания в конце не в счёт («Пароль: хранится в Vault»,
/// «Токен: устарел.») [решение исполнителя, задача 3 M7.1]. Значение в кавычках — всегда значение.
fn ru_prose_word(v: &str) -> bool {
    let w = v.trim_end_matches(|c: char| c.is_ascii_punctuation() || c == '…');
    !w.is_empty()
        && w.chars().all(|c| c.is_alphabetic() || c == '-')
        && w.chars()
            .any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

/// Заголовок `Authorization: Digest …` (и `Proxy-Authorization:`, в том числе в JSON с `\"`) —
/// от имени до конца строки (задача 3 M7.1). Параметры в нём — [`DIGEST_RESPONSE`].
static DIGEST_HEADER: LazyLock<Regex> =
    LazyLock::new(|| rx(r#"(?i)authorization["']?\s*:\s*["']?\s*digest\s[^\r\n]*"#));

/// Параметр `response` заголовка Digest: значение — группа 1 (в кавычках) или 2 (без кавычек).
/// `username`, `realm`, `nonce`, `cnonce`, `opaque`, `uri` — не секреты и не маскируются. Все
/// `response` заголовка берутся за один проход: по виду с масками первое значение — метка, и
/// поиск «первого `response`» перешёл бы к следующему, по одному за повтор. Перед `response` —
/// пробел или запятая (входят в совпадение): граница слова `\b` появилась бы и после метки
/// (`‹secret›response=`), то есть только по виду с масками.
static DIGEST_RESPONSE: LazyLock<Regex> = LazyLock::new(|| {
    rx(
        r#"(?i)[\s,]response\s*=\s*(?:\\?"([^"\\\r\n‹$<][^"\\\r\n‹]{5,})|([^\s,"\\‹$<][^\s,"\\‹]{5,}))"#,
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

/// Предел обычных повторных сканов вида с масками в [`scan_secrets`]: каждый скан линеен, их
/// число — константа. На практике хватает одного-двух (маска одного правила открывает находку
/// другого). Повторы сверх предела расширяют находки (три ступени `WIDEN_STEPS`: до пробелов,
/// строки, всего текста).
pub const FIXPOINT_MAX: u32 = 4;

/// Ступени расширения находок после [`FIXPOINT_MAX`] (задача 3 M7.1): находка повтора
/// `FIXPOINT_MAX + 1` маскируется до пробелов вокруг, `+ 2` — вся строка, `+ 3` — весь текст
/// ([`widen`]). Расширенная маска захватывает и звенья, которые открыл бы следующий повтор:
/// цепочка находок кончается за постоянное число сканов, а весь текст под одной маской новых
/// находок не даёт — неподвижная точка достигается всегда, и [`mask_text`] идемпотентна.
const WIDEN_STEPS: u32 = 3;

/// Итог скана [`scan_secrets_ext`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Находки — те же, что у [`scan_secrets`].
    pub hits: Vec<SecretHit>,
    /// Повторных сканов по виду с масками ([`MaskedView`]), от 0 до `FIXPOINT_MAX +
    /// WIDEN_STEPS`: 0 — находок нет или вид из одних масок; 1 — первый повтор уже ничего не
    /// нашёл (обычный текст с секретами). Последний повтор находок не дал (или вид — из одних
    /// масок): скан дошёл до неподвижной точки.
    pub passes: u32,
    /// Скан упёрся в [`FIXPOINT_MAX`]: обычные повторы не сошлись, и находки следующих повторов
    /// расширены (до пробелов, строки или всего текста). Маскировка такого текста грубее
    /// (вместе с секретом скрыт и соседний текст), но [`mask_text`] по-прежнему идемпотентна
    /// (до задачи 3 M7.1 — не была). Предупреждение `secrets_scan_depth` сервера и клиента.
    pub depth_capped: bool,
}

/// Находит секреты. Результат отсортирован по началу, диапазоны не пересекаются
/// (пересёкшиеся объединяются, вид берётся у первого), `line` считается с 1.
///
/// Скан — до неподвижной точки (полный прогон перед слиянием M5.6, `mask_text_idempotent`):
/// после первого прохода по тексту все правила повторяются по [`MaskedView`] — тексту, каким его
/// увидит повторная маскировка. Маска одного правила меняет контекст другого: убирает `://` из
/// URL-токена кандидата энтропии (`Ahf_<34>‹secret›password=…postgres://…`), `/` и `"` из имени
/// пользователя в URL (`s://x,Ab/<40>:пароль@h`), — и находка, которую иначе открыл бы только
/// второй проход, маскируется сразу. Находки первого прохода остаются: вид их не отменяет
/// (энтропия по тексту видит длинного кандидата целиком, по виду — обрывки между масками).
/// Скан останавливается, когда вид не даёт находок (находка ровно на метке вида его не меняет),
/// — тогда `mask_text` идемпотентна. Сверх [`FIXPOINT_MAX`] повторов находки расширяются
/// (`WIDEN_STEPS`), поэтому неподвижная точка достигается всегда, не дольше чем за
/// `FIXPOINT_MAX + WIDEN_STEPS` повторов (упор — [`scan_secrets_ext`]).
pub fn scan_secrets(text: &str) -> Vec<SecretHit> {
    scan_secrets_ext(text).hits
}

/// [`scan_secrets`] с числом повторов скана и признаком упора в [`FIXPOINT_MAX`]
/// ([`ScanOutcome`]).
pub fn scan_secrets_ext(text: &str) -> ScanOutcome {
    scan_to_fixpoint(text, FIXPOINT_MAX)
}

/// Скан до неподвижной точки: обычных повторов — не больше `limit`, дальше — расширение находок
/// ([`WIDEN_STEPS`]). Предел — параметр ради юнит-тестов ступеней расширения: обычные входы в
/// [`FIXPOINT_MAX`] укладываются.
fn scan_to_fixpoint(text: &str, limit: u32) -> ScanOutcome {
    let mut raw = scan_rules(text);
    let mut passes = 0u32;
    let mut depth_capped = false;
    while !raw.is_empty() {
        let view = MaskedView::new(text, &raw);
        // вид из одних масок находок не даст (ни одно правило не совпадает внутри `‹secret›`):
        // цепочка из десятков тысяч ключей не сканируется второй раз
        if view.all_masked() {
            break;
        }
        let mut found = scan_rules(&view.text);
        // находка ровно на метке вида его не меняет: повторная маскировка заменит метку меткой
        found.retain(|(r, _)| view.text.get(r.clone()) != Some(SECRET_MARK));
        passes += 1;
        if found.is_empty() {
            break;
        }
        // упор: обычные повторы не сошлись — находки расширяются, ступень за ступенью
        let step = passes.saturating_sub(limit).min(WIDEN_STEPS);
        depth_capped |= step > 0;
        raw.extend(found.into_iter().map(|(r, k)| {
            let r = widen(&view.text, r, step);
            (view.start_in_text(r.start)..view.end_in_text(r.end), k)
        }));
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
        if let Some(v) = (1..=4).find_map(|i| c.get(i))
            && !is_placeholder(v.as_str())
        {
            raw.push((v.range(), SecretKind::KeyValue));
        }
    }
    for c in KEY_VALUE_RU.captures_iter(text) {
        let Some((i, v)) = (1..=7).find_map(|i| c.get(i).map(|v| (i, v))) else {
            continue;
        };
        if !is_placeholder(v.as_str()) && !(i == 4 && ru_prose_word(v.as_str())) {
            raw.push((v.range(), SecretKind::KeyValue));
        }
    }
    for h in DIGEST_HEADER.find_iter(text) {
        for c in DIGEST_RESPONSE.captures_iter(h.as_str()) {
            if let Some(v) = (1..=2).find_map(|i| c.get(i))
                && !is_placeholder(v.as_str())
            {
                raw.push((
                    h.start() + v.start()..h.start() + v.end(),
                    SecretKind::KeyValue,
                ));
            }
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

/// Находка `r` в `text`, расширенная по ступени `step` ([`WIDEN_STEPS`]): 0 — как есть; 1 — до
/// пробельных символов по обе стороны; 2 — до границ строки; 3 и больше — весь текст. Границы —
/// на границах символов UTF-8.
fn widen(text: &str, r: Range<usize>, step: u32) -> Range<usize> {
    let (head, tail) = (&text[..r.start], &text[r.end..]);
    let after = |i: usize| i + head[i..].chars().next().map_or(0, char::len_utf8);
    match step {
        0 => r,
        1 => {
            let s = head.rfind(char::is_whitespace).map_or(0, after);
            let e = tail
                .find(char::is_whitespace)
                .map_or(text.len(), |i| r.end + i);
            s..e
        }
        2 => {
            let s = head.rfind('\n').map_or(0, |i| i + 1);
            let e = tail.find('\n').map_or(text.len(), |i| r.end + i);
            s..e
        }
        _ => 0..text.len(),
    }
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
                    // проходе (CI 41605984321, proptest `mask_text_idempotent`)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Псевдослучайная строка base62 (как `rnd` тестов крейта): секретов в исходнике нет.
    fn rnd(len: usize, seed: u64) -> String {
        const A: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let mut x = seed.wrapping_mul(6364136223846793005) | 1;
        (0..len)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                A[((x >> 33) % 62) as usize] as char
            })
            .collect()
    }

    /// Ступени расширения: до пробельных символов (в том числе не ASCII), до строки, весь текст.
    #[test]
    fn widen_steps() {
        let t = "до\u{a0}ab cd\u{2003}ef gh\nij kl";
        let at = t.find('d').unwrap();
        let r = at..at + 1;
        assert_eq!(widen(t, r.clone(), 0), r);
        assert_eq!(&t[widen(t, r.clone(), 1)], "cd");
        assert_eq!(&t[widen(t, r.clone(), 2)], "до\u{a0}ab cd\u{2003}ef gh");
        assert_eq!(widen(t, r.clone(), 3), 0..t.len());
        assert_eq!(widen(t, r, 7), 0..t.len());
        // находка с краю текста
        assert_eq!(widen("abc", 0..1, 1), 0..3);
        assert_eq!(widen("abc", 2..3, 2), 0..3);
    }

    /// Задача 3 M7.1: упор в предел не ломает идемпотентность — при любом пределе обычных повторов
    /// (здесь занижен, чтобы ступени расширения сработали на настоящих каскадах) скан доходит до
    /// неподвижной точки не дольше чем за `предел + WIDEN_STEPS` повторов, маскировка его
    /// находками повторно ничего не меняет, значения скрыты.
    #[test]
    fn widening_reaches_fixpoint_at_any_limit() {
        let (r40, r20) = (rnd(40, 1), rnd(20, 2));
        // каскады: маска энтропии открывает пароль в URL; цепочка «пароль в URL ↔ ключ-значение»
        // фикстуры `scan_depth_capped.txt`; то же в строке среди обычного текста
        let chain = format!("x://pwd=:`/{r40}@pwd=://{r20}\"pwd=:`@");
        let cases = [
            (format!("до s://x,Ab/{r40}:hunter22@h после"), "hunter22"),
            (chain.clone(), r40.as_str()),
            (
                format!("первая строка\nтекст {chain} ещё\nпоследняя"),
                r20.as_str(),
            ),
        ];
        for limit in 0..=FIXPOINT_MAX {
            for (t, secret) in &cases {
                let o = scan_to_fixpoint(t, limit);
                assert!(o.passes <= limit + WIDEN_STEPS, "{limit}: {} {t}", o.passes);
                let once = mask_secrets(t, &o.hits);
                assert!(!once.contains(secret), "{limit}: {once}");
                let again = scan_to_fixpoint(&once, limit);
                assert_eq!(mask_secrets(&once, &again.hits), once, "{limit}: {t}");
                assert!(!again.depth_capped, "{limit}: {once}");
            }
        }
        // предел 0: каскад сразу расширяется — до пробелов, соседние слова остаются
        let (t, _) = &cases[0];
        let o = scan_to_fixpoint(t, 0);
        assert!(o.depth_capped);
        assert_eq!(mask_secrets(t, &o.hits), format!("до {SECRET_MARK} после"));
        let (t, _) = &cases[2];
        let o = scan_to_fixpoint(t, 0);
        let once = mask_secrets(t, &o.hits);
        assert!(once.starts_with("первая строка\nтекст ") && once.ends_with(" ещё\nпоследняя"));
        // обычный предел: цепочка фикстуры сходится без упора
        assert!(!scan_to_fixpoint(&chain, FIXPOINT_MAX).depth_capped);
    }
}
