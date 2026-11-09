//! Импорт пространства из архива экспорта (M6 §12б.5, задача 18b2).
//!
//! **Шаги** (вызывает сервер, `POST /spaces/{space}/import`):
//! 1. [`validate_archive`] — весь архив проверяется до первой записи: имена (`..`, абсолютные,
//!    `\`, ссылки — отказ всего архива), предел файлов, распакованного объёма и коэффициента
//!    сжатия, разбор описи, frontmatter, истории, связей; sha256 файлов вложений сверяется с
//!    frontmatter. Итог — [`Plan`]: тексты в памяти, файлы вложений — именами в архиве.
//!    Архив без `manifest.json` — «папка `.md`»: все `*.md` — заметки, архив чужой.
//! 2. [`plan_ids`] — id назначения: чужой архив — новые (UUIDv7); свой — исходные, кроме
//!    коллизий (id занят в другой базе сервера, точкой Qdrant другого пользователя или в этой
//!    базе вне пространства). Уже импортированное (`imported_from.source_id`) получает id
//!    прежнего импорта — повтор находит ту же запись.
//! 3. [`remap_refs`] — **один сканер** ([`scan_refs`]) по всем текстовым полям и строкам
//!    `data`, плюс явные столбцы-ссылки [`REF_COLUMNS`]: ссылки на элементы архива
//!    переписываются, `a:<чанк>` и прочие — как есть, в `unresolved`.
//! 4. [`apply_plan`] — запись служебной операцией `system` пачками по [`BATCH`] в транзакции:
//!    статусы, `data` и история — как есть (исходная атрибуция из `_history`), плюс ревизия
//!    импорта. Производные (рёбра wikilink и frontmatter, `file_refs` заметок, чанки, FTS,
//!    задания индексации, документы вложений, словарь) строит обычная запись `zam-store`; связи
//!    идей и задач, `supersedes` и явные связи из `_links.jsonl` — последней транзакцией.
//!    `dry_run` — всё в одной транзакции с откатом: тот же отчёт без записи.
//!
//! **Повторы и конфликты.** Каждая запись и заметка получает `imported_from {archive_fingerprint,
//! source_id, source_rev, content_hash}` (колонка `imported_from` — не поле `data`: `data` задач,
//! вопросов, идей и терминов нормализуется типизированным разбором при каждой записи сервера).
//! Повтор сравнивается по `source_id` + `content_hash` исходного содержимого: совпали — пропуск,
//! отличаются — `on_conflict` (`skip` — в `conflicts`, `revision` — новая ревизия). У заметок id
//! сильнее пути: та же заметка по другому пути переносится только при `revision`; путь занят
//! другим узлом — конфликт `path-taken` (при `revision` заметка ложится рядом:
//! `<путь> (импорт <yyyy-MM-dd>)`).
//!
//! **Память** — все тексты архива (заметки, записи, история) в [`Plan`]; файлы вложений не
//! читаются в память (sha256 считается потоком, распаковка в блобы — [`extract_blobs`]).
//! Потоковый импорт без текстов в памяти — M7.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zam_core::{Id, TargetKind, TargetRef};
use zip::ZipArchive;

pub use crate::archive::{
    ATTACHMENTS_DIR, EXPORT_FORMAT, FOLDERS_FILE, Fingerprint, HISTORY_DIR, JOURNALS_DIR,
    LINKS_FILE, MANIFEST_FILE,
};
use crate::attachments::DocIntake;
use crate::blobs::BlobStore;
use crate::entries::{
    Certainty, Entry, EntryKind, EntryStatus, Fields, NewTrigger, allowed_statuses, check_triggers,
    content_data, ensure_release_version_free, entry_index_state, has_certainty, index_entry_now,
    load_entry, normalize_title, replace_triggers, save, tags_json,
};
use crate::graph::{EdgeType, NewEdge};
use crate::nodes::{Attribution, AuthorKind, NodeKind, NoteMeta, load_node, note_meta};
use crate::paths::NodePath;
use crate::time::{fmt_ts, parse_ts};
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Элементов (папок, заметок, записей) в одной транзакции записи.
pub const BATCH: usize = 200;
/// Коэффициент сжатия по умолчанию (распакованный объём к сжатому).
pub const MAX_RATIO_DEFAULT: u32 = 100;
/// Запас коэффициента сжатия на файл, байт: короткий повторяющийся текст жмётся сильнее 100 раз,
/// а `size ≤ packed × ratio + запас` пропускает его (бомбу из мелких файлов ловит и общий
/// коэффициент, и предел текстов).
const RATIO_SLACK: u64 = 64 << 10;
/// Запас общего коэффициента сжатия, байт.
const RATIO_SLACK_TOTAL: u64 = 1 << 20;
/// Предел текстов архива по умолчанию (`import.max_text_bytes`), байт.
pub const MAX_TEXT_BYTES_DEFAULT: u64 = 256 << 20;

/// Буквы ссылок, которые переписывает импорт: [`crate::ENTRY_REF_LETTERS`] (`i c r t k q p w e`)
/// ∪ {`n`, `d`} — канон `TargetRef` (`zam-core/src/target.rs`).
pub const REF_LETTERS: &[&str] = &["i", "c", "r", "t", "k", "q", "p", "w", "e", "n", "d"];

/// Явные столбцы-ссылки записи: голые id (или списки), кроме `data.blocked_by` и
/// `data.links` (строки, в них ссылки ищет [`scan_refs`]).
pub const REF_COLUMNS: &[&str] = &[
    "supersedes_id",
    "superseded_by_id",
    "data.task_id",
    "data.prev_task_ids",
    "data.question_id",
    "data.idea",
    "data.blocked_by",
    "data.links",
    "data.answer.decision_id",
];

// ---------- параметры и отчёт ----------

/// Что делать с изменившимся элементом повторного импорта.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnConflict {
    /// В отчёт `conflicts` (по умолчанию).
    #[default]
    Skip,
    /// Новая ревизия.
    Revision,
}

/// Аксиомы архива (решение владельца 2026-11-05, ответ А).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AxiomPolicy {
    /// `axiom` → `theorem` (по умолчанию для чужого архива).
    Demote,
    /// Как есть (свой архив; у чужого — прямое указание пользователя).
    Keep,
}

/// Параметры импорта.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Прямое указание пользователя «это мой архив» (`own=true`, `zam import --own`).
    pub own: bool,
    pub on_conflict: OnConflict,
    /// `None` — по архиву: свой → `Keep`, чужой → `Demote`.
    pub axioms: Option<AxiomPolicy>,
    pub dry_run: bool,
    /// Отчёты (`report`) записываются: только сессии WebUI (токену отчётов нет, M5.5).
    pub with_reports: bool,
    /// Остановка сервера: между пачками импорт прерывается ([`ImportError::Stopped`]).
    pub stop: Option<Arc<AtomicBool>>,
    /// Ход записи для задания импорта (счётчики элементов, записана ли хоть одна пачка).
    pub progress: Option<Arc<Progress>>,
    /// Элементов в пачке; 0 — [`BATCH`] (меньше — только тесты).
    pub batch: usize,
    /// Прервать запись после стольких пачек как при остановке (точка теста повтора после обрыва).
    pub stop_after: Option<usize>,
}

/// Ход записи импорта.
#[derive(Debug, Default)]
pub struct Progress {
    /// Элементов к записи (папки, заметки, записи).
    pub total: AtomicU64,
    /// Обработано элементов.
    pub done: AtomicU64,
    /// Хоть одна пачка зафиксирована (вызывающий должен оповестить индексацию и рендер, даже если
    /// импорт потом прервался).
    pub written: AtomicBool,
}

/// Происхождение импортированной записи или заметки (`imported_from`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedFrom {
    pub archive_fingerprint: String,
    pub source_id: String,
    pub source_rev: u32,
    /// Хэш исходного содержимого: sha256 текста заметки, у записи — [`entry_hash`].
    pub content_hash: String,
}

/// Конфликт: элемент по id или пути и причина (`changed`, `path-taken`, `report-immutable`,
/// `term-conflict`, `release-version`, `report-space`, `invalid`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub reason: String,
}

/// Переназначенный id своего архива (коллизия).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Remapped {
    pub from: String,
    pub to: String,
}

/// Отчёт импорта (§12б.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub created: u64,
    pub revised: u64,
    pub skipped: u64,
    pub conflicts: Vec<Conflict>,
    pub demoted_axioms: u64,
    /// Переназначено id своего архива из-за коллизий (у чужого все id новые — 0).
    pub id_remapped: u64,
    pub remapped: Vec<Remapped>,
    /// Ссылки, которых нет в таблице соответствия (`a:<чанк>`, цели вне архива), как в тексте.
    pub unresolved: Vec<String>,
    pub links_skipped: u64,
    pub warnings: Vec<String>,
    pub dry_run: bool,
    pub archive_fingerprint: String,
    /// Свой архив (аксиомы и id как есть).
    pub own: bool,
}

impl ImportReport {
    fn merge(&mut self, o: ImportReport) {
        self.created += o.created;
        self.revised += o.revised;
        self.skipped += o.skipped;
        self.conflicts.extend(o.conflicts);
        self.demoted_axioms += o.demoted_axioms;
        self.links_skipped += o.links_skipped;
        self.warnings.extend(o.warnings);
        for u in o.unresolved {
            if !self.unresolved.contains(&u) {
                self.unresolved.push(u);
            }
        }
    }
}

/// Ошибка импорта.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// Архив отвергнут целиком (422): ничего не записано.
    #[error("{0}")]
    Rejected(String),
    /// Сервер останавливается: записаны первые пачки, повтор импорта их пропустит.
    #[error("сервер останавливается: импорт прерван, повторите — записанное пропустится")]
    Stopped,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        ImportError::Store(e.into())
    }
}

fn rejected(msg: impl Into<String>) -> ImportError {
    ImportError::Rejected(msg.into())
}

/// Пределы архива.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLimits {
    /// Распакованный объём, байт.
    pub max_bytes: u64,
    pub max_files: usize,
    /// Коэффициент сжатия файла и архива (распакованный к сжатому) — не больше.
    pub max_ratio: u32,
    /// Все файлы, кроме вложений (`_attachments/`), — тексты плана в памяти, байт.
    pub max_text_bytes: u64,
}

// ---------- план ----------

/// Автор ревизии или записи (атрибуция из архива).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub kind: AuthorKind,
    pub user_id: Option<Id>,
    pub token_id: Option<Id>,
    pub session_id: Option<String>,
    pub machine: Option<String>,
}

/// Вложение заметки или файл отчёта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub sha256: String,
    pub name: String,
    pub mime: Option<String>,
    pub size: u64,
    pub created_at: Option<Timestamp>,
    /// Документ вложения в архиве (`d:<id>`) и id назначения.
    pub doc: Option<Id>,
    pub doc_to: Option<Id>,
    /// Файл в архиве; `None` — блоба нет (вложение пропускается с предупреждением).
    pub file: Option<String>,
}

/// Ревизия заметки из `_history`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRev {
    pub author: Author,
    pub message: Option<String>,
    pub ts: Timestamp,
    pub content: String,
}

/// Ревизия записи из `_history`.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryRev {
    pub title: String,
    pub body: String,
    pub data: Value,
    pub tags: Vec<String>,
    pub area: Option<String>,
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    pub reason: Option<String>,
    pub ts: Timestamp,
    pub author: Author,
}

/// Папка (`_folders.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFolder {
    pub src: Option<Id>,
    pub id: Id,
    pub path: String,
    pub l0: Option<String>,
    pub l0_source: Option<String>,
}

/// Заметка.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanNote {
    /// Файл архива (для сообщений).
    pub file: String,
    /// Исходный id (`None` — папка `.md`).
    pub src: Option<Id>,
    /// Id назначения ([`remap_refs`]).
    pub id: Id,
    pub path: String,
    pub content: String,
    pub rev: u32,
    pub l0: Option<String>,
    pub l0_source: Option<String>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub attachments: Vec<PlanFile>,
    pub history: Vec<NoteRev>,
    /// sha256 исходного текста.
    pub content_hash: String,
}

impl PlanNote {
    /// Ключ источника (`imported_from.source_id`): исходный id, у папки `.md` — `path:<путь>`.
    pub fn key(&self) -> String {
        match self.src {
            Some(s) => s.to_string(),
            None => format!("path:{}", self.path),
        }
    }
}

/// Ссылка записи на файл решения (`file_refs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRef {
    pub path: String,
    pub symbol: Option<String>,
    pub hash: Option<String>,
    pub symbol_hash: Option<String>,
    pub commit: Option<String>,
    pub solution: Option<String>,
}

/// Запись журнала.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub file: String,
    pub src: Id,
    pub id: Id,
    pub kind: EntryKind,
    pub title: String,
    pub body: String,
    pub data: Value,
    pub tags: Vec<String>,
    pub area: Option<String>,
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    pub triggers: Vec<NewTrigger>,
    pub refs: Vec<PlanRef>,
    pub supersedes: Option<Id>,
    pub superseded_by: Option<Id>,
    pub retract_reason: Option<String>,
    pub rev: u32,
    pub author: Author,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub files: Vec<PlanFile>,
    pub history: Vec<EntryRev>,
    pub content_hash: String,
}

