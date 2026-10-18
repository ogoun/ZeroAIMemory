//! Хранилище ZeroAIMemory: открытие SQLite, поток-писатель, пул читателей, миграции.

pub mod datadir;
pub mod db;
pub mod migrate;
pub mod system;
pub mod time;

pub use datadir::DataDir;
pub use db::Db;
pub use migrate::Migration;
pub use system::{
    Access, AuditRow, AuthToken, SYSTEM_MIGRATIONS, SystemStore, TokenInfo, User, WebSession,
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
