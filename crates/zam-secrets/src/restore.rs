//! Обратный путь маски для записи агентом (спецификация M7 §4.3.2, задача 6 M7.1). Обвязка —
//! `zam_server::api::secret_restore`; история решений — журнал этапа (круги правок 1–5 задачи 6,
//! решения координатора 10).
//!
//! **Метки с личностью.** Текст, который агент правит и возвращает записью, он получает видом
//! [`ObjectMasks::view`] ([`numbered_view`] — для объекта из одного текста, [`Numbering`] — без
//! хранения значений): каждая маска — `‹secret-T-N›`, где T — метка объекта и ревизии (её строит
//! вызывающий: 6 шестнадцатеричных знаков хэша `id ‖ ревизия ‖ RULES_VERSION ‖ версия крейта`,
//! [`VERSION`](crate::VERSION)), N — порядковый номер маски в объекте (с 1), **сквозной по всем
//! редактируемым полям** в постоянном порядке; находки — [`scan_secrets`], те же, что у
//! [`crate::mask_text`]. Литерал метки, уже лежавший в исходнике (`‹secret›`, `‹secret-N›`,
//! `‹secret-T-N›`: прежний `secret_mark_kept`, импорт архива с маской, текст человека), в виде —
//! `‹secret›`: метка с личностью есть только у маски со значением.
//!
//! **Восстановление** ([`ObjectMasks::fill_marks`], [`ObjectMasks::restore`], [`restore`]): метка
//! `‹secret-T-N›`, у которой T — метка этого объекта и **любой его ревизии** (ревизию по T
//! находит [`revisions_by_tag`]; две ревизии с одним T — ни одной: метка — литерал), а N — номер
//! маски той ревизии, — значение этой маски в любом поле объекта (перенос строки между полями
//! сохраняет значение; повтор — повтор значения; вторая правка подряд из того же контекста — по
//! меткам прежней ревизии, финальное ревью M7.1, I-2). Маски ревизий — [`ObjectMasks::revision`].
//! Любая другая метка (чужой T — копия из другого объекта или вид до обновления сканера, номер
//! вне той ревизии, `‹secret›`, прежняя `‹secret-N›`) записывается как есть. Маска базы, которую
//! агент не вернул, удалена правкой, как любой текст. Многострочное значение (закрытый ключ) — с
//! переводами строк нового текста. Предупреждение `secret_mark_kept` поля — меток-литералов в
//! итоге сверх их числа в исходнике текущей ревизии того же поля ([`literals_in`]).
//!
//! **Покрытие по происхождению** ([`uncovered_lines`], §4.3.2 п. 5, круг правок 5): каждая
//! позиция итога, где стоит значение секрета, — восстановленное из метки и маска текущей ревизии,
//! перенесённая без правки (у `PATCH`, [`Deferred::finish`]), — должна быть целиком покрыта
//! находкой итога; иначе — строки итога (агент сменил окружение значения: снял или открыл забор
//! кода, переписал `ключ: значение` прозой, разобрал URL). Текст, набранный агентом, не
//! проверяется: значения в нём нет, кроме угаданного, а угаданное агент и так знает (подсчёт
//! вхождений строки значения круга 4 давал обход и ложные отказы на слабых значениях и оракул
//! подбора).
//!
//! [`Deferred`] — значения в тексте, который собирается позже (операции `PATCH` по секциям
//! применяются к исходнику текущей ревизии): в куски вместо значений встают заменители, после
//! сборки — значения и их позиции.
//!
//! Время — линейное: скан исходника, скан итога и проход по упорядоченным позициям.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::ops::Range;

use crate::{Mark, NUMBERED_MARK_PREFIX, SECRET_MARK, has_any_mark, marks, scan_secrets};

/// Итог восстановления поля.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// Текст со значениями на месте меток объекта.
    pub text: String,
    /// Предупреждение `secret_mark_kept`: меток-литералов в итоге сверх их числа в исходнике
    /// текущей ревизии поля.
    pub kept: usize,
    /// Строки итога (с 1, по возрастанию), где значение секрета не покрыто находкой целиком.
    pub exposed: Vec<usize>,
}

/// Текст со значениями и их позициями.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filled {
    pub text: String,
    /// Позиции значений в `text` (восстановленных и, у [`Deferred`], перенесённых без правки), по
    /// возрастанию.
    pub at: Vec<Range<usize>>,
    /// Меток-литералов в `text`.
    pub literals: usize,
}

