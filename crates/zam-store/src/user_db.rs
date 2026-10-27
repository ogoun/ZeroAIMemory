//! Базы пользователей: `users/<id>.db`, ленивое открытие, фиксированные пространства (§6.3 ТЗ).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use jiff::Timestamp;
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use zam_core::{Id, SolutionGuid};

use crate::jobs::{JobOp, PRIORITY_REINDEX};
use crate::time::fmt_ts;
use crate::{Db, Migration, StoreError};

/// Миграции пользовательской БД.
pub static USER_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/user/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/user/0002_notes.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/user/0003_revisions_append_only.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/user/0004_index.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/user/0005_embed_key.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("../migrations/user/0006_pending_index.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("../migrations/user/0007_attribution.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("../migrations/user/0008_entries.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("../migrations/user/0009_node_certainty.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("../migrations/user/0010_edges.sql"),
    },
    Migration {
        version: 11,
        sql: include_str!("../migrations/user/0011_vault.sql"),
    },
    Migration {
        version: 12,
        sql: include_str!("../migrations/user/0012_live_children.sql"),
    },
];

/// Вид пространства.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceKind {
    Rules,
    Human,
    Knowledge,
    Solution,
}

impl SpaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceKind::Rules => "rules",
            SpaceKind::Human => "human",
            SpaceKind::Knowledge => "knowledge",
            SpaceKind::Solution => "solution",
        }
    }
}

impl ToSql for SpaceKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for SpaceKind {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "rules" => Ok(SpaceKind::Rules),
            "human" => Ok(SpaceKind::Human),
            "knowledge" => Ok(SpaceKind::Knowledge),
            "solution" => Ok(SpaceKind::Solution),
            other => Err(rusqlite::types::FromSqlError::Other(
                format!("неизвестный вид пространства: {other}").into(),
            )),
        }
    }
}

/// Пространство пользователя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Space {
    pub id: Id,
    pub kind: SpaceKind,
    /// GUID решения; в JSON API — `agent_solution_id` (§11.6).
    #[serde(rename = "agent_solution_id")]
    pub solution_guid: Option<String>,
    pub name: String,
}

/// Фиксированные пространства, создаваемые в каждой пользовательской БД.
const FIXED: [(SpaceKind, &str); 3] = [
    (SpaceKind::Rules, "Rules"),
    (SpaceKind::Human, "Human"),
    (SpaceKind::Knowledge, "Knowledge"),
];

/// Кэш открытых пользовательских баз; дёшево клонируется.
#[derive(Clone, Debug)]
pub struct UserDbs {
    dir: PathBuf,
    /// Ячейка на пользователя: глобальный замок держится только на поиск ячейки, открытие
    /// (миграции, копия, дозаполнение ключей) идёт под замком своей ячейки и не задерживает
    /// другие базы.
    open: Arc<Mutex<HashMap<Id, Slot>>>,
}

type Slot = Arc<Mutex<Option<OpenDb>>>;

/// Открытая база и признак завершения разовых фоновых задач открытия (перечанковка R1,
/// дозаполнение рёбер графа).
#[derive(Debug)]
struct OpenDb {
    db: Db,
    /// `true` — фоновые задачи не нужны или уже завершились (успешно или с ошибкой).
    rechunked: watch::Receiver<bool>,
    /// Перенарезка при открытии и её ход (`None` — не понадобилась).
    rechunk: Option<Arc<RechunkRun>>,
}

/// Почему база перенарезается при открытии (уведомление о перенарезке, §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RechunkReason {
    /// Изменились ключи `[index]` (пороги и флаги нарезки, L0 в префиксе).
    Params,
    /// Другая версия алгоритма нарезки (в том числе база M1 до чанков §8.2).
    Algorithm,
    /// Другой токенизатор (или чанки по оценке, а счётчик теперь точный).
    Tokenizer,
    /// База M3 без сохранённых параметров нарезки: причину по отпечатку не установить.
    Unknown,
}

impl RechunkReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RechunkReason::Params => "params",
            RechunkReason::Algorithm => "algorithm",
            RechunkReason::Tokenizer => "tokenizer",
            RechunkReason::Unknown => "unknown",
        }
    }
}

/// Уведомление о перенарезке базы при открытии (§8.2): причина, изменившиеся параметры —
/// имена ключей `[index]` (`chunk_boundary_lambda`, …), `algorithm` или `tokenizer` — и число
/// целей (живых заметок и записей журналов той части базы, что перенарезается).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RechunkNotice {
    pub reason: RechunkReason,
    pub changed: Vec<String>,
    pub targets: u64,
}

/// Ход идущей перенарезки базы: уведомление и сколько целей уже обработано (`done` не
/// больше `notice.targets`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RechunkProgress {
    pub notice: RechunkNotice,
    pub done: u64,
}

/// Перенарезка при открытии: уведомление, счётчик обработанных целей и признак «идёт».
#[derive(Debug)]
struct RechunkRun {
    notice: RechunkNotice,
    done: AtomicU64,
    running: AtomicBool,
}

