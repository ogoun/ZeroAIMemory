//! Восстановление значений под `‹secret›` при записи агентом (спецификация M7 §4.3.2, задача 6
//! M7.1). Агент по токену читает текст с маской ([`super::agent_view`]) и, правя его, присылает
//! обратно метки `‹secret›` вместо значений; без восстановления запись затёрла бы значение
//! литералом.
//!
//! Правило: база — текст, по которому агент правил (ревизия `If-Match`/`base_rev` заметки,
//! текущая запись журнала, замещаемая запись); её вид `masked_base` = `mask_text(base)`, и у
//! каждой метки вида известно исходное значение ([`MaskedBase`]). Новый текст выравнивается с
//! видом:
//! 1. строки — дифф строк ([`similar`], Майерс): неизменённая строка напротив строки вида
//!    возвращает значения своих меток по порядку; длинный участок без малого диффа делится на
//!    куски по строкам, единственным в обоих текстах (как в patience diff), — время близко к
//!    линейному и на заметке 5 МиБ;
//! 2. перенос строки целиком (строка с метками, удалённая в одном месте и вставленная без
//!    изменений в другом) — та же строка, значения с ней [решение исполнителя, задача 6];
//! 3. правленые строки (на месте или перенесённые) — пары строк с метками по похожести слов,
//!    самые похожие первыми (удалённая соседка правленой строки ей не пара) [решение
//!    исполнителя, задача 6], затем дифф «символов» внутри пары: слова, пробелы и знаки, метка —
//!    один знак; метка нового текста в неизменённом куске напротив метки вида получает её
//!    значение;
//! 4. остаток изменённого участка (строка переписана другими словами, строки склеены) — дифф
//!    символов по всем его строкам с метками без пары; участок больше предела диффа — метки по
//!    порядку появления, если их поровну; иначе метки остаются как есть.
//!
//! Метка без пары (дописана агентом, пришла из другого текста) записывается как есть и
//! считается в `kept`: ответ записи получает предупреждение [`SECRET_MARK_KEPT`] — данных секрета
//! там нет, терять нечего (решение координатора §4.3.2 п. 4). Литерал `‹secret›`, уже лежавший в
//! базе, — метка со значением «сама метка»: напротив себя он не считается новым.
//!
//! Результат детерминирован (нет сроков по часам): пределы работы диффа — по размерам входа, так
//! что повтор того же запроса (`Idempotency-Key` у `PUT`) даёт тот же текст.
//!
//! Человек в сессии WebUI видит исходник и пишет без восстановления ([`super::agent_view::masks`]).
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::ops::Range;

use serde_json::{Value, json};
use similar::algorithms::{DiffHook, myers};
use zam_core::Id;
use zam_markdown::{SECRET_MARK, scan_secrets};
use zam_store::{Db, StoreError, ViewedText, node_content};

use super::agent_view::{self, BLOCKING_FROM};
use crate::auth::Principal;
use crate::error::ApiError;

/// Предупреждение ответа записи: в новом тексте есть `‹secret›` без пары в базе — записаны как
/// есть; число таких мест — в строке предупреждения и в поле ответа с тем же именем.
pub const SECRET_MARK_KEPT: &str = "secret_mark_kept";

/// Итог восстановления: текст для хранилища и число меток нового текста без пары в базе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub text: String,
    pub kept: usize,
}

/// Нет пары (индекс метки базы).
const NONE: u32 = u32::MAX;
/// Участок строк длиннее (строк в обоих текстах) — без диффа Майерса, делится по уникальным
/// строкам.
const LINE_MYERS_MAX: usize = 4096;
/// Участок изменённых строк длиннее (токенов в обоих текстах) — без диффа символов.
const TOKEN_MYERS_MAX: usize = 8192;
/// Глубина деления по уникальным строкам.
const ANCHOR_DEPTH: u32 = 4;
/// Слово чаще (строк старого текста без пар) — не ищет пару строке нового (`similar_lines`):
/// частые слова («строка», «token») похожести не добавляют, а перебор растёт.
const SIMILAR_POSTINGS_MAX: usize = 64;
/// Кандидатов в пару строке нового текста — не больше (сначала по самым редким словам).
const SIMILAR_CANDIDATES: usize = 64;
/// Лучших кандидатов строки — в общий выбор пар.
const SIMILAR_KEEP: usize = 4;
/// Предел работы всех диффов одного восстановления: сумма `(n + m)²` по вызовам Майерса (оценка
/// сверху его `O((n + m)·D)`); исчерпан — остальные участки идут запасным путём (п. 4).
const WORK_BUDGET: u64 = 200_000_000;

/// Восстановление нового текста `new_masked`, правленного агентом по виду базы `base`.
pub fn restore(base: &str, new_masked: &str) -> Restored {
    if !new_masked.contains(SECRET_MARK) {
        return Restored {
            text: new_masked.to_owned(),
            kept: 0,
        };
    }
    MaskedBase::new(base).restore(new_masked)
}

