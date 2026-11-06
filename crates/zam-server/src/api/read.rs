//! Чтение пачкой `POST /read` (§9.3 шаг 2, §12; задача 6 M4).
//!
//! ```text
//! POST /read {items: [{id, tier?, section?, locator?, max_tokens?, if_rev?}],
//!             session_id?, format?: "json"|"md", budget_tokens?}
//! ```
//!
//! - `id`: `n:<id>` (узел), `i:|c:|r:|t:|k:|q:<id>` (запись журнала; `e:` тоже), голый id (узел,
//!   иначе запись) или путь `<пространство>:/<путь>` (`knowledge:/Инструменты/Bash`). Не больше
//!   `read.max_items` (20) элементов, пусто или больше — 422. Некорректный `tier`, нулевые
//!   `max_tokens` и `budget_tokens` — 400. Всё это проверяется до чтения;
//! - **вложения** (M5 §4, задача 7): `a:<chunk_id>` — фрагмент из выдачи поиска (его документ,
//!   `locator` по умолчанию — `locator` чанка), `b:<sha256>` — вложение блоба (несколько заметок с
//!   тем же файлом — первое по дате привязки и `warnings`), `d:<id>` — документ (`attachment_id`
//!   карточки). Ищутся только в читаемых областях ([`attachment_scope`]); недоступное и
//!   несуществующее — одинаковый `not_found` без пояснений. `locator` (`page:12`, `slide:3`,
//!   `para:40`, `sheet:<лист>!A1:F40`) — текст единиц документа с этим адресом: лист — блоки,
//!   пересекающие диапазон; абзац внутри блока DOCX — этот блок; лист ищется по сырому имени
//!   (и по его маске из оглавления), в ответе имя листа замаскировано сканером секретов. Без
//!   `locator` у `b:`/`d:` и при `tier` L0/L1 — оглавление: части документа с первой строкой (до
//!   [`TOC_MAX_ENTRIES`]) и подсказка. Текст — из чанков (секреты замаскированы при нарезке);
//!   `section` и `since_rev` к вложению неприменимы (`invalid`), `if_rev: 0` — `unchanged`;
//! - **`locator` неверного формата** или у цели без документа (`n:`, запись, путь, голый id) —
//!   422 `/problems/locator-invalid` до чтения;
//! - `tier` — `L0` | `L1` | `L2` (по умолчанию). Узел: сводка, оглавление с первым предложением
//!   секций, текст. Запись журнала **[решение исполнителя]**: L0 — заголовок (у term —
//!   определение), L1 — `note_l1` тела (нет — L0), L2 — тело. `section` — путь заголовков (как в
//!   `GET /nodes`);
//! - `if_rev` равен текущей ревизии → элемент `unchanged` без текста (и без учёта показа);
//! - **`since_rev: N`** (M4.5 §7) — изменения с ревизии `N`: `N` равна текущей — `unchanged`
//!   (как `if_rev`); меньше — `diff` (унифицированный, контекст 3, как `GET /nodes/{id}/diff`,
//!   заголовки `rev N` / `rev M`) и `rev`; дифф длиннее полного текста (в токенах) или ревизии
//!   `N` уже нет — полный текст `content` с `diff_skipped: true`. У записи журнала дифф —
//!   построчно по тексту снимков ревизий (`entry_revisions`): `title`, тело и `data`
//!   каноническим JSON ([`zam_store::entry_snapshot_text`]), полный текст — тот же снимок.
//!   `N` < 1, больше текущей, вместе с `if_rev`, с `tier` кроме L2 или с `section` — 400.
//!   Отданный целиком дифф или полный текст отмечает цель выданной сессии (как полный L2);
//! - **бюджет**: `budget_tokens` (по умолчанию `read.budget_tokens` = 1500) на весь ответ,
//!   заголовки и текст. Элемент, не влезший целиком (или длиннее своего `max_tokens`),
//!   обрезается с пометкой `…ещё N токенов`; обрезается и первый элемент (хоть что-то
//!   отдаётся всегда), следующие — только заголовок и `(budget)`. В `~T tok` заголовка —
//!   размер полного текста элемента (как в карточке поиска), в последней строке и в
//!   `total_tokens` — оценка выданного;
//! - ошибка по элементу лежит в самом элементе, ответ остаётся 200: `error` —
//!   `not_found` (нет, удалён, Human по токену, Vault, чужая база вне решения, неизвестная
//!   секция), `forbidden`, `solution_access` (+ `state`, `agent_solution_id`), `invalid`
//!   (папка без текста, неоднозначная секция, некорректный путь). Сбой хранилища — 500;
//! - **`session_seen`** (§6.3, решение контроллера R10/R11): сессия — `session_id` из тела
//!   или `X-Zam-Session`, лежит в базе пользователя запроса. Цель отмечается целиком
//!   (`part = WHOLE_TARGET`, ревизия выданного текста) только когда отдан **полный текст L2
//!   без `section` и без обрезки** по `max_tokens` и бюджету; секция, L0/L1, обрезанный текст,
//!   `unchanged`, `(budget)` и ошибки — не отмечаются. Вложение: текст по `locator`, отданный
//!   целиком, — ключ `blob` (sha256), часть — `locator`, ревизия 0; оглавление не отмечается.
//!   Касание сессии и отметка — одна
//!   транзакция; сбой учёта — WARN (без id сессии и текста), ответ не портится. Некорректный
//!   id сессии — 400;
//! - если у какого-то элемента ошибка, в ответе (md и json) заголовок
//!   `X-Zam-Read-Error: <код первой>`:
//!   клиенту не нужно разбирать Markdown, где текст заметки мог бы выглядеть как заголовок ошибки;
//! - ответ `json` (по умолчанию, как у поиска): `{items: [...], total_tokens}`; `md` —
//!   заголовок на элемент `### n:<id> <пространство>/<путь> › <секция>  rev<N> · ~<T> tok`
//!   (у записи — как карточка §9.3: `### i:<id> insight/gotcha «заголовок»  active ·
//!   hypothesis · rev2 · ~120 tok`), текст и последняя строка `~<total> tok`.
//!
//! Маршрут — служебный, не в `memory_routes`: `/read` читает память и не пишет в неё, потому
//! `memory_writes` сессии не растёт (решение контроллера R12).
use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_docs::Locator;
use zam_markdown::mask_text;
use zam_search::truncate_to_tokens;
use zam_store::{
    CHUNK_REF_LETTER, DocLookup, EntryKind, NodeKind, NodePath, SeenItem, SeenKey, SeenTarget,
    StoreError, WHOLE_TARGET, count_tokens, doc_chunk_heads, doc_chunk_texts, entry_revisions,
    entry_snapshot_text, get_node_by_path, mark_seen, node_content,
};

