//! Проактивное вспоминание `POST /recall` (§10.3) — чистые функции без ввода-вывода:
//! нормализация ошибки инструмента ([`normalize_error`]), сопоставление `error_regex`
//! ([`error_regex_matches`]), покрытие слов ошибки найденным текстом ([`error_terms`],
//! [`term_coverage`], [`coverage_enough`] по правилу [`CoverageRule`]: слова как есть или
//! основы [`StemWord`] — общий код с подсказками-идеями сервера, M6 §9), строка подсказки
//! ([`hint_line`]) и упаковка дайджеста в бюджет токенов ([`pack_sections`]).
//!
//! Сервер собирает кандидатов (SQLite, поиск), здесь — только то, что от данных не зависит.

use std::collections::HashMap;
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
    let mut out: Vec<String> = Vec::new();
    for w in normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_TERM_CHARS)
    {
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

/// Порог покрытия слов запроса текстом записи у ослабленного FTS (любое слово):
/// `need = clamp(⌈n / divisor⌉, 2, cap)`, не больше `n` — иначе общие слова ошибки («file»,
/// «error») поднимали бы случайные записи, а длинная ошибка (stack trace) требовала бы от
/// короткой записи десятков совпадений. Ключ `recall.post_tool_coverage` (M6 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRule {
    /// Делитель числа слов запроса, 1..=10.
    pub divisor: u32,
    /// Наибольший порог, 2..=32.
    pub cap: u32,
    pub word_match: WordMatch,
}

impl CoverageRule {
    /// Правило M4 **[решение исполнителя, задача 8 M4]**: ⌈n/3⌉, от 2 до 4, слова как есть.
    pub const M4: CoverageRule = CoverageRule {
        divisor: 3,
        cap: 4,
        word_match: WordMatch::Plain,
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
        CoverageRule::M6
    }
}

/// Слова запроса, подготовленные один раз под правило (сервер проверяет десятки находок FTS).
#[derive(Debug, Clone)]
pub struct CoverageQuery {
    rule: CoverageRule,
    plain: Vec<String>,
    stems: Vec<StemWord>,
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

    /// Покрытие `text` достаточно ([`CoverageRule::need`]); пустой запрос — нет.
    pub fn enough(&self, text: &str) -> bool {
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
/// один блок) и блоки — по одному на подсказку (строка и, возможно, строки с отступом).
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
    for (si, sec) in sections.iter().enumerate().filter(|(_, s)| !s.reserved) {
        let mut opened = false;
        for (bi, block) in sec.blocks.iter().enumerate() {
            let head = match (&sec.title, opened) {
                (Some(t), false) => count(&format!("\n{t}\n")),
                _ => 0,
            };
            let cost = head + count(&format!("{block}\n"));
            if used + cost <= cap {
                used += cost;
                opened = true;
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

/// Текст вошедших блоков (см. [`pack_sections`]).
fn render(sections: &[Section], taken: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for &(si, bi) in taken {
        if last != Some(si) {
            if let Some(t) = &sections[si].title {
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
            word_match: WordMatch::Plain,
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
}
