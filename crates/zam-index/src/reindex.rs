//! Пересборка векторного индекса области (§8.1, §18 ТЗ: «Qdrant пересобирается из
//! `embeddings` без инференса»).
//!
//! Один запрос — [`ReindexRequest`]: пользователь (`None` — все), пространство (`None` —
//! все), режим:
//! - **из кэша** (по умолчанию): точки области удаляются (`delete_scope`), затем чанки
//!   живых индексируемых заметок читаются страницами, их эмбеддинги берутся из
//!   `embeddings` по `chunks.embed_key` и пишутся в Qdrant пачками по 256 точек
//!   (`wait=true`, до [`PARALLEL_WRITES`] = 8 запросов одновременно; следующая страница читается, пока пишется
//!   текущая). Заметка, у которой в кэше нет хотя бы одного чанка, получает
//!   задание `upsert` (инференс воркером). Точки папок — из кэша по L0 папки.
//! - **полный** (`full`): `enqueue_reindex` — перечанковка и задания для всех заметок
//!   области, инференс для промахов кэша.
//!
//! Коллекция чужой модели (Review Focus 3) удаляется и создаётся заново для текущей;
//! после пересоздания (или если коллекцию удалили извне) область расширяется до всех
//! пользователей и пространств: в новой коллекции нет ничьих точек.
//!
//! До удаления точек в базе пользователя ставится отметка `reindex_pending`
//! ([`mark_reindex`]), после восстановления — снимается: прерванный reindex повторяется
//! воркером при следующем запуске. После пересборки всей базы пользователя — сборка мусора
//! `embeddings` ([`gc_embeddings`]) пачками. Базы, которые не открылись, отмечаются в
//! `system.db` исключёнными: воркер поставит им reindex, когда они снова откроются.
//!
//! Выполняется воркером (последовательно с заданиями индексации: точки узла не
//! переписываются параллельно) или командой `zam-server reindex` на остановленном сервере.
//! В лог — только счётчики.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use futures_util::stream::{self, StreamExt, TryStreamExt};

use zam_core::Id;
use zam_store::{
    Db, Embedding, IndexFolder, ReindexNote, StoreError, SystemStore, UserDbs,
    adopt_owned_embeddings_batch, enqueue_reindex, enqueue_upserts, finish_reindexed,
    folders_for_index, gc_embeddings, get_embeddings, mark_reindex, put_owned_embeddings,
    reindex_page,
};

use crate::worker::{chunk_point, folder_embed, folder_point};
use crate::{IndexError, IndexPoint, PassageEmbedder, VectorFilter, VectorIndex};

/// Заметок на страницу чтения из SQLite.
const PAGE_NOTES: usize = 256;

/// Страница: заметки и эмбеддинги их чанков по ключу кэша.
type Page = (Vec<ReindexNote>, HashMap<String, Embedding>);
/// Точек в одной записи в Qdrant (`wait=true`).
pub const POINTS_BATCH: usize = 256;
/// Записей в Qdrant одновременно.
const PARALLEL_WRITES: usize = 8;
/// Строк кэша в одной пачке сборки мусора.
pub const GC_BATCH: usize = 2000;

/// Что пересобрать.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReindexRequest {
    /// Пользователь; `None` — все.
    pub user: Option<Id>,
    /// Пространство пользователя (только вместе с `user`); `None` — все.
    pub space: Option<Id>,
    /// `true` — перечанковка и задания (инференс промахов); `false` — из кэша.
    pub full: bool,
}

impl ReindexRequest {
    /// Отметка `reindex_pending` базы: режим и пространство.
    pub(crate) fn spec(&self, space: Option<Id>) -> String {
        serde_json::json!({ "full": self.full, "space": space.map(|s| s.to_string()) }).to_string()
    }

    /// Запрос из отметки `reindex_pending` базы `user` (повтор прерванного).
    pub fn from_spec(user: Id, spec: &str) -> ReindexRequest {
        let v: serde_json::Value = serde_json::from_str(spec).unwrap_or_default();
        ReindexRequest {
            user: Some(user),
            space: v["space"].as_str().and_then(|s| s.parse().ok()),
            full: v["full"].as_bool().unwrap_or(false),
        }
    }
}

