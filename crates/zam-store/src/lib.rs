//! Хранилище ZeroAIMemory: открытие SQLite, поток-писатель, пул читателей, миграции,
//! узлы и ревизии заметок с синхронной индексацией (чанки §8.2, FTS5), outbox векторной
//! индексации (`index_jobs`), кэш эмбеддингов, полнотекстовый поиск, блобы вложений.

pub mod blobs;
pub mod datadir;
pub mod db;
pub mod embeddings;
pub mod jobs;
pub mod migrate;
pub mod nodes;
pub mod paths;
pub mod search;
pub mod system;
pub mod time;
pub mod user_db;

pub use blobs::{AttachScope, BlobStore, attach, attachment_link, blob_meta, resolve_attachment};
pub use datadir::DataDir;
pub use db::{Db, DbOptions};
pub use embeddings::{Embedding, get_embeddings, put_embeddings};
pub use jobs::{
    IndexChunk, IndexState, Job, JobOp, JobOutcome, JobStats, NodePayload, chunks_for_index,
    claim_jobs, complete_job, enqueue_reindex, fail_job, indexed_count, job_stats, release_job,
    requeue_running, set_index_state,
};
pub use migrate::Migration;
pub use nodes::{
    Attribution, AuthorKind, NewNote, Node, NodeKind, RevisionInfo, WriteError, count_tokens,
    create_folder, create_note, delete_node, find_by_name, get_node, get_node_by_path,
    list_children, list_revisions, list_trash, move_node, node_content, patch_note,
    patch_note_within, restore_node, revert_note, set_auto_l0, set_folder_l0, set_token_counter,
    unified_diff, update_note,
};
pub use paths::NodePath;
pub use search::{
    ChunkScope, FtsChannels, FtsHit, FtsQuery, LeafChunk, LeafHit, MARK_END, MARK_START,
    leaf_term_hits, leaves_of_nodes, live_chunks, neighbor_leaves, node_leaves, pending_count,
    query_needles, sanitize_fts, search_fts, search_fts_channels, text_snippet,
};
pub use system::{
    Access, AuditRow, AuthToken, SYSTEM_MIGRATIONS, SystemStore, TokenInfo, User, WebSession,
};
pub use user_db::{
    Space, SpaceKind, USER_MIGRATIONS, UserDbs, create_solution, get_solution, get_space,
    list_spaces, space_of_kind,
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
    #[error("каталог данных уже используется другим экземпляром zam-server: {}", .0.display())]
    DataDirLocked(std::path::PathBuf),
    #[error("повреждённые данные: {0}")]
    Corrupt(String),
    #[error("не поддерживается: {0}")]
    Unsupported(String),
    #[error("некорректный список миграций: {0}")]
    InvalidMigrations(String),
    #[error("база закрыта или операция аварийно прервана")]
    Closed,
}