use super::doc_locator::Sheets;
use super::json_body;
use super::nodes::diff_blocking;
use super::nodes::read::{Tier, l1, parse_tier, section_slice};
use super::scope::{
    AttachmentScope, EntryScope, Mode, NodeScope, attachment_scope, entry_scope, live, live_entry,
    node_scope, space_scope,
};
use super::search::md_inline;
use super::sessions::{McpOrigin, explicit_only, request_session};
use crate::app::AppState;
use crate::auth::Principal;
use crate::error::ApiError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    items: Vec<ReadItem>,
    session_id: Option<String>,
    format: Option<String>,
    budget_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadItem {
    id: String,
    tier: Option<String>,
    section: Option<String>,
    /// Адрес части вложения (`page:12`, `sheet:Итоги!A1:F40`, …; M5 §4).
    locator: Option<Value>,
    max_tokens: Option<usize>,
    if_rev: Option<i64>,
    /// Изменения с этой ревизии (M4.5 §7).
    since_rev: Option<i64>,
}

/// Ошибка элемента: код, пояснение и дополнительные поля.
struct Fail {
    code: &'static str,
    detail: Option<String>,
    extra: Vec<(&'static str, Value)>,
}

impl Fail {
    fn new(code: &'static str, detail: Option<String>) -> Fail {
        Fail {
            code,
            detail,
            extra: Vec::new(),
        }
    }
}

/// Сбой чтения элемента: ошибка самого элемента (остаётся в ответе) или сбой запроса.
enum Failure {
    Item(Fail),
    Fatal(ApiError),
}

impl From<ApiError> for Failure {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::NotFound => Failure::Item(Fail::new("not_found", None)),
            ApiError::Forbidden | ApiError::Denied(_) => {
                Failure::Item(Fail::new("forbidden", None))
            }
            ApiError::SolutionAccess { guid, state } => Failure::Item(Fail {
                code: "solution_access",
                detail: None,
                extra: vec![("state", json!(state)), ("agent_solution_id", json!(guid))],
            }),
            ApiError::BadRequest(m) | ApiError::Unprocessable(m) => {
                Failure::Item(Fail::new("invalid", Some(m)))
            }
            other => Failure::Fatal(other),
        }
    }
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Self {
        ApiError::from(e).into()
    }
}

/// Что получено по элементу.
enum Content {
    /// `if_rev` (или `since_rev`) совпал: текста нет.
    Unchanged,
    /// Текст выбранного уровня; `whole` — полный текст L2 без секции (или изменения с
    /// `since_rev`: дифф и полный текст тоже дают агенту всю текущую ревизию).
    Text {
        text: String,
        whole: bool,
        form: Form,
    },
}

/// Вид текста элемента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// Текст уровня (`text`).
    Plain,
    /// Дифф с `since_rev` (`diff`).
    Diff { since: i64 },
    /// Дифф длиннее текста или ревизии `since_rev` нет: полный текст (`content`,
    /// `diff_skipped: true`).
    Full,
}

impl Content {
    fn plain(text: String, whole: bool) -> Content {
        Content::Text {
            text,
            whole,
            form: Form::Plain,
        }
    }
}

/// Прочитанный элемент (до упаковки в бюджет).
struct Fetched {
    /// `id` как в запросе.
    raw: String,
    /// Каноническая ссылка `n:<id>` | `i:|c:|r:|t:|k:|q:<id>`.
    reference: String,
    /// `node` или вид записи (`insight`, `changelog`, `release`, `term`, `task`, `question`).
    kind: &'static str,
    /// Начало заголовка: ссылка и путь узла либо вид и заголовок записи.
    head: String,
    space: String,
    path: Option<String>,
    title: Option<String>,
    status: Option<String>,
    certainty: Option<String>,
    section: Option<String>,
    tier: Tier,
    rev: i64,
    max_tokens: Option<usize>,
    /// Ключи выданного сессии (у вложения — `locator` каждой выданной единицы).
    seen: Vec<SeenKey>,
    content: Content,
    /// Токены текста, не прочитанные из базы (за пределом бюджета, M-4): входят в размер
    /// элемента, текст считается обрезанным.
    omitted: usize,
    /// Пути устаревших ссылок цели на файлы (M4.5 §6.2); пусто — поля в ответе нет.
    stale_refs: Vec<String>,
    /// Цель, чьи устаревшие ссылки нужны: база и ссылка (собираются одним запросом на базу).
    stale_key: Option<(Id, zam_store::Db, TargetRef)>,
    /// Вложение: файл, документ, блоб, `locator` (M5).
    att: Option<AttMeta>,
    /// Предупреждения элемента (`b:` с несколькими вложениями).
    warnings: Vec<String>,
}

/// Поля элемента-вложения.
struct AttMeta {
    file: String,
    attachment_id: Id,
    node_id: Id,
    blob_sha256: String,
    /// `locator` выданной части (имя листа — через сканер секретов); `None` — оглавление.
    locator: Option<String>,
    /// Подпись `locator` (`стр. 12`, `лист «Итоги» A1:F40`), тоже замаскирована.
    locator_text: Option<String>,
}

/// Результат по элементу после упаковки.
enum Piece {
    Err {
        raw: String,
        fail: Fail,
    },
    Unchanged(Fetched),
    /// Не влез в бюджет: заголовок и `(budget)`.
    Budget {
        f: Fetched,
        full: usize,
    },
    Text {
        f: Fetched,
        text: String,
        full: usize,
        /// Токенов не выдано (обрезка по `max_tokens` или бюджету).
        more: usize,
        /// Отдан полный текст L2 без секции — цель выдана целиком.
        given_whole: bool,
        form: Form,
    },
}

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::L0 => "L0",
        Tier::L1 => "L1",
        Tier::L2 => "L2",
    }
}

/// Узел, запись или документ вложения, найденные по `id` элемента.
enum Located {
    Node(NodeScope),
    Entry(EntryScope),
    /// Документ вложения и каноническая ссылка (`a:<chunk>`, `b:<sha256>`, `d:<id>`).
    Attachment(Box<AttachmentScope>, String),
}

fn miss() -> Failure {
    Failure::Item(Fail::new("not_found", None))
}

/// Буква ссылки на блоб вложения `b:<sha256>`.
const BLOB_REF_LETTER: &str = "b";
/// Буква ссылки на документ вложения `d:<id>` (внутренняя форма `TargetKind::Attachment`).
const DOC_REF_LETTER: &str = "d";

/// Ссылка на вложение: `a:` (фрагмент), `b:` (блоб), `d:` (документ).
fn is_doc_letter(k: &str) -> bool {
    k == CHUNK_REF_LETTER || k == BLOB_REF_LETTER || k == DOC_REF_LETTER
}

