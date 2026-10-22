//! Запись журналов: каждая операция — одна транзакция писателя. Строка `entries`, ревизия
//! (`entry_revisions`), чанки, FTS и задание outbox фиксируются вместе или не фиксируются
//! вовсе (§8.1 ТЗ). Ничего не удаляется: удаление мягкое, замещение и отзыв — смена статуса.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use zam_core::Id;

use super::chunks::{entry_index_state, index_entry_now, unindex_entry};
use super::{
    AREA_MAX_CHARS, Certainty, Entry, EntryKind, EntryPatch, EntryStatus, KEY_MAX_CHARS, NewEntry,
    REASON_MAX_CHARS, TAG_MAX_CHARS, TAGS_MAX, TITLE_MAX_CHARS, check_triggers, load_entry,
    normalize_title, replace_triggers,
};
use crate::nodes::Attribution;
use crate::time::fmt_ts;
use crate::user_db::SpaceKind;
use crate::{Db, StoreError};

/// Поля записи, которые входят в ревизию.
#[derive(Debug, Clone, PartialEq)]
struct Fields {
    title: String,
    body: String,
    data: serde_json::Value,
    tags: Vec<String>,
    area: Option<String>,
    status: Option<EntryStatus>,
    certainty: Option<Certainty>,
}

impl Fields {
    fn of(e: &Entry) -> Fields {
        Fields {
            title: e.title.clone(),
            body: e.body.clone(),
            data: e.data.clone(),
            tags: e.tags.clone(),
            area: e.area.clone(),
            status: e.status,
            certainty: e.certainty,
        }
    }
}

fn bad(msg: impl Into<String>) -> StoreError {
    StoreError::BadInput(msg.into())
}

/// Заголовок без пробелов по краям: непустой, не длиннее [`TITLE_MAX_CHARS`].
fn check_title(t: &str) -> Result<String, StoreError> {
    let t = t.trim();
    if t.is_empty() {
        return Err(bad("пустой заголовок"));
    }
    if t.chars().count() > TITLE_MAX_CHARS {
        return Err(bad(format!("заголовок длиннее {TITLE_MAX_CHARS} символов")));
    }
    Ok(t.to_string())
}

/// Область: пустая — `None`; до [`AREA_MAX_CHARS`] печатных символов.
fn check_area(a: Option<String>) -> Result<Option<String>, StoreError> {
    let Some(a) = a.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    if a.chars().count() > AREA_MAX_CHARS || a.chars().any(char::is_control) {
        return Err(bad(format!("area: до {AREA_MAX_CHARS} печатных символов")));
    }
    Ok(Some(a))
}

/// Теги без пробелов по краям, без пустых и повторов (порядок сохраняется).
fn check_tags(tags: Vec<String>) -> Result<Vec<String>, StoreError> {
    let mut out: Vec<String> = Vec::with_capacity(tags.len());
    for t in tags {
        let t = t.trim();
        if t.is_empty() || out.iter().any(|o| o == t) {
            continue;
        }
        if t.chars().count() > TAG_MAX_CHARS || t.chars().any(char::is_control) {
            return Err(bad(format!("тег: до {TAG_MAX_CHARS} печатных символов")));
        }
        out.push(t.to_string());
    }
    if out.len() > TAGS_MAX {
        return Err(bad(format!("тегов больше {TAGS_MAX}")));
    }
    Ok(out)
}

/// `data` — JSON-объект.
fn check_data(d: &serde_json::Value) -> Result<(), StoreError> {
    if d.is_object() {
        Ok(())
    } else {
        Err(bad("data должно быть JSON-объектом"))
    }
}

/// Причина: если задана — непустая и не длиннее [`REASON_MAX_CHARS`].
fn check_reason(r: Option<String>) -> Result<Option<String>, StoreError> {
    let Some(r) = r else {
        return Ok(None);
    };
    let r = r.trim();
    if r.is_empty() {
        return Err(bad("пустая причина"));
    }
    if r.chars().count() > REASON_MAX_CHARS {
        return Err(bad(format!("причина длиннее {REASON_MAX_CHARS} символов")));
    }
    Ok(Some(r.to_string()))
}

