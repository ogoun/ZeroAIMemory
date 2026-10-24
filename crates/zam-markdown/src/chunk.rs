//! Чанкер Markdown (ТЗ §8.2).
//!
//! Порядок работы:
//! 1. Собственный текст каждой секции H1–H3 (и преамбулы) — от конца строк
//!    заголовка до начала следующей секции, так что текст подсекции не
//!    повторяется в родителе. Frontmatter и строки заголовков в листы не входят.
//! 2. Короткие секции (< `merge_below`) склеиваются со следующим непустым
//!    соседом того же родителя, если склейка не длиннее `target_max` (пустые
//!    соседи цепочку не рвут); без следующего соседа — с предыдущей группой того
//!    же родителя по тому же правилу. Путь склейки — путь первой секции; строки
//!    заголовков присоединённых секций остаются в тексте: диапазон непрерывен.
//! 3. Текст группы делится на блоки верхнего уровня по AST comrak. Блок кода,
//!    таблица, список и цитата/callout — неделимые единицы, пока не длиннее
//!    `hard_max`; длиннее — режутся по элементам (пункты, дочерние блоки) или
//!    строкам, а заголовок блока (строка ограждения с info, шапка таблицы
//!    с разделителем, маркер callout) повторяется в начале каждой следующей
//!    части. Абзац длиннее `target_max` режется по предложениям. Последний
//!    уровень дробления — слова, затем символы: `hard_max` соблюдается всегда.
//! 4. Единицы упаковываются в листы `target_min..=target_max`; лист, не добравший
//!    `target_min`, может вырасти до `hard_max`. При `boundary_lambda = 0` — жадно (M2), при
//!    `boundary_lambda > 0` — динамическим программированием с учётом силы границы перед
//!    каждой единицей ([`boundary_strength`], `crate::boundary`), по флагу `cohesion` — с
//!    лексической связностью прозы. В обоих режимах лист не кончается заголовком (H4–H6 или
//!    строкой H1–H3 склеенной секции) и абзацем на «:» — разрез после них запрещён, пока
//!    следующая единица умещается в `hard_max` (задача 17 M3).
//! 5. Перекрытие: следующий лист той же группы начинается с последних
//!    `overlap_sentences` предложений предыдущего, если тот кончается абзацем.
//!    Перекрытие расширяет `range.start` назад, поэтому `src[range] == text`.
//!
//! 6. Путь заголовков листа — путь группы (H1–H3) и, при `heading_path_h4`, действующие
//!    в начале листа заголовки H4–H6 (`H1 > H2 > H3 > H4`); секции по-прежнему по H1–H3.
//!
//! Инвариант листа: `text == src[range]` либо `text == заголовок блока + src[range]`,
//! где заголовок — целые строки исходника (с переводами строк документа).
//! Счётчик токенов считается монотонным на подстроках.

use std::ops::Range;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, parse_document};
use serde::Serialize;

use crate::boundary::{self, Budget, Cand, FORBIDDEN};
use crate::doc::Doc;
use crate::levels::{L1_MAX_TOKENS, NOTE_L1_MAX_CHARS, fit_items, l1_items, own_ranges};
use crate::options::comrak_options;
use crate::pos::LineIndex;
use crate::render::is_callout;
use crate::sentences::split_sentences;

/// Счётчик токенов для бюджетов чанкера.
pub trait TokenCounter: Send + Sync {
    /// Число токенов в `text`.
    fn count(&self, text: &str) -> usize;
    /// `true` — точный подсчёт токенизатором модели, `false` — оценка.
    fn is_exact(&self) -> bool;
}

/// Оценка без токенизатора: `ceil(символы / 3,5)` (исследование 05 §10).
#[derive(Debug, Clone, Copy, Default)]
pub struct ApproxCounter;

impl TokenCounter for ApproxCounter {
    fn count(&self, text: &str) -> usize {
        // ceil(n / 3.5) = ceil(2n / 7) без плавающей точки
        (text.chars().count() * 2).div_ceil(7)
    }

    fn is_exact(&self) -> bool {
        false
    }
}

/// Пороги чанкера (§8.2); значения по умолчанию подбираются на eval (§18.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkParams {
    /// Нижняя граница размера листа, токенов.
    pub target_min: usize,
    /// Верхняя граница размера листа, токенов.
    pub target_max: usize,
    /// Предел размера любого листа, токенов.
    pub hard_max: usize,
    /// Секция короче — склеивается со следующей секцией того же родителя.
    pub merge_below: usize,
    /// Предложений перекрытия между соседними листами секции.
    pub overlap_sentences: usize,
    /// Вес силы границы λ при выборе разреза (задача 17 M3): 0 — жадная упаковка M2
    /// (с исправлениями висячего заголовка и вводной фразы), больше 0 — динамическое
    /// программирование по кандидатам разреза.
    pub boundary_lambda: f64,
    /// Лексическая связность соседних окон прозы (основы слов) добавляется к силе границы
    /// при λ > 0.
    pub cohesion: bool,
    /// H4–H6 входят в `heading_path` листа (`H1 > H2 > H3 > H4`); `false` — путь M2 по H1–H3.
    pub heading_path_h4: bool,
}