impl UserDbs {
    /// `users_dir` — каталог `users/` из `DataDir::users_dir()`.
    pub fn new(users_dir: PathBuf) -> UserDbs {
        UserDbs {
            dir: users_dir,
            open: Arc::default(),
        }
    }

    /// База пользователя: открывается лениво (с миграцией и копией) и кэшируется.
    ///
    /// Чанки без `embed_key` (созданные до v5) получают ключ сразу, до возврата базы.
    ///
    /// Если база только что обновлена до v4, разовая перечанковка (R1) запускается
    /// фоновой задачей tokio один раз на открытие: `get` её не ждёт, записи во время
    /// неё безопасны (пакеты идемпотентны и читают текущую ревизию в своей транзакции).
    /// Так же, следом за ней, — дозаполнение рёбер графа базы, обновлённой до v10
    /// (`meta.edges_backfill`, задача 10 M3). Дождаться обеих — [`UserDbs::wait_rechunk`].
    pub async fn get(&self, user: Id) -> Result<Db, StoreError> {
        let slot = self.open.lock().await.entry(user).or_default().clone();
        // Открытие под замком ячейки: два одновременных вызова не создадут два Db на один
        // файл; другие базы при этом открываются параллельно.
        let mut cell = slot.lock().await;
        if let Some(e) = cell.as_ref() {
            return Ok(e.db.clone());
        }
        let db = Db::open(&self.dir.join(format!("{user}.db")), USER_MIGRATIONS)?;
        ensure_fixed_spaces(&db).await?;
        crate::nodes::backfill_embed_keys(&db).await?;
        // статус заметок из frontmatter (v9) — до первой записи: права certainty читают столбец
        crate::nodes::backfill_certainty(&db).await?;
        let pending = match rechunk_pending(&db).await? {
            Some(why) if !clear_rechunk_if_empty(&db).await? => Some(why),
            _ => None,
        };
        let run = pending.as_ref().map(|plan| {
            let n = &plan.notice;
            // одна строка на базу, без текста заметок (§8.2)
            tracing::warn!(
                reason = n.reason.as_str(),
                "перенарезка базы {user}: изменились {} — {} записей",
                changed_text(&n.changed),
                n.targets
            );
            Arc::new(RechunkRun {
                notice: n.clone(),
                done: AtomicU64::new(0),
                running: AtomicBool::new(true),
            })
        });
        let edges = crate::graph::backfill_pending(&db).await?;
        let secrets = secrets_scan_pending(&db).await?;
        // теги payload и вход эмбеддинга папок (v3): до возврата базы — воркер проверяет
        // отметку reindex при первом её открытии
        rescan_payload_secrets(&db).await?;
        let (done, rechunked) = watch::channel(pending.is_none() && !edges && !secrets);
        *cell = Some(OpenDb {
            db: db.clone(),
            rechunked,
            rechunk: run.clone(),
        });
        drop(cell);
        if pending.is_some() || edges || secrets {
            let bg = db.clone();
            tokio::spawn(async move {
                if let (Some(plan), Some(run)) = (pending, run) {
                    // «идёт» снимается и при панике задачи: баннер не повисает
                    let _running = RunningGuard(run.clone());
                    #[cfg(feature = "test-hooks")]
                    crate::test_hooks::rechunk_gate().await;
                    if let Err(e) = rechunk_all(&bg, &plan, &run.done).await {
                        // повторится при следующем открытии базы
                        tracing::warn!(error = %e, reason = plan.why.as_str(), "перечанковка заметок прервана");
                    }
                }
                if edges && let Err(e) = crate::graph::backfill_edges(&bg).await {
                    tracing::warn!(error = %e, "дозаполнение рёбер графа прервано");
                }
                if secrets && let Err(e) = rescan_secrets(&bg).await {
                    // повторится при следующем открытии базы
                    tracing::warn!(error = %e, "перенарезка заметок и записей с секретами прервана");
                }
                let _ = done.send(true);
            });
        }
        Ok(db)
    }

    /// Ждёт завершения разовых фоновых задач открытия базы `user` (перечанковка R1,
    /// дозаполнение рёбер графа), если она открыта и они идут; иначе возвращается сразу.
    pub async fn wait_rechunk(&self, user: Id) {
        let slot = self.open.lock().await.get(&user).cloned();
        let rx = match slot {
            Some(s) => s.lock().await.as_ref().map(|e| e.rechunked.clone()),
            None => None,
        };
        if let Some(mut rx) = rx {
            // ошибка — отправитель закрыт без `true` (задача прервана): ждать нечего
            let _ = rx.wait_for(|done| *done).await;
        }
    }

    /// Перенарезка при открытии базы `user` этим процессом (§8.2), идёт она или уже
    /// закончилась: уведомление и сколько целей обработано (`done` без ограничения сверху —
    /// по окончании равен `notice.targets`, если во время перенарезки цели не добавлялись).
    /// `None` — база не открыта или перенарезка не понадобилась.
    pub async fn opened_rechunk(&self, user: Id) -> Option<RechunkProgress> {
        let slot = self.open.lock().await.get(&user).cloned()?;
        let run = slot.lock().await.as_ref()?.rechunk.clone()?;
        Some(RechunkProgress {
            done: run.done.load(Ordering::Relaxed),
            notice: run.notice.clone(),
        })
    }

