//! Проактивное вспоминание `POST /recall` (§10.3) — чистые функции без ввода-вывода:
//! нормализация ошибки инструмента ([`normalize_error`]), сопоставление `error_regex`
//! ([`error_regex_matches`]), покрытие слов ошибки найденным текстом ([`error_terms`],
//! [`term_coverage`], [`coverage_enough`] по правилу [`CoverageRule`]: слова как есть или
//! основы [`StemWord`] — общий код с подсказками-идеями сервера, M6 §9; с M7.1 §4.6 п. 3 —
//! по предметным словам ошибки: [`strip_exit_header`], [`subject_error_terms`],
//! [`is_error_code`], [`is_common`], [`CoverageQuery::subject`]), предметные слова
//! запроса пользователя ([`subject_terms`], M6.6 §3.1), строка подсказки ([`hint_line`]) и
//! упаковка дайджеста в бюджет токенов ([`pack_sections`]).
//!
//! Сервер собирает кандидатов (SQLite, поиск), здесь — только то, что от данных не зависит.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zam_core::text::is_stop_word;

/// Предел нормализованной ошибки, символов (§10.3).
pub const ERROR_MAX_CHARS: usize = 512;
/// Начало каждой подсказки (§10.3, «Пометка»).
pub const HINT_PREFIX: &str = "ZeroAIMemory:";
/// Предел скомпилированного `error_regex` (как при проверке триггера в `zam-store`), байт.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Слов ошибки, по которым считается покрытие (как предел слов запроса FTS).
const MAX_TERMS: usize = 32;
/// Минимальная длина слова ошибки для покрытия, символов (как префиксный поиск FTS).
const MIN_TERM_CHARS: usize = 3;
/// Длина «хэша»: шестнадцатеричный токен с цифрой от стольких символов (короткий sha git — 7).
const HASH_MIN_CHARS: usize = 7;

/// Обрамление токена, которое снимается перед классификацией.
fn is_wrap(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '«' | '»'
    )
}

/// Токен — путь или URL: есть `/` или `\` (кроме одиночного символа).
fn is_path(core: &str) -> bool {
    core.chars().count() > 1 && core.contains(['/', '\\'])
}

/// Токен — UUID (`8-4-4-4-12` шестнадцатеричных цифр).
fn is_uuid(core: &str) -> bool {
    let parts: Vec<&str> = core.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Токен — хэш или длинное шестнадцатеричное число: `0x…`, либо только hex-цифры (в том числе
/// через `-`/`:`) от [`HASH_MIN_CHARS`] знаков с хотя бы одной цифрой.
fn is_hex(core: &str) -> bool {
    let t = core.trim_end_matches([':', '.']);
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit());
    }
    let hex: Vec<char> = t.chars().filter(|c| !matches!(c, '-' | ':')).collect();
    hex.len() >= HASH_MIN_CHARS
        && hex.iter().all(char::is_ascii_hexdigit)
        && hex.iter().any(char::is_ascii_digit)
}

/// Числовые хвосты позиции (`:12:5`, `:12:5:`) с конца токена; хвоста нет — токен как есть.
fn strip_numeric_tails(tok: &str) -> &str {
    let mut t = tok;
    let mut stripped = false;
    loop {
        let base = t.trim_end_matches(':');
        match base.rfind(':') {
            Some(i)
                if i > 0
                    && base.len() > i + 1
                    && base[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                t = &base[..i];
                stripped = true;
            }
            _ => break,
        }
    }
    if stripped { t } else { tok }
}

/// Нормализованная ошибка инструмента (§10.3) для FTS по insight: убраны пути и URL, UUID,
/// хэши и длинные шестнадцатеричные числа (`0x…`), числа (токены без единой буквы) и числовые
/// хвосты позиции (`a.rs:12:5` → `a.rs`); токены с буквой остаются целиком (`E0425`,
/// `LNK1181`, `TS2307`, `sha256`, `x86_64` — по ним и ищется ошибка); пробелы схлопнуты,
/// переводы строк — пробелы. Не длиннее [`ERROR_MAX_CHARS`] символов.
pub fn normalize_error(s: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    for tok in s.split_whitespace() {
        let core = tok.trim_matches(is_wrap);
        if is_path(core) || is_uuid(core) || is_hex(core) {
            continue;
        }
        if !core.chars().any(char::is_alphabetic) {
            continue;
        }
        let kept = strip_numeric_tails(tok);
        let n = kept.chars().count();
        let sep = usize::from(!out.is_empty());
        if chars + sep + n > ERROR_MAX_CHARS {
            let room = ERROR_MAX_CHARS.saturating_sub(chars + sep);
            if room > 0 {
                if sep == 1 {
                    out.push(' ');
                }
                out.extend(kept.chars().take(room));
            }
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(kept);
        chars += sep + n;
    }
    out
}

/// Предел кэша скомпилированных `error_regex`; при переполнении кэш сбрасывается целиком.
const REGEX_CACHE_MAX: usize = 1024;

/// Кэш скомпилированных `error_regex` по тексту шаблона (`None` — шаблон не компилируется).
fn regex_cache() -> &'static Mutex<HashMap<String, Option<regex::Regex>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<regex::Regex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `error_regex` триггера совпал с текстом ошибки. Шаблон компилируется с тем же пределом
/// размера, что при проверке триггера, один раз на процесс (кэш по тексту шаблона, до
/// [`REGEX_CACHE_MAX`]); некорректный шаблон не совпадает ни с чем.
pub fn error_regex_matches(pattern: &str, text: &str) -> bool {
    let re = {
        let mut cache = regex_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = cache.get(pattern) {
            r.clone()
        } else {
            let r = regex::RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
                .ok();
            if cache.len() >= REGEX_CACHE_MAX {
                cache.clear();
            }
            cache.insert(pattern.to_string(), r.clone());
            r
        }
    };
    // сопоставление — вне блокировки (`Regex` дешево клонируется)
    re.is_some_and(|r| r.is_match(text))
}

/// Слова нормализованной ошибки для покрытия: буквенно-цифровые от [`MIN_TERM_CHARS`]
/// символов, в нижнем регистре, без повторов, не больше 32 (как в запросе FTS).
pub fn error_terms(normalized: &str) -> Vec<String> {
    terms_of(normalized.split(|c: char| !c.is_alphanumeric()))
}

/// Слова для покрытия из последовательности слов: от [`MIN_TERM_CHARS`] символов, в нижнем
/// регистре, без повторов, не больше [`MAX_TERMS`].
fn terms_of<'a>(words: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in words.filter(|w| w.chars().count() >= MIN_TERM_CHARS) {
        let w = w.to_lowercase();
        if !out.contains(&w) {
            out.push(w);
            if out.len() == MAX_TERMS {
                break;
            }
        }
    }
    out
}

/// Первая строка ошибки `Exit code <N>` (её ставит Claude Code перед выводом команды в
/// `PostToolUseFailure.error`) снимается: служебная строка агента, не текст ошибки (M7.1 §4.6
/// п. 3). Строка — целиком, без учёта регистра и краевых пробелов; иначе текст как есть.
pub fn strip_exit_header(s: &str) -> &str {
    let (first, rest) = s.split_once('\n').unwrap_or((s, ""));
    let head = first.trim();
    let code = head
        .get(..10)
        .filter(|h| h.eq_ignore_ascii_case("exit code "))
        .map(|_| head[10..].trim_start_matches('-'));
    match code {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => rest,
        _ => s,
    }
}

/// Служебные фразы вывода (M7.1 §4.6 п. 3): сообщения ОС «нет такого файла или пути» (POSIX
/// `ENOENT` в английской и русской локали, Windows — тоже в обеих), а не предмет ошибки — их слова
/// в покрытие не идут ([`subject_error_terms`]). Сравнение — целыми словами после [`norm_word`]
/// (регистр, `ё` → `е`: «Не удаётся» = «Не удается»).
pub const SERVICE_PHRASES: [&str; 6] = [
    "no such file or directory",
    "нет такого файла или каталога",
    "the system cannot find the path specified",
    "the system cannot find the file specified",
    "не удается найти указанный путь",
    "не удается найти указанный файл",
];