/// Явная связь (`_links.jsonl`): концы — `n:<id>` | `e:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLink {
    pub src: TargetRef,
    pub kind: EdgeType,
    pub dst: TargetRef,
}

/// Проверенный архив.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Есть опись (архив экспорта); нет — «папка `.md`».
    pub has_manifest: bool,
    pub source_user_id: Option<Id>,
    /// Пространство архива (`space` описи): `data.task_space` вопросов-предложений «начать»,
    /// указывающий на него, переводится на пространство назначения.
    pub source_space: Option<Id>,
    pub archive_fingerprint: String,
    pub folders: Vec<PlanFolder>,
    pub notes: Vec<PlanNote>,
    pub entries: Vec<PlanEntry>,
    pub links: Vec<PlanLink>,
    /// Файлы вложений архива: имя → (sha256, размер).
    pub blobs: BTreeMap<String, (String, u64)>,
    pub warnings: Vec<String>,
    /// Решение по документу вложения (заполняет сервер после распаковки блобов): пара
    /// (sha256, имя). Нет — `unsupported`.
    pub intakes: HashMap<(String, String), DocIntake>,
}

impl Plan {
    /// Исходные id элементов архива (заметки, папки, записи, документы вложений).
    pub fn source_ids(&self) -> Vec<Id> {
        let mut v: Vec<Id> = self.folders.iter().filter_map(|f| f.src).collect();
        v.extend(self.notes.iter().filter_map(|n| n.src));
        v.extend(self.entries.iter().map(|e| e.src));
        v.extend(
            self.notes
                .iter()
                .flat_map(|n| n.attachments.iter().filter_map(|a| a.doc)),
        );
        v
    }

    /// Источник архива для детерминированных id: `<source_user_id>/<space>` описи, у папки
    /// `.md` — `folder`.
    pub fn source_key(&self) -> String {
        match (self.has_manifest, self.source_user_id, self.source_space) {
            (true, Some(u), Some(s)) => format!("{u}/{s}"),
            (true, u, s) => format!("{u:?}/{s:?}"),
            (false, _, _) => "folder".into(),
        }
    }

    /// Элементы плана: (ключ источника, исходный id, вид `node`/`entry`/`doc`).
    fn keyed(&self) -> Vec<(String, Option<Id>, &'static str)> {
        let mut v: Vec<(String, Option<Id>, &'static str)> = Vec::new();
        for f in &self.folders {
            if let Some(s) = f.src {
                v.push((s.to_string(), Some(s), "node"));
            }
        }
        for n in &self.notes {
            v.push((n.key(), n.src, "node"));
            for a in &n.attachments {
                if let Some(d) = a.doc {
                    v.push((d.to_string(), Some(d), "doc"));
                }
            }
        }
        for e in &self.entries {
            v.push((e.src.to_string(), Some(e.src), "entry"));
        }
        v
    }

    /// Кандидаты id назначения (для проверки коллизий вне базы — другие базы и точки Qdrant):
    /// детерминированные [`import_id`] и, у своего архива, исходные.
    pub fn candidate_ids(&self, user: Id, space: Id, own: bool) -> Vec<Id> {
        let src = self.source_key();
        let mut out = Vec::new();
        for (key, id, _) in self.keyed() {
            if own && let Some(i) = id {
                out.push(i);
            }
            out.push(import_id(user, space, &src, &key));
        }
        out
    }

    /// Свой архив: `source_user_id` = пользователь базы или прямое указание `own`.
    pub fn is_own(&self, user: Id, own_flag: bool) -> bool {
        own_flag || (self.has_manifest && self.source_user_id == Some(user))
    }
}

/// Пространство имён детерминированных id импорта (UUIDv5).
const NS_IMPORT: uuid::Uuid = uuid::Uuid::from_u128(0x5c1e_7a4d_2b9f_5e03_a1c6_88d2_4f0e_b713);

/// Детерминированный id назначения: UUIDv5 от (получатель, пространство, источник архива, ключ
/// элемента). Повтор того же архива после обрыва даёт те же id — ссылки уже записанных пачек на
/// ещё не записанные элементы не повисают; у другого получателя id другие (точки Qdrant общей
/// коллекции не пересекаются).
pub fn import_id(user: Id, space: Id, source: &str, key: &str) -> Id {
    Id::from(uuid::Uuid::new_v5(
        &NS_IMPORT,
        format!("{user}/{space}/{source}/{key}").as_bytes(),
    ))
}

// ---------- сканер ссылок ----------

/// Ссылка в тексте: байты `[start, end)` — `x:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSpan {
    pub start: usize,
    pub end: usize,
    pub letter: String,
    /// `None` — `a:<чанк>` (id чанка зависит от нарезки).
    pub id: Option<Id>,
}

/// Длина id в тексте (base32).
const ID_LEN: usize = 26;
/// Длина id чанка вложения (hex).
const CHUNK_LEN: usize = 32;

/// Ссылки текста (по образцу `refs_in_text` xtask): буква из [`REF_LETTERS`] или `a`, `:`, id
/// (26 символов base32; у `a:` — 32 hex) между границами слова (не буква и не цифра ASCII).
pub fn scan_refs(text: &str) -> Vec<RefSpan> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < b.len() {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if prev_ok && b[i + 1] == b':' && b[i].is_ascii_lowercase() {
            let letter = &text[i..i + 1];
            let len = if letter == "a" {
                CHUNK_LEN
            } else if REF_LETTERS.contains(&letter) {
                ID_LEN
            } else {
                0
            };
            let end = i + 2 + len;
            if len > 0
                && end <= b.len()
                && b[i + 2..end].iter().all(u8::is_ascii_alphanumeric)
                && b.get(end).is_none_or(|c| !c.is_ascii_alphanumeric())
            {
                let id = if letter == "a" {
                    b[i + 2..end]
                        .iter()
                        .all(u8::is_ascii_hexdigit)
                        .then_some(None)
                } else {
                    text[i + 2..end].parse::<Id>().ok().map(Some)
                };
                if let Some(id) = id {
                    out.push(RefSpan {
                        start: i,
                        end,
                        letter: letter.to_string(),
                        id,
                    });
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

// ---------- проверка архива ----------

/// Имя файла архива допустимо: относительное, без `..`, `.`, пустых сегментов, `\` и диска.
fn check_name(name: &str) -> Result<(), ImportError> {
    let bad = |why: &str| rejected(format!("архив отклонён: «{name}» — {why}"));
    if name.is_empty() {
        return Err(bad("пустое имя"));
    }
    if name.contains('\\') {
        return Err(bad("в имени «\\»"));
    }
    if name.starts_with('/') {
        return Err(bad("абсолютный путь"));
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return Err(bad("путь с диском"));
    }
    if name.contains('\0') {
        return Err(bad("нулевой символ"));
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(bad("сегмент «..», «.» или пустой"));
        }
    }
    Ok(())
}

/// Блок frontmatter экспорта: поля `key: <json>` и тело после блока.
fn front(name: &str, text: &str) -> Result<(Map<String, Value>, String), ImportError> {
    let bad = |why: String| rejected(format!("{name}: {why}"));
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| bad("нет frontmatter".into()))?;
    let end = rest
        .find("\n---\n")
        .ok_or_else(|| bad("нет конца frontmatter".into()))?;
    let mut m = Map::new();
    for line in rest[..end].lines() {
        let (k, v) = line
            .split_once(": ")
            .ok_or_else(|| bad(format!("строка frontmatter «{line}»")))?;
        let v: Value = serde_json::from_str(v).map_err(|e| bad(format!("поле {k}: {e}")))?;
        m.insert(k.to_string(), v);
    }
    Ok((m, rest[end + 5..].to_string()))
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

fn opt_id(name: &str, v: Option<&Value>) -> Result<Option<Id>, ImportError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => s
            .parse()
            .map(Some)
            .map_err(|_| rejected(format!("{name}: некорректный id «{s}»"))),
        Some(x) => Err(rejected(format!("{name}: id не строка: {x}"))),
    }
}

fn opt_ts(name: &str, v: Option<&Value>) -> Result<Option<Timestamp>, ImportError> {
    match v.and_then(Value::as_str) {
        None => Ok(None),
        Some(s) => parse_ts(s)
            .map(Some)
            .map_err(|_| rejected(format!("{name}: некорректное время «{s}»"))),
    }
}

fn need_ts(name: &str, v: Option<&Value>) -> Result<Timestamp, ImportError> {
    opt_ts(name, v)?.ok_or_else(|| rejected(format!("{name}: нет времени ревизии")))
}

fn author_kind(name: &str, s: Option<&str>) -> Result<AuthorKind, ImportError> {
    match s {
        Some("human") => Ok(AuthorKind::Human),
        Some("agent") => Ok(AuthorKind::Agent),
        Some("system") => Ok(AuthorKind::System),
        other => Err(rejected(format!("{name}: неизвестный автор {other:?}"))),
    }
}

fn author_of(name: &str, kind: Option<&str>, v: &Value) -> Result<Author, ImportError> {
    Ok(Author {
        kind: author_kind(name, kind)?,
        user_id: opt_id(name, v.get("user_id"))?,
        token_id: opt_id(name, v.get("token_id"))?,
        session_id: opt_str(v.get("session_id")),
        machine: opt_str(v.get("machine")),
    })
}

fn strings(name: &str, v: Option<&Value>) -> Result<Vec<String>, ImportError> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| rejected(format!("{name}: ожидается список строк")))
            })
            .collect(),
        Some(_) => Err(rejected(format!("{name}: ожидается список строк"))),
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(
    name: &str,
    field: &str,
    v: Option<&Value>,
) -> Result<Option<T>, ImportError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(x) => serde_json::from_value(x.clone())
            .map(Some)
            .map_err(|_| rejected(format!("{name}: {field} «{x}»"))),
    }
}

fn files_of(name: &str, v: Option<&Value>) -> Result<Vec<PlanFile>, ImportError> {
    let Some(Value::Array(a)) = v else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for f in a {
        let sha = opt_str(f.get("sha256"))
            .filter(|s| crate::blobs::is_sha256(s))
            .ok_or_else(|| rejected(format!("{name}: файл без sha256")))?;
        let fname =
            opt_str(f.get("name")).ok_or_else(|| rejected(format!("{name}: файл без имени")))?;
        out.push(PlanFile {
            sha256: sha,
            name: fname,
            mime: opt_str(f.get("mime")),
            size: f.get("size").and_then(Value::as_u64).unwrap_or(0),
            created_at: opt_ts(name, f.get("created_at"))?,
            doc: opt_id(name, f.get("doc"))?,
            doc_to: None,
            file: opt_str(f.get("file")),
        });
    }
    Ok(out)
}

/// sha256 hex.
fn sha256_hex(b: &[u8]) -> String {
    crate::nodes::sha256_hex(b)
}

/// JSON с ключами объектов по алфавиту (хэш не зависит от порядка полей).
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            let mut m = Map::new();
            for k in keys {
                m.insert(k.clone(), canonical(&o[k]));
            }
            Value::Object(m)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        x => x.clone(),
    }
}

/// Хэш содержимого записи для повтора импорта: вид, заголовок, тело, `data` (без захвата задачи),
/// теги, область, статус, `certainty`.
#[allow(clippy::too_many_arguments)]
pub fn entry_hash(
    kind: EntryKind,
    title: &str,
    body: &str,
    data: &Value,
    tags: &[String],
    area: Option<&str>,
    status: Option<EntryStatus>,
    certainty: Option<Certainty>,
) -> String {
    let d = content_data(kind, data);
    let v = json!([
        kind.as_str(),
        title,
        body,
        canonical(&d),
        tags,
        area,
        status.map(EntryStatus::as_str),
        certainty.map(Certainty::as_str)
    ]);
    blake3::hash(v.to_string().as_bytes()).to_hex().to_string()
}

fn entry_hash_of(e: &Entry) -> String {
    entry_hash(
        e.kind,
        &e.title,
        &e.body,
        &e.data,
        &e.tags,
        e.area.as_deref(),
        e.status,
        e.certainty,
    )
}

/// Сырой файл архива (текст) после проверки пределов.
struct RawText {
    name: String,
    text: String,
}

