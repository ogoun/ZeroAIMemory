//! Восстановление значений под [`SECRET_MARK`] в тексте, правленном по маске (спецификация M7
//! §4.3.2, задача 6 M7.1; с круга правок 1 — здесь, обвязка — `zam_server::api::secret_restore`).
//! Агент по токену читает текст с маской ([`crate::mask_text`]) и, правя его, присылает обратно
//! метки вместо значений; без восстановления запись затёрла бы значение литералом.
//!
//! База — текст, по которому правили; её вид `mask_text(base)`, и у каждой метки вида известно
//! исходное значение ([`MaskedBase`]). Новый текст выравнивается с видом (переводы строк обеих
//! сторон для выравнивания приводятся к `\n`; окончания результата — как у нового текста):
//! 1. дифф строк (patience: опоры — строки, единственные в обоих текстах, между ними — Майерс
//!    [`similar`]) — неизменённая строка возвращает значения своих меток по порядку; одинаковые
//!    строки разных секций не сопоставляются через чужой заголовок; время близко к линейному и на
//!    тексте 5 МиБ;
//! 2. перенос строки с метками без изменений — та же строка;
//! 3. изменённый участок между неизменёнными — строки с метками в пару по одинаковым словам (без
//!    слов меток; порядок сохраняется: цитата `> `, отступ, другие знаки);
//! 4. правленые строки (на месте и перенесённые) — пары по похожести слов, самые похожие первыми,
//!    при равной похожести — строка того же изменённого участка, затем ближайшая по месту;
//! 5. остаток изменённого участка (строка переписана другими словами, строки склеены) — дифф по
//!    всем его строкам с метками без пары; участок больше предела — по порядку появления, если
//!    меток поровну.
//!
//! Внутри пары строк метки сопоставляет дифф «символов» — токенов: слово, пробелы, знак, метка —
//! один токен. Метка нового текста получает значение, только если **слово перед ней** (ключ слева
//! от разделителя: `token`, `user` в URL, `p` у `-p…`) то же, что у метки базы (без учёта
//! регистра; метка в начале строки — слово после неё): `GITHUB_TOKEN: ‹secret›`, переписанный в
//! `OPENAI_KEY: ‹secret›`, значение не получает (и, раз маска базы осталась без пары, правка
//! отклоняется — ниже).
//!
//! Метка без пары записывается как есть и считается в [`Restored::kept`]. Литерал `‹secret›`, уже
//! лежавший в базе, — метка со значением «сама метка»: напротив себя он не новый.
//!
//! Итог пересканируется: значение, которое сканер в новом тексте не закрывает целиком (снят забор
//! кода вокруг `mysql -p‹secret›`, URL с паролем переписан прозой, `password = ‹secret›` стал
//! «пароль — ‹secret›»), выходит из-под маски — номера таких строк нового текста —
//! [`Restored::exposed`]; такую правку вызывающий отклоняет (решение координатора, круг правок 1).
//! Туда же — метка без пары в изменённом участке, где осталась маска базы без пары (ключ
//! переписан другими словами, URL — прозой): это правка окружения существующего секрета, литералом
//! значение молча пропало бы из текущей ревизии (решение координатора по сомнению 1 круга
//! правок 1). Метка, дописанная в участок, где все маски базы в паре или масок нет, — литерал.
//!
//! Результат детерминирован (нет сроков по часам; обход `HashMap` в выборе пар упорядочен):
//! пределы работы диффа — по размерам входа.
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::ops::Range;

use similar::algorithms::{DiffHook, myers};

use crate::{SECRET_MARK, scan_secrets};

/// Итог восстановления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// Текст со значениями на месте меток, у которых нашлась пара.
    pub text: String,
    /// Меток нового текста без пары в базе — записаны как есть.
    pub kept: usize,
    /// Строки нового текста (с 1, по возрастанию), где восстановленное значение сканер в итоге не
    /// закрывает (правка выводит секрет из-под маски) или где метка без пары стоит в участке с
    /// маской базы без пары (правка окружения существующего секрета).
    pub exposed: Vec<usize>,
}

impl Restored {
    fn same(text: String) -> Self {
        Restored {
            text,
            kept: 0,
            exposed: Vec::new(),
        }
    }
}

/// Есть ли в тексте метка [`SECRET_MARK`] — быстрый путь всех вызовов: без метки восстанавливать
/// нечего, база не сканируется.
pub fn has_mark(s: &str) -> bool {
    s.contains(SECRET_MARK)
}

/// Меток [`SECRET_MARK`] в тексте.
pub fn marks_in(s: &str) -> usize {
    s.matches(SECRET_MARK).count()
}

/// Восстановление нового текста `edited`, правленного по виду базы `base`.
pub fn restore(base: &str, edited: &str) -> Restored {
    if !has_mark(edited) {
        return Restored::same(edited.to_owned());
    }
    MaskedBase::new(base).restore(edited)
}