    /// Ход перенарезки базы `user`, начатой при её открытии этим процессом (§8.2): `None` —
    /// база не открыта, перенарезка не понадобилась или уже закончилась (успешно или с
    /// ошибкой). Базу не открывает.
    pub async fn rechunk_progress(&self, user: Id) -> Option<RechunkProgress> {
        let slot = self.open.lock().await.get(&user).cloned()?;
        let run = slot.lock().await.as_ref()?.rechunk.clone()?;
        if !run.running.load(Ordering::Relaxed) {
            return None;
        }
        Some(RechunkProgress {
            done: run.done.load(Ordering::Relaxed).min(run.notice.targets),
            notice: run.notice.clone(),
        })
    }
}

/// Снимает признак «перенарезка идёт» при выходе из фоновой задачи — в том числе при её
/// панике или отмене.
struct RunningGuard(Arc<RechunkRun>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Relaxed);
    }
}

/// Изменившиеся параметры для строки WARN: через запятую; не установлены — «параметры нарезки».
fn changed_text(changed: &[String]) -> String {
    if changed.is_empty() {
        "параметры нарезки".into()
    } else {
        changed.join(", ")
    }
}

/// Идемпотентно добавляет недостающие фиксированные пространства.
async fn ensure_fixed_spaces(db: &Db) -> Result<(), StoreError> {
    db.write(|tx| {
        let now = fmt_ts(Timestamp::now());
        for (kind, name) in FIXED {
            tx.execute(
                "INSERT INTO spaces (id, kind, name, created_at)
                 SELECT ?1, ?2, ?3, ?4
                 WHERE NOT EXISTS (SELECT 1 FROM spaces WHERE kind = ?2)",
                params![Id::new(), kind, name, now],
            )?;
        }
        Ok(())
    })
    .await
}

/// Ключ `meta`: разовая перенарезка заметок и записей со значениями секретов (сканер §7.6,
/// задача 11 M3) выполнена. Отсутствует у баз, открытых до сканера; у новой пустой базы
/// ставится сразу (все её записи нарезаются уже замаскированными).
const SECRETS_SCANNED: &str = "secrets_scanned_v2";

/// Нужна ли перенарезка секретов: ключа нет и в базе есть живые заметки или записи.
/// В пустой базе ключ ставится сразу.
async fn secrets_scan_pending(db: &Db) -> Result<bool, StoreError> {
    db.write(|tx| {
        if has_meta(tx, SECRETS_SCANNED)? {
            return Ok(false);
        }
        let any: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE kind = 'note' AND deleted_at IS NULL)
                 OR EXISTS (SELECT 1 FROM entries WHERE deleted_at IS NULL)",
            [],
            |r| r.get(0),
        )?;
        if !any {
            tx.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')",
                [SECRETS_SCANNED],
            )?;
        }
        Ok(any)
    })
    .await
}

/// Перенарезка живых заметок и записей, в которых сканер находит секреты: значения из версий
/// до сканера остаются в `chunks`/FTS и Qdrant, пока текст не перенарезан (нарушение инварианта
/// «значения не в FTS/Qdrant»). Флаг ставится после успеха; прерванная работа повторится.
async fn rescan_secrets(db: &Db) -> Result<(), StoreError> {
    let notes = crate::nodes::rescan_secrets(db, PRIORITY_REINDEX).await?;
    let entries = crate::entries::rescan_entry_secrets(db, PRIORITY_REINDEX).await?;
    // векторы старых (незамаскированных) чанков больше не нужны: чистим кэш эмбеддингов
    let mut pruned = 0u64;
    if notes + entries > 0 {
        loop {
            let n = crate::embeddings::gc_embeddings(db, 1000).await?;
            pruned += n;
            if n < 1000 {
                break;
            }
        }
    }
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')",
            [SECRETS_SCANNED],
        )?;
        Ok(())
    })
    .await?;
    tracing::info!(
        notes,
        entries,
        pruned,
        "перенарезка секретов базы завершена"
    );
    Ok(())
}

/// Ключ `meta`: разовая проверка payload и точек папок на значения секретов (ревью задачи 11,
/// задача 15 M3) выполнена. Теги точек Qdrant и вход эмбеддинга L0 папки маскируются с
/// задачи 15; у новой пустой базы ключ ставится сразу.
const SECRETS_PAYLOAD: &str = "secrets_scanned_v3";

