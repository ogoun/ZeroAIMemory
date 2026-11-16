//! Чтение записей журналов: запись по id, лента пространства с курсором, ревизии,
//! кандидаты в дубли insight по заголовку.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use zam_core::Id;

use super::{
    AuthorView, Certainty, ENTRY_COLS, Entry, EntryFilter, EntryKind, EntryRevision, EntryStatus,
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

/// Условия ленты пространства — общие у [`list_entries`] и [`count_by_status`], чтобы счётчики
/// совпадали с лентой. Параметры: `?1` пространство, `?2` вид, `?3` статусы (JSON), `?4`
/// `certainty` (JSON), `?5` область, `?6` тег, `?7`–`?8` интервал `created_at`, `?9`–`?10`
/// курсор (`key`, id), `?12` `data.kind`, `?13` `decided_by`, `?14` с отчётами, `?15`–`?16`
/// `origin`, `priority` идеи, `?17` только задачи из идей; `?11` — `LIMIT` ленты. `alive` —
/// живые или удалённые записи, `key` — столбец порядка и курсора.
fn feed_where(alive: &str, key: &str) -> String {
    format!(
        "e.space_id = ?1 AND {alive}
           AND (?2 IS NULL OR e.kind = ?2)
           AND (?2 IS NOT NULL OR ?14 OR e.kind <> 'report')
           AND (?3 IS NULL OR e.status IN (SELECT value FROM json_each(?3)))
           AND (?4 IS NULL OR e.certainty IN (SELECT value FROM json_each(?4)))
           AND (?5 IS NULL OR e.area = ?5)
           AND (?6 IS NULL OR EXISTS (SELECT 1 FROM json_each(e.tags) t
                                      WHERE t.value = ?6))
           AND (?7 IS NULL OR e.created_at >= ?7)
           AND (?8 IS NULL OR e.created_at < ?8)
           AND (?9 IS NULL OR {key} < ?9 OR ({key} = ?9 AND e.id < ?10))
           AND (?12 IS NULL OR json_extract(e.data, '$.kind') = ?12)
           AND (?13 IS NULL OR (e.kind = 'insight'
                AND json_extract(e.data, '$.kind') = 'decision'
                AND CASE WHEN json_type(e.data, '$.decision.decided_by') = 'text'
                         THEN json_extract(e.data, '$.decision.decided_by')
                         ELSE CASE WHEN e.author_kind = 'agent' THEN 'agent'
                              ELSE 'human' END END = ?13))
           AND (?15 IS NULL OR json_extract(e.data, '$.origin') = ?15)
           AND (?16 IS NULL OR json_extract(e.data, '$.priority') = ?16)
           AND (NOT ?17 OR {FROM_IDEA_SQL})"
    )
}

/// Задача `e` — из идеи: системное ребро `derived_from` на живую идею — то же правило, что у
/// пунктов плана дайджеста (`ideas::PLANNED_SQL`) и у поля `idea` задачи ([`crate::ideas_of_tasks`]).
const FROM_IDEA_SQL: &str = "EXISTS (SELECT 1 FROM edges g JOIN entries i ON i.id = g.dst_id
     WHERE g.src_kind = 'entry' AND g.src_id = e.id AND g.dst_kind = 'entry'
       AND g.type = 'derived_from' AND g.origin = 'system' AND g.deleted_at IS NULL
       AND i.kind = 'idea' AND i.deleted_at IS NULL)";

/// Фильтры `data` идеи (`origin`, `priority`) — только у ленты идей, «из идей» — только у
/// ленты задач (иначе `BadInput`).
fn check_kind_filters(f: &EntryFilter) -> Result<(), StoreError> {
    if (f.origin.is_some() || f.priority.is_some()) && f.kind != Some(EntryKind::Idea) {
        return Err(StoreError::BadInput(
            "origin и priority — фильтры идей: только с kind=idea".into(),
        ));
    }
    if f.from_idea && f.kind != Some(EntryKind::Task) {
        return Err(StoreError::BadInput(
            "from_idea — фильтр задач: только с kind=task".into(),
        ));
    }
    Ok(())
}

