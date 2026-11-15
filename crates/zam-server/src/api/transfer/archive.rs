//! Формат архива экспорта пространства (M6 §12б.5): опись, имена файлов для Windows, отпечаток
//! содержимого, frontmatter. Общий для экспорта (18b1) и импорта (18b2).
//!
//! **Раскладка:**
//! - `manifest.json` — [`ExportManifest`], последним файлом архива;
//! - `<file_name_for(path)>.md` — заметки; `_folders.jsonl` — папки (`{id, path, l0, l0_source,
//!   created_at}`: пустые папки и описания папок тоже переносятся);
//! - `_attachments/<sha256[0..16]>/<имя>` — вложения заметок и файлы отчётов, один файл на пару
//!   (блоб, имя) — как ссылка в теле ([`zam_store::attachment_link`]), но с именем без
//!   percent-кодирования ссылки Markdown (распакованный архив открывает ссылку тела по пути);
//! - `_journals/<kind>/<id>.md` — записи журналов;
//! - `_links.jsonl` — явные связи `{src, type, dst, dst_space, anchor?}`;
//! - `_history/<id>.jsonl` — ревизии заметки или записи, по строке на ревизию.
//!
//! Имена служебных файлов и каталогов начинаются с `_`; заметка, путь которой начинается с `_`,
//! получает первый символ `%5F` ([`file_name_for`]) и в служебные имена не попадает.
//!
//! **Frontmatter** ([`front_matter`]) — YAML, где каждое значение записано JSON (JSON — часть
//! YAML 1.2): `key: <json>` по строке на поле, переводы строк внутри значений экранированы,
//! поэтому строка `---` внутри блока невозможна. Тело после блока — байт в байт как в базе
//! (у заметки — вместе с её собственным frontmatter, если он есть).
//!
//! **Отпечаток** ([`Fingerprint`]) — blake3 по всем файлам архива, кроме описи, в порядке
//! записи: для каждого файла длина имени (u64 LE), имя UTF-8, длина содержимого (u64 LE) и
//! blake3 содержимого (32 байта). Хэш содержимого считается потоком — длина заранее не нужна
//! (история пишется по ревизии).
//!
//! **Имена и ссылки тела** (передать в документацию, 13c). Ссылка тела заметки —
//! `_attachments/<sha16>/<имя>` с percent-кодированием Markdown ([`zam_store::attachment_link`]),
//! файл в архиве — то же имя после декодирования, кроме случаев, когда имя меняется для
//! Windows: запрещённые символы (`%XX`), имя устройства (`_`), конечные точка и пробел,
//! совпадение без учёта регистра или формы Unicode (`~N`), длинный сегмент (`~h<8 hex>`). Тогда
//! ссылка тела в распакованном архиве не открывается; верное соответствие — поле
//! `attachments[].file` frontmatter заметки (`files[].file` у записи), импорт берёт его оттуда;
//! там же `attachments[].doc` — id документа вложения (импорт переписывает ссылки `d:`, 18b2).
//! Ссылки тела ведут от корня архива: из вложенной заметки обычный просмотрщик Markdown ищет
//! `_attachments/` рядом с ней и не находит.
//!
//! **Длина пути.** Сегмент после кодирования длиннее [`SEGMENT_MAX`] байт укорачивается с
//! суффиксом `~h<8 hex>` (blake3 исходного сегмента; расширение до 16 байт сохраняется). Весь
//! путь не укорачивается: путь заметки — до 1024 байт, а предел Windows в 260 символов зависит
//! от каталога распаковки. Распаковка глубоких деревьев на Windows требует длинных путей
//! (`LongPathsEnabled`) или короткого каталога назначения **[решение исполнителя 18b1]**.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use zam_core::Id;
use zam_store::EntryKind;

pub use crate::backup::MANIFEST_FILE;

// Раскладка и отпечаток — общие с импортом (`zam_store::archive`): одно место правды.
pub use zam_store::archive::{
    ATTACHMENTS_DIR, EXPORT_FORMAT, FOLDERS_FILE, Fingerprint, HISTORY_DIR, JOURNALS_DIR,
    LINKS_FILE,
};
/// Длина префикса sha256 в пути вложения.
pub const SHA_PREFIX: usize = 16;
/// Предел сегмента имени после кодирования, байт (предел ФС — 255; запас на `~N` и `.md`).
pub const SEGMENT_MAX: usize = 200;
/// Длиннее — не расширение (при укорачивании сегмента).
const EXT_MAX: usize = 16;