/// Префикс ссылки элемента: `n` (узел), буква записи (`i|c|r|t|k|q|e`) или вложения (`a|b|d`) и
/// остаток; без известного префикса — `(None, raw)`.
fn split_ref(raw: &str) -> (Option<&str>, &str) {
    match raw.split_once(':') {
        Some((k, rest)) if k == "n" || zam_store::is_entry_ref_letter(k) || is_doc_letter(k) => {
            (Some(k), rest)
        }
        _ => (None, raw),
    }
}

/// `id` элемента — ссылка на вложение (`locator` допустим только у неё).
fn is_doc_ref(raw: &str) -> bool {
    matches!(split_ref(raw.trim()), (Some(k), _) if is_doc_letter(k))
}

/// Строка из `n` hex-цифр (регистр не важен) → нижний регистр.
fn hex_lower(s: &str, n: usize) -> Option<String> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// Документ вложения по ссылке `letter:rest`: формат ссылки неверен, документа нет или он
/// недоступен — одинаково `not_found` (без пояснений: ответ не должен быть оракулом).
async fn resolve_doc(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    letter: &str,
    rest: &str,
) -> Result<Located, Failure> {
    let lookup = match letter {
        CHUNK_REF_LETTER => hex_lower(rest, 32).map(DocLookup::Chunk),
        BLOB_REF_LETTER => hex_lower(rest, 64).map(DocLookup::Blob),
        _ => rest.parse::<Id>().ok().map(DocLookup::Doc),
    }
    .ok_or_else(miss)?;
    let reference = match &lookup {
        DocLookup::Chunk(c) => zam_store::chunk_ref(c),
        DocLookup::Blob(s) => format!("{BLOB_REF_LETTER}:{s}"),
        DocLookup::Doc(id) => format!("{DOC_REF_LETTER}:{id}"),
    };
    match attachment_scope(st, p, headers, lookup).await {
        Ok(sc) => Ok(Located::Attachment(Box::new(sc), reference)),
        // неотличимо от несуществующего
        Err(ApiError::NotFound | ApiError::Forbidden | ApiError::SolutionAccess { .. }) => {
            Err(miss())
        }
        Err(e) => Err(e.into()),
    }
}

/// Находит узел или запись по `id` элемента с проверкой доступа (Human по токену и чужая база
/// вне решения — `not_found`).
async fn resolve(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    raw: &str,
) -> Result<Located, Failure> {
    let raw = raw.trim();
    if let Some((space, path)) = raw.split_once(":/")
        && !space.is_empty()
    {
        let path = NodePath::parse(path).map_err(ApiError::from)?;
        let sc = space_scope(st, p, headers, space, Mode::Read).await?;
        let node = get_node_by_path(&sc.db, sc.space.id, &path).await?;
        return Ok(Located::Node(NodeScope {
            user: sc.user,
            owner: sc.owner,
            db: sc.db,
            space: sc.space,
            node,
            role: sc.role,
        }));
    }
    let (prefix, rest) = split_ref(raw);
    if let Some(k) = prefix.filter(|k| is_doc_letter(k)) {
        return resolve_doc(st, p, headers, k, rest).await;
    }
    rest.parse::<Id>().map_err(|_| miss())?;
    match prefix {
        Some("n") => Ok(Located::Node(
            node_scope(st, p, headers, rest, Mode::Read).await?,
        )),
        Some(_) => Ok(Located::Entry(
            entry_scope(st, p, headers, rest, Mode::Read).await?,
        )),
        None => match node_scope(st, p, headers, rest, Mode::Read).await {
            Ok(sc) => Ok(Located::Node(sc)),
            Err(ApiError::NotFound) => Ok(Located::Entry(
                entry_scope(st, p, headers, rest, Mode::Read).await?,
            )),
            Err(e) => Err(e.into()),
        },
    }
}

/// Секция `section`: нет такой — `not_found` с пояснением.
fn slice<'a>(text: &'a str, section: &str) -> Result<&'a str, Failure> {
    section_slice(text, section).map_err(|e| match e {
        ApiError::NotFound => {
            Failure::Item(Fail::new("not_found", Some("секция не найдена".into())))
        }
        other => other.into(),
    })
}