/// Разовая проверка базы, открытой до маскировки тегов payload и входа эмбеддинга папок: если
/// у живой заметки или записи вне Human тег содержит секрет, или вход эмбеддинга L0 папки
/// (пространство, путь, имя, L0) содержит секрет, ставится отметка reindex базы из кэша
/// (`reindex_pending`, без инференса для известных текстов; уже стоящая отметка прерванного
/// reindex расширяется до всей базы с её режимом): воркер перепишет точки — теги уже замаскированы, у затронутых папок новый вход
/// эмбеддинга не найдётся в кэше и их свёртка пересчитается (`missed_folders`), а старые
/// точки области удаляются пересборкой. L0 папок с секретом маскируется сразу (он виден в
/// карточках папок). Синхронно, в одной транзакции писателя, до возврата базы: воркер
/// проверяет отметку при первом открытии базы после запуска. В лог — только счётчики.
async fn rescan_payload_secrets(db: &Db) -> Result<(), StoreError> {
    let found = db
        .write(|tx| {
            if has_meta(tx, SECRETS_PAYLOAD)? {
                return Ok(None);
            }
            let secret = |s: &str| !zam_markdown::scan_secrets(s).is_empty();
            let mut notes = 0u64;
            {
                let mut st = tx.prepare(
                    "SELECT n.frontmatter FROM nodes n JOIN spaces s ON s.id = n.space_id
                     WHERE n.kind = 'note' AND n.deleted_at IS NULL AND s.kind <> 'human'
                       AND n.frontmatter IS NOT NULL",
                )?;
                for fm in st.query_map([], |r| r.get::<_, String>(0))? {
                    let tags = crate::nodes::frontmatter_tags(&fm?).unwrap_or_default();
                    if tags.iter().any(|t| secret(t)) {
                        notes += 1;
                    }
                }
            }
            let mut entries = 0u64;
            {
                let mut st = tx.prepare(
                    "SELECT e.tags FROM entries e JOIN spaces s ON s.id = e.space_id
                     WHERE e.deleted_at IS NULL AND s.kind <> 'human'",
                )?;
                for tags in st.query_map([], |r| r.get::<_, String>(0))? {
                    let tags: Vec<String> = serde_json::from_str(&tags?).unwrap_or_default();
                    if tags.iter().any(|t| secret(t)) {
                        entries += 1;
                    }
                }
            }
            let folders: Vec<(Id, bool, String, String, String, String)> = {
                let mut st = tx.prepare(
                    "SELECT n.id, s.kind = 'human', s.name, n.path, n.name, n.l0
                     FROM nodes n JOIN spaces s ON s.id = n.space_id
                     WHERE n.kind = 'folder' AND n.deleted_at IS NULL AND n.l0 IS NOT NULL",
                )?;
                st.query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<Result<_, _>>()?
            };
            let mut stale_folders = 0u64;
            for (id, human, space, path, name, l0) in &folders {
                let masked = zam_markdown::mask_text(l0);
                if &masked != l0 {
                    tx.execute(
                        "UPDATE nodes SET l0 = ?2 WHERE id = ?1",
                        params![id, masked],
                    )?;
                }
                if !human && secret(&crate::jobs::folder_embed_raw(space, path, name, l0)) {
                    stale_folders += 1;
                }
            }
            if notes + entries + stale_folders > 0 {
                // уже стоящая отметка (прерванный reindex) расширяется до всей базы с её
                // режимом: частичная (одно пространство) оставила бы сырые теги в других
                let prev: Option<String> = tx
                    .query_row(
                        "SELECT value FROM meta WHERE key = ?1",
                        [crate::jobs::REINDEX_KEY],
                        |r| r.get(0),
                    )
                    .optional()?;
                let full = prev
                    .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
                    .and_then(|v| v["full"].as_bool())
                    .unwrap_or(false);
                tx.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                    params![
                        crate::jobs::REINDEX_KEY,
                        serde_json::json!({"full": full, "space": null}).to_string()
                    ],
                )?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, '1')",
                [SECRETS_PAYLOAD],
            )?;
            Ok(Some((notes, entries, stale_folders)))
        })
        .await?;
    if let Some((notes, entries, folders)) = found
        && notes + entries + folders > 0
    {
        tracing::info!(
            notes,
            entries,
            folders,
            "секреты в тегах payload и папках: отмечена пересборка индекса базы из кэша"
        );
    }
    Ok(())
}

/// Ключ `meta`: разовая перечанковка после миграции 0004 ещё не выполнена.
const RECHUNK_V4: &str = "rechunk_v4";

/// Ключ `meta`: часть чанков базы нарезана приближённым счётчиком токенов (свежая установка
/// без моделей, `ApproxCounter`). Ставится в транзакции нарезки, снимается после перенарезки
/// всех живых заметок точным счётчиком.
pub(crate) const CHUNKED_APPROX: &str = "chunked_approx";

/// Ключ `meta`: отпечаток нарезки, которой нарезаны живые заметки базы
/// (`crate::nodes::chunker_fingerprint`: версия алгоритма, пороги и флаги `[index]`, L0 в
/// префиксе, токенизатор). Ставится у пустой базы при открытии и после перенарезки всех заметок.
const CHUNKER_KEY: &str = "chunker";

/// Ключ `meta`: отпечаток нарезки живых записей журналов (`crate::nodes::entry_chunker_fingerprint`,
/// без L0 в префиксе). Отдельно от заметок: смена `chunk_prefix_l0` записи не перенарезает.
const ENTRY_CHUNKER_KEY: &str = "chunker_entries";

/// Ключ `meta` с JSON параметров нарезки рядом с отпечатком `key` (`chunker_params`,
/// `chunker_entries_params`; `crate::nodes::chunker_params`): по нему уведомление о
/// перенарезке называет изменившиеся ключи (§8.2). У базы M3 его нет.
fn params_key(key: &str) -> String {
    format!("{key}_params")
}

