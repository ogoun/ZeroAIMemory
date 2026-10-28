//! Хранилище ZeroAIMemory: открытие SQLite, поток-писатель, пул читателей, миграции,
//! узлы и ревизии заметок с синхронной индексацией (чанки §8.2, FTS5), outbox векторной
//! индексации (`index_jobs`), кэш эмбеддингов, полнотекстовый поиск, блобы вложений,
//! записи журналов решения (`entries`, M3), граф связей (`edges`, M3), реестр решений и доступ
//! участников (`system.db`), Vault (`vault_items`, ключи пользователей; M3), сессии агентов,
//! выданное им и показы/оценки (`sessions`, `session_seen`, `usage`; M4), кэш выжимок
//! (`answer_cache`, `cache_deps`; M4), ссылки на файлы решения (`file_refs`; M4.5).

pub mod access;
pub mod blobs;
pub mod cache;
pub mod certainty;
pub mod datadir;
pub mod db;
pub mod embeddings;
pub mod entries;
pub mod file_refs;
pub mod glossary;
pub mod graph;
pub mod jobs;
pub mod links;
pub mod migrate;
pub mod nodes;
pub mod paths;
pub mod questions;
pub mod search;
pub mod sessions;
pub mod solution_registry;
pub mod system;
pub mod tasks;
#[cfg(feature = "test-hooks")]
pub mod test_hooks;
pub mod time;
pub mod usage;
pub mod user_db;
pub mod vault;