/// Проверяет весь архив (см. модуль) и строит [`Plan`]. Отказ — [`ImportError::Rejected`] (422).
pub fn validate_archive<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    limits: &ImportLimits,
) -> Result<Plan, ImportError> {
    let n = zip.len();
    if n > limits.max_files {
        return Err(rejected(format!(
            "архив отклонён: файлов {n} — больше предела {}",
            limits.max_files
        )));
    }
    // имена и пределы — по центральному каталогу, до чтения содержимого
    let mut total: u64 = 0;
    let mut total_packed: u64 = 0;
    let mut names = Vec::with_capacity(n);
    let mut sizes = Vec::with_capacity(n);
    let mut has_manifest = false;
    for i in 0..n {
        let f = zip
            .by_index_raw(i)
            .map_err(|e| rejected(format!("архив повреждён: {e}")))?;
        let name = f
            .name()
            .map_err(|e| rejected(format!("архив: имя файла не UTF-8: {e}")))?
            .to_string();
        check_name(&name)?;
        if f.is_symlink() {
            return Err(rejected(format!(
                "архив отклонён: «{name}» — символьная ссылка"
            )));
        }
        if f.encrypted() {
            return Err(rejected(format!("архив отклонён: «{name}» зашифрован")));
        }
        let (size, packed) = (f.size(), f.compressed_size());
        total_packed = total_packed.saturating_add(packed);
        if size
            > packed
                .saturating_mul(u64::from(limits.max_ratio))
                .saturating_add(RATIO_SLACK)
        {
            return Err(rejected(format!(
                "архив отклонён: «{name}» сжат сильнее чем в {} раз (zip-бомба?)",
                limits.max_ratio
            )));
        }
        total = total.saturating_add(size);
        if total > limits.max_bytes {
            return Err(rejected(format!(
                "архив отклонён: распакованный объём больше предела {} байт",
                limits.max_bytes
            )));
        }
        has_manifest |= name == MANIFEST_FILE;
        sizes.push(size);
        names.push((name, f.is_dir()));
    }
    if total
        > total_packed
            .saturating_mul(u64::from(limits.max_ratio))
            .saturating_add(RATIO_SLACK_TOTAL)
    {
        return Err(rejected(format!(
            "архив отклонён: архив сжат сильнее чем в {} раз (zip-бомба?)",
            limits.max_ratio
        )));
    }
    // тексты (всё, кроме вложений) держит план: свой предел
    let is_text = |name: &str| {
        if has_manifest {
            !name.starts_with(&format!("{ATTACHMENTS_DIR}/"))
        } else {
            name.ends_with(".md")
        }
    };
    let text_total: u64 = names
        .iter()
        .zip(&sizes)
        .filter(|((name, dir), _)| !dir && is_text(name))
        .map(|(_, s)| *s)
        .sum();
    if text_total > limits.max_text_bytes {
        return Err(rejected(format!(
            "архив отклонён: тексты архива ({text_total} байт) больше предела \
             import.max_text_bytes = {} байт",
            limits.max_text_bytes
        )));
    }
    let mut plan = Plan {
        has_manifest,
        ..Plan::default()
    };
    let mut fp = Fingerprint::default();
    let mut texts: Vec<RawText> = Vec::new();
    let mut manifest: Option<Value> = None;
    let mut read_total: u64 = 0;
    let mut read_text: u64 = 0;
    for (i, (name, is_dir)) in names.iter().enumerate() {
        if *is_dir {
            continue;
        }
        let f = zip
            .by_index(i)
            .map_err(|e| rejected(format!("архив повреждён: {name}: {e}")))?;
        let declared = f.size();
        // не больше заявленного размера: заголовок не обманет предел
        let mut r = f.take(declared.saturating_add(1));
        let blob = has_manifest && name.starts_with(&format!("{ATTACHMENTS_DIR}/"));
        // не текст плана: хэш потоком, в память не читается
        let hashed = !is_text(name);
        if name != MANIFEST_FILE {
            fp.begin(name);
        }
        let mut buf = vec![0u8; 64 * 1024];
        let mut data = Vec::new();
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        loop {
            let k = r
                .read(&mut buf)
                .map_err(|e| rejected(format!("архив повреждён: {name}: {e}")))?;
            if k == 0 {
                break;
            }
            size += k as u64;
            read_total += k as u64;
            if !hashed {
                read_text += k as u64;
            }
            if size > declared || read_total > limits.max_bytes || read_text > limits.max_text_bytes
            {
                return Err(rejected(format!(
                    "архив отклонён: «{name}» больше заявленного размера"
                )));
            }
            if name != MANIFEST_FILE {
                fp.update(&buf[..k]);
            }
            if hashed {
                hasher.update(&buf[..k]);
            } else {
                data.extend_from_slice(&buf[..k]);
            }
        }
        fp.end();
        if size != declared {
            return Err(rejected(format!(
                "архив повреждён: «{name}» — {size} байт вместо заявленных {declared}"
            )));
        }
        if blob {
            plan.blobs
                .insert(name.clone(), (crate::nodes::hex(&hasher.finalize()), size));
            continue;
        }
        if name == MANIFEST_FILE {
            manifest = Some(
                serde_json::from_slice(&data)
                    .map_err(|e| rejected(format!("{MANIFEST_FILE}: {e}")))?,
            );
            continue;
        }
        if hashed {
            plan.warnings.push(format!(
                "файл «{name}» пропущен: из папки .md переносятся только заметки"
            ));
            continue;
        }
        let text =
            String::from_utf8(data).map_err(|_| rejected(format!("{name}: текст не UTF-8")))?;
        texts.push(RawText {
            name: name.clone(),
            text,
        });
    }
    let computed = fp.hex();
    if let Some(m) = &manifest {
        let format = m.get("format").and_then(Value::as_u64).unwrap_or(0);
        if format != u64::from(EXPORT_FORMAT) {
            return Err(rejected(format!(
                "{MANIFEST_FILE}: формат {format}, поддерживается {EXPORT_FORMAT}"
            )));
        }
        plan.source_user_id = opt_id(MANIFEST_FILE, m.get("source_user_id"))?;
        plan.source_space = opt_id(MANIFEST_FILE, m.get("space"))?;
        let declared = opt_str(m.get("archive_fingerprint")).unwrap_or_default();
        if declared != computed {
            plan.warnings
                .push("отпечаток архива не совпал с описью: архив изменён после экспорта".into());
        }
        for b in m
            .get("missing_blobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            plan.warnings.push(format!(
                "файла вложения нет в архиве (его не было на сервере): {}",
                b.as_str().unwrap_or_default()
            ));
        }
    }
    plan.archive_fingerprint = computed;
    if has_manifest {
        parse_archive(&mut plan, texts)?;
    } else {
        parse_folder(&mut plan, texts)?;
    }
    Ok(plan)
}

/// «Папка `.md`»: все `*.md` — заметки, путь — имя файла без `.md`.
fn parse_folder(plan: &mut Plan, texts: Vec<RawText>) -> Result<(), ImportError> {
    for t in texts {
        let path = t.name.strip_suffix(".md").unwrap_or(&t.name).to_string();
        if NodePath::parse(&path).is_err() {
            plan.warnings.push(format!(
                "файл «{}» пропущен: недопустимый путь заметки",
                t.name
            ));
            continue;
        }
        plan.notes.push(PlanNote {
            file: t.name,
            src: None,
            id: Id::new(),
            path,
            content_hash: sha256_hex(t.text.as_bytes()),
            content: t.text,
            rev: 1,
            l0: None,
            l0_source: None,
            created_at: None,
            updated_at: None,
            attachments: Vec::new(),
            history: Vec::new(),
        });
    }
    Ok(())
}

/// Архив экспорта: заметки, папки, записи, связи, история.
fn parse_archive(plan: &mut Plan, texts: Vec<RawText>) -> Result<(), ImportError> {
    let mut history: HashMap<Id, (String, Vec<Value>)> = HashMap::new();
    let mut folders_raw: Option<RawText> = None;
    let mut links_raw: Option<RawText> = None;
    for t in texts {
        if t.name == FOLDERS_FILE {
            folders_raw = Some(t);
        } else if t.name == LINKS_FILE {
            links_raw = Some(t);
        } else if let Some(rest) = t.name.strip_prefix(&format!("{HISTORY_DIR}/")) {
            let id: Id = rest
                .strip_suffix(".jsonl")
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| rejected(format!("{}: имя файла истории", t.name)))?;
            let mut lines = Vec::new();
            for l in t.text.lines().filter(|l| !l.trim().is_empty()) {
                lines.push(
                    serde_json::from_str::<Value>(l)
                        .map_err(|e| rejected(format!("{}: {e}", t.name)))?,
                );
            }
            history.insert(id, (t.name, lines));
        } else if t.name.starts_with(&format!("{JOURNALS_DIR}/")) {
            let e = parse_entry(&t)?;
            plan.entries.push(e);
        } else if t.name.starts_with('_') {
            plan.warnings.push(format!(
                "служебный файл «{}» не распознан и пропущен",
                t.name
            ));
        } else if t.name.ends_with(".md") {
            let n = parse_note(&t)?;
            plan.notes.push(n);
        } else {
            plan.warnings
                .push(format!("файл «{}» пропущен: не заметка", t.name));
        }
    }
    if let Some(t) = folders_raw {
        for l in t.text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value =
                serde_json::from_str(l).map_err(|e| rejected(format!("{}: {e}", t.name)))?;
            let path = opt_str(v.get("path"))
                .filter(|p| NodePath::parse(p).is_ok())
                .ok_or_else(|| rejected(format!("{}: путь папки", t.name)))?;
            plan.folders.push(PlanFolder {
                src: opt_id(&t.name, v.get("id"))?,
                id: Id::new(),
                path,
                l0: opt_str(v.get("l0")),
                l0_source: opt_str(v.get("l0_source")),
            });
        }
    }
    if let Some(t) = links_raw {
        for l in t.text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value =
                serde_json::from_str(l).map_err(|e| rejected(format!("{}: {e}", t.name)))?;
            let end = |k: &str| -> Result<TargetRef, ImportError> {
                opt_str(v.get(k))
                    .and_then(|s| s.parse::<TargetRef>().ok())
                    .ok_or_else(|| rejected(format!("{}: конец связи {k}", t.name)))
            };
            let kind = opt_str(v.get("type"))
                .and_then(|s| EdgeType::parse(&s))
                .ok_or_else(|| rejected(format!("{}: тип связи", t.name)))?;
            plan.links.push(PlanLink {
                src: end("src")?,
                kind,
                dst: end("dst")?,
            });
        }
    }
    // история — к своим элементам
    for n in &mut plan.notes {
        let Some(src) = n.src else { continue };
        if let Some((name, lines)) = history.remove(&src) {
            for v in lines {
                n.history.push(NoteRev {
                    author: author_of(&name, v.get("author_kind").and_then(Value::as_str), &v)?,
                    message: opt_str(v.get("message")),
                    ts: need_ts(&name, v.get("ts"))?,
                    content: opt_str(v.get("content"))
                        .ok_or_else(|| rejected(format!("{name}: ревизия без текста")))?,
                });
            }
        }
    }
    for e in &mut plan.entries {
        if let Some((name, lines)) = history.remove(&e.src) {
            for v in lines {
                let author = v.get("author").cloned().unwrap_or(Value::Null);
                e.history.push(EntryRev {
                    title: opt_str(v.get("title")).unwrap_or_default(),
                    body: opt_str(v.get("body")).unwrap_or_default(),
                    data: v.get("data").cloned().unwrap_or_else(|| json!({})),
                    tags: strings(&name, v.get("tags"))?,
                    area: opt_str(v.get("area")),
                    status: parse_enum(&name, "status", v.get("status"))?,
                    certainty: parse_enum(&name, "certainty", v.get("certainty"))?,
                    reason: opt_str(v.get("reason")),
                    ts: need_ts(&name, v.get("ts"))?,
                    author: author_of(&name, author.get("kind").and_then(Value::as_str), &author)?,
                });
            }
        }
    }
    for (_, (name, _)) in history {
        plan.warnings.push(format!(
            "история «{name}» без заметки или записи — пропущена"
        ));
    }
    // файлы вложений: есть в архиве и совпал sha256
    let blobs = plan.blobs.clone();
    let mut warn = Vec::new();
    let mut check = |owner: &str, files: &mut Vec<PlanFile>| {
        files.retain(|f| {
            let ok = f
                .file
                .as_ref()
                .and_then(|x| blobs.get(x))
                .is_some_and(|(sha, _)| sha == &f.sha256);
            if !ok {
                warn.push(format!(
                    "{owner}: файл «{}» ({}) не найден в архиве или не совпал sha256 — пропущен",
                    f.name, f.sha256
                ));
            }
            ok
        });
    };
    for n in &mut plan.notes {
        check(&n.file, &mut n.attachments);
    }
    for e in &mut plan.entries {
        check(&e.file, &mut e.files);
    }
    plan.warnings.extend(warn);
    Ok(())
}