/// Почему база перечанковывается при открытии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rechunk {
    /// Ruling R1: база M1 обновлена до v4.
    AfterV4,
    /// Чанки по оценке, а счётчик теперь точный (модель установлена после записи заметок).
    ExactCounter,
    /// Другой алгоритм или настройки нарезки (задача 17 M3: без висячих заголовков и вводных
    /// фраз, H4–H6 в пути — меняются `embed_key` затронутых листов; или изменены `[index]
    /// chunk_*`). База без отпечатка нарезана до задачи 17.
    Chunker,
}

impl Rechunk {
    fn as_str(self) -> &'static str {
        match self {
            Rechunk::AfterV4 => "after_v4",
            Rechunk::ExactCounter => "exact_counter",
            Rechunk::Chunker => "chunker",
        }
    }
}

fn has_meta(c: &Connection, key: &str) -> rusqlite::Result<bool> {
    Ok(
        c.query_row("SELECT 1 FROM meta WHERE key = ?1", [key], |_| Ok(()))
            .optional()?
            .is_some(),
    )
}

/// Что перенарезать при открытии базы, почему и уведомление о перенарезке.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RechunkPlan {
    why: Rechunk,
    notes: bool,
    entries: bool,
    notice: RechunkNotice,
}

/// Нарезка процесса для одной части базы: отпечаток и JSON параметров.
#[derive(Debug, Clone)]
struct Chunker {
    fingerprint: String,
    params: serde_json::Value,
}

/// Нарезка процесса: `(заметки, записи журналов)`.
fn fingerprints() -> (Chunker, Chunker) {
    (
        Chunker {
            fingerprint: crate::nodes::chunker_fingerprint(),
            params: crate::nodes::chunker_params(true),
        },
        Chunker {
            fingerprint: crate::nodes::entry_chunker_fingerprint(),
            params: crate::nodes::chunker_params(false),
        },
    )
}

/// Причина и изменившиеся параметры части базы, отпечаток которой разошёлся с текущим: по
/// сохранённому JSON параметров, а у базы M3 без него — `unknown` с параметрами,
/// восстановленными из отпечатка, если он разбирается (иначе список пуст)
/// **[решение исполнителя, M4]**: при обновлении с M3 администратор видит, что изменилась
/// `chunk_boundary_lambda`, хотя причина формально не установлена.
fn diagnose(
    stored_fp: Option<&str>,
    stored_params: Option<&str>,
    current: &Chunker,
    exact: bool,
) -> (RechunkReason, Vec<String>) {
    use crate::nodes::{PARAM_ALGORITHM, PARAM_TOKENIZER, changed_params};
    let stored = stored_params.and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
    let Some(stored) = stored else {
        let changed = stored_fp
            .and_then(crate::nodes::params_from_fingerprint)
            .map(|old| changed_params(&old, &current.params, exact))
            .unwrap_or_default();
        return (RechunkReason::Unknown, changed);
    };
    let changed = changed_params(&stored, &current.params, exact);
    let reason = if changed.iter().any(|k| k == PARAM_ALGORITHM) {
        RechunkReason::Algorithm
    } else if changed.iter().any(|k| k != PARAM_TOKENIZER) {
        RechunkReason::Params
    } else if changed.is_empty() {
        // отпечаток разошёлся, а параметры совпали (JSON записан другой версией сервера)
        RechunkReason::Unknown
    } else {
        RechunkReason::Tokenizer
    };
    (reason, changed)
}

/// Общая причина двух частей базы — первая по важности: алгоритм, параметры, токенизатор,
/// неизвестно.
fn first_reason(a: RechunkReason, b: RechunkReason) -> RechunkReason {
    let rank = |r| match r {
        RechunkReason::Algorithm => 0,
        RechunkReason::Params => 1,
        RechunkReason::Tokenizer => 2,
        RechunkReason::Unknown => 3,
    };
    if rank(b) < rank(a) { b } else { a }
}