impl Filled {
    /// Итог поля: предупреждение — сверх `current_literals` (литералов исходника текущей ревизии
    /// поля), покрытие — по позициям значений.
    pub fn into_restored(self, current_literals: usize) -> Restored {
        Restored {
            exposed: uncovered_lines(&self.text, &self.at),
            kept: self.literals.saturating_sub(current_literals),
            text: self.text,
        }
    }
}

/// Есть ли в тексте метка (`‹secret›`, `‹secret-N›`, `‹secret-T-N›`): без неё восстанавливать
/// нечего, база не сканируется.
pub fn has_mark(s: &str) -> bool {
    has_any_mark(s)
}

/// Меток в тексте.
pub fn marks_in(s: &str) -> usize {
    marks(s).count()
}

/// Меток-литералов в исходнике `raw` — меток вне находок (в виде они `‹secret›`).
pub fn literals_in(raw: &str) -> usize {
    if !has_any_mark(raw) {
        return 0;
    }
    let found = hits(raw);
    marks(raw).filter(|(m, _)| !covered(&found, m)).count()
}

/// Вид объекта из одного текста с метками `‹secret-T-N›` ([`ObjectMasks::view`]); без находок и
/// меток — заимствование.
pub fn numbered_view<'r>(tag: &str, raw: &'r str) -> Cow<'r, str> {
    Numbering::new(tag).view(raw)
}

/// Метки с личностью и с номером — `‹secret›` (вид старой ревизии и кусок вида без меток объекта,
/// §4.3.2 п. 1); без них — заимствование.
pub fn unnumbered(s: &str) -> Cow<'_, str> {
    if !s.contains(NUMBERED_MARK_PREFIX) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for (m, k) in marks(s) {
        if k != Mark::Plain {
            out.push_str(&s[pos..m.start]);
            out.push_str(SECRET_MARK);
            pos = m.end;
        }
    }
    out.push_str(&s[pos..]);
    Cow::Owned(out)
}

/// Восстановление поля `edited` объекта из одного текста `base` (исходник текущей ревизии) с меткой
/// объекта `tag`.
pub fn restore(tag: &str, base: &str, edited: &str) -> Restored {
    if !has_mark(edited) {
        return Restored {
            text: edited.to_owned(),
            kept: 0,
            exposed: Vec::new(),
        };
    }
    let mut m = ObjectMasks::new(tag);
    let literals = m.add(base);
    m.fill_marks(edited).into_restored(literals)
}

/// Находки исходника `raw` — позиции масок (у `PATCH` — для покрытия перенесённых без правки).
pub fn secret_ranges(raw: &str) -> Vec<Range<usize>> {
    hits(raw)
}

/// Метки объекта T меток `‹secret-T-N›` текстов `texts` — без повторов, по возрастанию.
pub fn tags_in<'s>(texts: impl IntoIterator<Item = &'s str>) -> BTreeSet<&'s str> {
    let mut out = BTreeSet::new();
    for s in texts {
        if !s.contains(NUMBERED_MARK_PREFIX) {
            continue;
        }
        for (_, m) in marks(s) {
            if let Mark::Tagged(t, _) = m {
                out.insert(t);
            }
        }
    }
    out
}

/// Ревизии объекта по меткам (M7 §4.3.2 п. 2, финальное ревью M7.1, I-2): каждой T из `tags` —
/// ревизия `k ∈ 1..=max_rev`, у которой `tag_of(k) == T`, если такая ревизия **одна**; T без
/// ревизии (чужой объект, вид до обновления сканера) и T двух ревизий (коллизия 24 бит) в итог не
/// входят — их метки записываются как есть. Цена — `max_rev` вызовов `tag_of` (хэш короткой
/// строки), без `tags` — ни одного.
pub fn revisions_by_tag<'t>(
    tags: &BTreeSet<&'t str>,
    max_rev: i64,
    mut tag_of: impl FnMut(i64) -> String,
) -> BTreeMap<&'t str, i64> {
    let mut found: BTreeMap<&'t str, Option<i64>> = BTreeMap::new();
    if tags.is_empty() {
        return BTreeMap::new();
    }
    for k in 1..=max_rev {
        let t = tag_of(k);
        if let Some(&want) = tags.get(t.as_str()) {
            found
                .entry(want)
                .and_modify(|r| *r = None)
                .or_insert(Some(k));
        }
    }
    found
        .into_iter()
        .filter_map(|(t, k)| k.map(|k| (t, k)))
        .collect()
}