fn parse_note(t: &RawText) -> Result<PlanNote, ImportError> {
    let (f, content) = front(&t.name, &t.text)?;
    let path = opt_str(f.get("path")).ok_or_else(|| rejected(format!("{}: нет path", t.name)))?;
    NodePath::parse(&path).map_err(|e| rejected(format!("{}: path: {e}", t.name)))?;
    Ok(PlanNote {
        file: t.name.clone(),
        src: opt_id(&t.name, f.get("id"))?,
        id: Id::new(),
        path,
        content_hash: sha256_hex(content.as_bytes()),
        content,
        rev: f.get("rev").and_then(Value::as_u64).unwrap_or(1) as u32,
        l0: opt_str(f.get("l0")),
        l0_source: opt_str(f.get("l0_source")),
        created_at: opt_ts(&t.name, f.get("created_at"))?,
        updated_at: opt_ts(&t.name, f.get("updated_at"))?,
        attachments: files_of(&t.name, f.get("attachments"))?,
        history: Vec::new(),
    })
}

fn parse_entry(t: &RawText) -> Result<PlanEntry, ImportError> {
    let name = t.name.as_str();
    let (f, body) = front(name, &t.text)?;
    let src = opt_id(name, f.get("id"))?.ok_or_else(|| rejected(format!("{name}: нет id")))?;
    let kind: EntryKind = parse_enum(name, "kind", f.get("kind"))?
        .ok_or_else(|| rejected(format!("{name}: нет kind")))?;
    let title = opt_str(f.get("title")).ok_or_else(|| rejected(format!("{name}: нет title")))?;
    let status: Option<EntryStatus> = parse_enum(name, "status", f.get("status"))?;
    if status.is_some_and(|s| !allowed_statuses(kind).contains(&s))
        || (status.is_none() && !allowed_statuses(kind).is_empty())
    {
        return Err(rejected(format!(
            "{name}: статус {status:?} недопустим для {}",
            kind.as_str()
        )));
    }
    let certainty: Option<Certainty> = parse_enum(name, "certainty", f.get("certainty"))?;
    let data = match f.get("data") {
        Some(v @ Value::Object(_)) => v.clone(),
        None | Some(Value::Null) => json!({}),
        Some(_) => return Err(rejected(format!("{name}: data не объект"))),
    };
    let tags = strings(name, f.get("tags"))?;
    let area = opt_str(f.get("area"));
    let mut triggers = Vec::new();
    for t in f
        .get("triggers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut o = Map::new();
        for k in ["type", "pattern", "tools"] {
            if let Some(v) = t.get(k).filter(|v| !v.is_null()) {
                o.insert(k.into(), v.clone());
            }
        }
        triggers.push(
            serde_json::from_value::<NewTrigger>(Value::Object(o))
                .map_err(|e| rejected(format!("{name}: триггер: {e}")))?,
        );
    }
    let mut refs = Vec::new();
    for r in f
        .get("refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        refs.push(PlanRef {
            path: opt_str(r.get("path"))
                .ok_or_else(|| rejected(format!("{name}: ссылка без path")))?,
            symbol: opt_str(r.get("symbol")),
            hash: opt_str(r.get("hash")),
            symbol_hash: opt_str(r.get("symbol_hash")),
            commit: opt_str(r.get("commit")),
            solution: opt_str(r.get("solution")),
        });
    }
    let author_v = f.get("author").cloned().unwrap_or(Value::Null);
    let author = author_of(
        name,
        author_v.get("kind").and_then(Value::as_str),
        &author_v,
    )?;
    let content_hash = entry_hash(
        kind,
        &title,
        &body,
        &data,
        &tags,
        area.as_deref(),
        status,
        certainty,
    );
    Ok(PlanEntry {
        file: t.name.clone(),
        src,
        id: Id::new(),
        kind,
        title,
        body,
        data,
        tags,
        area,
        status,
        certainty,
        triggers,
        refs,
        supersedes: opt_id(name, f.get("supersedes_id"))?,
        superseded_by: opt_id(name, f.get("superseded_by_id"))?,
        retract_reason: opt_str(f.get("retract_reason")),
        rev: f.get("rev").and_then(Value::as_u64).unwrap_or(1) as u32,
        author,
        created_at: opt_ts(name, f.get("created_at"))?,
        updated_at: opt_ts(name, f.get("updated_at"))?,
        files: files_of(name, f.get("files"))?,
        history: Vec::new(),
        content_hash,
    })
}

/// Распаковывает файлы вложений плана (`Plan::blobs`) в блобы пользователя (после [`validate_archive`], не при
/// `dry_run`). Файл — потоком, через `<root>/.incoming/<id>.part`; sha256 сверяется с планом.
pub fn extract_blobs<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    blobs: &BTreeMap<String, (String, u64)>,
    store: &BlobStore,
) -> Result<(), ImportError> {
    for (name, (sha, size)) in blobs {
        let f = zip
            .by_name(name)
            .map_err(|e| rejected(format!("архив: {name}: {e}")))?;
        let (got, _) = store.put_blocking(f, *size)?;
        if &got != sha {
            return Err(rejected(format!("архив изменился при распаковке: {name}")));
        }
    }
    Ok(())
}

// ---------- id назначения ----------

/// Соответствие исходных id новым.
#[derive(Debug, Clone, Default)]
pub struct IdMap {
    pub map: HashMap<Id, Id>,
    /// Заметки папки `.md` (без исходного id): ключ `path:<путь>` → id.
    pub paths: HashMap<String, Id>,
    /// Коллизии: (ключ источника, новый id) — id назначения занят не этим импортом.
    pub remapped: Vec<(String, Id)>,
}

/// Где id в этой базе: пространство (у документа вложения — нет), жив ли, вид
/// (`node`/`entry`/`doc`).
type Place = (Option<Id>, bool, &'static str);

/// Узел, запись или документ вложения этой базы по id.
fn where_is(c: &Connection, id: Id) -> Result<Option<Place>, StoreError> {
    if let Some((s, d)) = c
        .prepare_cached("SELECT space_id, deleted_at IS NULL FROM nodes WHERE id = ?1")?
        .query_row([id], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, bool>(1)?)))
        .optional()?
    {
        return Ok(Some((Some(s), d, "node")));
    }
    if let Some((s, d)) = c
        .prepare_cached("SELECT space_id, deleted_at IS NULL FROM entries WHERE id = ?1")?
        .query_row([id], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, bool>(1)?)))
        .optional()?
    {
        return Ok(Some((Some(s), d, "entry")));
    }
    if c.prepare_cached("SELECT 1 FROM attachment_docs WHERE id = ?1")?
        .query_row([id], |_| Ok(()))
        .optional()?
        .is_some()
    {
        return Ok(Some((None, true, "doc")));
    }
    Ok(None)
}

/// Импортированное ранее в пространство: ключ источника (`imported_from.source_id`) → id.
fn imported_ids(c: &Connection, space: Id) -> Result<HashMap<String, Id>, StoreError> {
    let mut out = HashMap::new();
    for sql in [
        "SELECT id, json_extract(imported_from, '$.source_id') FROM nodes
         WHERE space_id = ?1 AND deleted_at IS NULL AND imported_from IS NOT NULL",
        "SELECT id, json_extract(imported_from, '$.source_id') FROM entries
         WHERE space_id = ?1 AND deleted_at IS NULL AND imported_from IS NOT NULL",
    ] {
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([space], |r| {
            Ok((r.get::<_, Id>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (id, src) = row?;
            if let Some(src) = src.filter(|s| !s.is_empty()) {
                out.insert(src, id);
            }
        }
    }
    Ok(out)
}

/// Id назначения (см. модуль). Прежний импорт этого пространства (по ключу источника) — его id;
/// свой архив — исходный id, если свободен; иначе — детерминированный [`import_id`]. Кандидат
/// занят не этим импортом (`taken` — другие базы и точки Qdrant, или строка этой базы) —
/// случайный id и запись в `remapped`.
pub async fn plan_ids(
    db: &Db,
    user: Id,
    space: Id,
    plan: &Plan,
    own: bool,
    taken: &HashSet<Id>,
) -> Result<IdMap, StoreError> {
    let keyed = plan.keyed();
    let source = plan.source_key();
    let taken = taken.clone();
    db.read(move |c| {
        let prev = imported_ids(c, space)?;
        let mut m = IdMap::default();
        let mut seen: HashSet<String> = HashSet::new();
        for (key, src, kind) in keyed {
            if !seen.insert(format!("{kind}/{key}")) {
                continue;
            }
            let id = match prev.get(&key).filter(|_| kind != "doc") {
                Some(id) => *id,
                None => {
                    let det = import_id(user, space, &source, &key);
                    // свой элемент этого пространства (повтор своего архива) — тот же id
                    let same = |id: Id| -> Result<bool, StoreError> {
                        Ok(own
                            && src == Some(id)
                            && matches!(where_is(c, id)?,
                                        Some((Some(s), true, k)) if s == space && k == kind))
                    };
                    let free = |id: Id| -> Result<bool, StoreError> {
                        Ok(!taken.contains(&id) && (where_is(c, id)?.is_none() || same(id)?))
                    };
                    match src.filter(|_| own) {
                        Some(s) if free(s)? => s,
                        Some(_) if free(det)? => {
                            m.remapped.push((key.clone(), det));
                            det
                        }
                        None if free(det)? => det,
                        _ => {
                            let new = Id::new();
                            m.remapped.push((key.clone(), new));
                            new
                        }
                    }
                }
            };
            match src {
                Some(s) => {
                    m.map.insert(s, id);
                }
                None => {
                    m.paths.insert(key, id);
                }
            }
        }
        Ok(m)
    })
    .await
}

// ---------- переписывание ссылок ----------

/// Итог переписывания: ссылки вне таблицы.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemapReport {
    pub unresolved: Vec<String>,
}

struct Rewriter<'a> {
    map: &'a HashMap<Id, Id>,
    unresolved: Vec<String>,
}

impl Rewriter<'_> {
    fn miss(&mut self, s: String) {
        if !self.unresolved.contains(&s) {
            self.unresolved.push(s);
        }
    }

    /// Текст со ссылками на новые id.
    fn text(&mut self, s: &str) -> String {
        let spans = scan_refs(s);
        if spans.is_empty() {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut at = 0;
        for sp in spans {
            out.push_str(&s[at..sp.start]);
            match sp.id.and_then(|id| self.map.get(&id)) {
                Some(new) => out.push_str(&format!("{}:{new}", sp.letter)),
                None => {
                    self.miss(s[sp.start..sp.end].to_string());
                    out.push_str(&s[sp.start..sp.end]);
                }
            }
            at = sp.end;
        }
        out.push_str(&s[at..]);
        out
    }

    /// Голый id столбца-ссылки: из таблицы — новый, иначе как есть (в `unresolved`).
    fn bare(&mut self, s: &str) -> Option<String> {
        let id: Id = s.parse().ok()?;
        Some(match self.map.get(&id) {
            Some(new) => new.to_string(),
            None => {
                self.miss(s.to_string());
                s.to_string()
            }
        })
    }

    /// Все строки `data`: столбцы-ссылки с голыми id и [`scan_refs`] в остальных строках.
    fn data(&mut self, v: &Value) -> Value {
        let mut v = v.clone();
        if let Value::Object(o) = &mut v {
            for k in ["task_id", "question_id", "idea"] {
                if let Some(Value::String(s)) = o.get(k)
                    && let Some(n) = self.bare(s)
                {
                    o.insert(k.into(), Value::String(n));
                }
            }
            // решение из ответа на вопрос (`data.answer.decision_id`)
            if let Some(Value::Object(ans)) = o.get_mut("answer")
                && let Some(Value::String(s)) = ans.get("decision_id")
                && let Some(n) = self.bare(s)
            {
                ans.insert("decision_id".into(), Value::String(n));
            }
            if let Some(Value::Array(a)) = o.get_mut("prev_task_ids") {
                for x in a.iter_mut() {
                    if let Value::String(s) = x
                        && let Some(n) = self.bare(s)
                    {
                        *x = Value::String(n);
                    }
                }
            }
        }
        self.strings(&mut v);
        v
    }

    fn strings(&mut self, v: &mut Value) {
        match v {
            Value::String(s) => {
                let n = self.text(s);
                *s = n;
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.strings(x)),
            Value::Object(o) => o.values_mut().for_each(|x| self.strings(x)),
            _ => {}
        }
    }

    fn id(&mut self, id: Option<Id>) -> Option<Id> {
        let id = id?;
        match self.map.get(&id) {
            Some(n) => Some(*n),
            None => {
                self.miss(id.to_string());
                Some(id)
            }
        }
    }
}

/// Переписывает ссылки плана по таблице `map` и ставит id назначения (см. модуль).
pub fn remap_refs(plan: &mut Plan, map: &IdMap) -> RemapReport {
    let mut w = Rewriter {
        map: &map.map,
        unresolved: Vec::new(),
    };
    for f in &mut plan.folders {
        if let Some(s) = f.src {
            f.id = map.map.get(&s).copied().unwrap_or(s);
        }
        f.l0 = f.l0.as_deref().map(|x| w.text(x));
    }
    for n in &mut plan.notes {
        if let Some(s) = n.src {
            n.id = map.map.get(&s).copied().unwrap_or(s);
        } else if let Some(id) = map.paths.get(&n.key()) {
            n.id = *id;
        }
        n.content = w.text(&n.content);
        n.l0 = n.l0.as_deref().map(|x| w.text(x));
        for h in &mut n.history {
            h.content = w.text(&h.content);
            h.message = h.message.as_deref().map(|x| w.text(x));
        }
        for a in &mut n.attachments {
            a.doc_to = a
                .doc
                .map(|d| map.map.get(&d).copied().unwrap_or_else(Id::new));
        }
    }
    for e in &mut plan.entries {
        e.id = map.map.get(&e.src).copied().unwrap_or(e.src);
        e.title = w.text(&e.title);
        e.body = w.text(&e.body);
        e.data = w.data(&e.data);
        e.supersedes = w.id(e.supersedes);
        e.superseded_by = w.id(e.superseded_by);
        e.retract_reason = e.retract_reason.as_deref().map(|x| w.text(x));
        for h in &mut e.history {
            h.title = w.text(&h.title);
            h.body = w.text(&h.body);
            h.data = w.data(&h.data);
            h.reason = h.reason.as_deref().map(|x| w.text(x));
        }
    }
    for l in &mut plan.links {
        if let Some(n) = map.map.get(&l.src.id) {
            l.src.id = *n;
        }
        if let Some(n) = map.map.get(&l.dst.id) {
            l.dst.id = *n;
        }
    }
    RemapReport {
        unresolved: w.unresolved,
    }
}

// ---------- запись ----------

/// Понижение аксиомы заметки (I5 ревью): статус читает тот же разбор, что и запись
/// ([`note_meta`]). Не аксиома — `Ok(None)`. Аксиома — каждый ключ `certainty` верхнего уровня
/// собственного frontmatter (со строками продолжения: блочный скаляр, многострочное значение;
/// повтор ключа) заменяется на `certainty: theorem` с прежним комментарием, затем разбор
/// проверяется ещё раз; если статус не стал `theorem` — `Err` (конфликт `invalid`).
fn demote_note(content: &str) -> Result<Option<String>, String> {
    if note_meta(content, None).certainty_lenient() != Some(Certainty::Axiom) {
        return Ok(None);
    }
    let fail = || "certainty: axiom не удалось понизить до theorem — правьте заметку".to_string();
    let nl = if content.starts_with("---\r\n") {
        "\r\n"
    } else if content.starts_with("---\n") {
        "\n"
    } else {
        return Err(fail());
    };
    let start = 3 + nl.len();
    let end = content[start..]
        .find(&format!("{nl}---"))
        .map(|i| start + i)
        .ok_or_else(fail)?;
    let lines: Vec<&str> = content[start..end].split(nl).collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let top = !line.starts_with([' ', '\t', '#', '-']);
        let is_key = top
            && line
                .split_once(':')
                .is_some_and(|(k, _)| k.trim() == "certainty");
        if !is_key {
            out.push(line.to_string());
            continue;
        }
        // строки продолжения значения (с отступом или пустые) уходят вместе с ключом
        while i < lines.len() && (lines[i].starts_with([' ', '\t']) || lines[i].trim().is_empty()) {
            i += 1;
        }
        let comment = line
            .split_once(" #")
            .map(|(_, c)| format!(" #{c}"))
            .unwrap_or_default();
        out.push(format!("certainty: theorem{comment}"));
    }
    let text = format!("{}{}{}", &content[..start], out.join(nl), &content[end..]);
    match note_meta(&text, None).certainty() {
        Ok(Some(Certainty::Theorem)) => Ok(Some(text)),
        _ => Err(fail()),
    }
}