/// Нет пары (индекс метки базы).
const NONE: u32 = u32::MAX;
/// Участок строк длиннее (строк в обоих текстах) — без диффа Майерса, делится по уникальным
/// строкам.
const LINE_MYERS_MAX: usize = 4096;
/// Строки с метками длиннее (токенов в обоих текстах) — без диффа токенов.
const TOKEN_MYERS_MAX: usize = 8192;
/// Глубина деления по уникальным строкам.
const ANCHOR_DEPTH: u32 = 8;
/// Слово чаще (строк старого текста без пар) — не даёт кандидатов в пару (`similar_lines`).
const SIMILAR_POSTINGS_MAX: usize = 64;
/// Кандидатов в пару строке нового текста — не больше (сначала по самым редким словам; строки
/// того же изменённого участка — сверх, если их не больше этого же числа).
const SIMILAR_CANDIDATES: usize = 64;
/// Лучших кандидатов строки — в общий выбор пар.
const SIMILAR_KEEP: usize = 4;
/// Предел работы всех диффов одного восстановления: сумма `(n + m)²` по вызовам Майерса (оценка
/// сверху его `O((n + m)·D)`); исчерпан — участки идут запасным путём.
const WORK_BUDGET: u64 = 200_000_000;

/// Вид базы для агента и значения его меток.
pub struct MaskedBase<'a> {
    raw: &'a str,
    /// `mask_text(raw)`.
    masked: String,
    /// Метки `masked` по порядку: позиция в `masked` и диапазон значения в `raw` (литерал
    /// `‹secret›` исходника — он сам).
    marks: Vec<(usize, Range<usize>)>,
}

impl<'a> MaskedBase<'a> {
    /// Вид `raw` — как [`crate::mask_text`] (те же находки и тот же пропуск неверных диапазонов,
    /// что у [`crate::mask_secrets`]).
    pub fn new(raw: &'a str) -> Self {
        let hits = scan_secrets(raw);
        let mut masked = String::with_capacity(raw.len());
        let mut marks = Vec::with_capacity(hits.len());
        let mut pos = 0;
        for h in &hits {
            let r = &h.range;
            if r.start < pos
                || r.end < r.start
                || r.end > raw.len()
                || !raw.is_char_boundary(r.start)
                || !raw.is_char_boundary(r.end)
            {
                continue;
            }
            push_plain(raw, pos..r.start, &mut masked, &mut marks);
            marks.push((masked.len(), r.clone()));
            masked.push_str(SECRET_MARK);
            pos = r.end;
        }
        push_plain(raw, pos..raw.len(), &mut masked, &mut marks);
        MaskedBase { raw, masked, marks }
    }

    /// Вид базы.
    pub fn masked(&self) -> &str {
        &self.masked
    }

    /// Восстановление нового текста `edited`, правленного по виду (правило — в описании модуля).
    pub fn restore(&self, edited: &str) -> Restored {
        let new_marks = mark_positions(edited);
        if new_marks.is_empty() {
            return Restored::same(edited.to_owned());
        }
        if edited == self.masked {
            return Restored::same(self.raw.to_owned());
        }
        let (pair, orphans) = if self.marks.is_empty() {
            (vec![NONE; new_marks.len()], Vec::new())
        } else {
            // переводы строк — к `\n`: метки те же и в том же порядке (`\r` — не часть метки)
            let (old, new) = (normalized(&self.masked), normalized(edited));
            let (om, nm) = (mark_positions(&old), mark_positions(&new));
            debug_assert_eq!((om.len(), nm.len()), (self.marks.len(), new_marks.len()));
            align(&old, &om, &new, &nm)
        };
        self.assemble(edited, &new_marks, &pair, orphans)
    }

    /// Текст по `edited` со значениями пар; пересканирование итога и метки без пары при маске
    /// базы без пары в участке (`orphans`) — [`Restored::exposed`].
    fn assemble(
        &self,
        edited: &str,
        new_marks: &[usize],
        pair: &[u32],
        orphans: Vec<usize>,
    ) -> Restored {
        let crlf = edited.contains("\r\n");
        let extra: usize = pair
            .iter()
            .filter(|&&a| a != NONE)
            .map(|&a| self.marks[a as usize].1.len())
            .sum();
        let mut text = String::with_capacity(edited.len() + extra);
        // восстановленные значения: диапазон в итоге и индекс метки нового текста
        let mut values: Vec<(Range<usize>, usize)> = Vec::new();
        let (mut pos, mut kept) = (0, 0);
        for (j, (&at, &a)) in new_marks.iter().zip(pair).enumerate() {
            text.push_str(&edited[pos..at]);
            if a == NONE {
                text.push_str(SECRET_MARK);
                kept += 1;
            } else {
                let v = &self.raw[self.marks[a as usize].1.clone()];
                if v == SECRET_MARK {
                    text.push_str(v);
                } else {
                    let start = text.len();
                    push_value(&mut text, v, crlf);
                    values.push((start..text.len(), j));
                }
            }
            pos = at + SECRET_MARK.len();
        }
        text.push_str(&edited[pos..]);
        let exposed = exposed_lines(&text, &values, orphans, edited, new_marks);
        Restored {
            text,
            kept,
            exposed,
        }
    }
}

/// Значение `v` в итог: многострочное — с переводами строк нового текста (`crlf` — `\r\n`).
fn push_value(out: &mut String, v: &str, crlf: bool) {
    if !v.contains(['\r', '\n']) {
        out.push_str(v);
        return;
    }
    let lf = normalized(v);
    if crlf {
        out.push_str(&lf.replace('\n', "\r\n"));
    } else {
        out.push_str(&lf);
    }
}

