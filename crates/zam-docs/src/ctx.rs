//! Состояние одного разбора: лимиты, срок, счётчик символов, предупреждения.

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

/// Сборщик единиц: блоки текста под одним путём заголовков до [`UNIT_SOFT_CHARS`].
#[derive(Default)]
pub(crate) struct Units {
    pub units: Vec<Unit>,
    path: Vec<(u8, String)>,
    /// Текущая единица: адрес, текст, число символов.
    cur: Option<(Locator, String, usize)>,
}

impl Units {
    /// Заголовок уровня `level` (0 — `Title`) закрывает текущую единицу и меняет путь.
    pub fn heading(&mut self, level: u8, text: &str, at: Locator, ctx: &mut Ctx) {
        self.flush();
        while self.path.last().is_some_and(|(l, _)| *l >= level) {
            self.path.pop();
        }
        self.path.push((level, text.to_string()));
        self.block(text.to_string(), at, ctx);
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
            self.flush();
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
    pub fn flush(&mut self) {
        if let Some((locator, text, _)) = self.cur.take() {
            self.units.push(Unit {
                locator,
                heading_path: self.path.iter().map(|(_, h)| h.clone()).collect(),
                text,
            });
        }
    }

    /// Отдельная единица с заданным путём (сноски, колонтитулы, заметки докладчика).
    pub fn push(
        &mut self,
        locator: Locator,
        heading_path: Vec<String>,
        text: String,
        ctx: &mut Ctx,
    ) {
        self.flush();
        if ctx.stopped() {
            return;
        }
        let mut text = text;
        trim_in_place(&mut text);
        if text.is_empty() {
            return;
        }
        ctx.take_chars(&mut text);
        if !text.is_empty() {
            self.units.push(Unit {
                locator,
                heading_path,
                text,
            });
        }
    }

    pub fn finish(mut self) -> Vec<Unit> {
        self.flush();
        self.units
    }
}

/// Обрезка пробельных символов по краям без новой строки.
fn trim_in_place(s: &mut String) {
    let end = s.trim_end().len();
    s.truncate(end);
    let start = s.len() - s.trim_start().len();
    if start > 0 {
        s.drain(..start);
    }
}
