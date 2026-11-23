//! Число записей разделов пространства для WebUI (спецификация M7 §5.3, задача 4 M7.2): живые
//! записи журналов, открытые вопросы и задачи, заметки — для `GET /spaces/{space}/counts`.
//!
//! Число журнала — **живые записи** (без корзины, все статусы), то же условие, что у ленты
//! `GET /spaces/{space}/entries?kind=<вид>` без фильтров ([`crate::list_entries`]): число
//! совпадает с длиной ленты. Vault здесь не считается: Vault участника лежит в его базе, а не в
//! базе пространства, и сервер считает его тем же чтением, что `GET /spaces/{space}/vault`
//! зрителя.
//!
//! Цена — четыре чтения в одном снимке базы по существующим индексам (без миграций): виды — по
//! `space_id` частичного индекса живых записей `(space_id, kind, …)` с группировкой в порядке
//! индекса, открытые вопросы и задачи — по `(space_id, kind)` того же индекса, заметки — по
//! частичному индексу живых узлов, снимок карты кода — по первичному ключу (планы — модульный
//! тест `queries_use_indexes`).

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use zam_core::Id;

use crate::entries::EntryKind;
use crate::user_db::{Space, SpaceKind};
use crate::{Db, StoreError};

/// Что считает журнал.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// Живые записи вида, все статусы.
    Entries(EntryKind),
    /// Файлы снимка карты кода решения (`code_snapshots.files`).
    Code,
}

/// Журналы WebUI решения в порядке полосы журналов — `journalsOf('solution')`
/// (`web/src/lib/journals.ts`), без Vault.
const SOLUTION: [(&str, Source); 9] = [
    ("changelog", Source::Entries(EntryKind::Changelog)),
    ("releases", Source::Entries(EntryKind::Release)),
    ("reports", Source::Entries(EntryKind::Report)),
    ("code", Source::Code),
    ("insights", Source::Entries(EntryKind::Insight)),
    ("glossary", Source::Entries(EntryKind::Term)),
    ("tasks", Source::Entries(EntryKind::Task)),
    ("ideas", Source::Entries(EntryKind::Idea)),
    ("questions", Source::Entries(EntryKind::Question)),
];

/// Журналы WebUI Knowledge — `journalsOf('knowledge')`, без Vault: без Changelog, Releases,
/// Отчётов (пишутся только в решение) и Кода (карта — у решения).
const KNOWLEDGE: [(&str, Source); 5] = [
    ("insights", Source::Entries(EntryKind::Insight)),
    ("glossary", Source::Entries(EntryKind::Term)),
    ("tasks", Source::Entries(EntryKind::Task)),
    ("ideas", Source::Entries(EntryKind::Idea)),
    ("questions", Source::Entries(EntryKind::Question)),
];

/// Журналы вида пространства (без Vault); у Rules и Human журналов нет.
fn journals_of(kind: SpaceKind) -> &'static [(&'static str, Source)] {
    match kind {
        SpaceKind::Solution => &SOLUTION,
        SpaceKind::Knowledge => &KNOWLEDGE,
        SpaceKind::Rules | SpaceKind::Human => &[],
    }
}

/// Число записей разделов пространства ([`space_counts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceCounts {
    /// Имя журнала WebUI (`web/src/lib/router.ts`, `JOURNALS`) → число живых записей журнала
    /// (без корзины, все статусы); `code` — файлы карты кода. Только журналы вида пространства и
    /// без `vault` (его считает сервер по базе зрителя).
    pub journals: BTreeMap<&'static str, u64>,
    /// Вопросы `open`.
    pub open_questions: u64,
    /// Задачи `open`, `in_progress`, `blocked` (подзадачи тоже).
    pub open_tasks: u64,
    /// Живые заметки пространства, без папок.
    pub notes: u64,
}

