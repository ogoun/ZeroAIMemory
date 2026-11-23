//! Ошибки API в формате RFC 9457 (`application/problem+json`).
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use zam_core::SolutionGuid;
use zam_store::StoreError;

/// Тип problem «символа нет в карте кода» (M5.6 §9): MCP `memory_code` отличает его от прочих 404.
pub const CODE_SYMBOL_NOT_FOUND: &str = "/problems/code-symbol-not-found";

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("требуется аутентификация")]
    Unauthorized,
    #[error("доступ запрещён")]
    Forbidden,
    /// 403 с пояснением (например, запись участником с режимом `ro`).
    #[error("{0}")]
    Denied(String),
    /// 403 `/problems/password-change-required` (§11.4): пользователь входит с паролем,
    /// заданным администратором, и обязан сменить его (`POST /auth/password`).
    #[error("смените пароль, заданный администратором: POST /auth/password")]
    PasswordChangeRequired,
    /// Нет доступа к общему решению (§11.6): запрос ждёт, отклонён или отозван.
    #[error("нет доступа к решению {guid}: {}", access_state_text(state))]
    SolutionAccess {
        guid: SolutionGuid,
        /// `pending` | `rejected` | `revoked`.
        state: &'static str,
    },
    #[error("ресурс не найден")]
    NotFound,
    /// 400 `/problems/session-required` (M6 §8а): запись снимка сессии без явной сессии —
    /// `X-Zam-Session` не совпадает с `{id}` пути.
    #[error("{0}")]
    SessionRequired(String),
    /// 404 `/problems/session-not-found` (M6 §8а): сессии нет в базе пользователя запроса — в
    /// отличие от 404 старого сервера без маршрута (`about:blank`).
    #[error("сессия не найдена: зарегистрируйте её (POST /sessions)")]
    SessionNotFound,
    /// 404 [`CODE_SYMBOL_NOT_FOUND`] (M5.6 §9): символа нет в карте решения — в отличие от
    /// выключенной карты и неизвестного решения (`about:blank`).
    #[error("{0}")]
    CodeSymbolNotFound(String),
    /// `extra` — расширения RFC 9457: поля попадают в тело problem рядом с `detail`.
    #[error("{detail}")]
    Conflict {
        detail: String,
        extra: Map<String, Value>,
    },
    /// 409 `/problems/possible-duplicate` (§7.5): похожий insight уже есть; `extra` —
    /// `candidates` и `dedup`.
    #[error("{detail}")]
    PossibleDuplicate {
        detail: String,
        extra: Map<String, Value>,
    },
    /// 409 `/problems/term-conflict` (M4.5 §2.1): имя или псевдоним термина занят действующим
    /// термином пространства; тело несёт `conflicts: [{id, title, matched}]`.
    #[error("имя или псевдоним термина уже занят: {}", term_conflict_names(.0))]
    TermConflict(Vec<zam_store::TermConflict>),
    /// 409 `/problems/task-claimed` (M4.5 §3): задача захвачена другой сессией; тело несёт
    /// `claimed_by {session, agent, until}`.
    #[error("задача уже захвачена другой сессией")]
    TaskClaimed(zam_store::Claim),
    /// 409 `/problems/already-answered` (M4.5 §4): вопрос уже отвечен; тело несёт `decision_id`
    /// решения из первого ответа.
    #[error("вопрос уже отвечен")]
    AlreadyAnswered { decision_id: Option<zam_core::Id> },
    /// 409 `/problems/report-immutable` (M5.5 §4): отчёт не меняется после записи —
    /// исправление пишется новым отчётом.
    #[error("отчёт неизменяем — напишите новый")]
    ReportImmutable,
    /// 422 `/problems/reason-required` (M4.5 §3): переоткрытие задачи без причины.
    #[error("{0}")]
    ReasonRequired(String),
    /// 400 `/problems/by-required` (M4.5 §4, финальное ревью I-2): ответ на вопрос токеном
    /// без явного `by` — чей это ответ, решает агент, а не умолчание.
    #[error("укажите by: human — ответ человека, by: agent — решение агента")]
    ByRequired,
    /// 403 `/problems/idea-human-decision` (M5.7 §5): решение по идее за человека агентом.
    #[error("решение по идее принимает человек: передайте его слова с by: human")]
    IdeaHumanDecision,
    /// 403 `/problems/idea-human-owned` (M5.7 §7.1): токен правит или удаляет идею человека.
    #[error("идею человека правит человек; дополнение — append")]
    IdeaHumanOwned,
    /// 409 `/problems/idea-agent-origin` (M5.7 §5): идея «от агента» не предлагается и не
    /// начинается, пока человек не примет её как свою.
    #[error("идея от агента: сначала человек принимает идею как свою")]
    IdeaAgentOrigin,
    /// 409 `/problems/idea-mode` (M6.5 §7): операция не того режима идей `ideas.mode` владельца
    /// базы — разбор в режиме `propose` или `propose` идеи `new` в режиме `review`; в `detail` —
    /// что делать вместо неё.
    #[error("{0}")]
    IdeaMode(String),
    /// 409 `/problems/already-decided` (M5.7 §5): «начать» уже было; тело несёт `task_id`.
    #[error("по идее уже решено «начать»: задача создана (task_id)")]
    AlreadyDecided { task_id: Option<zam_core::Id> },
    /// 409 `/problems/already-adopted` (M5.7 §5.4): идея уже человека.
    #[error("идея уже принята как своя")]
    AlreadyAdopted,
    /// 409 `/problems/works-disabled` (M7.3 §6.1, решение координатора 76): `[works] enabled =
    /// false` — ручные запуски внутренних работ и проверки памяти, проверка подключения модели.
    #[error("{0}")]
    WorksDisabled(String),
    /// 409 `/problems/work-not-claimed` (M7.3 §6.3 п. 4): итог или возврат работы не от сессии,
    /// захватившей её (`running`), — работа снята, истекла, закрыта или захвачена другой сессией.
    #[error("{0}")]
    WorkNotClaimed(String),
    /// 409 `/problems/backup-running` (M7 §5.6): копия в процессе сервера уже идёт — со страницы
    /// «Копии» или по расписанию (один замок); тело несёт `running {started_at, by}`.
    #[error("копия уже идёт ({}): дождитесь её конца", backup_by_text(.0.by))]
    BackupRunning(crate::backup::runner::Running),
    /// 409 `/problems/check-running` (M7.3 §6.5 п. 1): у пользователя уже идёт прогон проверки
    /// памяти; тело несёт `run` — идущий прогон.
    #[error("прогон проверки памяти уже идёт — дождитесь его конца")]
    CheckRunning { run: zam_core::Id },
    /// 409 `/problems/check-accept-not-latest` (M7.3 §6.5 п. 5): принять как норму можно только
    /// последний ночной прогон идущего эпизода регрессии.
    #[error("принять как норму можно только последний ночной прогон идущего эпизода регрессии")]
    CheckAcceptNotLatest,
    /// 422 `/problems/option-required` (M5.7 §5.3): ответ на вопрос-предложение без варианта.
    #[error("{0}")]
    OptionRequired(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    /// 428 `/problems/precondition-required` (§11.3): правка без `If-Match`; в `detail` —
    /// что передать.
    #[error("{0}")]
    PreconditionRequired(String),
    #[error("{0}")]
    Unprocessable(String),
    /// 422 `/problems/locator-invalid` (M5 §4): `locator` в `/read` неверного формата или у цели
    /// без документа (заметка, запись журнала).
    #[error("{0}")]
    LocatorInvalid(String),
    /// 422 источника выжимки кэша (§9.4): `kind` — тип problem `/problems/cache-cross-db`,
    /// `/problems/cache-forbidden-source` или `/problems/cache-source-not-found`.
    #[error("{detail}")]
    CacheSource { kind: &'static str, detail: String },
    /// 422 `/problems/idempotency-mismatch`: ключ повторён с другим запросом.
    #[error("{0}")]
    IdempotencyMismatch(String),
    /// 422 `/problems/secret-context-changed`, `code: secret_context_changed` (M7 §4.3.2, круг
    /// правок 1 задачи 6 M7.1): правка агента по маске выводит восстановленный секрет из-под
    /// маски; в `detail` — номера строк маскированного вида, без значений.
    #[error("{0}")]
    SecretContextChanged(String),
    /// 422 `/problems/certainty` (§7.5): нарушение матрицы прав `certainty` — нет
    /// доказательства или причины; `code` — машинный код ([`zam_store::CertaintyError::code`]).
    #[error("{0}")]
    Certainty(zam_store::CertaintyError),
    #[error("слишком много запросов")]
    RateLimited { retry_after_s: u64 },
    /// 503 `/problems/busy` с `Retry-After`: заняты разрешения на большие тела запросов
    /// ([`crate::api::big_body`]).
    #[error("сервер занят обработкой больших запросов — повторите позже")]
    Busy { retry_after_s: u64 },
    /// 503 `/problems/busy` с `Retry-After`: заняты слоты экспорта пространства
    /// ([`crate::api::transfer::export`], M6 §12б.5).
    #[error("уже идут другие экспорты пространств — повторите позже")]
    ExportBusy { retry_after_s: u64 },
    /// 503 `/problems/busy` с `Retry-After`: сервер останавливается (экспорт не начинается).
    #[error("сервер останавливается — повторите позже")]
    ShuttingDown { retry_after_s: u64 },
    #[error("{0}")]
    Unavailable(String),
    /// 507 `/problems/insufficient-storage`: на диске сервера мало места для приёма (импорт
    /// пространства, M6 §12б.5).
    #[error("{0}")]
    InsufficientStorage(String),
    /// 408 `/problems/timeout`: тело запроса не получено в срок ([`crate::api::big_body`]).
    #[error("{0}")]
    RequestTimeout(String),
    /// 503 `/problems/vault-locked` (§7.6): мастер-ключ не подходит или файла ключа нет при
    /// существующих ключах; в `detail` — причина (без путей и ключей).
    #[error("Vault заблокирован: {0}")]
    VaultLocked(String),
    /// Подробности только в лог; клиенту уходит общий текст.
    #[error("внутренняя ошибка")]
    Internal(String),
}

impl ApiError {
    /// 500 для прерванной задачи `spawn_blocking`: в журнал — только вид сбоя. Текст паники
    /// (`JoinError` через `Display`) не выводится: при панике на срезе строки в нём бывает
    /// до 256 символов незамаскированного исходника заметки.
    pub fn aborted(what: &str, e: &tokio::task::JoinError) -> ApiError {
        ApiError::Internal(format!(
            "{what} прерван (panic = {}, cancelled = {})",
            e.is_panic(),
            e.is_cancelled()
        ))
    }

    /// Конфликт без расширений.
    pub fn conflict(detail: impl Into<String>) -> ApiError {
        ApiError::Conflict {
            detail: detail.into(),
            extra: Map::new(),
        }
    }

    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "about:blank", "Bad Request"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "about:blank", "Unauthorized"),
            ApiError::Forbidden | ApiError::Denied(_) => {
                (StatusCode::FORBIDDEN, "about:blank", "Forbidden")
            }
            ApiError::PasswordChangeRequired => (
                StatusCode::FORBIDDEN,
                "/problems/password-change-required",
                "Forbidden",
            ),
            ApiError::SolutionAccess { .. } => (
                StatusCode::FORBIDDEN,
                "/problems/solution-access",
                "Forbidden",
            ),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "about:blank", "Not Found"),
            ApiError::SessionRequired(_) => (
                StatusCode::BAD_REQUEST,
                "/problems/session-required",
                "Bad Request",
            ),
            ApiError::SessionNotFound => (
                StatusCode::NOT_FOUND,
                "/problems/session-not-found",
                "Not Found",
            ),
            ApiError::Conflict { .. } => (StatusCode::CONFLICT, "/problems/conflict", "Conflict"),
            ApiError::PossibleDuplicate { .. } => (
                StatusCode::CONFLICT,
                "/problems/possible-duplicate",
                "Conflict",
            ),
            ApiError::TermConflict(_) => {
                (StatusCode::CONFLICT, "/problems/term-conflict", "Conflict")
            }
            ApiError::TaskClaimed(_) => {
                (StatusCode::CONFLICT, "/problems/task-claimed", "Conflict")
            }
            ApiError::AlreadyAnswered { .. } => (
                StatusCode::CONFLICT,
                "/problems/already-answered",
                "Conflict",
            ),
            ApiError::ReportImmutable => (
                StatusCode::CONFLICT,
                "/problems/report-immutable",
                "Conflict",
            ),
            ApiError::ReasonRequired(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/reason-required",
                "Unprocessable Content",
            ),
            ApiError::ByRequired => (
                StatusCode::BAD_REQUEST,
                "/problems/by-required",
                "Bad Request",
            ),
            ApiError::IdeaHumanDecision => (
                StatusCode::FORBIDDEN,
                "/problems/idea-human-decision",
                "Forbidden",
            ),
            ApiError::IdeaHumanOwned => (
                StatusCode::FORBIDDEN,
                "/problems/idea-human-owned",
                "Forbidden",
            ),
            ApiError::IdeaAgentOrigin => (
                StatusCode::CONFLICT,
                "/problems/idea-agent-origin",
                "Conflict",
            ),
            ApiError::IdeaMode(_) => (StatusCode::CONFLICT, "/problems/idea-mode", "Conflict"),
            ApiError::AlreadyDecided { .. } => (
                StatusCode::CONFLICT,
                "/problems/already-decided",
                "Conflict",
            ),
            ApiError::AlreadyAdopted => (
                StatusCode::CONFLICT,
                "/problems/already-adopted",
                "Conflict",
            ),
            ApiError::WorksDisabled(_) => {
                (StatusCode::CONFLICT, "/problems/works-disabled", "Conflict")
            }
            ApiError::WorkNotClaimed(_) => (
                StatusCode::CONFLICT,
                "/problems/work-not-claimed",
                "Conflict",
            ),
            ApiError::BackupRunning(_) => {
                (StatusCode::CONFLICT, "/problems/backup-running", "Conflict")
            }
            ApiError::CheckRunning { .. } => {
                (StatusCode::CONFLICT, "/problems/check-running", "Conflict")
            }
            ApiError::CheckAcceptNotLatest => (
                StatusCode::CONFLICT,
                "/problems/check-accept-not-latest",
                "Conflict",
            ),
            ApiError::OptionRequired(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/option-required",
                "Unprocessable Content",
            ),
            ApiError::PayloadTooLarge(_) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "/problems/too-large",
                "Payload Too Large",
            ),
            ApiError::PreconditionRequired(_) => (
                StatusCode::PRECONDITION_REQUIRED,
                "/problems/precondition-required",
                "Precondition Required",
            ),
            ApiError::Unprocessable(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/unprocessable",
                "Unprocessable Content",
            ),
            ApiError::CodeSymbolNotFound(_) => {
                (StatusCode::NOT_FOUND, CODE_SYMBOL_NOT_FOUND, "Not Found")
            }
            ApiError::LocatorInvalid(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/locator-invalid",
                "Unprocessable Content",
            ),
            ApiError::CacheSource { kind, .. } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                kind,
                "Unprocessable Content",
            ),
            ApiError::IdempotencyMismatch(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/idempotency-mismatch",
                "Unprocessable Content",
            ),
            ApiError::SecretContextChanged(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/secret-context-changed",
                "Unprocessable Content",
            ),
            ApiError::Certainty(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/problems/certainty",
                "Unprocessable Content",
            ),
            ApiError::RateLimited { .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                "/problems/rate-limited",
                "Too Many Requests",
            ),
            ApiError::Busy { .. } | ApiError::ExportBusy { .. } | ApiError::ShuttingDown { .. } => {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "/problems/busy",
                    "Service Unavailable",
                )
            }
            ApiError::RequestTimeout(_) => (
                StatusCode::REQUEST_TIMEOUT,
                "/problems/timeout",
                "Request Timeout",
            ),
            ApiError::Unavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "about:blank",
                "Service Unavailable",
            ),
            ApiError::InsufficientStorage(_) => (
                StatusCode::INSUFFICIENT_STORAGE,
                "/problems/insufficient-storage",
                "Insufficient Storage",
            ),
            ApiError::VaultLocked(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "/problems/vault-locked",
                "Service Unavailable",
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "about:blank",
                "Internal Server Error",
            ),
        }
    }
}

