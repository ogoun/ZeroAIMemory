//! Корпус и запросы eval (ТЗ §18.2): загрузка `eval/corpus/**/*.md` и `eval/queries.toml`,
//! проверка разметки и лексическая проверка «перефраз без общих слов».
//!
//! **Путь заметки** — путь файла относительно `eval/corpus/` без `.md`, через `/`
//! (`ru/git/rebase-conflicts`); первый компонент — язык (`ru` | `en`). Под тем же путём
//! заметка пишется в Knowledge, поэтому путь входит в `context_prefix` чанков (§8.2 п. 6)
//! и учитывается в лексической проверке.
//!
//! **Общие слова.** Слово — непрерывная последовательность букв и цифр в нижнем регистре
//! длиной от 3 символов, не из списка служебных слов. Два слова считаются одним, если
//! совпадают их первые `min(5, длина)` символов: так грубо склеиваются словоформы
//! («настройка» / «настроить», «config» / «configuration») и повторяется префиксный поиск
//! FTS5 (`"слово"*`). Запрос с `disjoint = true` не должен иметь ни одного общего слова
//! с текстом и путём каждой ожидаемой заметки — его не найдут ни bm25, ни trigram по
//! словам, только векторный канал.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Язык заметки или запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    fn from_dir(s: &str) -> Option<Lang> {
        match s {
            "ru" => Some(Lang::Ru),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// Вид запроса (§18.2): по нему метрики считаются отдельно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Пересказ своими словами.
    Paraphrase,
    /// Слова запроса взяты из заметки.
    Exact,
    /// Код ошибки, имя функции, флаг, аббревиатура.
    Identifier,
    /// Запрос на другом языке, чем ожидаемые заметки.
    CrossLang,
    /// Тема целиком: ожидается несколько заметок.
    Broad,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Paraphrase,
        Kind::Exact,
        Kind::Identifier,
        Kind::CrossLang,
        Kind::Broad,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Paraphrase => "paraphrase",
            Kind::Exact => "exact",
            Kind::Identifier => "identifier",
            Kind::CrossLang => "cross-lang",
            Kind::Broad => "broad",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Заметка корпуса.
#[derive(Debug, Clone)]
pub struct Note {
    /// `ru/git/rebase-conflicts` — путь в Knowledge.
    pub path: String,
    pub lang: Lang,
    pub text: String,
}

/// Корпус: заметки по пути (порядок путей стабилен — загрузка детерминирована).
#[derive(Debug, Default)]
pub struct Corpus {
    pub notes: BTreeMap<String, Note>,
}

/// Запрос с ожидаемыми заметками.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub id: String,
    pub text: String,
    pub lang: Lang,
    /// Пути заметок корпуса, которые должны найтись (релевантность бинарная).
    pub expect: Vec<String>,
    pub kind: Kind,
    /// Перефраз без общих слов с каждой ожидаемой заметкой (проверяется).
    #[serde(default)]
    pub disjoint: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryFile {
    #[serde(default)]
    query: Vec<Query>,
}

/// Предел размера заметки корпуса.
const NOTE_MAX_BYTES: usize = 64 * 1024;
/// Предел длины запроса (`/search`: 512 символов).
const QUERY_MAX_CHARS: usize = 512;
/// Слово короче не считается (как префиксный поиск FTS5 от 3 символов).
const MIN_WORD: usize = 3;
/// Длина «основы» слова для сравнения словоформ.
const STEM: usize = 5;

/// Требования к полному набору (§18.2 и задача 14 M2).
pub const MIN_NOTES: usize = 150;
pub const MIN_QUERIES: usize = 120;
/// Доля запросов-перефразов без общих слов.
pub const MIN_DISJOINT_SHARE: f64 = 0.20;
/// Минимум запросов каждого вида.
const MIN_PER_KIND: usize = 8;

/// Служебные слова русского и английского: в запросе не считаются общими.
const STOPWORDS: &[&str] = &[
    // en
    "the",
    "and",
    "for",
    "with",
    "are",
    "was",
    "were",
    "this",
    "that",
    "these",
    "those",
    "how",
    "what",
    "why",
    "when",
    "where",
    "which",
    "who",
    "whom",
    "does",
    "did",
    "can",
    "could",
    "should",
    "would",
    "will",
    "shall",
    "may",
    "might",
    "must",
    "not",
    "you",
    "your",
    "our",
    "their",
    "its",
    "from",
    "into",
    "onto",
    "about",
    "after",
    "before",
    "there",
    "here",
    "then",
    "than",
    "but",
    "all",
    "any",
    "some",
    "each",
    "other",
    "has",
    "have",
    "had",
    "been",
    "being",
    "via",
    "per",
    "out",
    "off",
    "over",
    "under",
    "between",
    "without",
    "within",
    "while",
    "also",
    "only",
    "just",
    "yet",
    "still",
    "very",
    "too",
    "own",
    "same",
    "such",
    "both",
    "either",
    "neither",
    "one",
    "two",
    "her",
    "his",
    "him",
    "she",
    "they",
    "them",
    "who",
    "whose",
    "doing",
    "done",
    "get",
    "gets",
    "got",
    "make",
    "makes",
    "made",
    "way",
    "ways",
    "thing",
    "things",
    // ru
    "как",
    "что",
    "чтобы",
    "это",
    "этот",
    "эта",
    "эти",
    "этой",
    "этом",
    "того",
    "тот",
    "для",
    "при",
    "или",
    "если",
    "где",
    "когда",
    "почему",
    "зачем",
    "какой",
    "какая",
    "какие",
    "каким",
    "какого",
    "чем",
    "его",
    "её",
    "она",
    "они",
    "оно",
    "мне",
    "меня",
    "нам",
    "нас",
    "вам",
    "вас",
    "так",
    "там",
    "тут",
    "уже",
    "ещё",
    "еще",
    "все",
    "всё",
    "всех",
    "без",
    "под",
    "над",
    "про",
    "через",
    "после",
    "перед",
    "между",
    "только",
    "можно",
    "нужно",
    "надо",
    "нельзя",
    "есть",
    "был",
    "была",
    "было",
    "были",
    "быть",
    "будет",
    "чего",
    "кто",
    "себя",
    "свой",
    "своё",
    "свою",
    "свои",
    "сам",
    "сама",
    "само",
    "этого",
    "этому",
    "тоже",
    "также",
    "даже",
    "лишь",
    "ли",
    "не",
    "ни",
    "из",
    "от",
    "до",
    "по",
    "за",
    "на",
    "во",
    "со",
    "об",
    "обо",
    "раз",
    "один",
    "одна",
    "одно",
    "два",
    "две",
    "делать",
    "сделать",
    "чтоб",
    "куда",
    "откуда",
];

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Слова текста для лексической проверки (см. модуль): нижний регистр, от 3 символов,
/// без служебных.
pub fn content_words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_WORD && !is_stopword(w))
        .map(str::to_string)
        .collect()
}

