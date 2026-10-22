//! Хранилище ZeroAIMemory: открытие SQLite, поток-писатель, пул читателей, миграции,
//! узлы и ревизии заметок с синхронной индексацией (чанки §8.2, FTS5), outbox векторной
//! индексации (`index_jobs`), кэш эмбеддингов, полнотекстовый поиск, блобы вложений,
//! записи журналов решения (`entries`, M3), реестр решений и доступ участников (`system.db`).

pub mod access;
pub mod blobs;
pub mod datadir;
pub mod db;
pub mod embeddings;
pub mod entries;
pub mod jobs;
pub mod migrate;
pub mod nodes;
pub mod paths;
pub mod search;
pub mod solution_registry;
pub mod system;
pub mod time;
pub mod user_db;

pub use access::{AccessDecision, AccessEntry, AccessState, SolutionInfo, SolutionRole};
pub use blobs::{
    AttachScope, BlobStore, attach, attachment_link, blob_meta, blob_meta_in_space,
    resolve_attachment,
};
pub use datadir::{DataDir, MAX_DATA_DIR_CHARS_WINDOWS, check_data_dir_len};
pub use db::{Db, DbOptions};
pub use embeddings::{
    Embedding, adopt_owned_embeddings, adopt_owned_embeddings_batch, embed_input_of, embed_key_of,
    gc_embeddings, get_embeddings, prune_owned_embeddings, put_embeddings, put_owned_embeddings,
};
pub use entries::{
    AuthorView, CHANGELOG_BETWEEN_MAX, Certainty, Entry, EntryFilter, EntryKind, EntryPatch,
    EntryRevision, EntryStatus, NewEntry, ReindexEntry, changelog_between, create_entry,
    delete_entry, enqueue_entry_upserts, entry_by_idempotency_key, entry_chunks_for_index,
    entry_revisions, find_release_by_version, finish_entries_reindexed, get_entry, list_entries,
    normalize_title, reindex_entries_page, restore_entry, retract_entry, supersede_entry,
    update_entry,
};
pub use jobs::{
    EntryPayload, FailedJob, IndexChunk, IndexFolder, IndexState, Job, JobOp, JobOutcome, JobStats,
    NodePayload, ReindexNote, chunks_for_index, claim_jobs, complete_job, enqueue_reindex,
    enqueue_upserts, fail_job, failed_jobs, finish_reindexed, folders_for_index, indexable_count,
    indexed_count, job_stats, mark_reindex, pending_reindex, reindex_page, release_job,
    requeue_running, set_index_state,
};
pub use migrate::Migration;
pub use nodes::{
    Attribution, AuthorKind, NewNote, Node, NodeKind, RevisionInfo, WriteError, chunk_params,
    count_tokens, create_folder, create_note, delete_node, find_by_name, get_node,
    get_node_by_path, list_children, list_revisions, list_trash, move_node, node_content,
    patch_note, patch_note_within, restore_node, revert_note, set_auto_l0, set_chunk_params,
    set_folder_l0, set_token_counter, token_counter_exact, unified_diff, update_note,
};
pub use paths::{NodePath, path_ancestors};
pub use search::{
    ChunkScope, EntryHit, FtsChannels, FtsHit, FtsQuery, LeafChunk, LeafHit, MARK_END, MARK_START,
    TargetFilter, leaf_term_hits, leaves_of_nodes, live_chunks, neighbor_leaves, node_leaves,
    pending_count, query_needles, sanitize_fts, search_fts, search_fts_channels, text_snippet,
};
pub use solution_registry::{RegistryReport, register_user_solutions};
pub use system::{
    Access, AuditRow, AuthToken, SYSTEM_MIGRATIONS, SkippedIndex, SystemStore, TokenInfo, User,
    WebSession,
};
pub use user_db::{
    Space, SpaceKind, USER_MIGRATIONS, UserDbs, create_solution, create_solution_with_id,
    get_solution, get_space, list_spaces, rename_solution_guid, space_of_kind,
};

/// Ошибки слоя хранения.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("ошибка sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("схема БД новее поддерживаемой: в файле {found}, поддерживается {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),
    #[error("не найдено")]
    NotFound,
    #[error("конфликт: {0}")]
    Conflict(String),
    #[error("некорректные данные: {0}")]
    BadInput(String),
    /// `Idempotency-Key` повторён с другим содержимым запроса.
    #[error("{0}")]
    IdempotencyMismatch(String),
    #[error("каталог данных уже используется другим экземпляром zam-server: {}", .0.display())]
    DataDirLocked(std::path::PathBuf),
    /// Windows: путь каталога данных так длинен, что файлы внутри него (базы SQLite, их
    /// копии перед миграцией, рабочий каталог Qdrant) выходят за MAX_PATH (260).
    #[error(
        "каталог данных {} слишком длинный для Windows: {len} символов при пределе {max} \
         (пути файлов внутри него превысили бы MAX_PATH = 260). Укажите более короткий путь: \
         data_dir в zam.toml, ZAM_DATA_DIR или --data-dir",
        .path.display()
    )]
    DataDirTooLong {
        path: std::path::PathBuf,
        len: usize,
        max: usize,
    },
    #[error("повреждённые данные: {0}")]
    Corrupt(String),
    #[error("не поддерживается: {0}")]
    Unsupported(String),
    #[error("некорректный список миграций: {0}")]
    InvalidMigrations(String),
    #[error("база закрыта или операция аварийно прервана")]
    Closed,
}

impl StoreError {
    /// Файл базы повреждён или не читается этой версией: повтор не поможет (нужны
    /// восстановление из копии или обновление программы), в отличие от занятости и
    /// ошибок ввода-вывода.
    pub fn is_permanent(&self) -> bool {
        match self {
            StoreError::Corrupt(_)
            | StoreError::SchemaTooNew { .. }
            | StoreError::InvalidMigrations(_) => true,
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => matches!(
                f.code,
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
            ),
            _ => false,
        }
    }
}
