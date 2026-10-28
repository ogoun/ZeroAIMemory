//! Чтение записей журналов: запись по id, лента пространства с курсором, ревизии,
//! кандидаты в дубли insight по заголовку.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use zam_core::Id;

use super::{
    AuthorView, Certainty, ENTRY_COLS, Entry, EntryFilter, EntryRevision, EntryStatus,
    LIST_LIMIT_MAX, entry_row, json_col, load_entry, normalize_area, normalize_title,
};
use crate::time::{fmt_ts, ts_at};
use crate::{Db, StoreError};

/// Запись по id (в том числе удалённая: `deleted_at`). Нет → `NotFound`.
pub async fn get_entry(db: &Db, id: Id) -> Result<Entry, StoreError> {
    db.read(move |c| load_entry(c, id)).await
}

/// Разделитель полей курсора `<created_at>|<id>`.
const CURSOR_SEP: char = '|';

/// Курсор → `(created_at, id)` последней записи прошлой страницы.
fn parse_cursor(s: &str) -> Result<(String, Id), StoreError> {
    let bad = || StoreError::BadInput("некорректный cursor".into());
    let (ts, id) = s.split_once(CURSOR_SEP).ok_or_else(bad)?;
    let ts = crate::time::parse_ts(ts).map_err(|_| bad())?;
    Ok((fmt_ts(ts), id.parse().map_err(|_| bad())?))
}

/// JSON-массив строк для `json_each`; пустой список — `None` (без ограничения).
fn json_list<T>(v: &[T], f: impl Fn(&T) -> &'static str) -> Option<String> {
    (!v.is_empty())
        .then(|| serde_json::Value::from(v.iter().map(&f).collect::<Vec<_>>()).to_string())
}

/// Лента живых записей пространства: новые первыми (`created_at`, затем id по убыванию),
/// не больше `limit` (1..=200, иначе `BadInput`). Вторым значением — курсор следующей
/// страницы (`None` — страниц больше нет); некорректный курсор → `BadInput`. Фильтры:
/// вид, статусы (пусто — любые), `certainty` (пусто — любые), область, тег, `data.kind`
/// (`json_extract(data, '$.kind')`), интервал `created_at` `[since, until)`. Фильтры
/// применяются в SQL до `LIMIT`, поэтому курсор не теряет записей между страницами.
pub async fn list_entries(
    db: &Db,
    f: EntryFilter,
) -> Result<(Vec<Entry>, Option<String>), StoreError> {
    if !(1..=LIST_LIMIT_MAX).contains(&f.limit) {
        return Err(StoreError::BadInput(format!(
            "limit: от 1 до {LIST_LIMIT_MAX}"
        )));
    }
    let cursor = f.cursor.as_deref().map(parse_cursor).transpose()?;
    let statuses = json_list(&f.statuses, |s| s.as_str());
    let certainty = json_list(&f.certainty, |c| c.as_str());
    let (c_ts, c_id) = cursor.map_or((None, None), |(t, i)| (Some(t), Some(i)));
    let since = f.since.map(fmt_ts);
    let until = f.until.map(fmt_ts);
    let limit = f.limit;
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries e
             WHERE e.space_id = ?1 AND e.deleted_at IS NULL
               AND (?2 IS NULL OR e.kind = ?2)
               AND (?3 IS NULL OR e.status IN (SELECT value FROM json_each(?3)))
               AND (?4 IS NULL OR e.certainty IN (SELECT value FROM json_each(?4)))
               AND (?5 IS NULL OR e.area = ?5)
               AND (?6 IS NULL OR EXISTS (SELECT 1 FROM json_each(e.tags) t
                                          WHERE t.value = ?6))
               AND (?7 IS NULL OR e.created_at >= ?7)
               AND (?8 IS NULL OR e.created_at < ?8)
               AND (?9 IS NULL OR e.created_at < ?9 OR (e.created_at = ?9 AND e.id < ?10))
               AND (?12 IS NULL OR json_extract(e.data, '$.kind') = ?12)
               AND (?13 IS NULL OR (e.kind = 'insight'
                    AND json_extract(e.data, '$.kind') = 'decision'
                    AND CASE WHEN json_type(e.data, '$.decision.decided_by') = 'text'
                             THEN json_extract(e.data, '$.decision.decided_by')
                             ELSE 'human' END = ?13))
             ORDER BY e.created_at DESC, e.id DESC
             LIMIT ?11"
        ))?;
        let rows = st.query_map(
            params![
                f.space_id,
                f.kind,
                statuses,
                certainty,
                f.area,
                f.tag,
                since,
                until,
                c_ts,
                c_id,
                (limit + 1) as i64,
                f.data_kind,
                f.decided_by,
            ],
            entry_row,
        )?;
        let mut out: Vec<Entry> = rows.collect::<Result<_, _>>()?;
        let next = if out.len() > limit {
            out.truncate(limit);
            out.last()
                .map(|e| format!("{}{CURSOR_SEP}{}", fmt_ts(e.created_at), e.id))
        } else {
            None
        };
        Ok((out, next))
    })
    .await
}

