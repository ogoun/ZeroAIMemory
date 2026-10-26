//! Метаданные заметки (frontmatter, L0), чанки §8.2 и постановка заданий индексации
//! (§8.1–§8.3 ТЗ).
//!
//! Чанки строит `zam_markdown::chunk_note` с глобальными счётчиком токенов
//! ([`set_token_counter`], по умолчанию `ApproxCounter`) и порогами ([`set_chunk_params`],
//! по умолчанию `ChunkParams::default()`, сервер ставит из `[index]`): листы `chunk`, L1 секций
//! `section` и L1 заметки `note`. У заметки без текста тела — один `chunk`
//! с заголовком или именем, чтобы она находилась поиском по названию.
//! В FTS попадают только `chunk` и `note` (условие в триггерах миграции 0004).
//!
//! Сканер секретов (§7.6): нарезка идёт по тексту с замаскированными секретами
//! (`zam_markdown::scan_secrets`, значение → `‹secret›`), поэтому значения не попадают ни в
//! `chunks.text` (FTS), ни в эмбеддинги и Qdrant; ревизии и чтение заметки остаются
//! исходными. Автоматический L0 тоже маскируется.
//!
//! После пересборки чанков ставится задание outbox (в той же транзакции):
//! индексируемая заметка → `upsert`; Human и `index: false` → без задания
//! (`index_state = 'excluded'`), но заметка, бывшая индексируемой, получает `delete`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::Id;
use zam_markdown::{
    ApproxCounter, ChunkLevel, ChunkParams, Doc, TokenCounter, chunk_note, first_paragraph,
    mask_secrets, mask_text, parse, scan_secrets,
};

use super::{hex, revision_text};
use crate::embeddings::embed_key_of;
use crate::entries::Certainty;
use crate::jobs::{JobOp, PRIORITY_REINDEX, PRIORITY_WRITE, enqueue};
use crate::time::fmt_ts;
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

static TOKEN_COUNTER: OnceLock<RwLock<Arc<dyn TokenCounter>>> = OnceLock::new();

fn counter_cell() -> &'static RwLock<Arc<dyn TokenCounter>> {
    TOKEN_COUNTER.get_or_init(|| RwLock::new(Arc::new(ApproxCounter)))
}

/// Устанавливает счётчик токенов чанкера для всего процесса (например, токенизатор
/// bge-m3 после загрузки модели). Заметки, разбитые прежним точным счётчиком, остаются
/// как есть до перезаписи или reindex; разбитые оценкой (`is_exact() == false`) отмечены
/// в `meta` базы и перенарезаются в фоне при первом её открытии с точным счётчиком.
pub fn set_token_counter(tc: Arc<dyn TokenCounter>) {
    *counter_cell().write().unwrap_or_else(|e| e.into_inner()) = tc;
}

static CHUNK_PARAMS: OnceLock<RwLock<ChunkParams>> = OnceLock::new();

fn params_cell() -> &'static RwLock<ChunkParams> {
    CHUNK_PARAMS.get_or_init(|| RwLock::new(ChunkParams::default()))
}

/// Устанавливает пороги чанкера для всего процесса (`[index]` конфигурации сервера, §8.2).
/// Действует на заметки, записанные или перечанкованные после вызова. Базу, нарезанную
/// другими настройками (отпечаток [`chunker_fingerprint`] в `meta`), перенарезает разовая
/// фоновая задача при её открытии (задача 17 M3); вызывать до первого открытия баз.
pub fn set_chunk_params(p: ChunkParams) {
    *params_cell().write().unwrap_or_else(|e| e.into_inner()) = p;
}

/// Текущие пороги чанкера.
pub fn chunk_params() -> ChunkParams {
    *params_cell().read().unwrap_or_else(|e| e.into_inner())
}

static PREFIX_L0: AtomicBool = AtomicBool::new(false);

/// Эксперимент `[index] chunk_prefix_l0` (задача 17 M3): контекстный префикс листов заметки
/// дополняется L0 заметки (`… — <L0>`, не длиннее [`PREFIX_L0_MAX_TOKENS`]). Для всего процесса,
/// до первого открытия баз, как [`set_chunk_params`].
pub fn set_prefix_l0(on: bool) {
    PREFIX_L0.store(on, Ordering::Relaxed);
}

/// Включён ли L0 в префиксе листов заметок.
pub fn prefix_l0() -> bool {
    PREFIX_L0.load(Ordering::Relaxed)
}

/// Предел L0 в префиксе, токенов счётчика чанкера.
pub const PREFIX_L0_MAX_TOKENS: usize = 50;

/// Версия алгоритма нарезки: 2 — задача 17 M3 (лист не кончается заголовком и вводной фразой
/// на «:», H4–H6 в пути заголовков, выбор разреза по силе границы).
const CHUNKER_VERSION: u32 = 2;

/// Общая часть отпечатков нарезки: версия алгоритма, пороги и флаги `[index]`.
fn chunker_base() -> String {
    let p = chunk_params();
    format!(
        "v{CHUNKER_VERSION};{}/{}/{}/{}/{};lambda={};cohesion={};h4={}",
        p.target_min,
        p.target_max,
        p.hard_max,
        p.merge_below,
        p.overlap_sentences,
        p.boundary_lambda,
        u8::from(p.cohesion),
        u8::from(p.heading_path_h4),
    )
}