/// Контекст пачки записи.
struct Ctx<'a> {
    space: Id,
    space_kind: SpaceKind,
    opts: &'a ImportOptions,
    own: bool,
    attrib: &'a Attribution,
    /// `импорт <отпечаток> от <пользователь>`.
    reason: String,
    fp: &'a str,
    today: String,
    now: String,
    /// Идеи назначения (в плане): вопрос-предложение без своей идеи — `obsolete`.
    ideas: &'a HashSet<Id>,
    /// Пространство архива ([`Plan::source_space`]).
    source_space: Option<Id>,
}

const DEMOTE_REASON: &str = "импорт: аксиома другого пользователя — понижена до theorem";

impl Ctx<'_> {
    fn demote(&self) -> bool {
        match self.opts.axioms {
            Some(AxiomPolicy::Keep) => false,
            Some(AxiomPolicy::Demote) => true,
            None => !self.own,
        }
    }

    fn sys_author(&self) -> Author {
        Author {
            kind: AuthorKind::System,
            user_id: self.attrib.user_id,
            token_id: self.attrib.token_id,
            session_id: self.attrib.session_id.clone(),
            machine: self.attrib.machine.clone(),
        }
    }

    fn imported_from(&self, key: &str, rev: u32, hash: &str) -> String {
        json!(ImportedFrom {
            archive_fingerprint: self.fp.to_string(),
            source_id: key.to_string(),
            source_rev: rev,
            content_hash: hash.to_string(),
        })
        .to_string()
    }
}