/// Строки `text` (с 1, по возрастанию, без повторов), где значение на позиции из `at` (по
/// возрастанию) не покрыто находкой `text` целиком.
pub fn uncovered_lines(text: &str, at: &[Range<usize>]) -> Vec<usize> {
    if at.is_empty() {
        return Vec::new();
    }
    let found = hits(text);
    let (mut line, mut pos) = (1usize, 0usize);
    let mut out: Vec<usize> = Vec::new();
    for r in at {
        if covered(&found, r) {
            continue;
        }
        line += text.as_bytes()[pos..r.start.max(pos)]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        pos = pos.max(r.start);
        if out.last() != Some(&line) {
            out.push(line);
        }
    }
    out
}

/// Значения масок объекта по номерам (с 1) у каждой его ревизии с меткой T: нумерация ревизии —
/// по порядку кусков, добавленных [`ObjectMasks::view`] или [`ObjectMasks::add`] (поля объекта в
/// постоянном порядке) после [`ObjectMasks::new`] или [`ObjectMasks::revision`] с её меткой.
/// Ревизии с одной T вызывающий не добавляет ([`revisions_by_tag`]); если добавил — значение даёт
/// первая.
#[derive(Debug, Clone, Default)]
pub struct ObjectMasks<'a> {
    /// Ревизии: метка T и значения масок по номерам; куски — в последнюю.
    revs: Vec<(String, Vec<&'a str>)>,
}

impl<'a> ObjectMasks<'a> {
    /// Объект с одной ревизией (метка `tag`).
    pub fn new(tag: &str) -> Self {
        ObjectMasks {
            revs: vec![(tag.to_owned(), Vec::new())],
        }
    }

    /// Следующая ревизия объекта с меткой `tag`: маски кусков, добавленных после
    /// ([`Self::add`], [`Self::view`]), — её, нумерация с 1.
    pub fn revision(&mut self, tag: &str) -> &mut Self {
        self.revs.push((tag.to_owned(), Vec::new()));
        self
    }

    /// Значения последней ревизии (куски — в неё); у пустого объекта — ревизия без метки (её
    /// маски не восстанавливаются: у метки T всегда 6 знаков).
    fn last(&mut self) -> &mut (String, Vec<&'a str>) {
        if self.revs.is_empty() {
            self.revs.push((String::new(), Vec::new()));
        }
        self.revs.last_mut().expect("ревизия есть")
    }

