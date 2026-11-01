//! Состояние одного разбора: лимиты, срок, счётчик символов, предупреждения.

use std::sync::Arc;
use std::time::Instant;

use crate::{Limits, Locator, Unit};

/// Мягкий предел единицы, символов: блоки абзацев под одним заголовком собираются до него.
///
/// Нарезка на чанки (задача 5) режет длинные единицы по предложениям, поэтому предел лишь
/// держит единицы соразмерными чанку и точными по `locator`.
pub(crate) const UNIT_SOFT_CHARS: usize = 1500;

/// Как часто проверять срок: раз на столько событий XML.
pub(crate) const DEADLINE_EVERY: u64 = 1024;

pub(crate) struct Ctx<'l> {
    pub limits: &'l Limits,
    deadline: Instant,
    warnings: Vec<String>,
    chars: u64,
    /// Лимит документа исчерпан: разбор останавливается, собранное возвращается.
    stopped: bool,
    /// Текст обрезан по `max_chars` при накоплении (см. [`Ctx::append`]).
    chars_full: bool,
    ticks: u64,
}

impl<'l> Ctx<'l> {
    pub fn new(limits: &'l Limits, deadline: Instant) -> Self {
        Ctx {
            limits,
            deadline,
            warnings: Vec::new(),
            chars: 0,
            stopped: false,
            chars_full: false,
            ticks: 0,
        }
    }

    /// Предупреждение без повторов.
    pub fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    /// Частичный результат по лимиту `limit` (имя поля конфигурации `[docs]`).
    pub fn truncated(&mut self, limit: &str) {
        self.warn(format!("truncated: {limit}"));
    }

    /// Лимит всего документа: дальше не разбираем.
    pub fn stop(&mut self, limit: &str) {
        self.truncated(limit);
        self.stopped = true;
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Счётчик событий; раз в [`DEADLINE_EVERY`] сверяет срок. `false` — пора остановиться.
    pub fn tick(&mut self) -> bool {
        if self.stopped {
            return false;
        }
        self.ticks += 1;
        if self.ticks.is_multiple_of(DEADLINE_EVERY) && Instant::now() >= self.deadline {
            self.stop("timeout_ms");
        }
        !self.stopped
    }

    /// Учитывает текст в `max_chars`; лишнее отрезает по границе символа и останавливает разбор.
    pub fn take_chars(&mut self, text: &mut String) {
        let left = self.limits.max_chars.saturating_sub(self.chars);
        let n = text.chars().count() as u64;
        if n <= left {
            self.chars += n;
            return;
        }
        let cut = text
            .char_indices()
            .nth(left as usize)
            .map_or(text.len(), |(i, _)| i);
        text.truncate(cut);
        self.chars += left;
        self.stop("max_chars");
    }

    /// Дописывает текст события XML в буфер абзаца или ячейки не сверх остатка `max_chars`
    /// (с учётом `pending` — уже накопленного, но ещё не выданного текста). Обрезка ставит
    /// [`Ctx::chars_full`]: разборщик выдаёт текущий блок и останавливается.
    pub fn append(&mut self, dst: &mut String, s: &str, pending: &mut u64) {
        let left = self
            .limits
            .max_chars
            .saturating_sub(self.chars.saturating_add(*pending));
        // байтов не меньше, чем символов: короткая строка заведомо помещается
        let n = if s.len() as u64 <= left {
            s.chars().count() as u64
        } else {
            match s.char_indices().nth(left as usize) {
                Some((cut, _)) => {
                    dst.push_str(&s[..cut]);
                    *pending += left;
                    self.chars_full = true;
                    self.truncated("max_chars");
                    return;
                }
                None => s.chars().count() as u64,
            }
        };
        dst.push_str(s);
        *pending += n;
    }

    /// Остаток `max_chars` исчерпан накопленным текстом.
    pub fn chars_full(&self) -> bool {
        self.chars_full
    }

    pub fn into_warnings(self) -> Vec<String> {
        self.warnings
    }
}

/// Предел текста заголовка в `heading_path`, символов: длинный заголовок копировался бы в путь
/// каждой следующей единицы. Полный текст заголовка — в тексте его единицы.
pub const HEADING_PATH_MAX_CHARS: usize = 512;

/// Служебный вес единицы в `max_chars`: тысячи пустых заголовков дали бы тысячи единиц, каждая
/// со своими выделениями памяти (`Unit`, `Vec` пути) при почти нулевом тексте.
pub(crate) const UNIT_OVERHEAD_CHARS: u64 = 64;

impl Ctx<'_> {
    /// Учитывает в `max_chars` уже выданное сверх текста (путь заголовков, вес единицы).
    fn charge(&mut self, n: u64) {
        self.chars = self.chars.saturating_add(n);
        if self.chars > self.limits.max_chars {
            self.stop("max_chars");
        }
    }
}

/// Подпись для `heading_path`: пробельные символы схлопнуты, не длиннее
/// [`HEADING_PATH_MAX_CHARS`] (обрезка — с «…»). Части склеиваются через пробел.
pub(crate) fn path_label<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for word in parts.into_iter().flat_map(str::split_whitespace) {
        if n > 0 {
            out.push(' ');
            n += 1;
        }
        for c in word.chars() {
            if n >= HEADING_PATH_MAX_CHARS {
                out.push('…');
                return out;
            }
            out.push(c);
            n += 1;
        }
    }
    out
}