/// Фильтр экспорта. Пустые `paths` и `kinds` вместе — всё пространство; задан один —
/// только его часть (`paths` — заметки и папки под префиксами, без записей журналов; `kinds` —
/// записи этих видов, без заметок); заданы оба — обе части.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFilter {
    pub paths: Vec<String>,
    pub kinds: Vec<EntryKind>,
    pub history: bool,
}

/// Опись архива (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub format: u32,
    pub server_version: String,
    /// Владелец базы, из которой снят архив.
    pub source_user_id: Id,
    pub space: Id,
    /// `rules` | `human` | `knowledge` | `solution`.
    pub space_kind: String,
    pub space_name: String,
    /// GUID решения (у пространства решения).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_guid: Option<String>,
    /// UTC, RFC 3339.
    pub created_at: String,
    pub filter: ExportFilter,
    /// Число файлов архива без описи.
    pub files: usize,
    /// [`Fingerprint`] содержимого без описи, hex.
    pub archive_fingerprint: String,
    /// Блобы, файлов которых нет на диске сервера (`<sha256>/<имя>`): в архив не вошли.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_blobs: Vec<String>,
}

/// Символы, которые Windows не допускает в имени файла (и `%` — начало кода, `\` — разделитель).
const FORBIDDEN: [char; 10] = [':', '?', '*', '|', '<', '>', '"', '%', '\\', '/'];

/// Зарезервированные имена устройств Windows (без учёта регистра и расширения).
const RESERVED: [&str; 32] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³",
];

fn pct(out: &mut String, ch: char) {
    let mut buf = [0u8; 4];
    for b in ch.encode_utf8(&mut buf).bytes() {
        out.push_str(&format!("%{b:02X}"));
    }
}

/// Один сегмент имени для Windows: запрещённые символы и управляющие — `%XX`, конечные точки
/// и пробелы — `%2E`/`%20`, зарезервированное имя устройства — суффикс `_` у части до первой
/// точки (`CON` → `CON_`, `con.txt` → `con_.txt`), длиннее [`SEGMENT_MAX`] — укорачивание.
pub fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for ch in seg.chars() {
        if FORBIDDEN.contains(&ch) || ch.is_control() {
            pct(&mut out, ch);
        } else {
            out.push(ch);
        }
    }
    // конечные «.» и пробелы Windows отбрасывает молча
    let trimmed = out.trim_end_matches(['.', ' ']).len();
    if trimmed < out.len() {
        let tail: String = out[trimmed..].to_string();
        out.truncate(trimmed);
        for ch in tail.chars() {
            pct(&mut out, ch);
        }
    }
    let base_end = out.find('.').unwrap_or(out.len());
    let base = out[..base_end].trim_end_matches(' ');
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(base)) {
        out.insert(base_end, '_');
    }
    shorten(out, seg)
}

/// Сегмент длиннее [`SEGMENT_MAX`] — `<начало>~h<8 hex blake3 исходного><.расширение>`;
/// разрез — по границе символа и не посреди `%XX`.
fn shorten(enc: String, original: &str) -> String {
    if enc.len() <= SEGMENT_MAX {
        return enc;
    }
    let tag = format!("~h{}", &blake3::hash(original.as_bytes()).to_hex()[..8]);
    let ext = match enc.rfind('.') {
        Some(i) if i > 0 && enc.len() - i <= EXT_MAX => &enc[i..],
        _ => "",
    };
    let mut cut = SEGMENT_MAX - tag.len() - ext.len();
    while !enc.is_char_boundary(cut) {
        cut -= 1;
    }
    if let Some(i) = enc[..cut].rfind('%')
        && cut - i < 3
    {
        cut = i;
    }
    format!("{}{tag}{ext}", &enc[..cut])
}

/// Ключ занятого имени: регистр и форма Unicode не различаются (NTFS и APFS — без учёта
/// регистра, APFS и HFS+ сравнивают имена после нормализации).
fn key(s: &str) -> String {
    s.nfc().collect::<String>().to_lowercase()
}

/// Свободное имя `<stem>[~N]<ext>`: файл `<stem><ext>` или каталог с тем же именем уже есть
/// (без учёта регистра) — `~2`, `~3`, … перед `ext`. Имя и каталоги-предки занимаются.
fn claim(stem: &str, ext: &str, taken: &mut HashSet<String>) -> String {
    let mut n = 1u32;
    let name = loop {
        let cand = if n == 1 {
            format!("{stem}{ext}")
        } else {
            format!("{stem}~{n}{ext}")
        };
        let k = key(&cand);
        if !taken.contains(&k) && !taken.contains(&format!("{k}/")) {
            break cand;
        }
        n += 1;
    };
    for (i, _) in name.match_indices('/') {
        taken.insert(format!("{}/", key(&name[..i])));
    }
    taken.insert(key(&name));
    name
}