/// Живые записи пространства по видам (`?1` — пространство).
const KINDS_SQL: &str = "SELECT kind, count(*) FROM entries
     WHERE space_id = ?1 AND deleted_at IS NULL
     GROUP BY kind";

/// Открытые вопросы и задачи пространства (`?1`).
const OPEN_SQL: &str = "SELECT
       (SELECT count(*) FROM entries
         WHERE space_id = ?1 AND kind = 'question' AND deleted_at IS NULL AND status = 'open'),
       (SELECT count(*) FROM entries
         WHERE space_id = ?1 AND kind = 'task' AND deleted_at IS NULL
           AND status IN ('open', 'in_progress', 'blocked'))";

/// Живые заметки пространства (`?1`) без папок.
const NOTES_SQL: &str = "SELECT count(*) FROM nodes
     WHERE space_id = ?1 AND deleted_at IS NULL AND kind = 'note'";

/// Файлы снимка карты кода решения (`?1` — GUID).
const CODE_SQL: &str = "SELECT files FROM code_snapshots WHERE solution_guid = ?1";

/// Число строк из `count(*)` (отрицательного не бывает).
fn n(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// Число записей разделов пространства `space` его базы `db` (спецификация M7 §5.3 п. 1):
/// журналы WebUI вида пространства (у решения — все, кроме Vault; у Knowledge — без Changelog,
/// Releases, Отчётов и Кода; у Rules и Human — нет), открытые вопросы и задачи, заметки.
/// `with_reports` — с журналом отчётов (сессия WebUI); токену отчёт не виден нигде (M5.5 §7) —
/// ключа `reports` нет. У пространства без журналов открытых вопросов и задач — 0.
pub async fn space_counts(
    db: &Db,
    space: &Space,
    with_reports: bool,
) -> Result<SpaceCounts, StoreError> {
    let journals: Vec<(&'static str, Source)> = journals_of(space.kind)
        .iter()
        .copied()
        .filter(|(_, s)| with_reports || *s != Source::Entries(EntryKind::Report))
        .collect();
    let (id, guid) = (space.id, space.solution_guid.clone());
    db.read(move |c| {
        // один снимок базы: числа разделов согласованы между собой
        let tx = c.unchecked_transaction()?;
        let mut out = SpaceCounts {
            notes: one(&tx, NOTES_SQL, id)?,
            ..SpaceCounts::default()
        };
        if journals.is_empty() {
            return Ok(out);
        }
        let by_kind: HashMap<String, u64> = tx
            .prepare_cached(KINDS_SQL)?
            .query_map(params![id], |r| Ok((r.get::<_, String>(0)?, n(r.get(1)?))))?
            .collect::<Result<_, _>>()?;
        let code = match &guid {
            Some(g) if journals.iter().any(|(_, s)| *s == Source::Code) => tx
                .prepare_cached(CODE_SQL)?
                .query_row(params![g], |r| r.get::<_, i64>(0))
                .optional()?
                .map_or(0, n),
            _ => 0,
        };
        for (name, src) in journals {
            let v = match src {
                Source::Entries(k) => by_kind.get(k.as_str()).copied().unwrap_or(0),
                Source::Code => code,
            };
            out.journals.insert(name, v);
        }
        let (q, t) = tx
            .prepare_cached(OPEN_SQL)?
            .query_row(params![id], |r| Ok((n(r.get(0)?), n(r.get(1)?))))?;
        out.open_questions = q;
        out.open_tasks = t;
        Ok(out)
    })
    .await
}

/// Одно число запроса `sql` с пространством `?1`.
fn one(c: &Connection, sql: &str, space: Id) -> Result<u64, StoreError> {
    Ok(c.prepare_cached(sql)?
        .query_row(params![space], |r| r.get::<_, i64>(0))
        .map(n)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for m in crate::USER_MIGRATIONS {
            c.execute_batch(m.sql).unwrap();
        }
        c
    }

    /// Строки `EXPLAIN QUERY PLAN` запроса с одним параметром.
    fn plan(c: &Connection, sql: &str) -> Vec<String> {
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        st.query_map(["x"], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Запросы чисел идут по существующим индексам (без миграций): виды — по `space_id`
    /// частичного индекса живых записей `(space_id, kind, …)` (`entries_feed` или `entries_title`
    /// — выбор планировщика) без временного B-дерева группировки, открытые — по `(space_id,
    /// kind)`, заметки — по частичному индексу живых узлов, снимок карты — по первичному ключу;
    /// полного просмотра таблиц нет.
    #[test]
    fn queries_use_indexes() {
        let c = schema();
        let kinds = plan(&c, KINDS_SQL);
        assert!(
            kinds.iter().any(|l| l.starts_with("SEARCH entries USING ")
                && l.contains("INDEX entries_")
                && l.ends_with("(space_id=?)")),
            "{kinds:?}"
        );
        assert!(
            !kinds.iter().any(|l| l.contains("TEMP B-TREE")),
            "{kinds:?}"
        );
        let open = plan(&c, OPEN_SQL);
        let by_kind = open
            .iter()
            .filter(|l| l.starts_with("SEARCH entries USING INDEX entries_"))
            .filter(|l| l.ends_with("(space_id=? AND kind=?)"))
            .count();
        assert_eq!(by_kind, 2, "{open:?}");
        let notes = plan(&c, NOTES_SQL);
        assert!(
            notes
                .iter()
                .any(|l| l.starts_with("SEARCH nodes USING INDEX nodes_live_")),
            "{notes:?}"
        );
        let code = plan(&c, CODE_SQL);
        assert!(
            code.iter().any(|l| l.starts_with("SEARCH code_snapshots")
                && l.ends_with("(solution_guid=?)")),
            "{code:?}"
        );
        // планы — в журнал этапа (`--nocapture`)
        println!("kinds: {kinds:?}\nopen: {open:?}\nnotes: {notes:?}\ncode: {code:?}");
        // `SCAN CONSTANT ROW` — внешний `SELECT` подзапросов без таблицы, не просмотр
        for l in kinds.iter().chain(&open).chain(&notes).chain(&code) {
            assert!(
                !l.starts_with("SCAN") || l == "SCAN CONSTANT ROW",
                "полный просмотр: {l}"
            );
        }
    }

    /// Журналы вида пространства из исходника WebUI: массив, который `journalsOf`
    /// (`web/src/lib/journals.ts`) возвращает при `kind === '<kind>'`.
    fn webui_journals(src: &str, kind: &str) -> Vec<String> {
        let body = src
            .split("export function journalsOf")
            .nth(1)
            .expect("journalsOf в journals.ts");
        let at = body
            .find(&format!("kind === '{kind}'"))
            .unwrap_or_else(|| panic!("ветка {kind} в journalsOf"));
        let rest = &body[at..];
        let open = rest.find('[').expect("[");
        let close = open + rest[open..].find(']').expect("]");
        rest[open + 1..close]
            .split(',')
            .map(|s| s.trim().trim_matches('\'').to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Журналы — те же и в том же порядке, что `journalsOf` WebUI: список сверяется с исходником
    /// `web/src/lib/journals.ts` (ревью r1, M5), у хранилища — без Vault (его считает сервер по
    /// базе зрителя); у Rules и Human журналов нет.
    #[test]
    fn journals_match_webui_source() {
        let p =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/lib/journals.ts");
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        for (kind, name) in [
            (SpaceKind::Solution, "solution"),
            (SpaceKind::Knowledge, "knowledge"),
        ] {
            let mut ours: Vec<String> = journals_of(kind)
                .iter()
                .map(|(n, _)| n.to_string())
                .collect();
            ours.push("vault".into());
            assert_eq!(ours, webui_journals(&src, name), "{name}");
        }
        assert!(
            journals_of(SpaceKind::Rules).is_empty() && journals_of(SpaceKind::Human).is_empty()
        );
    }
}