impl Default for ChunkParams {
    fn default() -> Self {
        Self {
            target_min: 300,
            target_max: 500,
            hard_max: 800,
            merge_below: 100,
            overlap_sentences: 1,
            boundary_lambda: 0.0,
            cohesion: false,
            heading_path_h4: true,
        }
    }
}

impl ChunkParams {
    /// Согласованные пороги: `1 ≤ target_max ≤ hard_max`, `target_min ≤ target_max`,
    /// `boundary_lambda` — конечное неотрицательное (иначе 0).
    fn normalized(&self) -> Self {
        let hard_max = self.hard_max.max(1);
        let target_max = self.target_max.clamp(1, hard_max);
        let lambda = self.boundary_lambda;
        Self {
            target_min: self.target_min.min(target_max),
            target_max,
            hard_max,
            boundary_lambda: if lambda.is_finite() && lambda > 0.0 {
                lambda
            } else {
                0.0
            },
            ..*self
        }
    }
}

/// Уровень чанка (колонка `chunks.level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkLevel {
    /// Листовой чанк — фрагмент исходника.
    Chunk,
    /// L1 секции H1–H2 из двух и более листов.
    Section,
    /// L1 всей заметки.
    Note,
}

impl ChunkLevel {
    /// Значение для колонки `chunks.level`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chunk => "chunk",
            Self::Section => "section",
            Self::Note => "note",
        }
    }
}

/// Чанк до записи в БД.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkDraft {
    pub level: ChunkLevel,
    /// Путь заголовков через `" > "`; у преамбулы и `Note` — пустой.
    pub heading_path: String,
    pub text: String,
    /// Байты исходника для `Chunk`: `text == src[range]` или
    /// `text == заголовок блока + src[range]`. У `Section` и `Note` — `None`.
    pub range: Option<Range<usize>>,
    /// Токены `text` по переданному счётчику.
    pub token_count: usize,
}

/// Чанки заметки: листы в порядке документа, затем L1 секций, затем L1 заметки.
/// Заметка без текста тела (только заголовки и frontmatter) даёт пустой список.
/// `doc` — результат [`crate::parse`] того же `src`. Не паникует на любом входе.
pub fn chunk_note(src: &str, doc: &Doc, p: &ChunkParams, tc: &dyn TokenCounter) -> Vec<ChunkDraft> {
    let arena = Arena::new();
    let root = parse_document(&arena, src, &comrak_options());
    let li = LineIndex::new(src);
    let ch = Chunker {
        src,
        p: p.normalized(),
        tc,
    };
    let blocks = top_blocks(&li, root, doc.body_start, src.len());
    let own = own_ranges(doc);

    let mut leaves: Vec<(String, Leaf)> = Vec::new();
    for g in ch.groups(doc, &own) {
        let units = ch.units(g.range.clone(), &blocks);
        let mut group = ch.pack(&units);
        ch.add_overlap(&mut group);
        leaves.extend(group.into_iter().map(|l| {
            let path = if ch.p.heading_path_h4 {
                deep_path(src, doc, &g.path, l.core_start, l.end)
            } else {
                g.path.clone()
            };
            (path, l)
        }));
    }
    if leaves.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<ChunkDraft> = leaves
        .iter()
        .map(|(path, l)| {
            let text = ch.leaf_text(l.header.clone(), l.start, l.end);
            ChunkDraft {
                level: ChunkLevel::Chunk,
                heading_path: path.clone(),
                token_count: tc.count(&text),
                text,
                range: Some(l.start..l.end),
            }
        })
        .collect();

    // L1 из одних заголовков не индексируется; L1 секции, совпавший с L1 заметки, — тоже
    let items = l1_items(src, doc, root);
    let (note, note_has_sentence) = fit_items(&items, |t| {
        t.chars().count() <= NOTE_L1_MAX_CHARS && tc.count(t) <= L1_MAX_TOKENS
    });
    for (k, s) in doc.sections.iter().enumerate() {
        if !(1..=2).contains(&s.level) {
            continue;
        }
        let n = leaves
            .iter()
            .filter(|(_, l)| s.range.contains(&l.core_start))
            .count();
        if n < 2 {
            continue;
        }
        let slice: Vec<_> = items[k..]
            .iter()
            .take_while(|it| s.range.contains(&it.start))
            .cloned()
            .collect();
        let (text, has_sentence) = fit_items(&slice, |t| tc.count(t) <= L1_MAX_TOKENS);
        if has_sentence && text != note {
            out.push(level_draft(
                ChunkLevel::Section,
                s.path.join(" > "),
                text,
                tc,
            ));
        }
    }
    if note_has_sentence {
        out.push(level_draft(ChunkLevel::Note, String::new(), note, tc));
    }
    out
}

