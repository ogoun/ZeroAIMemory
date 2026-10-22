//! Vault (§7.6): шифрование значений ([`crypto`]) и ключи ([`keys`]): мастер-ключ сервера,
//! обёрнутые ключи пользователей, блокировка при потере или подмене мастер-ключа.
//! Хранение записей — `zam_store::vault`, REST — [`crate::api::vault`].
pub mod crypto;
pub mod keys;

pub use keys::{VaultKeys, VaultState};
