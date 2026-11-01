//! Таблицы (XLSX, XLS, ODS, CSV): общий приём строк листа и два приёмника — единицы для
//! [`crate::extract`] и страница листа для превью ([`sheet_page`]).
//!
//! Разборщики форматов (`xlsx`, `xls`, `ods`, `csv`) потоково передают ячейки в [`Feed`]: он
//! следит за порядком строк и столбцов, пределом столбцов и `max_cells` на лист, а приёмник
//! ([`Sink`]) пишет текст сразу в итоговые строки с учётом `max_chars`. Сетка листа в памяти
//! не строится: размер листа из `dimension`/`ref`, пустые ячейки и повторы строк ODS ничего не
//! выделяют. Общие строки XLSX и XLS — [`Strings`] с потолком `max_chars` и `max_cells`.

use std::time::{Duration, Instant};

use crate::ctx::{Ctx, Units, path_label};
use crate::{DocError, DocKind, Limits, Locator, Unit};

/// Мягкий предел единицы листа, символов: ~`chunk_target_max` (500 токенов) × 3,5 символа
/// на токен (в крейте нет токенизатора; уточнение нарезки — в конвейере, задача 5).
pub(crate) const SHEET_UNIT_CHARS: usize = 1750;

/// Столбцов листа (Excel: `XFD`); ячейки правее отбрасываются.
pub(crate) const MAX_COLS: u32 = 16_384;

/// Срок разбора для [`sheet_page`] (умолчание `timeout_ms`): превью вызывается без срока.
const PAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// Имя единственного листа CSV.
pub(crate) const CSV_SHEET: &str = "CSV";

/// Страница листа для превью (задача 8): шапка, строки окна и размеры листа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetPage {
    /// Имена листов книги по порядку (подписи не длиннее 512 символов).
    pub sheets: Vec<String>,
    /// Имя показанного листа.
    pub sheet: String,
    /// Шапка — первая непустая строка листа; столбцы с `A`, хвостовые пустые отброшены.
    pub header: Vec<String>,
    /// Строки окна по порядку листа, пустые строки — пустые векторы.
    pub rows: Vec<Vec<String>>,
    /// Строк данных под шапкой — до последней непустой строки листа.
    pub total_rows: u32,
    /// Столбцов листа — от `A` до последнего непустого.
    pub cols: u32,
    /// Номер строки листа (с 1) первой строки `rows`.
    pub first_row: u32,
    /// Предупреждения разбора (`truncated: <лимит>`), как у [`crate::Extracted`].
    pub warnings: Vec<String>,
}

/// Страница листа `sheet` (пустое имя — первый лист): `rows` строк, начиная со строки листа
/// `from` (с 1, как в `Locator::Sheet`; не выше строки под шапкой).
///
/// Лист разбирается целиком (нужны `total_rows` и `cols`), но в память попадает только окно:
/// его ячейки (с пустыми в разрывах) — не больше `max_cells`, текст — не больше `max_chars`.
pub fn sheet_page(
    kind: DocKind,
    bytes: &[u8],
    sheet: &str,
    from: u32,
    rows: u32,
    limits: &Limits,
) -> Result<SheetPage, DocError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(DocError::TooLarge);
    }
    let mut ctx = Ctx::new(limits, Instant::now() + PAGE_TIMEOUT);
    let mut page = Page::new(sheet, from, rows);
    feed(kind, bytes, &mut page, &mut ctx)?;
    if !page.found && !sheet.is_empty() {
        return Err(DocError::Malformed(format!("нет листа {sheet:?}")));
    }
    Ok(page.finish(ctx.into_warnings()))
}

/// Единицы таблицы для [`crate::extract`].
pub(crate) fn extract(kind: DocKind, bytes: &[u8], ctx: &mut Ctx) -> Result<Vec<Unit>, DocError> {
    let mut blocks = Blocks::default();
    feed(kind, bytes, &mut blocks, ctx)?;
    Ok(blocks.units.finish(ctx))
}

