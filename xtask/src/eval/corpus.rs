//! Корпус и запросы eval (ТЗ §18.2): загрузка `eval/corpus/**/*.md` и `eval/queries.toml`,
//! проверка разметки и лексическая проверка «перефраз без общих слов».
//!
//! **Путь заметки** — путь файла относительно `eval/corpus/` без `.md`, через `/`
//! (`ru/git/rebase-conflicts`); первый компонент — язык (`ru` | `en`). Под тем же путём
//! заметка пишется в Knowledge, поэтому путь входит в `context_prefix` чанков (§8.2 п. 6)
//! и учитывается в лексической проверке.
//!
//! **Общие слова.** Слово — непрерывная последовательность букв и цифр в нижнем регистре
//! длиной от 2 символов, не из списка служебных слов. Два слова считаются одним, если
//! совпадают их основы Snowball (`rust-stemmers`: русский для кириллицы, английский для
//! латиницы) или — у слов от 3 символов — первые `min(5, длина)` символов: префикс повторяет
//! префиксный поиск FTS5 (`"слово"*`) и ловит то, что стеммер разводит («настройка» /
//! «настроить», «config» / «configuration»); слова из 2 символов — только целиком или по
//! основе. Запрос с `disjoint = true` не должен иметь ни одного общего слова
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
    /// Устаревание (M3): ожидается действующий insight, `forbid` — замещённые или отозванные.
    Superseded,
    /// Надёжность (M3): ожидается `theorem`/`axiom`, `weaker` — противоречащая `hypothesis`.
    Certainty,
    /// Факт из середины длинной заметки (M3): `evidence` — дословное доказательство.
    Chunk,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Paraphrase,
        Kind::Exact,
        Kind::Identifier,
        Kind::CrossLang,
        Kind::Broad,
        Kind::Superseded,
        Kind::Certainty,
        Kind::Chunk,
    ];
    /// Виды M2: поиск заметок; к ним относятся минимум по виду и доля перефразов без общих
    /// слов.
    pub const M2: [Kind; 5] = [
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
            Kind::Superseded => "superseded",
            Kind::Certainty => "certainty",
            Kind::Chunk => "chunk",
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

impl Note {
    /// Длинная заметка набора M3: `<язык>/long/<имя>`.
    pub fn is_long(&self) -> bool {
        is_long_path(&self.path)
    }

    /// `index: false` во frontmatter: заметка не векторизуется, находит её только FTS.
    pub fn index_disabled(&self) -> bool {
        let Some(rest) = self.text.strip_prefix("---\n") else {
            return false;
        };
        let fm = rest.split("\n---").next().unwrap_or("");
        fm.lines()
            .filter_map(|l| l.split_once(':'))
            .any(|(k, v)| k == "index" && v.trim() == "false")
    }

    /// Число слов (по пробельным символам).
    pub fn words(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// Путь длинной заметки: второй компонент — `long`.
pub fn is_long_path(path: &str) -> bool {
    path.split('/').nth(1) == Some("long")
}

/// Заметки группы `e` (документы проекта, `<язык>/zam/…`) пишутся в пространство решения
/// набора, остальные — в Knowledge.
pub fn in_solution(path: &str) -> bool {
    path.split('/').nth(1) == Some("zam")
}

/// GUID решения набора eval: в него пишутся заметки группы `e` и insight `scope = solution`;
/// поиск идёт с `scope.solution` = этот GUID.
pub const EVAL_SOLUTION: &str = "5e7a1e00-2026-4e3a-9a11-000000000e01";

/// Префикс ссылки на insight в `expect`, `forbid`, `weaker`: `insight:<key>`.
pub const INSIGHT_REF: &str = "insight:";

/// Корпус: заметки по пути (порядок путей стабилен — загрузка детерминирована) и insight
/// набора M3 в порядке файла (замещаемая запись определена раньше замещающей).
#[derive(Debug, Default)]
pub struct Corpus {
    pub notes: BTreeMap<String, Note>,
    pub insights: Vec<Insight>,
}

/// Запрос с ожидаемыми заметками или insight.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub id: String,
    pub text: String,
    pub lang: Lang,
    /// Пути заметок корпуса или `insight:<key>`, которые должны найтись (релевантность
    /// бинарная).
    pub expect: Vec<String>,
    pub kind: Kind,
    /// Перефраз без общих слов с каждой ожидаемой заметкой (проверяется).
    #[serde(default)]
    pub disjoint: bool,
    /// `superseded`: замещённые или отозванные insight, которых не должно быть в top-5.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbid: Vec<String>,
    /// `certainty`: противоречащие гипотезы, которые должны стоять ниже `expect`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weaker: Vec<String>,
    /// `chunk`: дословная подстрока ожидаемой заметки — лист должен содержать её целиком.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryFile {
    #[serde(default)]
    query: Vec<Query>,
}

/// Доказательство `theorem` (`data.evidence` записи).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub checked: String,
    pub method: String,
    pub result: String,
}

impl Evidence {
    fn is_complete(&self) -> bool {
        [&self.checked, &self.method, &self.result]
            .iter()
            .all(|s| !s.trim().is_empty())
    }
}

/// Insight набора M3 (`eval/insights.toml`, `[[insight]]`): загрузчик создаёт его
/// `POST /spaces/{решение}/entries`, замещает (`supersedes`) и отзывает (`retract_reason`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Insight {
    /// Ключ набора: `insight:<key>` в запросах.
    pub key: String,
    pub lang: Lang,
    /// `data.kind`: `gotcha | failure | discovery | decision | experiment | pattern`.
    pub kind: String,
    /// `solution` (пространство решения набора) | `global` (Knowledge).
    pub scope: String,
    #[serde(default)]
    pub area: Option<String>,
    /// `axiom` (создаёт человеческий токен) | `theorem` (с `evidence`) | `hypothesis`; без
    /// поля — статус по умолчанию агента (`hypothesis`).
    #[serde(default)]
    pub certainty: Option<String>,
    #[serde(default)]
    pub evidence: Option<Evidence>,
    pub title: String,
    pub body: String,
    /// Триггеры как в API (`{type, pattern, tools?}`).
    #[serde(default)]
    pub triggers: Vec<serde_json::Value>,
    /// Ключ insight, который этот замещает (`POST /entries/{id}/supersede`).
    #[serde(default)]
    pub supersedes: Option<String>,
    /// Отзыв после создания (`POST /entries/{id}/retract`).
    #[serde(default)]
    pub retract_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InsightFile {
    #[serde(default)]
    insight: Vec<Insight>,
}

/// Виды insight (`data.kind`, как в API).
const INSIGHT_KINDS: [&str; 6] = [
    "gotcha",
    "failure",
    "discovery",
    "decision",
    "experiment",
    "pattern",
];
const CERTAINTIES: [&str; 3] = ["axiom", "theorem", "hypothesis"];

impl Corpus {
    /// Insight по ключу.
    pub fn insight(&self, key: &str) -> Option<&Insight> {
        self.insights.iter().find(|i| i.key == key)
    }

    /// Ключи insight, замещённых другими.
    pub fn superseded_keys(&self) -> BTreeSet<&str> {
        self.insights
            .iter()
            .filter_map(|i| i.supersedes.as_deref())
            .collect()
    }

    /// Insight действует (не замещён и не отозван).
    pub fn insight_active(&self, key: &str) -> bool {
        self.insight(key)
            .is_some_and(|i| i.retract_reason.is_none())
            && !self.superseded_keys().contains(key)
    }

    /// Итоговый статус insight по умолчанию агента: явный или `hypothesis`.
    pub fn insight_certainty(&self, key: &str) -> Option<&str> {
        self.insight(key)
            .map(|i| i.certainty.as_deref().unwrap_or("hypothesis"))
    }
}

/// Читает insight из TOML (`[[insight]]`); нет файла — пусто.
pub fn load_insights(path: &Path) -> Result<Vec<Insight>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let f: InsightFile =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok(f.insight)
}