/// Одно ли это слово с точностью до словоформы: совпадают первые `min(5, длины)` символов.
pub fn same_word(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let k = STEM.min(a.len()).min(b.len());
    k >= MIN_WORD && a[..k] == b[..k]
}

/// Общие слова запроса и текста: пары (слово запроса, слово текста).
pub fn shared_words(query: &str, text: &str) -> Vec<(String, String)> {
    let note = content_words(text);
    let mut out = Vec::new();
    for q in content_words(query) {
        if let Some(n) = note.iter().find(|n| same_word(&q, n)) {
            out.push((q, n.clone()));
        }
    }
    out
}

/// Лексический материал заметки для проверки: путь и текст целиком (frontmatter с тегами
/// тоже — теги входят в `context_prefix`).
fn note_material(n: &Note) -> String {
    format!("{} {}", n.path, n.text)
}

/// Токен-идентификатор: код ошибки, имя функции, флаг, аббревиатура (`E0502`, `CRLF`,
/// `split_at_mut`, `--force-with-lease`, `HashMap`).
pub fn is_identifier(tok: &str) -> bool {
    let n = tok.chars().count();
    if n < 2 {
        return false;
    }
    let has_digit = tok.chars().any(|c| c.is_ascii_digit());
    let has_alpha = tok.chars().any(char::is_alphabetic);
    let punct = tok
        .chars()
        .any(|c| matches!(c, '_' | '.' | ':' | '/' | '-'));
    let upper_word = n >= 3
        && tok
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let camel = tok.chars().next().is_some_and(char::is_alphabetic)
        && tok.chars().any(char::is_lowercase)
        && tok.chars().skip(1).any(char::is_uppercase);
    (has_alpha && (has_digit || punct)) || upper_word || camel
}