/// Отпечаток нарезки заметок процесса: версия алгоритма, пороги и флаги `[index]`, L0 в
/// префиксе и идентификатор токенизатора (`;tok=…`, всегда последним — см. [`same_chunker`]).
/// Хранится в `meta` базы (`chunker`): заметки базы, нарезанные с другим отпечатком,
/// перенарезаются при открытии (кэш эмбеддингов по `embed_key` — повторный инференс только
/// изменённых листов).
pub(crate) fn chunker_fingerprint() -> String {
    format!(
        "{};l0={};tok={}",
        chunker_base(),
        u8::from(prefix_l0()),
        token_counter().tokenizer_id()
    )
}

/// Отпечаток нарезки записей журналов (`meta.chunker_entries`): как [`chunker_fingerprint`],
/// но без L0 в префиксе — на записи флаг `chunk_prefix_l0` не влияет.
pub(crate) fn entry_chunker_fingerprint() -> String {
    format!("{};tok={}", chunker_base(), token_counter().tokenizer_id())
}

/// Совпадает ли отпечаток нарезки базы `stored` с текущим `current`. При счётчике-оценке
/// (`exact == false`: модель не установлена или токенизатор откатился на оценку)
/// идентификатор токенизатора не сравнивается: перенарезка оценкой базы, нарезанной точным
/// токенизатором, только ухудшила бы листы (как в M2). Отпечаток без `;tok=` (база до
/// финальной волны M3) при точном счётчике не совпадает — разовая перенарезка.
pub(crate) fn same_chunker(stored: Option<&str>, current: &str, exact: bool) -> bool {
    fn base(s: &str) -> &str {
        s.rsplit_once(";tok=").map_or(s, |(b, _)| b)
    }
    match stored {
        None => false,
        Some(s) if exact => s == current,
        Some(s) => base(s) == base(current),
    }
}