/// [`restore`] своих текстов: вместе больше [`BLOCKING_FROM`] — в пуле блокирующих задач (пул
/// снят — 503).
pub async fn restore_owned(base: String, new_masked: String) -> Result<Restored, ApiError> {
    if !new_masked.contains(SECRET_MARK) {
        return Ok(Restored {
            text: new_masked,
            kept: 0,
        });
    }
    if base.len() + new_masked.len() <= BLOCKING_FROM {
        return Ok(restore(&base, &new_masked));
    }
    agent_view::blocking(move || restore(&base, &new_masked)).await
}

/// Текст `PUT` узла `id` от принципала `p` по ревизии `base_rev` (`If-Match`): агенту —
/// восстановление по этой ревизии, человеку — как есть. Ревизии нет — как есть: хранилище всё
/// равно отклонит запись (409).
pub async fn node_text(
    p: &Principal,
    db: &Db,
    id: Id,
    base_rev: i64,
    content: String,
) -> Result<Restored, ApiError> {
    if !agent_view::masks(p) || !content.contains(SECRET_MARK) {
        return Ok(Restored {
            text: content,
            kept: 0,
        });
    }
    let base = match node_content(db, id, Some(base_rev)).await {
        Ok((_, t)) => t,
        Err(StoreError::NotFound) => {
            return Ok(Restored {
                text: content,
                kept: 0,
            });
        }
        Err(e) => return Err(e.into()),
    };
    restore_owned(base, content).await
}

/// Вид ревизии для PATCH агента ([`zam_store::patch_note_viewed`]): операции — по маске,
/// итог — обратно с значениями.
pub fn view(raw: &str) -> Box<dyn ViewedText + '_> {
    Box::new(MaskedBase::new(raw))
}

/// Предупреждение [`SECRET_MARK_KEPT`] в ответ-объект `v` (строка `warnings` и число); при
/// `kept == 0` ответ не меняется.
pub fn attach_kept(v: &mut Value, kept: usize) {
    if kept == 0 {
        return;
    }
    super::glossary::attach_warnings(v, vec![kept_warning(kept)]);
    if let Value::Object(o) = v {
        o.insert(SECRET_MARK_KEPT.into(), json!(kept));
    }
}

/// Строка предупреждения [`SECRET_MARK_KEPT`].
pub fn kept_warning(kept: usize) -> String {
    format!(
        "{SECRET_MARK_KEPT}: {kept} — метка ‹secret› без прежнего значения на этом месте записана \
         как есть; ключ — из Vault (memory_vault get)"
    )
}

/// Меток `‹secret›` в тексте `s` (весь текст новый: дописка, новое поле).
pub fn marks_in(s: &str) -> usize {
    s.matches(SECRET_MARK).count()
}

/// Строка `new` поля, прежний текст которого `base` (`None` — поля не было): восстановление на
/// месте; меток без пары.
pub fn restore_in_place(base: Option<&str>, new: &mut String) -> usize {
    if !new.contains(SECRET_MARK) {
        return 0;
    }
    let r = restore(base.unwrap_or(""), new);
    *new = r.text;
    r.kept
}

/// Значение `data` записи журнала, правленное агентом по маске прежнего `base` (маска выдачи —
/// у каждой строки своя, [`agent_view::mask_value`]): строка — по строке на том же пути (ключи
/// объекта, индекс массива); строка массива, вид которой целиком совпал с видом строки прежнего
/// массива на другом месте, — та строка (элемент переставлен); каждая прежняя строка — пара не
/// больше чем одной новой. Меток без пары.
pub fn restore_value(base: &Value, new: &mut Value) -> usize {
    match new {
        Value::String(s) => restore_in_place(base.as_str(), s),
        Value::Array(a) => {
            let olds: Vec<&Value> = base
                .as_array()
                .map(|v| v.iter().collect())
                .unwrap_or_default();
            let mut list = Listed::new(olds.iter().map(|v| v.as_str()).collect());
            a.iter_mut()
                .enumerate()
                .map(|(i, x)| match x {
                    Value::String(s) => list.restore(i, s),
                    x => restore_value(olds.get(i).copied().unwrap_or(&Value::Null), x),
                })
                .sum()
        }
        Value::Object(o) => o
            .iter_mut()
            .map(|(k, x)| restore_value(base.get(k).unwrap_or(&Value::Null), x))
            .sum(),
        _ => 0,
    }
}

/// Теги записи, правленные по маске прежних `base` (как строки массива [`restore_value`]).
pub fn restore_tags(base: &[String], new: &mut [String]) -> usize {
    let mut list = Listed::new(base.iter().map(|s| Some(s.as_str())).collect());
    new.iter_mut()
        .enumerate()
        .map(|(i, s)| list.restore(i, s))
        .sum()
}