fn feed<S: Sink>(kind: DocKind, bytes: &[u8], sink: &mut S, ctx: &mut Ctx) -> Result<(), DocError> {
    let mut feed = Feed::new(sink);
    match kind {
        DocKind::Xlsx => crate::xlsx::read(bytes, &mut feed, ctx),
        DocKind::Xls => crate::xls::read(bytes, &mut feed, ctx),
        DocKind::Ods => crate::ods::read(bytes, &mut feed, ctx),
        DocKind::Csv => crate::csv::read(bytes, &mut feed, ctx),
        _ => Err(DocError::Unsupported),
    }?;
    feed.end_sheet(ctx);
    Ok(())
}

/// Приёмник ячеек листа. Ячейки приходят по возрастанию строк и столбцов, только непустые.
pub(crate) trait Sink {
    /// Начало листа (подпись имени); `false` — лист не нужен, разборщик его пропускает.
    fn sheet(&mut self, name: &str, ctx: &mut Ctx) -> bool;
    /// Первый непустой кусок текста ячейки `row` (с 1), `col` (с 0).
    fn open(&mut self, row: u32, col: u32, ctx: &mut Ctx);
    /// Кусок текста открытой ячейки.
    fn text(&mut self, s: &str, ctx: &mut Ctx);
    /// Конец открытой ячейки.
    fn close(&mut self, ctx: &mut Ctx);
    /// Конец строки, в которой были непустые ячейки.
    fn end_row(&mut self, row: u32, ctx: &mut Ctx);
    /// Конец нужного листа.
    fn end_sheet(&mut self, ctx: &mut Ctx);
}

/// Приём ячеек от разборщика формата: порядок строк и столбцов, пределы, вызовы [`Sink`].
///
/// Строка или столбец не правее предыдущих (повтор, обратный порядок) получают следующий
/// номер; ячейки правее [`MAX_COLS`] отбрасываются; непустых ячеек листа — не больше
/// `max_cells` (дальше лист пропускается с `truncated: max_cells`); листов — не больше
/// `max_entries` (`truncated: max_entries`).
pub(crate) struct Feed<'s, S: Sink> {
    sink: &'s mut S,
    on: bool,
    sheets: u64,
    row: u32,
    row_open: bool,
    row_cells: bool,
    next_col: u32,
    cell: Option<u32>,
    opened: bool,
    cells: u64,
    /// Были ячейки правее [`MAX_COLS`] — предупреждение в конце листа.
    wide: bool,
}

impl<'s, S: Sink> Feed<'s, S> {
    fn new(sink: &'s mut S) -> Self {
        Feed {
            sink,
            on: false,
            sheets: 0,
            row: 0,
            row_open: false,
            row_cells: false,
            next_col: 0,
            cell: None,
            opened: false,
            cells: 0,
            wide: false,
        }
    }

    /// Начало листа `name`; `false` — лист пропускается.
    pub fn sheet(&mut self, name: &str, ctx: &mut Ctx) -> bool {
        self.end_sheet(ctx);
        if ctx.stopped() {
            return false;
        }
        self.sheets += 1;
        if self.sheets > u64::from(ctx.limits.max_entries) {
            ctx.truncated("max_entries");
            return false;
        }
        self.row = 0;
        self.cells = 0;
        self.on = self.sink.sheet(&path_label([name]), ctx);
        self.on
    }

    /// Разбирать ли лист дальше.
    pub fn live(&self, ctx: &Ctx) -> bool {
        self.on && !ctx.stopped()
    }

    /// Начало строки `r` (с 1); без номера — следующая.
    pub fn row(&mut self, r: Option<u32>, ctx: &mut Ctx) {
        if self.row_open {
            self.end_row(ctx);
        }
        self.row = match r {
            Some(r) if r > self.row => r,
            _ => self.row.saturating_add(1),
        };
        self.row_open = true;
        self.next_col = 0;
    }

