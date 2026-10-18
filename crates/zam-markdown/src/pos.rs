//! Перевод позиций comrak (строка и колонка в байтах, с 1) в байтовые оффсеты.

/// Таблица начал строк. Концы строк — `\n`, `\r\n` и одиночный `\r`,
/// как в CommonMark и comrak. BOM входит в первую строку.
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Строит таблицу для `src`.
    pub fn new(src: &str) -> Self {
        let b = src.as_bytes();
        let mut starts = vec![0];
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\n' => starts.push(i + 1),
                b'\r' if b.get(i + 1) == Some(&b'\n') => {
                    i += 1;
                    starts.push(i + 1);
                }
                b'\r' => starts.push(i + 1),
                _ => {}
            }
            i += 1;
        }
        Self {
            starts,
            len: src.len(),
        }
    }

    /// Число строк (пустая строка после последнего перевода тоже считается).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Оффсет начала строки `line` (с 1); за пределами — длина текста.
    pub fn line_start(&self, line: usize) -> usize {
        let line = line.max(1);
        self.starts.get(line - 1).copied().unwrap_or(self.len)
    }

    /// Оффсет начала строки, следующей за `line`, или длина текста.
    pub fn line_end(&self, line: usize) -> usize {
        self.line_start(line.max(1) + 1)
    }

    /// Оффсет позиции `line:col_byte` (обе с 1, колонка — в байтах),
    /// ограниченный длиной текста.
    pub fn offset(&self, line: usize, col_byte: usize) -> usize {
        let line = line.max(1);
        if line > self.starts.len() {
            return self.len;
        }
        (self.starts[line - 1] + col_byte.max(1) - 1).min(self.len)
    }

    /// Номер строки (с 1), в которой лежит байт `offset`.
    pub fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|&s| s <= offset).max(1)
    }
}