/// Идентификаторы запроса (по пробелам, без обрамляющей пунктуации).
pub fn identifiers(text: &str) -> Vec<String> {
    text.split_whitespace()
        // кавычки и скобки вокруг токена; точка и двоеточие в конце — пунктуация фразы,
        // в начале — часть имени (`.gitignore`), дефисы флага (`--cached`) сохраняются
        .map(|t| t.trim_matches(|c: char| "\"'`()[]{},;!?«»".contains(c)))
        .map(|t| t.trim_end_matches(['.', ':']))
        .filter(|t| is_identifier(t))
        .map(str::to_string)
        .collect()
}

/// Допустимый путь заметки: `ru|en/<сегменты>` из `[a-z0-9-]`.
fn valid_note_path(p: &str) -> bool {
    let mut parts = p.split('/');
    let lang_ok = parts.next().and_then(Lang::from_dir).is_some();
    let rest: Vec<&str> = parts.collect();
    lang_ok
        && !rest.is_empty()
        && rest.iter().all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl Corpus {
    /// Загружает все `*.md` под `dir` (рекурсивно). Пути — относительно `dir`, без `.md`.
    pub fn load(dir: &Path) -> Result<Corpus> {
        let mut c = Corpus::default();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).with_context(|| format!("чтение {}", d.display()))? {
                let p = e?.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let rel = p
                    .strip_prefix(dir)
                    .expect("файл внутри каталога корпуса")
                    .with_extension("");
                let path = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                let text =
                    fs::read_to_string(&p).with_context(|| format!("чтение {}", p.display()))?;
                let lang = path
                    .split('/')
                    .next()
                    .and_then(Lang::from_dir)
                    .with_context(|| format!("{path}: первый каталог — ru или en"))?;
                c.notes.insert(path.clone(), Note { path, lang, text });
            }
        }
        Ok(c)
    }

    pub fn get(&self, path: &str) -> Option<&Note> {
        self.notes.get(path)
    }

    /// blake3-подобная сводка не нужна: стабильный отпечаток корпуса (FNV-1a 64 по путям и
    /// текстам) — для результатов, чтобы сравнивать прогоны на одном корпусе.
    pub fn fingerprint(&self) -> String {
        fnv1a(self.notes.values().flat_map(|n| {
            [n.path.as_bytes(), b"\0", n.text.as_bytes(), b"\0"]
                .into_iter()
                .flatten()
                .copied()
        }))
    }
}

/// FNV-1a 64 бит в hex.
pub fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Читает запросы из TOML (`[[query]]`).
pub fn load_queries(path: &Path) -> Result<Vec<Query>> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let f: QueryFile =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok(f.query)
}

/// Итог проверки набора.
#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub notes: usize,
    pub notes_ru: usize,
    pub notes_en: usize,
    pub queries: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub disjoint: usize,
    /// Заметки, которых не ждёт ни один запрос (дистракторы).
    pub unreferenced: usize,
}

impl Summary {
    pub fn disjoint_share(&self) -> f64 {
        if self.queries == 0 {
            0.0
        } else {
            self.disjoint as f64 / self.queries as f64
        }
    }
}

