//! Словарь домена в процессе сервера (M4.5, спецификация §2.2): индекс имён и псевдонимов
//! действующих терминов базы и его кэш.
//!
//! - [`GlossaryIndex`] — все действующие термины базы; слова имён и псевдонимов (нижний регистр)
//!   индексированы по первому слову. Область запроса (пространства цели) отбирается при чтении.
//!   Потребители: канал `alias` поиска ([`GlossaryIndex::expand`]), подсказки терминов к
//!   промпту ([`GlossaryIndex::find_in_text`]), предупреждения `term_alias`
//!   ([`GlossaryIndex::terms_in`]), строка словаря в дайджесте ([`GlossaryIndex::count_in`]).
//!   Подсказки и `term_alias` сравнивают одной функцией [`zam_store::ProseText::spans`] (M6.6
//!   §6: код и идентификаторы не проза, аббревиатуры — с учётом регистра); расширение запроса
//!   поиска — по словам, как прежде.
//! - [`GlossaryCache`] — по базе (путь файла) хранит `(glossary_version, индекс)`; версия
//!   читается из `meta` на каждое обращение (один индексный запрос SQLite), при расхождении
//!   индекс собирается заново. Любая запись `term` растит версию, поэтому устаревшего словаря
//!   кэш не отдаёт.
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use zam_core::Id;
use zam_store::{
    AliasGroup, Db, GlossaryEntry, ProseText, StoreError, all_active_terms, glossary_version,
    is_acronym,
};

/// Не больше групп расширения в одном запросе (спецификация §2.2, Review Focus 2).
pub const MAX_GROUPS: usize = 8;
/// Пределы текста промпта для подсказок терминов, символов (как `QUERY_MAX_CHARS`).
pub const HINT_TEXT_CHARS: usize = 512;

/// Имя или псевдоним термина в индексе.
struct Name {
    /// Индекс термина в [`GlossaryIndex::terms`].
    term: usize,
    /// Слова имени в нижнем регистре.
    words: Vec<String>,
    /// Имя как записано.
    raw: String,
    /// Аббревиатура ([`zam_store::is_acronym`], при сборке индекса): подсказка к запросу — с
    /// учётом регистра; расширение запроса регистр не учитывает. Имя с теми же словами в другом
    /// написании того же термина («ZAM» и «Zam») снимает признак.
    acronym: bool,
}

/// Расширение одного слова запроса (поле `expanded` ответа `/search`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Expansion {
    /// Каноническое имя термина.
    pub term: String,
    /// Совпавшие слова запроса (как в запросе).
    pub word: String,
    /// Остальные имена и псевдонимы термина, которыми расширен запрос.
    pub with: Vec<String>,
    /// Индекс первого заменяемого слова запроса.
    #[serde(skip)]
    pub start: usize,
    /// Сколько слов запроса заменено.
    #[serde(skip)]
    pub len: usize,
}

/// Индекс словаря базы.
#[derive(Default)]
pub struct GlossaryIndex {
    terms: Vec<GlossaryEntry>,
    names: Vec<Name>,
    /// Первое слово имени (нижний регистр) → индексы в `names`.
    by_first: HashMap<String, Vec<usize>>,
}

/// Слова текста: всё, что не буква и не цифра, — разделитель (как `query_terms` хранилища).
fn split_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|t| !t.is_empty())
}

fn lower_words(s: &str) -> Vec<String> {
    split_words(s).map(str::to_lowercase).collect()
}

/// Совпадение имени с подряд идущими словами текста.
struct Hit {
    start: usize,
    len: usize,
    term: usize,
    /// Индекс совпавшего имени.
    name: usize,
}

impl GlossaryIndex {
    /// Собирает индекс из действующих терминов.
    pub fn build(terms: Vec<GlossaryEntry>) -> GlossaryIndex {
        let mut names: Vec<Name> = Vec::new();
        let mut by_first: HashMap<String, Vec<usize>> = HashMap::new();
        for (ti, t) in terms.iter().enumerate() {
            // слова имён термина → индекс имени в `names`
            let mut seen: Vec<(Vec<String>, usize)> = Vec::new();
            for raw in std::iter::once(&t.title).chain(&t.aliases) {
                let words = lower_words(raw);
                // имя из одних односимвольных слов («C++» -> «c», «C#» -> «c») совпало бы с любой
                // буквой запроса: в расширение и подсказки не входит
                if words.iter().all(|w| w.chars().count() < 2) {
                    continue;
                }
                let acronym = is_acronym(raw);
                if let Some(&(_, ni)) = seen.iter().find(|(w, _)| *w == words) {
                    names[ni].acronym &= acronym;
                    continue;
                }
                seen.push((words.clone(), names.len()));
                by_first
                    .entry(words[0].clone())
                    .or_default()
                    .push(names.len());
                names.push(Name {
                    term: ti,
                    words,
                    raw: raw.trim().to_string(),
                    acronym,
                });
            }
        }
        GlossaryIndex {
            terms,
            names,
            by_first,
        }
    }