/// `Idempotency-Key`: если задан — непустой, до [`KEY_MAX_CHARS`] печатных символов.
fn check_key(k: Option<String>) -> Result<Option<String>, StoreError> {
    let Some(k) = k else {
        return Ok(None);
    };
    let k = k.trim();
    if k.is_empty() || k.chars().count() > KEY_MAX_CHARS || k.chars().any(char::is_control) {
        return Err(bad(format!(
            "Idempotency-Key: от 1 до {KEY_MAX_CHARS} печатных символов"
        )));
    }
    Ok(Some(k.to_string()))
}

/// Тело с дописанным текстом: через пустую строку (у пустого тела — сам текст).
fn appended(body: &str, more: &str) -> String {
    let head = body.trim_end();
    if head.is_empty() {
        more.to_string()
    } else {
        format!("{head}\n\n{more}")
    }
}

/// Вид пространства; нет пространства → `NotFound`.
fn space_kind_of(c: &Connection, space: Id) -> Result<SpaceKind, StoreError> {
    c.prepare_cached("SELECT kind FROM spaces WHERE id = ?1")?
        .query_row([space], |r| r.get(0))
        .optional()?
        .ok_or(StoreError::NotFound)
}

/// Ключ идемпотентности в границах автора: `<автор>:<ключ>`, автор — пользователь, иначе
/// токен, иначе `-` (администратор без пользователя). Идемпотентность по `(автор, ключ)`:
/// в общем решении ключ участника не совпадает с ключом владельца. [решение исполнителя]
fn scoped_key(a: &Attribution, key: &str) -> String {
    let who = a
        .user_id
        .map(|u| u.to_string())
        .or_else(|| a.token_id.map(|t| format!("t{t}")))
        .unwrap_or_else(|| "-".into());
    format!("{who}:{key}")
}