/// Строки прежнего массива для строк нового: вид каждой (маска — по требованию) и занята ли она.
struct Listed<'b> {
    olds: Vec<Option<&'b str>>,
    shown: Vec<Option<String>>,
    taken: Vec<bool>,
}

impl<'b> Listed<'b> {
    fn new(olds: Vec<Option<&'b str>>) -> Self {
        let n = olds.len();
        Listed {
            olds,
            shown: vec![None; n],
            taken: vec![false; n],
        }
    }

    /// Вид прежней строки `j` совпал с `s` целиком (`j` не занята).
    fn same(&mut self, j: usize, s: &str) -> bool {
        let Some(b) = self.olds[j] else {
            return false;
        };
        if self.taken[j] {
            return false;
        }
        self.shown[j]
            .get_or_insert_with(|| agent_view::mask(b).into_owned())
            .as_str()
            == s
    }

    /// Строка `i` нового массива: вид совпал целиком (на том же месте или на другом) — прежняя
    /// строка; иначе — восстановление по строке того же места.
    fn restore(&mut self, i: usize, s: &mut String) -> usize {
        if !s.contains(SECRET_MARK) {
            return 0;
        }
        let n = self.olds.len();
        let found = (i < n && self.same(i, s))
            .then_some(i)
            .or_else(|| (0..n).find(|&j| self.same(j, s)));
        if let Some(j) = found {
            self.taken[j] = true;
            *s = self.olds[j].unwrap_or_default().to_owned();
            return 0;
        }
        let here = self
            .olds
            .get(i)
            .copied()
            .flatten()
            .filter(|_| !self.taken[i]);
        if here.is_some() {
            self.taken[i] = true;
        }
        restore_in_place(here, s)
    }
}

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
    /// Вид `raw` — как [`zam_markdown::mask_text`] (те же находки и тот же пропуск неверных
    /// диапазонов, что у `mask_secrets`).
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
        let new_marks: Vec<usize> = edited.match_indices(SECRET_MARK).map(|(i, _)| i).collect();
        if new_marks.is_empty() {
            return Restored {
                text: edited.to_owned(),
                kept: 0,
            };
        }
        if edited == self.masked {
            return Restored {
                text: self.raw.to_owned(),
                kept: 0,
            };
        }
        let pair = if self.marks.is_empty() {
            vec![NONE; new_marks.len()]
        } else {
            let old_marks: Vec<usize> = self.marks.iter().map(|m| m.0).collect();
            align(&self.masked, &old_marks, edited, &new_marks)
        };
        let extra: usize = pair
            .iter()
            .filter(|&&a| a != NONE)
            .map(|&a| self.marks[a as usize].1.len())
            .sum();
        let mut text = String::with_capacity(edited.len() + extra);
        let (mut pos, mut kept) = (0, 0);
        for (&at, &a) in new_marks.iter().zip(&pair) {
            text.push_str(&edited[pos..at]);
            if a == NONE {
                text.push_str(SECRET_MARK);
                kept += 1;
            } else {
                text.push_str(&self.raw[self.marks[a as usize].1.clone()]);
            }
            pos = at + SECRET_MARK.len();
        }
        text.push_str(&edited[pos..]);
        Restored { text, kept }
    }
}

impl ViewedText for MaskedBase<'_> {
    fn view(&self) -> &str {
        &self.masked
    }

    fn write_back(&self, edited: &str) -> (String, usize) {
        let r = self.restore(edited);
        (r.text, r.kept)
    }
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
fn align(old: &str, old_marks: &[usize], new: &str, new_marks: &[usize]) -> Vec<u32> {
    let mut intern: HashMap<&str, u32> = HashMap::new();
    let old = Side::new(old, old_marks, &mut intern);
    let new = Side::new(new, new_marks, &mut intern);
    let mut a = Aligner {
        pair: vec![NONE; new_marks.len()],
        old_paired: vec![false; old_marks.len()],
        old_used: vec![false; old.lines.len()],
        new_used: vec![false; new.lines.len()],
        old,
        new,
        budget: WORK_BUDGET,
    };
    a.run();
    a.pair
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
}