/// Слова нормализованной ошибки для покрытия по предметным словам (M7.1 §4.6 п. 3): как
/// [`error_terms`], но без вхождений [`SERVICE_PHRASES`].
pub fn subject_error_terms(normalized: &str) -> Vec<String> {
    let words: Vec<&str> = normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let normed: Vec<String> = words.iter().map(|w| norm_word(w)).collect();
    let phrases: Vec<Vec<&str>> = SERVICE_PHRASES
        .iter()
        .map(|p| p.split(' ').collect())
        .collect();
    let mut keep: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let hit = phrases.iter().find(|p| {
            words.len() - i >= p.len() && p.iter().zip(&normed[i..]).all(|(a, b)| b == a)
        });
        match hit {
            Some(p) => i += p.len(),
            None => {
                keep.push(words[i]);
                i += 1;
            }
        }
    }
    terms_of(keep.into_iter())
}

/// Код ошибки (M7.1 §4.6 п. 3): 1–4 латинские буквы и 4–6 цифр — `lnk1104`, `e0432`,
/// `ts2307`, `cs0246` (слово ошибки уже в нижнем регистре). Такое слово не бывает частым, и
/// одного его совпадения целиком достаточно ([`CoverageQuery::subject`]); `sha256`, `x86`, `e2e`
/// и hex — не коды.
pub fn is_error_code(w: &str) -> bool {
    let letters = w.bytes().take_while(u8::is_ascii_lowercase).count();
    let digits = w.len() - letters;
    (1..=4).contains(&letters)
        && (4..=6).contains(&digits)
        && w.bytes().skip(letters).all(|b| b.is_ascii_digit())
}

/// Наименьшее число insight со словом, при котором оно может быть частым ([`is_common`]): на
/// малом корпусе доля документов ничего не говорит (у решения из двух записей каждое их слово —
/// в половине) **[решение исполнителя, задача 8 M7.1]**.
pub const COMMON_MIN_DOCS: usize = 5;

/// Частое слово ошибки (M7.1 §4.6 п. 3): `df` из `docs` действующих insight области поиска
/// содержат слово с этим началом, `df ≥` [`COMMON_MIN_DOCS`] и доля `df / docs` строго выше
/// `pct` процентов (`recall.post_tool_coverage.common_pct`; 100 — правило выключено).
pub fn is_common(df: usize, docs: usize, pct: u32) -> bool {
    df >= COMMON_MIN_DOCS && df * 100 > pct as usize * docs
}

/// Сколько слов `terms` встречается в `text` по границе слова (без учёта регистра): слово
/// текста начинается с терма — как префиксный терм FTS (`file` находит `files`, но не
/// `profile`).
pub fn term_coverage(terms: &[String], text: &str) -> usize {
    let text = text.to_lowercase();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    terms
        .iter()
        .filter(|t| words.iter().any(|w| w.starts_with(t.as_str())))
        .count()
}

/// Короче — слово сравнивается целиком: у трёхбуквенных слов («код», «api», «pdf») окончание
/// неотличимо от корня.
pub const STEM_MIN_CHARS: usize = 4;

/// Слово для сравнения основ: нижний регистр, `ё` → `е` (бывш. `idea_norm_word` сервера).
pub fn norm_word(w: &str) -> String {
    w.to_lowercase().replace('ё', "е")
}

/// Основа слова: первые `max(4, длина − 3)` символов — срез типичных русских и английских
/// окончаний («отчётов» → «отчё», «экспортировать» → «экспортиров», «reports» → «repo»); слова
/// короче [`STEM_MIN_CHARS`] — целиком (бывш. `idea_stem` сервера).
pub fn stem(w: &str) -> String {
    let c: Vec<char> = w.chars().collect();
    if c.len() < STEM_MIN_CHARS {
        return w.to_string();
    }
    c[..c.len().saturating_sub(3).max(STEM_MIN_CHARS)]
        .iter()
        .collect()
}

/// Слово с готовой основой ([`stem`]) — сравнение без повторных аллокаций на каждой паре.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemWord {
    word: String,
    stem: String,
    short: bool,
}

impl StemWord {
    /// `w` — уже [`norm_word`].
    pub fn new(w: String) -> Self {
        let short = w.chars().count() < STEM_MIN_CHARS;
        let stem = if short { w.clone() } else { stem(&w) };
        StemWord {
            word: w,
            stem,
            short,
        }
    }

    /// Слово с цифрой — только целиком (`post_tool`, ревью задачи 9, I-2): коды ошибок
    /// (`lnk1181`/`lnk1104`, `e0382`/`e0384`) различаются хвостом, который [`stem`] срезает.
    /// Остальные слова — как [`StemWord::new`]. Подсказки-идеи этим не пользуются (эталон M5.7).
    pub fn exact_codes(w: String) -> Self {
        if w.chars().any(|c| c.is_ascii_digit()) {
            StemWord {
                stem: w.clone(),
                word: w,
                short: true,
            }
        } else {
            StemWord::new(w)
        }
    }

    /// Слова совпадают по основе: одно начинается с основы другого; короче [`STEM_MIN_CHARS`] —
    /// только точное совпадение. Ложные совпадения длинных слов с общим началом («план» ~
    /// «планета») допускаются сознательно: это подтверждение к FTS, а не самостоятельный поиск.
    pub fn matches(&self, o: &StemWord) -> bool {
        if self.short || o.short {
            return self.word == o.word;
        }
        o.word.starts_with(&self.stem) || self.word.starts_with(&o.stem)
    }
}

/// Правило совпадения слова запроса со словом текста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordMatch {
    /// Как есть (M4): нижний регистр, слово текста начинается со слова запроса ([`term_coverage`]).
    #[default]
    Plain,
    /// Основы: [`norm_word`], без служебных слов (`zam_core::text::is_stop_word`), совпадение
    /// [`StemWord::matches`] — как у подсказок-идей M5.7; у `post_tool` слова с цифрой (коды
    /// ошибок) — только целиком ([`StemWord::exact_codes`]).
    Stems,
}

/// Слова запроса `terms` ([`error_terms`]) под правило `m`: `Plain` — как есть; `Stems` — без
/// служебных слов ru/en, [`norm_word`], без повторов (после `ё` → `е` повторы могут быть не
/// соседними; порядок — по алфавиту) — бывш. `idea_terms` сервера.
pub fn prepared_terms(terms: &[String], m: WordMatch) -> Vec<String> {
    match m {
        WordMatch::Plain => terms.to_vec(),
        WordMatch::Stems => {
            let mut out: Vec<String> = terms
                .iter()
                .filter(|t| !is_stop_word(&t.to_lowercase()))
                .map(|t| norm_word(t))
                .filter(|t| !is_stop_word(t))
                .collect();
            out.sort();
            out.dedup();
            out
        }
    }
}

/// Значимые слова с основами: [`prepared_terms`] при [`WordMatch::Stems`].
pub fn stem_words(terms: &[String]) -> Vec<StemWord> {
    prepared_terms(terms, WordMatch::Stems)
        .into_iter()
        .map(StemWord::new)
        .collect()
}

/// Слова-реплики ru/en (M6.6 §3.1, замечание 17; формы — M7.1 §4.5 п. 2): согласие, понукание,
/// благодарность — не предмет запроса. Сравниваются со словом запроса после [`norm_word`]
/// **целиком**, без основ: основа «продол» съела бы «продолжительность», поэтому формы
/// («продолжаем», «продолжим») — отдельными словами. Слова короче трёх символов («да», «ок»,
/// «go») отсекает уже [`error_terms`].
pub const PROMPT_REPLY_WORDS: [&str; 71] = [
    "согласен",
    "согласна",
    "согласны",
    "ладно",
    "хорошо",
    "хорош",
    "отлично",
    "супер",
    "давай",
    "давайте",
    "продолжай",
    "продолжайте",
    "продолжи",
    "продолжаем",
    "продолжим",
    "дальше",
    "далее",
    "делай",
    "делайте",
    "сделай",
    "приступай",
    "вперед",
    "вперёд",
    "поехали",
    "погнали",
    "понял",
    "поняла",
    "понятно",
    "ясно",
    "принято",
    "верно",
    "точно",
    "конечно",
    "угу",
    "ага",
    "окей",
    "спасибо",
    "годится",
    "подходит",
    "одобряю",
    "начинай",
    "запускай",
    "готово",
    "норм",
    "нормально",
    "правильно",
    "именно",
    "нет",
    "yes",
    "yep",
    "yeah",
    "sure",
    "agreed",
    "agree",
    "okay",
    "fine",
    "great",
    "good",
    "thanks",
    "thank",
    "continue",
    "proceed",
    "next",
    "lgtm",
    "done",
    "right",
    "correct",
    "exactly",
    "please",
    "ahead",
    "approved",
];