/// Запись с ключом идемпотентности `key` (уже с автором), если есть.
fn by_key(c: &Connection, key: &str) -> Result<Option<Entry>, StoreError> {
    let id: Option<Id> = c
        .prepare_cached("SELECT id FROM entries WHERE idempotency_key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?;
    id.map(|id| load_entry(c, id)).transpose()
}

/// Запись автора `a` по `Idempotency-Key` (в том числе удалённая), если есть.
pub async fn entry_by_idempotency_key(
    db: &Db,
    a: &Attribution,
    key: &str,
) -> Result<Option<Entry>, StoreError> {
    let key = check_key(Some(key.to_string()))?.map(|k| scoped_key(a, &k));
    db.read(move |c| match key {
        Some(k) => by_key(c, &k),
        None => Ok(None),
    })
    .await
}

/// Повтор запроса с тем же ключом: та же запись, если отпечаток запроса (вид, пространство,
/// заголовок, тело, `data`) совпал с первой ревизией; расхождение → `IdempotencyMismatch`,
/// удалённая запись → `Conflict`.
fn same_request(c: &Connection, ex: Entry, e: &NewEntry) -> Result<Entry, StoreError> {
    let (title, body, data): (String, String, String) = c
        .prepare_cached(
            "SELECT title, body, data FROM entry_revisions WHERE entry_id = ?1 AND rev = 1",
        )?
        .query_row([ex.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let data: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    if ex.space_id != e.space_id
        || ex.kind != e.kind
        || title != e.title.trim()
        || body != e.body
        || data != e.data
    {
        return Err(StoreError::IdempotencyMismatch(
            "Idempotency-Key уже использован запросом с другим содержимым".into(),
        ));
    }
    if ex.deleted_at.is_some() {
        return Err(StoreError::Conflict(
            "запись по этому Idempotency-Key удалена: восстановите её или используйте новый ключ"
                .into(),
        ));
    }
    Ok(ex)
}

/// Версия релиза свободна среди живых релизов пространства (кроме `except`) — иначе
/// `Conflict`. Вызывается внутри транзакции писателя (`BEGIN IMMEDIATE`), поэтому проверка и
/// запись атомарны.
fn ensure_release_version_free(
    c: &Connection,
    space: Id,
    data: &serde_json::Value,
    except: Option<Id>,
) -> Result<(), StoreError> {
    let Some(v) = data.get("version").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let other: Option<Id> = c
        .prepare_cached(
            "SELECT id FROM entries
             WHERE space_id = ?1 AND kind = 'release' AND deleted_at IS NULL
               AND json_extract(data, '$.version') = ?2 AND (?3 IS NULL OR id <> ?3)
             LIMIT 1",
        )?
        .query_row(params![space, v, except], |r| r.get(0))
        .optional()?;
    if other.is_some() {
        return Err(StoreError::Conflict(format!(
            "релиз с версией «{v}» уже есть в пространстве"
        )));
    }
    Ok(())
}

/// Статус новой записи: у insight — `active` по умолчанию, `active` или `revisit`; у
/// остальных видов статуса нет.
fn new_status(kind: EntryKind, s: Option<EntryStatus>) -> Result<Option<EntryStatus>, StoreError> {
    match (kind, s) {
        (EntryKind::Insight, None) => Ok(Some(EntryStatus::Active)),
        (EntryKind::Insight, Some(s @ (EntryStatus::Active | EntryStatus::Revisit))) => Ok(Some(s)),
        (EntryKind::Insight, Some(s)) => Err(bad(format!(
            "новая запись не может быть {}: замещение и отзыв — отдельные операции",
            s.as_str()
        ))),
        (_, Some(_)) => Err(bad("статус есть только у insight")),
        (_, None) => Ok(None),
    }
}

/// Вставляет новую запись с ревизией 1, чанками и заданием. Возвращает её id.
fn insert_entry(
    c: &Connection,
    e: NewEntry,
    key: Option<String>,
    a: &Attribution,
    supersedes: Option<Id>,
    now: &str,
) -> Result<Id, StoreError> {
    let space_kind = space_kind_of(c, e.space_id)?;
    check_data(&e.data)?;
    if e.kind == EntryKind::Release {
        ensure_release_version_free(c, e.space_id, &e.data, None)?;
    }
    let status = new_status(e.kind, e.status)?;
    if e.kind != EntryKind::Insight && e.certainty.is_some() {
        return Err(bad("certainty есть только у insight"));
    }
    if e.kind != EntryKind::Insight && !e.triggers.is_empty() {
        return Err(bad("триггеры есть только у insight"));
    }
    let triggers = check_triggers(e.triggers)?;
    let f = Fields {
        title: check_title(&e.title)?,
        body: e.body,
        data: e.data,
        tags: check_tags(e.tags)?,
        area: check_area(e.area.or_else(|| a.area.clone()))?,
        status,
        certainty: e.certainty,
    };
    let id = Id::new();
    c.prepare_cached(
        "INSERT INTO entries (id, space_id, kind, title, title_norm, body, data, tags, area,
                              status, certainty, supersedes_id, idempotency_key, current_rev,
                              author_kind, user_id, token_id, session_id, machine, index_state,
                              created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?20)",
    )?
    .execute(params![
        id,
        e.space_id,
        e.kind,
        f.title,
        normalize_title(&f.title),
        f.body,
        f.data.to_string(),
        tags_json(&f.tags),
        f.area,
        f.status,
        f.certainty,
        supersedes,
        key,
        a.author_kind,
        a.user_id,
        a.token_id,
        a.session_id,
        a.machine,
        entry_index_state(space_kind).as_str(),
        now,
    ])?;
    insert_revision(c, id, 1, &f, None, a, now)?;
    replace_triggers(c, id, &triggers, now)?;
    index_entry_now(c, id, None, now)?;
    Ok(id)
}

fn tags_json(tags: &[String]) -> String {
    serde_json::to_string(tags).expect("сериализация списка строк")
}

/// Дописывает ревизию (ревизии только дописываются).
fn insert_revision(
    c: &Connection,
    id: Id,
    rev: i64,
    f: &Fields,
    reason: Option<&str>,
    a: &Attribution,
    now: &str,
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
        f.data.to_string(),
        tags_json(&f.tags),
        f.area,
        f.status,
        f.certainty,
        reason,
        now,
        a.author_kind,
        a.user_id,
        a.token_id,
        a.session_id,
        a.machine,
    ])?;
    Ok(())
}