fn level_draft(
    level: ChunkLevel,
    heading_path: String,
    text: String,
    tc: &dyn TokenCounter,
) -> ChunkDraft {
    ChunkDraft {
        level,
        heading_path,
        token_count: tc.count(&text),
        text,
        range: None,
    }
}

/// Путь заголовков листа: путь группы и заголовки H4–H6, действующие в начале собственного
/// текста листа (`core_start`, первый непробельный символ). Строка H1–H3 сбрасывает H4–H6;
/// H5 после H4 вкладывается, следующий H4 замещает предыдущий.
fn deep_path(src: &str, doc: &Doc, group: &str, core_start: usize, end: usize) -> String {
    let lead = src
        .get(core_start..end)
        .map_or(0, |s| s.len() - s.trim_start().len());
    let pos = core_start + lead;
    let mut stack: Vec<(u8, &str)> = Vec::new();
    for h in doc.headings.iter().take_while(|h| h.range.start <= pos) {
        if h.level <= 3 {
            stack.clear();
            continue;
        }
        while stack.last().is_some_and(|&(l, _)| l >= h.level) {
            stack.pop();
        }
        stack.push((h.level, h.text.as_str()));
    }
    let mut path = group.to_string();
    for (_, t) in stack.iter().filter(|(_, t)| !t.trim().is_empty()) {
        if !path.is_empty() {
            path.push_str(" > ");
        }
        path.push_str(t.trim());
    }
    path
}

/// Вид блока, которому принадлежит единица упаковки (для силы границы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitKind {
    /// Абзац.
    Prose,
    /// Заголовок любого уровня (H4–H6 внутри секции или строка H1–H3 склеенной секции).
    Heading,
    /// Тематический разрыв `---`.
    Break,
    /// Код, таблица, список, цитата, callout.
    Structure,
    /// Прочее (HTML, начало группы до первого блока).
    Other,
}

/// С чего начинается единица относительно своего блока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitStart {
    /// С начала блока: граница между блоками.
    Block,
    /// С элемента разрезанного блока (пункт списка, дочерний блок цитаты).
    Element,
    /// С предложения разрезанного абзаца.
    Sentence,
    /// Со строки, слова или символа (фолбэк дробления).
    Fragment,
}

/// Сила границы перед единицей `next` (`prev` — предыдущая единица группы, задача 17 M3):
/// - перед заголовком и `---` — 4;
/// - между блоками (абзацы, код, таблица, список, цитата) — 2;
/// - между предложениями абзаца и элементами разрезанного блока — 1;
/// - внутри строки, слова или предложения (фолбэк дробления) — 0;
/// - после заголовка или после абзаца на «:» — [`FORBIDDEN`] (−5, разрез запрещён).
pub(crate) fn boundary_strength(prev: &Unit, next: &Unit) -> i32 {
    match next.start {
        UnitStart::Block if prev.kind == UnitKind::Heading || prev.colon_end => FORBIDDEN,
        UnitStart::Block => match next.kind {
            UnitKind::Heading | UnitKind::Break => 4,
            _ => 2,
        },
        UnitStart::Element | UnitStart::Sentence => 1,
        UnitStart::Fragment => 0,
    }
}

/// Блок верхнего уровня по AST.
#[derive(Debug)]
struct Block {
    /// Начало первой строки блока.
    start: usize,
    kind: UnitKind,
    /// Абзац: режется по предложениям, даёт перекрытие.
    prose: bool,
    /// Конец заголовка блока (повторяется в частях), если он есть.
    header_end: Option<usize>,
    /// Начала строк элементов (пункты списка, дочерние блоки цитаты).
    elems: Vec<usize>,
}

