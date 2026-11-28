//! Помощники тестов ночной проверки памяти (M7.3, задача 5): общий замок файла, снятие тестовых
//! точек, пространство базы.
use jiff::Timestamp;
use zam_core::Id;
use zam_store::SpaceKind;

use super::api::Api;

/// Тестовые точки проверки и ночи, часы общие для процесса: тесты файла идут по одному.
pub static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Снимает переопределения проверки (`check::test_hooks`) и ночи (`works::night::test_hooks`)
/// при выходе из теста (и при панике).
pub struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        zam_server::check::test_hooks::reset();
        zam_server::works::night::test_hooks::reset();
    }
}

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// Пространство вида `kind` базы пользователя `user`.
pub async fn space_of(a: &Api, user: Id, kind: SpaceKind) -> Id {
    let db = a.st.users.get(user).await.unwrap();
    zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap()
        .id
}