/// Пробельные символы (юникодные, как `split_whitespace`: NBSP, U+3000…) схлопываются в один
/// пробел и обрезаются по краям — на месте, без копии: символ пишется не правее, чем читается.
pub(crate) fn collapse_ws(s: &mut String) {
    let mut b = std::mem::take(s).into_bytes();
    let (mut w, mut r, mut space) = (0usize, 0usize, false);
    while r < b.len() {
        let len = utf8_len(b[r]).min(b.len() - r);
        let ws = std::str::from_utf8(&b[r..r + len])
            .ok()
            .and_then(|c| c.chars().next())
            .is_some_and(char::is_whitespace);
        if ws {
            space = w > 0;
            r += len;
            continue;
        }
        if space {
            b[w] = b' ';
            w += 1;
            space = false;
        }
        b.copy_within(r..r + len, w);
        w += len;
        r += len;
    }
    b.truncate(w);
    *s = String::from_utf8(b).unwrap_or_default();
}

/// Длина символа UTF-8 по первому байту.
fn utf8_len(first: u8) -> usize {
    match first {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    }
}

/// Сборщик единиц: блоки текста под одним путём заголовков до [`UNIT_SOFT_CHARS`].
///
/// Путь хранится разделяемо (`Arc<str>`); в `max_chars` учитываются текст, путь каждой
/// выданной единицы и её служебный вес — поэтому всё выданное ограничено одним лимитом.
#[derive(Default)]
pub(crate) struct Units {
    pub units: Vec<Unit>,
    path: Vec<(u8, Arc<str>, u64)>,
    /// Текущая единица: адрес, текст, число символов.
    cur: Option<(Locator, String, usize)>,
}

impl Units {
    /// Заголовок уровня `level` (0 — `Title`) закрывает текущую единицу и меняет путь;
    /// текст заголовка (уже схлопнутый) становится первым блоком новой единицы.
    pub fn heading(&mut self, level: u8, text: String, at: Locator, ctx: &mut Ctx) {
        self.flush(ctx);
        while self.path.last().is_some_and(|(l, _, _)| *l >= level) {
            self.path.pop();
        }
        let label = path_label([text.as_str()]);
        let n = label.chars().count() as u64;
        self.path.push((level, Arc::from(label), n));
        self.block(text, at, ctx);
    }

    /// Блок текста (абзац, таблица); пустые пропускаются. Текст принимается во владение и
    /// обрезается на месте — без копии большого абзаца.
    pub fn block(&mut self, mut text: String, at: Locator, ctx: &mut Ctx) {
        if ctx.stopped() {
            return;
        }
        trim_in_place(&mut text);
        if text.is_empty() {
            return;
        }
        ctx.take_chars(&mut text);
        if text.is_empty() {
            return;
        }
        let n = text.chars().count();
        if let Some((_, _, len)) = &self.cur
            && *len + n > UNIT_SOFT_CHARS
        {
            self.flush(ctx);
        }
        match &mut self.cur {
            Some((_, buf, len)) => {
                buf.push_str("\n\n");
                buf.push_str(&text);
                *len += n + 2;
            }
            None => self.cur = Some((at, text, n)),
        }
    }

    /// Закрывает текущую единицу.
    pub fn flush(&mut self, ctx: &mut Ctx) {
        if let Some((locator, text, _)) = self.cur.take() {
            let path_chars: u64 = self.path.iter().map(|(_, _, n)| n).sum();
            ctx.charge(path_chars + UNIT_OVERHEAD_CHARS);
            self.units.push(Unit {
                locator,
                heading_path: self.path.iter().map(|(_, h, _)| h.to_string()).collect(),
                text,
            });
        }
    }

    /// Отдельная единица с заданным путём (сноски, колонтитулы, заметки докладчика); элементы
    /// пути — короткие подписи ([`path_label`] или постоянные строки).
    pub fn push(
        &mut self,
        locator: Locator,
        heading_path: Vec<String>,
        text: String,
        ctx: &mut Ctx,
    ) {
        self.flush(ctx);
        if ctx.stopped() {
            return;
        }
        let mut text = text;
        trim_in_place(&mut text);
        if text.is_empty() {
            return;
        }
        // путь, имя листа в адресе и служебный вес — до текста: выдача не выходит за `max_chars`
        let mut path_chars: u64 = heading_path.iter().map(|h| h.chars().count() as u64).sum();
        if let Locator::Sheet { name, range } = &locator {
            path_chars += (name.chars().count() + range.len()) as u64;
        }
        if ctx.chars.saturating_add(path_chars + UNIT_OVERHEAD_CHARS) >= ctx.limits.max_chars {
            ctx.stop("max_chars");
            return;
        }
        ctx.charge(path_chars + UNIT_OVERHEAD_CHARS);
        ctx.take_chars(&mut text);
        if !text.is_empty() {
            self.units.push(Unit {
                locator,
                heading_path,
                text,
            });
        }
    }

    pub fn finish(mut self, ctx: &mut Ctx) -> Vec<Unit> {
        self.flush(ctx);
        self.units
    }
}

/// Обрезка пробельных символов по краям без новой строки.
pub(crate) fn trim_in_place(s: &mut String) {
    let end = s.trim_end().len();
    s.truncate(end);
    let start = s.len() - s.trim_start().len();
    if start > 0 {
        s.drain(..start);
    }
}