impl<'x> Side<'x> {
    fn new(text: &'x str, marks: &'x [usize], intern: &mut HashMap<&'x str, u32>) -> Self {
        let mut lines = Vec::new();
        let mut ids = Vec::new();
        let mut first = Vec::new();
        let (mut start, mut m) = (0usize, 0usize);
        for piece in text.split_inclusive('\n') {
            let end = start + piece.len();
            first.push(m as u32);
            while m < marks.len() && marks[m] < end {
                m += 1;
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
        }
    }

    /// Индексы меток строки `l`.
    fn line_marks(&self, l: usize) -> Range<usize> {
        self.first[l] as usize..self.first[l + 1] as usize
    }

    /// Слова строки `l` (буквы, цифры, `_` подряд) без слов меток: слово → сколько раз.
    fn line_words(&self, l: usize) -> HashMap<&'x str, u32> {
        let mut out: HashMap<&'x str, u32> = HashMap::new();
        let end = self.lines[l].end;
        let mut pos = self.lines[l].start;
        let mut piece = |s: &'x str| {
            for w in s
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
            {
                *out.entry(w).or_insert(0) += 1;
            }
        };
        for mi in self.line_marks(l) {
            let at = self.marks[mi];
            piece(&self.text[pos..at]);
            pos = at + SECRET_MARK.len();
        }
        piece(&self.text[pos..end]);
        out
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
        if c.is_alphanumeric() || c == '_' {
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

struct Aligner<'x> {
    old: Side<'x>,
    new: Side<'x>,
    /// Пара каждой метки нового текста — индекс метки старого или [`NONE`].
    pair: Vec<u32>,
    /// Метка старого текста уже в паре.
    old_paired: Vec<bool>,
    /// Строка уже сопоставлена целиком (неизменённая, перенесённая или похожая правленая).
    old_used: Vec<bool>,
    new_used: Vec<bool>,
    budget: u64,
}

impl Aligner<'_> {
    /// Шаги выравнивания (описание модуля): неизменённые строки, перенос, похожие строки,
    /// остаток изменённых участков.
    fn run(&mut self) {
        let mut runs = Vec::new();
        line_runs(
            &self.old.ids,
            0..self.old.ids.len(),
            &self.new.ids,
            0..self.new.ids.len(),
            0,
            &mut self.budget,
            &mut runs,
        );
        for r in &runs {
            for t in 0..r.len {
                self.pair_lines(r.o + t, r.n + t);
            }
        }
        self.moves();
        let mut blocks = Vec::with_capacity(runs.len() + 1);
        let (mut po, mut pn) = (0, 0);
        let tail = Run {
            o: self.old.lines.len(),
            n: self.new.lines.len(),
            len: 0,
        };
        for r in runs.iter().chain(std::iter::once(&tail)) {
            if po < r.o && pn < r.n {
                blocks.push((po..r.o, pn..r.n));
            }
            po = r.o + r.len;
            pn = r.n + r.len;
        }
        self.similar_lines();
        for (o, n) in blocks {
            self.rest_of_block(o, n);
        }
    }

    fn link(&mut self, a: usize, b: usize) {
        self.pair[b] = a as u32;
        self.old_paired[a] = true;
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

    /// Пара правленых строк `l` и `k`: метки — дифф символов двух строк (запасной путь — по
    /// порядку появления).
    fn pair_edited(&mut self, l: usize, k: usize) {
        self.old_used[l] = true;
        self.new_used[k] = true;
        if !self.tokens_diff(&[l], &[k]) {
            self.ordered(&[l], &[k]);
        }
    }

    /// Правленые строки (на месте, перенесённые, в большом изменённом участке): свободные строки
    /// с метками старого и нового текста — в пары по похожести слов ([`similarity`]), самые
    /// похожие первыми, при равной — ближайшие по месту в тексте [решение исполнителя, задача 6].
    /// Удалённая соседка правленой строки ей не пара: у правленой строки больше общих слов с
    /// прежней собой. Новая строка с другими словами («x ‹secret›» вместо удалённой
    /// «token: ‹secret›») пары не получает. Кандидаты — по общим словам, слова чаще
    /// [`SIMILAR_POSTINGS_MAX`] строк кандидатов не дают.
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
        for (i, w) in old_words.iter().enumerate() {
            for &word in w.keys() {
                index.entry(word).or_default().push(i as u32);
            }
        }
        let (on, nn) = (self.old.lines.len(), self.new.lines.len());
        // (похожесть, расстояние по месту, строка нового, строка старого)
        let mut cands: Vec<(u32, usize, usize, usize)> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        let mut mine: Vec<(u32, usize, usize, usize)> = Vec::new();
        for k in news {
            let nw = self.new.line_words(k);
            // кандидаты — сначала по редким словам, не больше SIMILAR_CANDIDATES
            // порядок — по длине и слову: обход `HashMap` случаен, итог — нет (повтор запроса)
            let mut lists: Vec<(usize, &str, &Vec<u32>)> = nw
                .keys()
                .filter_map(|w| index.get(w).map(|p| (p.len(), *w, p)))
                .filter(|(n, _, _)| *n <= SIMILAR_POSTINGS_MAX)
                .collect();
            lists.sort_unstable();
            seen.clear();
            'lists: for (_, _, p) in lists {
                for &i in p {
                    if !seen.contains(&i) {
                        seen.push(i);
                        if seen.len() == SIMILAR_CANDIDATES {
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
                    mine.push((s, (l * nn).abs_diff(k * on), k, l));
                }
            }
            mine.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.3.cmp(&b.3)));
            cands.extend(mine.iter().take(SIMILAR_KEEP));
        }
        cands.sort_unstable_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        for (_, _, k, l) in cands {
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
    /// несопоставленной строке старого, — пара (одинаковые — по порядку появления).
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
        for k in 0..self.new.lines.len() {
            if self.new_used[k] || self.new.line_marks(k).is_empty() {
                continue;
            }
            if let Some(l) = pool.get_mut(&self.new.ids[k]).and_then(VecDeque::pop_front) {
                self.pair_lines(l, k);
            }
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
            pair: &mut self.pair,
            old_paired: &mut self.old_paired,
        };
        let Ok(()) = myers::diff(&mut hook, &ot[..], 0..ot.len(), &nt[..], 0..nt.len());
        true
    }

