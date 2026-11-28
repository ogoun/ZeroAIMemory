//! Уборка памяти — сборщик кандидатов (M7.5 §8.7 п. 1; кандидат К5 обзора claude-note без
//! автоматики): детерминированно, **без модели и без инференса**, только записи решений и
//! Knowledge **владельца базы** (запись, к которой приложил руку участник общего решения, и
//! записи Human не берутся никогда). Сборщик ничего не меняет в знаниях: его кандидаты становятся
//! работой вида `tidy`, итог работы — предложениями человеку (`proposals` типа `tidy`), а
//! применяет их только человек (сервер, `POST /proposals/{id}/accept`).
//!
//! **Типы кандидатов** ([`CandidateType`], порядок при срезе — порядок перечисления):
//! - **неразрешённые противоречия** — связи `contradicts` между записями старше
//!   [`CONTRADICTS_MIN_DAYS`] суток, оба конца действующие;
//! - **непроходящие пробы** — цель пробы не нашлась ни в бюджете, ни в глубоком поиске
//!   [`PROBE_FAIL_RUNS`] последних законченных ночных прогона подряд (`check_results`);
//! - **устаревшие** — insight с пометкой «возможно устарело» (ссылка на файл `stale`, ТЗ §7.10)
//!   или в статусе `revisit` [`REVISIT_MIN_DAYS`] суток и дольше;
//! - **почти-дубли** — пары действующих insight одного пространства, вида и области с
//!   dense-косинусом в `[NEAR_MIN; insights.dedup_threshold)` ([`near_window`]; порог не выше
//!   [`NEAR_MIN`] — окно пусто, WARN и пропуск); вектор — из кэша `embeddings` по ключу чанка
//!   записи (L1 записи, иначе первый лист), без инференса; запись проверяется против всех записей
//!   своей группы, окно записей ночи — [`window`] (курсор `meta` [`CURSOR_KEY`]); у терминов —
//!   тот же нормализованный заголовок или псевдоним у термина решения и термина Knowledge (в
//!   одном пространстве имена терминов и так уникальны);
//! - **шумные** — `shown ≥ 10`, `opened = 0`, `useful = 0` по показам за
//!   [`crate::NOISY_WINDOW_DAYS`] суток (`zam_store::noisy_ids`); первые 30 суток после начала
//!   учёта открытий (`meta.usage_opened_since`, M7.3) их нет.
//!
//! **Отсев**: запись — не больше чем в одном кандидате за сбор (первый по порядку типов);
//! кандидат с парой «ссылка, ревизия» из `exclude` (отправленное в этом запуске, принятое,
//! выброшенное вторым сканом) — нет; по [`dedup_key`] которого уже есть предложение (кроме
//! отозванного) — нет; отклонённый человеком — по ссылкам в любой ревизии ([`declined_key`]).
//! Открытые предложения, чья запись сменила ревизию, сборщик помечает `stale`
//! ([`crate::proposals::mark_stale_tidy`]) — кандидат возвращается с новыми ревизиями.

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::{SignedDuration, Timestamp};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zam_core::{Id, TargetKind};

use crate::entries::{Certainty, ENTRY_COLS, Entry, EntryKind, EntryStatus, entry_row};
use crate::proposals::{self as pr, ProposalType};
use crate::time::{fmt_ts, now, parse_ts};
use crate::{Db, StoreError};

/// Ключ `meta` курсора почти-дублей: `{"at": <время прошлого сбора>, "pos": <id>}`.
pub const CURSOR_KEY: &str = "tidy.cursor";
/// Кандидатов уборки за ночь на пользователя — не больше (§8.7 п. 1).
pub const NIGHT_MAX: usize = 20;
/// Записей окна почти-дублей за сбор — не больше (первая ночь — первые по id, дальше —
/// изменённые после прошлого сбора и по кругу остальные).
pub const WINDOW_RECORDS: usize = 200;
/// Нижняя граница окна dense-косинуса почти-дублей (включительно).
pub const NEAR_MIN: f32 = 0.80;
/// Связь `contradicts` — кандидат, если старше стольких суток.
pub const CONTRADICTS_MIN_DAYS: i64 = 3;
/// Insight в статусе `revisit` — кандидат, если в нём столько суток и дольше.
pub const REVISIT_MIN_DAYS: i64 = 14;
/// Непроходящая проба — столько последних законченных ночных прогонов подряд без цели.
pub const PROBE_FAIL_RUNS: usize = 3;
/// Вопросов непроходящих проб в сигналах кандидата — не больше.
pub const PROBE_QUESTIONS_MAX: usize = 2;
/// Текст записи (L1) кандидата — не длиннее, символов: две записи с сигналами помещаются в
/// элемент пакета (1500 символов, §6.3 п. 3).
pub const L1_MAX_CHARS: usize = 650;

/// Тип кандидата уборки; порядок перечисления — порядок при срезе до [`NIGHT_MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType {
    /// Неразрешённое противоречие (две записи).
    Contradiction,
    /// Непроходящая проба (запись-цель).
    ProbeFail,
    /// Устаревшая запись.
    Stale,
    /// Почти-дубль (две записи).
    NearDuplicate,
    /// Шумная запись.
    Noisy,
}