async fn fetch_node(sc: NodeScope, it: &ReadItem, tier: Tier) -> Result<Fetched, Failure> {
    live(&sc)?;
    let node = &sc.node;
    let section = (tier != Tier::L0)
        .then(|| {
            it.section
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .flatten();
    let content = if it.if_rev == Some(node.current_rev) || it.since_rev == Some(node.current_rev) {
        Content::Unchanged
    } else if tier == Tier::L0 {
        Content::plain(node.l0.clone().unwrap_or_default(), false)
    } else if node.kind == NodeKind::Folder {
        return Err(ApiError::BadRequest("узел — папка: текста нет".into()).into());
    } else if let Some(since) = it.since_rev {
        check_since(since, node.current_rev)?;
        let (_, new) = node_content(&sc.db, node.id, Some(node.current_rev)).await?;
        let old = match node_content(&sc.db, node.id, Some(since)).await {
            Ok((_, t)) => Some(t),
            // ревизии `since` нет (снята сроком хранения) — полный текст
            Err(StoreError::NotFound) => None,
            Err(e) => return Err(e.into()),
        };
        changes(old, new, since, node.current_rev).await?
    } else {
        // ровно та ревизия, что в снимке узла
        let (_, text) = node_content(&sc.db, node.id, Some(node.current_rev)).await?;
        let (text, whole) = match section {
            Some(s) => (slice(&text, s)?.to_string(), false),
            None => (text, true),
        };
        match tier {
            Tier::L1 => Content::plain(l1(&text), false),
            _ => Content::plain(text, whole),
        }
    };
    let reference = format!("n:{}", node.id);
    let target = TargetRef {
        kind: TargetKind::Node,
        id: node.id,
    };
    Ok(Fetched {
        raw: it.id.clone(),
        head: format!("{reference} {}/{}", sc.space.name, node.path),
        reference,
        kind: "node",
        space: sc.space.name.clone(),
        path: Some(node.path.clone()),
        title: None,
        status: None,
        certainty: node.certainty.map(|c| c.as_str().to_string()),
        section: section.map(str::to_string),
        tier,
        rev: node.current_rev,
        max_tokens: it.max_tokens,
        seen: vec![SeenKey {
            owner_id: sc.owner,
            target: target.into(),
            part: WHOLE_TARGET.to_string(),
        }],
        content,
        omitted: 0,
        stale_refs: Vec::new(),
        stale_key: Some((sc.owner, sc.db.clone(), target)),
        att: None,
        warnings: Vec::new(),
    })
}

/// Сводка записи (L0): у термина — определение (`data.definition`, M4.5 §2.1), у прочих —
/// заголовок.
fn summary_of(e: &zam_store::Entry) -> String {
    if e.kind == EntryKind::Term
        && let Some(d) = e
            .data
            .get("definition")
            .and_then(Value::as_str)
            .filter(|d| !d.trim().is_empty())
    {
        return d.to_string();
    }
    e.title.clone()
}

async fn fetch_entry(sc: EntryScope, it: &ReadItem, tier: Tier) -> Result<Fetched, Failure> {
    live_entry(&sc)?;
    let e = &sc.entry;
    let section = (tier != Tier::L0)
        .then(|| {
            it.section
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .flatten();
    let content = if it.if_rev == Some(e.rev) || it.since_rev == Some(e.rev) {
        Content::Unchanged
    } else if tier == Tier::L0 {
        Content::plain(summary_of(e), false)
    } else if let Some(since) = it.since_rev {
        check_since(since, e.rev)?;
        let revs = entry_revisions(&sc.db, e.id).await?;
        let snap = |rev: i64| {
            revs.iter()
                .find(|r| r.rev == rev)
                .map(|r| entry_snapshot_text(&r.title, &r.body, &r.data))
        };
        // снимка текущей ревизии нет (запись до M3) — по самой записи
        let new = snap(e.rev).unwrap_or_else(|| entry_snapshot_text(&e.title, &e.body, &e.data));
        changes(snap(since), new, since, e.rev).await?
    } else {
        let (text, whole) = match section {
            Some(s) => (slice(&e.body, s)?.to_string(), false),
            None => (e.body.clone(), true),
        };
        match tier {
            Tier::L1 => {
                let t = l1(&text);
                let t = if t.trim().is_empty() {
                    summary_of(e)
                } else {
                    t
                };
                Content::plain(t, false)
            }
            _ => Content::plain(text, whole),
        }
    };
    let (letter, kind) = (e.kind.ref_letter(), e.kind.as_str());
    let reference = format!("{letter}:{}", e.id);
    let target = TargetRef {
        kind: TargetKind::Entry,
        id: e.id,
    };
    let sub = match e.kind {
        EntryKind::Release => None,
        _ => e.data.get("kind").and_then(Value::as_str),
    };
    let label = match sub {
        Some(k) => format!("{kind}/{}", md_inline(k)),
        None => kind.to_string(),
    };
    Ok(Fetched {
        raw: it.id.clone(),
        head: format!("{reference} {label} «{}»", md_inline(&e.title)),
        reference,
        kind,
        space: sc.space.name.clone(),
        path: None,
        title: Some(e.title.clone()),
        status: e.status.map(|s| s.as_str().to_string()),
        certainty: e.certainty.map(|c| c.as_str().to_string()),
        section: section.map(str::to_string),
        tier,
        rev: e.rev,
        max_tokens: it.max_tokens,
        seen: vec![SeenKey {
            owner_id: sc.owner,
            target: target.into(),
            part: WHOLE_TARGET.to_string(),
        }],
        content,
        omitted: 0,
        stale_refs: Vec::new(),
        stale_key: Some((sc.owner, sc.db.clone(), target)),
        att: None,
        warnings: Vec::new(),
    })
}

/// `since_rev` больше текущей ревизии — 400 запроса (а не ошибка элемента): клиент прислал
/// ревизию, которой не было.
fn check_since(since: i64, current: i64) -> Result<(), Failure> {
    if since > current {
        return Err(Failure::Fatal(ApiError::BadRequest(format!(
            "since_rev: {since} больше текущей ревизии {current}"
        ))));
    }
    Ok(())
}

/// Изменения с ревизии `since` до `current`: дифф или (дифф длиннее текста, старой ревизии
/// нет) полный текст.
async fn changes(
    old: Option<String>,
    new: String,
    since: i64,
    current: i64,
) -> Result<Content, Failure> {
    let Some(old) = old else {
        return Ok(Content::Text {
            text: new,
            whole: true,
            form: Form::Full,
        });
    };
    let diff = diff_blocking(
        old,
        new.clone(),
        format!("rev {since}"),
        format!("rev {current}"),
    )
    .await?;
    Ok(if count_tokens(&diff) > count_tokens(&new) {
        Content::Text {
            text: new,
            whole: true,
            form: Form::Full,
        }
    } else {
        Content::Text {
            text: diff,
            whole: true,
            form: Form::Diff { since },
        }
    })
}

/// Пути устаревших ссылок на файлы прочитанных целей — одним запросом на базу (а не на элемент:
/// отложенное M-7, до 20 запросов на горячем пути MCP).
async fn fill_stale_refs(fetched: &mut [Result<Fetched, (String, Fail)>]) -> Result<(), ApiError> {
    let mut by_db: HashMap<Id, (zam_store::Db, Vec<TargetRef>)> = HashMap::new();
    for f in fetched.iter().flatten() {
        if let Some((owner, db, r)) = &f.stale_key {
            by_db
                .entry(*owner)
                .or_insert_with(|| (db.clone(), Vec::new()))
                .1
                .push(*r);
        }
    }
    let mut stale: HashMap<(Id, TargetRef), Vec<String>> = HashMap::new();
    for (owner, (db, refs)) in by_db {
        for (r, paths) in zam_store::stale_refs_of(&db, &refs).await? {
            stale.insert((owner, r), paths);
        }
    }
    for f in fetched.iter_mut().flatten() {
        if let Some((owner, _, r)) = f.stale_key.take() {
            // цель может повторяться в пачке (две секции одной заметки) — путь у каждого элемента
            f.stale_refs = stale.get(&(owner, r)).cloned().unwrap_or_default();
        }
    }
    Ok(())
}

/// Предел частей в оглавлении вложения: документ целиком без `locator` не выдаётся.
pub const TOC_MAX_ENTRIES: usize = 100;
/// Предел первой строки части в оглавлении, символов.
const TOC_LINE_CHARS: usize = 120;
/// Ревизия выданного вложения (у документа ревизий нет, как у его чанков).
const DOC_REV: i64 = 0;

/// `locator` элемента: нет (`null`) — `None`; строка неверного формата или не строка — 422
/// `locator-invalid`.
fn parse_locator(v: Option<&Value>) -> Result<Option<Locator>, ApiError> {
    const FORMS: &str = "page:N | slide:N | para:N | sheet:<лист>!A1:F40";
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Locator::parse(s.trim()).map(Some).map_err(|_| {
            ApiError::LocatorInvalid(format!("locator: неверный формат, нужно {FORMS}"))
        }),
        Some(_) => Err(ApiError::LocatorInvalid(format!(
            "locator — строка: {FORMS}"
        ))),
    }
}

/// Чанки документа, относящиеся к `want` (сырой адрес, [`Sheets::resolve`]): пересечение
/// адресов ([`Locator::overlaps`]) — по сырым `locator` чанков, не по маске; абзац внутри блока
/// DOCX (номера нет среди начал блоков) — блок с ближайшим меньшим началом.
fn select(heads: &[zam_store::DocChunkHead], want: &Locator) -> Vec<i64> {
    let parsed: Vec<(i64, Option<Locator>)> = heads
        .iter()
        .map(|h| {
            let l = h.locator.as_deref().and_then(|r| Locator::parse(r).ok());
            (h.ordinal, l)
        })
        .collect();
    let hit: Vec<i64> = parsed
        .iter()
        .filter(|(_, l)| l.as_ref().is_some_and(|l| l.overlaps(want)))
        .map(|(o, _)| *o)
        .collect();
    if !hit.is_empty() {
        return hit;
    }
    let Locator::Para(n) = want else {
        return hit;
    };
    let start = parsed
        .iter()
        .filter_map(|(_, l)| match l {
            Some(Locator::Para(s)) if s <= n => Some(*s),
            _ => None,
        })
        .max();
    let Some(start) = start else {
        return hit;
    };
    let block = Some(Locator::Para(start));
    parsed
        .iter()
        .filter(|(_, l)| *l == block)
        .map(|(o, _)| *o)
        .collect()
}

/// Текст выбранных чанков: подряд, путь заголовков — строкой `[путь]` при смене.
fn join_chunks(heads: &[zam_store::DocChunkHead], texts: &[(i64, String)]) -> String {
    let heading: HashMap<i64, &str> = heads
        .iter()
        .map(|h| (h.ordinal, h.heading_path.as_str()))
        .collect();
    let mut out = String::new();
    let mut last: Option<&str> = None;
    for (o, t) in texts {
        let h = heading.get(o).copied().unwrap_or("");
        if last != Some(h) {
            if !out.is_empty() {
                out.push('\n');
            }
            if !h.is_empty() {
                out.push_str(&format!("[{h}]\n"));
            }
        }
        out.push_str(t.trim());
        out.push('\n');
        last = Some(h);
    }
    out
}

/// Первая непустая строка, не длиннее [`TOC_LINE_CHARS`].
fn first_line(t: &str) -> String {
    let line = t
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= TOC_LINE_CHARS {
        return line.to_string();
    }
    let mut s: String = line.chars().take(TOC_LINE_CHARS).collect();
    s.push('…');
    s
}

/// Оглавление документа: части по порядку (страница, слайд, абзац-начало блока; лист — одной
/// строкой с объединённым диапазоном блоков, с секретом в имени — по номеру), первая строка части,
/// подсказка `locator`. Не больше [`TOC_MAX_ENTRIES`] частей.
async fn toc(
    db: &zam_store::Db,
    att: Id,
    heads: &[zam_store::DocChunkHead],
    sheets: &Sheets,
) -> Result<String, Failure> {
    // (адрес, первый чанк, путь заголовков)
    let mut parts: Vec<(Locator, i64, &str)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for h in heads {
        let Some(l) = h.locator.as_deref().and_then(|r| Locator::parse(r).ok()) else {
            continue;
        };
        if let Locator::Sheet { name, .. } = &l {
            if let Some(last) = parts.last_mut()
                && let Some(u) = last.0.sheet_union(&l)
            {
                last.0 = u;
                continue;
            }
            if !seen.insert(format!("sheet:{name}")) {
                continue;
            }
        } else if !seen.insert(l.render()) {
            continue;
        }
        parts.push((l, h.ordinal, h.heading_path.as_str()));
    }
    let shown = parts.len().min(TOC_MAX_ENTRIES);
    let ordinals = parts[..shown].iter().map(|p| p.1).collect();
    let firsts: HashMap<i64, String> = doc_chunk_texts(db, att, ordinals, Some(TOC_LINE_CHARS * 4))
        .await?
        .into_iter()
        .collect();
    let mut out = format!(
        "Оглавление: {} частей (locator — первая строка)\n",
        parts.len()
    );
    for (l, o, heading) in &parts[..shown] {
        let line = firsts.get(o).map(|t| first_line(t)).unwrap_or_default();
        let head = if heading.is_empty() {
            String::new()
        } else {
            format!("[{}] ", first_line(heading))
        };
        out.push_str(&format!("- {} — {head}{line}\n", sheets.shown(l).0));
    }
    if parts.len() > shown {
        out.push_str(&format!("…ещё {} частей\n", parts.len() - shown));
    }
    if let Some((l, _, _)) = parts.first() {
        out.push_str(&format!(
            "Часть документа: тот же id и locator из списка (например, \"{}\").\n",
            sheets.shown(l).0
        ));
    }
    Ok(out)
}

/// Начало выбранных чанков, которое читается из базы: накопленный `token_count` до `limit`
/// (первый чанк — всегда, чанк, переходящий предел, — тоже: упаковка обрежет его по токенам) и
/// сколько токенов не прочитано (раунд правок 1 задачи 7 M5, M-4: лист на 60 000 строк не
/// поднимается в память целиком ради бюджета в 1500 токенов).
fn within_budget(
    heads: &[zam_store::DocChunkHead],
    ordinals: &[i64],
    limit: usize,
) -> (Vec<i64>, usize) {
    let tokens: HashMap<i64, usize> = heads
        .iter()
        .map(|h| (h.ordinal, usize::try_from(h.token_count).unwrap_or(0)))
        .collect();
    let mut taken = Vec::new();
    let (mut sum, mut omitted) = (0usize, 0usize);
    for o in ordinals {
        let t = tokens.get(o).copied().unwrap_or(0);
        if taken.is_empty() || sum < limit {
            taken.push(*o);
            sum += t;
        } else {
            omitted += t;
        }
    }
    (taken, omitted)
}

/// Вложение: текст по `locator` (явному или фрагмента `a:`) либо оглавление; текст читается не
/// дальше `limit` токенов ([`within_budget`]).
async fn fetch_attachment(
    sc: AttachmentScope,
    reference: String,
    it: &ReadItem,
    tier: Tier,
    locator: Option<Locator>,
    limit: usize,
) -> Result<Fetched, Failure> {
    let section = it.section.as_deref().is_some_and(|s| !s.trim().is_empty());
    if section || it.since_rev.is_some() {
        return Err(Failure::Item(Fail::new(
            "invalid",
            Some("section и since_rev к вложению неприменимы: часть документа — locator".into()),
        )));
    }
    let doc = &sc.doc;
    // фрагмент `a:` без locator — его часть; L0/L1 — оглавление
    let want = locator.or_else(|| {
        (tier == Tier::L2)
            .then(|| {
                doc.chunk_locator
                    .as_deref()
                    .and_then(|r| Locator::parse(r).ok())
            })
            .flatten()
    });
    let mut parts: Vec<String> = Vec::new();
    let mut omitted = 0usize;
    let mut shown: Option<(String, String)> = None;
    let content = if it.if_rev == Some(DOC_REV) {
        Content::Unchanged
    } else {
        let heads = doc_chunk_heads(&sc.db, doc.id).await?;
        if heads.is_empty() {
            return Err(Failure::Item(Fail::new(
                "not_found",
                Some(format!(
                    "текст документа не извлечён ({})",
                    doc.extract_state.as_str()
                )),
            )));
        }
        let sheets = Sheets::of(heads.iter().filter_map(|h| h.locator.as_deref()));
        match &want {
            None => Content::plain(toc(&sc.db, doc.id, &heads, &sheets).await?, false),
            Some(w) => {
                let raw = sheets.resolve(w);
                let ordinals = raw.as_ref().map(|r| select(&heads, r)).unwrap_or_default();
                let show = match &raw {
                    Some(r) => sheets.shown(r),
                    None => (mask_text(&w.render()), mask_text(&w.label())),
                };
                if ordinals.is_empty() {
                    return Err(Failure::Item(Fail::new(
                        "not_found",
                        Some(format!(
                            "в документе нет части {}: оглавление — без locator",
                            show.0
                        )),
                    )));
                }
                shown = Some(show);
                let (ordinals, rest) = within_budget(&heads, &ordinals, limit);
                omitted = rest;
                // выдаются единицы целиком: в сессии — `locator` каждой (как у карточек поиска)
                let chosen: HashSet<i64> = ordinals.iter().copied().collect();
                let mut uniq: HashSet<&str> = HashSet::new();
                for h in heads.iter().filter(|h| chosen.contains(&h.ordinal)) {
                    if let Some(l) = &h.locator
                        && uniq.insert(l)
                    {
                        parts.push(l.clone());
                    }
                }
                let texts = doc_chunk_texts(&sc.db, doc.id, ordinals, None).await?;
                Content::plain(join_chunks(&heads, &texts), true)
            }
        }
    };
    // `unchanged` (текста нет) — адрес как в запросе
    let shown = shown.or_else(|| {
        want.as_ref()
            .map(|w| (mask_text(&w.render()), mask_text(&w.label())))
    });
    let mut warnings = Vec::new();
    if sc.others > 0 {
        warnings.push(format!(
            "блоб привязан ещё в {} местах ({} всего): выбрано первое по дате — {}/{}",
            sc.others,
            sc.others + 1,
            sc.space.name,
            doc.path
        ));
    }
    Ok(Fetched {
        raw: it.id.clone(),
        head: format!(
            "{reference} {}/{} › {}",
            sc.space.name,
            doc.path,
            md_inline(&doc.filename)
        ),
        reference,
        kind: "attachment",
        space: sc.space.name.clone(),
        path: Some(doc.path.clone()),
        title: None,
        status: None,
        certainty: None,
        section: None,
        tier,
        rev: DOC_REV,
        max_tokens: it.max_tokens,
        seen: parts
            .into_iter()
            .map(|part| SeenKey {
                owner_id: sc.owner,
                target: SeenTarget::Blob(doc.blob_sha256.clone()),
                part,
            })
            .collect(),
        content,
        omitted,
        stale_refs: Vec::new(),
        stale_key: None,
        att: Some(AttMeta {
            file: doc.filename.clone(),
            attachment_id: doc.id,
            node_id: doc.node_id,
            blob_sha256: doc.blob_sha256.clone(),
            locator: shown.as_ref().map(|s| s.0.clone()),
            locator_text: shown.map(|s| s.1),
        }),
        warnings,
    })
}

async fn fetch(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    it: &ReadItem,
    tier: Tier,
    locator: Option<Locator>,
    budget: usize,
) -> Result<Fetched, Failure> {
    match resolve(st, p, headers, &it.id).await? {
        Located::Node(sc) => fetch_node(sc, it, tier).await,
        Located::Entry(sc) => fetch_entry(sc, it, tier).await,
        Located::Attachment(sc, reference) => {
            // больше бюджета пачки (и `max_tokens` элемента) выдано не будет — дальше не читается
            let limit = it.max_tokens.unwrap_or(budget).min(budget);
            fetch_attachment(*sc, reference, it, tier, locator, limit).await
        }
    }
}

/// Заголовок элемента md: `### <начало>[ › секция]  [статус · ]rev<N> · <хвост>`.
fn header(f: &Fetched, tail: &str) -> String {
    let mut line = format!("### {}", f.head);
    if let Some(s) = &f.section {
        line.push_str(" › ");
        line.push_str(s);
    }
    // вложение: подпись части или «оглавление»; ревизий у документа нет
    if let Some(a) = &f.att {
        line.push_str(" · ");
        line.push_str(a.locator_text.as_deref().unwrap_or("оглавление"));
    }
    let mut parts: Vec<String> = [&f.status, &f.certainty]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    if f.att.is_none() {
        parts.push(format!("rev{}", f.rev));
    }
    parts.push(tail.to_string());
    line.push_str(&format!("  {}", parts.join(" · ")));
    line
}

fn err_line(raw: &str, fail: &Fail) -> String {
    format!("### {}  error: {}", md_inline(raw), fail.code)
}

/// Заголовок ответа: `error` первого элемента с ошибкой (нет заголовка — ошибок нет).
pub const ERROR_HEADER: &str = "x-zam-read-error";

/// Строка вместо текста элемента, не влезшего в бюджет.
const BUDGET_LINE: &str = "(budget)";

fn marker(more: usize) -> String {
    format!("…ещё {more} токенов")
}

/// Минимальная цена элемента в ответе: заголовок (и `(budget)` у элемента с текстом).
fn floor_cost(r: &Result<Fetched, (String, Fail)>) -> usize {
    match r {
        Err((raw, fail)) => count_tokens(&err_line(raw, fail)),
        Ok(f) => match &f.content {
            Content::Unchanged => count_tokens(&header(f, "unchanged")),
            Content::Text { text, .. } => {
                let full = count_tokens(text) + f.omitted;
                count_tokens(&header(f, &format!("~{full} tok"))) + count_tokens(BUDGET_LINE)
            }
        },
    }
}

/// Упаковка прочитанных элементов в бюджет; возвращает куски и оценку выданных токенов.
///
/// Каждому следующему элементу заранее оставлено место под его заголовок (и `(budget)`), поэтому
/// ответ укладывается в бюджет, если бюджета хватает на заголовки; первый элемент обрезается, но
/// отдаётся всегда.
fn pack(fetched: Vec<Result<Fetched, (String, Fail)>>, budget: usize) -> (Vec<Piece>, usize) {
    // suffix[i] — минимальная цена элементов после i-го
    let floors: Vec<usize> = fetched.iter().map(floor_cost).collect();
    let mut suffix = vec![0usize; floors.len() + 1];
    for i in (0..floors.len()).rev() {
        suffix[i] = suffix[i + 1] + floors[i];
    }
    let mut exhausted = false;
    let mut total = 0usize;
    let mut out = Vec::with_capacity(fetched.len());
    for (i, r) in fetched.into_iter().enumerate() {
        let f = match r {
            Err((raw, fail)) => {
                total += count_tokens(&err_line(&raw, &fail));
                out.push(Piece::Err { raw, fail });
                continue;
            }
            Ok(f) => f,
        };
        let (text, form) = match &f.content {
            Content::Unchanged => {
                total += count_tokens(&header(&f, "unchanged"));
                out.push(Piece::Unchanged(f));
                continue;
            }
            Content::Text { text, form, .. } => (text.clone(), *form),
        };
        let whole = matches!(f.content, Content::Text { whole: true, .. });
        let full = count_tokens(&text) + f.omitted;
        let hc = count_tokens(&header(&f, &format!("~{full} tok")));
        if exhausted {
            total += hc + count_tokens(BUDGET_LINE);
            out.push(Piece::Budget { f, full });
            continue;
        }
        let mut kept: &str = &text;
        // часть текста не прочитана (бюджет) — элемент уже обрезан
        let mut cut = f.omitted > 0;
        if let Some(m) = f.max_tokens.map(zam_store::budget_cap)
            && full > m
        {
            kept = truncate_to_tokens(kept, m, &count_tokens);
            cut = true;
        }
        let have = if cut { count_tokens(kept) } else { full };
        let allowed = budget
            .saturating_sub(total + suffix[i + 1])
            .saturating_sub(hc);
        if have > allowed {
            let room = allowed.saturating_sub(count_tokens(&marker(full)));
            kept = truncate_to_tokens(kept, room, &count_tokens);
            cut = true;
            exhausted = true;
        }
        let delivered = if cut { count_tokens(kept) } else { full };
        let more = full.saturating_sub(delivered);
        total += hc + delivered;
        if cut {
            total += count_tokens(&marker(more));
        }
        out.push(Piece::Text {
            text: kept.to_string(),
            full,
            more,
            given_whole: whole && !cut,
            form,
            f,
        });
    }
    (out, total)
}

fn item_json(piece: &Piece) -> Value {
    let mut o = Map::new();
    let base = |f: &Fetched, o: &mut Map<String, Value>| {
        o.insert("id".into(), json!(f.raw));
        o.insert("ref".into(), json!(f.reference));
        o.insert("kind".into(), json!(f.kind));
        o.insert("space".into(), json!(f.space));
        if let Some(p) = &f.path {
            o.insert("path".into(), json!(p));
        }
        if let Some(t) = &f.title {
            o.insert("title".into(), json!(t));
        }
        if let Some(s) = &f.status {
            o.insert("status".into(), json!(s));
        }
        if let Some(c) = &f.certainty {
            o.insert("certainty".into(), json!(c));
        }
        if let Some(s) = &f.section {
            o.insert("section".into(), json!(s));
        }
        o.insert("tier".into(), json!(tier_str(f.tier)));
        o.insert("rev".into(), json!(f.rev));
        if !f.stale_refs.is_empty() {
            o.insert("stale_refs".into(), json!(f.stale_refs));
        }
        if let Some(a) = &f.att {
            o.insert("file".into(), json!(a.file));
            o.insert("attachment_id".into(), json!(a.attachment_id));
            o.insert("node_id".into(), json!(a.node_id));
            o.insert("blob_sha256".into(), json!(a.blob_sha256));
            match (&a.locator, &a.locator_text) {
                (Some(l), Some(t)) => {
                    o.insert("locator".into(), json!(l));
                    o.insert("locator_text".into(), json!(t));
                }
                _ => {
                    o.insert("toc".into(), json!(true));
                }
            }
        }
        if !f.warnings.is_empty() {
            o.insert("warnings".into(), json!(f.warnings));
        }
    };
    match piece {
        Piece::Err { raw, fail } => {
            o.insert("id".into(), json!(raw));
            o.insert("error".into(), json!(fail.code));
            if let Some(d) = &fail.detail {
                o.insert("detail".into(), json!(d));
            }
            for (k, v) in &fail.extra {
                o.insert((*k).into(), v.clone());
            }
        }
        Piece::Unchanged(f) => {
            base(f, &mut o);
            o.insert("unchanged".into(), json!(true));
        }
        Piece::Budget { f, full } => {
            base(f, &mut o);
            o.insert("tokens".into(), json!(full));
            o.insert("budget".into(), json!(true));
        }
        Piece::Text {
            f,
            text,
            full,
            more,
            form,
            ..
        } => {
            base(f, &mut o);
            o.insert("tokens".into(), json!(full));
            match form {
                Form::Plain => {
                    o.insert("text".into(), json!(text));
                }
                Form::Diff { since } => {
                    o.insert("since_rev".into(), json!(since));
                    o.insert("diff".into(), json!(text));
                }
                Form::Full => {
                    o.insert("content".into(), json!(text));
                    o.insert("diff_skipped".into(), json!(true));
                }
            }
            if *more > 0 {
                o.insert("truncated".into(), json!(true));
                o.insert("more_tokens".into(), json!(more));
            }
        }
    }
    Value::Object(o)
}

fn push_block(out: &mut String, line: &str) {
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
}

/// Строка `(возможно устарело: изменился <путь>)` под заголовком элемента (M4.5 §6.2) и
/// предупреждения элемента (`(warning: …)`).
fn push_stale(out: &mut String, f: &Fetched) {
    if let Some(s) = super::search::stale_line(&f.stale_refs) {
        push_block(out, &format!("({s})"));
    }
    for w in &f.warnings {
        push_block(out, &format!("(warning: {})", md_inline(w)));
    }
}

fn render_md(pieces: &[Piece], total: usize) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Err { raw, fail } => {
                push_block(&mut out, &err_line(raw, fail));
                if let Some(d) = &fail.detail {
                    push_block(&mut out, &format!("({})", md_inline(d)));
                }
            }
            Piece::Unchanged(f) => {
                push_block(&mut out, &header(f, "unchanged"));
                push_stale(&mut out, f);
            }
            Piece::Budget { f, full } => {
                push_block(&mut out, &header(f, &format!("~{full} tok")));
                push_stale(&mut out, f);
                push_block(&mut out, BUDGET_LINE);
            }
            Piece::Text {
                f,
                text,
                full,
                more,
                form,
                ..
            } => {
                push_block(&mut out, &header(f, &format!("~{full} tok")));
                push_stale(&mut out, f);
                match form {
                    Form::Plain => {}
                    Form::Diff { since } => {
                        push_block(&mut out, &format!("(изменения с rev{since}, дифф)"));
                    }
                    Form::Full => push_block(&mut out, "(дифф длиннее текста — полный текст)"),
                }
                if !text.is_empty() {
                    push_block(&mut out, text);
                }
                if *more > 0 {
                    push_block(&mut out, &marker(*more));
                }
            }
        }
        out.push('\n');
    }
    out.push_str(&format!("~{total} tok\n"));
    out
}

