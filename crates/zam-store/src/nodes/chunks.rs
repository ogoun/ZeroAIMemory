//! Метаданные заметки (frontmatter, L0), чанки §8.2 и постановка заданий индексации
//! (§8.1–§8.3 ТЗ).
//!
//! Чанки строит `zam_markdown::chunk_note` с глобальным счётчиком токенов
//! ([`set_token_counter`], по умолчанию `ApproxCounter`): листы `chunk`, L1 секций
//! `section` и L1 заметки `note`. У заметки без текста тела — один `chunk`
//! с заголовком или именем, чтобы она находилась поиском по названию.
//! В FTS попадают только `chunk` и `note` (условие в триггерах миграции 0004).
//!
//! После пересборки чанков ставится задание outbox (в той же транзакции):
//! индексируемая заметка → `upsert`; Human и `index: false` → без задания
//! (`index_state = 'excluded'`), но заметка, бывшая индексируемой, получает `delete`.

use std::sync::{Arc, OnceLock, RwLock};

use jiff::Timestamp;
use rusqlite::{Connection, params};
use zam_core::Id;
use zam_markdown::{
    ApproxCounter, ChunkLevel, ChunkParams, Doc, TokenCounter, chunk_note, first_paragraph, parse,
};

use super::{hex, revision_text};
use crate::jobs::{JobOp, PRIORITY_WRITE, enqueue};
use crate::time::fmt_ts;
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

static TOKEN_COUNTER: OnceLock<RwLock<Arc<dyn TokenCounter>>> = OnceLock::new();

fn counter_cell() -> &'static RwLock<Arc<dyn TokenCounter>> {
    TOKEN_COUNTER.get_or_init(|| RwLock::new(Arc::new(ApproxCounter)))
}

/// Устанавливает счётчик токенов чанкера для всего процесса (например, токенизатор
/// bge-m3 после загрузки модели). Заметки, разбитые прежним счётчиком, остаются как
/// есть до перезаписи или reindex.
pub fn set_token_counter(tc: Arc<dyn TokenCounter>) {
    *counter_cell().write().unwrap_or_else(|e| e.into_inner()) = tc;
}

/// Текущий счётчик токенов.
pub(crate) fn token_counter() -> Arc<dyn TokenCounter> {
    counter_cell()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
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
    let index = fm.and_then(|f| f.index) != Some(false);
    NoteMeta {
        doc,
        frontmatter,
        l0,
        l0_source,
        index,
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
    s
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
    let drafts = chunk_note(src, doc, &ChunkParams::default(), tc);
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

/// Пересобирает чанки заметки `node_id` по ревизии `rev`: старые удаляются
/// (триггеры убирают их из FTS), новые вставляются всегда (и при `index: false`:
/// чанки и FTS остаются, исключение — только из векторного индекса, §6.4).
/// Вызывается внутри транзакции записи. Возвращает число вставленных чанков.
pub(crate) fn rebuild_chunks(
    c: &Connection,
    node_id: Id,
    rev: i64,
    content: &str,
    meta: &NoteMeta,
) -> Result<usize, StoreError> {
    c.prepare_cached("DELETE FROM chunks WHERE node_id = ?1")?
        .execute([node_id])?;
    let (space, path, name): (String, String, String) = c.query_row(
        "SELECT s.name, n.path, n.name FROM nodes n JOIN spaces s ON s.id = n.space_id
         WHERE n.id = ?1",
        [node_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let tags = meta
        .doc
        .frontmatter
        .as_ref()
        .map_or(&[][..], |f| &f.tags[..]);
    let tc = token_counter();
    let chunks = note_chunks(content, &meta.doc, &name, tc.as_ref());
    let mut st = c.prepare_cached(
        "INSERT INTO chunks (id, node_id, rev, level, heading_path, ordinal, text,
                             context_prefix, token_count, text_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for (ordinal, ch) in chunks.iter().enumerate() {
        let ordinal = ordinal as i64;
        st.execute(params![
            chunk_id(node_id, ordinal, &ch.text_hash),
            node_id,
            rev,
            ch.level.as_str(),
            ch.heading_path,
            ordinal,
            ch.text,
            context_prefix(&space, &path, &name, &ch.heading_path, tags),
            ch.token_count,
            ch.text_hash,
        ])?;
    }
    Ok(chunks.len())
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

/// Пересборка чанков и задание после записи заметки (создание, правка, откат,
/// восстановление) в транзакции писателя.
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
    enqueue_for_note(c, node_id, rev, prev_state, PRIORITY_WRITE, now)?;
    Ok(())
}

/// Заметок в одной транзакции перечанковки: писатель не занят надолго.
const RECHUNK_BATCH: i64 = 200;

/// Перечанковывает живые заметки (`space = None` — всех пространств) текущим
/// счётчиком токенов, пересчитывает `index_state` и ставит задания с `priority`.
/// L0 и прочие поля узла не меняются. Пакетами по [`RECHUNK_BATCH`] заметок
/// в отдельных транзакциях; повторный запуск безопасен. Возвращает
/// `(заметок, заданий upsert)`. В лог — только счётчики.
pub(crate) async fn rechunk_notes(
    db: &Db,
    space: Option<Id>,
    priority: i64,
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
                    tx.prepare_cached("UPDATE nodes SET index_state = ?2 WHERE id = ?1")?
                        .execute(params![id, index_state(&meta, kind)])?;
                    rebuild_chunks(tx, id, rev, &content, &meta)?;
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
    fn chunk_id_is_deterministic() {
        let n = Id::new();
        let a = chunk_id(n, 0, "h");
        assert_eq!(a, chunk_id(n, 0, "h"));
        assert_eq!(a.len(), 32);
        assert_ne!(a, chunk_id(n, 1, "h"));
        assert_ne!(a, chunk_id(Id::new(), 0, "h"));
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