/// Строки `edited` (с 1), где значение из `values` сканер итога `text` не закрывает целиком, и
/// строки меток `orphans` (индексы меток `edited`).
fn exposed_lines(
    text: &str,
    values: &[(Range<usize>, usize)],
    orphans: Vec<usize>,
    edited: &str,
    new_marks: &[usize],
) -> Vec<usize> {
    let mut open = orphans;
    if !values.is_empty() {
        uncovered(text, values, &mut open);
    }
    if open.is_empty() {
        return Vec::new();
    }
    open.sort_unstable();
    open.dedup();
    // номера строк меток — одним проходом по `edited`
    let (mut line, mut pos) = (1usize, 0usize);
    let mut out: Vec<usize> = Vec::with_capacity(open.len());
    for j in open {
        let at = new_marks[j];
        line += edited.as_bytes()[pos..at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        pos = at;
        if out.last() != Some(&line) {
            out.push(line);
        }
    }
    out
}

/// Индексы меток значений `values`, которые сканер итога `text` не закрывает целиком, — в `open`.
fn uncovered(text: &str, values: &[(Range<usize>, usize)], open: &mut Vec<usize>) {
    let hits = scan_secrets(text);
    let mut h = 0;
    for (r, j) in values {
        while h < hits.len() && hits[h].range.end <= r.start {
            h += 1;
        }
        let covered = hits
            .get(h)
            .is_some_and(|x| x.range.start <= r.start && r.end <= x.range.end);
        if !covered {
            open.push(*j);
        }
    }
}

/// Текст с переводами строк `\n` (`\r\n` и одиночный `\r` — `\n`).
fn normalized(s: &str) -> Cow<'_, str> {
    if !s.contains('\r') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Позиции меток в тексте, по возрастанию.
fn mark_positions(s: &str) -> Vec<usize> {
    s.match_indices(SECRET_MARK).map(|(i, _)| i).collect()
}

/// Кусок исходника `r` без находок — в вид как есть; литеральные `‹secret›` в нём — метки со
/// значением «сама метка».
fn push_plain(
    raw: &str,
    r: Range<usize>,
    masked: &mut String,
    marks: &mut Vec<(usize, Range<usize>)>,
) {
    let piece = &raw[r.clone()];
    for (i, _) in piece.match_indices(SECRET_MARK) {
        let at = r.start + i;
        marks.push((masked.len() + i, at..at + SECRET_MARK.len()));
    }
    masked.push_str(piece);
}

/// Пары меток: для каждой метки `new` (позиции `new_marks`) — индекс метки `old` (позиции
/// `old_marks`) или [`NONE`].
fn align(old: &str, old_marks: &[usize], new: &str, new_marks: &[usize]) -> (Vec<u32>, Vec<usize>) {
    let mut intern: HashMap<&str, u32> = HashMap::new();
    let old = Side::new(old, old_marks, &mut intern);
    let new = Side::new(new, new_marks, &mut intern);
    let mut heads = vec![false; intern.len()];
    for (line, &id) in &intern {
        heads[id as usize] = line.trim_start().starts_with('#');
    }
    let mut a = Aligner {
        heads,
        orphans: Vec::new(),
        pair: vec![NONE; new_marks.len()],
        old_paired: vec![false; old_marks.len()],
        old_used: vec![false; old.lines.len()],
        new_used: vec![false; new.lines.len()],
        old_block: vec![NONE; old.lines.len()],
        new_block: vec![NONE; new.lines.len()],
        old_sec: vec![0; old.lines.len()],
        new_sec: vec![0; new.lines.len()],
        old,
        new,
        budget: WORK_BUDGET,
    };
    a.run();
    (a.pair, a.orphans)
}

/// Слова вокруг метки в её строке: последнее слово перед ней и первое после (без слов соседних
/// меток).
type Key<'x> = (Option<&'x str>, Option<&'x str>);

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Слова куска `s` (буквы, цифры, `_` подряд).
fn word_iter(s: &str) -> impl DoubleEndedIterator<Item = &str> {
    s.split(|c: char| !is_word(c)).filter(|w| !w.is_empty())
}

/// Одно и то же слово (без учёта регистра).
fn same_word(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x == y || x.to_lowercase() == y.to_lowercase(),
        _ => false,
    }
}

/// Метки могут быть парой: слово перед ними одно и то же; у меток в начале строки — слово после
/// (или его нет у обеих).
fn keys_match(o: Key, n: Key) -> bool {
    match (o.0, n.0) {
        (Some(_), Some(_)) => same_word(o.0, n.0),
        (None, None) => (o.1.is_none() && n.1.is_none()) || same_word(o.1, n.1),
        _ => false,
    }
}

/// Текст одной стороны: строки (с переводом строки), их номера по содержимому, метки по строкам.
struct Side<'x> {
    text: &'x str,
    lines: Vec<Range<usize>>,
    /// Номер содержимого строки (одинаковые строки обеих сторон — один номер).
    ids: Vec<u32>,
    /// Метки строки `i` — индексы `first[i]..first[i + 1]` в `marks`.
    first: Vec<u32>,
    /// Позиции меток в `text`, по возрастанию.
    marks: &'x [usize],
    /// Слова вокруг каждой метки ([`Key`]).
    keys: Vec<Key<'x>>,
}

