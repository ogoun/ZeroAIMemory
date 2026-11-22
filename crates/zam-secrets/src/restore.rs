//! Обратный путь маски для записи агентом (спецификация M7 §4.3.2, задача 6 M7.1; с круга правок
//! 3 — **восстановление по номеру метки**, решение координатора: эвристика выравнивания кругов
//! 1–2 давала новые случаи тихой подмены и потери значения, потому что у метки не было личности).
//! Обвязка — `zam_server::api::secret_restore`.
//!
//! **Нумерованный вид.** Текст, который агент правит и возвращает записью, он получает видом
//! [`FieldMasks::view`] ([`numbered_view`] — для одного текста): каждая маска поля — `‹secret-N›`,
//! N — порядковый номер маски в поле этой ревизии (с 1), детерминированно: находки
//! [`scan_secrets`], те же, что у [`crate::mask_text`]. Литерал метки, уже лежавший в исходнике
//! (`‹secret›` или `‹secret-N›`: прежний `secret_mark_kept`, импорт архива с маской, текст
//! человека), в виде — `‹secret›` без номера: номер есть только у маски со значением, и чужой номер
//! из текста значение не подставит. Поле из нескольких строк (строки `data` записи журнала, теги) —
//! один [`FieldMasks`]: нумерация сквозная, по порядку обхода.
//!
//! **Восстановление** ([`FieldMasks::restore`], [`restore`]): каждая `‹secret-N›` нового текста —
//! значение N-й маски того же поля базы (повтор номера — повтор значения, явный выбор агента);
//! метка без номера или с номером, которого в базе нет, записывается как есть и считается в
//! [`Restored::kept`]; маска базы, номер которой не вернулся, удалена правкой, как любой текст.
//! Многострочное значение (закрытый ключ) — с переводами строк нового текста.
//!
//! **Пересканирование.** Итог сканируется заново: значение, которое сканер в новом тексте не
//! закрывает целиком (снят забор кода вокруг `mysql -p‹secret-1›`, URL с паролем переписан
//! прозой, `password = ‹secret-1›` стал «пароль — ‹secret-1›»), выходит из-под маски — номера таких
//! строк нового текста — [`Restored::exposed`]; такую правку вызывающий отклоняет (422).
//!
//! [`Deferred`] — значения в тексте, который собирается позже (операции `PATCH` по секциям
//! применяются к исходнику текущей ревизии): в куски вместо значений встают заменители, после
//! сборки — значения и то же пересканирование.
//!
//! Время — линейное: один скан базы и один скан итога.
use std::borrow::Cow;
use std::fmt::Write as _;
use std::ops::Range;

use crate::{NUMBERED_MARK_PREFIX, SECRET_MARK, has_any_mark, marks, scan_secrets};

/// Итог восстановления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// Текст со значениями на месте меток с номером маски базы.
    pub text: String,
    /// Меток нового текста без значения (без номера или с номером, которого в базе нет) —
    /// записаны как есть.
    pub kept: usize,
    /// Строки нового текста (с 1, по возрастанию), где восстановленное значение сканер в итоге не
    /// закрывает (правка выводит секрет из-под маски).
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
    FieldMasks::new().view(raw)
}

/// Восстановление нового текста `edited`, правленного по нумерованному виду базы `base` (одно
/// поле).
pub fn restore(base: &str, edited: &str) -> Restored {
    if !has_mark(edited) {
        return Restored::same(edited.to_owned());
    }
    FieldMasks::of(base).restore(edited)
}

/// Значения масок поля по номерам (с 1): нумерация — по порядку кусков, добавленных
/// [`FieldMasks::view`] или [`FieldMasks::add`].
#[derive(Debug, Default, Clone)]
pub struct FieldMasks<'a> {
    values: Vec<&'a str>,
}