    /// `n` пустых строк (повтор строк ODS).
    pub fn skip_rows(&mut self, n: u32, ctx: &mut Ctx) {
        if self.row_open {
            self.end_row(ctx);
        }
        self.row = self.row.saturating_add(n);
    }

    /// Начало ячейки столбца `c` (с 0); без номера — следующего.
    pub fn cell(&mut self, c: Option<u32>, ctx: &mut Ctx) {
        if !self.row_open {
            self.row(None, ctx);
        }
        if self.cell.is_some() {
            self.end_cell(ctx);
        }
        let col = c.filter(|&c| c >= self.next_col).unwrap_or(self.next_col);
        if col < MAX_COLS {
            self.cell = Some(col);
            self.next_col = col + 1;
        } else {
            self.next_col = MAX_COLS;
            self.wide = true;
        }
    }

    /// `n` пустых ячеек (повтор столбцов ODS).
    pub fn skip_cols(&mut self, n: u32) {
        self.next_col = self.next_col.saturating_add(n).min(MAX_COLS);
    }

    /// Свободных столбцов справа от текущего.
    pub fn cols_left(&self) -> u32 {
        MAX_COLS - self.next_col
    }

    /// Кусок текста текущей ячейки; пустой не открывает ячейку.
    pub fn text(&mut self, s: &str, ctx: &mut Ctx) {
        if !self.on || s.is_empty() || ctx.stopped() {
            return;
        }
        let Some(col) = self.cell else {
            return;
        };
        if !self.opened {
            self.cells += 1;
            if self.cells > ctx.limits.max_cells {
                ctx.truncated("max_cells");
                self.cell = None;
                self.end_sheet(ctx);
                return;
            }
            self.sink.open(self.row, col, ctx);
            self.opened = true;
            self.row_cells = true;
        }
        self.sink.text(s, ctx);
    }

    /// Ячейка целиком: начало, текст, конец.
    pub fn put(&mut self, c: Option<u32>, s: &str, ctx: &mut Ctx) {
        self.cell(c, ctx);
        self.text(s, ctx);
        self.end_cell(ctx);
    }

    /// Ячейка в строке `r` (XLS: записи ячеек по строкам).
    pub fn put_at(&mut self, r: u32, c: u32, s: &str, ctx: &mut Ctx) {
        if !self.row_open || r != self.row {
            self.row(Some(r), ctx);
        }
        self.put(Some(c), s, ctx);
    }

    pub fn end_cell(&mut self, ctx: &mut Ctx) {
        if self.opened && self.on {
            self.sink.close(ctx);
        }
        self.cell = None;
        self.opened = false;
    }

    pub fn end_row(&mut self, ctx: &mut Ctx) {
        if self.cell.is_some() || self.opened {
            self.end_cell(ctx);
        }
        if self.row_cells && self.on {
            self.sink.end_row(self.row, ctx);
        }
        self.row_open = false;
        self.row_cells = false;
        self.next_col = 0;
    }

    /// Конец листа (повторный вызов ничего не делает).
    pub fn end_sheet(&mut self, ctx: &mut Ctx) {
        if std::mem::take(&mut self.wide) {
            ctx.warn("skipped: columns after XFD".into());
        }
        if self.on {
            self.end_row(ctx);
            self.sink.end_sheet(ctx);
        }
        self.on = false;
        self.row_open = false;
    }
}

/// Общие строки книги (XLSX `sharedStrings.xml`, XLS `SST`): один буфер и концы строк.
///
/// Потолки: текст всех строк — `max_chars` символов, число строк — `max_cells` (на лист
/// больше ячеек не бывает); сверх них строки обрезаются или пусты, с `truncated: <лимит>`.
#[derive(Default)]
pub(crate) struct Strings {
    buf: String,
    ends: Vec<usize>,
    chars: u64,
    open: bool,
    full: bool,
}

impl Strings {
    /// Начало строки; `false` — число строк исчерпано, дальше разбирать незачем.
    pub fn begin(&mut self, ctx: &mut Ctx) -> bool {
        if self.ends.len() as u64 >= ctx.limits.max_cells {
            ctx.truncated("max_cells");
            return false;
        }
        self.open = true;
        true
    }

