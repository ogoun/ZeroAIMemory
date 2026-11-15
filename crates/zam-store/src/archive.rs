//! Формат архива экспорта пространства (M6 §12б.5): раскладка и отпечаток содержимого. Общий
//! для экспорта (`zam-server`, 18b1) и импорта ([`crate::import`], 18b2).

/// Версия формата архива экспорта.
pub const EXPORT_FORMAT: u32 = 1;
/// Опись архива.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Каталог вложений и файлов отчётов.
pub const ATTACHMENTS_DIR: &str = "_attachments";
/// Каталог записей журналов.
pub const JOURNALS_DIR: &str = "_journals";
/// Каталог истории.
pub const HISTORY_DIR: &str = "_history";
/// Явные связи.
pub const LINKS_FILE: &str = "_links.jsonl";
/// Папки.
pub const FOLDERS_FILE: &str = "_folders.jsonl";

/// Отпечаток содержимого архива: blake3 по файлам без описи в порядке записи — на файл длина
/// имени (u64 LE), имя, длина содержимого (u64 LE) и blake3 содержимого (32 байта).
pub struct Fingerprint {
    outer: blake3::Hasher,
    cur: Option<(String, u64, blake3::Hasher)>,
}

impl Default for Fingerprint {
    fn default() -> Self {
        Fingerprint {
            outer: blake3::Hasher::new(),
            cur: None,
        }
    }
}

impl Fingerprint {
    /// Начало файла `name`; затем [`Fingerprint::update`] содержимым и [`Fingerprint::end`].
    pub fn begin(&mut self, name: &str) {
        self.end();
        self.cur = Some((name.to_string(), 0, blake3::Hasher::new()));
    }

    pub fn update(&mut self, data: &[u8]) {
        if let Some((_, len, h)) = &mut self.cur {
            *len += data.len() as u64;
            h.update(data);
        }
    }

    /// Конец текущего файла (нет файла — ничего).
    pub fn end(&mut self) {
        if let Some((name, len, h)) = self.cur.take() {
            self.outer.update(&(name.len() as u64).to_le_bytes());
            self.outer.update(name.as_bytes());
            self.outer.update(&len.to_le_bytes());
            self.outer.update(h.finalize().as_bytes());
        }
    }

    /// Файл целиком.
    pub fn file(&mut self, name: &str, data: &[u8]) {
        self.begin(name);
        self.update(data);
        self.end();
    }

    /// Отпечаток (текущий файл завершается).
    pub fn hex(&mut self) -> String {
        self.end();
        self.outer.finalize().to_hex().to_string()
    }
}