impl<'a> FieldMasks<'a> {
    pub fn new() -> Self {
        FieldMasks { values: Vec::new() }
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

    /// Значение маски `n` (с 1).
    pub fn value(&self, n: u32) -> Option<&'a str> {
        let i = (n as usize).checked_sub(1)?;
        self.values.get(i).copied()
    }

    /// Маски куска `raw` — в поле (нумерация продолжается), без вида.
    pub fn add(&mut self, raw: &'a str) {
        for r in hits(raw) {
            self.values.push(&raw[r]);
        }
    }

    /// Нумерованный вид куска `raw` поля: маски — `‹secret-N›` (нумерация продолжается), литералы
    /// меток исходника — `‹secret›`; без находок и меток — заимствование.
    pub fn view(&mut self, raw: &'a str) -> Cow<'a, str> {
        view_into(raw, |v| {
            self.values.push(v);
            self.values.len()
        })
    }

    /// Восстановление куска `edited` поля, правленного по нумерованному виду (правило — в
    /// описании модуля).
    pub fn restore(&self, edited: &str) -> Restored {
        let crlf = edited.contains("\r\n");
        let mut text = String::with_capacity(edited.len());
        // восстановленные значения: диапазон в итоге и позиция метки в `edited`
        let mut values: Vec<(Range<usize>, usize)> = Vec::new();
        let (mut pos, mut kept, mut any) = (0, 0, false);
        for (m, n) in marks(edited) {
            any = true;
            text.push_str(&edited[pos..m.start]);
            match n.and_then(|n| self.value(n)) {
                Some(v) => {
                    let s = text.len();
                    push_value(&mut text, v, crlf);
                    values.push((s..text.len(), m.start));
                }
                None => {
                    text.push_str(&edited[m.clone()]);
                    kept += 1;
                }
            }
            pos = m.end;
        }
        if !any {
            return Restored::same(edited.to_owned());
        }
        text.push_str(&edited[pos..]);
        let mut open: Vec<usize> = uncovered(&text, &values)
            .into_iter()
            .map(|i| values[i].1)
            .collect();
        open.sort_unstable();
        Restored {
            exposed: lines_of(edited, &open),
            text,
            kept,
        }
    }

    /// Значения куска `edited` — в `d` на потом ([`Deferred`]): метка с номером маски поля —
    /// заменитель, прочие — как есть (считаются в `kept`).
    pub fn defer(&self, edited: &str, d: &mut Deferred<'a>) -> String {
        let mut out = String::with_capacity(edited.len());
        let mut pos = 0;
        for (m, n) in marks(edited) {
            out.push_str(&edited[pos..m.start]);
            match n.and_then(|n| self.value(n)) {
                Some(v) => {
                    d.values.push(v);
                    let _ = write!(out, "{0}{1}{0}", d.sentinel, d.values.len() - 1);
                }
                None => {
                    out.push_str(&edited[m.clone()]);
                    d.kept += 1;
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

/// Значения, отложенные до сборки текста ([`FieldMasks::defer`]): в кусках — заменители
/// `<знак><индекс><знак>`, знак — из области частного использования Unicode, которого нет ни в
/// одном тексте сборки; [`Deferred::finish`] ставит значения и пересканирует итог.
#[derive(Debug)]
pub struct Deferred<'a> {
    values: Vec<&'a str>,
    kept: usize,
    sentinel: char,
}

impl<'a> Deferred<'a> {
    /// Заменитель — первый знак `U+E000…U+F8FF`, которого нет ни в одном из `texts` (всё, из чего
    /// собирается итог); все заняты — `None`.
    pub fn new(texts: &[&str]) -> Option<Self> {
        let sentinel = ('\u{E000}'..='\u{F8FF}').find(|c| texts.iter().all(|t| !t.contains(*c)))?;
        Some(Deferred {
            values: Vec::new(),
            kept: 0,
            sentinel,
        })
    }

    /// Меток без значения в отложенных кусках.
    pub fn kept(&self) -> usize {
        self.kept
    }

    /// Итог: значения вместо заменителей в собранном тексте `text`; строки, где значение сканер
    /// не закрывает, — номера строк итога, где значение считается одной строкой (как метка в
    /// нумерованном виде).
    pub fn finish(self, text: &str) -> Restored {
        if self.values.is_empty() {
            return Restored {
                text: text.to_owned(),
                kept: self.kept,
                exposed: Vec::new(),
            };
        }
        let crlf = text.contains("\r\n");
        let mut out = String::with_capacity(text.len());
        let mut values: Vec<(Range<usize>, usize)> = Vec::new();
        // переводы строк внутри уже поставленных значений
        let mut extra = 0usize;
        let mut lines: Vec<usize> = Vec::new();
        let mut rest = text;
        let s = self.sentinel;
        while let Some(i) = rest.find(s) {
            let after = &rest[i + s.len_utf8()..];
            let Some(j) = after.find(s) else { break };
            let Some(v) = after[..j]
                .parse::<usize>()
                .ok()
                .and_then(|k| self.values.get(k))
            else {
                out.push_str(&rest[..i + s.len_utf8()]);
                rest = after;
                continue;
            };
            out.push_str(&rest[..i]);
            let line = 1 + out.bytes().filter(|&b| b == b'\n').count() - extra;
            let start = out.len();
            push_value(&mut out, v, crlf);
            extra += out[start..].bytes().filter(|&b| b == b'\n').count();
            lines.push(line);
            values.push((start..out.len(), values.len()));
            rest = &after[j + s.len_utf8()..];
        }
        out.push_str(rest);
        let mut exposed: Vec<usize> = uncovered(&out, &values)
            .into_iter()
            .map(|i| lines[i])
            .collect();
        exposed.dedup();
        Restored {
            text: out,
            kept: self.kept,
            exposed,
        }
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
        view_into(raw, |_| {
            self.n += 1;
            self.n
        })
    }
}

/// Нумерованный вид `raw`: маска — `‹secret-N›`, `N` — от `take(значение)`; литералы меток —
/// `‹secret›`; без находок и меток — заимствование.
fn view_into<'r>(raw: &'r str, mut take: impl FnMut(&'r str) -> usize) -> Cow<'r, str> {
    let found = hits(raw);
    if found.is_empty() && !has_any_mark(raw) {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len() + found.len() * 4);
    let mut pos = 0;
    for r in found {
        push_plain(&raw[pos..r.start], &mut out);
        let n = take(&raw[r.clone()]);
        let _ = write!(out, "{NUMBERED_MARK_PREFIX}{n}›");
        pos = r.end;
    }
    push_plain(&raw[pos..], &mut out);
    Cow::Owned(out)
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

/// Кусок исходника без находок — в вид: литералы меток (`‹secret›`, `‹secret-N›`) — `‹secret›`.
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

/// Индексы значений `values` (диапазоны в `text`, по возрастанию), которые сканер итога `text` не
/// закрывает целиком.
fn uncovered(text: &str, values: &[(Range<usize>, usize)]) -> Vec<usize> {
    if values.is_empty() {
        return Vec::new();
    }
    let found = scan_secrets(text);
    let mut h = 0;
    let mut out = Vec::new();
    for (i, (r, _)) in values.iter().enumerate() {
        while h < found.len() && found[h].range.end <= r.start {
            h += 1;
        }
        let covered = found
            .get(h)
            .is_some_and(|x| x.range.start <= r.start && r.end <= x.range.end);
        if !covered {
            out.push(i);
        }
    }
    out
}

/// Номера строк (с 1, без повторов) позиций `at` (по возрастанию) текста `s`.
fn lines_of(s: &str, at: &[usize]) -> Vec<usize> {
    let (mut line, mut pos) = (1usize, 0usize);
    let mut out: Vec<usize> = Vec::with_capacity(at.len());
    for &a in at {
        line += s.as_bytes()[pos..a].iter().filter(|&&b| b == b'\n').count();
        pos = a;
        if out.last() != Some(&line) {
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
mod tests;