    /// Кусок текста текущей строки в пределах `max_chars`.
    pub fn push(&mut self, s: &str, ctx: &mut Ctx) {
        if !self.open || self.full {
            return;
        }
        let left = ctx.limits.max_chars.saturating_sub(self.chars);
        if s.len() as u64 <= left {
            self.chars += s.chars().count() as u64;
            self.buf.push_str(s);
            return;
        }
        match s.char_indices().nth(left as usize) {
            Some((cut, _)) => {
                self.buf.push_str(&s[..cut]);
                self.chars += left;
                self.full = true;
                ctx.truncated("max_chars");
            }
            None => {
                self.chars += s.chars().count() as u64;
                self.buf.push_str(s);
            }
        }
    }

    /// Конец текущей строки.
    pub fn end(&mut self) {
        if self.open {
            self.ends.push(self.buf.len());
            self.open = false;
        }
    }

    /// Строка по номеру; нет такой — пустая.
    pub fn get(&self, i: usize) -> &str {
        let Some(&end) = self.ends.get(i) else {
            return "";
        };
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        &self.buf[start..end]
    }

    pub fn shrink(&mut self) {
        self.buf.shrink_to_fit();
        self.ends.shrink_to_fit();
    }
}

/// Буква столбца (`0` → `A`, `26` → `AA`).
pub(crate) fn col_name(mut c: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Номер столбца из адреса ячейки `B12` (с 0); строка адреса не проверяется.
pub(crate) fn col_of(addr: &str) -> Option<u32> {
    let mut n = 0u32;
    let mut any = false;
    for b in addr.bytes() {
        if !b.is_ascii_alphabetic() {
            break;
        }
        n = n
            .checked_mul(26)?
            .checked_add(u32::from(b.to_ascii_uppercase() - b'A') + 1)?;
        any = true;
    }
    any.then(|| n - 1)
}

/// Число в кратчайшей десятичной записи (`3.1400000000000001` → `3.14`).
pub(crate) fn number(v: f64, out: &mut String) {
    use std::fmt::Write;
    out.clear();
    if v.is_finite() {
        let _ = write!(out, "{v}");
    }
}

/// Дописывает текст ячейки в строку Markdown через `Ctx::append`: `|` → `\|`, переводы
/// строк → пробел.
fn put_escaped(ctx: &mut Ctx, dst: &mut String, s: &str, pending: &mut u64) {
    let mut rest = s;
    while let Some(i) = rest.find(['|', '\n', '\r']) {
        if i > 0 {
            ctx.append(dst, &rest[..i], pending);
        }
        let rep = if rest.as_bytes()[i] == b'|' {
            "\\|"
        } else {
            " "
        };
        ctx.append(dst, rep, pending);
        rest = &rest[i + 1..];
        if ctx.chars_full() {
            return;
        }
    }
    if !rest.is_empty() {
        ctx.append(dst, rest, pending);
    }
}

/// Приёмник единиц: блок строк листа под шапкой (строки Markdown `| a | b |`), шапка
/// повторяется в каждой единице; `Locator::Sheet` — диапазон строк данных блока.
#[derive(Default)]
struct Blocks {
    units: Units,
    name: String,
    header: String,
    header_row: u32,
    line: String,
    line_cols: u32,
    block: String,
    block_chars: usize,
    from: u32,
    to: u32,
    cols: u32,
    /// Строка листа, которая сейчас собирается.
    row: u32,
    /// Накоплено, но не выдано (шапка, блок, строка) — для `Ctx::append`.
    pending: u64,
}

impl Blocks {
    fn flush(&mut self, ctx: &mut Ctx) {
        if self.block.is_empty() {
            return;
        }
        let range = format!(
            "A{}:{}{}",
            self.from,
            col_name(self.cols.max(1) - 1),
            self.to
        );
        let text = std::mem::take(&mut self.block);
        self.pending = self.pending.saturating_sub(self.block_chars as u64);
        self.block_chars = 0;
        self.units.push(
            Locator::Sheet {
                name: self.name.clone(),
                range,
            },
            vec![self.name.clone()],
            text,
            ctx,
        );
    }

    /// Строка закончена: шапка (первая непустая строка) или строка блока.
    fn take_line(&mut self, row: u32, ctx: &mut Ctx) {
        let line = std::mem::take(&mut self.line);
        let cols = std::mem::take(&mut self.line_cols);
        if self.header_row == 0 {
            self.header = line;
            self.header_row = row;
            self.cols = cols;
            ctx.append(&mut self.header, "\n|", &mut self.pending);
            for _ in 0..cols {
                ctx.append(&mut self.header, " --- |", &mut self.pending);
            }
            return;
        }
        if self.block.is_empty() {
            let header = std::mem::take(&mut self.header);
            ctx.append(&mut self.block, &header, &mut self.pending);
            self.block_chars = self.block.chars().count();
            self.header = header;
            self.from = row;
            self.cols = self.cols.max(1);
        }
        let n = line.chars().count();
        ctx.append(&mut self.block, "\n", &mut self.pending);
        self.block.push_str(&line);
        self.block_chars += n + 1;
        self.to = row;
        self.cols = self.cols.max(cols);
        if self.block_chars >= SHEET_UNIT_CHARS {
            self.flush(ctx);
        }
    }

    /// Остаток `max_chars` исчерпан: собранное выдаётся, разбор останавливается.
    fn check_full(&mut self, row: u32, ctx: &mut Ctx) {
        if !ctx.chars_full() || ctx.stopped() {
            return;
        }
        if !self.line.is_empty() {
            self.take_line(row, ctx);
        }
        self.header_only();
        self.flush(ctx);
        ctx.stop("max_chars");
    }

    /// Лист из одной шапки — единица с шапкой.
    fn header_only(&mut self) {
        if self.header_row > 0 && self.to == 0 && self.block.is_empty() {
            self.block = std::mem::take(&mut self.header);
            self.block_chars = self.block.chars().count();
            self.from = self.header_row;
            self.to = self.header_row;
            self.cols = self.cols.max(1);
        }
    }
}

impl Sink for Blocks {
    fn sheet(&mut self, name: &str, _ctx: &mut Ctx) -> bool {
        self.name = name.to_string();
        self.header.clear();
        self.header_row = 0;
        self.line.clear();
        self.line_cols = 0;
        self.block.clear();
        self.block_chars = 0;
        self.from = 0;
        self.to = 0;
        self.cols = 0;
        self.row = 0;
        self.pending = 0;
        true
    }

    fn open(&mut self, row: u32, col: u32, ctx: &mut Ctx) {
        for _ in self.line_cols..col {
            ctx.append(&mut self.line, "|  ", &mut self.pending);
        }
        ctx.append(&mut self.line, "| ", &mut self.pending);
        self.line_cols = col + 1;
        self.row = row;
        self.check_full(row, ctx);
    }

    fn text(&mut self, s: &str, ctx: &mut Ctx) {
        put_escaped(ctx, &mut self.line, s, &mut self.pending);
        self.check_full(self.row, ctx);
    }

    fn close(&mut self, ctx: &mut Ctx) {
        ctx.append(&mut self.line, " ", &mut self.pending);
    }

    fn end_row(&mut self, row: u32, ctx: &mut Ctx) {
        if ctx.stopped() {
            return;
        }
        ctx.append(&mut self.line, "|", &mut self.pending);
        self.take_line(row, ctx);
        self.check_full(row, ctx);
    }

    fn end_sheet(&mut self, ctx: &mut Ctx) {
        self.header_only();
        self.flush(ctx);
        self.header.clear();
        self.pending = 0;
    }
}

/// Приёмник страницы превью.
struct Page {
    want: String,
    from: u32,
    count: u32,
    found: bool,
    active: bool,
    sheets: Vec<String>,
    sheet: String,
    header: Vec<String>,
    header_row: u32,
    rows: Vec<Vec<String>>,
    first_row: u32,
    next_row: u32,
    last_row: u32,
    cols: u32,
    /// Текущая строка: `Some` — собирается (шапка или строка окна).
    cur: Option<Vec<String>>,
    /// Ячеек страницы с пустыми в разрывах — в пределах `max_cells`.
    cells: u64,
    full: bool,
    pending: u64,
}

impl Page {
    fn new(want: &str, from: u32, count: u32) -> Self {
        Page {
            want: want.to_string(),
            from,
            count,
            found: false,
            active: false,
            sheets: Vec::new(),
            sheet: String::new(),
            header: Vec::new(),
            header_row: 0,
            rows: Vec::new(),
            first_row: 0,
            next_row: 0,
            last_row: 0,
            cols: 0,
            cur: None,
            cells: 0,
            full: false,
            pending: 0,
        }
    }

    /// Учёт ячейки страницы; `false` — предел исчерпан.
    fn take_cell(&mut self, ctx: &mut Ctx) -> bool {
        if self.full {
            return false;
        }
        self.cells += 1;
        if self.cells > ctx.limits.max_cells {
            ctx.truncated("max_cells");
            self.full = true;
            return false;
        }
        true
    }

    fn in_window(&self, row: u32) -> bool {
        self.header_row == 0
            || (row >= self.first_row
                && u64::from(row) < u64::from(self.first_row) + u64::from(self.count))
    }

    fn finish(self, warnings: Vec<String>) -> SheetPage {
        SheetPage {
            sheets: self.sheets,
            sheet: self.sheet,
            header: self.header,
            rows: self.rows,
            total_rows: self.last_row.saturating_sub(self.header_row),
            cols: self.cols,
            first_row: self.first_row,
            warnings,
        }
    }
}

impl Sink for Page {
    fn sheet(&mut self, name: &str, _ctx: &mut Ctx) -> bool {
        self.sheets.push(name.to_string());
        self.active = !self.found && (self.want.is_empty() || self.want == name);
        if self.active {
            self.found = true;
            self.sheet = name.to_string();
        }
        self.active
    }

    fn open(&mut self, row: u32, col: u32, ctx: &mut Ctx) {
        self.cols = self.cols.max(col + 1);
        if !self.in_window(row) || self.full {
            return;
        }
        let mut cur = self.cur.take().unwrap_or_default();
        while (cur.len() as u32) <= col {
            if !self.take_cell(ctx) {
                break;
            }
            cur.push(String::new());
        }
        self.cur = Some(cur);
    }

    fn text(&mut self, s: &str, ctx: &mut Ctx) {
        if let Some(cell) = self.cur.as_mut().and_then(|r| r.last_mut()) {
            ctx.append(cell, s, &mut self.pending);
        }
    }

    fn close(&mut self, _ctx: &mut Ctx) {}

    fn end_row(&mut self, row: u32, ctx: &mut Ctx) {
        self.last_row = row;
        let cur = self.cur.take();
        if self.header_row == 0 {
            self.header = cur.unwrap_or_default();
            self.header_row = row;
            self.first_row = self.from.max(row.saturating_add(1));
            self.next_row = self.first_row;
            return;
        }
        let Some(cur) = cur else {
            return;
        };
        while self.next_row < row {
            if !self.take_cell(ctx) {
                return;
            }
            self.rows.push(Vec::new());
            self.next_row += 1;
        }
        self.rows.push(cur);
        self.next_row = row.saturating_add(1);
    }

    fn end_sheet(&mut self, ctx: &mut Ctx) {
        self.active = false;
        // пустые строки окна перед следующей непустой за окном
        let window_end = u64::from(self.first_row) + u64::from(self.count);
        let end = (u64::from(self.last_row) + 1).min(window_end);
        while self.header_row > 0 && u64::from(self.next_row) < end {
            if !self.take_cell(ctx) {
                break;
            }
            self.rows.push(Vec::new());
            self.next_row += 1;
        }
    }
}