/// Нужна ли перечанковка при открытии: флаг R1, чанки по оценке при точном счётчике (всё)
/// или отпечаток нарезки заметок либо записей не совпадает с текущим (только расходящееся;
/// сравнение — `crate::nodes::same_chunker`). В плане — уведомление: причина, изменившиеся
/// параметры и число целей (живых заметок и записей перенарезаемых частей).
async fn rechunk_pending(db: &Db) -> Result<Option<RechunkPlan>, StoreError> {
    let exact = crate::nodes::token_counter_exact();
    let (notes_cur, entries_cur) = fingerprints();
    db.read(move |c| {
        let stored = |key: &str| -> rusqlite::Result<Option<String>> {
            c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        };
        let count = |sql: &str| -> rusqlite::Result<u64> {
            Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?.max(0) as u64)
        };
        let targets = |notes: bool, entries: bool| -> rusqlite::Result<u64> {
            let n = if notes {
                count("SELECT count(*) FROM nodes WHERE kind = 'note' AND deleted_at IS NULL")?
            } else {
                0
            };
            let e = if entries {
                count("SELECT count(*) FROM entries WHERE deleted_at IS NULL")?
            } else {
                0
            };
            Ok(n + e)
        };
        let all = |why, reason, changed: &str| -> rusqlite::Result<Option<RechunkPlan>> {
            Ok(Some(RechunkPlan {
                why,
                notes: true,
                entries: true,
                notice: RechunkNotice {
                    reason,
                    changed: vec![changed.to_string()],
                    targets: targets(true, true)?,
                },
            }))
        };
        if has_meta(c, RECHUNK_V4)? {
            return all(
                Rechunk::AfterV4,
                RechunkReason::Algorithm,
                crate::nodes::PARAM_ALGORITHM,
            )
            .map_err(Into::into);
        }
        if exact && has_meta(c, CHUNKED_APPROX)? {
            return all(
                Rechunk::ExactCounter,
                RechunkReason::Tokenizer,
                crate::nodes::PARAM_TOKENIZER,
            )
            .map_err(Into::into);
        }
        // расхождение части базы с текущей нарезкой; `None` — отпечаток совпал
        let part =
            |key: &str, cur: &Chunker| -> rusqlite::Result<Option<(RechunkReason, Vec<String>)>> {
                let fp = stored(key)?;
                if crate::nodes::same_chunker(fp.as_deref(), &cur.fingerprint, exact) {
                    return Ok(None);
                }
                let params = stored(&params_key(key))?;
                Ok(Some(diagnose(fp.as_deref(), params.as_deref(), cur, exact)))
            };
        let notes = part(CHUNKER_KEY, &notes_cur)?;
        let entries = part(ENTRY_CHUNKER_KEY, &entries_cur)?;
        let (reason, changed) = match (&notes, &entries) {
            (None, None) => return Ok(None),
            (Some(d), None) | (None, Some(d)) => d.clone(),
            (Some((rn, cn)), Some((re, ce))) => {
                let mut changed = cn.clone();
                changed.extend(ce.iter().filter(|k| !cn.contains(k)).cloned());
                changed.sort_by_key(|k| crate::nodes::param_order(k));
                (first_reason(*rn, *re), changed)
            }
        };
        Ok(Some(RechunkPlan {
            why: Rechunk::Chunker,
            notes: notes.is_some(),
            entries: entries.is_some(),
            notice: RechunkNotice {
                reason,
                changed,
                targets: targets(notes.is_some(), entries.is_some())?,
            },
        }))
    })
    .await
}

/// Отпечатки нарезки базы — текущие: вызывается после перечанковки всех живых заметок и
/// записей (`reindex --full` без области).
pub(crate) async fn mark_chunker_current(db: &Db) -> Result<(), StoreError> {
    let (notes_cur, entries_cur) = fingerprints();
    db.write(move |tx| {
        put_chunker(tx, CHUNKER_KEY, &notes_cur)?;
        put_chunker(tx, ENTRY_CHUNKER_KEY, &entries_cur)?;
        Ok(())
    })
    .await
}

/// Записывает отпечаток нарезки `key` (заметок или записей) базы и рядом — JSON её
/// параметров (`<key>_params`).
fn put_chunker(tx: &Connection, key: &str, cur: &Chunker) -> rusqlite::Result<()> {
    let mut st = tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")?;
    st.execute(params![key, cur.fingerprint])?;
    st.execute(params![params_key(key), cur.params.to_string()])?;
    Ok(())
}

/// База без живых заметок и записей (новая или пустая) — флаги перечанковки снимаются сразу,
/// отпечаток нарезки ставится текущий, без фоновой задачи. `true` — сняты.
async fn clear_rechunk_if_empty(db: &Db) -> Result<bool, StoreError> {
    let (notes_cur, entries_cur) = fingerprints();
    db.write(move |tx| {
        let any: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE kind = 'note' AND deleted_at IS NULL)
                 OR EXISTS (SELECT 1 FROM entries WHERE deleted_at IS NULL)",
            [],
            |r| r.get(0),
        )?;
        if !any {
            tx.execute(
                "DELETE FROM meta WHERE key IN (?1, ?2)",
                [RECHUNK_V4, CHUNKED_APPROX],
            )?;
            put_chunker(tx, CHUNKER_KEY, &notes_cur)?;
            put_chunker(tx, ENTRY_CHUNKER_KEY, &entries_cur)?;
        }
        Ok(!any)
    })
    .await
}

/// Снимает отметку «чанки по оценке», если счётчик точный: вызывается после перечанковки
/// всех живых заметок базы. Заметка, записанная во время перечанковки, нарезана тем же
/// точным счётчиком; счётчик, откатившийся на оценку посреди работы, отметку не снимает.
pub(crate) async fn clear_approx_if_exact(db: &Db) -> Result<(), StoreError> {
    if !crate::nodes::token_counter_exact() {
        return Ok(());
    }
    db.write(|tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [CHUNKED_APPROX])?;
        Ok(())
    })
    .await
}