/// Имя файла заметки `path` в архиве без расширения (файл — `<имя>.md`): сегменты —
/// [`encode_segment`], первый символ `_` первого сегмента — `%5F` (служебные имена архива);
/// второй и следующие пути, совпадающие с занятыми без учёта регистра и формы Unicode
/// (`Readme`/`README`) или с занятым каталогом, получают `~2`, `~3`, … Исходный путь пишется во
/// frontmatter (`path`). `taken` — занятые имена архива (ключи [`key`]: файлы и каталоги
/// `<путь>/`); порядок вызовов решает, кто из пары «второй». Каталог, совпавший с ранее занятым
/// файлом, не переименовывается — поэтому экспорт зовёт функцию от глубоких путей к мелким.
pub fn file_name_for(path: &str, taken: &mut HashSet<String>) -> String {
    let mut segs: Vec<String> = path.split('/').map(encode_segment).collect();
    if let Some(first) = segs.first_mut()
        && first.starts_with('_')
    {
        first.replace_range(..1, "%5F");
    }
    let stem = segs.join("/");
    let name = claim(&stem, ".md", taken);
    name.strip_suffix(".md").unwrap_or(&name).to_string()
}

/// Путь файла вложения в архиве: `_attachments/<sha16>/<имя>`; имя — по сегментам
/// [`encode_segment`] (имя с `/` запись не пропускает — `check_filename`; если такое всё же есть
/// в базе, пустые сегменты отбрасываются: `a//b`, `/x`), совпадение без учёта регистра — `~N`
/// перед расширением.
pub fn attachment_file(sha256: &str, filename: &str, taken: &mut HashSet<String>) -> String {
    let sha16 = &sha256[..SHA_PREFIX.min(sha256.len())];
    let segs: Vec<String> = filename
        .split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect();
    let enc = if segs.is_empty() {
        "%5F".to_string()
    } else {
        segs.join("/")
    };
    let last = enc.rfind('/').map_or(0, |i| i + 1);
    let (stem, ext) = match enc[last..].rfind('.') {
        Some(i) if i > 0 => enc.split_at(last + i),
        _ => (enc.as_str(), ""),
    };
    claim(&format!("{ATTACHMENTS_DIR}/{sha16}/{stem}"), ext, taken)
}