    /// Масок в последней ревизии.
    pub fn len(&self) -> usize {
        self.revs.last().map_or(0, |(_, v)| v.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Значения масок последней ревизии по порядку номеров.
    pub fn values(&self) -> &[&'a str] {
        self.revs.last().map_or(&[], |(_, v)| v.as_slice())
    }

    /// Значение метки `m`: `‹secret-T-N›` с T одной из ревизий объекта и N — номер маски той
    /// ревизии; иначе `None`.
    pub fn value(&self, m: Mark) -> Option<&'a str> {
        match m {
            Mark::Tagged(t, n) => {
                let (_, values) = self.revs.iter().find(|(tag, _)| tag == t)?;
                let i = (n as usize).checked_sub(1)?;
                values.get(i).copied()
            }
            _ => None,
        }
    }

    /// Маски куска `raw` — в последнюю ревизию (нумерация продолжается), без вида; итог —
    /// литералов в куске.
    pub fn add(&mut self, raw: &'a str) -> usize {
        let found = hits(raw);
        let values = &mut self.last().1;
        for r in &found {
            values.push(&raw[r.clone()]);
        }
        if !has_any_mark(raw) {
            return 0;
        }
        marks(raw).filter(|(m, _)| !covered(&found, m)).count()
    }

    /// Вид куска `raw` последней ревизии: маски — `‹secret-T-N›` (нумерация продолжается),
    /// литералы меток — `‹secret›`; без находок и меток — заимствование.
    pub fn view(&mut self, raw: &'a str) -> Cow<'a, str> {
        let (tag, values) = self.last();
        let tag = tag.clone();
        view_into(raw, &tag, |v| {
            values.push(v);
            values.len()
        })
    }

    /// `edited` со значениями вместо меток объекта и их позиции; прочие метки — как есть.
    pub fn fill_marks(&self, edited: &str) -> Filled {
        let crlf = edited.contains("\r\n");
        let mut text = String::with_capacity(edited.len());
        let mut at = Vec::new();
        let (mut pos, mut literals) = (0, 0);
        for (m, k) in marks(edited) {
            text.push_str(&edited[pos..m.start]);
            match self.value(k) {
                Some(v) => {
                    let s = text.len();
                    push_value(&mut text, v, crlf);
                    at.push(s..text.len());
                }
                None => {
                    text.push_str(&edited[m.clone()]);
                    literals += 1;
                }
            }
            pos = m.end;
        }
        text.push_str(&edited[pos..]);
        Filled { text, at, literals }
    }

    /// Восстановление поля `edited`: предупреждение — сверх `current_literals` (литералов
    /// исходника текущей ревизии поля), покрытие — по позициям восстановленных значений.
    pub fn restore(&self, edited: &str, current_literals: usize) -> Restored {
        self.fill_marks(edited).into_restored(current_literals)
    }

    /// Значения куска `edited` — в `d` на потом ([`Deferred`]): метка объекта — заменитель,
    /// прочие — как есть.
    pub fn defer(&self, edited: &str, d: &mut Deferred<'a>) -> String {
        let mut out = String::with_capacity(edited.len());
        let mut pos = 0;
        for (m, k) in marks(edited) {
            out.push_str(&edited[pos..m.start]);
            match self.value(k) {
                Some(v) => {
                    d.values.push(v);
                    let _ = write!(out, "{0}{1}{0}", d.sentinel, d.values.len() - 1);
                }
                None => out.push_str(&edited[m.clone()]),
            }
            pos = m.end;
        }
        out.push_str(&edited[pos..]);
        out
    }

    /// `s` со значениями вместо меток объекта (адрес секции `PATCH` по заголовку вида — к
    /// заголовку исходника); прочее — как есть.
    pub fn fill<'s>(&self, s: &'s str) -> Cow<'s, str> {
        if !has_mark(s) {
            return Cow::Borrowed(s);
        }
        let mut out = String::with_capacity(s.len());
        let mut pos = 0;
        for (m, k) in marks(s) {
            out.push_str(&s[pos..m.start]);
            out.push_str(self.value(k).unwrap_or(&s[m.clone()]));
            pos = m.end;
        }
        out.push_str(&s[pos..]);
        Cow::Owned(out)
    }
}

/// Нумерация масок объекта без значений — для выдачи ([`ObjectMasks::view`] без хранения значений:
/// вид строится на месте, исходник заменяется видом).
#[derive(Debug, Clone)]
pub struct Numbering {
    tag: String,
    n: usize,
}

impl Numbering {
    pub fn new(tag: &str) -> Self {
        Numbering {
            tag: tag.to_owned(),
            n: 0,
        }
    }

    /// Вид куска `raw` объекта (нумерация продолжается) — как [`ObjectMasks::view`].
    pub fn view<'r>(&mut self, raw: &'r str) -> Cow<'r, str> {
        let tag = self.tag.clone();
        view_into(raw, &tag, |_| {
            self.n += 1;
            self.n
        })
    }
}

/// Вид `raw`: маска — `‹secret-T-N›` (`N` — от `take(значение)`), литералы меток — `‹secret›`;
/// без находок и меток — заимствование.
fn view_into<'r>(raw: &'r str, tag: &str, mut take: impl FnMut(&'r str) -> usize) -> Cow<'r, str> {
    let found = hits(raw);
    if found.is_empty() && !has_any_mark(raw) {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len() + found.len() * 12);
    let mut pos = 0;
    for r in found {
        push_plain(&raw[pos..r.start], &mut out);
        let n = take(&raw[r.clone()]);
        let _ = write!(out, "{NUMBERED_MARK_PREFIX}{tag}-{n}›");
        pos = r.end;
    }
    push_plain(&raw[pos..], &mut out);
    Cow::Owned(out)
}

/// Значения, отложенные до сборки текста ([`ObjectMasks::defer`]): в кусках — заменители
/// `<знак><индекс><знак>`, знак — из области частного использования Unicode, которого нет ни в
/// одном тексте сборки; [`Deferred::finish`] ставит значения.
#[derive(Debug)]
pub struct Deferred<'a> {
    values: Vec<&'a str>,
    sentinel: char,
}

/// Область частного использования Unicode — знаки заменителей [`Deferred`].
const PUA: std::ops::RangeInclusive<char> = '\u{E000}'..='\u{F8FF}';

