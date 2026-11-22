//! Обратный путь маски для записи агентом (спецификация M7 §4.3.2, задача 6 M7.1; с круга правок
//! 3 — **восстановление по номеру метки**, решение координатора: эвристика выравнивания кругов
//! 1–2 давала новые случаи тихой подмены и потери значения, потому что у метки не было личности).
//! Обвязка — `zam_server::api::secret_restore`.
//!
//! **Нумерованный вид.** Текст, который агент правит и возвращает записью, он получает видом
//! [`FieldMasks::view`] ([`numbered_view`] — для одного текста, [`Numbering`] — без хранения
//! значений): каждая маска поля — `‹secret-N›`, N — порядковый номер маски в поле этой ревизии
//! (с 1), детерминированно: находки [`scan_secrets`], те же, что у [`crate::mask_text`]. Литерал
//! метки, уже лежавший в исходнике (`‹secret›` или `‹secret-N›`: прежний `secret_mark_kept`,
//! импорт архива с маской, текст человека), в виде — `‹secret›` без номера: номер есть только у
//! маски со значением, и чужой номер из текста значение не подставит. Поле из нескольких строк
//! (строки `data` записи журнала, теги) — один [`FieldMasks`]: нумерация сквозная, по порядку
//! обхода.
//!
//! **Восстановление** ([`FieldMasks::restore`], [`restore`]): каждая `‹secret-N›` нового текста —
//! значение N-й маски того же поля базы (повтор номера — повтор значения, явный выбор агента);
//! метка без номера или с номером, которого в базе нет, записывается как есть; маска базы, номер
//! которой не вернулся, удалена правкой, как любой текст. [`Restored::kept`] — меток без номера
//! **сверх** числа литералов в виде базы поля (литералы из архивов с маской не предупреждают на
//! каждой правке) плюс номера вне поля (§4.3.2 п. 2, круг правок 4). Многострочное значение
//! (закрытый ключ) — с переводами строк нового текста.
//!
//! **Покрытие всего текста** ([`exposure`], §4.3.2 п. 5, круг правок 4): итог сканируется заново;
//! у каждого секретного значения — маски исходника текущей ревизии и восстановленного — число
//! вхождений в итоге, не покрытых находкой, не должно быть больше, чем в исходнике. Иначе
//! (снят или открыт забор кода, `ключ: значение` переписан прозой, разобран URL — и вне правленого
//! места) — строки итога [`Restored::exposed`]; такую правку вызывающий отклоняет (422).
//! Вхождения всех значений — один проход Aho-Corasick, покрытие — по отсортированным находкам.
//!
//! [`Deferred`] — значения в тексте, который собирается позже (операции `PATCH` по секциям
//! применяются к исходнику текущей ревизии): в куски вместо значений встают заменители, после
//! сборки — значения и та же проверка покрытия.
//!
//! Время — линейное: сканы исходника и итога, один проход поиска значений, один проход выбора
//! заменителя.
use std::borrow::Cow;
use std::fmt::Write as _;
use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};

use crate::{NUMBERED_MARK_PREFIX, SECRET_MARK, has_any_mark, marks, scan_secrets};

/// Итог восстановления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// Текст со значениями на месте меток с номером маски базы.
    pub text: String,
    /// Меток нового текста без значения для предупреждения `secret_mark_kept`: без номера —
    /// сверх литералов в виде базы поля, с номером вне поля — все (записаны как есть).
    pub kept: usize,
    /// Строки итога (с 1, по возрастанию), где значение секрета не покрыто находкой чаще, чем в
    /// исходнике (правка выводит секрет из-под маски).
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

/// Есть ли в тексте метка — `‹secret›` или `‹secret-N›`: без неё восстанавливать нечего, база не
/// сканируется.
pub fn has_mark(s: &str) -> bool {
    has_any_mark(s)
}

/// Меток в тексте (`‹secret›` и `‹secret-N›`).
pub fn marks_in(s: &str) -> usize {
    marks(s).count()
}

/// Нумерованный вид одного текста ([`FieldMasks::view`]); без находок и меток — заимствование.
pub fn numbered_view(raw: &str) -> Cow<'_, str> {
    Numbering::new().view(raw)
}

/// Метки с номером — `‹secret›` (вид старой ревизии и кусок нумерованного вида без номеров,
/// §4.3.2 п. 1); без них — заимствование.
pub fn unnumbered(s: &str) -> Cow<'_, str> {
    if !s.contains(NUMBERED_MARK_PREFIX) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for (m, n) in marks(s) {
        if n.is_some() {
            out.push_str(&s[pos..m.start]);
            out.push_str(SECRET_MARK);
            pos = m.end;
        }
    }
    out.push_str(&s[pos..]);
    Cow::Owned(out)
}

