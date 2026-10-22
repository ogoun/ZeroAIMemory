//! Чтение записей журналов: запись по id, лента пространства с курсором, ревизии.

use rusqlite::params;
use zam_core::Id;

use super::{
    AuthorView, ENTRY_COLS, Entry, EntryFilter, EntryRevision, LIST_LIMIT_MAX, entry_row, json_col,
    load_entry,
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
/// вид, статусы (пусто — любые), `certainty` (пусто — любые), область, тег, интервал
/// `created_at` `[since, until)`.
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