/// Новая ревизия `cur.rev + 1` с полями `f`: строка записи, ревизия, чанки и задание
/// `upsert` (payload меняется и при смене одного статуса или `certainty`).
fn save(
    c: &Connection,
    cur: &Entry,
    f: &Fields,
    reason: Option<&str>,
    a: &Attribution,
    now: &str,
) -> Result<(), StoreError> {
    let rev = cur.rev + 1;
    c.prepare_cached(
        "UPDATE entries SET title = ?2, title_norm = ?3, body = ?4, data = ?5, tags = ?6,
                            area = ?7, status = ?8, certainty = ?9, current_rev = ?10,
                            updated_at = ?11,
                            index_state = CASE WHEN index_state = 'excluded' THEN 'excluded'
                                               ELSE 'pending' END
         WHERE id = ?1",
    )?
    .execute(params![
        cur.id,
        f.title,
        normalize_title(&f.title),
        f.body,
        f.data.to_string(),
        tags_json(&f.tags),
        f.area,
        f.status,
        f.certainty,
        rev,
        now,
    ])?;
    insert_revision(c, cur.id, rev, f, reason, a, now)?;
    index_entry_now(c, cur.id, Some(cur.index_state), now)
}

/// Живая запись по id: нет или удалена → `NotFound`.
fn live_entry(c: &Connection, id: Id) -> Result<Entry, StoreError> {
    let e = load_entry(c, id)?;
    if e.deleted_at.is_some() {
        return Err(StoreError::NotFound);
    }
    Ok(e)
}

/// Создаёт запись журнала (ревизия 1, чанки, FTS, задание `upsert` — одной транзакцией).
/// Повтор с тем же `idempotency_key` возвращает уже созданную запись и `false`; ключ,
/// расходящийся по отпечатку (вид, пространство, заголовок, тело, `data`) →
/// `IdempotencyMismatch`, удалённая запись → `Conflict`. Ключ действует в границах автора
/// (пользователь или токен). Нет пространства →
/// `NotFound`; пустой заголовок, `data` не объект, статус или `certainty` не у insight →
/// `BadInput`. Область по умолчанию — `a.area` (`X-Zam-Area`).
pub async fn create_entry(
    db: &Db,
    e: NewEntry,
    a: &Attribution,
) -> Result<(Entry, bool), StoreError> {
    let a = a.clone();
    db.write(move |tx| {
        let key = check_key(e.idempotency_key.clone())?.map(|k| scoped_key(&a, &k));
        if let Some(k) = &key
            && let Some(ex) = by_key(tx, k)?
        {
            return same_request(tx, ex, &e).map(|ex| (ex, false));
        }
        let now = fmt_ts(Timestamp::now());
        let id = insert_entry(tx, e, key, &a, None, &now)?;
        Ok((load_entry(tx, id)?, true))
    })
    .await
}