    /// Запасной путь: метки строк `ol` и `nl` без пары — по порядку появления, если их поровну.
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
                self.link(a, b);
            }
        }
    }
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

/// Пары меток в неизменённых кусках диффа символов.
struct TokPairs<'p, 'x> {
    old: &'p [Tok<'x>],
    new: &'p [Tok<'x>],
    pair: &'p mut [u32],
    old_paired: &'p mut [bool],
}

impl DiffHook for TokPairs<'_, '_> {
    type Error = Infallible;

    fn equal(&mut self, o: usize, n: usize, len: usize) -> Result<(), Infallible> {
        for i in 0..len {
            let (a, b) = (self.old[o + i].mark, self.new[n + i].mark);
            if a != NONE
                && b != NONE
                && self.pair[b as usize] == NONE
                && !self.old_paired[a as usize]
            {
                self.pair[b as usize] = a;
                self.old_paired[a as usize] = true;
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

/// Неизменённые участки строк `old[o]` и `new[n]` по возрастанию: общие начало и конец; середина
/// до [`LINE_MYERS_MAX`] строк — Майерс (в пределах работы `budget`), длиннее — деление по
/// строкам, единственным в обоих участках (наибольшая возрастающая подпоследовательность, как в
/// patience diff), до глубины [`ANCHOR_DEPTH`]; глубже — участок целиком изменённый.
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
        let size = o.len() + n.len();
        let cost = (size as u64).pow(2);
        if size <= LINE_MYERS_MAX && cost <= *budget {
            *budget -= cost;
            let Ok(()) = myers::diff(&mut Runs(out), old, o.clone(), new, n.clone());
        } else if depth < ANCHOR_DEPTH {
            let anchors = unique_lis(old, o.clone(), new, n.clone());
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

/// Строки, единственные и в `old[o]`, и в `new[n]`, в наибольшей последовательности, возрастающей
/// в обоих: пары (строка старого, строка нового).
fn unique_lis(old: &[u32], o: Range<usize>, new: &[u32], n: Range<usize>) -> Vec<(usize, usize)> {
    // номер строки → (раз в старом, где, раз в новом, где)
    let mut seen: HashMap<u32, (u32, usize, u32, usize)> = HashMap::with_capacity(o.len());
    for i in o {
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
mod tests {
    use super::*;
    use zam_markdown::mask_text;

    /// Ключ из частей (в исходнике целиком его нет — push protection); `n` — отличие ключей.
    fn key(n: u32) -> String {
        format!("gh{}_{}{n:04}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF")
    }

    /// Пароль ключ-значения из частей.
    fn pw(n: u32) -> String {
        format!("{}{n}{}", "hunter", "Qz9x")
    }

    const M: &str = SECRET_MARK;

    /// Метки, оставшиеся напротив метки вида, получают значение; дописанное — как есть.
    #[test]
    fn restore_unchanged_marks() {
        let k = key(1);
        let base = format!("a token: {k} b");
        assert_eq!(mask_text(&base), format!("a token: {M} b"));
        let r = restore(&base, &format!("a token: {M} b c"));
        assert_eq!(r.text, format!("a token: {k} b c"));
        assert_eq!(r.kept, 0);
    }

    /// Строка с меткой правлена вокруг метки — значение на месте; соседние строки — как есть.
    #[test]
    fn restore_line_edited_around_mark() {
        let k = key(2);
        let base = format!("# Ключи\n\ntoken: {k}\n\nконец\n");
        let r = restore(&base, &format!("# Ключи\n\ntoken: {M} (prod)\n\nконец\n"));
        assert_eq!(r.text, format!("# Ключи\n\ntoken: {k} (prod)\n\nконец\n"));
        assert_eq!(r.kept, 0);
        // правка и перед меткой, и после; кириллица рядом с меткой
        let r = restore(
            &base,
            &format!("# Ключи\n\nboevoy token: {M}, не трогать\n\nконец\n"),
        );
        assert_eq!(
            r.text,
            format!("# Ключи\n\nboevoy token: {k}, не трогать\n\nконец\n")
        );
        assert_eq!(r.kept, 0);
    }

    /// Две строки с ключами переставлены — каждый ключ остаётся при своей строке (перенос
    /// строки целиком); переставлены и правлены — по порядку появления в участке.
    #[test]
    fn restore_two_secrets_order() {
        let (k1, k2) = (key(11), key(12));
        let base = format!("# K\n\na: {k1}\nb: {k2}\n\nхвост\n");
        let r = restore(&base, &format!("# K\n\nb: {M}\na: {M}\n\nхвост\n"));
        assert_eq!(r.text, format!("# K\n\nb: {k2}\na: {k1}\n\nхвост\n"));
        assert_eq!(r.kept, 0);
        // перенос далеко, через неизменённые строки
        let base = format!("a: {k1}\nx\ny\nz\nb: {k2}\n");
        let r = restore(&base, &format!("x\nb: {M}\ny\nz\na: {M}\n"));
        assert_eq!(r.text, format!("x\nb: {k2}\ny\nz\na: {k1}\n"));
        assert_eq!(r.kept, 0);
    }

    /// Новая строка с меткой — литерал и `kept`; прежняя — значение.
    #[test]
    fn restore_inserted_mark_kept() {
        let k = key(3);
        let base = format!("token: {k}\nконец\n");
        let r = restore(&base, &format!("token: {M}\nx {M}\nконец\n"));
        assert_eq!(r.text, format!("token: {k}\nx {M}\nконец\n"));
        assert_eq!(r.kept, 1);
        // дописано в ту же строку после метки — вторая метка без пары
        let r = restore(&base, &format!("token: {M} и ещё {M}\nконец\n"));
        assert_eq!(r.text, format!("token: {k} и ещё {M}\nконец\n"));
        assert_eq!(r.kept, 1);
    }

    /// Строка с ключом удалена — ключа нет, `kept == 0`; удалено всё, кроме метки другой строки.
    #[test]
    fn restore_deleted_secret() {
        let (k1, k2) = (key(4), key(5));
        let base = format!("a\ntoken: {k1}\nb\nkey2: {k2}\n");
        let r = restore(&base, &format!("a\nb\nkey2: {M}\n"));
        assert_eq!(r.text, format!("a\nb\nkey2: {k2}\n"));
        assert!(!r.text.contains(&k1));
        assert_eq!(r.kept, 0);
        let r = restore(&base, "a\nb\n");
        assert_eq!((r.text.as_str(), r.kept), ("a\nb\n", 0));
    }

    /// Быстрые пути: вид без правки — исходник; без меток — как есть; база без секретов — все
    /// метки как есть.
    #[test]
    fn restore_fast_paths() {
        let k = key(6);
        let base = format!("# K\n\ntoken: {k}\npassword = {}\n", pw(6));
        let masked = mask_text(&base);
        assert_eq!(restore(&base, &masked).text, base);
        assert_eq!(restore(&base, "новое без меток").text, "новое без меток");
        let r = restore("чистый текст\n", &format!("чистый текст {M}\n"));
        assert_eq!((r.text, r.kept), (format!("чистый текст {M}\n"), 1));
    }

    /// Литерал `‹secret›` базы — метка со значением «сама метка»: напротив себя — не новая.
    #[test]
    fn restore_literal_mark_in_base() {
        let k = key(7);
        let base = format!("было {M} и token: {k}\nконец\n");
        let r = restore(
            &base,
            &format!("было {M} и token: {M} (prod)\nконец\nещё\n"),
        );
        assert_eq!(
            r.text,
            format!("было {M} и token: {k} (prod)\nконец\nещё\n")
        );
        assert_eq!(r.kept, 0);
    }

    /// Вид [`MaskedBase`] — ровно `mask_text`, метки вида — все `‹secret›` в нём.
    #[test]
    fn masked_base_is_mask_text() {
        let k = key(8);
        for raw in [
            String::new(),
            "без секретов\n".into(),
            format!("token: {k}\n"),
            format!("{M}{k} {M}\nhttps://u:{}@h.example.com/\n", pw(1)),
            format!("```bash\nmysql -p{} db\n```\nтекст {M}", pw(2)),
            format!("‹secret{k} secret›"),
        ] {
            let b = MaskedBase::new(&raw);
            assert_eq!(b.masked(), mask_text(&raw), "{raw}");
            let at: Vec<usize> = b.masked.match_indices(M).map(|(i, _)| i).collect();
            let ours: Vec<usize> = b.marks.iter().map(|m| m.0).collect();
            assert_eq!(at, ours, "{raw}");
        }
    }

    /// Псевдослучайный генератор (LCG) — воспроизводимые правки.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n.max(1)
        }
    }

    /// Строка `i` исходника: проза, ключ, пароль ключ-значения, пароль в URL или два секрета.
    fn line(i: u32, kind: usize) -> String {
        match kind {
            0 => format!("строка {i}: обычный текст про сборку\n"),
            1 => format!("ключ {i}: {}\n", key(i)),
            2 => format!("строка {i}: password = {}\n", pw(i)),
            3 => format!("строка {i}: https://user:{}@db.example.com/x\n", pw(i)),
            _ => format!("строка {i}: {} и password = {}\n", key(i), pw(i)),
        }
    }

    /// Случайные правки агента по виду — вставка и удаление строк, правка до и после меток,
    /// перенос строки (и перенос правленой) — дают ровно тот же текст, что те же правки
    /// исходника; меток без пары нет. Зерно и число случаев — `T6_SEED`, `T6_CASES` (проба
    /// на 3 × 20 000 случаев — журнал этапа, задача 6).
    #[test]
    fn restore_random_line_edits() {
        let env = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<u64>().ok());
        let mut rng = Rng(env("T6_SEED").unwrap_or(7));
        for case in 0..env("T6_CASES").unwrap_or(1000) as u32 {
            let n = 5 + rng.next(40);
            let raw_lines: Vec<String> = (0..n as u32)
                .map(|i| line(case * 100 + i, rng.next(5)))
                .collect();
            let base: String = raw_lines.concat();
            let masked = mask_text(&base);
            let mut raw: Vec<String> = raw_lines.clone();
            let mut seen: Vec<String> = masked.split_inclusive('\n').map(str::to_owned).collect();
            assert_eq!(raw.len(), seen.len(), "строки вида — как строки исходника");
            // (правлена, перенесена): каждая строка — не больше одной правки и одного переноса
            let mut done = vec![(false, false); raw.len()];
            for e in 0..1 + rng.next(6) {
                let len = raw.len();
                match rng.next(5) {
                    0 => {
                        let at = rng.next(len + 1);
                        let t = format!("вставка {case}-{e}\n");
                        raw.insert(at, t.clone());
                        seen.insert(at, t);
                        done.insert(at, (true, true));
                    }
                    1 if len > 1 => {
                        let at = rng.next(len);
                        raw.remove(at);
                        seen.remove(at);
                        done.remove(at);
                    }
                    2 | 3 => {
                        let at = rng.next(len);
                        if done[at].0 {
                            continue;
                        }
                        let edit = |s: &mut String| {
                            if e % 2 == 0 {
                                s.insert_str(s.len() - 1, &format!(" (правка {e})"));
                            } else {
                                s.insert_str(0, "- ");
                            }
                        };
                        edit(&mut raw[at]);
                        edit(&mut seen[at]);
                        done[at].0 = true;
                    }
                    _ => {
                        let from = rng.next(len);
                        if done[from].1 {
                            continue;
                        }
                        let (r, s, d) = (raw.remove(from), seen.remove(from), done.remove(from));
                        let to = rng.next(len);
                        raw.insert(to, r);
                        seen.insert(to, s);
                        done.insert(to, (d.0, true));
                    }
                }
            }
            let (want, edited) = (raw.concat(), seen.concat());
            let r = restore(&base, &edited);
            assert_eq!(r.text, want, "случай {case}:\n{edited}");
            assert_eq!(r.kept, 0, "случай {case}");
        }
    }

    /// Каждая строка большой заметки правлена (цитата `> `): участок целиком изменённый,
    /// больше предела диффа символов — строки попарно; все значения на месте.
    #[test]
    fn restore_every_line_rewritten() {
        let lines: Vec<String> = (0..6000).map(|i| line(i, (i % 5) as usize)).collect();
        let base = lines.concat();
        let edited: String = mask_text(&base)
            .split_inclusive('\n')
            .map(|l| format!("> {l}"))
            .collect();
        let want: String = lines.iter().map(|l| format!("> {l}")).collect();
        let r = restore(&base, &edited);
        assert_eq!(r.kept, 0);
        assert!(r.text == want, "значения не на месте");
    }

    /// Большой изменённый участок с другим числом строк (строки склеены попарно) и тем же числом
    /// меток — метки по порядку появления.
    #[test]
    fn restore_big_block_ordered() {
        let lines: Vec<String> = (0..3000).map(|i| line(i, 1 + (i % 4) as usize)).collect();
        let base = lines.concat();
        let seen: Vec<String> = mask_text(&base)
            .split_inclusive('\n')
            .map(str::to_owned)
            .collect();
        let join = |v: &[String]| -> String {
            v.chunks(2)
                .map(|c| {
                    c.iter()
                        .map(|l| l.trim_end())
                        .collect::<Vec<_>>()
                        .join(" | ")
                        + "\n"
                })
                .collect()
        };
        let r = restore(&base, &join(&seen));
        assert_eq!(r.kept, 0);
        assert!(r.text == join(&lines), "значения не по порядку");
    }

    /// Пустая база, пустая правка, текст без перевода строки в конце, метка в начале и в конце.
    #[test]
    fn restore_edges() {
        let k = key(9);
        assert_eq!(restore("", "").text, "");
        assert_eq!(restore("", M).kept, 1);
        let base = format!("{k} середина {k}");
        let r = restore(&base, &format!("{M} середина {M}!"));
        assert_eq!(r.text, format!("{k} середина {k}!"));
        let r = restore(&base, "");
        assert_eq!((r.text.as_str(), r.kept), ("", 0));
    }

    /// `data` записи: строки по пути, переставленный элемент массива — по виду целиком, новое
    /// поле — как есть с `kept`; теги — так же.
    #[test]
    fn restore_value_and_tags() {
        let (k1, k2) = (key(21), key(22));
        let base = json!({
            "kind": "decision",
            "evidence": {"checked": format!("token: {k1}"), "n": 3},
            "options": [format!("a {k1}"), format!("b {k2}"), "c"],
        });
        let mut shown = base.clone();
        agent_view::mask_value(&mut shown);
        let mut new = json!({
            "kind": "decision",
            "evidence": {"checked": format!("token: {M} (prod)"), "n": 3},
            "options": ["c", shown["options"][1].clone(), shown["options"][0].clone()],
            "new": format!("x {M}"),
        });
        let kept = restore_value(&base, &mut new);
        assert_eq!(kept, 1);
        assert_eq!(new["evidence"]["checked"], format!("token: {k1} (prod)"));
        assert_eq!(
            new["options"],
            json!(["c", format!("b {k2}"), format!("a {k1}")])
        );
        assert_eq!(new["new"], format!("x {M}"));
        let tags = vec![format!("t-{k1}"), "ops".to_string()];
        let mut seen: Vec<String> = tags.iter().map(|t| mask_text(t)).collect();
        seen.reverse();
        seen.push(M.to_string());
        assert_eq!(restore_tags(&tags, &mut seen), 1);
        assert_eq!(
            seen,
            vec!["ops".to_string(), format!("t-{k1}"), M.to_string()]
        );
    }

    /// Предупреждение: строка в `warnings` (к прежним) и число; без меток — ответ как есть.
    #[test]
    fn attach_kept_warning() {
        let mut v = json!({"id": "x", "warnings": ["term_alias: a"]});
        attach_kept(&mut v, 0);
        assert_eq!(v, json!({"id": "x", "warnings": ["term_alias: a"]}));
        attach_kept(&mut v, 2);
        assert_eq!(v[SECRET_MARK_KEPT], 2);
        let w = v["warnings"].as_array().unwrap();
        assert_eq!(w[0], "term_alias: a");
        assert!(
            w[1].as_str().unwrap().starts_with("secret_mark_kept: 2"),
            "{v}"
        );
    }

    /// Цена восстановления (журнал этапа, задача 6 M7.1): заметка 1 МБ и 5 МиБ — разделы с прозой,
    /// кодом и ключом на ~270 байт (как `bench_mask_l2_1mb` задачи 5); правка агента —
    /// дописка к строкам с метками в десяти местах, вставка и удаление строк. Худший случай — та
    /// же заметка, где правлена каждая строка. Медиана 5 прогонов, только release: `cargo test
    /// --release -p zam-server --lib -- --ignored bench_restore --nocapture`.
    #[test]
    #[ignore]
    fn bench_restore() {
        let doc = |size: usize| {
            let k = key(1);
            let mut t = String::from("# Заметка о стенде\n\n");
            let mut i = 0usize;
            while t.len() < size {
                t.push_str(&format!(
                    "## Раздел {i}\n\nСборка идёт скриптом `deploy.sh`, журнал — в \
                     `/var/log/app`.\nThe service reads its config at start.\n\n```bash\ncurl -s \
                     https://api.example.com/v1/items?page={i}\n```\n\ntoken: {k}\n\nСсылка: \
                     https://user:pw{i}x@db.example.com/\n\n"
                ));
                i += 1;
            }
            t
        };
        for size in [1024 * 1024, 5 * 1024 * 1024] {
            let base = doc(size);
            let masked = mask_text(&base);
            let mut lines: Vec<String> = masked.split_inclusive('\n').map(str::to_owned).collect();
            let step = lines.len() / 10;
            for j in 0..10 {
                let at = j * step + 1;
                lines.insert(at, format!("вставка {j}\n"));
                lines.remove(at + 3);
            }
            let mut edited_marks = 0;
            for l in lines.iter_mut() {
                if l.starts_with("token: ") && edited_marks < 10 {
                    l.insert_str(l.len() - 1, " (prod)");
                    edited_marks += 1;
                }
            }
            let small: String = lines.concat();
            let all: String = masked
                .split_inclusive('\n')
                .map(|l| format!("> {l}"))
                .collect();
            for (what, edited) in [("правка", &small), ("каждая строка", &all)] {
                let mut times = Vec::new();
                let mut kept = 0;
                for _ in 0..5 {
                    let t0 = std::time::Instant::now();
                    kept = restore(&base, edited).kept;
                    times.push(t0.elapsed());
                }
                times.sort();
                println!(
                    "восстановление {} байт ({what}, {} меток, без пары {kept}): медиана {:?}, \
                     мин {:?}, макс {:?}",
                    base.len(),
                    edited.matches(M).count(),
                    times[2],
                    times[0],
                    times[4]
                );
            }
        }
    }
}