impl CandidateType {
    /// Все типы в порядке среза.
    pub const ALL: [CandidateType; 5] = [
        CandidateType::Contradiction,
        CandidateType::ProbeFail,
        CandidateType::Stale,
        CandidateType::NearDuplicate,
        CandidateType::Noisy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CandidateType::Contradiction => "contradiction",
            CandidateType::ProbeFail => "probe_fail",
            CandidateType::Stale => "stale",
            CandidateType::NearDuplicate => "near_duplicate",
            CandidateType::Noisy => "noisy",
        }
    }

    pub fn parse(s: &str) -> Option<CandidateType> {
        CandidateType::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// Операция кандидата уборки (§8.7 п. 2). Удаления нет: операции замещают, отзывают, меняют
/// статус и триггеры, дописывают, заводят термин или псевдонимы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Merge,
    Retract,
    Revisit,
    Triggers,
    Term,
    Append,
    Keep,
}

impl Op {
    pub const ALL: [Op; 7] = [
        Op::Merge,
        Op::Retract,
        Op::Revisit,
        Op::Triggers,
        Op::Term,
        Op::Append,
        Op::Keep,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Merge => "merge",
            Op::Retract => "retract",
            Op::Revisit => "revisit",
            Op::Triggers => "triggers",
            Op::Term => "term",
            Op::Append => "append",
            Op::Keep => "keep",
        }
    }

    pub fn parse(s: &str) -> Option<Op> {
        Op::ALL.into_iter().find(|o| o.as_str() == s)
    }
}

/// Запись кандидата: ссылка и ревизия, пространство, вид, текст (L1) и что нужно проверке
/// операций и карточке (статус, достоверность, псевдонимы термина).
#[derive(Debug, Clone, PartialEq)]
pub struct TidyRecord {
    pub id: Id,
    /// `i:<id>` | `t:<id>`.
    pub target: String,
    pub rev: i64,
    pub space: Id,
    pub kind: EntryKind,
    /// `data.kind` insight.
    pub insight_kind: Option<String>,
    pub area: Option<String>,
    pub title: String,
    /// L1 записи (чанк уровня `note`), иначе тело; исходник, не длиннее [`L1_MAX_CHARS`].
    pub l1: String,
    pub status: Option<EntryStatus>,
    pub certainty: Option<Certainty>,
    /// Псевдонимы термина.
    pub aliases: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Кандидат уборки: тип, пространство работы, записи (первая — основная) и сигналы вида
/// (`имя`, `значение`).
#[derive(Debug, Clone, PartialEq)]
pub struct TidyCandidate {
    pub ctype: CandidateType,
    pub space: Id,
    pub records: Vec<TidyRecord>,
    pub signals: Vec<(String, String)>,
}

impl TidyCandidate {
    /// Пары «ссылка, ревизия» записей.
    pub fn refs(&self) -> Vec<(String, i64)> {
        self.records
            .iter()
            .map(|r| (r.target.clone(), r.rev))
            .collect()
    }

    /// Допустимые операции кандидата (§8.7 п. 2): по типу и записям.
    pub fn ops(&self) -> Vec<Op> {
        ops_for(self.ctype, &self.records)
    }
}

/// Допустимые операции кандидата типа `t` с записями `records`: `merge` — только у двух insight,
/// `revisit` — только у действующего (`active`) insight, `triggers` — только у insight, `term` —
/// у непроходящей пробы и почти-дубля терминов; `keep` — всегда.
pub fn ops_for(t: CandidateType, records: &[TidyRecord]) -> Vec<Op> {
    let insights = records.iter().all(|r| r.kind == EntryKind::Insight);
    let any_active = records
        .iter()
        .any(|r| r.kind == EntryKind::Insight && r.status == Some(EntryStatus::Active));
    let mut out = match t {
        CandidateType::Contradiction => vec![Op::Merge, Op::Retract, Op::Revisit, Op::Append],
        CandidateType::ProbeFail if insights => vec![Op::Triggers, Op::Append, Op::Term],
        CandidateType::ProbeFail => vec![Op::Term, Op::Append],
        CandidateType::Stale => vec![Op::Retract, Op::Revisit, Op::Append],
        CandidateType::NearDuplicate if insights => vec![Op::Merge, Op::Retract],
        CandidateType::NearDuplicate => vec![Op::Term, Op::Retract],
        CandidateType::Noisy => vec![Op::Triggers, Op::Retract, Op::Revisit],
    };
    if !(insights && records.len() == 2) {
        out.retain(|o| *o != Op::Merge);
    }
    if !any_active {
        out.retain(|o| *o != Op::Revisit);
    }
    if !insights {
        out.retain(|o| *o != Op::Triggers);
    }
    out.push(Op::Keep);
    out
}

/// Ключ отсева кандидата (`proposals.dedup_key`): тип кандидата, ссылки и ревизии записей.
pub fn dedup_key(ctype: CandidateType, refs: &[(String, i64)]) -> String {
    pr::key_refs_rev(ProposalType::Tidy, ctype.as_str(), refs)
}

/// Ключ «отклонено человеком»: тип кандидата и ссылки без ревизий.
pub fn declined_key(ctype: CandidateType, refs: &[String]) -> String {
    pr::key_refs(ProposalType::Tidy, ctype.as_str(), refs)
}

/// Окно dense-косинуса почти-дублей `[NEAR_MIN; threshold)`; порог не выше [`NEAR_MIN`] —
/// `None` (окно пусто).
pub fn near_window(threshold: f32) -> Option<(f32, f32)> {
    (threshold > NEAR_MIN).then_some((NEAR_MIN, threshold))
}

/// Косинус двух векторов (длина разная или нулевая норма — 0).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

/// Курсор окна почти-дублей (`meta` [`CURSOR_KEY`]): время прошлого сбора и последняя запись,
/// взятая по кругу.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub at: Option<String>,
    pub pos: Option<Id>,
}