pub use access::{AccessDecision, AccessEntry, AccessState, SolutionInfo, SolutionRole};
pub use blobs::{
    AttachScope, BlobStore, attach, attachment_link, blob_meta, blob_meta_in_space,
    resolve_attachment,
};
pub use cache::{
    CACHE_LOOKUP_MAX, CACHE_QUERY_MAX_CHARS, CACHE_SOURCES_MAX, CacheDep, CacheEntry, CacheError,
    CacheFilter, CacheGc, CacheHit, CachePut, CacheQuery, PromoteAs, SourceSpec, StaleKind,
    StaleSource, cache_count, cache_delete, cache_get, cache_list, cache_lookup, cache_promote,
    cache_put, cache_put_with, gc_cache, invalidate_target, mark_new_sources, normalize_query,
    scope_hash, section_text,
};
pub use certainty::{
    CertaintyChange, CertaintyError, Evidence, check_certainty, check_final_state,
    default_certainty,
};
pub use datadir::{DataDir, MAX_DATA_DIR_CHARS_WINDOWS, check_data_dir_len};
pub use db::{Db, DbOptions};
pub use embeddings::{
    Embedding, adopt_owned_embeddings, adopt_owned_embeddings_batch, embed_input_of, embed_key_of,
    gc_embeddings, get_embeddings, prune_owned_embeddings, put_embeddings, put_owned_embeddings,
};
pub use entries::{
    AREA_MAX_CHARS, AuthorView, CHANGELOG_BETWEEN_MAX, Certainty, DEDUP_CANDIDATES_MAX,
    DedupCandidate, ENTRY_REF_LETTERS, Entry, EntryFilter, EntryKind, EntryPatch, EntryRevision,
    EntryStatus, KEYWORD_MAX_CHARS, NewEntry, NewTrigger, PATTERN_MAX_CHARS, REASON_MAX_CHARS,
    ReindexEntry, TITLE_MAX_CHARS, TOOLS_MAX, TRIGGERS_MAX, Trigger, TriggerKind, TriggerRow,
    active_triggers, allowed_statuses, changelog_between, create_entry, delete_entry,
    enqueue_entry_upserts, entry_by_idempotency_key, entry_chunks_for_index, entry_revisions,
    entry_triggers, find_release_by_version, finish_entries_reindexed, get_entry, has_certainty,
    initial_status, is_entry_ref_letter, list_entries, normalize_area, normalize_title,
    reindex_entries_page, restore_entry, retract_entry, set_triggers, supersede_entry,
    title_duplicates, update_entry, validate_trigger,
};
pub use file_refs::{
    FileRef, PathError, PathRefs, RefItem, RefState, check_commit, check_hash, files_changed,
    files_verified, normalize_ref_path, refs_index, refs_to_path, stale_refs_of,
};
pub use glossary::{
    ALIAS_USES_MAX, GlossaryEntry, TERM_TITLE_MAX_CHARS, TermConflict, TermData, active_terms,
    all_active_terms, check_term, find_alias_uses, find_term, glossary_version,
};
pub use graph::{
    EdgeOrigin, EdgeType, EdgeView, GRAPH_DEPTH_MAX, GRAPH_MAX_EDGES, GRAPH_MAX_NODES, Graph,
    GraphNode, GraphStore, NewEdge, get_edge, graph_node, graph_nodes,
};
pub use jobs::{
    EntryPayload, FailedJob, IndexChunk, IndexFolder, IndexState, Job, JobOp, JobOutcome, JobStats,
    NodePayload, ReindexNote, chunks_for_index, claim_jobs, complete_job, enqueue_reindex,
    enqueue_upserts, fail_job, failed_jobs, finish_reindexed, folder_embed_input,
    folders_for_index, indexable_count, indexed_count, job_stats, mark_reindex, pending_reindex,
    reindex_page, release_job, requeue_running, set_index_state,
};
pub use migrate::Migration;
pub use nodes::{
    Attribution, AuthorKind, NewNote, Node, NodeKind, PREFIX_L0_MAX_TOKENS, RevisionInfo,
    WriteError, chunk_params, count_tokens, create_folder, create_note, delete_node, find_by_name,
    get_node, get_node_by_path, list_children, list_revisions, list_trash, move_node, node_content,
    patch_note, patch_note_within, prefix_l0, restore_node, revert_note, set_auto_l0,
    set_chunk_params, set_folder_l0, set_prefix_l0, set_token_counter, token_counter_exact,
    unified_diff, update_note,
};
pub use paths::{NodePath, path_ancestors};
pub use questions::{
    ANSWER_TEXT_MAX_CHARS, Answer, AnswerBy, Channel, NewAnswer, OPTION_KEY_MAX_CHARS,
    OPTION_TEXT_MAX_CHARS, OPTIONS_MAX, QOption, QUESTION_DUPLICATES_MAX, QuestionCandidate,
    QuestionData, QuestionError, answer_question, check_question, check_question_transition,
    days_ago, open_questions, question_duplicates, recent_answered,
};
pub use search::{
    AliasGroup, ChunkScope, EntryHit, FolderRow, FtsChannels, FtsHit, FtsQuery, LeafChunk, LeafHit,
    MARK_END, MARK_START, TargetFilter, first_leaves, leaf_term_hits, leaves_of_targets,
    live_chunks, live_folders, neighbor_leaves, node_leaves, pending_count, query_needles,
    query_terms, sanitize_fts, search_fts, search_fts_channels, text_snippet,
};
pub use sessions::{
    RECALL_PART, SESSION_ID_MAX_CHARS, SeenItem, SeenKey, Session, SessionTouch, WHOLE_TARGET,
    add_memory_write, check_session_id, end_session, gc_sessions, get_session, latest_session,
    list_sessions, mark_seen, seen_revs, touch_session,
};
pub use solution_registry::{RegistryReport, register_user_solutions};
pub use system::{
    Access, AuditRow, AuthToken, SYSTEM_MIGRATIONS, SkippedIndex, SystemStore, TokenInfo, User,
    WebSession,
};
pub use tasks::{
    CLAIM_TTL_DEFAULT_MIN, CLAIM_TTL_MAX_MIN, Claim, ClaimedTask, Owner, TaskData, TaskError,
    check_task, check_task_transition, claim_task, claimed_tasks, release_task,
};
pub use usage::{
    UsageCounts, UsageEvent, UsageKind, UsageSource, record_usage, record_usages, usage_counts,
    usage_counts_many,
};
pub use user_db::{
    RechunkNotice, RechunkProgress, RechunkReason, Space, SpaceKind, USER_MIGRATIONS, UserDbs,
    create_solution, create_solution_with_id, get_solution, get_space, list_spaces,
    rename_solution_guid, rename_solution_space, space_of_kind,
};
pub use vault::{
    KEY_CHECK_SETTING, VAULT_TITLE_MAX_CHARS, VaultMeta, VaultRevision, VaultScope,
    check_vault_title, vault_delete, vault_get, vault_get_any, vault_list, vault_put,
    vault_revision_secret, vault_revisions, vault_update,
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
    /// Ожидаемая ревизия (`If-Match`) записи журнала не совпала с текущей (§11.3).
    #[error("ревизия записи {current}, а не {expected}")]
    StaleRevision { expected: i64, current: i64 },
    #[error("некорректные данные: {0}")]
    BadInput(String),
    /// Имя или псевдоним термина пересекается с действующим термином пространства
    /// ([`glossary::TermConflict`], спецификация M4.5 §2.1).
    #[error("имя или псевдоним термина уже занят действующим термином")]
    TermConflict(Vec<glossary::TermConflict>),
    /// `Idempotency-Key` повторён с другим содержимым запроса.
    #[error("{0}")]
    IdempotencyMismatch(String),
    /// Нарушение матрицы прав `certainty` ([`certainty::check_certainty`]).
    #[error(transparent)]
    Certainty(#[from] CertaintyError),
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