/// Предел размера заметки корпуса.
const NOTE_MAX_BYTES: usize = 64 * 1024;
/// Предел длины запроса (`/search`: 512 символов).
const QUERY_MAX_CHARS: usize = 512;
/// Слово короче не считается.
const MIN_WORD: usize = 2;
/// Префиксное сравнение — для слов от этой длины (как префиксный поиск FTS5).
const MIN_PREFIX: usize = 3;
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
    // служебные слова из 2 символов (слова от 2 символов тоже сравниваются)
    "in",
    "on",
    "to",
    "of",
    "is",
    "it",
    "at",
    "as",
    "by",
    "or",
    "an",
    "be",
    "do",
    "if",
    "no",
    "so",
    "up",
    "we",
    "my",
    "me",
    "us",
    "am",
    "he",
    "vs",
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
    // служебные слова из 2 символов (слова от 2 символов тоже сравниваются)
    "то",
    "же",
    "бы",
    "мы",
    "вы",
    "их",
    "ей",
    "им",
    "он",
    "да",
    "ну",
    "уж",
    "ее",
    "ты",
    // местоимения: в перефразе «мой код» не делает слово общим
    "мой",
    "моя",
    "моё",
    "мое",
    "мои",
    "моего",
    "моей",
    "моих",
    "твой",
    "наш",
    "ваш",
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