/// Правит запись: новая ревизия с причиной `p.reason`, пересборка чанков и `upsert`.
/// `if_rev` не совпал с текущей ревизией → `Conflict`; запись удалена или нет → `NotFound`.
/// `body` вместе с `append_body`, статус или `certainty` не у insight, статус кроме
/// `active`/`revisit`, пустая причина → `BadInput`; смена статуса у `superseded` или
/// `retracted` → `Conflict`. Правка без изменений возвращает запись как есть, без ревизии.
/// `triggers` заменяет набор триггеров insight в той же транзакции (неверный → `BadInput`);
/// триггеры в ревизию не входят.
pub async fn update_entry(
    db: &Db,
    id: Id,
    if_rev: Option<i64>,
    p: EntryPatch,
    a: &Attribution,
) -> Result<Entry, StoreError> {
    let a = a.clone();
    db.write(move |tx| {
        let cur = live_entry(tx, id)?;
        if let Some(r) = if_rev
            && r != cur.rev
        {
            return Err(StoreError::Conflict(format!(
                "ревизия записи {}, а не {r}",
                cur.rev
            )));
        }
        let reason = check_reason(p.reason)?;
        if p.body.is_some() && p.append_body.is_some() {
            return Err(bad("body и append_body вместе"));
        }
        let mut f = Fields::of(&cur);
        if let Some(t) = p.title {
            f.title = check_title(&t)?;
        }
        if let Some(b) = p.body {
            f.body = b;
        }
        if let Some(more) = p.append_body {
            f.body = appended(&f.body, &more);
        }
        if let Some(d) = p.data {
            check_data(&d)?;
            if cur.kind == EntryKind::Release {
                ensure_release_version_free(tx, cur.space_id, &d, Some(cur.id))?;
            }
            f.data = d;
        }
        if let Some(t) = p.tags {
            f.tags = check_tags(t)?;
        }
        if let Some(ar) = p.area {
            f.area = check_area(ar)?;
        }
        if let Some(s) = p.status {
            if cur.kind != EntryKind::Insight {
                return Err(bad("статус есть только у insight"));
            }
            if !matches!(s, EntryStatus::Active | EntryStatus::Revisit) {
                return Err(bad(
                    "superseded и retracted ставятся замещением и отзывом, а не правкой",
                ));
            }
            if let Some(old @ (EntryStatus::Superseded | EntryStatus::Retracted)) = cur.status
                && old != s
            {
                return Err(StoreError::Conflict(format!(
                    "запись {}: статус больше не меняется",
                    old.as_str()
                )));
            }
            f.status = Some(s);
        }
        if let Some(c) = p.certainty {
            if cur.kind != EntryKind::Insight {
                return Err(bad("certainty есть только у insight"));
            }
            f.certainty = Some(c);
        }
        let now = fmt_ts(Timestamp::now());
        if let Some(ts) = p.triggers {
            if cur.kind != EntryKind::Insight {
                return Err(bad("триггеры есть только у insight"));
            }
            replace_triggers(tx, id, &check_triggers(ts)?, &now)?;
        }
        if f == Fields::of(&cur) {
            return Ok(cur);
        }
        save(tx, &cur, &f, reason.as_deref(), &a, &now)?;
        load_entry(tx, id)
    })
    .await
}

