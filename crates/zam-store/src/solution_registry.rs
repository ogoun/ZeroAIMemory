//! Регистрация решений M1–M2 в реестре `system.db` (§11.6 ТЗ, этап M3).
//!
//! До M3 решения жили только в базах пользователей, и один GUID мог оказаться у двух
//! пользователей. [`register_user_solutions`] вызывается при старте сервера для каждого
//! пользователя и из `zam-server migrate`; вызывающий обходит пользователей по времени
//! создания (`users.created_at`; [`SystemStore::list_users`] упорядочен по UUIDv7 id, то
//! есть так же): первый зарегистрировавший GUID — его владелец.

use zam_core::{Id, SolutionGuid};

use crate::user_db::{SpaceKind, list_spaces, rename_solution_guid};
use crate::{Db, StoreError, SystemStore};

/// Итог регистрации решений одной базы.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryReport {
    /// Зарегистрировано сейчас (включая переименованные).
    pub registered: usize,
    /// Уже были в реестре за этим пользователем.
    pub already: usize,
    /// Конфликты GUID: (было, стало).
    pub renamed: Vec<(SolutionGuid, SolutionGuid)>,
    /// Пропущено из-за ошибки (WARN в логе); повторится при следующем вызове.
    pub failed: usize,
}

impl RegistryReport {
    /// Сложение итогов нескольких баз.
    pub fn absorb(&mut self, other: RegistryReport) {
        self.registered += other.registered;
        self.already += other.already;
        self.renamed.extend(other.renamed);
        self.failed += other.failed;
    }
}

/// Что сделано с одним пространством.
enum Outcome {
    Registered,
    Already,
    Renamed(SolutionGuid, SolutionGuid),
}

/// Регистрирует пространства `solution` базы `db` пользователя `user`. Идемпотентно.
///
/// - GUID не в реестре — регистрируется за `user`;
/// - уже за `user` — ничего;
/// - за другим владельцем — конфликт **[решение исполнителя]**: локальное пространство
///   получает новый GUID (данные остаются на месте), новый GUID регистрируется за `user`,
///   к исходному ставится запрос доступа `pending`, аудит `solution.guid_conflict`, WARN.
///   Сначала пишется реестр, затем база пользователя: прерванное переименование
///   довершается при следующем вызове (пространство уже в реестре за `user`).
///
/// Ошибка одного пространства не обрывает остальные: оно пропускается с WARN и считается
/// в [`RegistryReport::failed`]. Ошибкой функции остаётся только недоступный список
/// пространств базы.
pub async fn register_user_solutions(
    sys: &SystemStore,
    db: &Db,
    user: Id,
) -> Result<RegistryReport, StoreError> {
    let mut rep = RegistryReport::default();
    for space in list_spaces(db).await? {
        if space.kind != SpaceKind::Solution {
            continue;
        }
        let Some(guid) = space
            .solution_guid
            .as_deref()
            .and_then(|g| g.parse::<SolutionGuid>().ok())
        else {
            tracing::warn!(%user, space = %space.id, "пространство решения без корректного GUID пропущено");
            rep.failed += 1;
            continue;
        };
        match register_space(sys, db, user, &space, &guid).await {
            Ok(Outcome::Registered) => rep.registered += 1,
            Ok(Outcome::Already) => rep.already += 1,
            Ok(Outcome::Renamed(old, new)) => {
                rep.registered += 1;
                rep.renamed.push((old, new));
            }
            Err(e) => {
                tracing::warn!(
                    %user,
                    guid = %guid,
                    space = %space.id,
                    error = %e,
                    "решение не зарегистрировано в реестре: пропущено до следующего запуска"
                );
                rep.failed += 1;
            }
        }
    }
    Ok(rep)
}

async fn register_space(
    sys: &SystemStore,
    db: &Db,
    user: Id,
    space: &crate::user_db::Space,
    guid: &SolutionGuid,
) -> Result<Outcome, StoreError> {
    let info = match sys.get_solution(guid).await? {
        Some(i) => i,
        None => {
            let (info, created) = sys
                .register_solution(guid, user, space.id, &space.name)
                .await?;
            // гонка: между чтением и записью GUID мог зарегистрировать другой — тогда
            // это ветка конфликта ниже
            if info.owner_id == user && info.space_id == space.id {
                return Ok(if created {
                    Outcome::Registered
                } else {
                    Outcome::Already
                });
            }
            info
        }
    };
    if info.owner_id == user {
        if info.space_id != space.id {
            tracing::warn!(
                %user,
                guid = %guid,
                space = %space.id,
                registered_space = %info.space_id,
                "решение в реестре указывает на другое пространство базы владельца"
            );
        }
        return Ok(Outcome::Already);
    }
    // реестр уже записан за этим пространством (переименование прервано) — довершить с тем
    // же GUID
    let new = match sys.solution_by_space(space.id).await? {
        Some(i) if i.owner_id == user => i.guid,
        Some(i) => {
            return Err(StoreError::Conflict(format!(
                "пространство {} зарегистрировано за решением {} другого владельца",
                space.id, i.guid
            )));
        }
        None => SolutionGuid::new_v7(),
    };
    sys.register_guid_conflict(guid, &new, user, space.id, &space.name)
        .await?;
    rename_solution_guid(db, space.id, &new).await?;
    tracing::warn!(
        %user,
        old = %guid,
        new = %new,
        space = %space.id,
        "конфликт GUID решения: локальное решение переименовано, к исходному поставлен запрос доступа"
    );
    Ok(Outcome::Renamed(guid.clone(), new))
}