/// Блоки верхнего уровня тела, упорядоченные по началу.
fn top_blocks<'a>(
    li: &LineIndex,
    root: &'a AstNode<'a>,
    body_start: usize,
    len: usize,
) -> Vec<Block> {
    let child_starts = |node: &'a AstNode<'a>| -> Vec<usize> {
        node.children()
            .map(|c| li.line_start(c.data().sourcepos.start.line))
            .collect()
    };
    let mut out: Vec<Block> = Vec::new();
    for node in root.children() {
        let data = node.data();
        let line = data.sourcepos.start.line;
        let start = li.line_start(line);
        if start < body_start || start >= len || out.last().is_some_and(|b| b.start >= start) {
            continue;
        }
        let mut b = Block {
            start,
            kind: UnitKind::Structure,
            prose: false,
            header_end: None,
            elems: Vec::new(),
        };
        match &data.value {
            NodeValue::FrontMatter(_) => continue,
            NodeValue::Paragraph => {
                b.prose = true;
                b.kind = UnitKind::Prose;
            }
            NodeValue::Heading(_) => b.kind = UnitKind::Heading,
            NodeValue::ThematicBreak => b.kind = UnitKind::Break,
            NodeValue::HtmlBlock(_) => b.kind = UnitKind::Other,
            NodeValue::CodeBlock(cb) if cb.fenced => b.header_end = Some(li.line_end(line)),
            // шапка и строка-разделитель
            NodeValue::Table(_) => b.header_end = Some(li.line_end(line + 1)),
            // callout Obsidian (`> [!info]-`, `> [!заметка]`) — цитата с маркером:
            // строка маркера повторяется в частях, как у GitHub alert
            NodeValue::BlockQuote if is_callout(node) => {
                b.elems = child_starts(node);
                b.header_end = Some(li.line_end(line));
            }
            NodeValue::List(_) | NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                b.elems = child_starts(node);
            }
            NodeValue::Alert(_) => {
                b.elems = child_starts(node);
                b.header_end = Some(
                    b.elems
                        .first()
                        .copied()
                        .filter(|&h| h > start)
                        .unwrap_or_else(|| li.line_end(line)),
                );
            }
            _ => {}
        }
        out.push(b);
    }
    out
}

/// Группа секций, режущаяся вместе.
struct Group {
    range: Range<usize>,
    path: String,
    /// Индекс последней секции группы в `Doc::sections`.
    last: usize,
}

/// Лист до сборки текста.
#[derive(Debug, Clone)]
struct Leaf {
    /// Начало с учётом перекрытия.
    start: usize,
    end: usize,
    /// Начало без перекрытия.
    core_start: usize,
    /// Повторяемый заголовок блока перед `src[start..end]`.
    header: Option<Range<usize>>,
    /// Начало абзаца, которым кончается лист (источник перекрытия).
    last_para: Option<usize>,
}

/// Единица упаковки: непрерывный кусок исходника, единицы группы идут встык.
#[derive(Debug)]
pub(crate) struct Unit {
    range: Range<usize>,
    tokens: usize,
    /// Заголовок блока, если лист начнётся с этой единицы.
    header: Option<Range<usize>>,
    /// Начало абзаца, если единица — абзац или его предложения.
    para: Option<usize>,
    kind: UnitKind,
    start: UnitStart,
    /// Последняя часть абзаца, который кончается на «:» (вводная фраза к следующему блоку).
    colon_end: bool,
}

/// Уровни дробления единицы, превышающей бюджет.
#[derive(Debug, Clone, Copy)]
enum Gran {
    Lines,
    Sentences,
    Words,
    Chars,
}

const BLOCK_STEPS: &[Gran] = &[Gran::Lines, Gran::Words, Gran::Chars];
const PROSE_STEPS: &[Gran] = &[Gran::Words, Gran::Chars];

struct Chunker<'s> {
    src: &'s str,
    p: ChunkParams,
    tc: &'s dyn TokenCounter,
}