/// Найденный элемент назначения: id и хэш для сравнения.
fn note_existing(
    c: &Connection,
    ctx: &Ctx,
    n: &PlanNote,
) -> Result<Option<(Id, String, String)>, StoreError> {
    let row: Option<(Id, String, Option<String>, Option<String>)> = c
        .prepare_cached(
            "SELECT id, path, content_hash, imported_from FROM nodes
             WHERE id = ?1 AND space_id = ?2 AND kind = 'note' AND deleted_at IS NULL",
        )?
        .query_row(params![n.id, ctx.space], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    Ok(row.map(|(id, path, hash, imp)| {
        let h = imp
            .and_then(|s| serde_json::from_str::<ImportedFrom>(&s).ok())
            .filter(|i| i.source_id == n.key())
            .map(|i| i.content_hash)
            .or(hash)
            .unwrap_or_default();
        (id, path, h)
    }))
}

/// Свободный путь «рядом» при `path-taken` (`<путь> (импорт <дата>)`, затем `… 2`, …).
fn beside(c: &Connection, space: Id, path: &str, today: &str) -> Result<String, StoreError> {
    let mut k = 1;
    loop {
        let p = if k == 1 {
            format!("{path} (импорт {today})")
        } else {
            format!("{path} (импорт {today}) {k}")
        };
        if crate::nodes::live_by_path(c, space, &p)?.is_none() {
            return Ok(p);
        }
        k += 1;
    }
}

/// Ревизия заметки (`revisions`).
#[allow(clippy::too_many_arguments)]
fn put_note_rev(
    c: &Connection,
    id: Id,
    rev: i64,
    content: &str,
    a: &Author,
    message: Option<&str>,
    ts: &str,
) -> Result<(), StoreError> {
    c.prepare_cached(
        "INSERT INTO revisions (node_id, rev, content, content_hash, size, author_kind,
                                token_id, session_id, machine, message, ts, user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?
    .execute(params![
        id,
        rev,
        content,
        sha256_hex(content.as_bytes()),
        content.len() as i64,
        a.kind,
        a.token_id,
        a.session_id,
        a.machine,
        message,
        ts,
        a.user_id,
    ])?;
    Ok(())
}

fn note_attrib(ctx: &Ctx) -> Attribution {
    Attribution {
        author_kind: AuthorKind::System,
        ..ctx.attrib.clone()
    }
}

/// Вложения новой заметки (блобы уже на месте; документы — с id назначения).
fn put_attachments(
    c: &Connection,
    ctx: &Ctx,
    plan_intakes: &HashMap<(String, String), DocIntake>,
    node: Id,
    files: &[PlanFile],
) -> Result<(), StoreError> {
    for f in files {
        let mime = f
            .mime
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into());
        let size = i64::try_from(f.size).unwrap_or(i64::MAX);
        let at = f.created_at.map(fmt_ts).unwrap_or_else(|| ctx.now.clone());
        c.prepare_cached(
            "INSERT INTO blobs (sha256, size, mime, refcount, created_at)
             VALUES (?1, ?2, ?3, 0, ?4) ON CONFLICT (sha256) DO NOTHING",
        )?
        .execute(params![f.sha256, size, mime, at])?;
        let new = c
            .prepare_cached(
                "INSERT INTO attachments (node_id, blob_sha256, filename, mime, size, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (node_id, blob_sha256, filename) DO NOTHING",
            )?
            .execute(params![node, f.sha256, f.name, mime, size, at])?;
        let intake = plan_intakes
            .get(&(f.sha256.clone(), f.name.clone()))
            .copied()
            .unwrap_or(DocIntake::Unsupported);
        crate::attachments::ensure_doc_as(
            c, f.doc_to, node, &f.sha256, &f.name, &mime, intake, &ctx.now,
        )?;
        if new == 1 {
            c.prepare_cached("UPDATE blobs SET refcount = refcount + 1 WHERE sha256 = ?1")?
                .execute([&f.sha256])?;
        }
    }
    Ok(())
}

/// Заметка: создание, пропуск, конфликт или ревизия.
fn apply_note(
    c: &Connection,
    ctx: &Ctx,
    intakes: &HashMap<(String, String), DocIntake>,
    n: &PlanNote,
    rep: &mut ImportReport,
) -> Result<(), StoreError> {
    let mut content = n.content.clone();
    let mut demoted = false;
    if ctx.demote() {
        match demote_note(&content) {
            Ok(Some(t)) => {
                content = t;
                demoted = true;
            }
            Ok(None) => {}
            Err(m) => {
                rep.conflicts.push(Conflict {
                    id: n.src.map(|s| s.to_string()),
                    path: Some(n.path.clone()),
                    reason: format!("invalid: {m}"),
                });
                return Ok(());
            }
        }
    }
    let reason = if demoted {
        format!("{DEMOTE_REASON} ({})", ctx.reason)
    } else {
        ctx.reason.clone()
    };
    if let Some((id, path, hash)) = note_existing(c, ctx, n)? {
        if hash == n.content_hash {
            rep.skipped += 1;
            return Ok(());
        }
        if ctx.opts.on_conflict == OnConflict::Skip {
            rep.conflicts.push(Conflict {
                id: Some(id.to_string()),
                path: Some(path),
                reason: "changed".into(),
            });
            return Ok(());
        }
        let mut a = note_attrib(ctx);
        a.message = Some(reason);
        let node = load_node(c, id)?;
        let agent_l0 = n
            .l0_source
            .as_deref()
            .filter(|s| *s == "agent")
            .and(n.l0.as_deref());
        if let Err(e) = note_meta(&content, agent_l0).certainty() {
            rep.conflicts.push(Conflict {
                id: Some(id.to_string()),
                path: Some(path),
                reason: format!("invalid: {e}"),
            });
            return Ok(());
        }
        revise_note(c, node, &content, agent_l0, &a)?;
        c.prepare_cached("UPDATE nodes SET imported_from = ?2 WHERE id = ?1")?
            .execute(params![
                id,
                ctx.imported_from(&n.key(), n.rev, &n.content_hash)
            ])?;
        if path != n.path {
            let p = NodePath::parse(&n.path).map_err(|e| StoreError::BadInput(e.to_string()))?;
            if crate::nodes::live_by_path(c, ctx.space, &n.path)?.is_none() {
                crate::nodes::move_tx(c, id, &p, AuthorKind::System)?;
            } else {
                rep.warnings.push(format!(
                    "заметка {id}: путь «{}» занят — осталась по пути «{path}»",
                    n.path
                ));
            }
        }
        if demoted {
            rep.demoted_axioms += 1;
        }
        rep.revised += 1;
        secrets_warn(rep, &n.path, &content);
        return Ok(());
    }
    let mut path = n.path.clone();
    if let Some(other) = crate::nodes::live_by_path(c, ctx.space, &path)? {
        if ctx.opts.on_conflict == OnConflict::Skip {
            rep.conflicts.push(Conflict {
                id: Some(other.id.to_string()),
                path: Some(path),
                reason: "path-taken".into(),
            });
            return Ok(());
        }
        path = beside(c, ctx.space, &path, &ctx.today)?;
    }
    let np = match NodePath::parse(&path) {
        Ok(p) => p,
        Err(e) => {
            rep.conflicts.push(Conflict {
                id: None,
                path: Some(path),
                reason: format!("invalid: {e}"),
            });
            return Ok(());
        }
    };
    let agent_l0 = n
        .l0_source
        .as_deref()
        .filter(|s| *s == "agent")
        .and(n.l0.as_deref());
    let meta = note_meta(&content, agent_l0);
    let certainty = match meta.certainty() {
        Ok(x) => x,
        Err(e) => {
            rep.conflicts.push(Conflict {
                id: n.src.map(|s| s.to_string()),
                path: Some(path),
                reason: format!("invalid: {e}"),
            });
            return Ok(());
        }
    };
    let parent = match crate::nodes::ensure_ancestors(c, ctx.space, &np, &ctx.now) {
        Ok(p) => p,
        Err(StoreError::Conflict(m)) => {
            rep.conflicts.push(Conflict {
                id: n.src.map(|s| s.to_string()),
                path: Some(path),
                reason: format!("path-taken: {m}"),
            });
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    insert_note_row(c, ctx, n, &np, parent, &content, &meta, certainty)?;
    // ревизии: исходная история, затем ревизия импорта
    let mut rev = 0i64;
    for h in &n.history {
        rev += 1;
        put_note_rev(
            c,
            n.id,
            rev,
            &h.content,
            &h.author,
            h.message.as_deref(),
            &fmt_ts(h.ts),
        )?;
    }
    rev += 1;
    put_note_rev(
        c,
        n.id,
        rev,
        &content,
        &ctx.sys_author(),
        Some(&reason),
        &ctx.now,
    )?;
    c.prepare_cached("UPDATE nodes SET current_rev = ?2 WHERE id = ?1")?
        .execute(params![n.id, rev])?;
    crate::nodes::index_note(c, n.id, rev, &content, &meta, None, &ctx.now)?;
    crate::file_refs::sync_note(
        c,
        n.id,
        meta.doc.frontmatter.as_ref(),
        &note_attrib(ctx),
        &ctx.now,
    )?;
    crate::graph::tree_changed(c, ctx.space, &[n.id], &[])?;
    put_attachments(c, ctx, intakes, n.id, &n.attachments)?;
    if demoted {
        rep.demoted_axioms += 1;
    }
    rep.created += 1;
    secrets_warn(rep, &path, &content);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_note_row(
    c: &Connection,
    ctx: &Ctx,
    n: &PlanNote,
    np: &NodePath,
    parent: Option<Id>,
    content: &str,
    meta: &NoteMeta,
    certainty: Option<Certainty>,
) -> Result<(), StoreError> {
    let created = n.created_at.map(fmt_ts).unwrap_or_else(|| ctx.now.clone());
    let updated = n.updated_at.map(fmt_ts).unwrap_or_else(|| ctx.now.clone());
    c.execute(
        "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, current_rev,
                            content_hash, frontmatter, l0, l0_source, index_state,
                            created_at, updated_at, area, certainty, imported_from)
         VALUES (?1, ?2, ?3, 'note', ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            n.id,
            ctx.space,
            parent,
            np.name(),
            np.as_str(),
            sha256_hex(content.as_bytes()),
            meta.frontmatter.as_ref().map(|v| v.to_string()),
            meta.l0,
            meta.l0_source,
            crate::nodes::index_state(meta, ctx.space_kind),
            created,
            updated,
            ctx.attrib.area,
            certainty,
            ctx.imported_from(&n.key(), n.rev, &n.content_hash),
        ],
    )?;
    Ok(())
}

/// Новая ревизия заметки при `on_conflict=revision` (как правка: чанки, рёбра, ссылки, кэш).
fn revise_note(
    c: &Connection,
    node: crate::nodes::Node,
    content: &str,
    agent_l0: Option<&str>,
    a: &Attribution,
) -> Result<(), StoreError> {
    let hash = sha256_hex(content.as_bytes());
    if node.content_hash.as_deref() == Some(hash.as_str()) {
        return Ok(());
    }
    let now = fmt_ts(crate::time::now());
    let meta = note_meta(content, agent_l0);
    let certainty = meta.certainty()?;
    let kind = crate::nodes::space_kind(c, node.space_id)?;
    let rev = node.current_rev + 1;
    c.execute(
        "UPDATE nodes SET current_rev = ?2, content_hash = ?3, frontmatter = ?4, l0 = ?5,
                          l0_source = ?6, index_state = ?7, updated_at = ?8, certainty = ?9
         WHERE id = ?1",
        params![
            node.id,
            rev,
            hash,
            meta.frontmatter.as_ref().map(|v| v.to_string()),
            meta.l0,
            meta.l0_source,
            crate::nodes::index_state(&meta, kind),
            now,
            certainty,
        ],
    )?;
    crate::nodes::store_revision(
        c,
        node.id,
        rev,
        content,
        &hash,
        &meta,
        a,
        Some(&node.index_state),
        &now,
    )?;
    crate::cache::invalidate_revised(
        c,
        TargetRef {
            kind: TargetKind::Node,
            id: node.id,
        },
        rev,
        content,
        crate::cache::StaleKind::Changed,
    )?;
    Ok(())
}

fn secrets_warn(rep: &mut ImportReport, at: &str, text: &str) {
    let hits = zam_markdown::scan_secrets(text);
    if hits.is_empty() {
        return;
    }
    let lines: Vec<String> = hits
        .iter()
        .map(|h| format!("{} (строка {})", h.kind.as_str(), h.line))
        .collect();
    rep.warnings.push(format!(
        "secrets_detected: «{at}» — {}; в индекс значения уходят замаскированными, \
         храните секреты в Vault",
        lines.join(", ")
    ));
}

/// Папка: путь занят папкой — она и есть; иначе создаётся с id назначения.
fn apply_folder(c: &Connection, ctx: &Ctx, f: &PlanFolder) -> Result<(), StoreError> {
    let Ok(np) = NodePath::parse(&f.path) else {
        return Ok(());
    };
    let id = match crate::nodes::live_by_path(c, ctx.space, &f.path)? {
        Some(n) if n.kind == NodeKind::Folder => n.id,
        Some(_) => return Ok(()),
        None => {
            let parent = match crate::nodes::ensure_ancestors(c, ctx.space, &np, &ctx.now) {
                Ok(p) => p,
                Err(StoreError::Conflict(_)) => return Ok(()),
                Err(e) => return Err(e),
            };
            let busy = c
                .prepare_cached("SELECT 1 FROM nodes WHERE id = ?1")?
                .query_row([f.id], |_| Ok(()))
                .optional()?
                .is_some();
            let id = if busy { Id::new() } else { f.id };
            c.execute(
                "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, current_rev,
                                    index_state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'folder', ?4, ?5, 0, 'excluded', ?6, ?6)",
                params![id, ctx.space, parent, np.name(), np.as_str(), ctx.now],
            )?;
            crate::graph::tree_changed(c, ctx.space, &[id], &[])?;
            id
        }
    };
    if f.l0_source.as_deref() == Some("manual")
        && let Some(l0) = f.l0.as_deref().filter(|s| !s.trim().is_empty())
    {
        c.prepare_cached(
            "UPDATE nodes SET l0 = ?2, l0_source = 'manual' WHERE id = ?1 AND kind = 'folder'",
        )?
        .execute(params![id, l0])?;
    }
    Ok(())
}

/// Ревизия записи (`entry_revisions`).
#[allow(clippy::too_many_arguments)]
fn put_entry_rev(
    c: &Connection,
    id: Id,
    rev: i64,
    kind: EntryKind,
    f: &Fields,
    reason: Option<&str>,
    a: &Author,
    ts: &str,
) -> Result<(), StoreError> {
    c.prepare_cached(
        "INSERT INTO entry_revisions (entry_id, rev, title, body, data, tags, area, status,
                                      certainty, reason, ts, author_kind, user_id, token_id,
                                      session_id, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
    )?
    .execute(params![
        id,
        rev,
        f.title,
        f.body,
        content_data(kind, &f.data).to_string(),
        tags_json(&f.tags),
        f.area,
        f.status,
        f.certainty,
        reason,
        ts,
        a.kind,
        a.user_id,
        a.token_id,
        a.session_id,
        a.machine,
    ])?;
    Ok(())
}

/// Ссылки записи на файлы (как в архиве: решение, хэши, коммит).
fn put_refs(c: &Connection, id: Id, refs: &[PlanRef], now: &str) -> Result<(), StoreError> {
    c.prepare_cached("DELETE FROM file_refs WHERE owner_kind = 'entry' AND owner_id = ?1")?
        .execute([id])?;
    let mut st = c.prepare_cached(
        "INSERT OR IGNORE INTO file_refs (owner_kind, owner_id, solution_guid, path, symbol,
                                          content_hash, symbol_hash, git_commit, recorded_at,
                                          state)
         VALUES ('entry', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'ok')",
    )?;
    for r in refs.iter().take(crate::file_refs::REFS_MAX) {
        let Ok(path) = crate::file_refs::normalize_ref_path(&r.path) else {
            continue;
        };
        st.execute(params![
            id,
            r.solution,
            path,
            r.symbol,
            r.hash,
            r.symbol_hash,
            r.commit,
            now
        ])?;
    }
    Ok(())
}

/// Запись: создание, пропуск, конфликт или ревизия.
fn apply_entry(
    c: &Connection,
    ctx: &Ctx,
    e: &PlanEntry,
    rep: &mut ImportReport,
) -> Result<(), StoreError> {
    let conflict = |rep: &mut ImportReport, reason: String| {
        rep.conflicts.push(Conflict {
            id: Some(e.src.to_string()),
            path: None,
            reason,
        });
    };
    if e.kind == EntryKind::Report && !ctx.opts.with_reports {
        rep.warnings.push(format!(
            "отчёт «{}» пропущен: импорт отчётов — только из сессии WebUI",
            e.title
        ));
        return Ok(());
    }
    if e.kind == EntryKind::Report && ctx.space_kind != SpaceKind::Solution {
        conflict(rep, "report-space".into());
        return Ok(());
    }
    let mut data = e.data.clone();
    let mut status = e.status;
    let mut certainty = e.certainty;
    if !has_certainty(e.kind) {
        certainty = None;
    }
    // захваты: сессий старой машины нет
    if e.kind == EntryKind::Task
        && let Some(o) = data.as_object_mut()
    {
        o.remove("claimed_by");
    }
    let mut extra = Vec::new();
    // пространство задачи «начать» — пространство архива → пространство назначения
    if e.kind == EntryKind::Question
        && let Some(src) = ctx.source_space
        && let Some(o) = data.as_object_mut()
        && o.get("task_space").and_then(Value::as_str) == Some(src.to_string().as_str())
    {
        o.insert("task_space".into(), json!(ctx.space.to_string()));
    }
    // вопрос-предложение без своей идеи
    if e.kind == EntryKind::Question
        && status == Some(EntryStatus::Open)
        && let Some(idea) = data.get("idea").and_then(Value::as_str)
    {
        let alive = idea.parse::<Id>().ok().is_some_and(|i| {
            ctx.ideas.contains(&i)
                || c.prepare_cached(
                    "SELECT 1 FROM entries WHERE id = ?1 AND kind = 'idea' AND deleted_at IS NULL",
                )
                .and_then(|mut s| s.query_row([i], |_| Ok(())).optional())
                .ok()
                .flatten()
                .is_some()
        });
        if !alive {
            status = Some(EntryStatus::Obsolete);
            extra.push("вопрос-предложение без своей идеи — obsolete");
        }
    }
    let demoted = ctx.demote() && certainty == Some(Certainty::Axiom);
    if demoted {
        certainty = Some(Certainty::Theorem);
    }
    let mut reason = if demoted {
        format!("{DEMOTE_REASON} ({})", ctx.reason)
    } else {
        ctx.reason.clone()
    };
    for x in extra {
        reason.push_str("; ");
        reason.push_str(x);
    }
    let fields = Fields {
        title: e.title.clone(),
        body: e.body.clone(),
        data: data.clone(),
        tags: e.tags.clone(),
        area: e.area.clone(),
        status,
        certainty,
    };
    // та же запись: по id назначения в этом пространстве
    let existing: Option<Entry> = c
        .prepare_cached(
            "SELECT 1 FROM entries WHERE id = ?1 AND space_id = ?2 AND deleted_at IS NULL",
        )?
        .query_row(params![e.id, ctx.space], |_| Ok(()))
        .optional()?
        .map(|_| load_entry(c, e.id))
        .transpose()?;
    if let Some(cur) = existing {
        let imp: Option<ImportedFrom> = c
            .prepare_cached("SELECT imported_from FROM entries WHERE id = ?1")?
            .query_row([e.id], |r| r.get::<_, Option<String>>(0))?
            .and_then(|s| serde_json::from_str(&s).ok());
        let hash = imp
            .filter(|i| i.source_id == e.src.to_string())
            .map(|i| i.content_hash)
            .unwrap_or_else(|| entry_hash_of(&cur));
        if hash == e.content_hash {
            rep.skipped += 1;
            return Ok(());
        }
        if ctx.opts.on_conflict == OnConflict::Skip {
            conflict(rep, "changed".into());
            return Ok(());
        }
        if cur.kind == EntryKind::Report {
            conflict(rep, "report-immutable".into());
            return Ok(());
        }
        let a = note_attrib(ctx);
        save(c, &cur, &fields, Some(&reason), &a, &ctx.now)?;
        c.prepare_cached("UPDATE entries SET imported_from = ?2 WHERE id = ?1")?
            .execute(params![
                e.id,
                ctx.imported_from(&e.src.to_string(), e.rev, &e.content_hash)
            ])?;
        if e.kind == EntryKind::Insight
            && let Ok(ts) = check_triggers(e.triggers.clone())
        {
            replace_triggers(c, e.id, &ts, &ctx.now)?;
        }
        put_refs(c, e.id, &e.refs, &ctx.now)?;
        if e.kind == EntryKind::Term {
            crate::glossary::terms_written(c, ctx.space)?;
        }
        if demoted {
            rep.demoted_axioms += 1;
        }
        rep.revised += 1;
        secrets_warn(rep, &e.title, &format!("{}\n{}", e.title, e.body));
        return Ok(());
    }
    // проверки вида (как при записи): словарь и версия релиза
    if e.kind == EntryKind::Term {
        match crate::glossary::check_term(&e.title, &data) {
            Ok((title, td)) => {
                if let Err(err) =
                    crate::glossary::ensure_names_free(c, ctx.space, &title, &td, None)
                {
                    conflict(rep, format!("term-conflict: {err}"));
                    return Ok(());
                }
            }
            Err(m) => {
                conflict(rep, format!("invalid: {m}"));
                return Ok(());
            }
        }
    }
    if e.kind == EntryKind::Release
        && let Err(err) = ensure_release_version_free(c, ctx.space, &data, None)
    {
        conflict(rep, format!("release-version: {err}"));
        return Ok(());
    }
    let created = e.created_at.map(fmt_ts).unwrap_or_else(|| ctx.now.clone());
    let updated = e.updated_at.map(fmt_ts).unwrap_or_else(|| ctx.now.clone());
    let report = e.kind == EntryKind::Report;
    let cur_rev: i64 = if report {
        1
    } else {
        e.history.len() as i64 + 1
    };
    c.prepare_cached(
        "INSERT INTO entries (id, space_id, kind, title, title_norm, body, data, tags, area,
                              status, certainty, retract_reason, current_rev,
                              author_kind, user_id, token_id, session_id, machine, index_state,
                              created_at, updated_at, imported_from)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
    )?
    .execute(params![
        e.id,
        ctx.space,
        e.kind,
        fields.title,
        normalize_title(&fields.title),
        fields.body,
        fields.data.to_string(),
        tags_json(&fields.tags),
        fields.area,
        fields.status,
        fields.certainty,
        if report {
            None
        } else {
            e.retract_reason.clone()
        },
        cur_rev,
        e.author.kind,
        e.author.user_id,
        e.author.token_id,
        e.author.session_id,
        e.author.machine,
        entry_index_state(ctx.space_kind, e.kind).as_str(),
        created,
        updated,
        ctx.imported_from(&e.src.to_string(), e.rev, &e.content_hash),
    ])?;
    if report {
        // отчёт неизменяем: одна ревизия (исходная), файлы — с `created_at` отчёта
        let (a, ts) = match e.history.first() {
            Some(h) => (h.author.clone(), fmt_ts(h.ts)),
            None => (e.author.clone(), created.clone()),
        };
        put_entry_rev(c, e.id, 1, e.kind, &fields, None, &a, &ts)?;
        for f in &e.files {
            let size = i64::try_from(f.size).unwrap_or(i64::MAX);
            let mime = f
                .mime
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into());
            c.prepare_cached(
                "INSERT INTO blobs (sha256, size, mime, refcount, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4) ON CONFLICT (sha256) DO NOTHING",
            )?
            .execute(params![f.sha256, size, mime, created])?;
            c.prepare_cached(
                "INSERT INTO entry_attachments (entry_id, blob_sha256, filename, mime, size,
                                                created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![e.id, f.sha256, f.name, mime, size, created])?;
            c.prepare_cached("UPDATE blobs SET refcount = refcount + 1 WHERE sha256 = ?1")?
                .execute([&f.sha256])?;
        }
    } else {
        let mut rev = 0i64;
        for h in &e.history {
            rev += 1;
            let hf = Fields {
                title: h.title.clone(),
                body: h.body.clone(),
                data: h.data.clone(),
                tags: h.tags.clone(),
                area: h.area.clone(),
                status: h.status,
                certainty: h.certainty,
            };
            put_entry_rev(
                c,
                e.id,
                rev,
                e.kind,
                &hf,
                h.reason.as_deref(),
                &h.author,
                &fmt_ts(h.ts),
            )?;
        }
        put_entry_rev(
            c,
            e.id,
            rev + 1,
            e.kind,
            &fields,
            Some(&reason),
            &ctx.sys_author(),
            &ctx.now,
        )?;
        if e.kind == EntryKind::Insight && !e.triggers.is_empty() {
            match check_triggers(e.triggers.clone()) {
                Ok(ts) => replace_triggers(c, e.id, &ts, &ctx.now)?,
                Err(err) => rep
                    .warnings
                    .push(format!("запись «{}»: триггеры пропущены: {err}", e.title)),
            }
        }
        put_refs(c, e.id, &e.refs, &ctx.now)?;
        if e.kind == EntryKind::Term {
            crate::glossary::terms_written(c, ctx.space)?;
        }
    }
    index_entry_now(c, e.id, None, &ctx.now)?;
    if demoted {
        rep.demoted_axioms += 1;
    }
    rep.created += 1;
    secrets_warn(rep, &e.title, &format!("{}\n{}", e.title, e.body));
    Ok(())
}