    /// Терминов в индексе (всех пространств).
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Термины пространств `spaces` (по имени).
    pub fn terms_in<'a>(&'a self, spaces: &'a [Id]) -> impl Iterator<Item = &'a GlossaryEntry> {
        self.terms
            .iter()
            .filter(move |t| spaces.contains(&t.space_id))
    }

    /// Терминов в пространствах `spaces`.
    pub fn count_in(&self, spaces: &[Id]) -> usize {
        self.terms_in(spaces).count()
    }

    /// Совпадения имён с подряд идущими словами `words` (нижний регистр): в каждой позиции —
    /// самое длинное имя (при равной длине — имена всех терминов), следующая позиция — после
    /// совпавших слов. Не больше `limit` позиций.
    fn hits(&self, words: &[String], spaces: &[Id], limit: usize) -> Vec<Hit> {
        let mut out: Vec<Hit> = Vec::new();
        let (mut i, mut positions) = (0, 0);
        while i < words.len() && positions < limit {
            let mut best = 0;
            let mut found: Vec<usize> = Vec::new();
            for &ni in self.by_first.get(&words[i]).into_iter().flatten() {
                let n = &self.names[ni];
                if !spaces.contains(&self.terms[n.term].space_id)
                    || i + n.words.len() > words.len()
                    || words[i..i + n.words.len()] != n.words[..]
                {
                    continue;
                }
                match n.words.len().cmp(&best) {
                    std::cmp::Ordering::Greater => {
                        best = n.words.len();
                        found = vec![ni];
                    }
                    std::cmp::Ordering::Equal => found.push(ni),
                    std::cmp::Ordering::Less => {}
                }
            }
            if found.is_empty() {
                i += 1;
                continue;
            }
            positions += 1;
            for ni in found {
                out.push(Hit {
                    start: i,
                    len: best,
                    term: self.names[ni].term,
                    name: ni,
                });
            }
            i += best;
        }
        out
    }

    /// Расширения слов запроса `terms` (`zam_store::query_terms`) именами и псевдонимами
    /// терминов пространств `spaces`: многословные имена — по подряд идущим словам, не больше
    /// [`MAX_GROUPS`] мест запроса.
    pub fn expand(&self, terms: &[&str], spaces: &[Id]) -> Vec<Expansion> {
        if self.is_empty() {
            return Vec::new();
        }
        let lower: Vec<String> = terms.iter().map(|w| w.to_lowercase()).collect();
        self.hits(&lower, spaces, MAX_GROUPS)
            .into_iter()
            .map(|h| {
                let t = &self.terms[h.term];
                let with = self
                    .names
                    .iter()
                    .filter(|n| n.term == h.term)
                    .filter(|n| n.words != self.names[h.name].words)
                    .map(|n| n.raw.clone())
                    .collect();
                Expansion {
                    term: t.title.clone(),
                    word: terms[h.start..h.start + h.len].join(" "),
                    with,
                    start: h.start,
                    len: h.len,
                }
            })
            .collect()
    }

    /// Термины пространств `spaces`, чьё имя или псевдоним встречается в прозе первых
    /// [`HINT_TEXT_CHARS`] символов `text`, в порядке первого вхождения.
    pub fn find_in_text(&self, text: &str, spaces: &[Id]) -> Vec<&GlossaryEntry> {
        self.find_in_head(text, spaces, HINT_TEXT_CHARS)
    }