/// Окно записей почти-дублей сбора: `records` — `(id, изменена)` в любом порядке. Без курсора
/// (первая ночь) — первые `max` по id; с курсором — изменённые после `cursor.at` (по id), затем по
/// кругу остальные после `cursor.pos`, всего не больше `max`. Новый курсор — `at = now`, `pos` —
/// последняя взятая по кругу запись (по кругу ничего — прежний).
pub fn window(
    records: &[(Id, Timestamp)],
    cursor: Option<&Cursor>,
    max: usize,
    now: Timestamp,
) -> (Vec<Id>, Cursor) {
    let mut all: Vec<(Id, Timestamp)> = records.to_vec();
    all.sort_by_key(|(id, _)| *id);
    let at = Some(fmt_ts(now));
    let Some(c) = cursor else {
        let take: Vec<Id> = all.iter().take(max).map(|(id, _)| *id).collect();
        let pos = take.last().copied();
        return (take, Cursor { at, pos });
    };
    let since = c.at.as_deref().and_then(|s| parse_ts(s).ok());
    let mut out: Vec<Id> = all
        .iter()
        .filter(|(_, u)| since.is_some_and(|s| *u > s))
        .map(|(id, _)| *id)
        .take(max)
        .collect();
    let taken: HashSet<Id> = out.iter().copied().collect();
    let start = c
        .pos
        .map_or(0, |p| all.iter().position(|(id, _)| *id > p).unwrap_or(0));
    let mut pos = c.pos;
    for i in 0..all.len() {
        if out.len() >= max {
            break;
        }
        let id = all[(start + i) % all.len()].0;
        if !taken.contains(&id) {
            out.push(id);
            pos = Some(id);
        }
    }
    (out, Cursor { at, pos })
}

/// Параметры сбора.
#[derive(Debug, Clone)]
pub struct Collect {
    /// Владелец базы: берутся только его записи.
    pub owner: Id,
    /// Пространства сбора (решения и Knowledge; Human сюда не передаётся).
    pub spaces: Vec<Id>,
    /// Модель dense-векторов кэша `embeddings`; нет — почти-дубли insight не ищутся.
    pub model: Option<String>,
    /// `insights.dedup_threshold` — верхняя граница окна почти-дублей.
    pub dedup_threshold: f32,
    /// Кандидатов — не больше.
    pub limit: usize,
    /// Пары «ссылка, ревизия», которые не берутся (отсев запуска вида).
    pub exclude: HashSet<(String, i64)>,
    /// Ключи отклонённых человеком ([`declined_key`]).
    pub declined: HashSet<String>,
}

/// Действующие записи сбора (insight `active`/`revisit`, термины `active`) пространств
/// `spaces`, у которых автор **каждой** ревизии — владелец `owner` или не указан, и ключ вектора
/// записи (L1, иначе первый лист).
struct Loaded {
    records: Vec<TidyRecord>,
    keys: HashMap<Id, String>,
}