/// Блок frontmatter: `---`, строки `key: <json>`, `---`.
pub fn front_matter(fields: &[(&str, serde_json::Value)]) -> String {
    let mut s = String::from("---\n");
    for (k, v) in fields {
        s.push_str(k);
        s.push_str(": ");
        s.push_str(&v.to_string());
        s.push('\n');
    }
    s.push_str("---\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_rules() {
        assert_eq!(encode_segment("a:b"), "a%3Ab");
        assert_eq!(encode_segment("q?*|<>\""), "q%3F%2A%7C%3C%3E%22");
        assert_eq!(encode_segment("100%"), "100%25");
        assert_eq!(encode_segment("CON"), "CON_");
        assert_eq!(encode_segment("con.txt"), "con_.txt");
        assert_eq!(encode_segment("Lpt9"), "Lpt9_");
        assert_eq!(encode_segment("CONSOLE"), "CONSOLE");
        assert_eq!(encode_segment("x."), "x%2E");
        assert_eq!(encode_segment("x. "), "x%2E%20");
        assert_eq!(encode_segment("обычное имя"), "обычное имя");
        assert_eq!(encode_segment("COM0"), "COM0_");
        assert_eq!(encode_segment("lpt0.log"), "lpt0_.log");
        assert_eq!(encode_segment("CONIN$"), "CONIN$_");
        assert_eq!(encode_segment("conout$.txt"), "conout$_.txt");
        assert_eq!(encode_segment("COM10"), "COM10");
    }

    #[test]
    fn long_segment_shortened_with_hash() {
        let long = format!("{}.png", ":".repeat(100));
        let e = encode_segment(&long);
        assert!(e.len() <= SEGMENT_MAX, "{}", e.len());
        assert!(e.ends_with(".png"), "{e}");
        // разрез не посреди %XX
        let head = e.split("~h").next().unwrap();
        assert!(head.ends_with("%3A"), "{head}");
        // разные исходные сегменты с общим началом — разные имена
        let other = format!("{}x.png", ":".repeat(100));
        assert_ne!(encode_segment(&other), e);
        let cyr = encode_segment(&"я".repeat(150));
        assert!(cyr.len() <= SEGMENT_MAX && cyr.contains("~h"), "{cyr}");
        assert_eq!(encode_segment(&"a".repeat(200)), "a".repeat(200));
        // файл заметки и каталог с длинным сегментом — в пределе 255 байт с `.md` и `~N`
        let mut t = HashSet::new();
        let n = file_name_for(&format!("{}/x", "я".repeat(200)), &mut t);
        assert!(n.split('/').all(|s| s.len() <= SEGMENT_MAX), "{n}");
    }

    #[test]
    fn names_case_pairs_and_service_prefix() {
        let mut t = HashSet::new();
        assert_eq!(file_name_for("Readme", &mut t), "Readme");
        assert_eq!(file_name_for("README", &mut t), "README~2");
        assert_eq!(file_name_for("readme", &mut t), "readme~3");
        assert_eq!(file_name_for("_attachments/x", &mut t), "%5Fattachments/x");
        assert_eq!(file_name_for("a/CON/b.", &mut t), "a/CON_/b%2E");
        // файл «x.md» и каталог «x.md/»: глубокий путь первым — файл получает ~2
        assert_eq!(file_name_for("x.md/y", &mut t), "x.md/y");
        assert_eq!(file_name_for("x", &mut t), "x~2");
    }

    #[test]
    fn nfc_and_nfd_collide() {
        let mut t = HashSet::new();
        let nfc = "\u{e9}t\u{e9}";
        let nfd = "e\u{301}te\u{301}";
        assert_eq!(file_name_for(nfc, &mut t), nfc);
        assert_eq!(file_name_for(nfd, &mut t), format!("{nfd}~2"));
    }

    #[test]
    fn attachment_names() {
        let sha = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut t = HashSet::new();
        assert_eq!(
            attachment_file(sha, "a b.png", &mut t),
            "_attachments/0123456789abcdef/a b.png"
        );
        assert_eq!(
            attachment_file(sha, "A B.png", &mut t),
            "_attachments/0123456789abcdef/A B~2.png"
        );
        assert_eq!(
            attachment_file(sha, "figs/loss:1.png", &mut t),
            "_attachments/0123456789abcdef/figs/loss%3A1.png"
        );
        assert_eq!(
            attachment_file(sha, ".env", &mut t),
            "_attachments/0123456789abcdef/.env"
        );
        // пустые сегменты (имя из базы до проверки `check_filename`) схлопываются
        assert_eq!(
            attachment_file(sha, "c//d.txt", &mut t),
            "_attachments/0123456789abcdef/c/d.txt"
        );
        assert_eq!(
            attachment_file(sha, "/e.txt", &mut t),
            "_attachments/0123456789abcdef/e.txt"
        );
        assert_eq!(
            attachment_file(sha, "/", &mut t),
            "_attachments/0123456789abcdef/%5F"
        );
    }

    /// Опись экспорта и импорта — один файл.
    #[test]
    fn manifest_name_shared_with_import() {
        assert_eq!(MANIFEST_FILE, zam_store::archive::MANIFEST_FILE);
    }

    #[test]
    fn front_matter_is_one_line_per_field() {
        let s = front_matter(&[("path", "a\n---\nb".into()), ("rev", 3.into())]);
        assert_eq!(s, "---\npath: \"a\\n---\\nb\"\nrev: 3\n---\n");
    }

    #[test]
    fn fingerprint_order_and_boundaries() {
        let fp = |files: &[(&str, &[u8])]| {
            let mut f = Fingerprint::default();
            for (n, d) in files {
                f.file(n, d);
            }
            f.hex()
        };
        assert_ne!(fp(&[("a", b"bc")]), fp(&[("ab", b"c")]));
        assert_ne!(
            fp(&[("a", b"1"), ("b", b"2")]),
            fp(&[("b", b"2"), ("a", b"1")])
        );
        assert_ne!(fp(&[("a", b"")]), fp(&[]));
        let mut s = Fingerprint::default();
        s.begin("a");
        s.update(b"b");
        s.update(b"c");
        assert_eq!(s.hex(), fp(&[("a", b"bc")]));
    }
}