    /// Как [`GlossaryIndex::find_in_text`], но по первым `chars` символам `text` (пакет под
    /// задачу `brief` — до 2000 символов). Сравнение — как у предупреждений `term_alias`
    /// ([`ProseText::spans`], M6.6 §6): код и идентификаторы — не проза, аббревиатура
    /// (`Name.acronym`) — с учётом регистра. Кандидаты — имена, чьё первое слово есть среди
    /// слов прозы (индекс по первому слову); в каждой позиции — самое длинное имя (при равной
    /// длине — имена всех терминов), следующее ищется после него, как у расширения запроса.
    pub fn find_in_head(&self, text: &str, spaces: &[Id], chars: usize) -> Vec<&GlossaryEntry> {
        if self.is_empty() {
            return Vec::new();
        }
        let head: String = text.chars().take(chars).collect();
        let prose = ProseText::new(&head);
        // (начало, конец, имя) всех вхождений имён-кандидатов
        let mut found: Vec<(usize, usize, usize)> = Vec::new();
        let words = lower_words(prose.as_str());
        let mut tried: HashSet<&str> = HashSet::new();
        for w in &words {
            if !tried.insert(w.as_str()) {
                continue;
            }
            for &ni in self.by_first.get(w).into_iter().flatten() {
                let n = &self.names[ni];
                if spaces.contains(&self.terms[n.term].space_id) {
                    found.extend(prose.spans(&n.raw, n.acronym).map(|r| (r.start, r.end, ni)));
                }
            }
        }
        found.sort_by_key(|&(start, end, ni)| (start, Reverse(end), ni));
        let mut out: Vec<&GlossaryEntry> = Vec::new();
        let (mut i, mut cursor) = (0, 0);
        while i < found.len() {
            let (start, end, _) = found[i];
            if start < cursor {
                i += 1;
                continue;
            }
            while found.get(i).is_some_and(|f| (f.0, f.1) == (start, end)) {
                let t = &self.terms[self.names[found[i].2].term];
                if !out.iter().any(|o| o.id == t.id) {
                    out.push(t);
                }
                i += 1;
            }
            cursor = end;
        }
        out
    }
}

/// Группы расширения для FTS: расширения одного места запроса сливаются.
pub fn alias_groups(exp: &[Expansion]) -> Vec<AliasGroup> {
    let mut out: Vec<AliasGroup> = Vec::new();
    for e in exp {
        match out.iter_mut().find(|g| g.start == e.start) {
            Some(g) => {
                for w in std::iter::once(&e.term).chain(&e.with) {
                    if !g.with.contains(w) {
                        g.with.push(w.clone());
                    }
                }
            }
            None => out.push(AliasGroup {
                start: e.start,
                len: e.len,
                with: e.with.clone(),
            }),
        }
    }
    out
}

/// Кэш индексов словаря по базам (см. модуль).
#[derive(Default)]
pub struct GlossaryCache {
    map: Mutex<HashMap<PathBuf, (i64, Arc<GlossaryIndex>)>>,
    builds: AtomicUsize,
}

impl GlossaryCache {
    /// Индекс словаря базы `db` на текущую версию `meta.glossary_version`.
    pub async fn index(&self, db: &Db) -> Result<Arc<GlossaryIndex>, StoreError> {
        let version = glossary_version(db).await?;
        let key = db.path().to_path_buf();
        if let Some((v, idx)) = self.lock().get(&key)
            && *v == version
        {
            return Ok(idx.clone());
        }
        // версия прочитана до терминов: запись между чтениями оставит в кэше старую версию с
        // более новыми данными — следующее обращение пересоберёт индекс
        let idx = Arc::new(GlossaryIndex::build(all_active_terms(db).await?));
        self.builds.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(key, (version, idx.clone()));
        Ok(idx)
    }