/// Проверяет запросы против корпуса. `full` — ещё и требования к полному набору (объём,
/// доли, виды); без него — только разметка каждого запроса (фрагмент при написании).
/// Все нарушения собираются в одну ошибку.
pub fn validate(corpus: &Corpus, queries: &[Query], full: bool) -> Result<Summary> {
    let mut errs: Vec<String> = Vec::new();
    for n in corpus.notes.values() {
        if !valid_note_path(&n.path) {
            errs.push(format!(
                "заметка {}: путь — ru|en/<сегменты [a-z0-9-]>",
                n.path
            ));
        }
        if n.text.len() > NOTE_MAX_BYTES {
            errs.push(format!("заметка {}: больше {NOTE_MAX_BYTES} байт", n.path));
        }
        if n.text.contains('\r') {
            errs.push(format!(
                "заметка {}: переводы строк CRLF (нужен LF)",
                n.path
            ));
        }
        let body = n.text.trim_start();
        if !(body.starts_with("# ") || body.starts_with("---")) {
            errs.push(format!(
                "заметка {}: начинается с заголовка `# ` или frontmatter",
                n.path
            ));
        }
    }
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    let mut s = Summary {
        notes: corpus.notes.len(),
        notes_ru: corpus.notes.values().filter(|n| n.lang == Lang::Ru).count(),
        notes_en: corpus.notes.values().filter(|n| n.lang == Lang::En).count(),
        queries: queries.len(),
        ..Summary::default()
    };
    let mut referenced: BTreeSet<&str> = BTreeSet::new();
    for q in queries {
        let mut bad = |m: String| errs.push(format!("запрос {}: {m}", q.id));
        if !ids.insert(&q.id) {
            bad("id повторяется".into());
        }
        let len = q.text.chars().count();
        if q.text.trim().is_empty() || len > QUERY_MAX_CHARS {
            bad(format!("текст пуст или длиннее {QUERY_MAX_CHARS} символов"));
        }
        if q.expect.is_empty() {
            bad("пустой expect".into());
        }
        let mut notes: Vec<&Note> = Vec::new();
        for p in &q.expect {
            match corpus.get(p) {
                Some(n) => notes.push(n),
                None => bad(format!("нет заметки {p}")),
            }
            referenced.insert(p.as_str());
        }
        *s.by_kind.entry(q.kind.as_str().to_string()).or_default() += 1;
        match q.kind {
            Kind::CrossLang => {
                if let Some(n) = notes.iter().find(|n| n.lang == q.lang) {
                    bad(format!(
                        "cross-lang, но заметка {} на языке запроса",
                        n.path
                    ));
                }
            }
            Kind::Broad => {
                if q.expect.len() < 2 {
                    bad("broad ожидает не меньше двух заметок".into());
                }
            }
            Kind::Identifier => {
                let toks = identifiers(&q.text);
                if toks.is_empty() {
                    bad("identifier без идентификатора в тексте".into());
                }
                for n in &notes {
                    let low = n.text.to_lowercase();
                    if !toks.iter().any(|t| low.contains(&t.to_lowercase())) {
                        bad(format!(
                            "ни один идентификатор {toks:?} не встречается в {}",
                            n.path
                        ));
                    }
                }
            }
            Kind::Exact => {
                let words = content_words(&q.text);
                for n in &notes {
                    let shared = shared_words(&q.text, &note_material(n)).len();
                    if words.is_empty() || shared * 5 < words.len() * 3 {
                        bad(format!(
                            "exact: с {} общих слов {shared} из {} (нужно ≥ 60 %)",
                            n.path,
                            words.len()
                        ));
                    }
                }
            }
            Kind::Paraphrase => {}
        }
        // broad может собирать тему на обоих языках
        if !matches!(q.kind, Kind::CrossLang | Kind::Broad)
            && let Some(n) = notes.iter().find(|n| n.lang != q.lang)
        {
            bad(format!(
                "язык запроса не совпадает с заметкой {} — это cross-lang",
                n.path
            ));
        }
        if q.disjoint {
            if q.kind != Kind::Paraphrase {
                bad("disjoint = true допустимо только у paraphrase".into());
            }
            for n in &notes {
                let shared = shared_words(&q.text, &note_material(n));
                if !shared.is_empty() {
                    bad(format!("disjoint, но общие слова с {}: {shared:?}", n.path));
                }
            }
            s.disjoint += 1;
        }
    }
    s.unreferenced = corpus
        .notes
        .keys()
        .filter(|p| !referenced.contains(p.as_str()))
        .count();
    if full {
        if s.notes < MIN_NOTES {
            errs.push(format!("заметок {} < {MIN_NOTES}", s.notes));
        }
        if s.notes_ru * 3 < s.notes || s.notes_en * 3 < s.notes {
            errs.push(format!(
                "языки несбалансированы: ru {}, en {}",
                s.notes_ru, s.notes_en
            ));
        }
        if s.queries < MIN_QUERIES {
            errs.push(format!("запросов {} < {MIN_QUERIES}", s.queries));
        }
        if s.disjoint_share() < MIN_DISJOINT_SHARE {
            errs.push(format!(
                "перефразов без общих слов {} из {} ({:.0} %) < {:.0} %",
                s.disjoint,
                s.queries,
                s.disjoint_share() * 100.0,
                MIN_DISJOINT_SHARE * 100.0
            ));
        }
        for k in Kind::ALL {
            let n = s.by_kind.get(k.as_str()).copied().unwrap_or(0);
            if n < MIN_PER_KIND {
                errs.push(format!("запросов вида {k}: {n} < {MIN_PER_KIND}"));
            }
        }
        for lang in [Lang::Ru, Lang::En] {
            if !queries.iter().any(|q| q.lang == lang) {
                errs.push(format!("нет запросов на {lang:?}"));
            }
        }
    }
    if !errs.is_empty() {
        bail!(
            "набор eval не прошёл проверку ({} нарушений):\n  {}",
            errs.len(),
            errs.join("\n  ")
        );
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn word_forms_are_one_word() {
        assert!(same_word("настройка", "настроить"));
        assert!(same_word("config", "configuration"));
        assert!(same_word("git", "github"));
        assert!(same_word("файл", "файлы"));
        assert!(!same_word("commit", "common"));
        assert!(!same_word("run", "rust"));
        assert_eq!(
            shared_words(
                "How do plants feed on sunlight?",
                "Photosynthesis in plants"
            ),
            [("plants".to_string(), "plants".to_string())]
        );
        // служебные слова и слова короче 3 символов не считаются
        assert!(shared_words("how is it in the ok", "the ok how it is").is_empty());
    }

    #[test]
    fn identifiers_are_detected() {
        assert_eq!(
            identifiers("ошибка E0502 при split_at_mut, флаг --force-with-lease и CRLF"),
            ["E0502", "split_at_mut", "--force-with-lease", "CRLF"]
        );
        assert_eq!(
            identifiers("HashMap vs BTreeMap in std"),
            ["HashMap", "BTreeMap"]
        );
        assert!(identifiers("обычный вопрос про git").is_empty());
    }

    fn note(path: &str, text: &str) -> Note {
        Note {
            path: path.into(),
            lang: if path.starts_with("ru") {
                Lang::Ru
            } else {
                Lang::En
            },
            text: text.into(),
        }
    }

    fn q(id: &str, text: &str, lang: Lang, expect: &[&str], kind: Kind, disjoint: bool) -> Query {
        Query {
            id: id.into(),
            text: text.into(),
            lang,
            expect: expect.iter().map(|s| s.to_string()).collect(),
            kind,
            disjoint,
        }
    }

    #[test]
    fn validation_catches_bad_markup() {
        let mut c = Corpus::default();
        for n in [
            note(
                "en/bio/photosynthesis",
                "# Photosynthesis\n\nPlants convert sunlight into sugar.\n",
            ),
            note(
                "ru/git/rebase",
                "# Rebase\n\nПеребазирование ветки E0502 на main.\n",
            ),
        ] {
            c.notes.insert(n.path.clone(), n);
        }
        let ok = [
            q(
                "a",
                "how greenery feeds itself on daylight",
                Lang::En,
                &["en/bio/photosynthesis"],
                Kind::Paraphrase,
                true,
            ),
            q(
                "b",
                "ошибка E0502",
                Lang::Ru,
                &["ru/git/rebase"],
                Kind::Identifier,
                false,
            ),
            q(
                "c",
                "convert sunlight",
                Lang::Ru,
                &["en/bio/photosynthesis"],
                Kind::CrossLang,
                false,
            ),
        ];
        let s = validate(&c, &ok, false).unwrap();
        assert_eq!((s.disjoint, s.queries), (1, 3));
        let bad = [
            // общее слово «plants»
            q(
                "d",
                "what plants eat",
                Lang::En,
                &["en/bio/photosynthesis"],
                Kind::Paraphrase,
                true,
            ),
            q("e", "x", Lang::En, &["en/nope"], Kind::Exact, false),
            q(
                "f",
                "rebase",
                Lang::Ru,
                &["ru/git/rebase"],
                Kind::Broad,
                false,
            ),
            q(
                "g",
                "перебазирование",
                Lang::Ru,
                &["ru/git/rebase"],
                Kind::CrossLang,
                false,
            ),
            q(
                "h",
                "без кода",
                Lang::Ru,
                &["ru/git/rebase"],
                Kind::Identifier,
                false,
            ),
        ];
        let err = validate(&c, &bad, false).unwrap_err().to_string();
        for needle in ["запрос d", "en/nope", "broad", "запрос g", "запрос h"] {
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }

    /// Полный набор `eval/` проходит проверку: объём, доли, разметка, ≥ 20 % перефразов
    /// без общих слов (по словоформам, см. модуль).
    #[test]
    fn eval_set_is_valid() {
        let root = repo().join("eval");
        let corpus = Corpus::load(&root.join("corpus")).unwrap();
        let queries = load_queries(&root.join("queries.toml")).unwrap();
        let s = validate(&corpus, &queries, true).unwrap();
        assert!(s.disjoint_share() >= MIN_DISJOINT_SHARE);
    }
}