/// Последний шаг: `supersedes`, связи идей и задач, явные связи архива.
fn apply_links(
    c: &Connection,
    ctx: &Ctx,
    entries: &[LinkInfo],
    links: &[PlanLink],
    rep: &mut ImportReport,
) -> Result<(), StoreError> {
    let a = note_attrib(ctx);
    let live_entry = |id: Id| -> Result<bool, StoreError> {
        Ok(
            c.prepare_cached("SELECT 1 FROM entries WHERE id = ?1 AND deleted_at IS NULL")?
                .query_row([id], |_| Ok(()))
                .optional()?
                .is_some(),
        )
    };
    for e in entries {
        if e.kind == EntryKind::Report || !live_entry(e.id)? {
            continue;
        }
        // столбцы замещения: цель жива в этой базе
        let set = |col: &str, v: Option<Id>| -> Result<(), StoreError> {
            let Some(t) = v.filter(|t| *t != e.id) else {
                return Ok(());
            };
            if live_entry(t)? {
                c.execute(
                    &format!("UPDATE entries SET {col} = ?2 WHERE id = ?1 AND kind <> 'report'"),
                    params![e.id, t],
                )?;
            }
            Ok(())
        };
        set("supersedes_id", e.supersedes)?;
        set("superseded_by_id", e.superseded_by)?;
        if let Some(old) = e.supersedes.filter(|o| *o != e.id)
            && live_entry(old)?
        {
            crate::graph::insert_supersedes(c, e.id, old, ctx.space, Some(&a), &ctx.now)?;
        }
        // системные связи идей (как у «начать» и «предложить»)
        let sys = |src: Id, kind: EdgeType, dst: Id| -> Result<(), StoreError> {
            if src == dst || !live_entry(src)? || !live_entry(dst)? {
                return Ok(());
            }
            let exists: bool = c
                .prepare_cached(
                    "SELECT EXISTS (SELECT 1 FROM edges WHERE src_kind = 'entry' AND src_id = ?1
                       AND dst_kind = 'entry' AND dst_id = ?2 AND type = ?3
                       AND deleted_at IS NULL)",
                )?
                .query_row(params![src, dst, kind], |r| r.get(0))?;
            if !exists {
                let dst_ref = TargetRef {
                    kind: TargetKind::Entry,
                    id: dst,
                };
                crate::graph::insert_entry_links(
                    c,
                    src,
                    &[(kind, dst_ref)],
                    &a,
                    &ctx.now,
                    crate::graph::EdgeOrigin::System,
                )?;
            }
            Ok(())
        };
        if e.kind == EntryKind::Idea {
            for t in &e.tasks {
                sys(*t, EdgeType::DerivedFrom, e.id)?;
            }
        }
        if e.kind == EntryKind::Question
            && let Some(i) = e.idea
        {
            sys(e.id, EdgeType::RelatesTo, i)?;
        }
    }
    for l in links {
        let e = NewEdge {
            src: l.src,
            dst: l.dst,
            kind: l.kind,
        };
        match crate::graph::add_explicit(c, e, &a, &ctx.now) {
            Ok(_) | Err(StoreError::Conflict(_)) => {}
            Err(StoreError::NotFound) => rep.links_skipped += 1,
            Err(StoreError::BadInput(m)) => {
                rep.links_skipped += 1;
                rep.warnings
                    .push(format!("связь {} → {} пропущена: {m}", l.src, l.dst));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Элемент пачки.
enum Item {
    Folder(PlanFolder),
    Note(Box<PlanNote>),
    Entry(Box<PlanEntry>),
}

/// Сводка записи для последней транзакции (связи, замещение): план к тому времени уже отдан
/// пачкам — тексты не держатся дважды.
struct LinkInfo {
    id: Id,
    kind: EntryKind,
    supersedes: Option<Id>,
    superseded_by: Option<Id>,
    /// Задачи идеи (`data.task_id`, `data.prev_task_ids`).
    tasks: Vec<Id>,
    /// Идея вопроса-предложения (`data.idea`).
    idea: Option<Id>,
}

impl LinkInfo {
    fn of(e: &PlanEntry) -> LinkInfo {
        let id_of =
            |v: Option<&Value>| v.and_then(Value::as_str).and_then(|s| s.parse::<Id>().ok());
        let mut tasks: Vec<Id> = id_of(e.data.get("task_id")).into_iter().collect();
        tasks.extend(
            e.data
                .get("prev_task_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|x| id_of(Some(x))),
        );
        LinkInfo {
            id: e.id,
            kind: e.kind,
            supersedes: e.supersedes,
            superseded_by: e.superseded_by,
            tasks,
            idea: id_of(e.data.get("idea")),
        }
    }
}

/// Маркер отката пробного импорта.
const DRY_RUN: &str = "\u{0}dry-run";

/// Записывает план в пространство `space` (см. модуль). `attrib` — атрибуция запроса (автор
/// ревизий импорта — `system`). `user` — пользователь базы (свой ли архив). План забирается по
/// значению: элементы уходят пачкам без копий.
pub async fn apply_plan(
    db: &Db,
    space: Id,
    user: Id,
    plan: Plan,
    opts: &ImportOptions,
    attrib: &Attribution,
) -> Result<ImportReport, ImportError> {
    let own = plan.is_own(user, opts.own);
    let src_user = match (plan.has_manifest, plan.source_user_id) {
        (true, Some(u)) => u.to_string(),
        _ => "папки .md".to_string(),
    };
    let reason = format!("импорт {} от {src_user}", plan.archive_fingerprint);
    let Plan {
        archive_fingerprint,
        folders,
        notes,
        entries,
        links,
        warnings,
        intakes,
        source_space,
        ..
    } = plan;
    let mut report = ImportReport {
        dry_run: opts.dry_run,
        archive_fingerprint: archive_fingerprint.clone(),
        own,
        warnings,
        ..ImportReport::default()
    };
    let ideas: HashSet<Id> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::Idea)
        .map(|e| e.id)
        .collect();
    let infos: Vec<LinkInfo> = entries.iter().map(LinkInfo::of).collect();
    let mut items: Vec<Item> = Vec::with_capacity(folders.len() + notes.len() + entries.len());
    items.extend(folders.into_iter().map(Item::Folder));
    items.extend(notes.into_iter().map(|n| Item::Note(Box::new(n))));
    items.extend(entries.into_iter().map(|e| Item::Entry(Box::new(e))));
    if let Some(p) = &opts.progress {
        p.total.store(items.len() as u64, Ordering::SeqCst);
    }
    let shared = Arc::new(Shared {
        space,
        opts: opts.clone(),
        own,
        attrib: attrib.clone(),
        reason,
        fp: archive_fingerprint,
        ideas,
        source_space,
        intakes,
        infos,
        links,
    });
    if opts.dry_run {
        // одна транзакция с откатом: отчёт тот же, записи нет (писатель базы занят на время
        // проверки)
        let cell = Arc::new(std::sync::Mutex::new(ImportReport::default()));
        let (sh, cl) = (shared.clone(), cell.clone());
        let res = db
            .write(move |c| {
                let mut r = ImportReport::default();
                sh.batch(c, &items, &mut r)?;
                sh.links(c, &mut r)?;
                *cl.lock().unwrap_or_else(|e| e.into_inner()) = r;
                Err::<(), _>(StoreError::Conflict(DRY_RUN.into()))
            })
            .await;
        match res {
            Err(StoreError::Conflict(m)) if m == DRY_RUN => {}
            Err(e) => return Err(e.into()),
            Ok(()) => {}
        }
        let r = std::mem::take(&mut *cell.lock().unwrap_or_else(|e| e.into_inner()));
        report.merge(r);
        if let Some(p) = &opts.progress {
            p.done
                .store(p.total.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        return Ok(report);
    }
    let size = if opts.batch == 0 { BATCH } else { opts.batch };
    let mut rest = items;
    let mut batches = 0usize;
    while !rest.is_empty() {
        if opts.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
            || opts.stop_after.is_some_and(|n| batches >= n)
        {
            return Err(ImportError::Stopped);
        }
        let tail = rest.split_off(rest.len().min(size));
        let batch = std::mem::replace(&mut rest, tail);
        let n = batch.len() as u64;
        let sh = shared.clone();
        let r = db
            .write(move |c| {
                let mut r = ImportReport::default();
                sh.batch(c, &batch, &mut r)?;
                Ok(r)
            })
            .await?;
        batches += 1;
        if let Some(p) = &opts.progress {
            p.written.store(true, Ordering::SeqCst);
            p.done.fetch_add(n, Ordering::SeqCst);
        }
        report.merge(r);
    }
    let sh = shared.clone();
    let r = db
        .write(move |c| {
            let mut r = ImportReport::default();
            sh.links(c, &mut r)?;
            Ok(r)
        })
        .await?;
    if let Some(p) = &opts.progress {
        p.written.store(true, Ordering::SeqCst);
    }
    report.merge(r);
    Ok(report)
}

/// Общее для пачек (передаётся в поток писателя).
struct Shared {
    space: Id,
    opts: ImportOptions,
    own: bool,
    attrib: Attribution,
    reason: String,
    fp: String,
    ideas: HashSet<Id>,
    source_space: Option<Id>,
    intakes: HashMap<(String, String), DocIntake>,
    infos: Vec<LinkInfo>,
    links: Vec<PlanLink>,
}

impl Shared {
    fn ctx<'a>(&'a self, c: &Connection) -> Result<Ctx<'a>, StoreError> {
        let now_ts = crate::time::now();
        Ok(Ctx {
            space: self.space,
            space_kind: crate::nodes::space_kind(c, self.space)?,
            opts: &self.opts,
            own: self.own,
            attrib: &self.attrib,
            reason: self.reason.clone(),
            fp: &self.fp,
            today: now_ts.to_zoned(jiff::tz::TimeZone::UTC).date().to_string(),
            now: fmt_ts(now_ts),
            ideas: &self.ideas,
            source_space: self.source_space,
        })
    }

    fn batch(
        &self,
        c: &Connection,
        items: &[Item],
        r: &mut ImportReport,
    ) -> Result<(), StoreError> {
        let ctx = self.ctx(c)?;
        for it in items {
            match it {
                Item::Folder(f) => apply_folder(c, &ctx, f)?,
                Item::Note(n) => apply_note(c, &ctx, &self.intakes, n, r)?,
                Item::Entry(e) => apply_entry(c, &ctx, e, r)?,
            }
        }
        Ok(())
    }

    fn links(&self, c: &Connection, r: &mut ImportReport) -> Result<(), StoreError> {
        let ctx = self.ctx(c)?;
        apply_links(c, &ctx, &self.infos, &self.links, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn zip_of(files: &[(&str, &[u8])]) -> ZipArchive<Cursor<Vec<u8>>> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (n, d) in files {
            z.start_file(*n, SimpleFileOptions::default()).unwrap();
            z.write_all(d).unwrap();
        }
        ZipArchive::new(Cursor::new(z.finish().unwrap().into_inner())).unwrap()
    }

    const LIM: ImportLimits = ImportLimits {
        max_bytes: 1 << 30,
        max_files: 1000,
        max_ratio: 100,
        max_text_bytes: 64 << 20,
    };

    fn deflated(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (n, d) in files {
            z.start_file(*n, o).unwrap();
            z.write_all(d).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    /// Бомба из мелких файлов: каждый меньше 1 МиБ, но сжат в тысячи раз.
    #[test]
    fn import_rejects_small_files_bomb() {
        let files: Vec<(String, Vec<u8>)> = (0..64)
            .map(|i| (format!("n{i}.md"), vec![0u8; (1 << 20) - 1]))
            .collect();
        let refs: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
        let z = deflated(&refs);
        let mut z = ZipArchive::new(Cursor::new(z)).unwrap();
        let e = validate_archive(&mut z, &LIM).unwrap_err().to_string();
        assert!(e.contains("сжат"), "{e}");
        // предел текстов: умеренно сжатые тексты больше max_text_bytes
        let txt: Vec<u8> = (0..400_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut z = ZipArchive::new(Cursor::new(deflated(&[("a.md", txt)]))).unwrap();
        let lim = ImportLimits {
            max_text_bytes: 1 << 20,
            ..LIM
        };
        let e = validate_archive(&mut z, &lim).unwrap_err().to_string();
        assert!(e.contains("max_text_bytes"), "{e}");
    }

    /// Бомба рядом с несжимаемым файлом: общий коэффициент мал, файловый — нет.
    #[test]
    fn import_rejects_mixed_bomb() {
        let noise: Vec<u8> = (0..(5u64 << 20))
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let st = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let df = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        z.start_file("_attachments/aa/n.bin", st).unwrap();
        z.write_all(&noise).unwrap();
        z.start_file("manifest.json", df).unwrap();
        z.write_all(b"{}").unwrap();
        z.start_file("b.md", df).unwrap();
        z.write_all(&vec![0u8; 900 << 10]).unwrap();
        let mut a = ZipArchive::new(Cursor::new(z.finish().unwrap().into_inner())).unwrap();
        let e = validate_archive(&mut a, &LIM).unwrap_err().to_string();
        assert!(e.contains("«b.md» сжат"), "{e}");
    }

    /// Много мелких файлов, каждый в пределе файлового коэффициента, — ловит общий.
    #[test]
    fn import_rejects_many_tiny_bombs() {
        let files: Vec<(String, Vec<u8>)> = (0..1000)
            .map(|i| (format!("n{i}.md"), vec![0u8; 60 << 10]))
            .collect();
        let refs: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
        let mut a = ZipArchive::new(Cursor::new(deflated(&refs))).unwrap();
        let e = validate_archive(&mut a, &LIM).unwrap_err().to_string();
        assert!(e.contains("архив сжат"), "{e}");
    }

    /// Подделанный размер в заголовках (меньше настоящего) — отказ, а не чтение сверх заявленного.
    #[test]
    fn import_rejects_forged_size_header() {
        let data: Vec<u8> = (0..50_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut z = deflated(&[("a.md", data)]);
        let patch = |z: &mut Vec<u8>, sig: &[u8], off: usize| {
            let p = z.windows(4).position(|w| w == sig).unwrap();
            z[p + off..p + off + 4].copy_from_slice(&100u32.to_le_bytes());
        };
        patch(&mut z, b"PK\x03\x04", 22);
        patch(&mut z, b"PK\x01\x02", 24);
        let mut a = ZipArchive::new(Cursor::new(z)).unwrap();
        assert!(matches!(
            validate_archive(&mut a, &LIM),
            Err(ImportError::Rejected(_))
        ));
    }

    #[test]
    fn import_ids_deterministic_per_recipient() {
        let (u, v, s) = (Id::new(), Id::new(), Id::new());
        assert_eq!(import_id(u, s, "a/b", "k"), import_id(u, s, "a/b", "k"));
        assert_ne!(import_id(u, s, "a/b", "k"), import_id(v, s, "a/b", "k"));
        assert_ne!(import_id(u, s, "a/b", "k"), import_id(u, s, "a/b", "k2"));
    }

    #[test]
    fn import_scan_refs_letters_and_bounds() {
        let id = Id::new();
        let chunk = "0123456789abcdef".repeat(2);
        let t = format!("k:{id}, n:{id}x d:{id} a:{chunk} z:{id} xk:{id} (w:{id})");
        let s = scan_refs(&t);
        let got: Vec<(&str, Option<Id>)> = s.iter().map(|x| (x.letter.as_str(), x.id)).collect();
        assert_eq!(
            got,
            vec![
                ("k", Some(id)),
                ("d", Some(id)),
                ("a", None),
                ("w", Some(id))
            ]
        );
        assert_eq!(&t[s[0].start..s[0].end], format!("k:{id}"));
    }

    #[test]
    fn import_rewrite_text_data_and_columns() {
        let (a, b, x) = (Id::new(), Id::new(), Id::new());
        let mut map = HashMap::new();
        map.insert(a, b);
        let mut w = Rewriter {
            map: &map,
            unresolved: Vec::new(),
        };
        assert_eq!(
            w.text(&format!("см. e:{a} и e:{x}")),
            format!("см. e:{b} и e:{x}")
        );
        let d = w.data(
            &json!({"task_id": a.to_string(), "prev_task_ids": [a.to_string()],
                                "links": [format!("i:{a}")], "blocked_by": format!("ждёт k:{a}")}),
        );
        assert_eq!(d["task_id"], json!(b.to_string()));
        assert_eq!(d["prev_task_ids"], json!([b.to_string()]));
        assert_eq!(d["links"], json!([format!("i:{b}")]));
        assert_eq!(d["blocked_by"], json!(format!("ждёт k:{b}")));
        assert_eq!(w.unresolved, vec![format!("e:{x}")]);
    }

    #[test]
    fn import_rejects_bad_names_and_symlinks() {
        for bad in [
            "../a.md", "/a.md", "a\\b.md", "C:/a.md", "a//b.md", "./a.md",
        ] {
            let mut z = zip_of(&[(bad, b"x")]);
            assert!(
                matches!(
                    validate_archive(&mut z, &LIM),
                    Err(ImportError::Rejected(_))
                ),
                "{bad}"
            );
        }
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        z.add_symlink("l.md", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        let mut z = ZipArchive::new(Cursor::new(z.finish().unwrap().into_inner())).unwrap();
        assert!(matches!(
            validate_archive(&mut z, &LIM),
            Err(ImportError::Rejected(_))
        ));
    }

    #[test]
    fn import_limits_ratio_files_bytes() {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        z.start_file(
            "b.md",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        z.write_all(&vec![0u8; 8 << 20]).unwrap();
        let mut z = ZipArchive::new(Cursor::new(z.finish().unwrap().into_inner())).unwrap();
        let e = validate_archive(&mut z, &LIM).unwrap_err().to_string();
        assert!(e.contains("сжат"), "{e}");
        let mut z = zip_of(&[("a.md", b"x"), ("b.md", b"y")]);
        let lim = ImportLimits {
            max_files: 1,
            ..LIM
        };
        assert!(validate_archive(&mut z, &lim).is_err());
        let mut z = zip_of(&[("a.md", &[b'x'; 100])]);
        let lim = ImportLimits {
            max_bytes: 50,
            ..LIM
        };
        assert!(validate_archive(&mut z, &lim).is_err());
    }

    #[test]
    fn import_folder_md_is_foreign_notes() {
        let mut z = zip_of(&[
            ("a.md", b"# A\n"),
            ("sub/b.md", b"# B\n"),
            ("pic.png", b"\x89"),
        ]);
        let p = validate_archive(&mut z, &LIM).unwrap();
        assert!(!p.has_manifest);
        let paths: Vec<&str> = p.notes.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(paths, ["a", "sub/b"]);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(!p.is_own(Id::new(), false));
        assert!(p.is_own(Id::new(), true));
    }

    #[test]
    fn import_demote_note_by_parser() {
        let d = |t: &str| demote_note(t);
        // обычный ключ и тело с той же строкой
        assert_eq!(
            d("---\ntags: [a]\ncertainty: axiom\n---\n# X\n\ncertainty: axiom\n").unwrap(),
            Some("---\ntags: [a]\ncertainty: theorem\n---\n# X\n\ncertainty: axiom\n".into())
        );
        // комментарий сохраняется
        assert_eq!(
            d("---\ncertainty: axiom # подтвердил\n---\nx").unwrap(),
            Some("---\ncertainty: theorem # подтвердил\n---\nx".into())
        );
        // блочный скаляр: строки продолжения уходят вместе с ключом
        let t = d("---\ncertainty: |\n  axiom\ntags: [a]\n---\nx")
            .unwrap()
            .unwrap();
        assert_eq!(t, "---\ncertainty: theorem\ntags: [a]\n---\nx");
        // повтор ключа: действует последний, переписаны оба
        let t = d("---\ncertainty: hypothesis\ncertainty: axiom\n---\nx")
            .unwrap()
            .unwrap();
        assert_eq!(
            note_meta(&t, None).certainty().unwrap(),
            Some(Certainty::Theorem)
        );
        assert!(!t.contains("axiom") && !t.contains("hypothesis"), "{t}");
        // в кавычках и с пробелом перед двоеточием
        assert!(d("---\ncertainty : \"axiom\"\n---\nx").unwrap().is_some());
        // не аксиома — без изменений
        assert_eq!(d("---\ncertainty: theorem\n---\nx").unwrap(), None);
        assert_eq!(d("# без frontmatter\ncertainty: axiom\n").unwrap(), None);
    }

    #[test]
    fn import_front_block() {
        let (m, body) = front("x", "---\nid: \"a\"\nrev: 2\n---\n---\nown: fm\n---\nтело").unwrap();
        assert_eq!(m["rev"], json!(2));
        assert_eq!(body, "---\nown: fm\n---\nтело");
        assert!(front("x", "без блока").is_err());
    }
}