impl<'a> Deferred<'a> {
    /// Заменитель — первый знак `U+E000…U+F8FF`, которого нет ни в одном из `texts` (всё, из чего
    /// собирается итог, и база значений): один проход по текстам с картой занятых знаков; все
    /// заняты — `None`.
    pub fn new(texts: &[&str]) -> Option<Self> {
        let base = u32::from(*PUA.start());
        let mut used = vec![false; (u32::from(*PUA.end()) - base + 1) as usize];
        for t in texts {
            for c in t.chars() {
                if PUA.contains(&c) {
                    used[(u32::from(c) - base) as usize] = true;
                }
            }
        }
        let i = used.iter().position(|u| !u)?;
        Some(Deferred {
            values: Vec::new(),
            sentinel: char::from_u32(base + i as u32)?,
        })
    }

    /// Итог: значения вместо заменителей в собранном тексте `text` и позиции значений —
    /// поставленных и масок текущей ревизии, перенесённых без правки (`keep` — их позиции в
    /// `text`, по возрастанию, вне заменителей).
    pub fn finish(self, text: &str, keep: &[Range<usize>]) -> Filled {
        let crlf = text.contains("\r\n");
        let mut out = String::with_capacity(text.len());
        let mut at: Vec<Range<usize>> = Vec::new();
        // сдвиг позиций `text` → итог после каждого поставленного значения
        let mut shifts: Vec<(usize, isize)> = Vec::new();
        let s = self.sentinel;
        let w = s.len_utf8();
        let mut pos = 0usize;
        while let Some(i) = text[pos..].find(s).map(|i| pos + i) {
            let after = i + w;
            let Some(j) = text[after..].find(s).map(|j| after + j) else {
                break;
            };
            match text[after..j]
                .parse::<usize>()
                .ok()
                .and_then(|k| self.values.get(k))
            {
                Some(v) => {
                    out.push_str(&text[pos..i]);
                    let start = out.len();
                    push_value(&mut out, v, crlf);
                    at.push(start..out.len());
                    pos = j + w;
                    shifts.push((pos, out.len() as isize - pos as isize));
                }
                None => {
                    out.push_str(&text[pos..after]);
                    pos = after;
                }
            }
        }
        out.push_str(&text[pos..]);
        for r in keep {
            let k = shifts.partition_point(|&(p, _)| p <= r.start);
            let d = if k == 0 { 0 } else { shifts[k - 1].1 };
            let (a, b) = (r.start as isize + d, r.end as isize + d);
            at.push(a as usize..b as usize);
        }
        at.sort_unstable_by_key(|r| (r.start, r.end));
        Filled {
            literals: marks_in(&out),
            text: out,
            at,
        }
    }
}

/// Диапазон `r` целиком внутри одной из находок `hits` (по возрастанию, без пересечений).
fn covered(hits: &[Range<usize>], r: &Range<usize>) -> bool {
    let i = hits.partition_point(|h| h.start <= r.start);
    i > 0 && hits[i - 1].end >= r.end
}

/// Находки `raw`, которые маскирует [`crate::mask_secrets`] (неверный диапазон — пропуск, как
/// там).
fn hits(raw: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut pos = 0;
    for h in scan_secrets(raw) {
        let r = h.range;
        if r.start < pos
            || r.end < r.start
            || r.end > raw.len()
            || !raw.is_char_boundary(r.start)
            || !raw.is_char_boundary(r.end)
        {
            continue;
        }
        pos = r.end;
        out.push(r);
    }
    out
}

/// Кусок исходника без находок — в вид: литералы меток — `‹secret›`.
fn push_plain(piece: &str, out: &mut String) {
    let mut pos = 0;
    for (m, _) in marks(piece) {
        out.push_str(&piece[pos..m.start]);
        out.push_str(SECRET_MARK);
        pos = m.end;
    }
    out.push_str(&piece[pos..]);
}

/// Значение `v` в итог: многострочное — с переводами строк нового текста (`crlf` — `\r\n`).
fn push_value(out: &mut String, v: &str, crlf: bool) {
    if !v.contains(['\r', '\n']) {
        out.push_str(v);
        return;
    }
    let lf = v.replace("\r\n", "\n").replace('\r', "\n");
    if crlf {
        out.push_str(&lf.replace('\n', "\r\n"));
    } else {
        out.push_str(&lf);
    }
}

#[cfg(test)]
mod tests;