/// Итог пересборки (счётчики, без текстов).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReindexReport {
    pub full: bool,
    /// Обработано баз пользователей.
    pub users: usize,
    /// Заметок восстановлено из кэша.
    pub nodes: u64,
    /// Точек чанков записано.
    pub points: u64,
    /// Точек папок записано.
    pub folders: u64,
    /// Заметок без эмбеддингов в кэше (для них поставлены задания).
    pub misses: u64,
    /// Папок с L0 без эмбеддинга в кэше и без модели в контексте (точку построит свёртка).
    pub folder_misses: u64,
    /// Эти папки `(пользователь, папка)`: воркер ставит им свёртку L0.
    pub missed_folders: Vec<(Id, Id)>,
    /// Поставлено заданий индексации.
    pub jobs: u64,
    /// Удалено строк кэша сборкой мусора.
    pub gc_deleted: u64,
    /// Коллекция создана заново (была удалена или построена другой моделью).
    pub recreated: bool,
    /// Базы, которые не удалось обработать: `(пользователь, ошибка)`.
    pub failed_users: Vec<(Id, String)>,
    /// Сколько из них не получили отметку исключения в `system.db` (`index_skip` не записан):
    /// такая база не восстановится сама, поэтому флаг стёртого хранилища снимать нельзя.
    pub skip_unrecorded: usize,
    pub elapsed_ms: u64,
    /// Чтение страниц и кэша из SQLite, мс (идёт параллельно с записью).
    pub read_ms: u64,
    /// Удаление и запись точек в Qdrant, мс.
    pub write_ms: u64,
}

/// Зависимости пересборки.
pub struct ReindexCtx<'a> {
    pub system: &'a SystemStore,
    pub users: &'a UserDbs,
    pub vector: &'a dyn VectorIndex,
    /// `model_id` текущей модели (ключ кэша `embeddings` и метаданные коллекции).
    pub model_id: &'a str,
    pub dim: usize,
    /// Модель для L0 папок, которых нет в кэше (CLI); `None` — их свёртку ставит воркер.
    /// Чанки заметок моделью не считаются никогда (промахи — задания).
    pub embedder: Option<Arc<dyn PassageEmbedder>>,
}

/// Ошибка базы пользователя — в отчёт, ошибка Qdrant — наверх.
enum UserFail {
    Store(StoreError),
    Vector(IndexError),
}

impl From<StoreError> for UserFail {
    fn from(e: StoreError) -> Self {
        UserFail::Store(e)
    }
}

impl From<IndexError> for UserFail {
    fn from(e: IndexError) -> Self {
        UserFail::Vector(e)
    }
}

