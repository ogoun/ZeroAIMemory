//! Привязки баз каталога данных к старой машине (M6 §12б.1, §17 п. 24; задача 18c): ни одна
//! ячейка `system.db` и `users/*.db`, кроме атрибуции `machine`, не содержит путей старой
//! машины и её имени.
//!
//! **Один исходник на два потребителя** — чтобы проверки не расходились:
//! `cargo xtask transfer-check bindings` (`mod bindings` в `xtask/src/transfer_check.rs`) и тест
//! `restore_has_no_machine_bindings` (`crates/zam-server/tests/backup.rs`,
//! `#[path = "../../../xtask/src/transfer_check/bindings.rs"]`). Поэтому зависимости — только
//! общие для обоих: `std`, `anyhow`, `rusqlite`, `serde_json`, `zstd`. Юнит-тесты модуля — в
//! `xtask/src/transfer_check.rs` (иначе они шли бы дважды).
//!
//! **Сравнение.** И ячейка, и игла приводятся к нижнему регистру (пути Windows и имена машин
//! регистр не различают). Путь ищется в *нормальной форме* текста: любая серия `\` и `/` — один
//! `/`. Так одна игла покрывает запись пути как есть (`H:\zam\data`), через `/`, с удвоенным
//! `\\` (экранирование JSON), с учетверённым `\\\\` (JSON в JSON) и со смешанными слэшами
//! (`H:\zam/data`). Второй проход — по тексту с раскрытым процентным кодированием: `file://`
//! с `%20`, `%5C`, кириллицей в UTF-8 (`%D0%B4…`). После совпадения пути не должно идти
//! продолжение имени (буква, цифра, `_`, `-`): `…/data2` — другой каталог.
//!
//! **Имя машины** ищется по границам слова (соседи — не буква, не цифра, не `_` и не `-`);
//! имя короче [`MIN_MACHINE`] символов не принимается: `pc` совпало бы со словами текста.
//!
//! **Атрибуция — не привязка** (§12б.1): столбцы `machine` и поля `machine` JSON-ячеек (на
//! любой глубине, в том числе в JSON, вложенном строкой) пропускаются.
//!
//! **BLOB.** Ячейка-BLOB с магией zstd распаковывается (`blob_extract.units` — zstd(JSON)),
//! BLOB из корректного UTF-8 читается как текст; остальные (векторы, индексы FTS) — двоичные,
//! пропускаются. Виртуальные таблицы пропускаются: их данные — в теневых таблицах.
//!
//! **Сырой текст** (ячейка — не JSON-объект и не массив: обрывок JSON, JSONL, журнал)
//! проверяется в нескольких видах: как есть; с раскрытыми `\uXXXX` (`\u005c` — это `\`);
//! каждый — ещё и с раскрытым процентным кодированием, до трёх проходов (`%2520` → `%20` →
//! пробел). Литеральные `\n`, `\t`, `\r` перед именем машины считаются границей слова
//! (`line1\nwin-box started`). Цена — редкий ложный красный на каталоге вида `…\nwin-box`;
//! пропуск привязки хуже.
//!
//! **Имя-слово.** Имя машины, совпадающее с обычным словом (`dev`, `test`, `server`), найдётся
//! в тексте заметок («the dev server») — это ложный красный, а не привязка: проверяйте
//! окрестность находки.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::types::ValueRef;
use serde_json::Value;

/// Наименьшая длина имени машины, символов.
pub const MIN_MACHINE: usize = 3;
/// Наименьшая длина имени машины, которое добавляется само (`add_machine_if_distinct`).
const MIN_DISTINCT_MACHINE: usize = 4;
/// Наименьшая длина пути в нормальной форме, символов (`c:/zam`).
const MIN_PATH: usize = 6;
/// Символов текста до и после совпадения в сообщении о находке.
const CONTEXT: usize = 60;
/// Предел вложенности JSON в строках JSON.
const JSON_DEPTH: usize = 8;
/// Предел распакованного BLOB zstd, байт.
const ZSTD_MAX: usize = 256 << 20;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
/// Проходов раскрытия процентного кодирования сырого текста (`%2520` → `%20` → пробел).
const PERCENT_PASSES: usize = 3;