    /// Сколько раз индекс собирался с нуля (проверка инвалидации в тестах).
    pub fn builds(&self) -> usize {
        self.builds.load(Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, (i64, Arc<GlossaryIndex>)>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(space: Id, title: &str, aliases: &[&str]) -> GlossaryEntry {
        GlossaryEntry {
            id: Id::new(),
            space_id: space,
            title: title.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            definition: "определение".into(),
            rev: 1,
        }
    }

    fn words(q: &str) -> Vec<&str> {
        zam_store::query_terms(q)
    }

    #[test]
    fn expands_names_aliases_and_multiword() {
        let sp = Id::new();
        let idx = GlossaryIndex::build(vec![
            entry(sp, "Выжимка", &["summary", "digest"]),
            entry(sp, "Кэш выжимок", &["summary cache"]),
        ]);
        let e = idx.expand(&words("Summary кэша"), &[sp]);
        assert_eq!(e.len(), 1);
        assert_eq!(
            (e[0].term.as_str(), e[0].word.as_str()),
            ("Выжимка", "Summary")
        );
        assert_eq!(e[0].with, ["Выжимка", "digest"]);
        // многословный псевдоним — по подряд идущим словам, самый длинный побеждает
        let e = idx.expand(&words("про summary cache тут"), &[sp]);
        assert_eq!(e.len(), 1);
        assert_eq!(
            (e[0].term.as_str(), e[0].word.as_str(), e[0].start, e[0].len),
            ("Кэш выжимок", "summary cache", 1, 2)
        );
        assert_eq!(e[0].with, ["Кэш выжимок"]);
        // другое пространство и чужое слово
        assert!(idx.expand(&words("summary"), &[Id::new()]).is_empty());
        assert!(idx.expand(&words("summaries"), &[sp]).is_empty());
    }

    #[test]
    fn single_letter_names_are_not_indexed() {
        let sp = Id::new();
        let idx = GlossaryIndex::build(vec![
            entry(sp, "Си", &["C++", "C#", "c"]),
            entry(sp, "План", &["Plan B"]),
        ]);
        // «C++» и «C#» дают только слово «c»: ложные срабатывания на любую букву «c»
        assert!(idx.expand(&words("версия c тут"), &[sp]).is_empty());
        assert!(idx.find_in_text("пишем на C++ и C#", &[sp]).is_empty());
        // многословное имя с односимвольным словом остаётся
        let e = idx.expand(&words("нужен plan b"), &[sp]);
        assert_eq!((e.len(), e[0].term.as_str(), e[0].len), (1, "План", 2));
        // сам термин находится по имени
        assert_eq!(idx.expand(&words("си"), &[sp]).len(), 1);
    }

    #[test]
    fn expansion_capped_at_eight_places() {
        let sp = Id::new();
        let terms: Vec<_> = (0..12)
            .map(|i| entry(sp, &format!("Терм{i}"), &[&format!("al{i}x")]))
            .collect();
        let idx = GlossaryIndex::build(terms);
        let q = (0..12)
            .map(|i| format!("al{i}x"))
            .collect::<Vec<_>>()
            .join(" ");
        let e = idx.expand(&words(&q), &[sp]);
        assert_eq!(e.len(), MAX_GROUPS);
        assert_eq!(alias_groups(&e).len(), MAX_GROUPS);
    }

    #[test]
    fn same_place_in_two_terms_merges_group() {
        let (a, b) = (Id::new(), Id::new());
        let idx = GlossaryIndex::build(vec![
            entry(a, "Альфа", &["common"]),
            entry(b, "Бета", &["common"]),
        ]);
        let e = idx.expand(&words("common"), &[a, b]);
        assert_eq!(e.len(), 2);
        let g = alias_groups(&e);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].with, ["Альфа", "Бета"]);
    }

    #[test]
    fn finds_terms_in_text_in_order() {
        let sp = Id::new();
        let idx = GlossaryIndex::build(vec![
            entry(sp, "Выжимка", &["summary"]),
            entry(sp, "Чанк", &[]),
        ]);
        let found = idx.find_in_text("Собери чанк и summary; ещё раз Чанк", &[sp]);
        let titles: Vec<&str> = found.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Чанк", "Выжимка"]);
        // за пределами 512 символов — не ищется
        let far = format!("{} чанк", "я ".repeat(300));
        assert!(idx.find_in_text(&far, &[sp]).is_empty());
    }

    /// M6.6 §6.4: подсказка термина к запросу — по тем же правилам, что `term_alias`: код и
    /// идентификаторы не проза, аббревиатура — с учётом регистра.
    #[test]
    fn find_in_text_skips_code_and_identifiers() {
        let sp = Id::new();
        let idx = GlossaryIndex::build(vec![
            entry(sp, "ZeroAIMemory", &["ZAM"]),
            entry(sp, "Хуки", &["hooks"]),
            entry(sp, "Карта кода", &["code map", "zam code"]),
        ]);
        let titles = |t: &str| -> Vec<String> {
            idx.find_in_text(t, &[sp])
                .iter()
                .map(|t| t.title.clone())
                .collect()
        };
        for text in [
            "собери zam-code и ZAM-code",
            "zam и Zam — не аббревиатура",
            "правка hooks.rs и `hooks`",
            "```\nhooks\nZAM\n```",
            "Карта — `zam code map`.",
            "ZAM_X, crates/hooks, zam::foo",
        ] {
            assert!(titles(text).is_empty(), "{text:?}: {:?}", titles(text));
        }
        assert_eq!(
            titles("Почини ZAM: настроены hooks."),
            ["ZeroAIMemory", "Хуки"]
        );
        assert_eq!(titles("обнови zam code map"), ["Карта кода"]);
        // в позиции — самое длинное имя, как прежде: «ZAM code» поглощает и «ZAM», и «code map»
        assert_eq!(titles("ZAM code map"), ["Карта кода"]);
    }
}