/// Перечанковка всех живых заметок и записей журналов текущими счётчиком и настройками
/// нарезки с заданиями `upsert`.
///
/// Ruling R1: чанки M1 построены по секциям (`section`) и без векторов, поэтому при первом
/// открытии базы v4 все живые заметки перечанковываются. То же — при первом открытии с точным
/// счётчиком базы, чанки которой нарезаны оценкой: иначе листы могут не уложиться в окно
/// модели (усечение при инференсе) и разойтись с порогами §8.2. И при смене алгоритма или
/// настроек нарезки (отпечатки `meta.chunker` и `meta.chunker_entries`, задача 17 и финальная
/// волна M3) — тогда только той части (заметки, записи), чей отпечаток разошёлся. Кэш
/// эмбеддингов ключуется по `embed_key`: повторный инференс — только у листов, текст или
/// префикс которых изменился; цель, нарезка которой не изменилась, не переписывается и не
/// получает `upsert` (её точки в Qdrant уже совпадают с чанками).
/// Флаги снимаются, а отпечаток ставится только после успеха: прерванная перечанковка
/// повторится при следующем открытии (она идемпотентна). `done` — счётчик обработанных целей
/// (ход перенарезки в `GET /index/status`). В лог идут только счётчики.
async fn rechunk_all(db: &Db, plan: &RechunkPlan, done: &AtomicU64) -> Result<(), StoreError> {
    let why = plan.why;
    let (part_notes, part_entries) = (plan.notes, plan.entries);
    tracing::info!(reason = why.as_str(), "перечанковка заметок базы");
    let started = std::time::Instant::now();
    let (notes_cur, entries_cur) = fingerprints();
    let (notes, upserts) = if part_notes {
        crate::nodes::rechunk_notes(db, None, PRIORITY_REINDEX, true, Some(done)).await?
    } else {
        (0, 0)
    };
    let entries = if part_entries {
        crate::entries::rechunk_entries(db, None, PRIORITY_REINDEX, true, Some(done)).await?
    } else {
        0
    };
    db.write(move |tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [RECHUNK_V4])?;
        if part_notes {
            put_chunker(tx, CHUNKER_KEY, &notes_cur)?;
        }
        if part_entries {
            put_chunker(tx, ENTRY_CHUNKER_KEY, &entries_cur)?;
        }
        Ok(())
    })
    .await?;
    clear_approx_if_exact(db).await?;
    tracing::info!(
        reason = why.as_str(),
        notes,
        upserts,
        entries,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "перечанковка заметок базы завершена"
    );
    Ok(())
}