/// Восстановление нового текста `edited`, правленного по нумерованному виду базы `base` (одно
/// поле; база — исходник текущей ревизии).
pub fn restore(base: &str, edited: &str) -> Restored {
    if !has_mark(edited) {
        return Restored::same(edited.to_owned());
    }
    FieldMasks::of(base).restore(edited)
}

/// Кусок исходника поля и его находки.
#[derive(Debug, Clone)]
struct Piece<'a> {
    raw: &'a str,
    hits: Vec<Range<usize>>,
}

/// Значения масок поля по номерам (с 1): нумерация — по порядку кусков, добавленных
/// [`FieldMasks::view`] или [`FieldMasks::add`]; куски с находками — для проверки покрытия.
#[derive(Debug, Default, Clone)]
pub struct FieldMasks<'a> {
    values: Vec<&'a str>,
    pieces: Vec<Piece<'a>>,
    /// Литералов меток в виде поля (меток исходника вне находок).
    literals: usize,
}

/// Счёт меток нового текста.
#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    /// Без номера.
    unnumbered: usize,
    /// С номером, которого в поле нет.
    stray: usize,
}

impl Counts {
    /// Предупреждение: без номера — сверх `literals`, вне поля — все.
    fn kept(self, literals: usize) -> usize {
        self.unnumbered.saturating_sub(literals) + self.stray
    }
}

impl<'a> FieldMasks<'a> {
    pub fn new() -> Self {
        FieldMasks::default()
    }

    /// Поле из одного текста.
    pub fn of(raw: &'a str) -> Self {
        let mut f = FieldMasks::new();
        f.add(raw);
        f
    }

    /// Масок в поле.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Значения масок поля по порядку номеров.
    pub fn values(&self) -> &[&'a str] {
        &self.values
    }

    /// Литералов меток в виде поля.
    pub fn literals(&self) -> usize {
        self.literals
    }

    /// Значение маски `n` (с 1).
    pub fn value(&self, n: u32) -> Option<&'a str> {
        let i = (n as usize).checked_sub(1)?;
        self.values.get(i).copied()
    }

    /// Маски куска `raw` — в поле (нумерация продолжается), без вида.
    pub fn add(&mut self, raw: &'a str) {
        let found = hits(raw);
        for r in &found {
            self.values.push(&raw[r.clone()]);
        }
        self.literals += marks(raw).filter(|(m, _)| !covered(&found, m)).count();
        self.pieces.push(Piece { raw, hits: found });
    }

    /// Нумерованный вид куска `raw` поля: маски — `‹secret-N›` (нумерация продолжается), литералы
    /// меток исходника — `‹secret›`; без находок и меток — заимствование.
    pub fn view(&mut self, raw: &'a str) -> Cow<'a, str> {
        let found = hits(raw);
        let (view, literals) = view_into(raw, &found, |v| {
            self.values.push(v);
            self.values.len()
        });
        self.literals += literals;
        self.pieces.push(Piece { raw, hits: found });
        view
    }

    /// Восстановление куска `edited` (поле из одного куска — исходник базы), правленного по
    /// нумерованному виду; покрытие — по исходнику поля (правило — в описании модуля).
    pub fn restore(&self, edited: &str) -> Restored {
        if !has_mark(edited) {
            return Restored::same(edited.to_owned());
        }
        let mut c = Counts::default();
        let text = self.fill_values(edited, &mut c);
        let cur: Vec<(&str, &[Range<usize>])> = self
            .pieces
            .iter()
            .map(|p| (p.raw, p.hits.as_slice()))
            .collect();
        let exposed = exposure_with(&cur, &[text.as_str()], &[])
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        Restored {
            text,
            kept: c.kept(self.literals),
            exposed,
        }
    }

    /// Восстановление кусков поля `edited` (все строки поля по порядку) без проверки покрытия —
    /// её делает вызывающий ([`exposure`]) по нужным кускам исходника; второе — предупреждение
    /// `secret_mark_kept` поля.
    pub fn restore_texts(&self, edited: &[&str]) -> (Vec<String>, usize) {
        let mut c = Counts::default();
        let texts = edited
            .iter()
            .map(|e| {
                if has_mark(e) {
                    self.fill_values(e, &mut c)
                } else {
                    (*e).to_owned()
                }
            })
            .collect();
        (texts, c.kept(self.literals))
    }

    /// `edited` со значениями вместо меток с номером маски поля; прочие метки — как есть (счёт —
    /// в `c`).
    fn fill_values(&self, edited: &str, c: &mut Counts) -> String {
        let crlf = edited.contains("\r\n");
        let mut text = String::with_capacity(edited.len());
        let mut pos = 0;
        for (m, n) in marks(edited) {
            text.push_str(&edited[pos..m.start]);
            match n.map(|n| (n, self.value(n))) {
                Some((_, Some(v))) => push_value(&mut text, v, crlf),
                Some((_, None)) => {
                    text.push_str(&edited[m.clone()]);
                    c.stray += 1;
                }
                None => {
                    text.push_str(&edited[m.clone()]);
                    c.unnumbered += 1;
                }
            }
            pos = m.end;
        }
        text.push_str(&edited[pos..]);
        text
    }

    /// Значения куска `edited` — в `d` на потом ([`Deferred`]): метка с номером маски поля —
    /// заменитель, прочие — как есть (считаются для предупреждения).
    pub fn defer(&self, edited: &str, d: &mut Deferred<'a>) -> String {
        let mut out = String::with_capacity(edited.len());
        let mut pos = 0;
        for (m, n) in marks(edited) {
            out.push_str(&edited[pos..m.start]);
            match n.map(|n| self.value(n)) {
                Some(Some(v)) => {
                    d.values.push(v);
                    let _ = write!(out, "{0}{1}{0}", d.sentinel, d.values.len() - 1);
                }
                Some(None) => {
                    out.push_str(&edited[m.clone()]);
                    d.counts.stray += 1;
                }
                None => {
                    out.push_str(&edited[m.clone()]);
                    d.counts.unnumbered += 1;
                }
            }
            pos = m.end;
        }
        out.push_str(&edited[pos..]);
        out
    }

    /// `s` со значениями вместо меток с номером маски поля (адрес секции `PATCH` по заголовку
    /// нумерованного вида — к заголовку исходника); прочее — как есть.
    pub fn fill<'s>(&self, s: &'s str) -> Cow<'s, str> {
        if !has_mark(s) {
            return Cow::Borrowed(s);
        }
        let mut out = String::with_capacity(s.len());
        let mut pos = 0;
        for (m, n) in marks(s) {
            out.push_str(&s[pos..m.start]);
            match n.and_then(|n| self.value(n)) {
                Some(v) => out.push_str(v),
                None => out.push_str(&s[m.clone()]),
            }
            pos = m.end;
        }
        out.push_str(&s[pos..]);
        Cow::Owned(out)
    }
}