/// Основа Snowball: русский стеммер для слов с кириллицей, английский — для остальных.
pub fn stem(w: &str) -> String {
    use rust_stemmers::{Algorithm, Stemmer};
    use std::sync::OnceLock;
    static RU: OnceLock<Stemmer> = OnceLock::new();
    static EN: OnceLock<Stemmer> = OnceLock::new();
    let cyr = w.chars().any(|c| matches!(c, 'а'..='я' | 'ё'));
    let s = if cyr {
        RU.get_or_init(|| Stemmer::create(Algorithm::Russian))
    } else {
        EN.get_or_init(|| Stemmer::create(Algorithm::English))
    };
    s.stem(w).into_owned()
}

/// Одно ли это слово с точностью до словоформы: совпали основы Snowball или (у слов от
/// 3 символов) первые `min(5, длины)` символов.
#[cfg(test)]
fn same_word(a: &str, b: &str) -> bool {
    same_word_stemmed(a, &stem(a), b, &stem(b))
}

fn same_word_stemmed(a: &str, sa: &str, b: &str, sb: &str) -> bool {
    if a == b || sa == sb {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let k = STEM.min(a.len()).min(b.len());
    k >= MIN_PREFIX && a[..k] == b[..k]
}

/// Общие слова запроса и текста: пары (слово запроса, слово текста).
pub fn shared_words(query: &str, text: &str) -> Vec<(String, String)> {
    let note: Vec<(String, String)> = content_words(text)
        .into_iter()
        .map(|w| {
            let s = stem(&w);
            (w, s)
        })
        .collect();
    let mut out = Vec::new();
    for q in content_words(query) {
        let sq = stem(&q);
        if let Some((n, _)) = note
            .iter()
            .find(|(n, sn)| same_word_stemmed(&q, &sq, n, sn))
        {
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
    ///
    /// Insight набора M3 входят в отпечаток (JSON каждого в порядке файла); без них отпечаток
    /// совпадает с отпечатком M2 того же набора заметок.
    pub fn fingerprint(&self) -> String {
        let insights: Vec<u8> = self
            .insights
            .iter()
            .flat_map(|i| {
                let mut v = serde_json::to_vec(i).expect("insight сериализуется");
                v.push(0);
                v
            })
            .collect();
        fnv1a(
            self.notes
                .values()
                .flat_map(|n| {
                    [n.path.as_bytes(), b"\0", n.text.as_bytes(), b"\0"]
                        .into_iter()
                        .flatten()
                        .copied()
                })
                .chain(insights),
        )
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
    /// Запросы видов M2 ([`Kind::M2`]) — знаменатель доли перефразов без общих слов.
    pub queries_m2: usize,
    /// Заметки, которых не ждёт ни один запрос (дистракторы).
    pub unreferenced: usize,
    /// Длинные заметки (`<язык>/long/…`).
    pub long_notes: usize,
    /// Заметки с `index: false` (находит только FTS).
    pub fts_only_notes: usize,
    pub insights: usize,
    /// Цепочки замещения (корни, у которых есть замещающая запись).
    pub chains: usize,
    pub retracted: usize,
    /// Пары противоречий «надёжное — гипотеза» из запросов `certainty`.
    pub contradictions: usize,
}

impl Summary {
    /// Доля перефразов без общих слов среди запросов видов M2 **[решение исполнителя]**:
    /// запросы M3 (устаревание, надёжность, факты длинных заметок) меряют другое и долю
    /// не размывают.
    pub fn disjoint_share(&self) -> f64 {
        if self.queries_m2 == 0 {
            0.0
        } else {
            self.disjoint as f64 / self.queries_m2 as f64
        }
    }
}

/// Требования к набору M3 (задача 16 M3).
pub const MIN_SUPERSEDED: usize = 30;
pub const MIN_CERTAINTY: usize = 12;
pub const CHUNK_QUERIES: std::ops::RangeInclusive<usize> = 40..=60;
pub const LONG_NOTES: std::ops::RangeInclusive<usize> = 20..=30;
pub const LONG_WORDS: std::ops::RangeInclusive<usize> = 2000..=5000;
pub const MIN_FTS_ONLY: usize = 10;
pub const MIN_INSIGHTS: usize = 48;
pub const MIN_CHAINS: usize = 12;
pub const MIN_RETRACTED: usize = 4;
pub const MIN_CONTRADICTIONS: usize = 12;
/// `evidence` запроса `chunk` — из середины заметки: начало не раньше этой доли текста,
/// конец не позже `1 − доля`.
const EVIDENCE_MARGIN: f64 = 0.10;
/// Длина `evidence`, символов.
const EVIDENCE_CHARS: std::ops::RangeInclusive<usize> = 40..=600;

/// Ключ insight: `[a-z0-9-]`, непустой.
fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Пробелы и переводы строк — одним пробелом (сравнение `evidence` с текстом листа).
pub fn squash_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Проверка insight набора: ключи, виды, статусы, доказательства, цепочки замещения без
/// циклов (замещаемая запись определена раньше), отзывы.
fn validate_insights(corpus: &Corpus, errs: &mut Vec<String>) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut successor: BTreeMap<&str, &str> = BTreeMap::new();
    for (idx, i) in corpus.insights.iter().enumerate() {
        let mut bad = |m: String| errs.push(format!("insight {}: {m}", i.key));
        if !valid_key(&i.key) {
            bad("ключ — [a-z0-9-]".into());
        }
        if seen.insert(&i.key, idx).is_some() {
            bad("ключ повторяется".into());
        }
        if !INSIGHT_KINDS.contains(&i.kind.as_str()) {
            bad(format!("kind — одно из {}", INSIGHT_KINDS.join(", ")));
        }
        if !matches!(i.scope.as_str(), "solution" | "global") {
            bad("scope — solution или global".into());
        }
        let title = i.title.trim().chars().count();
        if title == 0 || title > 500 || i.body.trim().is_empty() {
            bad("заголовок 1–500 символов и непустое тело".into());
        }
        if i.body.contains('\r') || i.title.contains('\r') {
            bad("переводы строк CRLF".into());
        }
        match i.certainty.as_deref() {
            None | Some("hypothesis") | Some("axiom") => {}
            Some("theorem") => {
                if !i.evidence.as_ref().is_some_and(Evidence::is_complete) {
                    bad("theorem без evidence {checked, method, result}".into());
                }
            }
            Some(c) => bad(format!("certainty «{c}»: {}", CERTAINTIES.join(" | "))),
        }
        if i.retract_reason
            .as_ref()
            .is_some_and(|r| r.trim().is_empty())
        {
            bad("пустая retract_reason".into());
        }
        if let Some(old) = &i.supersedes {
            match seen.get(old.as_str()) {
                Some(&j) if j < idx => {
                    let o = &corpus.insights[j];
                    if o.retract_reason.is_some() {
                        bad(format!("замещает отозванный {old}"));
                    }
                    if o.certainty.as_deref() == Some("axiom")
                        || i.certainty.as_deref() == Some("axiom")
                    {
                        bad("цепочки замещения без axiom (axiom меняет только человек)".into());
                    }
                    if o.scope != i.scope {
                        bad(format!("scope отличается от замещаемого {old}"));
                    }
                }
                _ => bad(format!(
                    "supersedes {old}: нет insight с таким ключом выше по файлу"
                )),
            }
            if let Some(prev) = successor.insert(old, &i.key) {
                bad(format!("{old} уже замещён записью {prev}"));
            }
        }
    }
    // циклы: путь по замещениям от любой записи конечен (при «выше по файлу» их нет, но
    // проверка независима от порядка)
    let by_key: BTreeMap<&str, &Insight> = corpus
        .insights
        .iter()
        .map(|i| (i.key.as_str(), i))
        .collect();
    for i in &corpus.insights {
        let mut cur = i;
        let mut steps = 0;
        while let Some(next) = cur.supersedes.as_deref().and_then(|k| by_key.get(k)) {
            steps += 1;
            if steps > corpus.insights.len() {
                errs.push(format!("insight {}: цикл замещений", i.key));
                break;
            }
            cur = next;
        }
    }
}

/// Цель запроса: заметка корпуса или insight.
enum Target<'a> {
    Note(&'a Note),
    Insight(&'a Insight),
}

impl Target<'_> {
    fn lang(&self) -> Lang {
        match self {
            Target::Note(n) => n.lang,
            Target::Insight(i) => i.lang,
        }
    }

    fn name(&self) -> String {
        match self {
            Target::Note(n) => n.path.clone(),
            Target::Insight(i) => format!("{INSIGHT_REF}{}", i.key),
        }
    }
}

fn resolve<'a>(corpus: &'a Corpus, r: &str) -> Option<Target<'a>> {
    match r.strip_prefix(INSIGHT_REF) {
        Some(k) => corpus.insight(k).map(Target::Insight),
        None => corpus.get(r).map(Target::Note),
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
        if n.is_long() {
            if !LONG_WORDS.contains(&n.words()) {
                errs.push(format!(
                    "длинная заметка {}: {} слов, нужно {}–{}",
                    n.path,
                    n.words(),
                    LONG_WORDS.start(),
                    LONG_WORDS.end()
                ));
            }
            if !n.text.lines().any(|l| l.starts_with("#### ")) {
                errs.push(format!("длинная заметка {}: нет заголовка H4", n.path));
            }
        }
    }
    validate_insights(corpus, &mut errs);
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    let mut s = Summary {
        notes: corpus.notes.len(),
        notes_ru: corpus.notes.values().filter(|n| n.lang == Lang::Ru).count(),
        notes_en: corpus.notes.values().filter(|n| n.lang == Lang::En).count(),
        queries: queries.len(),
        queries_m2: queries
            .iter()
            .filter(|q| Kind::M2.contains(&q.kind))
            .count(),
        long_notes: corpus.notes.values().filter(|n| n.is_long()).count(),
        fts_only_notes: corpus.notes.values().filter(|n| n.index_disabled()).count(),
        insights: corpus.insights.len(),
        chains: {
            let sup = corpus.superseded_keys();
            corpus
                .insights
                .iter()
                .filter(|i| i.supersedes.is_none() && sup.contains(i.key.as_str()))
                .count()
        },
        retracted: corpus
            .insights
            .iter()
            .filter(|i| i.retract_reason.is_some())
            .count(),
        ..Summary::default()
    };
    let mut referenced: BTreeSet<&str> = BTreeSet::new();
    let mut exact_targets: BTreeSet<&str> = BTreeSet::new();
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
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
        let mut targets: Vec<Target> = Vec::new();
        for p in &q.expect {
            match resolve(corpus, p) {
                Some(t) => targets.push(t),
                None => bad(format!("нет заметки или insight {p}")),
            }
            referenced.insert(p.as_str());
        }
        let notes: Vec<&Note> = targets
            .iter()
            .filter_map(|t| match t {
                Target::Note(n) => Some(*n),
                Target::Insight(_) => None,
            })
            .collect();
        *s.by_kind.entry(q.kind.as_str().to_string()).or_default() += 1;
        if !q.forbid.is_empty() && q.kind != Kind::Superseded {
            bad("forbid — только у superseded".into());
        }
        if !q.weaker.is_empty() && q.kind != Kind::Certainty {
            bad("weaker — только у certainty".into());
        }
        if q.evidence.is_some() && q.kind != Kind::Chunk {
            bad("evidence — только у chunk".into());
        }
        if Kind::M2.contains(&q.kind) && notes.len() != targets.len() {
            bad("запросы видов M2 ждут только заметки".into());
        }
        match q.kind {
            Kind::CrossLang => {
                if let Some(t) = targets.iter().find(|t| t.lang() == q.lang) {
                    bad(format!(
                        "cross-lang, но заметка {} на языке запроса",
                        t.name()
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
                    exact_targets.insert(n.path.as_str());
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
            Kind::Superseded => {
                if q.forbid.is_empty() {
                    bad("superseded без forbid".into());
                }
                for t in &targets {
                    if let Target::Insight(i) = t
                        && !corpus.insight_active(&i.key)
                    {
                        bad(format!("expect {}: замещён или отозван", t.name()));
                    }
                }
                for f in &q.forbid {
                    match resolve(corpus, f) {
                        Some(Target::Insight(i)) if !corpus.insight_active(&i.key) => {
                            if i.lang != q.lang {
                                bad(format!("язык forbid {f} отличается от запроса"));
                            }
                        }
                        Some(Target::Insight(_)) => bad(format!("forbid {f}: действующий insight")),
                        _ => bad(format!("forbid {f}: нет такого insight")),
                    }
                    if q.expect.contains(f) {
                        bad(format!("{f} и в expect, и в forbid"));
                    }
                }
            }
            Kind::Certainty => {
                if q.weaker.is_empty() {
                    bad("certainty без weaker".into());
                }
                for t in &targets {
                    match t {
                        Target::Insight(i)
                            if matches!(
                                corpus.insight_certainty(&i.key),
                                Some("theorem" | "axiom")
                            ) && corpus.insight_active(&i.key) => {}
                        _ => bad(format!(
                            "expect {}: ожидается действующий insight theorem или axiom",
                            t.name()
                        )),
                    }
                }
                for w in &q.weaker {
                    match resolve(corpus, w) {
                        Some(Target::Insight(i))
                            if corpus.insight_certainty(&i.key) == Some("hypothesis")
                                && corpus.insight_active(&i.key) =>
                        {
                            if i.lang != q.lang {
                                bad(format!("язык weaker {w} отличается от запроса"));
                            }
                            for e in &q.expect {
                                pairs.insert((e.clone(), w.clone()));
                            }
                        }
                        _ => bad(format!(
                            "weaker {w}: ожидается действующий insight hypothesis"
                        )),
                    }
                }
            }
            Kind::Chunk => {
                if q.expect.len() != 1 || notes.len() != 1 {
                    bad("chunk ожидает ровно одну заметку".into());
                }
                match (&q.evidence, notes.first()) {
                    (None, _) => bad("chunk без evidence".into()),
                    (Some(ev), Some(n)) => {
                        if !n.is_long() {
                            bad(format!("chunk: {} — не длинная заметка", n.path));
                        }
                        let chars = ev.chars().count();
                        if !EVIDENCE_CHARS.contains(&chars) {
                            bad(format!(
                                "evidence {chars} символов, нужно {}–{}",
                                EVIDENCE_CHARS.start(),
                                EVIDENCE_CHARS.end()
                            ));
                        }
                        match n.text.find(ev.as_str()) {
                            None => bad(format!("evidence нет дословно в {}", n.path)),
                            Some(at) => {
                                let total = n.text.len() as f64;
                                let (a, b) = (at as f64 / total, (at + ev.len()) as f64 / total);
                                if a < EVIDENCE_MARGIN || b > 1.0 - EVIDENCE_MARGIN {
                                    bad(format!(
                                        "evidence не из середины {} ({:.0}–{:.0} % текста)",
                                        n.path,
                                        a * 100.0,
                                        b * 100.0
                                    ));
                                }
                            }
                        }
                        if ev.contains("\n#") || ev.starts_with('#') {
                            bad("evidence захватывает строку заголовка".into());
                        }
                    }
                    (Some(_), None) => {}
                }
            }
        }
        // broad может собирать тему на обоих языках
        if !matches!(q.kind, Kind::CrossLang | Kind::Broad)
            && let Some(t) = targets.iter().find(|t| t.lang() != q.lang)
        {
            bad(format!(
                "язык запроса не совпадает с {} — это cross-lang",
                t.name()
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
    s.contradictions = pairs.len();
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
                "перефразов без общих слов {} из {} запросов видов M2 ({:.0} %) < {:.0} %",
                s.disjoint,
                s.queries_m2,
                s.disjoint_share() * 100.0,
                MIN_DISJOINT_SHARE * 100.0
            ));
        }
        let count = |k: Kind| s.by_kind.get(k.as_str()).copied().unwrap_or(0);
        for k in Kind::M2 {
            if count(k) < MIN_PER_KIND {
                errs.push(format!("запросов вида {k}: {} < {MIN_PER_KIND}", count(k)));
            }
        }
        if count(Kind::Superseded) < MIN_SUPERSEDED {
            errs.push(format!(
                "запросов superseded {} < {MIN_SUPERSEDED}",
                count(Kind::Superseded)
            ));
        }
        if count(Kind::Certainty) < MIN_CERTAINTY {
            errs.push(format!(
                "запросов certainty {} < {MIN_CERTAINTY}",
                count(Kind::Certainty)
            ));
        }
        if !CHUNK_QUERIES.contains(&count(Kind::Chunk)) {
            errs.push(format!(
                "запросов chunk {}, нужно {}–{}",
                count(Kind::Chunk),
                CHUNK_QUERIES.start(),
                CHUNK_QUERIES.end()
            ));
        }
        if !LONG_NOTES.contains(&s.long_notes) {
            errs.push(format!(
                "длинных заметок {}, нужно {}–{}",
                s.long_notes,
                LONG_NOTES.start(),
                LONG_NOTES.end()
            ));
        }
        for lang in [Lang::Ru, Lang::En] {
            if !corpus.notes.values().any(|n| n.is_long() && n.lang == lang) {
                errs.push(format!("нет длинных заметок на {lang:?}"));
            }
        }
        if s.fts_only_notes < MIN_FTS_ONLY {
            errs.push(format!(
                "заметок с index: false {} < {MIN_FTS_ONLY}",
                s.fts_only_notes
            ));
        }
        for n in corpus.notes.values().filter(|n| n.index_disabled()) {
            if !exact_targets.contains(n.path.as_str()) {
                errs.push(format!(
                    "заметка {} (index: false): нет запроса exact к ней",
                    n.path
                ));
            }
        }
        if s.insights < MIN_INSIGHTS {
            errs.push(format!("insight {} < {MIN_INSIGHTS}", s.insights));
        }
        if s.chains < MIN_CHAINS {
            errs.push(format!("цепочек замещения {} < {MIN_CHAINS}", s.chains));
        }
        if s.retracted < MIN_RETRACTED {
            errs.push(format!("отзывов {} < {MIN_RETRACTED}", s.retracted));
        }
        if s.contradictions < MIN_CONTRADICTIONS {
            errs.push(format!(
                "пар противоречий {} < {MIN_CONTRADICTIONS}",
                s.contradictions
            ));
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
        // служебные слова не считаются; слова из 2 символов — считаются (целиком)
        assert!(shared_words("how is it in the", "the how it is").is_empty());
        assert_eq!(
            shared_words("ok then", "it is ok"),
            [("ok".into(), "ok".into())]
        );
        // основа Snowball: «индексы» / «индексов», «running» / «runs»
        assert!(same_word("индексы", "индексов") && same_word("running", "runs"));
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
            forbid: Vec::new(),
            weaker: Vec::new(),
            evidence: None,
        }
    }

    fn insight(key: &str, certainty: Option<&str>, supersedes: Option<&str>) -> Insight {
        Insight {
            key: key.into(),
            lang: Lang::En,
            kind: "gotcha".into(),
            scope: "solution".into(),
            area: None,
            certainty: certainty.map(String::from),
            evidence: None,
            title: format!("title {key}"),
            body: format!("body {key}"),
            triggers: Vec::new(),
            supersedes: supersedes.map(String::from),
            retract_reason: None,
        }
    }

    /// Длинная заметка: 2100 слов, H4, факт в середине.
    fn long_note() -> Note {
        let filler = "lorem ipsum dolor sit amet ".repeat(210);
        note(
            "en/long/ingest",
            &format!(
                "# Ingest\n\n{filler}\n\n#### Retries\n\nThe ingest worker retries a failed batch \
                 exactly four times before parking it.\n\n{filler}\n"
            ),
        )
    }

    fn m3_corpus() -> Corpus {
        let mut c = Corpus::default();
        let n = long_note();
        c.notes.insert(n.path.clone(), n);
        let mut proven = insight("cache-ok", Some("theorem"), None);
        proven.evidence = Some(Evidence {
            checked: "c".into(),
            method: "m".into(),
            result: "r".into(),
        });
        let mut retracted = insight("wrong-cause", None, None);
        retracted.retract_reason = Some("ошибочный вывод".into());
        c.insights = vec![
            insight("old", None, None),
            insight("mid", None, Some("old")),
            insight("new", None, Some("mid")),
            insight("cache-guess", Some("hypothesis"), None),
            proven,
            retracted,
        ];
        c
    }

    fn m3_query(id: &str, kind: Kind, expect: &str) -> Query {
        q(id, "query text", Lang::En, &[expect], kind, false)
    }

    /// Разметка M3: forbid, weaker, supersedes, evidence, цепочки без циклов.
    #[test]
    fn m3_markup_is_checked() {
        let c = m3_corpus();
        let mut sup = m3_query("s1", Kind::Superseded, "insight:new");
        sup.forbid = vec!["insight:old".into(), "insight:mid".into()];
        let mut sup2 = m3_query("s2", Kind::Superseded, "insight:new");
        sup2.forbid = vec!["insight:wrong-cause".into()];
        let mut cert = m3_query("c1", Kind::Certainty, "insight:cache-ok");
        cert.weaker = vec!["insight:cache-guess".into()];
        let mut chunk = m3_query("k1", Kind::Chunk, "en/long/ingest");
        chunk.evidence = Some("The ingest worker retries a failed batch exactly four times".into());
        let s = validate(&c, &[sup.clone(), sup2, cert.clone(), chunk.clone()], false).unwrap();
        assert_eq!((s.chains, s.retracted, s.contradictions), (1, 1, 1));
        assert_eq!((s.long_notes, s.insights, s.queries_m2), (1, 6, 0));

        let mut bad_forbid = sup;
        bad_forbid.id = "b1".into();
        bad_forbid.forbid = vec!["insight:nope".into(), "insight:new".into()];
        let mut bad_weaker = cert.clone();
        bad_weaker.id = "b2".into();
        bad_weaker.weaker = vec!["insight:cache-ok".into()];
        let mut bad_expect = cert;
        bad_expect.id = "b3".into();
        bad_expect.expect = vec!["insight:cache-guess".into()];
        let mut bad_ev = chunk.clone();
        bad_ev.id = "b4".into();
        bad_ev.evidence =
            Some("The ingest worker retries a failed batch exactly five times".into());
        let mut edge_ev = chunk;
        edge_ev.id = "b5".into();
        edge_ev.evidence = Some("lorem ipsum dolor sit amet lorem ipsum dolor sit amet".into());
        let mut stray = m3_query("b6", Kind::Exact, "en/long/ingest");
        stray.evidence = Some("x".into());
        let err = validate(
            &c,
            &[bad_forbid, bad_weaker, bad_expect, bad_ev, edge_ev, stray],
            false,
        )
        .unwrap_err()
        .to_string();
        for needle in [
            "запрос b1: forbid insight:nope",
            "запрос b1: forbid insight:new: действующий",
            "запрос b2: weaker insight:cache-ok",
            "запрос b3: expect insight:cache-guess",
            "запрос b4: evidence нет дословно",
            "запрос b5: evidence не из середины",
            "запрос b6: evidence — только у chunk",
        ] {
            assert!(err.contains(needle), "{needle}: {err}");
        }

        // theorem без доказательства, ссылка вперёд и на несуществующий ключ, развилка
        let mut c = m3_corpus();
        c.insights.push(insight("t", Some("theorem"), None));
        c.insights.push(insight("fork", None, Some("mid")));
        c.insights.push(insight("ghost", None, Some("nope")));
        c.insights.insert(0, insight("early", None, Some("t")));
        let err = validate(&c, &[], false).unwrap_err().to_string();
        for needle in [
            "insight t: theorem без evidence",
            "insight fork: mid уже замещён",
            "insight ghost: supersedes nope",
            "insight early: supersedes t: нет insight с таким ключом выше",
        ] {
            assert!(err.contains(needle), "{needle}: {err}");
        }
        // цикл (порядок файла нарушен намеренно)
        let c = Corpus {
            insights: vec![insight("a", None, Some("b")), insight("b", None, Some("a"))],
            ..Corpus::default()
        };
        let err = validate(&c, &[], false).unwrap_err().to_string();
        assert!(err.contains("цикл замещений"), "{err}");
    }

    #[test]
    fn note_flags() {
        let n = note(
            "en/ops/x",
            "---\ntags: [a]\nindex: false\n---\n# X\n\nbody\n",
        );
        assert!(n.index_disabled() && !n.is_long());
        assert!(!note("en/ops/y", "# Y\n\nindex: false\n").index_disabled());
        assert!(long_note().is_long() && long_note().words() > 2000);
        assert!(in_solution("ru/zam/vault-secrets") && !in_solution("ru/git/x"));
        assert_eq!(squash_ws(" a\n  b\tc "), "a b c");
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
        let mut corpus = Corpus::load(&root.join("corpus")).unwrap();
        corpus.insights = load_insights(&root.join("insights.toml")).unwrap();
        let queries = load_queries(&root.join("queries.toml")).unwrap();
        let s = validate(&corpus, &queries, true).unwrap();
        assert!(s.disjoint_share() >= MIN_DISJOINT_SHARE);
    }
}