/// Пространства: rules, human, knowledge, затем решения по имени.
pub async fn list_spaces(db: &Db) -> Result<Vec<Space>, StoreError> {
    db.read(|c| {
        let mut st = c.prepare(&format!(
            "SELECT {SPACE_COLS} FROM spaces
             ORDER BY CASE kind WHEN 'rules' THEN 0 WHEN 'human' THEN 1
                                WHEN 'knowledge' THEN 2 ELSE 3 END, name, id"
        ))?;
        let rows = st.query_map([], space_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
}

const SPACE_COLS: &str = "id, kind, solution_guid, name";

fn space_row(r: &Row) -> rusqlite::Result<Space> {
    Ok(Space {
        id: r.get(0)?,
        kind: r.get(1)?,
        solution_guid: r.get(2)?,
        name: r.get(3)?,
    })
}

fn solution_by_guid(c: &Connection, guid: &str) -> Result<Option<Space>, StoreError> {
    Ok(c.query_row(
        &format!("SELECT {SPACE_COLS} FROM spaces WHERE solution_guid = ?1"),
        [guid],
        space_row,
    )
    .optional()?)
}

/// Пространство по id; нет — `None`.
pub async fn get_space(db: &Db, id: Id) -> Result<Option<Space>, StoreError> {
    db.read(move |c| {
        Ok(c.query_row(
            &format!("SELECT {SPACE_COLS} FROM spaces WHERE id = ?1"),
            [id],
            space_row,
        )
        .optional()?)
    })
    .await
}

/// Фиксированное пространство вида `kind` (`Rules`, `Human`, `Knowledge`).
/// Для `Solution` и при отсутствии — `NotFound`.
pub async fn space_of_kind(db: &Db, kind: SpaceKind) -> Result<Space, StoreError> {
    if kind == SpaceKind::Solution {
        return Err(StoreError::NotFound);
    }
    db.read(move |c| {
        c.query_row(
            &format!("SELECT {SPACE_COLS} FROM spaces WHERE kind = ?1"),
            [kind],
            space_row,
        )
        .optional()?
        .ok_or(StoreError::NotFound)
    })
    .await
}

/// Пространство решения по GUID (как сохранён: нижний регистр с дефисами).
pub async fn get_solution(db: &Db, guid: &str) -> Result<Option<Space>, StoreError> {
    let guid = guid.to_string();
    db.read(move |c| solution_by_guid(c, &guid)).await
}

/// Идемпотентно регистрирует решение: создаёт пространство `solution` с `guid`
/// или возвращает существующее (имя существующего не меняется).
/// Второе значение — `true`, если пространство создано сейчас.
/// GUID и имя проверяет вызывающий; GUID хранится как передан.
pub async fn create_solution(db: &Db, guid: &str, name: &str) -> Result<(Space, bool), StoreError> {
    let (guid, name) = (guid.to_string(), name.to_string());
    db.write(move |tx| {
        if let Some(s) = solution_by_guid(tx, &guid)? {
            return Ok((s, false));
        }
        let id = Id::new();
        tx.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', ?2, ?3, ?4)",
            params![id, guid, name, fmt_ts(Timestamp::now())],
        )?;
        let s = solution_by_guid(tx, &guid)?.ok_or(StoreError::NotFound)?;
        Ok((s, true))
    })
    .await
}

/// Создаёт пространство `solution` с заданным id (`space_id` из реестра `system.db`).
/// Идемпотентно: то же пространство с этим GUID уже есть — возвращается оно (`false`, имя
/// не меняется). GUID у другого пространства или id занят — `Conflict`.
pub async fn create_solution_with_id(
    db: &Db,
    space_id: Id,
    guid: &SolutionGuid,
    name: &str,
) -> Result<(Space, bool), StoreError> {
    let (guid, name) = (guid.clone(), name.to_string());
    db.write(move |tx| {
        if let Some(s) = solution_by_guid(tx, guid.as_str())? {
            if s.id == space_id {
                return Ok((s, false));
            }
            return Err(StoreError::Conflict(format!(
                "решение {guid} уже у другого пространства"
            )));
        }
        let taken = tx
            .query_row("SELECT 1 FROM spaces WHERE id = ?1", [space_id], |_| Ok(()))
            .optional()?
            .is_some();
        if taken {
            return Err(StoreError::Conflict(format!(
                "пространство {space_id} уже существует"
            )));
        }
        tx.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', ?2, ?3, ?4)",
            params![space_id, guid, name, fmt_ts(Timestamp::now())],
        )?;
        let s = solution_by_guid(tx, guid.as_str())?.ok_or(StoreError::NotFound)?;
        Ok((s, true))
    })
    .await
}

/// Сброс `index_state` живых индексируемых заметок пространства `?1` в `pending` (смена GUID
/// решения): узлы — по живому индексу `nodes_live_children (space_id=?)`, корзина не
/// просматривается (план закреплён тестом `jobs::tests::indexable_count_and_folders_skip_trash`).
pub(crate) const SPACE_NOTES_PENDING_SQL: &str = "UPDATE nodes SET index_state = 'pending'
     WHERE space_id = ?1 AND kind = 'note' AND deleted_at IS NULL
       AND index_state <> 'excluded'
     RETURNING id, current_rev";

/// Меняет имя пространства решения `space_id`. Нет такого пространства решения — `NotFound`.
pub async fn rename_solution_space(db: &Db, space_id: Id, name: &str) -> Result<(), StoreError> {
    let name = name.to_string();
    db.write(move |tx| {
        let n = tx.execute(
            "UPDATE spaces SET name = ?2 WHERE id = ?1 AND kind = 'solution'",
            params![space_id, name],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    })
    .await
}

/// Меняет GUID пространства решения `space_id` (конфликт GUID при регистрации решений
/// M1–M2). Узлы, ревизии и чанки остаются на месте; живые индексируемые заметки получают
/// `upsert`, чтобы точки Qdrant получили новый `solution_guid` в payload. Нет такого
/// пространства решения — `NotFound`, GUID занят — `Conflict`.
pub async fn rename_solution_guid(
    db: &Db,
    space_id: Id,
    new: &SolutionGuid,
) -> Result<(), StoreError> {
    let new = new.clone();
    db.write(move |tx| {
        let cur: Option<Option<String>> = tx
            .query_row(
                "SELECT solution_guid FROM spaces WHERE id = ?1 AND kind = 'solution'",
                [space_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(cur) = cur else {
            return Err(StoreError::NotFound);
        };
        if cur.as_deref() == Some(new.as_str()) {
            return Ok(());
        }
        tx.execute(
            "UPDATE spaces SET solution_guid = ?2 WHERE id = ?1",
            params![space_id, new],
        )
        .map_err(|e| {
            if crate::system::is_unique_violation(&e) {
                StoreError::Conflict(format!("решение {new} уже есть в базе"))
            } else {
                e.into()
            }
        })?;
        let now = fmt_ts(Timestamp::now());
        let notes: Vec<(Id, i64)> = {
            let mut st = tx.prepare(SPACE_NOTES_PENDING_SQL)?;
            st.query_map([space_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        for (node, rev) in notes {
            crate::jobs::enqueue(tx, node, rev, JobOp::Upsert, PRIORITY_REINDEX, &now)?;
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Паника фоновой перенарезки снимает «идёт» (иначе баннер WebUI висел бы до перезапуска).
    #[tokio::test]
    async fn running_flag_cleared_on_panic() {
        let run = Arc::new(RechunkRun {
            notice: RechunkNotice {
                reason: RechunkReason::Params,
                changed: vec!["chunk_boundary_lambda".into()],
                targets: 3,
            },
            done: AtomicU64::new(1),
            running: AtomicBool::new(true),
        });
        let bg = run.clone();
        let r = tokio::spawn(async move {
            let _running = RunningGuard(bg);
            panic!("сбой перенарезки");
        })
        .await;
        assert!(r.is_err_and(|e| e.is_panic()));
        assert!(!run.running.load(Ordering::Relaxed));
    }
}