/// Текущий счётчик токенов.
pub(crate) fn token_counter() -> Arc<dyn TokenCounter> {
    counter_cell()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Текущий счётчик чанкера точный (токенизатор модели), а не оценка.
pub fn token_counter_exact() -> bool {
    token_counter().is_exact()
}

/// Число токенов `text` текущим счётчиком чанкера (бюджет выдачи поиска).
pub fn count_tokens(text: &str) -> usize {
    token_counter().count(text)
}

/// Предел длины автоматического L0, символов.
pub(crate) const L0_AUTO_MAX_CHARS: usize = 200;

/// Что извлечено из исходника заметки при записи.
#[derive(Debug)]
pub(crate) struct NoteMeta {
    pub doc: Doc,
    /// JSON для `nodes.frontmatter`.
    pub frontmatter: Option<serde_json::Value>,
    pub l0: Option<String>,
    pub l0_source: Option<&'static str>,
    /// `frontmatter.index != false`.
    pub index: bool,
    /// `certainty:` из frontmatter как в исходнике (`None` — поля нет); значение проверяет
    /// [`NoteMeta::certainty`].
    pub certainty_raw: Option<String>,
    /// Доказательство `evidence:` из frontmatter.
    pub evidence: Option<crate::certainty::Evidence>,
}

impl NoteMeta {
    /// Статус заметки; неизвестное значение `certainty:` → `BadInput` (при записи — 400).
    pub fn certainty(&self) -> Result<Option<Certainty>, StoreError> {
        match self.certainty_raw.as_deref() {
            None => Ok(None),
            Some(c) => Certainty::parse(c).map(Some).ok_or_else(|| {
                StoreError::BadInput(format!(
                    "frontmatter certainty: axiom, theorem или hypothesis, не «{c}»"
                ))
            }),
        }
    }

    /// Статус для чтения уже записанной заметки: неизвестное значение — статуса нет.
    pub fn certainty_lenient(&self) -> Option<Certainty> {
        self.certainty_raw.as_deref().and_then(Certainty::parse)
    }
}

/// Разбор заметки и выбор L0: `agent_l0` → `l0` из frontmatter → `description` →
/// первый абзац, обрезанный по границе слова.
pub(crate) fn note_meta(content: &str, agent_l0: Option<&str>) -> NoteMeta {
    let doc = parse(content);
    let fm = doc.frontmatter.as_ref();
    let frontmatter = fm.map(|f| {
        let mut m = serde_json::Map::new();
        m.insert("tags".into(), serde_json::json!(f.tags));
        if let Some(d) = &f.description {
            m.insert("description".into(), d.clone().into());
        }
        if let Some(l0) = &f.l0 {
            m.insert("l0".into(), l0.clone().into());
        }
        if let Some(i) = f.index {
            m.insert("index".into(), i.into());
        }
        serde_json::Value::Object(m)
    });
    let nonempty = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (l0, l0_source) = if let Some(s) = nonempty(agent_l0) {
        (Some(s), Some("agent"))
    } else if let Some(s) = nonempty(fm.and_then(|f| f.l0.as_deref())) {
        (Some(s), Some("manual"))
    } else if let Some(s) = nonempty(fm.and_then(|f| f.description.as_deref())) {
        (Some(s), Some("manual"))
    } else if let Some(s) = first_paragraph(content, &doc) {
        (Some(truncate_words(&s, L0_AUTO_MAX_CHARS)), Some("auto"))
    } else {
        (None, None)
    };
    // L0 попадает в payload индекса и выдачу: значения секретов в нём не нужны
    let l0 = l0.map(|s| mask_text(&s));
    let index = fm.and_then(|f| f.index) != Some(false);
    let certainty_raw = fm.and_then(|f| f.certainty.clone());
    let evidence = crate::certainty::Evidence::from_note(fm.and_then(|f| f.evidence.as_deref()));
    NoteMeta {
        doc,
        frontmatter,
        l0,
        l0_source,
        index,
        certainty_raw,
        evidence,
    }
}

/// Не длиннее `max` символов; если строку пришлось резать — по последней границе слова.
pub(crate) fn truncate_words(s: &str, max: usize) -> String {
    let Some((cut, _)) = s.char_indices().nth(max) else {
        return s.to_string();
    };
    let head = &s[..cut];
    let at_boundary = s[cut..].starts_with(char::is_whitespace);
    let head = if at_boundary {
        head
    } else {
        // слово разрезано: отбрасываем его хвост, если до него было хоть одно слово
        head.rfind(char::is_whitespace).map_or(head, |i| &head[..i])
    };
    head.trim_end().to_string()
}

/// `[<space>/<path>] <имя> > <heading_path> | <тег1>, <тег2>` (§8.2 п.6);
/// ` > …` опускается при пустом пути заголовков, ` | …` — без тегов.
pub(crate) fn context_prefix(
    space: &str,
    path: &str,
    name: &str,
    heading_path: &str,
    tags: &[String],
) -> String {
    let mut s = format!("[{space}/{path}] {name}");
    if !heading_path.is_empty() {
        s.push_str(" > ");
        s.push_str(heading_path);
    }
    if !tags.is_empty() {
        s.push_str(" | ");
        s.push_str(&tags.join(", "));
    }
    // имя, путь, заголовок записи и теги задаёт автор: секрет в них попадает в
    // `chunks_fts.context_prefix` и во вход эмбеддинга, поэтому префикс маскируется целиком (§7.6)
    mask_text(&s)
}

/// Префикс листа заметки: [`context_prefix`] и, если включён [`set_prefix_l0`], `" — " + L0`
/// заметки, обрезанный по словам до [`PREFIX_L0_MAX_TOKENS`] токенов. L0 берётся на момент
/// нарезки: экстрактивный L0 воркера (`set_auto_l0`) префикс не меняет — иначе каждая новая
/// сводка инвалидировала бы эмбеддинги заметки **[решение исполнителя]**.
pub(crate) fn note_prefix(
    space: &str,
    path: &str,
    name: &str,
    heading_path: &str,
    tags: &[String],
    l0: Option<&str>,
) -> String {
    let base = context_prefix(space, path, name, heading_path, tags);
    let l0 = l0.map(str::trim).filter(|s| !s.is_empty());
    match l0 {
        Some(l0) if prefix_l0() => {
            let tc = token_counter();
            let l0 = truncate_tokens(l0, PREFIX_L0_MAX_TOKENS, tc.as_ref());
            if l0.is_empty() {
                base
            } else {
                mask_text(&format!("{base} — {l0}"))
            }
        }
        _ => base,
    }
}

/// Префикс `s` по границе слова не длиннее `max` токенов счётчика `tc`.
fn truncate_tokens(s: &str, max: usize, tc: &dyn TokenCounter) -> String {
    if tc.count(s) <= max {
        return s.to_string();
    }
    let ends: Vec<usize> = s
        .char_indices()
        .filter(|&(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    // счётчик монотонен на префиксах: двоичный поиск по концам слов
    let (mut lo, mut hi) = (0usize, ends.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if tc.count(&s[..ends[mid - 1]]) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        String::new()
    } else {
        s[..ends[lo - 1]].trim_end().to_string()
    }
}

/// Теги для payload точек Qdrant: теги задаёт автор, значение секрета в теге маскируется
/// так же, как в `context_prefix` (§7.6; ревью задачи 11 M3).
pub(crate) fn mask_tags(tags: Vec<String>) -> Vec<String> {
    tags.into_iter().map(|t| mask_text(&t)).collect()
}

/// Чанк до вставки в БД.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkRow {
    pub level: ChunkLevel,
    pub heading_path: String,
    pub text: String,
    pub text_hash: String,
    pub token_count: i64,
}

/// Строка `chunks` нарезки заметки или записи журнала: пишется [`replace_chunks`].
pub(crate) struct ChunkInsert {
    pub id: String,
    pub rev: i64,
    pub level: &'static str,
    pub heading_path: String,
    pub ordinal: i64,
    pub text: String,
    pub context_prefix: String,
    pub token_count: i64,
    pub text_hash: String,
    pub embed_key: String,
}

impl ChunkInsert {
    /// Совпадает ли строка со строкой `chunks`, прочитанной [`replace_chunks`] (все
    /// записываемые столбцы, кроме владельца).
    fn same_as(&self, r: &rusqlite::Row<'_>) -> rusqlite::Result<bool> {
        Ok(r.get_ref(0)?.as_str()? == self.id
            && r.get::<_, i64>(1)? == self.rev
            && r.get_ref(2)?.as_str()? == self.level
            && r.get_ref(3)?.as_str()? == self.heading_path
            && r.get::<_, i64>(4)? == self.ordinal
            && r.get_ref(5)?.as_str()? == self.text
            && r.get_ref(6)?.as_str()? == self.context_prefix
            && r.get::<_, i64>(7)? == self.token_count
            && r.get_ref(8)?.as_str()? == self.text_hash
            && r.get::<_, Option<String>>(9)?.as_deref() == Some(self.embed_key.as_str()))
    }
}

/// Заменяет чанки владельца (`col` — `node_id` или `entry_id`) строками `rows`, если нарезка
/// отличается от записанной (любой столбец, включая текст); совпадающая не переписывается
/// (FTS и `rowid` не трогаются). `true` — чанки переписаны. В транзакции писателя.
pub(crate) fn replace_chunks(
    c: &Connection,
    col: &'static str,
    owner: Id,
    rows: &[ChunkInsert],
) -> Result<bool, StoreError> {
    let same = {
        let mut st = c.prepare_cached(&format!(
            "SELECT id, rev, level, heading_path, ordinal, text, context_prefix, token_count,
                    text_hash, embed_key
             FROM chunks WHERE {col} = ?1 ORDER BY ordinal"
        ))?;
        let mut stored = st.query([owner])?;
        let mut same = true;
        for want in rows {
            match stored.next()? {
                Some(r) if want.same_as(r)? => {}
                _ => {
                    same = false;
                    break;
                }
            }
        }
        same && stored.next()?.is_none()
    };
    if same {
        return Ok(false);
    }
    c.prepare_cached(&format!("DELETE FROM chunks WHERE {col} = ?1"))?
        .execute([owner])?;
    let mut st = c.prepare_cached(&format!(
        "INSERT INTO chunks (id, {col}, rev, level, heading_path, ordinal, text,
                             context_prefix, token_count, text_hash, embed_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
    ))?;
    for r in rows {
        st.execute(params![
            r.id,
            owner,
            r.rev,
            r.level,
            r.heading_path,
            r.ordinal,
            r.text,
            r.context_prefix,
            r.token_count,
            r.text_hash,
            r.embed_key,
        ])?;
    }
    Ok(true)
}

fn text_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Чанки заметки по §8.2 в порядке `chunk_note` (листы, L1 секций, L1 заметки);
/// у заметки без листов — один `chunk` из [`title_chunk`].
pub(crate) fn note_chunks(
    src: &str,
    doc: &Doc,
    name: &str,
    tc: &dyn TokenCounter,
) -> Vec<ChunkRow> {
    // секреты маскируются до нарезки: границы чанков и заголовки считаются по тексту
    // без значений (индексируется только он); без секретов — обычный путь без повторного разбора
    let hits = scan_secrets(src);
    let masked_src;
    let masked_doc;
    let (src, doc) = if hits.is_empty() {
        (src, doc)
    } else {
        masked_src = mask_secrets(src, &hits);
        masked_doc = parse(&masked_src);
        (masked_src.as_str(), &masked_doc)
    };
    let drafts = chunk_note(src, doc, &chunk_params(), tc);
    let has_leaf = drafts.iter().any(|d| d.level == ChunkLevel::Chunk);
    let mut out: Vec<ChunkRow> = Vec::with_capacity(drafts.len() + 1);
    if !has_leaf {
        out.push(title_chunk(doc, name, tc));
    }
    out.extend(drafts.into_iter().map(|d| ChunkRow {
        level: d.level,
        text_hash: text_hash(&d.text),
        token_count: d.token_count as i64,
        heading_path: d.heading_path,
        text: d.text,
    }));
    out
}

/// Чанк заметки без текста тела (например, только `# Имя` из WebUI):
/// путь и текст — первый заголовок, а без заголовков — имя узла.
pub(crate) fn title_chunk(doc: &Doc, name: &str, tc: &dyn TokenCounter) -> ChunkRow {
    let (heading_path, text) = match doc.sections.iter().find(|s| s.level > 0) {
        Some(s) => (
            s.path.join(" > "),
            s.path.last().cloned().unwrap_or_default(),
        ),
        None => (String::new(), name.to_string()),
    };
    let text = if text.trim().is_empty() {
        name.to_string()
    } else {
        text
    };
    ChunkRow {
        level: ChunkLevel::Chunk,
        heading_path,
        text_hash: text_hash(&text),
        token_count: tc.count(&text) as i64,
        text,
    }
}

/// Детерминированный id чанка: первые 16 байт blake3(node ‖ ordinal ‖ text_hash), hex.
pub(crate) fn chunk_id(node: Id, ordinal: i64, text_hash: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(node.as_uuid().as_bytes());
    h.update(&ordinal.to_le_bytes());
    h.update(text_hash.as_bytes());
    hex(&h.finalize().as_bytes()[..16])
}

/// Теги из JSON `nodes.frontmatter` (тот же список, что попадает в чанки при записи).
pub(crate) fn frontmatter_tags(json: &str) -> Result<Vec<String>, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Fm {
        #[serde(default)]
        tags: Vec<String>,
    }
    Ok(serde_json::from_str::<Fm>(json)?.tags)
}

/// Вид пространства.
pub(crate) fn space_kind(c: &Connection, space_id: Id) -> Result<SpaceKind, StoreError> {
    Ok(
        c.query_row("SELECT kind FROM spaces WHERE id = ?1", [space_id], |r| {
            r.get(0)
        })?,
    )
}

/// `excluded` — Human (никогда не векторизуется) или `index: false`; иначе `pending`.
pub(crate) fn index_state(meta: &NoteMeta, kind: SpaceKind) -> &'static str {
    if meta.index && kind != SpaceKind::Human {
        "pending"
    } else {
        "excluded"
    }
}