impl<'x> Side<'x> {
    fn new(text: &'x str, marks: &'x [usize], intern: &mut HashMap<&'x str, u32>) -> Self {
        let mut lines = Vec::new();
        let mut ids = Vec::new();
        let mut first = Vec::new();
        let mut keys = Vec::with_capacity(marks.len());
        let (mut start, mut m) = (0usize, 0usize);
        for piece in text.split_inclusive('\n') {
            let end = start + piece.len();
            first.push(m as u32);
            let m0 = m;
            while m < marks.len() && marks[m] < end {
                m += 1;
            }
            for i in m0..m {
                let at = marks[i];
                let left_from = if i > m0 {
                    marks[i - 1] + SECRET_MARK.len()
                } else {
                    start
                };
                let right_to = if i + 1 < m { marks[i + 1] } else { end };
                keys.push((
                    word_iter(&text[left_from..at]).next_back(),
                    word_iter(&text[at + SECRET_MARK.len()..right_to]).next(),
                ));
            }
            let next = intern.len() as u32;
            ids.push(*intern.entry(piece).or_insert(next));
            lines.push(start..end);
            start = end;
        }
        first.push(m as u32);
        Side {
            text,
            lines,
            ids,
            first,
            marks,
            keys,
        }
    }

    /// Индексы меток строки `l`.
    fn line_marks(&self, l: usize) -> Range<usize> {
        self.first[l] as usize..self.first[l + 1] as usize
    }

    /// Куски строки `l` без меток.
    fn pieces(&self, l: usize) -> impl Iterator<Item = &'x str> + '_ {
        let r = self.lines[l].clone();
        let ms = self.line_marks(l);
        let starts = std::iter::once(r.start).chain(
            self.marks[ms.clone()]
                .iter()
                .map(|&m| m + SECRET_MARK.len()),
        );
        let ends = self.marks[ms].iter().copied().chain(std::iter::once(r.end));
        let text = self.text;
        starts.zip(ends).map(move |(s, e)| &text[s..e])
    }

    /// Слова строки `l` без слов меток: слово → сколько раз.
    fn line_words(&self, l: usize) -> Words<'x> {
        let mut out: Words<'x> = HashMap::new();
        for p in self.pieces(l) {
            for w in word_iter(p) {
                *out.entry(w).or_insert(0) += 1;
            }
        }
        out
    }

    /// Слова строки `l` по порядку, в нижнем регистре, через пробел (подпись строки для
    /// сопоставления в изменённом участке).
    fn signature(&self, l: usize) -> String {
        let mut s = String::new();
        for p in self.pieces(l) {
            for w in word_iter(p) {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(&w.to_lowercase());
            }
        }
        s
    }

    /// Токены строк `lines` (слова, пробелы, знаки; метка — один токен с её индексом); больше
    /// `cap` — `None`.
    fn tokens(&self, lines: &[usize], cap: usize) -> Option<Vec<Tok<'x>>> {
        let mut out = Vec::new();
        for &l in lines {
            let end = self.lines[l].end;
            let mut pos = self.lines[l].start;
            for mi in self.line_marks(l) {
                let at = self.marks[mi];
                words(&self.text[pos..at], &mut out);
                out.push(Tok {
                    s: &self.text[at..at + SECRET_MARK.len()],
                    mark: mi as u32,
                });
                pos = at + SECRET_MARK.len();
            }
            words(&self.text[pos..end], &mut out);
            if out.len() > cap {
                return None;
            }
        }
        Some(out)
    }
}

/// Токен диффа символов: равенство — по тексту (метка равна метке), `mark` — индекс метки или
/// [`NONE`].
#[derive(Clone, Copy)]
struct Tok<'x> {
    s: &'x str,
    mark: u32,
}

impl PartialEq for Tok<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.s == o.s
    }
}

/// Слова (буквы, цифры, `_`), пробелы — подряд одним токеном; прочие знаки — по одному.
fn words<'x>(s: &'x str, out: &mut Vec<Tok<'x>>) {
    let class = |c: char| {
        if is_word(c) {
            1u8
        } else if c.is_whitespace() {
            2
        } else {
            0
        }
    };
    let mut start = 0;
    let mut prev: Option<u8> = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        if let Some(p) = prev
            && (p != k || k == 0)
        {
            out.push(Tok {
                s: &s[start..i],
                mark: NONE,
            });
            start = i;
        }
        prev = Some(k);
    }
    if prev.is_some() {
        out.push(Tok {
            s: &s[start..],
            mark: NONE,
        });
    }
}

/// Неизменённый участок строк: `len` строк с `o` в старом и с `n` в новом.
#[derive(Clone, Copy)]
struct Run {
    o: usize,
    n: usize,
    len: usize,
}

/// Слова строки: слово → сколько раз.
type Words<'x> = HashMap<&'x str, u32>;

/// Похожесть строк по словам: общих слов (с повторами), если их не меньше половины слов более
/// короткой строки, иначе 0 («token: ‹secret› (prod)» и «token: ‹secret›» — похожи, «x ‹secret›»
/// и «token: ‹secret›» — нет).
fn similarity(a: &Words, b: &Words) -> u32 {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let common: u32 = small
        .iter()
        .map(|(w, c)| (*c).min(big.get(w).copied().unwrap_or(0)))
        .sum();
    let shorter = a.values().sum::<u32>().min(b.values().sum());
    if common == 0 || common * 2 < shorter {
        0
    } else {
        common
    }
}

