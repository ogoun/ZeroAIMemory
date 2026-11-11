//! Хранилище ZeroAIMemory: открытие SQLite, поток-писатель, пул читателей, миграции,
//! узлы и ревизии заметок с синхронной индексацией (чанки §8.2, FTS5), outbox векторной
//! индексации (`index_jobs`), кэш эмбеддингов, полнотекстовый поиск, блобы вложений,
//! записи журналов решения (`entries`, M3), граф связей (`edges`, M3), реестр решений и доступ
//! участников (`system.db`), Vault (`vault_items`, ключи пользователей; M3), сессии агентов,
//! выданное им и показы/оценки (`sessions`, `session_seen`, `usage`; M4), кэш выжимок
//! (`answer_cache`, `cache_deps`; M4), ссылки на файлы решения (`file_refs`; M4.5), вложения как
//! цель индексации (`attachment_docs`, кэш извлечения `blob_extract`; M5), отчёты (вид записи
//! `report`, файлы `entry_attachments`; M5.5), карта кода решения (`code_*`, ссылки на символы;
//! M5.6), журнал идей (вид записи `idea`; M5.7), эпохи выданного сессии, учёт без сессии и
//! снимки сессий (`session_snapshots`; M6).

pub mod access;
pub mod archive;
pub mod attachments;
pub mod backup;
pub mod blobs;
pub mod cache;
pub mod certainty;
pub mod code;
pub mod datadir;
pub mod db;
pub mod embeddings;
pub mod entries;
pub mod export;
pub mod file_refs;
pub mod glossary;
pub mod graph;
pub mod ideas;
pub mod import;
pub mod jobs;
pub mod links;
pub mod migrate;
pub mod nodes;
pub mod paths;
pub mod questions;
mod schema_repair;
pub mod search;
mod secrets_delta;
pub mod sessions;
pub mod snapshots;
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
pub use attachments::{
    AttachmentDoc, BLOB_MISSING, CHUNK_REF_LETTER, CHUNKER_ATTACHMENTS_KEY, CachedExtract,
    DOCS_LIMITS_KEY, DocChunkHead, DocFound, DocIntake, DocLimitsKey, DocLookup, ExtractEnd,
    ExtractState, ExtractTarget, PDFIUM_UNAVAILABLE, ReindexAttachment, TOO_LARGE,
    attachment_chunk_owner, attachment_chunker_fingerprint, attachment_chunks_for_index,
    attachment_pieces, blob_sheet_locators, cached_extract, chunk_ref, detach, doc_chunk_heads,
    doc_chunk_texts, doc_sheet_locators, enqueue_attachment_upserts, ensure_extract_jobs,
    error_code, extract_cached, extract_target, find_docs, finish_extract, has_attachment_chunks,
    has_failed_with, list_attachments, put_extract, put_extract_failure, rechunk_docs_if_stale,
    reindex_attachments_page, reindex_docs, replace_attachment_chunks, requeue_failed_with,
    set_extract_state, sync_doc_limits,
};
pub use backup::{
    BACKUP_FLAG_FILE, BackupFlag, BackupFlagHold, Chunkers, OpenMode, backup_flag_active,
    backup_flag_path, hold_backup_flag, integrity_ok, meta_value, set_backup_flag_max_age,
    vacuum_into,
};
pub use blobs::{
    AttachScope, BlobStore, attach, attachment_link, blob_meta, blob_meta_by_sha,
    blob_meta_by_sha_in_space, blob_meta_in_space, resolve_attachment,
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
pub use datadir::{
    DataDir, DirLock, LockMark, MAX_DATA_DIR_CHARS_WINDOWS, RESTORE_INCOMPLETE_FILE,
    RESTORE_MARK_FILE, check_data_dir_len, lock_mark, restore_incomplete, try_lock_dir,
};
pub use db::{Db, DbOptions, LongReader};
pub use embeddings::{
    Embedding, adopt_owned_embeddings, adopt_owned_embeddings_batch, embed_input_of, embed_key_of,
    gc_embeddings, get_embeddings, prune_owned_embeddings, put_embeddings, put_owned_embeddings,
};
pub use entries::{
    AREA_MAX_CHARS, AuthorView, CHANGELOG_BETWEEN_MAX, Certainty, DEDUP_CANDIDATES_MAX,
    DedupCandidate, ENTRY_REF_LETTERS, Entry, EntryAttachment, EntryFilter, EntryKind, EntryPatch,
    EntryRevision, EntryStatus, KEYWORD_MAX_CHARS, NewEntry, NewReportFile, NewTrigger,
    PATTERN_MAX_CHARS, REASON_MAX_CHARS, ReindexEntry, TITLE_MAX_CHARS, TOOLS_MAX, TRIGGERS_MAX,
    Trigger, TriggerKind, TriggerRow, active_triggers, allowed_statuses, changelog_between,
    create_entry, create_report, delete_entry, enqueue_entry_upserts, entry_attachment,
    entry_attachments, entry_by_idempotency_key, entry_chunks_for_index, entry_revisions,
    entry_triggers, find_release_by_version, finish_entries_reindexed, first_images, get_entry,
    has_certainty, initial_status, is_entry_ref_letter, list_entries, normalize_area,
    normalize_title, reindex_entries_page, report_blob_meta_by_sha, restore_entry, retract_entry,
    set_triggers, supersede_entry, title_duplicates, update_entry, validate_trigger,
};
pub use export::{
    EntryDump, ExportSelect, NodeRevision, NoteAttachment, RefRow, SpaceReader, export_space,
    under_prefix,
};
pub use file_refs::{
    ChangedFile, FileRef, PathError, PathRefs, RefItem, RefState, VerifiedNote, check_commit,
    check_hash, files_changed, files_verified, normalize_ref_path, path_owners, refs_index,
    refs_to_path, shown_ref, stale_refs_of, symbols_uploaded, verified_notes,
};
pub use glossary::{
    ALIAS_USES_MAX, GlossaryEntry, TERM_TITLE_MAX_CHARS, TermConflict, TermData, active_terms,
    all_active_terms, check_term, find_alias_uses, find_term, glossary_version,
};
pub use graph::{
    EdgeOrigin, EdgeType, EdgeView, GRAPH_DEPTH_MAX, GRAPH_MAX_EDGES, GRAPH_MAX_NODES, Graph,
    GraphNode, GraphStore, IDEA_DERIVED_FROM_DENIED, NewEdge, get_edge, graph_node, graph_nodes,
};
pub use ideas::{
    Decided, Decision, IDEA_DEFER_DAYS_DEFAULT, IDEA_LINK_MAX_CHARS, IDEA_LINKS_MAX, IDEA_OPTIONS,
    IDEA_ORIGIN_REQUIRED, IDEA_REOPEN_OPTIONS, IdeaData, IdeaDecision, IdeaError, IdeaOrigin,
    IdeaPriority, NewDecision, PROPOSE_TEXT_MAX_CHARS, ProposalKind, TaskState, adopt_idea,
    any_idea, any_new_idea, check_idea, check_idea_shape, check_idea_transition, decide_idea,
    idea_candidate, idea_task_state, proactive_ideas, propose_idea, released_ideas, task_released,
};
pub use jobs::{
    AttachmentPayload, EntryPayload, FailedJob, IndexChunk, IndexFolder, IndexState, Job, JobOp,
    JobOutcome, JobStats, NodePayload, ReindexNote, chunks_for_index, claim_extract_jobs,
    claim_jobs, complete_job, defer_job, enqueue_reindex, enqueue_upserts, fail_job, failed_jobs,
    finish_reindexed, folder_embed_input, folders_for_index, give_up_job, indexable_count,
    indexed_count, job_stats, mark_reindex, pending_reindex, reindex_page, release_job,
    requeue_running, requeue_running_extract, set_index_state, set_reindex_pending,
};
pub use migrate::Migration;
pub use nodes::{
    Attribution, AuthorKind, BudgetCounterKind, NewNote, Node, NodeKind, PREFIX_L0_MAX_TOKENS,
    RevisionInfo, WriteError, budget_cap, budget_counter_kind, budget_tokenizer_id, chunk_params,
    count_tokens, count_tokens_chunker, create_folder, create_note, create_note_keyed, delete_node,
    entry_snapshot_text, find_by_name, fits_budget, get_node, get_node_by_path, list_children,
    list_revisions, list_trash, move_node, node_content, patch_note, patch_note_keyed,
    patch_note_within, prefix_l0, restore_node, revert_note, set_auto_l0, set_bundled_counter,
    set_chunk_params, set_folder_l0, set_prefix_l0, set_token_counter, token_counter_exact,
    unified_diff, update_note, update_note_keyed,
};
pub use paths::{NodePath, path_ancestors};
pub use questions::{
    ANSWER_TEXT_MAX_CHARS, Answer, AnswerBy, Answered, Channel, NewAnswer, OPTION_KEY_MAX_CHARS,
    OPTION_TEXT_MAX_CHARS, OPTIONS_MAX, QOption, QUESTION_DUPLICATES_MAX, QuestionCandidate,
    QuestionData, QuestionError, answer_question, check_question, check_question_transition,
    days_ago, open_questions, question_duplicates, recent_answered,
};
pub use search::{
    AliasGroup, AttachmentHit, ChunkScope, EntryHit, FocusLeaf, FolderRow, FtsChannels, FtsHit,
    FtsQuery, LeafChunk, LeafHit, LightSearch, MARK_END, MARK_START, TargetFilter, first_leaves,
    focus_leaves, leaf_term_hits, leaves_of_targets, live_chunks, live_folders, neighbor_leaves,
    node_leaves, pending_count, query_needles, query_terms, sanitize_fts, search_fts,
    search_fts_channels, search_fts_channels_light, text_snippet,
};
pub use sessions::{
    RECALL_PART, SESSION_ID_MAX_CHARS, SeenItem, SeenKey, SeenTarget, Session, SessionTouch,
    TOKEN_EVENT_MAX_CHARS, TokenCount, TokenStat, WHOLE_TARGET, add_memory_write, add_token_counts,
    add_tokens, bump_epoch, check_session_id, end_session, gc_sessions, get_session,
    latest_session, list_sessions, mark_seen, mark_seen_with_tokens, register_session,
    seen_entry_ids, seen_revs, seen_revs_any_epoch, session_epoch, session_holdout, session_tokens,
    sessions_tokens, set_holdout, touch_session,
};
pub use snapshots::{
    SnapTask, SnapshotRow, digest_snapshot, gc_snapshots, get_snapshot, put_snapshot,
    snapshot_tasks, snapshot_times, writes_after,
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
    UnattributedRow, UsageCounts, UsageEvent, UsageKind, UsageSource, add_unattributed,
    record_usage, record_usages, unattributed_tokens, usage_counts, usage_counts_many,
};
pub use user_db::{
    RechunkNotice, RechunkProgress, RechunkReason, Space, SpaceKind, USER_MIGRATIONS, UserDbs,
    create_solution, create_solution_with_id, get_solution, get_space, list_spaces,
    rename_solution_guid, rename_solution_space, space_of_kind,
};
pub use vault::{
    KEY_CHECK_SETTING, VAULT_ENV_TYPES, VAULT_OPEN_FIELDS, VAULT_TITLE_MAX_CHARS, VaultMeta,
    VaultRevision, VaultScope, check_vault_title, vault_delete, vault_environment, vault_get,
    vault_get_any, vault_list, vault_put, vault_revision_secret, vault_revisions, vault_update,
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
    /// Отчёт (`report`, M5.5) не меняется после записи: правка, замещение, отзыв, триггеры,
    /// захват и ответ над ним запрещены (сервер — 409 `/problems/report-immutable`).
    #[error("отчёт неизменяем — напишите новый")]
    ReportImmutable,
    /// Клиентская связь `derived_from` с целью-идеей (M5.7 §4): её ставит только «начать»
    /// (сервер — 422).
    #[error("{}", graph::IDEA_DERIVED_FROM_DENIED)]
    IdeaLinkDenied,
    /// Нарушение матрицы прав `certainty` ([`certainty::check_certainty`]).
    #[error(transparent)]
    Certainty(#[from] CertaintyError),
    #[error("каталог данных уже используется другим экземпляром zam-server: {}", .0.display())]
    DataDirLocked(std::path::PathBuf),
    /// В каталоге данных прервано `zam-server restore` (M6 §12б.3): данные неполны.
    #[error(
        "восстановление каталога данных {} не завершено: повторите zam-server restore \
         <копия> --replace (прежние данные — в backups/pre-restore-…)",
        .0.display()
    )]
    RestoreIncomplete(std::path::PathBuf),
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