/// Слово запроса (уже [`norm_word`]) — реплика [`PROMPT_REPLY_WORDS`].
fn is_reply_word(w: &str) -> bool {
    // множество нормализованных слов — один раз на процесс (`вперёд` и `вперед` — одно слово)
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| PROMPT_REPLY_WORDS.iter().map(|w| norm_word(w)).collect())
        .contains(w)
}

/// Предметные слова запроса пользователя (M6.6 §3.1): слова [`error_terms`] (от 3 символов,
/// нижний регистр) → [`prepared_terms`] при [`WordMatch::Stems`] (без служебных слов ru/en,
/// [`norm_word`], без повторов, по алфавиту) → без слов-реплик [`PROMPT_REPLY_WORDS`]. Пусто —
/// запрос без предмета («согласен», «да, давай», «продолжай дальше»): сервер не ищет к нему
/// подсказок.
pub fn subject_terms(prompt: &str) -> Vec<String> {
    let mut out = prepared_terms(&error_terms(prompt), WordMatch::Stems);
    out.retain(|w| !is_reply_word(w));
    out
}

/// Предметных слов ([`subject_terms`]), при которых запрос к подсказкам «короткий» (M6.6 §3.2,
/// **[решение координатора]** — константа, не ключ конфигурации): у одного-двух слов векторный
/// сигнал самый слабый, поэтому подсказка поиска — только с лексическим совпадением
/// ([`PromptGate::admits_card`]), а подсказка-задача — только с покрытым заголовком (ревью задачи
/// 2, M1).
pub const PROMPT_SHORT_WORDS: usize = 2;

/// Решение по запросу пользователя к подсказкам (`/recall` `prompt`, M6.6 §3.1–§3.3) — до
/// поиска: одно правило для сервера и модели калибровки `xtask eval --hints` ([`prompt_gate`],
/// отбор карточек — [`PromptGate::admits_card`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptGate {
    /// Пересланное сообщение агента ([`zam_core::text::is_relayed_message`]) — тишина, без
    /// строк словаря.
    Relayed,
    /// Предметных слов нет — только строки словаря (имя термина в запросе — уже предмет).
    NoSubject,
    /// Поиск подсказок: предметные слова и «короткий запрос» (`subject.len() <=`
    /// [`PROMPT_SHORT_WORDS`]).
    Search { subject: Vec<String>, short: bool },
}

impl PromptGate {
    /// Карточка внутреннего поиска `/search` (JSON элемента `items`) может стать подсказкой
    /// поиска: поиск идёт ([`PromptGate::Search`]) и запрос не короткий — или карточку нашёл
    /// лексический канал ([`lexical_card`]). Порог балла и виды записей — у сервера.
    pub fn admits_card(&self, card: &serde_json::Value) -> bool {
        match self {
            PromptGate::Search { short, .. } => !short || lexical_card(card),
            PromptGate::Relayed | PromptGate::NoSubject => false,
        }
    }
}

/// Имена каналов карточки `/search` — её поле `sources` (нестроковые элементы пропускаются).
pub fn card_sources(card: &serde_json::Value) -> impl Iterator<Item = &str> {
    card["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
}

/// Карточку `/search` нашёл лексический канал ([`crate::any_lexical`] по [`card_sources`]):
/// короткий запрос подсказок ([`PromptGate::admits_card`]) и подтверждение подсказок-идей
/// сервера (M6.6 §3.2).
pub fn lexical_card(card: &serde_json::Value) -> bool {
    crate::any_lexical(card_sources(card))
}

/// Правило тишины подсказок к запросу (M6.6 §3.1–§3.3). Пересланное сообщение — раньше всего и
/// по всему `prompt` (тот же признак, что у хука `zam hook prompt`); предметные слова — по
/// первым `head_chars` символам (запрос поиска подсказок, у сервера —
/// `zam_store::search::QUERY_MAX_CHARS`); иначе — поиск с признаком короткого запроса.
pub fn prompt_gate(prompt: &str, head_chars: usize) -> PromptGate {
    if zam_core::text::is_relayed_message(prompt) {
        return PromptGate::Relayed;
    }
    let head: String = prompt.chars().take(head_chars).collect();
    let subject = subject_terms(&head);
    if subject.is_empty() {
        return PromptGate::NoSubject;
    }
    let short = subject.len() <= PROMPT_SHORT_WORDS;
    PromptGate::Search { subject, short }
}

/// Порог покрытия слов запроса текстом записи у ослабленного FTS (любое слово):
/// `need = clamp(⌈n / divisor⌉, 2, cap)`, не больше `n` — иначе общие слова ошибки («file»,
/// «error») поднимали бы случайные записи, а длинная ошибка (stack trace) требовала бы от
/// короткой записи десятков совпадений. Ключ `recall.post_tool_coverage` (M6 §9); с M7.1 §4.6
/// п. 3 — и порог частого слова `common_pct` ([`is_common`], [`CoverageQuery::subject`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRule {
    /// Делитель числа слов запроса, 1..=10.
    pub divisor: u32,
    /// Наибольший порог, 2..=32.
    pub cap: u32,
    pub word_match: WordMatch,
    /// Доля действующих insight области поиска со словом ошибки, %, выше которой слово частое и
    /// в покрытие не идёт (1..=100; 100 — правило выключено). Нет в конфигурации — умолчание.
    #[serde(default = "default_common_pct")]
    pub common_pct: u32,
}

/// Умолчание `common_pct` ([`CoverageRule::M71`]).
fn default_common_pct() -> u32 {
    CoverageRule::M71.common_pct
}

impl CoverageRule {
    /// Правило M4 **[решение исполнителя, задача 8 M4]**: ⌈n/3⌉, от 2 до 4, слова как есть.
    pub const M4: CoverageRule = CoverageRule {
        divisor: 3,
        cap: 4,
        word_match: WordMatch::Plain,
        common_pct: 100,
    };

    /// Умолчание M6 по сетке §9 (задача 9 с правками ревью, `eval/grid-m6-hints.toml`):
    /// ⌈n/2⌉, от 2 до 6, слова как есть. Решение — на случаях M4 и двух независимых наборах (84 случая, 42
    /// релевантных; случаи исполнителя задачи 9 — справочно): Δ hint_precision +0,252
    /// (Бонферрони — нижняя граница +0,156), потерь ожидаемых подсказок 0 из 42, Δ silence_rate
    /// +0,357; рост точности в каждой части — M4 +0,247, независимые +0,254.
    pub const M6: CoverageRule = CoverageRule {
        divisor: 2,
        cap: 6,
        word_match: WordMatch::Plain,
        common_pct: 100,
    };

    /// Умолчание M7.1 (§4.6 п. 3): правило M6 и порог частого слова 10 % по сетке
    /// `eval/grid-m71-hints-common.toml` (правило выбора M6 §9, записано до прогона; журнал этапа
    /// M7, задача 8): на решающей части (60 случаев `post_tool` M4 и независимых наборов, 42
    /// релевантных) Δ hint_precision +0,160 (Бонферрони — нижняя граница +0,051), потерь
    /// ожидаемых подсказок 0 из 42, Δ silence_rate +0,095; 5 % теряет 3 подсказки.
    pub const M71: CoverageRule = CoverageRule {
        common_pct: 10,
        ..CoverageRule::M6
    };

    /// Нужно совпавших слов из `n` (`n` — после [`prepared_terms`]).
    pub fn need(&self, n: usize) -> usize {
        let cap = (self.cap as usize).max(2);
        n.div_ceil((self.divisor as usize).max(1))
            .clamp(2, cap)
            .min(n)
    }
}