/// Занятые имена для `detail` (до 5, как заданы в запросе).
fn term_conflict_names(c: &[zam_store::TermConflict]) -> String {
    let mut names: Vec<String> = Vec::new();
    for x in c {
        let n = format!("«{}» (термин «{}»)", x.matched, x.title);
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.truncate(5);
    names.join(", ")
}

/// Кто снимает копию — для `detail` 409 `/problems/backup-running`.
fn backup_by_text(by: crate::backup::runner::RunBy) -> &'static str {
    match by {
        crate::backup::runner::RunBy::WebUi => "из WebUI",
        crate::backup::runner::RunBy::Schedule => "по расписанию",
    }
}

/// Пояснение состояния запроса доступа для `detail`.
fn access_state_text(state: &str) -> &'static str {
    match state {
        "pending" => "запрос ждёт подтверждения владельца",
        "rejected" => "запрос отклонён владельцем",
        "revoked" => "доступ отозван владельцем",
        _ => "доступа нет",
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::NotFound,
            StoreError::Conflict(m) => ApiError::conflict(m),
            // устаревшая ревизия записи журнала (§11.3): 409 с текущей ревизией `rev`
            e @ StoreError::StaleRevision { current, .. } => {
                let mut extra = Map::new();
                extra.insert("rev".into(), json!(current));
                ApiError::Conflict {
                    detail: e.to_string(),
                    extra,
                }
            }
            // клиентская связь `derived_from` на идею — 422 в `POST /edges` и `links` (M5.7 §4)
            e @ StoreError::IdeaLinkDenied => ApiError::Unprocessable(e.to_string()),
            StoreError::BadInput(m) => ApiError::BadRequest(m),
            StoreError::TermConflict(c) => ApiError::TermConflict(c),
            StoreError::IdempotencyMismatch(m) => ApiError::IdempotencyMismatch(m),
            StoreError::Certainty(c) => ApiError::Certainty(c),
            StoreError::ReportImmutable => ApiError::ReportImmutable,
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind, title) = self.parts();
        if let ApiError::Internal(d) = &self {
            tracing::error!(detail = %d, "внутренняя ошибка");
        }
        let mut body = json!({
            "type": kind,
            "title": title,
            "status": status.as_u16(),
            "detail": self.to_string(),
        });
        if let (
            ApiError::Conflict { extra, .. } | ApiError::PossibleDuplicate { extra, .. },
            Value::Object(o),
        ) = (&self, &mut body)
        {
            for (k, v) in extra {
                // стандартные поля problem не перекрываются
                o.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        if let (ApiError::CheckRunning { run }, Value::Object(o)) = (&self, &mut body) {
            o.insert("run".into(), json!(run.to_string()));
        }
        if let (ApiError::TermConflict(c), Value::Object(o)) = (&self, &mut body) {
            o.insert("conflicts".into(), json!(c));
        }
        if let (ApiError::AlreadyAnswered { decision_id }, Value::Object(o)) = (&self, &mut body) {
            o.insert("decision_id".into(), json!(decision_id));
        }
        if let (ApiError::AlreadyDecided { task_id }, Value::Object(o)) = (&self, &mut body) {
            o.insert("task_id".into(), json!(task_id));
        }
        if let (ApiError::TaskClaimed(c), Value::Object(o)) = (&self, &mut body) {
            o.insert("claimed_by".into(), json!(c));
        }
        if let (ApiError::BackupRunning(r), Value::Object(o)) = (&self, &mut body) {
            o.insert("running".into(), json!(r));
        }
        if let (ApiError::Certainty(c), Value::Object(o)) = (&self, &mut body) {
            o.insert("code".into(), json!(c.code()));
        }
        if let (ApiError::SecretContextChanged(_), Value::Object(o)) = (&self, &mut body) {
            o.insert(
                "code".into(),
                json!(crate::api::secret_restore::SECRET_CONTEXT_CHANGED),
            );
        }
        if let (ApiError::SolutionAccess { guid, state }, Value::Object(o)) = (&self, &mut body) {
            o.insert("state".into(), json!(state));
            o.insert("agent_solution_id".into(), json!(guid));
        }
        let mut resp = (status, Json(body)).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        match self {
            ApiError::RateLimited { retry_after_s }
            | ApiError::Busy { retry_after_s }
            | ApiError::ExportBusy { retry_after_s }
            | ApiError::ShuttingDown { retry_after_s } => {
                h.insert(header::RETRY_AFTER, HeaderValue::from(retry_after_s));
            }
            // RFC 9110 §15.5.2: 401 обязан нести схему.
            ApiError::Unauthorized => {
                h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            _ => {}
        }
        resp
    }
}

/// Обработчик неизвестных маршрутов.
pub async fn not_found() -> ApiError {
    ApiError::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Текст паники (с фрагментом заметки) не попадает ни в подробности 500, ни в ответ.
    #[tokio::test]
    async fn aborted_hides_panic_message() {
        let e = tokio::task::spawn_blocking(|| panic!("SECRET-MARKER ghp_abc"))
            .await
            .unwrap_err();
        let err = ApiError::aborted("рендер", &e);
        let ApiError::Internal(d) = &err else {
            panic!("ожидалась Internal");
        };
        assert!(!d.contains("SECRET-MARKER"), "{d}");
        assert!(d.contains("panic = true"), "{d}");
        assert!(!err.to_string().contains("SECRET-MARKER"));
    }

    /// Неизменность отчёта из хранилища — 409 `/problems/report-immutable`, а не 500.
    #[test]
    fn report_immutable_is_409() {
        let e = ApiError::from(StoreError::ReportImmutable);
        assert!(matches!(e, ApiError::ReportImmutable));
        let (status, kind, _) = e.parts();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(kind, "/problems/report-immutable");
    }
}