/// Пересобирает индекс по запросу. Ошибка Qdrant прерывает пересборку (вызывающий
/// повторяет её; отметки `reindex_pending` необработанных баз остаются), ошибка базы
/// пользователя попадает в [`ReindexReport::failed_users`], остальные базы обрабатываются.
pub async fn reindex(
    ctx: &ReindexCtx<'_>,
    req: &ReindexRequest,
) -> Result<ReindexReport, IndexError> {
    let started = Instant::now();
    let mut rep = ReindexReport {
        full: req.full,
        ..ReindexReport::default()
    };
    let meta = ctx.vector.ensure(ctx.model_id, ctx.dim).await?;
    let ours = meta.model_id.as_deref() == Some(ctx.model_id) && meta.dim == ctx.dim;
    rep.recreated = meta.created || !ours;
    // область: после пересоздания коллекции — все базы и пространства
    let users: Vec<Id> = match req.user {
        Some(u) if !rep.recreated => vec![u],
        _ => ctx
            .system
            .list_users()
            .await
            .map_err(|e| IndexError::Other(format!("список пользователей: {e}")))?
            .into_iter()
            .map(|u| u.id)
            .collect(),
    };
    let space = if rep.recreated { None } else { req.space };
    // отметки до любых разрушающих действий: прерванная пересборка повторится
    let mut dbs: Vec<(Id, Db)> = Vec::with_capacity(users.len());
    for u in users {
        let opened = match ctx.users.get(u).await {
            Ok(db) => mark_reindex(&db, Some(req.spec(space))).await.map(|()| db),
            Err(e) => Err(e),
        };
        match opened {
            Ok(db) => dbs.push((u, db)),
            Err(e) => {
                tracing::warn!(user = %u, error = %e, "reindex: база пользователя пропущена");
                rep.failed_users.push((u, e.to_string()));
            }
        }
    }
    if !ours {
        tracing::warn!(
            collection_model = meta.model_id.as_deref().unwrap_or("(без model_id)"),
            model = ctx.model_id,
            "reindex: коллекция построена другой моделью, создаётся заново"
        );
        ctx.vector.drop_all().await?;
        ctx.vector.ensure(ctx.model_id, ctx.dim).await?;
    }
    for (u, db) in dbs {
        let res = if req.full {
            full_user(&db, space, &mut rep).await
        } else {
            cache_user(ctx, u, &db, space, rep.recreated, &mut rep).await
        };
        let res = match res {
            // сборка мусора — после пересборки всей базы: ключи старых ревизий не нужны
            Ok(()) if space.is_none() => gc_all(&db, &mut rep).await.map_err(UserFail::from),
            other => other,
        };
        let res = match res {
            Ok(()) => mark_reindex(&db, None).await.map_err(UserFail::from),
            other => other,
        };
        match res {
            Ok(()) => rep.users += 1,
            Err(UserFail::Store(e)) => {
                tracing::warn!(user = %u, error = %e, "reindex: база пользователя не пересобрана");
                rep.failed_users.push((u, e.to_string()));
            }
            Err(UserFail::Vector(e)) => return Err(e),
        }
    }
    // базы, которые не открылись или не пересобрались: отметка в system.db переживает
    // перезапуск, воркер поставит им reindex, когда база снова откроется (точек в новой
    // коллекции у них нет)
    for (u, e) in &rep.failed_users {
        if let Err(se) = ctx.system.index_skip(*u, e, 1).await {
            tracing::warn!(user = %u, error = %se, "отметка исключённой базы не записана");
            rep.skip_unrecorded += 1;
        }
    }
    rep.elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        full = rep.full,
        users = rep.users,
        nodes = rep.nodes,
        points = rep.points,
        folders = rep.folders,
        misses = rep.misses,
        jobs = rep.jobs,
        gc_deleted = rep.gc_deleted,
        recreated = rep.recreated,
        failed = rep.failed_users.len(),
        elapsed_ms = rep.elapsed_ms,
        read_ms = rep.read_ms,
        write_ms = rep.write_ms,
        "reindex завершён"
    );
    Ok(rep)
}

/// Полный режим: перечанковка и задания (точки заменит воркер).
async fn full_user(db: &Db, space: Option<Id>, rep: &mut ReindexReport) -> Result<(), UserFail> {
    rep.jobs += enqueue_reindex(db, space).await?;
    Ok(())
}