/// Пересобирает чанки заметки `node_id` по ревизии `rev`: при отличии от записанных старые
/// удаляются (триггеры убирают их из FTS), новые вставляются (и при `index: false`:
/// чанки и FTS остаются, исключение — только из векторного индекса, §6.4); совпадающая
/// нарезка не переписывается ([`replace_chunks`]).
/// Вызывается внутри транзакции записи. `true` — чанки переписаны.
pub(crate) fn rebuild_chunks(
    c: &Connection,
    node_id: Id,
    rev: i64,
    content: &str,
    meta: &NoteMeta,
) -> Result<bool, StoreError> {
    // L0 — записанный в узле (агентский, ручной или авто): строка узла обновляется до нарезки,
    // а перенарезка не знает агентского L0 из запроса записи
    let (space, path, name, l0): (String, String, String, Option<String>) = c.query_row(
        "SELECT s.name, n.path, n.name, n.l0 FROM nodes n JOIN spaces s ON s.id = n.space_id
         WHERE n.id = ?1",
        [node_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let tags = meta
        .doc
        .frontmatter
        .as_ref()
        .map_or(&[][..], |f| &f.tags[..]);
    let tc = token_counter();
    let rows: Vec<ChunkInsert> = note_chunks(content, &meta.doc, &name, tc.as_ref())
        .into_iter()
        .enumerate()
        .map(|(ordinal, ch)| {
            let ordinal = ordinal as i64;
            let prefix = note_prefix(&space, &path, &name, &ch.heading_path, tags, l0.as_deref());
            ChunkInsert {
                id: chunk_id(node_id, ordinal, &ch.text_hash),
                rev,
                level: ch.level.as_str(),
                ordinal,
                embed_key: embed_key_of(&prefix, &ch.text),
                context_prefix: prefix,
                heading_path: ch.heading_path,
                text: ch.text,
                token_count: ch.token_count,
                text_hash: ch.text_hash,
            }
        })
        .collect();
    let changed = replace_chunks(c, "node_id", node_id, &rows)?;
    // после нарезки: счётчик мог откатиться на оценку посреди заметки
    if !tc.is_exact() {
        c.prepare_cached("INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')")?
            .execute([crate::user_db::CHUNKED_APPROX])?;
    }
    Ok(changed)
}

/// Строк в одной транзакции дозаполнения `embed_key`.
const BACKFILL_BATCH: i64 = 500;

/// Дозаполняет `chunks.embed_key` у строк, созданных до миграции v5 (формула ключа —
/// только в Rust, SQL-миграция её не посчитает). Пакетами по [`BACKFILL_BATCH`] строк
/// в отдельных транзакциях; повторный вызов безопасен. Возвращает число строк.
pub(crate) async fn backfill_embed_keys(db: &Db) -> Result<u64, StoreError> {
    let mut total = 0u64;
    loop {
        let n = db
            .write(|tx| {
                let rows: Vec<(i64, String, String)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT seq, context_prefix, text FROM chunks
                         WHERE embed_key IS NULL LIMIT ?1",
                    )?;
                    st.query_map([BACKFILL_BATCH], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect::<Result<_, _>>()?
                };
                let mut upd =
                    tx.prepare_cached("UPDATE chunks SET embed_key = ?2 WHERE seq = ?1")?;
                for (seq, prefix, text) in &rows {
                    upd.execute(params![seq, embed_key_of(prefix, text)])?;
                }
                Ok(rows.len() as u64)
            })
            .await?;
        total += n;
        if n < BACKFILL_BATCH as u64 {
            break;
        }
    }
    if total > 0 {
        tracing::info!(chunks = total, "ключи кэша эмбеддингов дозаполнены (v5)");
    }
    Ok(total)
}

/// Ключ `meta`: `nodes.certainty` ещё не дозаполнен из frontmatter (миграция 0009).
pub(crate) const CERTAINTY_BACKFILL: &str = "certainty_backfill";

/// Дозаполняет `nodes.certainty` всех заметок — живых и удалённых (столбец описывает текущую
/// ревизию, восстановление его не пересчитывает) — из frontmatter текущей ревизии (миграция
/// 0009 добавила столбец пустым: разбор Markdown SQL не посчитает).
///
/// `axiom` ставит только человек: до задачи 9 агент мог записать `certainty: axiom` в
/// frontmatter, поэтому `axiom` сохраняется, только если текущая ревизия записана с
/// `author_kind = 'human'`, иначе статус понижается до `hypothesis` (счётчик понижений — в лог)
/// **[решение исполнителя]**. Текст заметки не меняется: агент, правящий такую заметку, должен
/// убрать `axiom` из frontmatter (перевод в `axiom` — 403).
///
/// Живым индексируемым заметкам со статусом ставится `upsert` с приоритетом reindex (статус
/// попадает в payload Qdrant). Пакетами по [`BACKFILL_BATCH`] заметок; флаг
/// `meta.certainty_backfill` снимается после последнего пакета, прерванное дозаполнение
/// повторится при следующем открытии (оно идемпотентно). Возвращает число заметок со статусом.
pub(crate) async fn backfill_certainty(db: &Db) -> Result<u64, StoreError> {
    let pending = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT 1 FROM meta WHERE key = ?1",
                [CERTAINTY_BACKFILL],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })
        .await?;
    if !pending {
        return Ok(0);
    }
    let (mut total, mut demoted_total, mut after) = (0u64, 0u64, None::<Id>);
    loop {
        let cursor = after;
        let (n, found, demoted, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, i64, String)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT n.id, n.current_rev, r.author_kind
                         FROM nodes n JOIN revisions r
                              ON r.node_id = n.id AND r.rev = n.current_rev
                         WHERE n.kind = 'note' AND (?1 IS NULL OR n.id > ?1)
                         ORDER BY n.id LIMIT ?2",
                    )?;
                    st.query_map(params![cursor, BACKFILL_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<Result<_, _>>()?
                };
                let (mut found, mut demoted) = (0u64, 0u64);
                for (id, rev, author) in &batch {
                    let (id, rev) = (*id, *rev);
                    let content = revision_text(tx, id, rev)?;
                    let Some(mut c) = note_meta(&content, None).certainty_lenient() else {
                        continue;
                    };
                    if c == Certainty::Axiom && author != "human" {
                        c = Certainty::Hypothesis;
                        demoted += 1;
                    }
                    found += 1;
                    tx.prepare_cached("UPDATE nodes SET certainty = ?2 WHERE id = ?1")?
                        .execute(params![id, c])?;
                    let state: Option<i64> = tx
                        .prepare_cached(
                            "UPDATE nodes SET index_state = 'pending'
                             WHERE id = ?1 AND index_state <> 'excluded'
                               AND deleted_at IS NULL
                             RETURNING current_rev",
                        )?
                        .query_row([id], |r| r.get(0))
                        .optional()?;
                    if let Some(rev) = state {
                        enqueue(tx, id, rev, JobOp::Upsert, PRIORITY_REINDEX, &now)?;
                    }
                }
                let last = batch.last().map(|b| b.0);
                if (batch.len() as i64) < BACKFILL_BATCH {
                    tx.execute("DELETE FROM meta WHERE key = ?1", [CERTAINTY_BACKFILL])?;
                }
                Ok((batch.len() as i64, found, demoted, last))
            })
            .await?;
        total += found;
        demoted_total += demoted;
        match last {
            Some(l) if n == BACKFILL_BATCH => after = Some(l),
            _ => break,
        }
    }
    if demoted_total > 0 {
        tracing::info!(
            notes = demoted_total,
            "certainty: axiom заметок не от человека понижен до hypothesis (v9)"
        );
    }
    if total > 0 {
        tracing::info!(
            notes = total,
            "certainty заметок дозаполнен из frontmatter (v9)"
        );
    }
    Ok(total)
}