struct Aligner<'x> {
    old: Side<'x>,
    new: Side<'x>,
    /// Пара каждой метки нового текста — индекс метки старого или [`NONE`].
    pair: Vec<u32>,
    /// Метка старого текста уже в паре.
    old_paired: Vec<bool>,
    /// Строка уже сопоставлена целиком (неизменённая, перенесённая или правленая).
    old_used: Vec<bool>,
    new_used: Vec<bool>,
    /// Изменённый участок строки (индекс в списке участков) или [`NONE`].
    old_block: Vec<u32>,
    new_block: Vec<u32>,
    /// Секция строки (число сопоставленных заголовков до неё).
    old_sec: Vec<u32>,
    new_sec: Vec<u32>,
    /// Номер содержимого строки — заголовок Markdown (`#…`).
    heads: Vec<bool>,
    /// Метки нового текста без пары, где в том же участке осталась маска базы без пары.
    orphans: Vec<usize>,
    budget: u64,
}

impl Aligner<'_> {
    /// Шаги выравнивания (описание модуля).
    fn run(&mut self) {
        let mut runs = Vec::new();
        let heads = section_runs(
            &self.old.ids,
            &self.new.ids,
            &self.heads,
            &mut self.budget,
            &mut runs,
        );
        // секция строки — число сопоставленных заголовков до неё (у обеих сторон одна нумерация)
        let (mut so, mut sn) = (0u32, 0u32);
        let mut hi = heads.iter().peekable();
        for l in 0..self.old.lines.len() {
            while hi.peek().is_some_and(|h| h.0 <= l) {
                hi.next();
                so += 1;
            }
            self.old_sec[l] = so;
        }
        let mut hi = heads.iter().peekable();
        for k in 0..self.new.lines.len() {
            while hi.peek().is_some_and(|h| h.1 <= k) {
                hi.next();
                sn += 1;
            }
            self.new_sec[k] = sn;
        }
        for r in &runs {
            for t in 0..r.len {
                self.pair_lines(r.o + t, r.n + t);
            }
        }
        let mut blocks = Vec::with_capacity(runs.len() + 1);
        let (mut po, mut pn) = (0, 0);
        let tail = Run {
            o: self.old.lines.len(),
            n: self.new.lines.len(),
            len: 0,
        };
        for r in runs.iter().chain(std::iter::once(&tail)) {
            if po < r.o && pn < r.n {
                let b = blocks.len() as u32;
                self.old_block[po..r.o].fill(b);
                self.new_block[pn..r.n].fill(b);
                blocks.push((po..r.o, pn..r.n));
            }
            po = r.o + r.len;
            pn = r.n + r.len;
        }
        self.moves();
        for (o, n) in &blocks {
            self.same_words_in_block(o.clone(), n.clone());
        }
        self.similar_lines();
        for (o, n) in &blocks {
            self.rest_of_block(o.clone(), n.clone());
        }
        for (o, n) in blocks {
            self.orphans_of_block(o, n);
        }
    }

    /// Метки нового текста без пары в изменённом участке, где осталась маска базы без пары: это
    /// правка окружения существующего секрета (литералом значение молча пропало бы) — в
    /// `orphans`, вызывающий отклоняет такую правку (решение координатора по сомнению 1 круга
    /// правок 1). Маски базы участка все в паре (дописана ещё одна метка) или участок без масок —
    /// метки остаются литералом.
    fn orphans_of_block(&mut self, o: Range<usize>, n: Range<usize>) {
        let lost = o.into_iter().any(|l| {
            self.old
                .line_marks(l)
                .into_iter()
                .any(|a| !self.old_paired[a])
        });
        if !lost {
            return;
        }
        for k in n {
            for b in self.new.line_marks(k) {
                if self.pair[b] == NONE {
                    self.orphans.push(b);
                }
            }
        }
    }

    /// Пара без проверки слов (одинаковые строки).
    fn link(&mut self, a: usize, b: usize) {
        self.pair[b] = a as u32;
        self.old_paired[a] = true;
    }

    /// Пара правленых строк: только свободные метки с тем же словом рядом ([`keys_match`]).
    fn link_checked(&mut self, a: usize, b: usize) {
        if self.pair[b] == NONE
            && !self.old_paired[a]
            && keys_match(self.old.keys[a], self.new.keys[b])
        {
            self.link(a, b);
        }
    }

    /// Строка старого текста с метками, ни одна из которых не в паре, ещё не сопоставлена.
    fn old_free(&self, l: usize) -> bool {
        let m = self.old.line_marks(l);
        !self.old_used[l] && !m.is_empty() && m.into_iter().all(|a| !self.old_paired[a])
    }

    /// Строка нового текста с метками, ни одна из которых не в паре, ещё не сопоставлена.
    fn new_free(&self, k: usize) -> bool {
        let m = self.new.line_marks(k);
        !self.new_used[k] && !m.is_empty() && m.into_iter().all(|b| self.pair[b] == NONE)
    }

    /// Одинаковые строки `o` и `n`: метки — по порядку.
    fn pair_lines(&mut self, o: usize, n: usize) {
        let (om, nm) = (self.old.line_marks(o), self.new.line_marks(n));
        debug_assert_eq!(om.len(), nm.len(), "одинаковые строки — поровну меток");
        for (a, b) in om.zip(nm) {
            self.link(a, b);
        }
        self.old_used[o] = true;
        self.new_used[n] = true;
    }

    /// Перенос строки с метками целиком: несопоставленная строка нового текста, равная
    /// несопоставленной строке старого, — пара; из одинаковых — строка той же секции, того же
    /// изменённого участка, затем ближайшая по месту (одинаковых больше
    /// [`SIMILAR_CANDIDATES`] — по порядку появления).
    fn moves(&mut self) {
        let mut pool: HashMap<u32, VecDeque<usize>> = HashMap::new();
        for l in 0..self.old.lines.len() {
            if !self.old_used[l] && !self.old.line_marks(l).is_empty() {
                pool.entry(self.old.ids[l]).or_default().push_back(l);
            }
        }
        if pool.is_empty() {
            return;
        }
        let (on, nn) = (self.old.lines.len(), self.new.lines.len());
        for k in 0..nn {
            if self.new_used[k] || self.new.line_marks(k).is_empty() {
                continue;
            }
            let Some(q) = pool.get_mut(&self.new.ids[k]) else {
                continue;
            };
            let pick = if q.len() > SIMILAR_CANDIDATES {
                q.pop_front()
            } else {
                let (sec, block) = (self.new_sec[k], self.new_block[k]);
                let best = q.iter().enumerate().min_by_key(|&(_, &l)| {
                    (
                        self.old_sec[l] != sec,
                        block == NONE || self.old_block[l] != block,
                        (l * nn).abs_diff(k * on),
                        l,
                    )
                });
                best.map(|(i, _)| i).and_then(|i| q.remove(i))
            };
            if let Some(l) = pick {
                self.pair_lines(l, k);
            }
        }
    }

    /// Изменённый участок: свободные строки с метками в пару по одинаковой подписи (слова без
    /// меток, по порядку) — тот же дифф строк, порядок сохраняется (цитата, отступ, знаки).
    fn same_words_in_block(&mut self, o: Range<usize>, n: Range<usize>) {
        let ol: Vec<usize> = o.filter(|&l| self.old_free(l)).collect();
        let nl: Vec<usize> = n.filter(|&k| self.new_free(k)).collect();
        if ol.is_empty() || nl.is_empty() {
            return;
        }
        let mut sigs: HashMap<String, u32> = HashMap::new();
        let mut id = |s: String| {
            let next = sigs.len() as u32;
            *sigs.entry(s).or_insert(next)
        };
        let os: Vec<u32> = ol.iter().map(|&l| id(self.old.signature(l))).collect();
        let ns: Vec<u32> = nl.iter().map(|&k| id(self.new.signature(k))).collect();
        let mut runs = Vec::new();
        line_runs(
            &os,
            0..os.len(),
            &ns,
            0..ns.len(),
            0,
            &mut self.budget,
            &mut runs,
        );
        for r in runs {
            for t in 0..r.len {
                self.pair_edited(ol[r.o + t], nl[r.n + t]);
            }
        }
    }

    /// Пара правленых строк `l` и `k`: метки — дифф символов двух строк (запасной путь — по
    /// порядку появления).
    fn pair_edited(&mut self, l: usize, k: usize) {
        self.old_used[l] = true;
        self.new_used[k] = true;
        if !self.tokens_diff(&[l], &[k]) {
            self.ordered(&[l], &[k]);
        }
    }

    /// Правленые строки (на месте и перенесённые): свободные строки с метками в пары по похожести
    /// слов ([`similarity`]), самые похожие первыми; при равной — строка того же изменённого
    /// участка, затем ближайшая по месту в тексте. Новая строка с другими словами
    /// («x ‹secret›» вместо удалённой «token: ‹secret›») пары не получает. Кандидаты — строки того
    /// же участка (если их не больше [`SIMILAR_CANDIDATES`]) и строки с общими словами (слова
    /// чаще [`SIMILAR_POSTINGS_MAX`] строк кандидатов не дают).
    fn similar_lines(&mut self) {
        let olds: Vec<usize> = (0..self.old.lines.len())
            .filter(|&l| self.old_free(l))
            .collect();
        if olds.is_empty() {
            return;
        }
        let news: Vec<usize> = (0..self.new.lines.len())
            .filter(|&k| self.new_free(k))
            .collect();
        if news.is_empty() {
            return;
        }
        let old_words: Vec<Words> = olds.iter().map(|&l| self.old.line_words(l)).collect();
        let mut index: HashMap<&str, Vec<u32>> = HashMap::new();
        let mut by_block: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, w) in old_words.iter().enumerate() {
            for &word in w.keys() {
                index.entry(word).or_default().push(i as u32);
            }
            let b = self.old_block[olds[i]];
            if b != NONE {
                by_block.entry(b).or_default().push(i as u32);
            }
        }
        let (on, nn) = (self.old.lines.len(), self.new.lines.len());
        let mut cands: Vec<Cand> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        let mut mine: Vec<Cand> = Vec::new();
        for k in news {
            let nw = self.new.line_words(k);
            let block = self.new_block[k];
            seen.clear();
            if let Some(same) = by_block.get(&block)
                && same.len() <= SIMILAR_CANDIDATES
            {
                seen.extend(same);
            }
            // порядок — по длине и слову: обход `HashMap` случаен, итог — нет (повтор запроса)
            let mut lists: Vec<(usize, &str, &Vec<u32>)> = nw
                .keys()
                .filter_map(|w| index.get(w).map(|p| (p.len(), *w, p)))
                .filter(|(n, _, _)| *n <= SIMILAR_POSTINGS_MAX)
                .collect();
            lists.sort_unstable();
            let cap = seen.len() + SIMILAR_CANDIDATES;
            'lists: for (_, _, p) in lists {
                for &i in p {
                    if !seen.contains(&i) {
                        seen.push(i);
                        if seen.len() == cap {
                            break 'lists;
                        }
                    }
                }
            }
            mine.clear();
            for &i in &seen {
                let s = similarity(&old_words[i as usize], &nw);
                if s > 0 {
                    let l = olds[i as usize];
                    mine.push((
                        std::cmp::Reverse(s),
                        block == NONE || self.old_block[l] != block,
                        self.old_sec[l] != self.new_sec[k],
                        (l * nn).abs_diff(k * on),
                        k,
                        l,
                    ));
                }
            }
            mine.sort_unstable();
            cands.extend(mine.iter().take(SIMILAR_KEEP));
        }
        cands.sort_unstable();
        for (_, _, _, _, k, l) in cands {
            if !self.old_used[l] && !self.new_used[k] {
                self.pair_edited(l, k);
            }
        }
    }

    /// Остаток изменённого участка — строки с метками без пары (и уже сопоставленные, где пара
    /// есть не у всех: строки склеены или разделены): дифф символов по всем вместе (строка
    /// переписана другими словами), запасной путь — по порядку появления.
    fn rest_of_block(&mut self, o: Range<usize>, n: Range<usize>) {
        let ol: Vec<usize> = o
            .filter(|&l| self.old.line_marks(l).any(|a| !self.old_paired[a]))
            .collect();
        let nl: Vec<usize> = n
            .filter(|&k| self.new.line_marks(k).any(|b| self.pair[b] == NONE))
            .collect();
        if ol.is_empty() || nl.is_empty() {
            return;
        }
        if !self.tokens_diff(&ol, &nl) {
            self.ordered(&ol, &nl);
        }
    }

    /// Дифф символов строк `ol` и `nl`; `false` — участок больше предела или работа исчерпана
    /// (ничего не сопоставлено).
    fn tokens_diff(&mut self, ol: &[usize], nl: &[usize]) -> bool {
        let Some(ot) = self.old.tokens(ol, TOKEN_MYERS_MAX) else {
            return false;
        };
        let Some(nt) = self.new.tokens(nl, TOKEN_MYERS_MAX) else {
            return false;
        };
        let size = ot.len() + nt.len();
        let cost = (size as u64).pow(2);
        if size > TOKEN_MYERS_MAX || cost > self.budget {
            return false;
        }
        self.budget -= cost;
        let mut hook = TokPairs {
            old: &ot,
            new: &nt,
            equal: Vec::new(),
        };
        let Ok(()) = myers::diff(&mut hook, &ot[..], 0..ot.len(), &nt[..], 0..nt.len());
        for (a, b) in hook.equal {
            self.link_checked(a, b);
        }
        true
    }

    /// Запасной путь: метки строк `ol` и `nl` без пары — по порядку появления, если их поровну
    /// (и только с тем же словом рядом).
    fn ordered(&mut self, ol: &[usize], nl: &[usize]) {
        let om: Vec<usize> = ol
            .iter()
            .flat_map(|&l| self.old.line_marks(l))
            .filter(|&a| !self.old_paired[a])
            .collect();
        let nm: Vec<usize> = nl
            .iter()
            .flat_map(|&k| self.new.line_marks(k))
            .filter(|&b| self.pair[b] == NONE)
            .collect();
        if om.len() == nm.len() {
            for (a, b) in om.into_iter().zip(nm) {
                self.link_checked(a, b);
            }
        }
    }
}