/// Ревизии записи по возрастанию номера. Нет записи → `NotFound`.
pub async fn entry_revisions(db: &Db, id: Id) -> Result<Vec<EntryRevision>, StoreError> {
    db.read(move |c| {
        load_entry(c, id)?;
        let mut st = c.prepare_cached(
            "SELECT rev, title, body, data, tags, area, status, certainty, reason, ts,
                    author_kind, user_id, token_id, session_id, machine
             FROM entry_revisions WHERE entry_id = ?1 ORDER BY rev",
        )?;
        let rows = st.query_map([id], |r| {
            Ok(EntryRevision {
                rev: r.get(0)?,
                title: r.get(1)?,
                body: r.get(2)?,
                data: json_col(r, 3)?,
                tags: json_col(r, 4)?,
                area: r.get(5)?,
                status: r.get(6)?,
                certainty: r.get(7)?,
                reason: r.get(8)?,
                ts: ts_at(r, 9)?,
                author: AuthorView {
                    kind: r.get(10)?,
                    user_id: r.get(11)?,
                    token_id: r.get(12)?,
                    session_id: r.get(13)?,
                    machine: r.get(14)?,
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Предел выдачи [`changelog_between`].
pub const CHANGELOG_BETWEEN_MAX: usize = 10_000;

/// Живые записи changelog пространства с `created_at` в `(after, upto]` по возрастанию
/// времени (`after = None` — с начала журнала); не больше [`CHANGELOG_BETWEEN_MAX`].
pub async fn changelog_between(
    db: &Db,
    space_id: Id,
    after: Option<jiff::Timestamp>,
    upto: jiff::Timestamp,
) -> Result<Vec<Entry>, StoreError> {
    let after = after.map(fmt_ts);
    let upto = fmt_ts(upto);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries e
             WHERE e.space_id = ?1 AND e.kind = 'changelog' AND e.deleted_at IS NULL
               AND (?2 IS NULL OR e.created_at > ?2) AND e.created_at <= ?3
             ORDER BY e.created_at, e.id
             LIMIT {CHANGELOG_BETWEEN_MAX}"
        ))?;
        let rows = st.query_map(params![space_id, after, upto], entry_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

/// Живой релиз пространства с `data.version = version`, если есть.
pub async fn find_release_by_version(
    db: &Db,
    space_id: Id,
    version: &str,
) -> Result<Option<Entry>, StoreError> {
    let version = version.to_string();
    db.read(move |c| {
        Ok(c.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries e
             WHERE e.space_id = ?1 AND e.kind = 'release' AND e.deleted_at IS NULL
               AND json_extract(e.data, '$.version') = ?2
             ORDER BY e.created_at, e.id LIMIT 1"
        ))?
        .query_row(params![space_id, version], entry_row)
        .optional()?)
    })
    .await
}

/// Предел кандидатов в дубли в ответе 409 (§7.5).
pub const DEDUP_CANDIDATES_MAX: usize = 5;

/// Кандидат в дубли нового insight (§7.5): действующая запись той же области.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DedupCandidate {
    pub id: Id,
    pub title: String,
    pub status: EntryStatus,
    pub certainty: Option<Certainty>,
    /// `title` — совпал нормализованный заголовок, `vector` — косинус выше порога.
    pub reason: &'static str,
    /// Косинус у `vector`.
    pub score: Option<f32>,
}

/// Живые insight пространства `space_id` со статусом `active` или `revisit`, той же области
/// (`area` после обрезки; пустая — без области, и тогда совпадают только записи без
/// области) и с тем же [`normalize_title`]; не больше [`DEDUP_CANDIDATES_MAX`], новые
/// первыми. Заголовок без букв и цифр (нормализуется в пустую строку) дублей не имеет.
pub async fn title_duplicates(
    db: &Db,
    space_id: Id,
    title: &str,
    area: Option<&str>,
) -> Result<Vec<DedupCandidate>, StoreError> {
    let norm = normalize_title(title);
    if norm.is_empty() {
        return Ok(Vec::new());
    }
    let area = normalize_area(area);
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT e.id, e.title, e.status, e.certainty FROM entries e
             WHERE e.space_id = ?1 AND e.kind = 'insight' AND e.title_norm = ?2
               AND e.deleted_at IS NULL AND e.status IN ('active', 'revisit')
               AND e.area IS ?3
             ORDER BY e.created_at DESC, e.id DESC
             LIMIT {DEDUP_CANDIDATES_MAX}"
        ))?;
        let rows = st.query_map(params![space_id, norm, area], |r| {
            Ok(DedupCandidate {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                certainty: r.get(3)?,
                reason: "title",
                score: None,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}