/// Нумерация масок поля без значений — для выдачи ([`FieldMasks::view`] без хранения значений:
/// вид строится на месте, исходник заменяется видом).
#[derive(Debug, Default, Clone, Copy)]
pub struct Numbering {
    n: usize,
}

impl Numbering {
    pub fn new() -> Self {
        Numbering { n: 0 }
    }

    /// Нумерованный вид куска `raw` поля (нумерация продолжается) — как [`FieldMasks::view`].
    pub fn view<'r>(&mut self, raw: &'r str) -> Cow<'r, str> {
        view_into(raw, &hits(raw), |_| {
            self.n += 1;
            self.n
        })
        .0
    }
}

/// Нумерованный вид `raw` с находками `found`: маска — `‹secret-N›`, `N` — от `take(значение)`;
/// литералы меток — `‹secret›` (их число — второе); без находок и меток — заимствование.
fn view_into<'r>(
    raw: &'r str,
    found: &[Range<usize>],
    mut take: impl FnMut(&'r str) -> usize,
) -> (Cow<'r, str>, usize) {
    if found.is_empty() && !has_any_mark(raw) {
        return (Cow::Borrowed(raw), 0);
    }
    let mut out = String::with_capacity(raw.len() + found.len() * 4);
    let (mut pos, mut literals) = (0, 0);
    for r in found {
        literals += push_plain(&raw[pos..r.start], &mut out);
        let n = take(&raw[r.clone()]);
        let _ = write!(out, "{NUMBERED_MARK_PREFIX}{n}›");
        pos = r.end;
    }
    literals += push_plain(&raw[pos..], &mut out);
    (Cow::Owned(out), literals)
}