/// Кандидат в пару строк, упорядоченный по возрастанию: похожесть по убыванию, другой участок,
/// другая секция, расстояние по месту, строка нового, строка старого (полный порядок — итог
/// детерминирован).
type Cand = (std::cmp::Reverse<u32>, bool, bool, usize, usize, usize);

/// Пары меток в неизменённых кусках диффа символов (проверка слов — у вызывающего).
struct TokPairs<'p, 'x> {
    old: &'p [Tok<'x>],
    new: &'p [Tok<'x>],
    equal: Vec<(usize, usize)>,
}

impl DiffHook for TokPairs<'_, '_> {
    type Error = Infallible;

    fn equal(&mut self, o: usize, n: usize, len: usize) -> Result<(), Infallible> {
        for i in 0..len {
            let (a, b) = (self.old[o + i].mark, self.new[n + i].mark);
            if a != NONE && b != NONE {
                self.equal.push((a as usize, b as usize));
            }
        }
        Ok(())
    }
}

/// Неизменённые участки диффа строк — в `out`.
struct Runs<'p>(&'p mut Vec<Run>);

impl DiffHook for Runs<'_> {
    type Error = Infallible;

    fn equal(&mut self, o: usize, n: usize, len: usize) -> Result<(), Infallible> {
        self.0.push(Run { o, n, len });
        Ok(())
    }
}