/// Записи сбора пространств `spaces` (`None` — любых) или только записи `only`.
fn load(
    c: &rusqlite::Connection,
    owner: Id,
    spaces: Option<&[Id]>,
    only: Option<&[Id]>,
) -> Result<Loaded, StoreError> {
    let json = |v: Option<&[Id]>| {
        v.map(|v| Value::from(v.iter().map(Id::to_string).collect::<Vec<_>>()).to_string())
    };
    let mut st = c.prepare_cached(&format!(
        "SELECT {ENTRY_COLS} FROM entries e
         WHERE (?1 IS NULL OR e.space_id IN (SELECT value FROM json_each(?1)))
           AND (?3 IS NULL OR e.id IN (SELECT value FROM json_each(?3)))
           AND e.deleted_at IS NULL
           AND ((e.kind = 'insight' AND e.status IN ('active', 'revisit'))
                OR (e.kind = 'term' AND e.status = 'active'))
           AND (e.user_id IS NULL OR e.user_id = ?2)
           AND NOT EXISTS (SELECT 1 FROM entry_revisions r WHERE r.entry_id = e.id
                           AND r.user_id IS NOT NULL AND r.user_id != ?2)
         ORDER BY e.created_at, e.id"
    ))?;
    let entries: Vec<Entry> = st
        .query_map(params![json(spaces), owner, json(only)], entry_row)?
        .collect::<Result<_, _>>()?;
    let mut ch = c.prepare_cached(
        "SELECT level, ordinal, text, embed_key FROM chunks
         WHERE entry_id = ?1 AND level IN ('note', 'chunk') ORDER BY rev DESC, ordinal",
    )?;
    let mut records = Vec::with_capacity(entries.len());
    let mut keys = HashMap::new();
    for e in entries {
        let rows: Vec<(String, i64, String, Option<String>)> = ch
            .query_map([e.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let note = rows.iter().find(|r| r.0 == "note");
        let leaf = rows.iter().find(|r| r.0 == "chunk");
        if let Some(k) = note.or(leaf).and_then(|r| r.3.clone()) {
            keys.insert(e.id, k);
        }
        let l1 = note.map_or(e.body.as_str(), |r| r.2.as_str());
        let aliases = if e.kind == EntryKind::Term {
            e.data["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect()
        } else {
            Vec::new()
        };
        records.push(TidyRecord {
            id: e.id,
            target: format!("{}:{}", e.kind.ref_letter(), e.id),
            rev: e.rev,
            space: e.space_id,
            kind: e.kind,
            insight_kind: e.data["kind"].as_str().map(str::to_string),
            area: crate::normalize_area(e.area.as_deref()),
            title: e.title.clone(),
            l1: cut(l1.trim(), L1_MAX_CHARS),
            status: e.status,
            certainty: e.certainty,
            aliases,
            created_at: e.created_at,
            updated_at: e.updated_at,
        });
    }
    Ok(Loaded { records, keys })
}

/// Действующие записи `ids` владельца `owner` в виде записей кандидата (проверка итога работы и
/// карточка предложения): удалённые, замещённые, отозванные и записи с правкой участника в ответ
/// не входят.
pub async fn records(db: &Db, owner: Id, ids: &[Id]) -> Result<Vec<TidyRecord>, StoreError> {
    let ids = ids.to_vec();
    db.read(move |c| Ok(load(c, owner, None, Some(&ids))?.records))
        .await
}

/// Текст не длиннее `max` символов (обрезка — с «…»).
fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn sig(k: &str, v: impl ToString) -> (String, String) {
    (k.to_string(), v.to_string())
}

/// Противоречия: связи `contradicts` между записями сбора старше [`CONTRADICTS_MIN_DAYS`]
/// суток; пространство кандидата — источника связи.
fn contradictions(
    c: &rusqlite::Connection,
    by_id: &HashMap<Id, &TidyRecord>,
    at: Timestamp,
) -> Result<Vec<TidyCandidate>, StoreError> {
    let before = fmt_ts(at - SignedDuration::from_hours(24 * CONTRADICTS_MIN_DAYS));
    let mut st = c.prepare_cached(
        "SELECT src_id, dst_id, created_at FROM edges
         WHERE type = 'contradicts' AND deleted_at IS NULL AND src_kind = 'entry'
           AND dst_kind = 'entry' AND dst_id IS NOT NULL AND created_at <= ?1
         ORDER BY created_at, id",
    )?;
    let rows: Vec<(Id, Id, String)> = st
        .query_map([before], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (s, d, created) in rows {
        let (Some(a), Some(b)) = (by_id.get(&s), by_id.get(&d)) else {
            continue;
        };
        if a.id == b.id {
            continue;
        }
        let days = parse_ts(&created)
            .map(|t| at.duration_since(t).as_hours() / 24)
            .unwrap_or(0);
        out.push(TidyCandidate {
            ctype: CandidateType::Contradiction,
            space: a.space,
            records: vec![(*a).clone(), (*b).clone()],
            signals: vec![sig("contradicts_days", days)],
        });
    }
    Ok(out)
}

/// Устаревшие: insight с ссылкой на файл `stale` или в `revisit` не меньше
/// [`REVISIT_MIN_DAYS`] суток (с первой ревизии непрерывного хвоста `revisit`).
fn stale(
    c: &rusqlite::Connection,
    records: &[TidyRecord],
    at: Timestamp,
) -> Result<Vec<TidyCandidate>, StoreError> {
    let mut refs = c.prepare_cached(
        "SELECT count(*) FROM file_refs
         WHERE owner_kind = 'entry' AND owner_id = ?1 AND state = 'stale'",
    )?;
    let mut revs = c.prepare_cached(
        "SELECT status, ts FROM entry_revisions WHERE entry_id = ?1 ORDER BY rev DESC",
    )?;
    let mut out = Vec::new();
    for r in records.iter().filter(|r| r.kind == EntryKind::Insight) {
        let n: i64 = refs.query_row([r.id], |x| x.get(0))?;
        let mut signals = Vec::new();
        if n > 0 {
            signals.push(sig("stale_refs", n));
        }
        if r.status == Some(EntryStatus::Revisit) {
            let mut since: Option<Timestamp> = None;
            let rows: Vec<(Option<String>, String)> = revs
                .query_map([r.id], |x| Ok((x.get(0)?, x.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (status, ts) in rows {
                if status.as_deref() != Some("revisit") {
                    break;
                }
                since = parse_ts(&ts).ok();
            }
            let days = since.map_or(0, |s| at.duration_since(s).as_hours() / 24);
            if days >= REVISIT_MIN_DAYS {
                signals.push(sig("revisit_days", days));
            }
        }
        if !signals.is_empty() {
            out.push(TidyCandidate {
                ctype: CandidateType::Stale,
                space: r.space,
                records: vec![r.clone()],
                signals,
            });
        }
    }
    Ok(out)
}

/// Читает курсор почти-дублей (повреждённый — как нет курсора, WARN).
fn read_cursor(c: &rusqlite::Connection) -> Result<Option<Cursor>, StoreError> {
    let raw: Option<String> = c
        .query_row("SELECT value FROM meta WHERE key = ?1", [CURSOR_KEY], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(raw.and_then(|s| match serde_json::from_str(&s) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "уборка: курсор почти-дублей не разобран — как первая ночь");
            None
        }
    }))
}

/// Пишет курсор почти-дублей.
async fn write_cursor(db: &Db, cur: &Cursor) -> Result<(), StoreError> {
    let v = serde_json::to_string(cur).map_err(|e| StoreError::BadInput(format!("курсор: {e}")))?;
    db.write(move |t| {
        t.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![CURSOR_KEY, v],
        )?;
        Ok(())
    })
    .await
}

/// Почти-дубли insight: окно записей ([`window`]) против всех записей своей группы
/// «пространство, вид, область»; пары с косинусом в окне `[lo; hi)`.
fn near_insights(
    records: &[TidyRecord],
    vectors: &HashMap<Id, Vec<f32>>,
    take: &[Id],
    (lo, hi): (f32, f32),
) -> Vec<TidyCandidate> {
    type Group<'a> = (Id, Option<&'a str>, Option<&'a str>);
    let mut groups: BTreeMap<Group<'_>, Vec<&TidyRecord>> = BTreeMap::new();
    for r in records
        .iter()
        .filter(|r| r.kind == EntryKind::Insight && vectors.contains_key(&r.id))
    {
        groups
            .entry((r.space, r.insight_kind.as_deref(), r.area.as_deref()))
            .or_default()
            .push(r);
    }
    let by_id: HashMap<Id, &TidyRecord> = records.iter().map(|r| (r.id, r)).collect();
    let mut seen: HashSet<(Id, Id)> = HashSet::new();
    let mut out = Vec::new();
    for id in take {
        let Some(a) = by_id.get(id) else { continue };
        let Some(va) = vectors.get(id) else { continue };
        let key = (a.space, a.insight_kind.as_deref(), a.area.as_deref());
        for b in groups.get(&key).into_iter().flatten() {
            if b.id == a.id {
                continue;
            }
            let pair = if a.id < b.id {
                (a.id, b.id)
            } else {
                (b.id, a.id)
            };
            if !seen.insert(pair) {
                continue;
            }
            let cos = cosine(va, &vectors[&b.id]);
            if cos >= lo && cos < hi {
                // старшая запись — первой
                let (x, y) = if (a.created_at, a.id) <= (b.created_at, b.id) {
                    (*a, *b)
                } else {
                    (*b, *a)
                };
                out.push(TidyCandidate {
                    ctype: CandidateType::NearDuplicate,
                    space: x.space,
                    records: vec![x.clone(), y.clone()],
                    signals: vec![sig("cosine", format!("{cos:.2}"))],
                });
            }
        }
    }
    out
}

/// Почти-дубли терминов: термин решения и термин Knowledge с общим нормализованным именем
/// (заголовок или псевдоним); пространство кандидата — решения.
fn near_terms(records: &[TidyRecord], knowledge: Option<Id>) -> Vec<TidyCandidate> {
    let Some(k) = knowledge else {
        return Vec::new();
    };
    let names = |r: &TidyRecord| -> Vec<String> {
        std::iter::once(r.title.as_str())
            .chain(r.aliases.iter().map(String::as_str))
            .map(crate::normalize_title)
            .filter(|n| !n.is_empty())
            .collect()
    };
    let terms: Vec<&TidyRecord> = records
        .iter()
        .filter(|r| r.kind == EntryKind::Term)
        .collect();
    let mut out = Vec::new();
    for a in terms.iter().filter(|r| r.space != k) {
        let na = names(a);
        for b in terms.iter().filter(|r| r.space == k) {
            if let Some(n) = names(b).into_iter().find(|n| na.contains(n)) {
                out.push(TidyCandidate {
                    ctype: CandidateType::NearDuplicate,
                    space: a.space,
                    records: vec![(*a).clone(), (*b).clone()],
                    signals: vec![sig("same_name", n)],
                });
            }
        }
    }
    out
}

/// Сбор кандидатов уборки (см. модуль): все типы, отсев, порядок типов, срез до `limit`. Пишет
/// только служебное: курсор почти-дублей ([`CURSOR_KEY`]) и пометку `stale` открытых
/// предложений, чья запись сменила ревизию.
pub async fn collect(db: &Db, p: Collect) -> Result<Vec<TidyCandidate>, StoreError> {
    if p.limit == 0 || p.spaces.is_empty() {
        return Ok(Vec::new());
    }
    pr::mark_stale_tidy(db).await?;
    let at = now();
    let (owner, spaces) = (p.owner, p.spaces.clone());
    let knowledge = crate::list_spaces(db)
        .await?
        .into_iter()
        .find(|s| s.kind == crate::SpaceKind::Knowledge && spaces.contains(&s.id))
        .map(|s| s.id);
    let (loaded, cursor, mut cands) = db
        .read(move |c| {
            let l = load(c, owner, Some(&spaces), None)?;
            let by_id: HashMap<Id, &TidyRecord> = l.records.iter().map(|r| (r.id, r)).collect();
            let mut cands = contradictions(c, &by_id, at)?;
            cands.extend(stale(c, &l.records, at)?);
            let cursor = read_cursor(c)?;
            Ok((l, cursor, cands))
        })
        .await?;
    cands.extend(probe_fails(db, &loaded.records).await?);
    // почти-дубли insight: окно, векторы из кэша, курсор
    match (near_window(p.dedup_threshold), p.model.as_deref()) {
        (None, _) => {
            tracing::warn!(
                threshold = p.dedup_threshold,
                "уборка: insights.dedup_threshold не выше 0,80 — окно почти-дублей пусто, пропуск"
            );
        }
        (Some(_), None) => {
            tracing::debug!("уборка: модели векторов нет — почти-дубли insight не ищутся");
        }
        (Some(win), Some(model)) => {
            let ids: Vec<(Id, Timestamp)> = loaded
                .records
                .iter()
                .filter(|r| r.kind == EntryKind::Insight && loaded.keys.contains_key(&r.id))
                .map(|r| (r.id, r.updated_at))
                .collect();
            let keys: Vec<String> = loaded.keys.values().cloned().collect();
            let embs = crate::embeddings::get_embeddings(db, model, &keys).await?;
            let vectors: HashMap<Id, Vec<f32>> = loaded
                .keys
                .iter()
                .filter_map(|(id, k)| embs.get(k).map(|e| (*id, e.dense.clone())))
                .collect();
            let ids: Vec<(Id, Timestamp)> = ids
                .into_iter()
                .filter(|(id, _)| vectors.contains_key(id))
                .collect();
            let (take, next) = window(&ids, cursor.as_ref(), WINDOW_RECORDS, at);
            cands.extend(near_insights(&loaded.records, &vectors, &take, win));
            write_cursor(db, &next).await?;
        }
    }
    cands.extend(near_terms(&loaded.records, knowledge));
    cands.extend(noisy(db, &loaded.records, at).await?);
    // порядок: тип, затем старшая основная запись
    cands.sort_by(|a, b| {
        (a.ctype, a.records[0].created_at, a.records[0].id).cmp(&(
            b.ctype,
            b.records[0].created_at,
            b.records[0].id,
        ))
    });
    // отсев: исключённые пары, отклонённые по ссылкам, предложения с тем же ключом
    let keys: Vec<String> = cands
        .iter()
        .map(|x| dedup_key(x.ctype, &x.refs()))
        .collect();
    let existing = pr::existing_keys(db, ProposalType::Tidy, &keys).await?;
    let mut used: HashSet<Id> = HashSet::new();
    let mut out = Vec::new();
    for (x, key) in cands.into_iter().zip(keys) {
        if out.len() >= p.limit {
            break;
        }
        let refs = x.refs();
        let plain: Vec<String> = refs.iter().map(|(r, _)| r.clone()).collect();
        if refs.iter().any(|r| p.exclude.contains(r))
            || p.declined.contains(&declined_key(x.ctype, &plain))
            || existing.contains(&key)
            || x.records.iter().any(|r| used.contains(&r.id))
        {
            continue;
        }
        used.extend(x.records.iter().map(|r| r.id));
        out.push(x);
    }
    Ok(out)
}

/// Непроходящие пробы: цели проб (не посторонних вопросов), не найденные ни в бюджете, ни в
/// глубоком поиске [`PROBE_FAIL_RUNS`] последних законченных ночных прогонов подряд.
async fn probe_fails(db: &Db, records: &[TidyRecord]) -> Result<Vec<TidyCandidate>, StoreError> {
    let runs = crate::check::nightly_done(db, PROBE_FAIL_RUNS).await?;
    if runs.len() < PROBE_FAIL_RUNS {
        return Ok(Vec::new());
    }
    let ids: Vec<Id> = runs.iter().map(|r| r.id).collect();
    let results = crate::check::results_of(db, ids.clone()).await?;
    let mut fails: HashMap<Id, usize> = HashMap::new();
    for run in &ids {
        let mut seen = HashSet::new();
        for r in results.get(run).into_iter().flatten() {
            if r.search_rank.is_none() && r.deep_rank.is_none() && seen.insert(r.probe_id) {
                *fails.entry(r.probe_id).or_default() += 1;
            }
        }
    }
    let mut failing: Vec<Id> = fails
        .into_iter()
        .filter(|(_, n)| *n >= PROBE_FAIL_RUNS)
        .map(|(id, _)| id)
        .collect();
    failing.sort();
    if failing.is_empty() {
        return Ok(Vec::new());
    }
    let by_id: HashMap<Id, &TidyRecord> = records.iter().map(|r| (r.id, r)).collect();
    // цель → вопросы её проб (по id пробы)
    let mut by_target: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    for pb in crate::probes::probes_by_ids(db, failing).await? {
        if !pb.active || pb.kind == crate::probes::ProbeKind::Negative {
            continue;
        }
        for t in &pb.targets {
            if let Some(id) = t.id().filter(|id| by_id.contains_key(id)) {
                by_target.entry(id).or_default().push(pb.question.clone());
            }
        }
    }
    Ok(by_target
        .into_iter()
        .map(|(id, qs)| {
            let r = by_id[&id];
            let mut signals = vec![sig("probe_fails", PROBE_FAIL_RUNS)];
            signals.extend(
                qs.into_iter()
                    .take(PROBE_QUESTIONS_MAX)
                    .map(|q| sig("question", q)),
            );
            TidyCandidate {
                ctype: CandidateType::ProbeFail,
                space: r.space,
                records: vec![r.clone()],
                signals,
            }
        })
        .collect())
}

/// Шумные insight: `zam_store::noisy_ids` за [`crate::NOISY_WINDOW_DAYS`] суток — только когда
/// учёт открытий идёт не меньше этого срока; сигналы — показы за всё время.
async fn noisy(
    db: &Db,
    records: &[TidyRecord],
    at: Timestamp,
) -> Result<Vec<TidyCandidate>, StoreError> {
    let window = at - SignedDuration::from_hours(24 * crate::NOISY_WINDOW_DAYS);
    match crate::usage_opened_since(db).await? {
        Some(s) if s <= window => {}
        _ => return Ok(Vec::new()),
    }
    let noisy: HashSet<Id> = crate::noisy_ids(db, window).await?.into_iter().collect();
    let picked: Vec<&TidyRecord> = records
        .iter()
        .filter(|r| r.kind == EntryKind::Insight && noisy.contains(&r.id))
        .collect();
    let ids: Vec<Id> = picked.iter().map(|r| r.id).collect();
    let counts = crate::usage_counts_many(db, TargetKind::Entry, &ids).await?;
    Ok(picked
        .into_iter()
        .map(|r| {
            let u = counts.get(&r.id).cloned().unwrap_or_default();
            TidyCandidate {
                ctype: CandidateType::Noisy,
                space: r.space,
                records: vec![r.clone()],
                signals: vec![
                    sig("shown", u.shown),
                    sig("opened", u.opened),
                    sig("useful", u.useful),
                ],
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: EntryKind, status: EntryStatus) -> TidyRecord {
        let id = Id::new();
        TidyRecord {
            id,
            target: format!("{}:{id}", kind.ref_letter()),
            rev: 1,
            space: Id::new(),
            kind,
            insight_kind: Some("gotcha".into()),
            area: None,
            title: "t".into(),
            l1: "l1".into(),
            status: Some(status),
            certainty: None,
            aliases: Vec::new(),
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
        }
    }

    /// Окно косинуса `[0,80; порог)`: 0,79 — нет, 0,85 — да, 0,91 — нет (порог 0,90); порог
    /// не выше 0,80 — окна нет.
    #[test]
    fn near_window_and_cosine() {
        let (lo, hi) = near_window(0.90).unwrap();
        let inside = |c: f32| c >= lo && c < hi;
        assert!(!inside(0.79));
        assert!(inside(0.85));
        assert!(!inside(0.91));
        assert!(inside(0.80));
        assert!(!inside(0.90));
        // векторы: косинус считается по направлению, а не по длине
        let a = [1.0, 0.0];
        let b = [0.85 * 3.0, (1.0f32 - 0.85 * 0.85).sqrt() * 3.0];
        assert!((cosine(&a, &b) - 0.85).abs() < 1e-5);
        assert_eq!(cosine(&a, &[0.0, 0.0]), 0.0);
        assert_eq!(cosine(&a, &[1.0]), 0.0);
    }

    /// `dedup_threshold ≤ 0,80` — окно пусто (WARN и пропуск у сбора).
    #[test]
    fn near_window_empty_at_min() {
        assert_eq!(near_window(0.80), None);
        assert_eq!(near_window(0.5), None);
        assert_eq!(near_window(0.81), Some((0.80, 0.81)));
    }

    /// Первая ночь — не больше 200 записей (первые по id); дальше — изменённые после прошлого
    /// сбора и по кругу остальные после курсора, всего не больше 200; курсор — последняя взятая
    /// по кругу.
    #[test]
    fn tidy_first_night_cap_and_cursor() {
        let t0 = Timestamp::from_second(1_700_000_000).unwrap();
        let mut recs: Vec<(Id, Timestamp)> = (0..250).map(|_| (Id::new(), t0)).collect();
        recs.sort_by_key(|(id, _)| *id);
        let (first, c1) = window(&recs, None, WINDOW_RECORDS, t0);
        assert_eq!(first.len(), 200);
        assert_eq!(first[0], recs[0].0);
        assert_eq!(first[199], recs[199].0);
        assert_eq!(c1.pos, Some(recs[199].0));
        assert_eq!(c1.at.as_deref(), Some(fmt_ts(t0).as_str()));
        // курсор переживает JSON (`meta`)
        let back: Cursor = serde_json::from_str(&serde_json::to_string(&c1).unwrap()).unwrap();
        assert_eq!(back, c1);
        // вторая ночь: запись 5 изменена после прошлого сбора — первой, затем по кругу 200..249
        // и с начала
        let t1 = t0 + SignedDuration::from_hours(24);
        recs[5].1 = t0 + SignedDuration::from_hours(1);
        let (second, c2) = window(&recs, Some(&c1), WINDOW_RECORDS, t1);
        assert_eq!(second.len(), 200);
        assert_eq!(second[0], recs[5].0);
        assert_eq!(second[1], recs[200].0);
        assert_eq!(second[50], recs[249].0);
        assert_eq!(second[51], recs[0].0);
        assert!(
            second.iter().filter(|id| **id == recs[5].0).count() == 1,
            "изменённая — один раз"
        );
        assert_eq!(c2.pos, Some(recs[149].0));
        // записей меньше окна — все, по разу
        let few = &recs[..3];
        let (all, _) = window(few, Some(&c2), WINDOW_RECORDS, t1);
        assert_eq!(all.len(), 3);
        // пусто
        let (none, c3) = window(&[], Some(&c2), WINDOW_RECORDS, t1);
        assert!(none.is_empty());
        assert_eq!(c3.pos, c2.pos);
    }

    /// Порядок среза — противоречия, непроходящие пробы, устаревшие, почти-дубли, шумные.
    #[test]
    fn cut_order_of_types() {
        let mut v = vec![
            CandidateType::Noisy,
            CandidateType::NearDuplicate,
            CandidateType::Stale,
            CandidateType::ProbeFail,
            CandidateType::Contradiction,
        ];
        v.sort();
        assert_eq!(v, CandidateType::ALL.to_vec());
        for t in CandidateType::ALL {
            assert_eq!(CandidateType::parse(t.as_str()), Some(t));
            assert_eq!(
                serde_json::to_value(t).unwrap(),
                serde_json::json!(t.as_str())
            );
        }
    }

    /// Операции: удаления нет; `merge` — только у двух insight; `revisit` — у действующего
    /// insight; `keep` — всегда.
    #[test]
    fn ops_by_type_without_delete() {
        assert_eq!(Op::parse("delete"), None);
        assert_eq!(Op::parse("remove"), None);
        for o in Op::ALL {
            assert_eq!(Op::parse(o.as_str()), Some(o));
        }
        let a = rec(EntryKind::Insight, EntryStatus::Active);
        let b = rec(EntryKind::Insight, EntryStatus::Revisit);
        let ops = ops_for(CandidateType::NearDuplicate, &[a.clone(), b.clone()]);
        assert_eq!(ops, vec![Op::Merge, Op::Retract, Op::Keep]);
        let ops = ops_for(CandidateType::Stale, std::slice::from_ref(&b));
        assert_eq!(ops, vec![Op::Retract, Op::Append, Op::Keep]);
        let ops = ops_for(CandidateType::Stale, std::slice::from_ref(&a));
        assert_eq!(ops, vec![Op::Retract, Op::Revisit, Op::Append, Op::Keep]);
        let t = rec(EntryKind::Term, EntryStatus::Active);
        let ops = ops_for(CandidateType::NearDuplicate, &[t.clone(), t.clone()]);
        assert_eq!(ops, vec![Op::Term, Op::Retract, Op::Keep]);
        let ops = ops_for(CandidateType::ProbeFail, std::slice::from_ref(&t));
        assert_eq!(ops, vec![Op::Term, Op::Append, Op::Keep]);
        let ops = ops_for(CandidateType::Contradiction, &[a, b]);
        assert_eq!(
            ops,
            vec![Op::Merge, Op::Retract, Op::Revisit, Op::Append, Op::Keep]
        );
    }

    /// Ключи: ревизия меняет ключ отсева, но не ключ «отклонено».
    #[test]
    fn dedup_and_declined_keys() {
        let t = CandidateType::NearDuplicate;
        let r1 = [("i:a".to_string(), 1), ("i:b".to_string(), 1)];
        let r2 = [("i:a".to_string(), 2), ("i:b".to_string(), 1)];
        assert_ne!(dedup_key(t, &r1), dedup_key(t, &r2));
        let plain = ["i:a".to_string(), "i:b".to_string()];
        assert_eq!(declined_key(t, &plain), declined_key(t, &plain));
        assert_ne!(
            declined_key(t, &plain),
            declined_key(CandidateType::Contradiction, &plain)
        );
    }
}