/// `POST /read`.
pub async fn read(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    origin: Option<Extension<McpOrigin>>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let req: ReadRequest = json_body(body)?;
    let md = match req.format.as_deref().unwrap_or("json") {
        "json" => false,
        "md" => true,
        _ => return Err(ApiError::BadRequest("format: json или md".into())),
    };
    let max_items = st.cfg.read.max_items;
    if req.items.is_empty() || req.items.len() > max_items {
        return Err(ApiError::Unprocessable(format!(
            "items: от 1 до {max_items} элементов"
        )));
    }
    let budget = req.budget_tokens.unwrap_or(st.cfg.read.budget_tokens);
    if budget == 0 {
        return Err(ApiError::BadRequest("budget_tokens: больше 0".into()));
    }
    let mut tiers = Vec::with_capacity(req.items.len());
    let mut locators = Vec::with_capacity(req.items.len());
    for it in &req.items {
        let locator = parse_locator(it.locator.as_ref())?;
        if locator.is_some() && !is_doc_ref(&it.id) {
            return Err(ApiError::LocatorInvalid(
                "locator — только у вложений: a:<фрагмент>, b:<sha256>, d:<документ>".into(),
            ));
        }
        locators.push(locator);
        if it.max_tokens == Some(0) {
            return Err(ApiError::BadRequest("max_tokens: больше 0".into()));
        }
        let tier = parse_tier(it.tier.as_deref())?;
        if let Some(since) = it.since_rev {
            if since < 1 {
                return Err(ApiError::BadRequest("since_rev: от 1".into()));
            }
            let section = it.section.as_deref().is_some_and(|s| !s.trim().is_empty());
            if it.if_rev.is_some() || tier != Tier::L2 || section {
                return Err(ApiError::BadRequest(
                    "since_rev: только полный текст L2, без if_rev и section".into(),
                ));
            }
        }
        tiers.push(tier);
    }
    // сессия: некорректный id — 400; сбой её базы чтение не валит (WARN)
    let session = match request_session(
        &st,
        &p,
        &headers,
        origin.as_deref(),
        req.session_id.as_deref(),
    )
    .await
    {
        // неявная сессия MCP (M6 §7.1) — без отметок выданного
        Ok(s) => explicit_only(s),
        // некорректный id — 400, несуществующий пользователь (`X-Zam-User`) — 404
        Err(e @ (ApiError::BadRequest(_) | ApiError::NotFound)) => return Err(e),
        Err(e) => {
            tracing::warn!(error = %e, "чтение: база сессии недоступна, показ не записан");
            None
        }
    };

    let mut fetched = Vec::with_capacity(req.items.len());
    for ((it, tier), locator) in req.items.iter().zip(tiers).zip(locators) {
        fetched.push(
            match fetch(&st, &p, &headers, it, tier, locator, budget).await {
                Ok(f) => Ok(f),
                Err(Failure::Item(fail)) => Err((it.id.clone(), fail)),
                Err(Failure::Fatal(e)) => return Err(e),
            },
        );
    }
    fill_stale_refs(&mut fetched).await?;
    // упаковка суммами частей — против `budget_cap` (M6 §4.2: без модели — с запасом оценки)
    let (pieces, total) = pack(fetched, zam_store::budget_cap(budget));

    if let Some(s) = &session {
        let given: Vec<SeenItem> = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Text {
                    f,
                    given_whole: true,
                    ..
                } => Some(f.seen.iter().map(|k| SeenItem {
                    key: k.clone(),
                    rev: f.rev,
                })),
                _ => None,
            })
            .flatten()
            .collect();
        if let Err(e) = mark_seen(&s.db, &s.touch, &given).await {
            tracing::warn!(error = %e, "чтение: отметка выданного сессии не выполнена");
        }
    }

    // код `error` первого элемента с ошибкой — для клиентов, которым md не разобрать
    let first_error = pieces.iter().find_map(|p| match p {
        Piece::Err { fail, .. } => Some(fail.code),
        _ => None,
    });
    let mut r = if md {
        let mut r = render_md(&pieces, total).into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/markdown; charset=utf-8"),
        );
        r
    } else {
        axum::Json(json!({
            "items": pieces.iter().map(item_json).collect::<Vec<_>>(),
            "total_tokens": total,
        }))
        .into_response()
    };
    if let Some(code) = first_error {
        r.headers_mut()
            .insert(ERROR_HEADER, HeaderValue::from_static(code));
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Буквы ссылок записи в `id` элемента: `i|c|r|e` и с M4.5 `t` (term), `k` (task),
    /// `q` (question); неизвестный префикс — путь или голый id.
    #[test]
    fn entry_ref_letters() {
        for p in ["n", "i", "c", "r", "e", "t", "k", "q"] {
            let raw = format!("{p}:01ABC");
            assert_eq!(split_ref(&raw), (Some(p), "01ABC"), "{p}");
        }
        for p in ["a", "b", "d"] {
            let raw = format!("{p}:01ABC");
            assert_eq!(split_ref(&raw), (Some(p), "01ABC"), "{p}");
            assert!(is_doc_ref(&raw), "{p}");
        }
        assert!(!is_doc_ref("n:01ABC") && !is_doc_ref("knowledge:/a"));
        assert_eq!(split_ref("x:01ABC"), (None, "x:01ABC"));
        assert_eq!(split_ref("01ABC"), (None, "01ABC"));
        for k in EntryKind::ALL {
            assert!(zam_store::is_entry_ref_letter(k.ref_letter()));
            assert_eq!(EntryKind::from_ref_letter(k.ref_letter()), Some(k));
        }
        assert_eq!(EntryKind::Term.ref_letter(), "t");
        assert_eq!(EntryKind::Task.ref_letter(), "k");
        assert_eq!(EntryKind::Question.ref_letter(), "q");
    }
}