/// Мягко удаляет запись: чанки убираются (и из FTS), индексируемой — `delete`. Ревизии
/// остаются. Нет записи или уже удалена → `NotFound`. Атрибуция — для проверок сервера
/// (задача 9); в строке записи автор удаления не хранится, как и у узлов.
pub async fn delete_entry(db: &Db, id: Id, a: &Attribution) -> Result<(), StoreError> {
    let _ = a;
    db.write(move |tx| {
        let cur = live_entry(tx, id)?;
        let now = fmt_ts(Timestamp::now());
        tx.execute(
            "UPDATE entries SET deleted_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        unindex_entry(tx, id, cur.rev, cur.index_state, &now)
    })
    .await
}

/// Восстанавливает удалённую запись: чанки из текущих полей, `upsert`. Нет → `NotFound`;
/// не удалена → `BadInput`.
pub async fn restore_entry(db: &Db, id: Id, a: &Attribution) -> Result<Entry, StoreError> {
    let _ = a;
    db.write(move |tx| {
        let cur = load_entry(tx, id)?;
        if cur.deleted_at.is_none() {
            return Err(bad("запись не удалена"));
        }
        if cur.kind == EntryKind::Release {
            ensure_release_version_free(tx, cur.space_id, &cur.data, Some(cur.id))?;
        }
        let now = fmt_ts(Timestamp::now());
        tx.execute(
            "UPDATE entries SET deleted_at = NULL,
                                index_state = CASE WHEN index_state = 'excluded' THEN 'excluded'
                                                   ELSE 'pending' END
             WHERE id = ?1",
            [id],
        )?;
        index_entry_now(tx, id, Some(cur.index_state), &now)?;
        load_entry(tx, id)
    })
    .await
}

/// Замещает insight `old` новой записью одной транзакцией: новая получает
/// `supersedes_id = old`, старая — `superseded`, `superseded_by_id` и новую ревизию; обе
/// переиндексируются. Возвращает `(новая, старая)`. Повтор с тем же `idempotency_key`
/// возвращает ту же пару. Не insight, другое пространство → `BadInput`; старая уже
/// `superseded`/`retracted` → `Conflict`; нет или удалена → `NotFound`.
pub async fn supersede_entry(
    db: &Db,
    old: Id,
    new: NewEntry,
    a: &Attribution,
) -> Result<(Entry, Entry), StoreError> {
    let a = a.clone();
    db.write(move |tx| {
        let o = live_entry(tx, old)?;
        if o.kind != EntryKind::Insight || new.kind != EntryKind::Insight {
            return Err(bad("замещаются только insight"));
        }
        if new.space_id != o.space_id {
            return Err(bad("новая запись — в пространстве замещаемой"));
        }
        let key = check_key(new.idempotency_key.clone())?.map(|k| scoped_key(&a, &k));
        if let Some(k) = &key
            && let Some(ex) = by_key(tx, k)?
        {
            if ex.supersedes_id != Some(old) {
                return Err(StoreError::IdempotencyMismatch(
                    "Idempotency-Key уже использован другим запросом".into(),
                ));
            }
            let ex = same_request(tx, ex, &new)?;
            return Ok((ex, load_entry(tx, old)?));
        }
        if let Some(s @ (EntryStatus::Superseded | EntryStatus::Retracted)) = o.status {
            return Err(StoreError::Conflict(format!(
                "запись {}: замещение невозможно",
                s.as_str()
            )));
        }
        let now = fmt_ts(Timestamp::now());
        let nid = insert_entry(tx, new, key, &a, Some(old), &now)?;
        tx.execute(
            "UPDATE entries SET superseded_by_id = ?2 WHERE id = ?1",
            params![old, nid],
        )?;
        let mut f = Fields::of(&o);
        f.status = Some(EntryStatus::Superseded);
        save(tx, &o, &f, None, &a, &now)?;
        Ok((load_entry(tx, nid)?, load_entry(tx, old)?))
    })
    .await
}

/// Отзывает insight с обязательной причиной: статус `retracted`, `retract_reason`, ревизия
/// с причиной, переиндексация. Пустая причина или не insight → `BadInput`; уже отозвана →
/// `Conflict`; нет или удалена → `NotFound`.
pub async fn retract_entry(
    db: &Db,
    id: Id,
    reason: &str,
    a: &Attribution,
) -> Result<Entry, StoreError> {
    let reason = check_reason(Some(reason.to_string()))?.expect("причина задана");
    let a = a.clone();
    db.write(move |tx| {
        let cur = live_entry(tx, id)?;
        if cur.kind != EntryKind::Insight {
            return Err(bad("отзываются только insight"));
        }
        if cur.status == Some(EntryStatus::Retracted) {
            return Err(StoreError::Conflict("запись уже отозвана".into()));
        }
        let now = fmt_ts(Timestamp::now());
        tx.execute(
            "UPDATE entries SET retract_reason = ?2 WHERE id = ?1",
            params![id, reason],
        )?;
        let mut f = Fields::of(&cur);
        f.status = Some(EntryStatus::Retracted);
        save(tx, &cur, &f, Some(&reason), &a, &now)?;
        load_entry(tx, id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_with_blank_line() {
        assert_eq!(appended("a", "b"), "a\n\nb");
        assert_eq!(appended("a\n\n\n", "b"), "a\n\nb");
        assert_eq!(appended("  ", "b"), "b");
    }

    #[test]
    fn tags_and_area_normalized() {
        assert_eq!(
            check_tags(vec![" a ".into(), "".into(), "a".into(), "b".into()]).unwrap(),
            vec!["a", "b"]
        );
        assert!(check_tags(vec!["x".repeat(101)]).is_err());
        assert_eq!(check_area(Some("  ".into())).unwrap(), None);
        assert_eq!(
            check_area(Some(" web ".into())).unwrap().as_deref(),
            Some("web")
        );
        assert!(check_area(Some("a\tb".into())).is_err());
        assert!(check_reason(Some(" ".into())).is_err());
        assert!(check_key(Some("".into())).is_err());
    }
}