/// Неизменённые участки строк всего текста: сначала опоры — заголовки Markdown, единственные в
/// обоих текстах (секции сопоставляются как секции: перенесённая через заголовок строка не
/// перетягивает соседние), затем [`line_runs`] внутри секций. Возвращает сопоставленные
/// заголовки (строка старого, строка нового) по возрастанию.
fn section_runs(
    old: &[u32],
    new: &[u32],
    heads: &[bool],
    budget: &mut u64,
    out: &mut Vec<Run>,
) -> Vec<(usize, usize)> {
    let anchors = unique_lis(old, 0..old.len(), new, 0..new.len(), |id| {
        heads[id as usize]
    });
    let (mut po, mut pn) = (0, 0);
    for &(ao, an) in &anchors {
        line_runs(old, po..ao, new, pn..an, 0, budget, out);
        out.push(Run {
            o: ao,
            n: an,
            len: 1,
        });
        po = ao + 1;
        pn = an + 1;
    }
    line_runs(old, po..old.len(), new, pn..new.len(), 0, budget, out);
    anchors
}

/// Неизменённые участки строк `old[o]` и `new[n]` по возрастанию (patience diff): общие начало и
/// конец; середина делится по строкам, единственным в обоих участках (наибольшая возрастающая
/// подпоследовательность), до глубины [`ANCHOR_DEPTH`]; участок без таких строк до
/// [`LINE_MYERS_MAX`] строк — Майерс (в пределах работы `budget`), длиннее — целиком изменённый.
fn line_runs(
    old: &[u32],
    mut o: Range<usize>,
    new: &[u32],
    mut n: Range<usize>,
    depth: u32,
    budget: &mut u64,
    out: &mut Vec<Run>,
) {
    let p = old[o.clone()]
        .iter()
        .zip(&new[n.clone()])
        .take_while(|(a, b)| a == b)
        .count();
    if p > 0 {
        out.push(Run {
            o: o.start,
            n: n.start,
            len: p,
        });
        o.start += p;
        n.start += p;
    }
    let s = old[o.clone()]
        .iter()
        .rev()
        .zip(new[n.clone()].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    o.end -= s;
    n.end -= s;
    if !o.is_empty() && !n.is_empty() {
        // сначала опоры — строки, единственные в обоих участках (заголовки, строки с номерами):
        // одинаковые строки разных секций не сопоставляются через чужой заголовок
        let anchors = if depth < ANCHOR_DEPTH {
            unique_lis(old, o.clone(), new, n.clone(), |_| true)
        } else {
            Vec::new()
        };
        if !anchors.is_empty() {
            let (mut po, mut pn) = (o.start, n.start);
            for (ao, an) in anchors {
                line_runs(old, po..ao, new, pn..an, depth + 1, budget, out);
                out.push(Run {
                    o: ao,
                    n: an,
                    len: 1,
                });
                po = ao + 1;
                pn = an + 1;
            }
            line_runs(old, po..o.end, new, pn..n.end, depth + 1, budget, out);
        } else {
            let size = o.len() + n.len();
            let cost = (size as u64).pow(2);
            if size <= LINE_MYERS_MAX && cost <= *budget {
                *budget -= cost;
                let Ok(()) = myers::diff(&mut Runs(out), old, o.clone(), new, n.clone());
            }
        }
    }
    if s > 0 {
        out.push(Run {
            o: o.end,
            n: n.end,
            len: s,
        });
    }
}

/// Строки (из отобранных `keep`), единственные и в `old[o]`, и в `new[n]`, в наибольшей
/// последовательности, возрастающей в обоих: пары (строка старого, строка нового).
fn unique_lis(
    old: &[u32],
    o: Range<usize>,
    new: &[u32],
    n: Range<usize>,
    keep: impl Fn(u32) -> bool,
) -> Vec<(usize, usize)> {
    // номер строки → (раз в старом, где, раз в новом, где)
    let mut seen: HashMap<u32, (u32, usize, u32, usize)> = HashMap::with_capacity(o.len());
    for i in o {
        if !keep(old[i]) {
            continue;
        }
        let e = seen.entry(old[i]).or_insert((0, 0, 0, 0));
        e.0 += 1;
        e.1 = i;
    }
    for j in n {
        if let Some(e) = seen.get_mut(&new[j]) {
            e.2 += 1;
            e.3 = j;
        }
    }
    let mut pairs: Vec<(usize, usize)> = seen
        .into_values()
        .filter(|e| e.0 == 1 && e.2 == 1)
        .map(|e| (e.1, e.3))
        .collect();
    pairs.sort_unstable_by_key(|p| p.1);
    // наибольшая возрастающая по строке старого (по новому пары уже упорядочены)
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; pairs.len()];
    for i in 0..pairs.len() {
        let k = tails.partition_point(|&t| pairs[t].0 < pairs[i].0);
        if k > 0 {
            prev[i] = tails[k - 1];
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(pairs[i]);
        cur = (prev[i] != usize::MAX).then_some(prev[i]);
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests;