/// Значения, отложенные до сборки текста ([`FieldMasks::defer`]): в кусках — заменители
/// `<знак><индекс><знак>`, знак — из области частного использования Unicode, которого нет ни в
/// одном тексте сборки; [`Deferred::finish`] ставит значения и проверяет покрытие итога.
#[derive(Debug)]
pub struct Deferred<'a> {
    values: Vec<&'a str>,
    counts: Counts,
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
            counts: Counts::default(),
            sentinel: char::from_u32(base + i as u32)?,
        })
    }

    /// Итог: значения вместо заменителей в собранном тексте `text`; предупреждение — сверх
    /// `literals` (литералов в виде базы поля); покрытие — по исходнику текущей ревизии `current`
    /// (значения его масок и поставленные).
    pub fn finish(self, text: &str, current: &str, literals: usize) -> Restored {
        let kept = self.counts.kept(literals);
        let crlf = text.contains("\r\n");
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        let s = self.sentinel;
        let w = s.len_utf8();
        while let Some(i) = rest.find(s) {
            let after = &rest[i + w..];
            let Some(j) = after.find(s) else { break };
            match after[..j]
                .parse::<usize>()
                .ok()
                .and_then(|k| self.values.get(k))
            {
                Some(v) => {
                    out.push_str(&rest[..i]);
                    push_value(&mut out, v, crlf);
                    rest = &after[j + w..];
                }
                None => {
                    out.push_str(&rest[..i + w]);
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        let exposed = exposure(&[current], &[out.as_str()], &self.values)
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        Restored {
            text: out,
            kept,
            exposed,
        }
    }
}

/// Покрытие всего текста (§4.3.2 п. 5): у каждого значения — маски кусков `current` (исходник
/// текущей ревизии) и `extra` (восстановленные) — вхождений в кусках `new`, не покрытых находкой,
/// не больше, чем в `current`. Итог — `(кусок new, строка с 1)` непокрытых вхождений значений,
/// где их больше (по возрастанию, без повторов); пусто — правка значений не открывает.
pub fn exposure(current: &[&str], new: &[&str], extra: &[&str]) -> Vec<(usize, usize)> {
    let found: Vec<Vec<Range<usize>>> = current.iter().map(|c| hits(c)).collect();
    let cur: Vec<(&str, &[Range<usize>])> = current
        .iter()
        .zip(&found)
        .map(|(c, h)| (*c, h.as_slice()))
        .collect();
    exposure_with(&cur, new, extra)
}

/// [`exposure`] с находками кусков `current`, уже известными.
fn exposure_with(
    current: &[(&str, &[Range<usize>])],
    new: &[&str],
    extra: &[&str],
) -> Vec<(usize, usize)> {
    let mut uniq: Vec<&str> = current
        .iter()
        .flat_map(|(raw, h)| h.iter().map(|r| &raw[r.clone()]))
        .chain(extra.iter().copied())
        .filter(|v| !v.is_empty())
        .collect();
    uniq.sort_unstable();
    uniq.dedup();
    if uniq.is_empty() {
        return Vec::new();
    }
    // образцы: значение, у многострочного — ещё с `\n` и с `\r\n` (окончания итога — как у правки)
    let mut pats: Vec<Cow<str>> = Vec::with_capacity(uniq.len());
    let mut owner: Vec<usize> = Vec::with_capacity(uniq.len());
    for (i, v) in uniq.iter().enumerate() {
        pats.push(Cow::Borrowed(v));
        owner.push(i);
        if v.contains(['\r', '\n']) {
            let lf = v.replace("\r\n", "\n").replace('\r', "\n");
            let crlf = lf.replace('\n', "\r\n");
            for p in [lf, crlf] {
                if p != *v {
                    pats.push(Cow::Owned(p));
                    owner.push(i);
                }
            }
        }
    }
    let Ok(ac) = AhoCorasick::builder()
        .match_kind(MatchKind::Standard)
        .build(pats.iter().map(|p| p.as_ref()))
    else {
        // автомат не строится (предел размера) — безопасный путь: отказ
        return vec![(0, 1)];
    };
    let mut open_cur = vec![0usize; uniq.len()];
    for (raw, h) in current {
        for m in ac.find_overlapping_iter(*raw) {
            if !covered(h, &m.range()) {
                open_cur[owner[m.pattern().as_usize()]] += 1;
            }
        }
    }
    let mut open_new = vec![0usize; uniq.len()];
    // непокрытые вхождения итога: (кусок, начало, значение)
    let mut open: Vec<(usize, usize, usize)> = Vec::new();
    for (k, text) in new.iter().enumerate() {
        let h = hits(text);
        for m in ac.find_overlapping_iter(*text) {
            if !covered(&h, &m.range()) {
                let v = owner[m.pattern().as_usize()];
                open_new[v] += 1;
                open.push((k, m.start(), v));
            }
        }
    }
    open.retain(|&(_, _, v)| open_new[v] > open_cur[v]);
    if open.is_empty() {
        return Vec::new();
    }
    open.sort_unstable();
    // номера строк — нарастающим итогом по каждому куску
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(open.len());
    let (mut piece, mut line, mut pos) = (usize::MAX, 1usize, 0usize);
    for (k, at, _) in open {
        if k != piece {
            (piece, line, pos) = (k, 1, 0);
        }
        line += new[k].as_bytes()[pos..at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        pos = at;
        if out.last() != Some(&(k, line)) {
            out.push((k, line));
        }
    }
    out
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

/// Кусок исходника без находок — в вид: литералы меток (`‹secret›`, `‹secret-N›`) — `‹secret›`;
/// их число.
fn push_plain(piece: &str, out: &mut String) -> usize {
    let (mut pos, mut n) = (0, 0);
    for (m, _) in marks(piece) {
        out.push_str(&piece[pos..m.start]);
        out.push_str(SECRET_MARK);
        pos = m.end;
        n += 1;
    }
    out.push_str(&piece[pos..]);
    n
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