impl Chunker<'_> {
    fn count(&self, r: Range<usize>) -> usize {
        self.tc.count(&self.src[r])
    }

    fn leaf_text(&self, header: Option<Range<usize>>, start: usize, end: usize) -> String {
        match header {
            Some(h) => format!("{}{}", &self.src[h], &self.src[start..end]),
            None => self.src[start..end].to_string(),
        }
    }

    /// Группы секций с непустым собственным текстом, с учётом склейки коротких.
    ///
    /// Группа короче `merge_below` присоединяет следующего непустого соседа того
    /// же родителя, если склейка не длиннее `target_max`; пустые соседи между
    /// ними (только строка заголовка) цепочку не рвут. Если следующего непустого
    /// соседа нет, короткая группа присоединяется к предыдущей группе того же
    /// родителя по тому же правилу. Иначе секция остаётся отдельной.
    fn groups(&self, doc: &Doc, own: &[Range<usize>]) -> Vec<Group> {
        let tokens: Vec<Option<usize>> = own
            .iter()
            .map(|r| {
                let t = self.src[r.clone()].trim();
                (!t.is_empty()).then(|| self.tc.count(t))
            })
            .collect();
        let secs = &doc.sections;
        let merged_tokens = |a: usize, b: usize| self.tc.count(self.src[a..b].trim());
        // следующий непустой сосед `k` через пустых соседей
        let next_sibling = |k: usize| -> Option<usize> {
            let mut j = k + 1;
            while j < secs.len() && siblings(&secs[k], &secs[j]) {
                if tokens[j].is_some() {
                    return Some(j);
                }
                j += 1;
            }
            None
        };
        let mut out: Vec<Group> = Vec::new();
        let mut i = 0;
        while i < secs.len() {
            let Some(mut total) = tokens[i] else {
                i += 1;
                continue;
            };
            let (first, mut last) = (i, i);
            let mut no_next = false;
            while total < self.p.merge_below {
                let Some(j) = next_sibling(last) else {
                    no_next = true;
                    break;
                };
                let t = merged_tokens(own[first].start, own[j].end);
                if t > self.p.target_max {
                    break;
                }
                total = t;
                last = j;
            }
            let range = own[first].start..own[last].end;
            i = last + 1;
            if total < self.p.merge_below
                && no_next
                && let Some(prev) = out.last_mut()
                && (prev.last + 1..first).all(|k| tokens[k].is_none())
                && (prev.last + 1..=first).all(|k| siblings(&secs[prev.last], &secs[k]))
                && merged_tokens(prev.range.start, range.end) <= self.p.target_max
            {
                prev.range.end = range.end;
                prev.last = last;
                continue;
            }
            out.push(Group {
                range,
                path: secs[first].path.join(" > "),
                last,
            });
        }
        out
    }

    /// Единицы группы: блоки верхнего уровня, при необходимости раздробленные.
    fn units(&self, r: Range<usize>, blocks: &[Block]) -> Vec<Unit> {
        let lo = blocks.partition_point(|b| b.start < r.start);
        let inner: Vec<&Block> = blocks[lo..]
            .iter()
            .take_while(|b| b.start < r.end)
            .collect();
        let mut bounds: Vec<(usize, Option<&Block>)> = Vec::with_capacity(inner.len() + 1);
        if inner.first().is_none_or(|b| b.start > r.start) {
            bounds.push((r.start, None));
        }
        bounds.extend(inner.iter().map(|b| (b.start, Some(*b))));
        let mut out = Vec::new();
        for (k, &(start, block)) in bounds.iter().enumerate() {
            let end = bounds.get(k + 1).map_or(r.end, |n| n.0);
            self.atom_units(start..end, block, &mut out);
        }
        self.split_long_intros(out)
    }

    /// Вводная фраза на «:», которая вместе с первой частью следующего блока не умещается
    /// в `hard_max`: целый абзац дробится по предложениям, чтобы к блоку ушло хотя бы
    /// последнее предложение (иначе разрез на запрещённой границе неизбежен).
    fn split_long_intros(&self, units: Vec<Unit>) -> Vec<Unit> {
        let needs = |k: usize| {
            let (u, next) = (&units[k], units.get(k + 1));
            u.colon_end
                && u.start == UnitStart::Block
                && next.is_some_and(|n| {
                    n.start == UnitStart::Block && u.tokens + n.tokens > self.p.hard_max
                })
        };
        if !(0..units.len()).any(needs) {
            return units;
        }
        let split: Vec<bool> = (0..units.len()).map(needs).collect();
        let mut out = Vec::with_capacity(units.len() + 8);
        for (u, split) in units.into_iter().zip(split) {
            if split {
                let first = out.len();
                self.prose_pieces(u.range.clone(), u.kind, &mut out);
                mark_block(&mut out[first..], true);
            } else {
                out.push(u);
            }
        }
        out
    }

    /// Абзац по предложениям (предложение длиннее `hard_max` — по словам и символам).
    fn prose_pieces(&self, r: Range<usize>, kind: UnitKind, out: &mut Vec<Unit>) {
        for t in self.split_by(r.clone(), Gran::Sentences) {
            let mut pieces = Vec::new();
            self.refine(t, PROSE_STEPS, None, &mut pieces);
            out.extend(pieces.into_iter().enumerate().map(|(k, p)| Unit {
                tokens: self.count(p.clone()),
                range: p,
                header: None,
                para: Some(r.start),
                kind,
                start: if k == 0 {
                    UnitStart::Sentence
                } else {
                    UnitStart::Fragment
                },
                colon_end: false,
            }));
        }
    }

    fn atom_units(&self, r: Range<usize>, block: Option<&Block>, out: &mut Vec<Unit>) {
        let tokens = self.count(r.clone());
        let prose = block.is_some_and(|b| b.prose);
        let kind = block.map_or(UnitKind::Other, |b| b.kind);
        let colon = prose && self.src[r.clone()].trim_end().ends_with(':');
        let first = out.len();
        let fits = if prose {
            tokens <= self.p.target_max
        } else {
            tokens <= self.p.hard_max
        };
        if fits || self.src[r.clone()].trim().is_empty() {
            out.push(Unit {
                range: r.clone(),
                tokens,
                header: None,
                para: prose.then_some(r.start),
                kind,
                start: UnitStart::Block,
                colon_end: colon,
            });
            return;
        }
        if prose {
            self.prose_pieces(r, kind, out);
            mark_block(&mut out[first..], colon);
            return;
        }

        // длинный блок: заголовок повторяется, если он не съедает больше половины бюджета
        let header = block
            .and_then(|b| b.header_end)
            .filter(|&h| h > r.start && h < r.end)
            .map(|h| r.start..h)
            .filter(|h| 2 * self.count(h.clone()) <= self.p.hard_max);
        let body_start = header.as_ref().map_or(r.start, |h| h.end);
        let mut starts_of: Vec<UnitStart> = Vec::new();
        let elems = block.map_or(&[][..], |b| &b.elems[..]);
        let elem_tiles = if elems.is_empty() {
            line_tiles(self.src, body_start..r.end)
        } else {
            tiles(body_start..r.end, elems.iter().copied())
        };
        let elem_start = if elems.is_empty() {
            UnitStart::Fragment
        } else {
            UnitStart::Element
        };
        let mut pieces = Vec::new();
        for t in elem_tiles {
            let at = pieces.len();
            self.refine(t, BLOCK_STEPS, header.as_ref(), &mut pieces);
            starts_of.resize(pieces.len(), UnitStart::Fragment);
            if let Some(s) = starts_of.get_mut(at) {
                *s = elem_start;
            }
        }
        for (k, p) in pieces.into_iter().enumerate() {
            // первая часть начинается с самого заголовка — без повтора
            let (range, head) = match (k, &header) {
                (0, Some(h)) => (h.start..p.end, None),
                _ => (p, header.clone()),
            };
            out.push(Unit {
                tokens: self.count(range.clone()),
                range,
                header: head,
                para: None,
                kind,
                start: starts_of[k],
                colon_end: false,
            });
        }
        mark_block(&mut out[first..], false);
    }

    /// Дробит `r`, пока `header + src[r]` не уложится в `hard_max`; части идут встык.
    fn refine(
        &self,
        r: Range<usize>,
        steps: &[Gran],
        header: Option<&Range<usize>>,
        out: &mut Vec<Range<usize>>,
    ) {
        let fits = match header {
            Some(h) => self
                .tc
                .count(&self.leaf_text(Some(h.clone()), r.start, r.end)),
            None => self.count(r.clone()),
        } <= self.p.hard_max;
        let Some((&step, rest)) = steps.split_first() else {
            out.push(r);
            return;
        };
        if fits {
            out.push(r);
            return;
        }
        let subs = self.split_by(r.clone(), step);
        if subs.len() <= 1 {
            self.refine(r, rest, header, out);
        } else {
            for s in subs {
                self.refine(s, steps, header, out);
            }
        }
    }

    fn split_by(&self, r: Range<usize>, step: Gran) -> Vec<Range<usize>> {
        let s = &self.src[r.clone()];
        match step {
            Gran::Lines => line_tiles(self.src, r),
            Gran::Sentences => tiles(
                r.clone(),
                split_sentences(s).into_iter().map(|x| r.start + x.start),
            ),
            Gran::Words => {
                let mut prev_ws = false;
                let starts = s.char_indices().filter_map(|(i, c)| {
                    let start = prev_ws && !c.is_whitespace();
                    prev_ws = c.is_whitespace();
                    start.then_some(r.start + i)
                });
                tiles(r.clone(), starts.collect::<Vec<_>>())
            }
            Gran::Chars => {
                let mut mid = r.start + r.len() / 2;
                while !self.src.is_char_boundary(mid) {
                    mid += 1;
                }
                tiles(r, [mid])
            }
        }
    }

    /// Упаковка единиц группы в листы.
    ///
    /// Балансировка: цель листа — `ceil(T / ceil(T / target_max))` в пределах
    /// `target_min..=target_max`, где `T` — сумма группы. Так 620 токенов дают
    /// два листа около 310, а не 500 и короткий хвост в 120. При `boundary_lambda = 0` —
    /// жадно ([`Self::pack_greedy`]), иначе — DP по силе границ ([`Self::pack_dp`]).
    fn pack(&self, units: &[Unit]) -> Vec<Leaf> {
        let p = &self.p;
        let total: usize = units.iter().map(|u| u.tokens).sum();
        let parts = total.div_ceil(p.target_max).max(1);
        let goal = total.div_ceil(parts).clamp(p.target_min, p.target_max);
        let ranges = if p.boundary_lambda > 0.0 {
            self.pack_dp(units, goal)
        } else {
            self.pack_greedy(units, goal)
        };
        let mut out: Vec<Leaf> = ranges
            .into_iter()
            .filter_map(|r| self.make_leaf(units[r.start].header.clone(), &units[r]))
            .collect();
        // хвост короче половины target_min (остаток балансировки после повторённых
        // заголовков и округлений) присоединяется к предыдущему листу в пределах hard_max
        if let [.., prev, last] = &out[..] {
            let short =
                2 * self
                    .tc
                    .count(&self.leaf_text(last.header.clone(), last.start, last.end))
                    < p.target_min;
            let merged = self.leaf_text(prev.header.clone(), prev.start, last.end);
            if short && self.tc.count(&merged) <= p.hard_max {
                let (end, last_para) = (last.end, last.last_para);
                out.pop();
                if let Some(prev) = out.last_mut() {
                    prev.end = end;
                    prev.last_para = last_para;
                }
            }
        }
        out
    }

    /// Разрез перед `units[j]` запрещён: после заголовка или вводной фразы на «:».
    fn forbidden(units: &[Unit], j: usize) -> bool {
        j > 0 && j < units.len() && boundary_strength(&units[j - 1], &units[j]) == FORBIDDEN
    }

    /// Жадная упаковка M2: диапазоны индексов единиц. Лист закрывается, когда следующая
    /// единица перешагнула бы цель больше чем наполовину, — но не на запрещённой границе,
    /// пока следующая единица умещается в `hard_max` (задача 17 M3).
    fn pack_greedy(&self, units: &[Unit], goal: usize) -> Vec<Range<usize>> {
        let p = &self.p;
        let mut out = Vec::new();
        let mut i = 0;
        while i < units.len() {
            // лист не начинается с пробельной единицы: её заголовок (хвост
            // разрезанного блока) после обрезки достался бы следующему блоку
            if self.src[units[i].range.clone()].trim().is_empty() {
                i += 1;
                continue;
            }
            let header = units[i].header.clone();
            let mut est = header.as_ref().map_or(0, |h| self.count(h.clone())) + units[i].tokens;
            let mut j = i + 1;
            while let Some(u) = units.get(j) {
                let next = est + u.tokens;
                // лист закрывается, если единица перешагнула бы цель больше чем наполовину
                let full = next > p.target_max || est + u.tokens / 2 > goal;
                if next > p.hard_max || (est >= p.target_min && full && !Self::forbidden(units, j))
                {
                    break;
                }
                est = next;
                j += 1;
            }
            // оценка — сумма по единицам; точный подсчёт может снять хвост; лист,
            // упёршийся в hard_max на запрещённой границе, отступает до разрешённой
            while j > i + 1 {
                let text =
                    self.leaf_text(header.clone(), units[i].range.start, units[j - 1].range.end);
                if self.tc.count(&text) <= self.p.hard_max && !Self::forbidden(units, j) {
                    break;
                }
                j -= 1;
            }
            out.push(i..j);
            i = j;
        }
        out
    }

    /// Выбор разреза динамическим программированием по силе границ (`crate::boundary`).
    /// DP считает по оценке (сумма токенов единиц); лист, который точным подсчётом
    /// не уложился в `hard_max`, режется жадно.
    fn pack_dp(&self, units: &[Unit], goal: usize) -> Vec<Range<usize>> {
        let p = &self.p;
        let bonus = if p.cohesion {
            let bags: Vec<_> = units
                .iter()
                .map(|u| {
                    (u.kind == UnitKind::Prose).then(|| boundary::stems(&self.src[u.range.clone()]))
                })
                .collect();
            let eligible: Vec<bool> = (0..units.len())
                .map(|j| {
                    j > 0
                        && units[j - 1].kind == UnitKind::Prose
                        && units[j].kind == UnitKind::Prose
                        && matches!(units[j].start, UnitStart::Block | UnitStart::Sentence)
                        && !units[j - 1].colon_end
                })
                .collect();
            boundary::cohesion_bonus(&bags, &eligible)
        } else {
            vec![0.0; units.len()]
        };
        let cands: Vec<Cand> = units
            .iter()
            .enumerate()
            .map(|(j, u)| {
                let strength = if j == 0 {
                    0
                } else {
                    boundary_strength(&units[j - 1], u)
                };
                Cand {
                    tokens: u.tokens,
                    header: u.header.as_ref().map_or(0, |h| self.count(h.clone())),
                    can_start: !self.src[u.range.clone()].trim().is_empty(),
                    strength: f64::from(strength.max(0)) + bonus[j],
                    forbidden: strength == FORBIDDEN,
                }
            })
            .collect();
        let budget = Budget {
            goal,
            target_min: p.target_min,
            target_max: p.target_max,
            hard_max: p.hard_max,
        };
        let mut out = Vec::new();
        for r in boundary::choose_cuts(&cands, &budget, p.boundary_lambda) {
            let first = &units[r.start];
            let text = self.leaf_text(
                first.header.clone(),
                first.range.start,
                units[r.end - 1].range.end,
            );
            if r.len() > 1 && self.tc.count(&text) > p.hard_max {
                let base = r.start;
                out.extend(
                    self.pack_greedy(&units[r], goal)
                        .into_iter()
                        .map(|x| x.start + base..x.end + base),
                );
            } else {
                out.push(r);
            }
        }
        out
    }

    /// Лист из единиц без краевых пустых строк и хвостовых пробелов.
    fn make_leaf(&self, header: Option<Range<usize>>, units: &[Unit]) -> Option<Leaf> {
        let (first, last) = (units.first()?, units.last()?);
        let (mut start, mut end) = (first.range.start, last.range.end);
        let s = &self.src[start..end];
        end = start + s.trim_end().len();
        let lead = s.len() - s.trim_start().len();
        if start + lead >= end {
            return None;
        }
        // отступ первой строки сохраняется: сдвиг только на целые пустые строки
        let line = self.src[start..start + lead]
            .rfind(['\n', '\r'])
            .map_or(start, |k| start + k + 1);
        start = line;
        Some(Leaf {
            start,
            end,
            core_start: start,
            header,
            last_para: last.para,
        })
    }

    /// Перекрытие: лист начинается с последних предложений предыдущего листа группы.
    fn add_overlap(&self, leaves: &mut [Leaf]) {
        let want = self.p.overlap_sentences;
        if want == 0 {
            return;
        }
        for k in 1..leaves.len() {
            let (prev, cur) = (&leaves[k - 1], &leaves[k]);
            if cur.header.is_some() {
                continue;
            }
            // окно — только предыдущий лист: предложение, начатое раньше, не подходит,
            // а сегментация длинного абзаца с начала была бы квадратичной
            let Some(from) = prev
                .last_para
                .map(|p| p.max(prev.core_start))
                .filter(|&p| p <= prev.end)
            else {
                continue;
            };
            let sentences = split_sentences(&self.src[from..prev.end]);
            let n = sentences.len();
            let found = (1..=want.min(n)).rev().find_map(|m| {
                let start = from + sentences[n - m].start;
                let ok = start > prev.core_start && self.count(start..cur.end) <= self.p.hard_max;
                ok.then_some(start)
            });
            if let Some(start) = found {
                leaves[k].start = start;
            }
        }
    }
}