impl Default for CoverageRule {
    fn default() -> Self {
        CoverageRule::M71
    }
}

/// Слова запроса, подготовленные один раз под правило (сервер проверяет десятки находок FTS).
#[derive(Debug, Clone)]
pub struct CoverageQuery {
    rule: CoverageRule,
    plain: Vec<String>,
    stems: Vec<StemWord>,
    /// Коды ошибок ([`is_error_code`]) покрытия по предметным словам: совпадение одного кода
    /// целиком — достаточно ([`CoverageQuery::subject`]); у [`CoverageQuery::new`] — пусто.
    codes: Vec<String>,
}

impl CoverageQuery {
    /// `query` — слова ошибки ([`error_terms`]).
    pub fn new(query: &[String], rule: &CoverageRule) -> Self {
        let (plain, stems) = match rule.word_match {
            WordMatch::Plain => (query.to_vec(), Vec::new()),
            WordMatch::Stems => (
                Vec::new(),
                prepared_terms(query, WordMatch::Stems)
                    .into_iter()
                    .map(StemWord::exact_codes)
                    .collect(),
            ),
        };
        CoverageQuery {
            rule: *rule,
            plain,
            stems,
            codes: Vec::new(),
        }
    }

    /// Покрытие по предметным словам (M7.1 §4.6 п. 3): `query` ([`subject_error_terms`]) без
    /// слов, для которых `skip` (частые, имена каталогов из команды); коды ошибок ([`is_error_code`]) не
    /// снимаются и в число слов порога не входят — совпадение одного кода целиком
    /// достаточно. Порог `need` — по числу оставшихся слов.
    pub fn subject(query: &[String], rule: &CoverageRule, skip: impl Fn(&str) -> bool) -> Self {
        let (codes, words): (Vec<String>, Vec<String>) =
            query.iter().cloned().partition(|t| is_error_code(t));
        let words: Vec<String> = words.into_iter().filter(|t| !skip(t)).collect();
        CoverageQuery {
            codes,
            ..CoverageQuery::new(&words, rule)
        }
    }

    /// Слов запроса после подготовки (знаменатель порога).
    pub fn len(&self) -> usize {
        match self.rule.word_match {
            WordMatch::Plain => self.plain.len(),
            WordMatch::Stems => self.stems.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Совпавших слов запроса в `text`.
    pub fn covered(&self, text: &str) -> usize {
        match self.rule.word_match {
            WordMatch::Plain => term_coverage(&self.plain, text),
            WordMatch::Stems => {
                // все слова текста (без предела слов запроса): запись длиннее 32 слов
                let words: Vec<StemWord> = text
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(norm_word)
                    .filter(|w| !is_stop_word(w))
                    .map(StemWord::exact_codes)
                    .collect();
                self.stems
                    .iter()
                    .filter(|t| words.iter().any(|w| t.matches(w)))
                    .count()
            }
        }
    }

    /// Покрытие `text` достаточно ([`CoverageRule::need`]) или в `text` целиком есть код ошибки
    /// запроса ([`CoverageQuery::subject`]); пустой запрос без кодов — нет.
    pub fn enough(&self, text: &str) -> bool {
        if !self.codes.is_empty() {
            let text = text.to_lowercase();
            let mut words = text.split(|c: char| !c.is_alphanumeric());
            if words.any(|w| self.codes.iter().any(|c| c == w)) {
                return true;
            }
        }
        let n = self.len();
        n > 0 && self.covered(text) >= self.rule.need(n)
    }
}

/// Покрытие слов запроса `query` ([`error_terms`]) текстом `text` достаточно по правилу `rule`
/// (см. [`CoverageQuery`]).
pub fn coverage_enough(query: &[String], text: &str, rule: &CoverageRule) -> bool {
    CoverageQuery::new(query, rule).enough(text)
}

/// Одна строка без переводов строк и с обрезанными краями (заголовок, путь).
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Строка подсказки (§10.3, M6 §4.3): `ZeroAIMemory: <lead><title> [<теги через «·»>] <ссылка>`;
/// пустые теги опускаются вместе со скобками, надёжность сокращается (`ax`, `th`, `hyp`).
pub fn hint_line(lead: &str, title: &str, tags: &[&str], reference: &str) -> String {
    let tags: Vec<&str> = tags
        .iter()
        .copied()
        .filter(|t| !t.is_empty())
        .map(zam_core::certainty_short)
        .collect();
    let mut s = format!("{HINT_PREFIX} {lead}{}", one_line(title));
    if !tags.is_empty() {
        s.push_str(&format!(" [{}]", tags.join("·")));
    }
    s.push_str(&format!(" {reference}"));
    s
}

/// Раздел дайджеста: заголовок (строка `### …`, ставится, только если в раздел вошёл хоть
/// один блок, и не повторяется, если раздел предыдущего выведенного блока — с тем же
/// заголовком: такой раздел продолжает его, M6.6 §4.2) и блоки — по одному на подсказку
/// (строка и, возможно, строки с отступом).
/// `reserved` — раздел-резерв (Rules дайджеста и пакета восстановления, M6 §8): выводится
/// целиком сверх бюджета и в бюджете не считается.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: Option<String>,
    pub blocks: Vec<String>,
    pub reserved: bool,
    /// Стойкость к пределу вывода канала (ревью 8a, C1): при превышении сначала снимаются блоки
    /// разделов с меньшим `keep` (внутри — с конца); бюджет токенов идёт по порядку разделов.
    pub keep: u8,
    /// Стойкость первого блока, если выше `keep` (первая ловушка — наравне с обязательными).
    pub keep_head: Option<u8>,
    /// Строка о снятых пределом вывода блоках: `(что, где полностью)` →
    /// `ZeroAIMemory: не показано — <что>: N (<где>)` (без склонения по числу); `None` —
    /// «строк», `memory_recall`.
    pub omit: Option<(String, String)>,
}

impl Section {
    /// Раздел в бюджете.
    pub fn new(title: Option<String>, blocks: Vec<String>) -> Self {
        Self {
            title,
            blocks,
            reserved: false,
            keep: 0,
            keep_head: None,
            omit: None,
        }
    }

    /// Стойкость блока `bi`.
    pub fn keep_of(&self, bi: usize) -> u8 {
        match self.keep_head {
            Some(h) if bi == 0 => h.max(self.keep),
            _ => self.keep,
        }
    }