/// Из кэша: удалить точки области (если коллекция не новая) и записать их заново.
async fn cache_user(
    ctx: &ReindexCtx<'_>,
    user: Id,
    db: &Db,
    space: Option<Id>,
    fresh: bool,
    rep: &mut ReindexReport,
) -> Result<(), UserFail> {
    if !fresh {
        let t = Instant::now();
        ctx.vector
            .delete_scope(&VectorFilter {
                user_id: user,
                space_ids: space.into_iter().collect(),
                path_prefix: None,
                levels: Vec::new(),
            })
            .await?;
        rep.write_ms += t.elapsed().as_millis() as u64;
    }
    let mut misses: Vec<Id> = Vec::new();
    // конвейер: пока пишется страница N, читается страница N + 1
    let (mut page, read_ms) = load_page(ctx, db, space, None).await?;
    rep.read_ms += read_ms;
    while let Some((notes, embs)) = page {
        let after = notes.last().map(|n| n.node_id);
        let mut pts: Vec<IndexPoint> = Vec::new();
        let mut done: Vec<(Id, i64)> = Vec::new();
        for n in &notes {
            if n.chunks.iter().all(|c| embs.contains_key(&c.embed_key)) {
                pts.extend(
                    n.chunks
                        .iter()
                        .map(|c| chunk_point(user, c, &embs[&c.embed_key])),
                );
                done.push((n.node_id, n.rev));
            } else {
                misses.push(n.node_id);
            }
        }
        drop(embs);
        let n = pts.len() as u64;
        let t = Instant::now();
        let (written, next) = tokio::join!(
            write_points(ctx.vector, pts),
            load_page(ctx, db, space, after)
        );
        written?;
        rep.write_ms += t.elapsed().as_millis() as u64;
        rep.points += n;
        // заметки — только после подтверждённой записи их точек
        rep.nodes += done.len() as u64;
        finish_reindexed(db, done).await?;
        let (p, read_ms) = next?;
        rep.read_ms += read_ms;
        page = p;
    }
    rep.misses += misses.len() as u64;
    rep.jobs += enqueue_upserts(db, misses).await?;
    // точки папок по их L0 (кэш служебных эмбеддингов)
    let folders = folders_for_index(db, space).await?;
    let embedded: Vec<(String, String)> = folders
        .iter()
        .map(|f| folder_embed(&f.space_name, &f.path, &f.name, &f.l0))
        .collect();
    let keys: Vec<String> = embedded
        .iter()
        .map(|(_, k)| k.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let embs = get_embeddings(db, ctx.model_id, &keys).await?;
    let mut fpts = Vec::new();
    let mut missed: Vec<(&IndexFolder, &(String, String))> = Vec::new();
    let mut adopt: Vec<(Id, String)> = Vec::new();
    for (f, e) in folders.iter().zip(&embedded) {
        match embs.get(&e.1) {
            Some(v) => {
                adopt.push((f.node_id, e.1.clone()));
                fpts.push(folder_point(user, f, v.clone(), e.1.clone()));
            }
            None => missed.push((f, e)),
        }
    }
    // строки до v5 (без владельца) — теперь папок, иначе их снимет сборка мусора; одна
    // транзакция на базу, а не на папку
    adopt_owned_embeddings_batch(db, ctx.model_id, adopt).await?;
    // L0 папки нет в кэше: с моделью (CLI) — эмбеддинг сразу, иначе свёртку пересчитает
    // воркер ([`ReindexReport::missed_folders`])
    if let (Some(model), false) = (&ctx.embedder, missed.is_empty()) {
        let inputs: Vec<String> = missed.iter().map(|(_, e)| e.0.clone()).collect();
        let model = model.clone();
        let out = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
            model.embed_passages(&refs)
        })
        .await
        .map_err(|e| IndexError::Other(format!("поток инференса: {e}")))?
        .map_err(IndexError::Other)?;
        for ((f, e), v) in missed.drain(..).zip(out) {
            put_owned_embeddings(db, ctx.model_id, f.node_id, vec![(e.1.clone(), v.clone())])
                .await?;
            fpts.push(folder_point(user, f, v, e.1.clone()));
        }
    }
    rep.folder_misses += missed.len() as u64;
    rep.missed_folders
        .extend(missed.iter().map(|(f, _)| (user, f.node_id)));
    rep.folders += fpts.len() as u64;
    write_points(ctx.vector, fpts).await?;
    Ok(())
}

/// Страница заметок после `after` и эмбеддинги их чанков из кэша; `None` — заметок больше
/// нет. Второе значение — время чтения, мс.
async fn load_page(
    ctx: &ReindexCtx<'_>,
    db: &Db,
    space: Option<Id>,
    after: Option<Id>,
) -> Result<(Option<Page>, u64), StoreError> {
    let t = Instant::now();
    let notes = reindex_page(db, space, after, PAGE_NOTES).await?;
    if notes.is_empty() {
        return Ok((None, t.elapsed().as_millis() as u64));
    }
    let mut keys: Vec<String> = notes
        .iter()
        .flat_map(|n| n.chunks.iter().map(|c| c.embed_key.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    let embs = get_embeddings(db, ctx.model_id, &keys).await?;
    Ok((Some((notes, embs)), t.elapsed().as_millis() as u64))
}

/// Записывает точки пачками по [`POINTS_BATCH`], до [`PARALLEL_WRITES`] запросов
/// одновременно (каждый — `wait=true`): на HDD задержка подтверждения, а не объём,
/// ограничивает скорость.
async fn write_points(vector: &dyn VectorIndex, pts: Vec<IndexPoint>) -> Result<(), IndexError> {
    if pts.is_empty() {
        return Ok(());
    }
    let mut batches: Vec<Vec<IndexPoint>> = Vec::new();
    let mut it = pts.into_iter().peekable();
    while it.peek().is_some() {
        batches.push(it.by_ref().take(POINTS_BATCH).collect());
    }
    stream::iter(batches.into_iter().map(|b| vector.upsert(b)))
        .buffer_unordered(PARALLEL_WRITES)
        .try_collect::<Vec<()>>()
        .await?;
    Ok(())
}

/// Сборка мусора кэша базы до конца, пачками по [`GC_BATCH`].
async fn gc_all(db: &Db, rep: &mut ReindexReport) -> Result<(), StoreError> {
    loop {
        let n = gc_embeddings(db, GC_BATCH).await?;
        rep.gc_deleted += n;
        if n < GC_BATCH as u64 {
            return Ok(());
        }
    }
}