/// Ставит задание индексации по уже записанному `nodes.index_state` узла.
/// `prev_state` — состояние до записи (`None` — узел создан сейчас).
/// Возвращает поставленную операцию.
pub(crate) fn enqueue_for_note(
    c: &Connection,
    node_id: Id,
    rev: i64,
    prev_state: Option<&str>,
    priority: i64,
    now: &str,
) -> Result<Option<JobOp>, StoreError> {
    let (state, kind): (String, SpaceKind) = c.query_row(
        "SELECT n.index_state, s.kind FROM nodes n JOIN spaces s ON s.id = n.space_id
         WHERE n.id = ?1",
        [node_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let op = if state != "excluded" {
        Some(JobOp::Upsert)
    } else if kind != SpaceKind::Human && prev_state.is_some_and(|p| p != "excluded") {
        // была индексируемой: точки узла убираются из Qdrant
        Some(JobOp::Delete)
    } else {
        None
    };
    if let Some(op) = op {
        enqueue(c, node_id, rev, op, priority, now)?;
    }
    Ok(op)
}

/// Пересборка чанков, выведенных рёбер графа (`crate::graph::rebuild_derived`) и задание после
/// записи заметки (создание, правка, откат, восстановление) в транзакции писателя.
pub(crate) fn index_note(
    c: &Connection,
    node_id: Id,
    rev: i64,
    content: &str,
    meta: &NoteMeta,
    prev_state: Option<&str>,
    now: &str,
) -> Result<(), StoreError> {
    rebuild_chunks(c, node_id, rev, content, meta)?;
    crate::graph::rebuild_derived(c, node_id, &meta.doc, now)?;
    enqueue_for_note(c, node_id, rev, prev_state, PRIORITY_WRITE, now)?;
    Ok(())
}

/// В записанных чанках владельца (`col` — `node_id` или `entry_id`) есть секрет в
/// `context_prefix` (имя, путь, заголовок записи, теги): сканер маскирует префикс при записи,
/// а чанки старых баз хранят его как есть.
pub(crate) fn stored_prefix_hit(c: &Connection, col: &str, id: Id) -> Result<bool, StoreError> {
    let mut st = c.prepare_cached(&format!(
        "SELECT DISTINCT context_prefix FROM chunks WHERE {col} = ?1"
    ))?;
    let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
    for p in rows {
        if !scan_secrets(&p?).is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Разовая перенарезка заметок с секретами (миграция на сканер §7.6).
///
/// До задачи 11 M3 значения секретов попадали в `chunks.text` (и FTS) и, после индексации, в
/// эмбеддинги и Qdrant. Для каждой живой заметки, текущий текст которой (или L0) содержит
/// секреты, чанки пересобираются из замаскированного текста, L0 маскируется, ставится
/// `upsert` с приоритетом `priority` (кэш эмбеддингов ключуется по тексту: новый текст — новый
/// ключ, старый вектор со значением остаётся в `embeddings` неиспользуемым до очистки кэша).
/// Заметки без секретов не трогаются. Пакетами по [`RECHUNK_BATCH`] заметок; идемпотентно.
/// Возвращает число перенарезанных заметок. В лог — только счётчики.
pub(crate) async fn rescan_secrets(db: &Db, priority: i64) -> Result<u64, StoreError> {
    let (mut total, mut after) = (0u64, None::<Id>);
    loop {
        let cursor = after;
        let (n, found, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, i64, String, Option<String>)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT id, current_rev, index_state, l0 FROM nodes
                         WHERE kind = 'note' AND deleted_at IS NULL
                           AND (?1 IS NULL OR id > ?1)
                         ORDER BY id LIMIT ?2",
                    )?;
                    st.query_map(params![cursor, RECHUNK_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<_, _>>()?
                };
                let mut found = 0u64;
                for (id, rev, state, l0) in &batch {
                    let content = revision_text(tx, *id, *rev)?;
                    // секрет в тексте или в уже записанных префиксах чанков (путь, имя, теги)
                    let text_hit = !scan_secrets(&content).is_empty()
                        || stored_prefix_hit(tx, "node_id", *id)?;
                    let masked_l0 = l0
                        .as_deref()
                        .map(mask_text)
                        .filter(|m| Some(m.as_str()) != l0.as_deref());
                    if !text_hit && masked_l0.is_none() {
                        continue;
                    }
                    found += 1;
                    if let Some(m) = masked_l0 {
                        tx.prepare_cached("UPDATE nodes SET l0 = ?2 WHERE id = ?1")?
                            .execute(params![id, m])?;
                    }
                    if text_hit {
                        let meta = note_meta(&content, None);
                        rebuild_chunks(tx, *id, *rev, &content, &meta)?;
                    }
                    enqueue_for_note(tx, *id, *rev, Some(state), priority, &now)?;
                }
                Ok((batch.len() as i64, found, batch.last().map(|b| b.0)))
            })
            .await?;
        total += found;
        match last {
            Some(l) if n == RECHUNK_BATCH => after = Some(l),
            _ => break,
        }
    }
    if total > 0 {
        tracing::info!(
            notes = total,
            "секреты заметок: чанки перенарезаны без значений"
        );
    }
    Ok(total)
}

/// Заметок в одной транзакции перечанковки: писатель не занят надолго.
const RECHUNK_BATCH: i64 = 200;

/// Перечанковывает живые заметки (`space = None` — всех пространств) текущим
/// счётчиком токенов, пересчитывает `index_state` и ставит задания с `priority`.
/// `only_changed` (перенарезка по отпечатку при открытии базы): проиндексированная заметка,
/// нарезка которой не изменилась, не получает ни задания, ни нового `index_state` — её точки в
/// Qdrant уже совпадают с чанками; без флага (reindex, пересборка коллекции) `upsert` ставится
/// каждой индексируемой.
/// L0 и прочие поля узла не меняются. Пакетами по [`RECHUNK_BATCH`] заметок
/// в отдельных транзакциях; повторный запуск безопасен. Возвращает
/// `(заметок, заданий upsert)`. В лог — только счётчики.
pub(crate) async fn rechunk_notes(
    db: &Db,
    space: Option<Id>,
    priority: i64,
    only_changed: bool,
) -> Result<(u64, u64), StoreError> {
    let (mut notes, mut upserts) = (0u64, 0u64);
    let mut after: Option<Id> = None;
    loop {
        let cursor = after;
        let (n, u, last) = db
            .write(move |tx| {
                let now = fmt_ts(Timestamp::now());
                let batch: Vec<(Id, i64, String, SpaceKind)> = {
                    let mut st = tx.prepare_cached(
                        "SELECT n.id, n.current_rev, n.index_state, s.kind
                         FROM nodes n JOIN spaces s ON s.id = n.space_id
                         WHERE n.kind = 'note' AND n.deleted_at IS NULL
                           AND (?1 IS NULL OR n.space_id = ?1)
                           AND (?2 IS NULL OR n.id > ?2)
                         ORDER BY n.id
                         LIMIT ?3",
                    )?;
                    st.query_map(params![space, cursor, RECHUNK_BATCH], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<_, _>>()?
                };
                let mut upserts = 0u64;
                for &(id, rev, ref prev, kind) in &batch {
                    let content = revision_text(tx, id, rev)?;
                    let meta = note_meta(&content, None);
                    let state = index_state(&meta, kind);
                    let changed = rebuild_chunks(tx, id, rev, &content, &meta)?;
                    if only_changed && !changed && prev == "indexed" && state != "excluded" {
                        continue;
                    }
                    tx.prepare_cached("UPDATE nodes SET index_state = ?2 WHERE id = ?1")?
                        .execute(params![id, state])?;
                    let op = enqueue_for_note(tx, id, rev, Some(prev), priority, &now)?;
                    upserts += u64::from(op == Some(JobOp::Upsert));
                }
                let last = batch.last().map(|b| b.0);
                Ok((batch.len() as u64, upserts, last))
            })
            .await?;
        notes += n;
        upserts += u;
        if n > 0 {
            tracing::debug!(notes, upserts, "перечанковка: обработан пакет заметок");
        }
        match last {
            Some(l) if n as i64 == RECHUNK_BATCH => after = Some(l),
            _ => break,
        }
    }
    Ok((notes, upserts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_at_word_boundary() {
        assert_eq!(truncate_words("короткая строка", 200), "короткая строка");
        assert_eq!(truncate_words("ab cd ef", 5), "ab cd");
        assert_eq!(truncate_words("ab cd ef", 4), "ab");
        assert_eq!(truncate_words("ab cd ef", 6), "ab cd");
        assert_eq!(truncate_words("abcdef", 3), "abc");
        let s = "слово ".repeat(60);
        let t = truncate_words(s.trim(), 200);
        assert!(t.chars().count() <= 200 && t.ends_with("слово"));
    }

    #[test]
    fn levels_and_title_chunk() {
        let src = "intro text here.
# A

first sentence. second one.
## B

b text.
";
        let doc = parse(src);
        let ch = note_chunks(src, &doc, "n", &ApproxCounter);
        assert_eq!(ch[0].level, ChunkLevel::Chunk);
        assert!(
            ch.iter()
                .all(|c| c.token_count > 0 && c.text_hash.len() == 64)
        );
        assert!(ch.iter().any(|c| c.level == ChunkLevel::Note));

        let src = "# Только заголовок
";
        let doc = parse(src);
        let ch = note_chunks(src, &doc, "n", &ApproxCounter);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].level, ChunkLevel::Chunk);
        assert_eq!(ch[0].text, "Только заголовок");
        assert_eq!(
            note_chunks("", &parse(""), "имя", &ApproxCounter)[0].text,
            "имя"
        );
    }

    #[test]
    fn same_chunker_ignores_tokenizer_only_for_estimate() {
        let cur = "v2;a;l0=0;tok=sha256:b";
        assert!(same_chunker(Some(cur), cur, true));
        assert!(!same_chunker(None, cur, true));
        assert!(!same_chunker(None, cur, false));
        let other_tok = "v2;a;l0=0;tok=sha256:c";
        assert!(!same_chunker(Some(other_tok), cur, true));
        assert!(
            same_chunker(Some(other_tok), cur, false),
            "оценка: токенизатор не сравнивается"
        );
        let legacy = "v2;a;l0=0";
        assert!(
            !same_chunker(Some(legacy), cur, true),
            "база до токенизатора в отпечатке"
        );
        assert!(same_chunker(Some(legacy), cur, false));
        assert!(!same_chunker(Some("v2;a;l0=1;tok=sha256:b"), cur, false));
    }

    #[test]
    fn chunk_id_is_deterministic() {
        let n = Id::new();
        let a = chunk_id(n, 0, "h");
        assert_eq!(a, chunk_id(n, 0, "h"));
        assert_eq!(a.len(), 32);
        assert_ne!(a, chunk_id(n, 1, "h"));
        assert_ne!(a, chunk_id(Id::new(), 0, "h"));
    }

    #[test]
    fn truncate_tokens_by_words() {
        let tc = ApproxCounter;
        assert_eq!(truncate_tokens("коротко", 50, &tc), "коротко");
        let long = "слово ".repeat(100);
        let t = truncate_tokens(long.trim(), 50, &tc);
        assert!(tc.count(&t) <= 50 && t.ends_with("слово"), "{t:?}");
        assert!(tc.count(&format!("{t} слово")) > 50);
        assert_eq!(truncate_tokens("оченьдлинноеслово", 1, &tc), "");
    }

    #[test]
    fn prefix_format() {
        let tags = ["x".to_string(), "y z".to_string()];
        assert_eq!(context_prefix("K", "a/b", "b", "", &[]), "[K/a/b] b");
        assert_eq!(
            context_prefix("K", "a/b", "b", "", &tags),
            "[K/a/b] b | x, y z"
        );
        assert_eq!(
            context_prefix("K", "a/b", "b", "H", &tags),
            "[K/a/b] b > H | x, y z"
        );
        assert_eq!(
            context_prefix("K", "a/b", "b", "X > Y", &[]),
            "[K/a/b] b > X > Y"
        );
    }
}