    /// Строка о `n` снятых пределом вывода блоках раздела.
    fn omit_line(&self, n: usize) -> String {
        let (what, whole) = self
            .omit
            .clone()
            .unwrap_or_else(|| ("строк".to_string(), "memory_recall".to_string()));
        format!("ZeroAIMemory: не показано — {what}: {n} ({whole})")
    }
}

/// Упаковка разделов в `cap` по счётчику `count` (M6 §4.2, §8): разделы-резервы — целиком и вне
/// бюджета; остальные разделы и блоки — по порядку (приоритет), блок, который не влезает,
/// пропускается, следующие короткие ещё пробуются. `cap` — наибольший счёт, что помещается в
/// бюджет (`TokenCounter::budget_cap`): с ним сравниваются суммы частей, и с ним же — счёт
/// текста разделов в бюджете целиком (при расхождении суммы с целым с конца снимаются блоки).
/// `fits_all` — предел вывода канала на весь текст (ревью 8a, C1): не влезает — снимаются блоки
/// в бюджете, начиная с разделов с меньшим `keep`, внутри — с конца (резерв укладывается до
/// упаковки). Итог — текст всех
/// вошедших блоков в порядке разделов (блоки через перевод строки, перед заголовком раздела —
/// пустая строка) и вошедшие блоки `(раздел, блок)`.
pub fn pack_sections(
    sections: &[Section],
    cap: usize,
    count: &dyn Fn(&str) -> usize,
    fits_all: &dyn Fn(&str) -> bool,
) -> (String, Vec<(usize, usize)>) {
    let mut used = 0usize;
    let mut budgeted: Vec<(usize, usize)> = Vec::new();
    // раздел последнего выведенного блока (резерв выводится всегда): заголовок, совпавший с его
    // заголовком, не выводится и не оплачивается — как в [`render`]
    let mut last: Option<usize> = None;
    for (si, sec) in sections.iter().enumerate() {
        if sec.reserved {
            if !sec.blocks.is_empty() {
                last = Some(si);
            }
            continue;
        }
        for (bi, block) in sec.blocks.iter().enumerate() {
            let head = match &sec.title {
                Some(t) if !continues(sections, last, si) => count(&format!("\n{t}\n")),
                _ => 0,
            };
            let cost = head + count(&format!("{block}\n"));
            if used + cost <= cap {
                used += cost;
                last = Some(si);
                budgeted.push((si, bi));
            }
        }
    }
    while !budgeted.is_empty() && count(&render(sections, &budgeted)) > cap {
        budgeted.pop();
    }
    let reserved: Vec<(usize, usize)> = sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.reserved)
        .flat_map(|(si, s)| (0..s.blocks.len()).map(move |bi| (si, bi)))
        .collect();
    let all = |budgeted: &[(usize, usize)]| {
        let mut t: Vec<(usize, usize)> = reserved.iter().chain(budgeted).copied().collect();
        t.sort_unstable();
        t
    };
    // предел вывода канала (весь текст вместе с резервом и строками о снятом): снимаются блоки в
    // бюджете, наименее стойкие — первыми, внутри — с конца
    let mut popped: Vec<usize> = vec![0; sections.len()];
    loop {
        let (text, taken) = with_omits(sections, &all(&budgeted), &popped);
        if budgeted.is_empty() || fits_all(&text) {
            return (text, taken);
        }
        let k = (0..budgeted.len())
            .min_by_key(|&k| {
                let (si, bi) = budgeted[k];
                (sections[si].keep_of(bi), std::cmp::Reverse(k))
            })
            .unwrap_or(budgeted.len() - 1);
        popped[budgeted[k].0] += 1;
        budgeted.remove(k);
    }
}

/// Текст с честными строками о снятом пределом вывода (повторное ревью 8a): у раздела, где
/// снят хоть один блок, — строка [`Section::omit_line`] в конце раздела; снят целиком — та же
/// строка в конце текста. Строки — дополнительные блоки (индексы за пределами `blocks`
/// раздела; раздел конца — индекс `sections.len()`).
fn with_omits(
    sections: &[Section],
    taken: &[(usize, usize)],
    popped: &[usize],
) -> (String, Vec<(usize, usize)>) {
    if popped.iter().all(|&n| n == 0) {
        return (render(sections, taken), taken.to_vec());
    }
    let mut ext: Vec<Section> = sections.to_vec();
    let mut taken = taken.to_vec();
    let mut tail: Vec<String> = Vec::new();
    // снятые целиком разделы с одинаковой строкой (что и где) — одной строкой с суммой
    let mut whole: Vec<(usize, usize)> = Vec::new();
    for (si, &n) in popped.iter().enumerate().filter(|(_, n)| **n > 0) {
        if taken.iter().any(|&(s, _)| s == si) {
            ext[si].blocks.push(sections[si].omit_line(n));
            taken.push((si, ext[si].blocks.len() - 1));
        } else if let Some(w) = whole
            .iter_mut()
            .find(|(s, _)| sections[*s].omit == sections[si].omit)
        {
            w.1 += n;
        } else {
            whole.push((si, n));
        }
    }
    tail.extend(whole.iter().map(|&(si, n)| sections[si].omit_line(n)));
    if !tail.is_empty() {
        let n = tail.len();
        ext.push(Section::new(None, tail));
        taken.extend((0..n).map(|bi| (sections.len(), bi)));
    }
    taken.sort_unstable();
    (render(&ext, &taken), taken)
}

/// Текст разделов-резервов целиком (размер резерва — отдельной строкой ответа, M6 §17 п. 3).
pub fn reserved_text(sections: &[Section]) -> String {
    let taken: Vec<(usize, usize)> = sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.reserved)
        .flat_map(|(si, s)| (0..s.blocks.len()).map(move |bi| (si, bi)))
        .collect();
    render(sections, &taken)
}

/// Блок раздела `si` идёт сразу за блоком раздела `last` и продолжает его: тот же раздел или
/// раздел с тем же заголовком — заголовок `si` не выводится.
fn continues(sections: &[Section], last: Option<usize>, si: usize) -> bool {
    last.is_some_and(|l| l == si || sections[l].title == sections[si].title)
}

