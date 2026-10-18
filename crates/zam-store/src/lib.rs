//! Хранилище ZeroAIMemory: открытие SQLite, поток-писатель, пул читателей, миграции,
//! узлы и ревизии заметок с синхронной индексацией (чанки секций, FTS5), полнотекстовый поиск.

pub mod datadir;
pub mod db;
pub mod migrate;
pub mod nodes;
pub mod paths;
pub mod search;
pub mod system;
pub mod time;
pub mod user_db;

pub use datadir::DataDir;
pub use db::Db;
pub use migrate::Migration;
pub use nodes::{
    Attribution, AuthorKind, NewNote, Node, NodeKind, RevisionInfo, WriteError, create_folder,
    create_note, delete_node, find_by_name, get_node, get_node_by_path, list_children,
    list_revisions, list_trash, move_node, node_content, patch_note, patch_note_within,
    restore_node, revert_note, unified_diff, update_note,
};
pub use paths::NodePath;
pub use search::{FtsHit, FtsQuery, MARK_END, MARK_START, sanitize_fts, search_fts};
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