/// Отметки частей одного блока: вводная фраза — только у последней части абзаца на «:»,
/// первая часть начинается с границы блока.
fn mark_block(parts: &mut [Unit], colon: bool) {
    if let Some(u) = parts.first_mut() {
        u.start = UnitStart::Block;
    }
    if let Some(u) = parts.last_mut() {
        u.colon_end = colon;
    }
}

/// Соседи одного родителя: одинаковый уровень и путь без последнего заголовка.
fn siblings(a: &crate::doc::Section, b: &crate::doc::Section) -> bool {
    a.level > 0
        && a.level == b.level
        && a.path.len() == b.path.len()
        && a.path[..a.path.len().saturating_sub(1)] == b.path[..b.path.len().saturating_sub(1)]
}

/// Разбиение `r` встык по точкам `starts` (лишние и вне `r` пропускаются).
fn tiles(r: Range<usize>, starts: impl IntoIterator<Item = usize>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut cur = r.start;
    for s in starts {
        if s > cur && s < r.end {
            out.push(cur..s);
            cur = s;
        }
    }
    if cur < r.end || out.is_empty() {
        out.push(cur..r.end);
    }
    out
}

/// Разбиение `r` по строкам (`\n`, `\r\n`, одиночный `\r`).
fn line_tiles(src: &str, r: Range<usize>) -> Vec<Range<usize>> {
    let b = src.as_bytes();
    let starts = (r.start..r.end).filter_map(|i| match b[i] {
        b'\n' => Some(i + 1),
        b'\r' if b.get(i + 1) != Some(&b'\n') => Some(i + 1),
        _ => None,
    });
    tiles(r.clone(), starts.collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(kind: UnitKind, start: UnitStart, colon_end: bool) -> Unit {
        Unit {
            range: 0..1,
            tokens: 1,
            header: None,
            para: None,
            kind,
            start,
            colon_end,
        }
    }

    #[test]
    fn boundary_strength_table() {
        use UnitKind::*;
        use UnitStart::*;
        let prose = unit(Prose, Block, false);
        let s = |prev: &Unit, next: &Unit| boundary_strength(prev, next);
        assert_eq!(s(&prose, &unit(Heading, Block, false)), 4);
        assert_eq!(s(&prose, &unit(Break, Block, false)), 4);
        assert_eq!(s(&prose, &unit(Prose, Block, false)), 2);
        assert_eq!(s(&prose, &unit(Structure, Block, false)), 2);
        assert_eq!(s(&unit(Structure, Block, false), &prose), 2);
        assert_eq!(s(&prose, &unit(Prose, Sentence, false)), 1);
        assert_eq!(s(&prose, &unit(Structure, Element, false)), 1);
        assert_eq!(s(&prose, &unit(Prose, Fragment, false)), 0);
        // после заголовка и после вводной фразы на «:» резать нельзя
        assert_eq!(s(&unit(Heading, Block, false), &prose), FORBIDDEN);
        assert_eq!(
            s(&unit(Prose, Sentence, true), &unit(Structure, Block, false)),
            FORBIDDEN
        );
        // внутри заголовка или абзаца на «:» (дробление) запрета нет: он только на границе блока
        assert_eq!(
            s(
                &unit(Heading, Block, false),
                &unit(Heading, Fragment, false)
            ),
            0
        );
    }
}