/// Текст вошедших блоков (см. [`pack_sections`]).
fn render(sections: &[Section], taken: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for &(si, bi) in taken {
        if last != Some(si) {
            if let Some(t) = &sections[si].title
                && !continues(sections, last, si)
            {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
                out.push('\n');
            }
            last = Some(si);
        }
        out.push_str(&sections[si].blocks[bi]);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_error_strips_paths_numbers_hashes() {
        let raw = "/bin/bash: ./deploy.sh: /bin/bash^M: bad interpreter: No such file or \
                   directory\nerror at C:\\work\\a.rs:12:5 commit 3f2a9c1d0b request \
                   123e4567-e89b-12d3-a456-426614174000 ptr 0xDEADBEEF line 42 (exit 127)";
        let n = normalize_error(raw);
        assert_eq!(
            n,
            "bad interpreter: No such file or directory error at commit request ptr line (exit"
        );
        for gone in [
            "/",
            "\\",
            "12",
            "3f2a9c1d0b",
            "123e4567",
            "DEADBEEF",
            "42",
            "127",
        ] {
            assert!(!n.contains(gone), "{gone}: {n}");
        }
        // слова без цифр и обычные слова-«hex» (deadbeef без цифр) остаются
        assert_eq!(normalize_error("cafe added"), "cafe added");
        // токены с буквой — целиком, числа и хвосты позиции — прочь
        assert_eq!(normalize_error("E0425 unresolved"), "E0425 unresolved");
        assert_eq!(
            normalize_error("error[E0425]: at main.rs:12:5: LNK1181 TS2307 sha256 x86_64 42 1.5"),
            "error[E0425]: at main.rs LNK1181 TS2307 sha256 x86_64"
        );
        assert_eq!(normalize_error("  \n 123 456 \t"), "");
        // предел 512 символов, по границе символа
        let long = "ошибка ".repeat(200);
        let n = normalize_error(&long);
        assert_eq!(n.chars().count(), ERROR_MAX_CHARS);
        assert!(n.starts_with("ошибка ошибка"));
    }

    #[test]
    fn error_regex_and_coverage() {
        assert!(error_regex_matches(
            r"bad interpreter",
            "x: bad interpreter: y"
        ));
        assert!(!error_regex_matches(r"^ok$", "bad"));
        // некорректный шаблон не совпадает
        assert!(!error_regex_matches(r"(", "("));
        let t = error_terms("Bad interpreter: No such file or directory, bad");
        assert_eq!(t, ["bad", "interpreter", "such", "file", "directory"]);
        assert_eq!(term_coverage(&t, "BAD interpreter и CRLF"), 2);
        // по границе слова: `file` — в `files`, но не в `profile`
        let f = vec!["file".to_string()];
        assert_eq!(term_coverage(&f, "no files here"), 1);
        assert_eq!(term_coverage(&f, "profile"), 0);
        let m4 = |matched_terms: &[&str], total: &[&str]| {
            let q: Vec<String> = total.iter().map(|s| s.to_string()).collect();
            coverage_enough(&q, &matched_terms.join(" "), &CoverageRule::M4)
        };
        let w = [
            "aa1", "bb2", "cc3", "dd4", "ee5", "ff6", "gg7", "hh8", "ii9", "jj0",
        ];
        assert!(m4(&w[..2], &w[..5]));
        assert!(!m4(&w[..1], &w[..5]));
        assert!(m4(&w[..1], &w[..1]));
        assert!(!m4(&[], &[]));
        assert!(!m4(&w[..3], &w));
        assert!(m4(&w[..4], &w));
        // длинная ошибка: порог не выше 4
        let long: Vec<String> = (0..32).map(|i| format!("w{i:02}")).collect();
        let text = |k: usize| long[..k].join(" ");
        assert!(coverage_enough(&long, &text(4), &CoverageRule::M4));
        assert!(!coverage_enough(&long, &text(3), &CoverageRule::M4));
        // кэш шаблонов: повтор и некорректный шаблон
        assert!(error_regex_matches(r"bad interpreter", "bad interpreter"));
        assert!(!error_regex_matches(r"(", "("));
    }

    /// Правило M4 через [`CoverageRule::need`] — та же формула, что прежний
    /// `coverage_enough(matched, total)` (задача 8 M4), при любых `n` и `m`.
    #[test]
    fn coverage_rule_m4_equals_old_formula() {
        let old = |m: usize, n: usize| n > 0 && m >= n.div_ceil(3).clamp(2, 4).min(n);
        for n in 0..40usize {
            for m in 0..=n {
                let r = CoverageRule::M4;
                assert_eq!(n > 0 && m >= r.need(n), old(m, n), "n={n} m={m}");
            }
        }
        let r = |divisor, cap| CoverageRule {
            divisor,
            cap,
            ..CoverageRule::M6
        };
        assert_eq!(r(2, 6).need(20), 6);
        assert_eq!(r(2, 6).need(9), 5);
        assert_eq!(r(3, 3).need(20), 3);
        assert_eq!(r(2, 3).need(1), 1);
        assert_eq!(r(3, 6).need(3), 2);
    }

    /// Финальное ревью M5.7, I-1′ (перенесено из сервера): основы слов — формы, короткие слова.
    #[test]
    fn stem_word_match_forms_and_short_words() {
        let m =
            |a: &str, b: &str| StemWord::new(norm_word(a)).matches(&StemWord::new(norm_word(b)));
        // формы одного слова
        assert!(m("отчёты", "отчётов"));
        assert!(m("отчеты", "Отчётов"));
        assert!(m("экспортировать", "экспорт"));
        assert!(m("экспорт", "экспортировать"));
        assert!(m("reports", "report"));
        assert!(m("PDF", "pdf"));
        // короткие слова — только целиком
        assert!(!m("код", "кодекс"));
        assert!(!m("pdf", "pdfium"));
        // длинные с общим началом — совпадают (сознательно: это подтверждение, не поиск)
        assert!(m("план", "планета"));
        // разные слова
        assert!(!m("логирование", "экспорт"));
        assert!(!m("сервера", "скачать"));
        assert_eq!(stem("отчётов"), "отчё");
        assert_eq!(stem("экспортировать"), "экспортиров");
        assert_eq!(stem("код"), "код");
        assert_eq!(norm_word("ЁЛКА"), "елка");
    }

    /// `prepared_terms`: `Plain` — как есть; `Stems` — без служебных слов, `ё` → `е`, без повторов.
    #[test]
    fn prepared_terms_by_word_match() {
        let t = error_terms("Ещё еще отчёты отчеты для the Reports");
        assert_eq!(prepared_terms(&t, WordMatch::Plain), t);
        assert_eq!(prepared_terms(&t, WordMatch::Stems), ["reports", "отчеты"]);
        assert_eq!(stem_words(&t).len(), 2);
    }

    /// `Stems`: формы слов и `ё` совпадают, служебные слова не считаются ни в запросе, ни в
    /// пороге; `Plain` — прежнее сравнение по началу слова.
    #[test]
    fn coverage_stems_vs_plain() {
        let stems = CoverageRule {
            word_match: WordMatch::Stems,
            ..CoverageRule::M4
        };
        let q = error_terms("консоль кракозябры");
        let text = "Вывод консоли Windows с кириллицей превращается в кракозябры";
        assert!(!coverage_enough(&q, text, &CoverageRule::M4));
        assert!(coverage_enough(&q, text, &stems));
        // служебные слова: «error timed out» → в Stems остаются error, timed, out (out — 3 буквы,
        // не стоп-слово), «times out» совпадает по основе
        let q = error_terms("error: timed out");
        let text = "Cargo fetch times out: spurious network error";
        assert_eq!(CoverageQuery::new(&q, &CoverageRule::M4).covered(text), 2);
        assert_eq!(CoverageQuery::new(&q, &stems).covered(text), 3);
        // запрос только из служебных слов — пусто, подсказки нет
        let q = error_terms("that this with from");
        assert!(CoverageQuery::new(&q, &stems).is_empty());
        assert!(!coverage_enough(&q, "that this with from", &stems));
        assert!(coverage_enough(
            &q,
            "that this with from",
            &CoverageRule::M4
        ));
        // слово с «ё» в тексте
        let q = error_terms("ещё отчеты выгрузка");
        assert!(coverage_enough(&q, "Выгрузка отчётов", &stems));
    }

    /// Ревью задачи 9, I-2 (M-5): коды ошибок с цифрами не совпадают по общему началу —
    /// `lnk1181` ≠ `lnk1104`, `e0382` ≠ `e0384`, `0x80070005` ≠ `0x80070002`; у идей (`new`) —
    /// прежнее сравнение основ.
    #[test]
    fn stems_codes_with_digits_match_whole() {
        let stems = CoverageRule {
            word_match: WordMatch::Stems,
            ..CoverageRule::M4
        };
        for (q, t) in [
            ("lnk1181 onnxruntime", "LNK1104 onnxruntime"),
            ("e0382 borrow", "E0384 borrow"),
            ("0x80070005 access", "0x80070002 access"),
        ] {
            let q = error_terms(q);
            assert_eq!(CoverageQuery::new(&q, &stems).covered(t), 1, "{q:?} / {t}");
            assert!(!coverage_enough(&q, t, &stems), "{q:?} / {t}");
        }
        // тот же код целиком совпадает
        let q = error_terms("lnk1181 onnxruntime");
        assert!(coverage_enough(&q, "LNK1181: onnxruntime.lib", &stems));
        // идеи: основы прежние (эталон M5.7)
        let a = StemWord::new("lnk1181".into());
        assert!(a.matches(&StemWord::new("lnk1104".into())));
        let c = StemWord::exact_codes("lnk1181".into());
        assert!(!c.matches(&StemWord::exact_codes("lnk1104".into())));
        assert!(
            StemWord::exact_codes("отчетов".into())
                .matches(&StemWord::exact_codes("отчеты".into()))
        );
    }

    /// M7.1 §4.6 п. 3: строка `Exit code <N>` агента снимается только первой строкой ошибки.
    #[test]
    fn exit_header_stripped_only_first_line() {
        assert_eq!(strip_exit_header("Exit code 2\nls: x"), "ls: x");
        assert_eq!(strip_exit_header("  exit code 127 \r\nboom"), "boom");
        assert_eq!(strip_exit_header("Exit code 1"), "");
        assert_eq!(strip_exit_header("Exit code -1073741819\nx"), "x");
        for keep in [
            "robocopy exited with code 3",
            "error: Exit code 1 from build",
            "boom\nExit code 1",
            "Exit code x\ny",
        ] {
            assert_eq!(strip_exit_header(keep), keep);
        }
    }

    /// M7.1 §4.6 п. 3: коды ошибок — 1–4 латинские буквы и 4–6 цифр.
    #[test]
    fn error_code_words() {
        for w in ["lnk1104", "e0432", "ts2307", "cs0246", "c2065", "lnk1181"] {
            assert!(is_error_code(w), "{w}");
        }
        for w in [
            "sha256",
            "x86",
            "e2e",
            "utf8",
            "h264",
            "0x80070005",
            "00942",
            "error",
            "lnk1104x",
            "abcde1234",
            "LNK1104",
        ] {
            assert!(!is_error_code(w), "{w}");
        }
    }

    /// M7.1 §4.6 п. 3: частое слово — доля insight со словом выше порога и таких insight не
    /// меньше [`COMMON_MIN_DOCS`]; 100 % — правило выключено.
    #[test]
    fn common_word_share_and_min_docs() {
        assert!(is_common(6, 7, 20));
        assert!(!is_common(4, 4, 20), "меньше пяти insight — не частое");
        assert!(is_common(5, 5, 20));
        assert!(!is_common(20, 100, 20), "ровно порог — не выше");
        assert!(is_common(21, 100, 20));
        assert!(!is_common(100, 100, 100), "100 % — выключено");
        assert!(!is_common(0, 0, 20));
    }

    /// M7.1 §4.6 п. 3: слова ошибки для покрытия по предметным словам — без фразы «No such file or
    /// directory» (в любом регистре, целыми словами).
    #[test]
    fn service_phrase_words_dropped() {
        let n = normalize_error(strip_exit_header(
            "Exit code 2\nls: cannot access 'web/x': No Such File or Directory",
        ));
        assert_eq!(n, "ls: cannot access No Such File or Directory");
        assert_eq!(subject_error_terms(&n), ["cannot", "access"]);
        // слово вне фразы остаётся
        assert_eq!(
            subject_error_terms("cannot open file: no such file or directory"),
            ["cannot", "open", "file"]
        );
        assert_eq!(
            subject_error_terms("no such file or directoryX"),
            ["such", "file", "directoryx"]
        );
        // русская локаль и Windows (M-6 ревью задачи 8), `ё` = `е`
        for (err, rest) in [
            (
                "ls: невозможно получить доступ к 'x': Нет такого файла или каталога",
                vec!["невозможно", "получить", "доступ"],
            ),
            ("The system cannot find the path specified.", vec![]),
            (
                "open: The system cannot find the file specified. (os error 2)",
                vec!["open", "error"],
            ),
            ("Не удаётся найти указанный путь", vec![]),
            ("Системе: Не удается найти указанный файл.", vec!["системе"]),
        ] {
            assert_eq!(subject_error_terms(&normalize_error(err)), rest, "{err}");
        }
    }

    /// M7.1 §4.6 п. 3: покрытие по предметным словам — снятые слова не идут ни в число слов, ни
    /// в совпавшие; код ошибки не снимается, и одного его совпадения целиком достаточно.
    #[test]
    fn subject_coverage_skips_words_and_codes_suffice() {
        let rule = CoverageRule::M6;
        let q = error_terms("cannot access build test dist");
        let lnk = "LNK1104: cannot open file; access denied; cargo test, cargo build";
        // прежнее покрытие: 4 слова из 5 при пороге 3
        assert!(CoverageQuery::new(&q, &rule).enough(lnk));
        let common = |t: &str| matches!(t, "cannot" | "access" | "build" | "test");
        let sq = CoverageQuery::subject(&q, &rule, common);
        assert_eq!(sq.len(), 1);
        assert!(!sq.enough(lnk), "из предметных слов осталось только dist");
        assert!(sq.enough("dist"));
        // ничего не осталось — подсказки нет
        let none = CoverageQuery::subject(&error_terms("cannot access"), &rule, common);
        assert!(none.is_empty() && !none.enough(lnk));
        // код ошибки: снимать нельзя, совпадение целиком — достаточно
        let q = error_terms("LINK fatal error LNK1104 cannot open file exe");
        let all = |_: &str| true;
        let sq = CoverageQuery::subject(&q, &rule, all);
        assert!(sq.enough("Компоновщик: LNK1104 — exe занят"));
        assert!(!sq.enough("LNK1181 onnxruntime.lib"));
        assert!(!sq.enough("LNK11040 другое"));
        // у `stems` — так же
        let stems = CoverageRule {
            word_match: WordMatch::Stems,
            ..rule
        };
        assert!(CoverageQuery::subject(&q, &stems, all).enough("ошибка lnk1104"));
    }

    /// M6 §4.3: `ZeroAIMemory: <заголовок> [<вид>·<cert>] i:<id>`, надёжность сокращена.
    #[test]
    fn recall_hint_line_v2() {
        let id = "01M60A7Q4T7QK2M9ZWAXPHBK9C";
        assert_eq!(
            hint_line(
                "",
                "LF в\nскриптах",
                &["gotcha", "hypothesis"],
                &format!("i:{id}")
            ),
            format!("ZeroAIMemory: LF в скриптах [gotcha·hyp] i:{id}")
        );
        assert_eq!(
            hint_line("правило ", "Rules/x", &["theorem"], "n:1"),
            "ZeroAIMemory: правило Rules/x [th] n:1"
        );
        assert_eq!(
            hint_line("", "A", &["axiom"], "n:2"),
            "ZeroAIMemory: A [ax] n:2"
        );
        assert_eq!(
            hint_line("эта ошибка уже разбиралась: ", "x", &["", ""], "i:2"),
            "ZeroAIMemory: эта ошибка уже разбиралась: x i:2"
        );
    }

    #[test]
    fn pack_sections_respects_budget_and_skips_big_blocks() {
        let words = |s: &str| s.split_whitespace().count();
        let secs = vec![
            Section::new(
                Some("### A".into()),
                vec!["a1 a1".into(), "big ".repeat(50)],
            ),
            Section::new(Some("### B".into()), vec!["b1".into()]),
            Section::new(None, vec!["c1 c1 c1".into()]),
        ];
        let (text, taken) = pack_sections(&secs, 10, &words, &|_| true);
        assert_eq!(taken, [(0, 0), (1, 0), (2, 0)]);
        assert_eq!(text, "### A\na1 a1\n\n### B\nb1\nc1 c1 c1\n");
        assert!(words(&text) <= 10);
        // раздел без вошедших блоков — без заголовка
        let (text, taken) = pack_sections(&secs, 4, &words, &|_| true);
        assert_eq!(taken, [(0, 0)]);
        assert_eq!(text, "### A\na1 a1\n");
        let (text, taken) = pack_sections(&secs, 0, &words, &|_| true);
        assert!(taken.is_empty() && text.is_empty());
    }

    /// M6 §8: раздел-резерв (Rules) выводится целиком сверх бюджета и в нём не считается;
    /// остальные — в бюджете, в порядке разделов.
    #[test]
    fn pack_sections_reserved_outside_budget() {
        let words = |s: &str| s.split_whitespace().count();
        let mut rules = Section::new(
            Some("### R".into()),
            vec!["r1 r1 r1".into(), "r2 r2".into()],
        );
        rules.reserved = true;
        let secs = vec![
            Section::new(Some("### A".into()), vec!["a1 a1".into()]),
            rules,
            Section::new(None, vec!["c1 c1 c1".into(), "c2".into()]),
        ];
        // бюджет 5: «### A a1 a1» (4) + «c2» (1); Rules — сверх
        let (text, taken) = pack_sections(&secs, 5, &words, &|_| true);
        assert_eq!(taken, [(0, 0), (1, 0), (1, 1), (2, 1)]);
        assert_eq!(text, "### A\na1 a1\n\n### R\nr1 r1 r1\nr2 r2\nc2\n");
        assert_eq!(reserved_text(&secs), "### R\nr1 r1 r1\nr2 r2\n");
        // предел всего текста (ревью 8a, C1): резерв остаётся, с конца снимаются блоки бюджета
        let (text, taken) = pack_sections(&secs, 5, &words, &|t: &str| t.len() <= 30);
        let own: Vec<(usize, usize)> = taken.iter().copied().filter(|&(si, _)| si < 3).collect();
        assert_eq!(own, [(1, 0), (1, 1)], "{text}");
        assert!(
            text.contains("не показано — строк: 2 (memory_recall)"),
            "{text}"
        );
        // снятое пределом — честной строкой: в конце раздела или текста (повторное ревью 8a)
        let mut secs2 = secs.clone();
        secs2[2].omit = Some(("строк c".into(), "memory_tree C".into()));
        let (text, _) = pack_sections(&secs2, 5, &words, &|t: &str| !t.contains("c2"));
        assert!(
            text.ends_with("ZeroAIMemory: не показано — строк c: 1 (memory_tree C)\n"),
            "{text}"
        );
        // нулевой бюджет — только резерв
        let (text, taken) = pack_sections(&secs, 0, &words, &|_| true);
        assert_eq!(taken, [(1, 0), (1, 1)]);
        assert_eq!(text, "### R\nr1 r1 r1\nr2 r2\n");
    }

    /// M6.6 §4.2 (ревью задачи 3, M2): раздел с тем же заголовком, что у раздела предыдущего
    /// выведенного блока, заголовок не повторяет и в бюджете его не оплачивает (продолжение
    /// раздела); предыдущий не вывелся — заголовок у него свой; между ними другой раздел —
    /// заголовок повторяется.
    #[test]
    fn pack_sections_same_title_continues_section() {
        let words = |s: &str| s.split_whitespace().count();
        let secs = vec![
            Section::new(Some("### T".into()), vec!["t1 t1".into()]),
            Section::new(Some("### T".into()), vec!["o1".into()]),
            Section::new(None, vec!["p1".into()]),
        ];
        // «### T t1 t1» (4) + «o1» (1) + «p1» (1): заголовок второго раздела не оплачивается
        let (text, taken) = pack_sections(&secs, 6, &words, &|_| true);
        assert_eq!(taken, [(0, 0), (1, 0), (2, 0)]);
        assert_eq!(text, "### T\nt1 t1\no1\np1\n");
        // первый раздел не вошёл в бюджет — заголовок у второго
        let mut big = secs.clone();
        big[0].blocks = vec!["big ".repeat(50)];
        let (text, taken) = pack_sections(&big, 6, &words, &|_| true);
        assert_eq!(taken, [(1, 0), (2, 0)]);
        assert_eq!(text, "### T\no1\np1\n");
        // между ними другой выведенный раздел — заголовок повторяется
        let secs3 = vec![
            secs[0].clone(),
            Section::new(Some("### U".into()), vec!["u1".into()]),
            secs[1].clone(),
        ];
        let (text, _) = pack_sections(&secs3, 20, &words, &|_| true);
        assert_eq!(text, "### T\nt1 t1\n\n### U\nu1\n\n### T\no1\n");
    }

    /// M6.6 §3.1 (замечание 17): реплики согласия и служебные слова — не предмет запроса;
    /// реплики сравниваются целиком (без основ), предметное слово рядом с репликой остаётся.
    #[test]
    fn subject_terms_drop_replies_and_stops() {
        for reply in [
            "согласен",
            "Да, давай",
            "ok go",
            "продолжай дальше",
            "Согласен!",
            "  ок, поехали  ",
            "Вперёд",
            "yes, please continue",
            "LGTM",
            "",
        ] {
            assert!(subject_terms(reply).is_empty(), "{reply:?}");
        }
        // служебные слова — тоже не предмет
        assert!(subject_terms("что это так").is_empty());
        // слова — в форме `prepared_terms` (нижний регистр, `ё` → `е`, без повторов, по алфавиту)
        assert_eq!(
            subject_terms("согласен, мигрируй 0026"),
            ["0026", "мигрируй"]
        );
        assert_eq!(subject_terms("Давай Перенос перенос"), ["перенос"]);
        // реплика — только целиком: основа «продол» не съедает предметное слово
        assert_eq!(subject_terms("продолжительность"), ["продолжительность"]);
        assert_eq!(subject_terms("продолжай индексацию"), ["индексацию"]);
        // список реплик: нижний регистр, без повторов, только слова от 3 символов (короче
        // `error_terms` и так отсекает)
        let mut seen = std::collections::HashSet::new();
        for w in PROMPT_REPLY_WORDS {
            assert_eq!(w, w.to_lowercase(), "{w}");
            assert!(w.chars().count() >= MIN_TERM_CHARS, "{w}");
            assert!(w.chars().all(char::is_alphanumeric), "{w}");
            assert!(seen.insert(w), "повтор {w}");
            assert!(subject_terms(w).is_empty(), "{w}");
        }
    }

    /// M7.1 §4.5 п. 2: формы реплик — не предмет запроса; сравнение по-прежнему целым словом,
    /// без основ: предметное слово с тем же началом остаётся.
    #[test]
    fn reply_words_forms_silent() {
        for reply in [
            "ладно",
            "сделай",
            "делай",
            "продолжаем",
            "продолжим",
            "хорошо",
            "хорош",
            "давай",
            "поехали",
            "приступай",
            "Ладно, сделай",
            "хорош, приступай",
        ] {
            assert!(subject_terms(reply).is_empty(), "{reply:?}");
        }
        assert_eq!(subject_terms("продолжительность"), ["продолжительность"]);
        assert_eq!(subject_terms("делегирование"), ["делегирование"]);
        assert_eq!(subject_terms("ладно, сделай экспорт"), ["экспорт"]);
    }

    /// M6.6 §3.1–§3.3: правило тишины подсказок к запросу — одно для сервера и модели
    /// калибровки (ревью задачи 2, I1). Пересланное сообщение — раньше всего (даже с предметом),
    /// затем «нет предмета», затем граница короткого запроса `<= PROMPT_SHORT_WORDS`.
    #[test]
    fn prompt_gate_rules() {
        // запрос поиска подсказок сервера — первые 512 символов промпта
        const HEAD: usize = 512;
        let gate = |p: &str| prompt_gate(p, HEAD);
        let search = |p: &str| match gate(p) {
            PromptGate::Search { subject, short } => Some((subject.len(), short)),
            _ => None,
        };
        assert_eq!(
            gate("[Subagent hand-back] квазар найден в каталоге"),
            PromptGate::Relayed
        );
        assert_eq!(gate("\n<task-notification>"), PromptGate::Relayed);
        // признак пересылки — по всему промпту, как у хука (ревью задачи 2, R2): маркер за
        // пределом запроса поиска после ведущих пробелов — тоже пересланное сообщение
        let far = format!("{}[Subagent hand-back] квазар", " ".repeat(HEAD + 10));
        assert!(zam_core::text::is_relayed_message(&far));
        assert_eq!(gate(&far), PromptGate::Relayed);
        // предметные слова — только из первых `head_chars` символов
        let tail = format!("{} квазар светит ярко", "да ".repeat(HEAD / 3 + 1));
        assert_eq!(gate(&tail), PromptGate::NoSubject);
        assert_eq!(gate("согласен"), PromptGate::NoSubject);
        assert_eq!(gate("Да, давай, продолжай"), PromptGate::NoSubject);
        assert_eq!(gate(""), PromptGate::NoSubject);
        assert_eq!(
            gate("согласен, мигрируй 0026"),
            PromptGate::Search {
                subject: vec!["0026".into(), "мигрируй".into()],
                short: true
            }
        );
        assert_eq!(search("квазар"), Some((1, true)));
        assert_eq!(PROMPT_SHORT_WORDS, 2);
        assert_eq!(search("квазар светит"), Some((2, true)));
        assert_eq!(search("квазар светит ярко"), Some((3, false)));
        // маркер не в начале — слова человека
        assert_eq!(
            search("посмотри [Subagent hand-back] квазар"),
            Some((5, false))
        );
    }

    /// Отбор карточек `/search` правилом тишины (ревью задачи 2, R1): реплика и пересланное —
    /// ни одной; короткий запрос — только лексические (`fts`, `trigram`, `alias` в `sources`);
    /// длинный — любые.
    #[test]
    fn prompt_gate_admits_cards() {
        let card = |sources: serde_json::Value| serde_json::json!({"sources": sources});
        let (vector, fts, trigram, alias, folder, odd, none) = (
            card(serde_json::json!(["vector"])),
            card(serde_json::json!(["vector", "fts"])),
            card(serde_json::json!(["trigram"])),
            card(serde_json::json!(["alias"])),
            card(serde_json::json!(["vector", "folder"])),
            card(serde_json::json!([1, null, "FTS"])),
            serde_json::json!({}),
        );
        for c in [&fts, &trigram, &alias] {
            assert!(lexical_card(c), "{c}");
        }
        for c in [&vector, &folder, &odd, &none] {
            assert!(!lexical_card(c), "{c}");
        }
        assert_eq!(card_sources(&odd).collect::<Vec<_>>(), ["FTS"]);
        let all = [&vector, &fts, &trigram, &alias, &folder, &odd, &none];
        let admitted = |p: &str| -> usize {
            let g = prompt_gate(p, 512);
            all.iter().filter(|c| g.admits_card(c)).count()
        };
        assert_eq!(admitted("согласен"), 0);
        assert_eq!(admitted("[Subagent hand-back] квазар светит ярко"), 0);
        assert_eq!(admitted("квазар"), 3);
        assert_eq!(admitted("квазар светит ярко"), all.len());
    }
}