/// Нормальная форма для поиска пути: нижний регистр, серия `\` и `/` — один `/`.
pub fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut slash = false;
    for c in s.chars() {
        if c == '\\' || c == '/' {
            if !slash {
                out.push('/');
            }
            slash = true;
        } else {
            slash = false;
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Раскрывает процентное кодирование (`%20`, `%D0%B4`); неполные и нешестнадцатеричные
/// последовательности остаются как есть, не-UTF-8 — с заменой.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |x: u8| (x as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Путь старой машины в нормальной форме: без префикса `\\?\` и хвостовых слэшей. Ошибка —
/// пустой или слишком общий путь (корень диска, `/var`): он совпал бы с чем угодно.
pub fn path_needle(p: &str) -> Result<String> {
    let t = p.trim();
    let t = t.strip_prefix(r"\\?\").unwrap_or(t);
    let n = norm(t);
    let n = n.trim_end_matches('/').to_string();
    if n.is_empty() {
        bail!("пустой путь старой машины");
    }
    let parts = n.split('/').filter(|s| !s.is_empty()).count();
    if parts < 2 || n.chars().count() < MIN_PATH {
        bail!(
            "путь «{p}» слишком общий (нужно не меньше двух частей и {MIN_PATH} символов): \
             он совпал бы с посторонним текстом"
        );
    }
    Ok(n)
}

/// Имя старой машины в нижнем регистре. Ошибка — пустое или короче [`MIN_MACHINE`] символов.
pub fn machine_needle(m: &str) -> Result<String> {
    let n = m.trim().to_lowercase();
    if n.chars().count() < MIN_MACHINE {
        bail!(
            "имя машины «{m}» короче {MIN_MACHINE} символов: совпадения со словами текста \
             неотличимы от привязки — проверку по такому имени сделать нельзя"
        );
    }
    Ok(n)
}

/// Иглы: пути (нормальная форма) и имена машин (нижний регистр).
#[derive(Debug, Clone, Default)]
pub struct Needles {
    paths: Vec<String>,
    machines: Vec<String>,
}

impl Needles {
    /// Иглы из путей и имён старой машины; хотя бы один путь обязателен. Ошибки — как у
    /// [`path_needle`] и [`machine_needle`].
    pub fn new<P: AsRef<str>, M: AsRef<str>>(paths: &[P], machines: &[M]) -> Result<Self> {
        if paths.is_empty() {
            bail!("не задан ни один путь старой машины");
        }
        let mut n = Self::default();
        for p in paths {
            n.paths.push(path_needle(p.as_ref())?);
        }
        for m in machines {
            n.machines.push(machine_needle(m.as_ref())?);
        }
        n.dedup();
        Ok(n)
    }

    /// Добавляет путь, если он не слишком общий; `false` — пропущен.
    #[allow(dead_code)] // только тест `restore_has_no_machine_bindings` и юнит-тесты xtask
    pub fn add_path_if_specific(&mut self, p: &str) -> bool {
        let Ok(n) = path_needle(p) else {
            return false;
        };
        self.paths.push(n);
        self.dedup();
        true
    }

    /// Добавляет имя машины, если оно приметное: не короче [`MIN_DISTINCT_MACHINE`] символов и
    /// не из одних букв (`fv-az1774-221`, `ogoun-pc`); `false` — пропущено. Имя-слово (`dev`,
    /// `runner`) совпало бы с текстом корпуса и дало бы ложный красный — так тест добавляет имя
    /// машины, на которой идёт (её не выбирают).
    #[allow(dead_code)] // только тест `restore_has_no_machine_bindings` и юнит-тесты xtask
    pub fn add_machine_if_distinct(&mut self, m: &str) -> bool {
        let Ok(n) = machine_needle(m) else {
            return false;
        };
        if n.chars().count() < MIN_DISTINCT_MACHINE || n.chars().all(char::is_alphabetic) {
            return false;
        }
        self.machines.push(n);
        self.dedup();
        true
    }

    fn dedup(&mut self) {
        self.paths.sort();
        self.paths.dedup();
        self.machines.sort();
        self.machines.dedup();
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    #[allow(dead_code)] // только xtask (вывод команды) и его юнит-тесты
    pub fn machines(&self) -> &[String] {
        &self.machines
    }

    /// Первое совпадение в сыром тексте `text` (без разбора JSON): игла и окрестность.
    pub fn find_raw(&self, text: &str) -> Option<(String, String)> {
        // виды текста: как есть, с раскрытыми `\uXXXX`; каждый — с раскрытым процентным
        // кодированием, до PERCENT_PASSES проходов (двойное кодирование `%2520`)
        let lower = text.to_lowercase();
        let mut views = vec![lower];
        if views[0].contains("\\u") {
            let u = unescape_unicode(&views[0]).to_lowercase();
            if u != views[0] {
                views.push(u);
            }
        }
        for k in 0..views.len() {
            let mut cur = views[k].clone();
            for _ in 0..PERCENT_PASSES {
                if !cur.contains('%') {
                    break;
                }
                let next = percent_decode(&cur).to_lowercase();
                if next == cur {
                    break;
                }
                views.push(next.clone());
                cur = next;
            }
        }
        for v in &views {
            for m in &self.machines {
                if let Some(i) = find_word(v, m) {
                    return Some((m.clone(), around(v, i, m.len())));
                }
            }
        }
        for v in &views {
            let f = norm(v);
            for p in &self.paths {
                if let Some(i) = find_path(&f, p) {
                    return Some((p.clone(), around(&f, i, p.len())));
                }
            }
        }
        None
    }

    /// Первое совпадение в ячейке: JSON-объект или массив разбирается, поля `machine`
    /// пропускаются, проверяются ключи и строки (строка с вложенным JSON — так же); прочее —
    /// сырой текст.
    pub fn find(&self, text: &str) -> Option<(String, String)> {
        self.find_depth(text, 0)
    }

    fn find_depth(&self, text: &str, depth: usize) -> Option<(String, String)> {
        let t = text.trim_start();
        if (t.starts_with('{') || t.starts_with('['))
            && depth < JSON_DEPTH
            && let Ok(v) = serde_json::from_str::<Value>(text)
        {
            return self.find_json(&v, depth);
        }
        self.find_raw(text)
    }

    fn find_json(&self, v: &Value, depth: usize) -> Option<(String, String)> {
        match v {
            Value::Object(m) => m
                .iter()
                .filter(|(k, _)| k.as_str() != "machine")
                .find_map(|(k, x)| self.find_raw(k).or_else(|| self.find_json(x, depth))),
            Value::Array(a) => a.iter().find_map(|x| self.find_json(x, depth)),
            Value::String(s) => self.find_depth(s, depth + 1),
            _ => None,
        }
    }
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Текст кончается литеральным экранированием `\n`, `\t` или `\r` (обратная косая и буква):
/// в неразобранном JSON и журналах это перевод строки или табуляция, то есть граница слова.
fn after_literal_escape(before: &str) -> bool {
    let mut it = before.chars().rev();
    matches!(it.next(), Some('n' | 't' | 'r')) && it.next() == Some('\\')
}

/// Раскрывает `\uXXXX` (в том числе суррогатные пары — с заменой); прочее остаётся как есть.
pub fn unescape_unicode(s: &str) -> String {
    let b = s.as_bytes();
    let mut units: Vec<u16> = Vec::new();
    let mut out = String::with_capacity(s.len());
    let flush = |units: &mut Vec<u16>, out: &mut String| {
        out.push_str(&String::from_utf16_lossy(units));
        units.clear();
    };
    let mut i = 0;
    while i < b.len() {
        let code = (b[i] == b'\\' && b.get(i + 1) == Some(&b'u'))
            .then(|| s.get(i + 2..i + 6))
            .flatten()
            .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
            .and_then(|h| u16::from_str_radix(h, 16).ok());
        if let Some(c) = code {
            units.push(c);
            i += 6;
            continue;
        }
        flush(&mut units, &mut out);
        let ch = s[i..].chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    flush(&mut units, &mut out);
    out
}

/// Позиция `needle` в `hay` по границам слова; литеральные `\n`, `\t`, `\r` перед именем —
/// граница.
fn find_word(hay: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let at = from + i;
        let end = at + needle.len();
        let before = hay[..at].chars().next_back().is_some_and(word_char)
            && !after_literal_escape(&hay[..at]);
        let after = hay[end..].chars().next().is_some_and(word_char);
        if !before && !after {
            return Some(at);
        }
        from = at + hay[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Позиция пути `needle` в нормальной форме `hay`: после совпадения нет продолжения имени.
fn find_path(hay: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let at = from + i;
        if !hay[at + needle.len()..]
            .chars()
            .next()
            .is_some_and(word_char)
        {
            return Some(at);
        }
        from = at + hay[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Окрестность совпадения `[at, at + len)` в `s`: до [`CONTEXT`] символов с каждой стороны,
/// переводы строк — пробелы.
fn around(s: &str, at: usize, len: usize) -> String {
    let start = s[..at]
        .char_indices()
        .rev()
        .nth(CONTEXT - 1)
        .map_or(0, |(i, _)| i);
    let tail = &s[at + len..];
    let end = at
        + len
        + tail
            .char_indices()
            .nth(CONTEXT)
            .map_or(tail.len(), |(i, _)| i);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(
        s[start..end]
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c }),
    );
    if end < s.len() {
        out.push('…');
    }
    out
}

/// Итог обхода баз.
#[derive(Debug, Default)]
pub struct Scan {
    /// Проверено баз.
    pub dbs: usize,
    /// Проверено текстовых ячеек.
    pub text_cells: u64,
    /// Проверено BLOB-ячеек с текстом: распакованных zstd и корректного UTF-8.
    pub blob_cells: u64,
    /// Находки: «файл таблица.столбец: «игла» в …окрестность…».
    pub hits: Vec<String>,
}

/// `system.db` и `users/*.db` каталога данных `data_dir`.
pub fn databases(data_dir: &Path) -> Result<Vec<PathBuf>> {
    let users = data_dir.join("users");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&users)
        .with_context(|| users.display().to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .collect();
    v.sort();
    v.insert(0, data_dir.join("system.db"));
    Ok(v)
}

/// Текст BLOB-ячейки: распакованный zstd или сам BLOB, если это UTF-8; двоичный — `None`.
fn blob_text(b: &[u8]) -> Option<String> {
    if b.starts_with(&ZSTD_MAGIC) {
        let mut out = Vec::new();
        let mut dec = zstd::stream::read::Decoder::new(b).ok()?;
        std::io::Read::read_to_end(
            &mut std::io::Read::take(&mut dec, ZSTD_MAX as u64),
            &mut out,
        )
        .ok()?;
        return Some(String::from_utf8_lossy(&out).into_owned());
    }
    if b.is_empty() {
        return None;
    }
    std::str::from_utf8(b).ok().map(str::to_string)
}

/// Обходит ячейки баз каталога `data_dir` (см. описание модуля) и возвращает находки `needles`.
pub fn scan(data_dir: &Path, needles: &Needles) -> Result<Scan> {
    let mut out = Scan::default();
    for db in databases(data_dir)? {
        let file = db
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let c =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| db.display().to_string())?;
        out.dbs += 1;
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND coalesce(sql, '') NOT LIKE 'CREATE VIRTUAL%'",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for t in tables {
            let qt = t.replace('"', "\"\"");
            let cols: Vec<String> = c
                .prepare("SELECT name FROM pragma_table_info(?1)")?
                .query_map([&t], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for col in cols.iter().filter(|c| c.as_str() != "machine") {
                let qc = col.replace('"', "\"\"");
                let mut st = c.prepare(&format!(
                    "SELECT \"{qc}\" FROM \"{qt}\" WHERE typeof(\"{qc}\") IN ('text', 'blob')"
                ))?;
                let mut rows = st.query([])?;
                while let Some(r) = rows.next()? {
                    let text = match r.get_ref(0)? {
                        ValueRef::Text(b) => {
                            out.text_cells += 1;
                            String::from_utf8_lossy(b).into_owned()
                        }
                        ValueRef::Blob(b) => match blob_text(b) {
                            Some(s) => {
                                out.blob_cells += 1;
                                s
                            }
                            None => continue,
                        },
                        _ => continue,
                    };
                    if let Some((needle, ctx)) = needles.find(&text) {
                        out.hits
                            .push(format!("{file} {t}.{col}: «{needle}» в «{ctx}»"));
                    }
                }
            }
        }
    }
    Ok(out)
}