/// Лента живых записей пространства: новые первыми (`created_at`, затем id по убыванию),
/// не больше `limit` (1..=200, иначе `BadInput`). Вторым значением — курсор следующей
/// страницы (`None` — страниц больше нет); некорректный курсор → `BadInput`. Фильтры:
/// вид, статусы (пусто — любые), `certainty` (пусто — любые), область, тег, `data.kind`
/// (`json_extract(data, '$.kind')`), интервал `created_at` `[since, until)`. Фильтры
/// применяются в SQL до `LIMIT`, поэтому курсор не теряет записей между страницами.
/// Без вида отчёты (`report`) не выдаются, если не задан `with_reports` (сессии WebUI).
/// `deleted_only` — корзина: только удалённые записи, новые удаления первыми (курсор —
/// `<deleted_at>|<id>`). `origin` и `priority` — фильтры идей по `data` (M5.7), только при
/// `kind = idea`; `from_idea` — задачи из идей (M6.5 §9), только при `kind = task` (иначе
/// `BadInput`).
pub async fn list_entries(
    db: &Db,
    f: EntryFilter,
) -> Result<(Vec<Entry>, Option<String>), StoreError> {
    if !(1..=LIST_LIMIT_MAX).contains(&f.limit) {
        return Err(StoreError::BadInput(format!(
            "limit: от 1 до {LIST_LIMIT_MAX}"
        )));
    }
    check_kind_filters(&f)?;
    let origin = f.origin.map(|o| o.as_str());
    let priority = f.priority.map(|p| p.as_str());
    let cursor = f.cursor.as_deref().map(parse_cursor).transpose()?;
    let statuses = json_list(&f.statuses, |s| s.as_str());
    let certainty = json_list(&f.certainty, |c| c.as_str());
    let (c_ts, c_id) = cursor.map_or((None, None), |(t, i)| (Some(t), Some(i)));
    let since = f.since.map(fmt_ts);
    let until = f.until.map(fmt_ts);
    let limit = f.limit;
    // ключ порядка и курсора: время создания в живой ленте, время удаления в корзине
    let (alive, key) = if f.deleted_only {
        ("e.deleted_at IS NOT NULL", "e.deleted_at")
    } else {
        ("e.deleted_at IS NULL", "e.created_at")
    };
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries e
             WHERE {}
             ORDER BY {key} DESC, e.id DESC
             LIMIT ?11",
            feed_where(alive, key)
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
                f.with_reports,
                origin,
                priority,
                f.from_idea,
            ],
            entry_row,
        )?;
        let mut out: Vec<Entry> = rows.collect::<Result<_, _>>()?;
        let next = if out.len() > limit {
            out.truncate(limit);
            out.last().map(|e| {
                let ts = if f.deleted_only {
                    e.deleted_at.unwrap_or(e.created_at)
                } else {
                    e.created_at
                };
                format!("{}{CURSOR_SEP}{}", fmt_ts(ts), e.id)
            })
        } else {
            None
        };
        Ok((out, next))
    })
    .await
}

/// Число живых записей вида `f.kind` по статусам — с теми же фильтрами, что у
/// [`list_entries`] (чипы статусов ленты идей WebUI, M6.5 §9): каждый статус вида
/// ([`super::allowed_statuses`], в его порядке), отсутствующий — с нулём. `statuses`, `cursor` и
/// `limit` фильтра не учитываются. Без вида, у вида без статусов и с `deleted_only` — `BadInput`.
pub async fn count_by_status(
    db: &Db,
    f: EntryFilter,
) -> Result<Vec<(EntryStatus, usize)>, StoreError> {
    let Some(kind) = f.kind else {
        return Err(StoreError::BadInput(
            "счётчики статусов — только с kind".into(),
        ));
    };
    let all = super::allowed_statuses(kind);
    if all.is_empty() {
        return Err(StoreError::BadInput(format!(
            "у вида {} нет статусов",
            kind.as_str()
        )));
    }
    if f.deleted_only {
        return Err(StoreError::BadInput(
            "счётчики статусов — только у живых записей".into(),
        ));
    }
    check_kind_filters(&f)?;
    let origin = f.origin.map(|o| o.as_str());
    let priority = f.priority.map(|p| p.as_str());
    let certainty = json_list(&f.certainty, |c| c.as_str());
    let since = f.since.map(fmt_ts);
    let until = f.until.map(fmt_ts);
    let counts: Vec<(Option<EntryStatus>, i64)> = db
        .read(move |c| {
            // `?3` (статусы) и курсор `?9`–`?10` — пустые, `?11` (`LIMIT` ленты) не участвует
            let mut st = c.prepare_cached(&format!(
                "SELECT e.status, count(*) FROM entries e
                 WHERE {}
                 GROUP BY e.status",
                feed_where("e.deleted_at IS NULL", "e.created_at")
            ))?;
            let rows = st.query_map(
                params![
                    f.space_id,
                    Some(kind),
                    None::<String>,
                    certainty,
                    f.area,
                    f.tag,
                    since,
                    until,
                    None::<String>,
                    None::<Id>,
                    0_i64,
                    f.data_kind,
                    f.decided_by,
                    false,
                    origin,
                    priority,
                    f.from_idea,
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await?;
    Ok(all
        .iter()
        .map(|s| {
            let n = counts
                .iter()
                .find(|(st, _)| *st == Some(*s))
                .map_or(0, |(_, n)| usize::try_from(*n).unwrap_or(0));
            (*s, n)
        })
        .collect())
}

/// Живые записи по id (одним запросом, для полей ленты вроде задачи идеи): id → запись;
/// удалённых и несуществующих в ответе нет.
pub async fn live_entries_by_ids(
    db: &Db,
    ids: &[Id],
) -> Result<std::collections::HashMap<Id, Entry>, StoreError> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let list =
        serde_json::Value::from(ids.iter().map(Id::to_string).collect::<Vec<_>>()).to_string();
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries e
             WHERE e.id IN (SELECT value FROM json_each(?1)) AND e.deleted_at IS NULL"
        ))?;
        let rows = st.query_map([list], entry_row)?;
        let mut out = std::collections::HashMap::new();
        for e in rows {
            let e = e?;
            out.insert(e.id, e);
        }
        Ok(out)
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
