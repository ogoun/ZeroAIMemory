//! Запись узлов: создание, замена (`If-Match`), PATCH секций, перенос, удаление в
//! корзину, восстановление и список корзины.
//!
//! Ответ записи заметки (создание, замена, PATCH) — `Node` и непустые `warnings`:
//! `certainty_not_recognized` — во frontmatter ключ `certainty`/`evidence` в нераспознаваемом
//! написании (в кавычках, с заглавными буквами): такой ключ статус не задаёт (§7.5);
//! `unknown relation type: <тип>` и `invalid relation: <строка>` — строка `relations:`, не
//! давшая ребра графа (§8.5); `term_alias: «<псевдоним>» → «<имя>»` — в имени или тексте
//! заметки псевдоним термина словаря вместо имени (M4.5 §2.2, до 5); `secrets_detected` (+
//! `secrets_detected: [{kind, line}]`) — в тексте найдены секреты, в индекс они уходят
//! замаскированными (§7.6, [`crate::api::secrets`]); `verified_at_invalid` — `verified_at:`
//! не дата `YYYY-MM-DD`; `verified_at_commit_invalid` — `verified_at_commit:` не sha коммита
//! (4–64 hex); `invalid_ref: <путь> (<причина>)` — путь `refs:` не стал ссылкой на файл;
//! `refs_truncated` — ссылок больше 64 (M4.5 §6.1, §7); `secret_mark_kept: N …` (+ поле
//! `secret_mark_kept: N`) — в правке агента (`PUT`, `PATCH`) метка без номера маски записана как
//! есть ([`crate::api::secret_restore`]).
//!
//! **Агент по токену** правит вид с метками (`‹secret-T-N›`, задача 6 M7.1, M7 §4.3.2): `PUT` —
//! значения масок ревизии `If-Match` по меткам этой заметки и ревизии (устаревшая без ключа повтора — 409 без
//! восстановления), `PATCH` — операции по виду ревизии `base_rev` к исходнику текущей
//! ([`patch_restored`]: адрес секции с меткой — заголовок исходника, метки содержимого — значения
//! масок `base_rev`), подсчёт вне транзакции писателя со сверкой ревизии при записи
//! ([`zam_store::patch_note_prepared`]). Правка, выводящая секрет из-под маски, — 422
//! `secret_context_changed`; `secrets_detected` — только новые находки. Сессия WebUI пишет
//! исходник как есть.
//!
//! `Idempotency-Key` (M6 §16 п. 19, CR-20; заголовок — `api::entries::idempotency_key`, как у
//! записей журнала): у создания заметки, `PUT` и `PATCH`. Повтор того же запроса — 200 и узел в
//! текущем виде без новой правки (`POST` — не 409 «путь занят», `PUT` — не 409 по старому
//! `If-Match`, `PATCH append` — одна вставка); тот же ключ с другим запросом — 422
//! `/problems/idempotency-mismatch`. Узел по ключу в корзине: повтор `POST` — 409, `PUT` и
//! `PATCH` — 404 (проверка `live` области узла раньше ключа; в гонке с удалением — 409 из
//! хранилища). Сверка и правка — одна транзакция писателя (`zam_store::create_note_keyed` и
//! соседние). У папки ключа нет — 400.
//!
//! Пределы: текст заметки при создании и замене и итог PATCH — не больше
//! `limits.note_max_bytes` (иначе 413, ничего не записано); тело запроса ограничено
//! слоем `DefaultBodyLimit` (см. `api::router`).
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use zam_markdown::PatchOp;
use zam_store::{
    NewNote, Node, NodePath, apply_ops_tracking, create_folder, create_note_keyed, delete_node,
    list_trash, move_node, node_content, patch_note_keyed, patch_note_prepared, patch_replayed,
    restore_node, update_note_keyed,
};

use super::{audit_node, write_error};
use crate::api::entries::idempotency_key;
use crate::api::scope::{Mode, live, node_scope, space_scope, user_db};
use crate::api::spaces::readable_space_ids;
use crate::api::{agent_view, secret_restore};
use crate::api::{json_body, query};
use crate::app::AppState;
use crate::attrib::attribution;
use crate::auth::{Client, Principal};
use crate::error::ApiError;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Note,
    Folder,
}

#[derive(Deserialize)]
pub struct Create {
    space: String,
    path: String,
    kind: Kind,
    content: Option<String>,
    l0: Option<String>,
    message: Option<String>,
}

/// Предупреждение: ключ `certainty`/`evidence` во frontmatter написан так, что не распознан.
pub const CERTAINTY_NOT_RECOGNIZED: &str = "certainty_not_recognized";

/// Предупреждение: `verified_at_commit:` во frontmatter — не sha коммита (4–64 hex).
pub const VERIFIED_AT_COMMIT_INVALID: &str = "verified_at_commit_invalid";

/// Предупреждения записи заметки по её тексту; `mask` — агенту: путь `invalid_ref` маскируется до
/// усечения (кусок секрета на границе усечения сканер уже не узнал бы, круг правок 4 задачи 6).
fn note_warnings(content: &str, mask: bool) -> Vec<String> {
    let doc = zam_markdown::parse(content);
    let mut out = Vec::new();
    if let Some(f) = &doc.frontmatter {
        if !zam_markdown::unrecognized_certainty_keys(&f.raw).is_empty() {
            out.push(CERTAINTY_NOT_RECOGNIZED.to_string());
        }
        // `verified_at_invalid` и ссылки на файлы, которые не стали ссылками (M4.5 §6.1, §7)
        out.extend(f.warnings.iter().cloned());
        if f.verified_at_commit
            .as_deref()
            .is_some_and(|c| zam_store::check_commit(c).is_none())
        {
            out.push(VERIFIED_AT_COMMIT_INVALID.to_string());
        }
        // ссылки — как их берёт хранилище (`note_refs`): `путь` или `путь#qual`, без повторов
        // `(путь, символ)` (M8 ревью задачи 3 M5.6)
        let mut valid: std::collections::HashSet<(String, Option<String>)> = Default::default();
        for r in &f.refs {
            let fr = zam_store::FileRef::parse(r);
            match zam_store::normalize_ref_path(&fr.path) {
                Ok(p) => {
                    valid.insert((p, fr.symbol));
                }
                Err(e) => {
                    let shown = if mask {
                        std::borrow::Cow::Owned(zam_markdown::mask_text(r))
                    } else {
                        std::borrow::Cow::Borrowed(r.as_str())
                    };
                    out.push(format!(
                        "invalid_ref: {} ({e})",
                        zam_store::shown_ref(&shown)
                    ))
                }
            }
        }
        if valid.len() > zam_store::file_refs::REFS_MAX {
            out.push(format!(
                "refs_truncated: учтены первые {}",
                zam_store::file_refs::REFS_MAX
            ));
        }
    }
    out.extend(zam_store::links::relation_warnings(&doc));
    out
}

/// `Node` как JSON с `warnings` (только непустые); `db` и `space` — база и пространство
/// заметки для предупреждений о псевдонимах словаря (кэш словаря — в `st`). Агенту `p` по
/// токену — эхо с маской (`frontmatter`, L0 и тексты предупреждений, задача 5 M7.1); находки
/// `secrets_detected` — по исходнику `content`, у правки агента по маске (`base` — прежний
/// текст) — только новые (круг правок 1 задачи 6, M4).
async fn node_reply(
    st: &AppState,
    p: &Principal,
    db: &zam_store::Db,
    space: zam_core::Id,
    node: &Node,
    content: &str,
    base: Option<&str>,
) -> Result<Value, ApiError> {
    let mut v = serde_json::to_value(node).map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut w = note_warnings(content, agent_view::masks(p));
    if !content.is_empty() {
        w.extend(crate::api::glossary::alias_warnings(st, db, space, &node.name, content).await?);
    }
    if !w.is_empty()
        && let Value::Object(o) = &mut v
    {
        o.insert("warnings".into(), json!(w));
    }
    agent_view::view_json(p, &mut v, agent_view::NODE_FIELDS);
    agent_view::view_json(p, &mut v, &["warnings"]);
    match base {
        Some(b) => crate::api::secrets::attach_new(&mut v, content, b),
        None => crate::api::secrets::attach(&mut v, content),
    }
    Ok(v)
}

fn check_size(st: &AppState, content: &str) -> Result<(), ApiError> {
    let max = st.cfg.limits.note_max_bytes;
    if content.len() > max {
        return Err(ApiError::PayloadTooLarge(format!(
            "заметка — {} байт, предел {max}",
            content.len()
        )));
    }
    Ok(())
}

/// `POST /nodes` → 201 `Node`; повтор с тем же `Idempotency-Key` — 200.
pub async fn create(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let req: Create = json_body(body)?;
    let path = NodePath::parse(&req.path)?;
    let key = idempotency_key(&headers)?;
    let sc = space_scope(&st, &p, &headers, &req.space, Mode::Write).await?;
    let (node, content, created) = match req.kind {
        Kind::Folder => {
            if req.content.is_some() || req.l0.is_some() {
                return Err(ApiError::BadRequest("у папки нет content и l0".into()));
            }
            if key.is_some() {
                return Err(ApiError::BadRequest(
                    "Idempotency-Key — только у заметок (kind: note)".into(),
                ));
            }
            (
                create_folder(&sc.db, sc.space.id, path).await?,
                String::new(),
                true,
            )
        }
        Kind::Note => {
            let content = req.content.unwrap_or_default();
            check_size(&st, &content)?;
            let attrib = attribution(&p, &headers, req.message)?;
            let n = NewNote {
                space_id: sc.space.id,
                path,
                content: content.clone(),
                l0: req.l0,
                attrib,
            };
            let (node, created) = create_note_keyed(&sc.db, n, key).await?;
            (node, content, created)
        }
    };
    // повтор по `Idempotency-Key` (M6 §16 п. 19) — 200 и узел в текущем виде, без уведомлений
    if !created {
        let (_, text) = node_content(&sc.db, node.id, Some(node.current_rev)).await?;
        return Ok((
            StatusCode::OK,
            Json(node_reply(&st, &p, &sc.db, sc.space.id, &node, &text, None).await?),
        ));
    }
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    st.docs.notify();
    let mut v = node_reply(&st, &p, &sc.db, sc.space.id, &node, &content, None).await?;
    // создание по токену: базы нет — метка `‹secret›` значения не хранит (решение
    // координатора по задаче 6 M7.1)
    if agent_view::masks(&p) {
        secret_restore::attach_kept(&mut v, secret_restore::marks_in(&content));
    }
    Ok((StatusCode::CREATED, Json(v)))
}

/// Ревизия из `If-Match`: `3`, `"3"` или `W/"3"`.
pub(crate) fn if_match(headers: &HeaderMap) -> Result<i64, ApiError> {
    let v = headers.get(header::IF_MATCH).ok_or_else(|| {
        ApiError::PreconditionRequired("передайте If-Match: <rev> текущей ревизии".into())
    })?;
    let s = v.to_str().unwrap_or("").trim();
    let s = s.strip_prefix("W/").unwrap_or(s);
    s.trim_matches('"')
        .parse::<i64>()
        .map_err(|_| ApiError::BadRequest("If-Match: номер ревизии".into()))
}

#[derive(Deserialize)]
pub struct Replace {
    content: String,
    l0: Option<String>,
    message: Option<String>,
}

/// `PUT /nodes/{id}` с `If-Match: <rev>` → 200 `Node`; без заголовка — 428;
/// устаревшая ревизия — 409 с `current_rev` и `diff`; повтор с тем же `Idempotency-Key` — 200
/// без новой ревизии.
pub async fn replace(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let base = if_match(&headers)?;
    let req: Replace = json_body(body)?;
    check_size(&st, &req.content)?;
    let key = idempotency_key(&headers)?;
    let attrib = attribution(&p, &headers, req.message)?;
    // агент правил вид ревизии `If-Match` (M7 §4.3.2): значения под `‹secret-T-N›`
    // — обратно по её маскам. Ревизия уже устарела (текущая только растёт) и ключа повтора нет — 409
    // хранилища без восстановления (круг правок 1, M6); с ключом — как прежде: повтор по ключу
    // старый `If-Match` не сверяет
    let stale = key.is_none() && base < sc.node.current_rev;
    let base_text = if agent_view::masks(&p) && !stale {
        secret_restore::node_base(&sc.db, sc.node.id, base).await?
    } else {
        None
    };
    let (content, kept, base_text) = match base_text {
        Some(b) => {
            // метки любой ревизии узла (§4.3.2 п. 2): вид, прочитанный до прошлой записи агента,
            // тоже возвращает значения
            let revs = secret_restore::node_revisions(
                &sc.db,
                sc.node.id,
                sc.node.current_rev.max(base),
                &[&req.content],
                &[base],
            )
            .await?;
            let (r, b) = secret_restore::restore_owned(revs, base, b, req.content).await?;
            let mut ex = secret_restore::Exposed::default();
            ex.add(None, &r);
            ex.check()?;
            (r.text, r.kept, Some(b))
        }
        None => (req.content, 0, None),
    };
    // предел — и по итоговому тексту
    check_size(&st, &content)?;
    let r = update_note_keyed(
        &sc.db,
        sc.node.id,
        base,
        content.clone(),
        req.l0,
        attrib,
        key,
    )
    .await;
    match r {
        Ok((n, fresh)) => {
            // повтор по ключу — текущий текст узла (его могли править после первого запроса)
            let (text, base_text) = if fresh {
                st.index.notify();
                st.docs.notify();
                (content, base_text)
            } else {
                (
                    node_content(&sc.db, n.id, Some(n.current_rev)).await?.1,
                    None,
                )
            };
            let mut v = node_reply(
                &st,
                &p,
                &sc.db,
                sc.space.id,
                &n,
                &text,
                base_text.as_deref(),
            )
            .await?;
            if fresh {
                secret_restore::attach_kept(&mut v, kept);
            }
            Ok(Json(v))
        }
        Err(e) => Err(write_error(&p, &sc.db, sc.node.id, base, e).await),
    }
}

#[derive(Deserialize)]
pub struct Patch {
    base_rev: i64,
    ops: Vec<PatchOp>,
    message: Option<String>,
}

/// `PATCH /nodes/{id} {base_rev, ops, message?}` → 200 `Node` / 409 / 413 / 422; повтор с тем же
/// `Idempotency-Key` — 200 без второй правки.
pub async fn patch(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let mut req: Patch = json_body(body)?;
    let key = idempotency_key(&headers)?;
    let attrib = attribution(&p, &headers, req.message.take())?;
    let max = st.cfg.limits.note_max_bytes;
    // агенту — операции по виду с метками (заголовки он видел с `‹secret-T-N›`), значения —
    // обратно по меткам (M7 §4.3.2); человеку — по исходнику, как прежде (у папки текста нет — ответ
    // хранилища тот же)
    if agent_view::masks(&p) && sc.node.kind == zam_store::NodeKind::Note {
        return patch_by_view(&st, &p, &sc, req, attrib, max, key).await;
    }
    let r = patch_note_keyed(&sc.db, sc.node.id, req.base_rev, req.ops, attrib, max, key).await;
    match r {
        Ok((n, fresh)) => {
            if fresh {
                st.index.notify();
                st.docs.notify();
            }
            let (_, text) = node_content(&sc.db, n.id, Some(n.current_rev)).await?;
            Ok(Json(
                node_reply(&st, &p, &sc.db, sc.space.id, &n, &text, None).await?,
            ))
        }
        Err(e) => Err(write_error(&p, &sc.db, sc.node.id, req.base_rev, e).await),
    }
}

/// Попыток PATCH агента по маске, если заметку меняют между подсчётом и записью.
const PATCH_ATTEMPTS: usize = 3;

/// `PATCH` агента по нумерованному виду (M7 §4.3.2; круг правок 1 задачи 6 — вне транзакции
/// писателя, M5; круг правок 3 — по номеру метки): повтор по ключу — сразу; иначе
/// [`patch_restored`] по исходникам `base_rev` и текущей ревизии; выход секрета из-под маски —
/// 422; запись — со сверкой текущей ревизии ([`zam_store::patch_note_prepared`]), она сменилась —
/// подсчёт заново (до [`PATCH_ATTEMPTS`] раз, затем 409 с `current_rev` и `diff`, как у
/// устаревшей ревизии).
async fn patch_by_view(
    st: &AppState,
    p: &Principal,
    sc: &crate::api::scope::NodeScope,
    req: Patch,
    attrib: zam_store::Attribution,
    max: usize,
    key: Option<String>,
) -> Result<Json<Value>, ApiError> {
    let id = sc.node.id;
    if let Some(n) =
        patch_replayed(&sc.db, id, req.base_rev, &req.ops, &attrib, key.clone()).await?
    {
        let (_, text) = node_content(&sc.db, n.id, Some(n.current_rev)).await?;
        return Ok(Json(
            node_reply(st, p, &sc.db, sc.space.id, &n, &text, None).await?,
        ));
    }
    if req.ops.is_empty() {
        return Err(ApiError::BadRequest("пустой список операций PATCH".into()));
    }
    for _ in 0..PATCH_ATTEMPTS {
        let (cur_rev, current) = node_content(&sc.db, id, None).await?;
        if req.base_rev > cur_rev {
            let e = zam_store::WriteError::RevMismatch {
                current_rev: cur_rev,
                current,
            };
            return Err(write_error(p, &sc.db, id, req.base_rev, e).await);
        }
        let base = if req.base_rev == cur_rev {
            None
        } else {
            let (_, b) = node_content(&sc.db, id, Some(req.base_rev)).await?;
            Some(b)
        };
        // маски ревизий по T меток операций (§4.3.2 пп. 2–3): база, текущая и любая прежняя
        let edited: Vec<&str> = req
            .ops
            .iter()
            .flat_map(|op| {
                let (section, content) = op_parts(op);
                section.into_iter().chain(std::iter::once(content))
            })
            .collect();
        let revs =
            secret_restore::node_revisions(&sc.db, id, cur_rev, &edited, &[cur_rev, req.base_rev])
                .await?;
        let ops = req.ops.clone();
        let (base_rev, tag) = (req.base_rev, agent_view::object_tag(&id, req.base_rev));
        let big = current.len()
            + base.as_ref().map_or(0, String::len)
            + revs
                .iter()
                .map(|r| r.text.as_ref().map_or(0, String::len))
                .sum::<usize>()
            > agent_view::BLOCKING_FROM;
        let prepare = move || -> (Result<secret_restore::Restored, zam_store::WriteError>, String) {
            let views = PatchViews {
                tag: &tag,
                base: base.as_deref(),
                base_rev,
                revs: &revs,
            };
            let r = patch_restored(&views, &current, cur_rev, &ops);
            (r, current)
        };
        let (r, current) = if big {
            agent_view::blocking(prepare).await?
        } else {
            prepare()
        };
        let restored = match r {
            Ok(r) => r,
            Err(e) => return Err(write_error(p, &sc.db, id, req.base_rev, e).await),
        };
        let mut ex = secret_restore::Exposed::default();
        ex.add(None, &restored);
        ex.check()?;
        #[cfg(feature = "test-hooks")]
        patch_race(&sc.db, id, cur_rev, &attrib).await;
        let w = patch_note_prepared(
            &sc.db,
            id,
            req.base_rev,
            &req.ops,
            cur_rev,
            restored.text,
            attrib.clone(),
            max,
            key.clone(),
        )
        .await;
        match w {
            Ok(Some((n, fresh))) => {
                if fresh {
                    st.index.notify();
                    st.docs.notify();
                }
                let (_, text) = node_content(&sc.db, n.id, Some(n.current_rev)).await?;
                let base = fresh.then_some(current.as_str());
                let mut v = node_reply(st, p, &sc.db, sc.space.id, &n, &text, base).await?;
                if fresh {
                    secret_restore::attach_kept(&mut v, restored.kept);
                }
                return Ok(Json(v));
            }
            Ok(None) => continue,
            Err(e) => return Err(write_error(p, &sc.db, id, req.base_rev, e).await),
        }
    }
    // заметку меняют быстрее подсчёта — тот же 409, что у устаревшей ревизии: `current_rev` и
    // `diff` от `base_rev` (круг правок 2 задачи 6, B5)
    let (current_rev, current) = node_content(&sc.db, id, None).await?;
    let e = zam_store::WriteError::RevMismatch {
        current_rev,
        current,
    };
    Err(write_error(p, &sc.db, id, req.base_rev, e).await)
}

/// Виды, по которым агент строил операции `PATCH`: ревизия `base_rev` с меткой объекта `tag`
/// (`base`; `None` — она же текущая) и ревизии с T меток операций (`revs`,
/// [`secret_restore::node_revisions`]; тексты базы и текущей — у вызывающего).
struct PatchViews<'v> {
    tag: &'v str,
    base: Option<&'v str>,
    base_rev: i64,
    revs: &'v [secret_restore::RevText],
}

/// Операции `PATCH` агента, построенные по виду `v` (`base_rev`), — к исходнику `current`
/// (M7 §4.3.2 п. 3): адрес секции — сначала точно к заголовку исходника (метки объекта — значения
/// масок их ревизий), затем, если в адресе `‹secret›`, — по заголовкам вида базы без меток; метки
/// содержимого — отложенные значения масок их ревизий (любой ревизии объекта, п. 2,
/// [`secret_restore::Deferred`]); сверка «секция не менялась после `base_rev`» — по исходникам
/// базы и текущей ([`zam_store::apply_ops_tracking`]). Покрытие по происхождению (п. 5): значения
/// из меток операций и маски текущей ревизии, перенесённые без правки, — в позициях итога.
/// Предупреждение — литералов итога сверх литералов исходника. Ошибка операции — с адресом агента.
fn patch_restored(
    v: &PatchViews,
    current: &str,
    cur_rev: i64,
    ops: &[PatchOp],
) -> Result<secret_restore::Restored, zam_store::WriteError> {
    let (tag, base) = (v.tag, v.base);
    let base_text = base.unwrap_or(current);
    let masks = secret_restore::node_masks(v.revs, &[(cur_rev, current), (v.base_rev, base_text)]);
    // адрес с меткой без номера: заголовки исходника (точно) и вида базы (без меток, §4.3.2 п. 3);
    // найденный заголовок вида базы — к исходнику по её маскам
    let docs = ops
        .iter()
        .filter_map(|op| op_parts(op).0)
        .any(super::read::plain_mark)
        .then(|| {
            let mut base_masks = secret_restore::ObjectMasks::new(tag);
            base_masks.add(base_text);
            (
                zam_markdown::parse(current),
                zam_markdown::parse(&secret_restore::numbered_view(tag, base_text)),
                base_masks,
            )
        });
    let mut texts: Vec<&str> = vec![current];
    texts.extend(base);
    // и тексты прежних ревизий меток — база значений
    texts.extend(v.revs.iter().filter_map(|r| r.text.as_deref()));
    for op in ops {
        let (section, content) = op_parts(op);
        texts.extend(section);
        texts.push(content);
    }
    let Some(mut d) = secret_restore::Deferred::new(&texts) else {
        return Err(zam_store::WriteError::Store(
            zam_store::StoreError::BadInput(
                "в тексте заняты все знаки частного использования Unicode — правьте в WebUI".into(),
            ),
        ));
    };
    // адрес секции с исходником → адрес агента (ошибки операций)
    let mut back: Vec<(String, String)> = Vec::new();
    let mut raw_ops: Vec<PatchOp> = Vec::with_capacity(ops.len());
    for op in ops {
        let (section, content) = op_parts(op);
        let section = match section {
            Some(s) => {
                let mut filled = masks.fill(s).into_owned();
                if let Some((cur_doc, view_doc, base_masks)) = &docs
                    && super::read::plain_mark(s)
                    && zam_markdown::section_by_path(cur_doc, &filled).is_none()
                {
                    match super::read::resolve_unmarked(view_doc, s, false) {
                        zam_markdown::SectionLookup::Found(x) => {
                            filled = base_masks.fill(&x.path.join(" > ")).into_owned();
                        }
                        zam_markdown::SectionLookup::NotFound => {}
                        zam_markdown::SectionLookup::Ambiguous => {
                            return Err(zam_store::WriteError::Patch(
                                zam_markdown::PatchError::Ambiguous(s.to_string()),
                            ));
                        }
                    }
                }
                if filled != s {
                    back.push((filled.clone(), s.to_string()));
                }
                Some(filled)
            }
            None => None,
        };
        raw_ops.push(with_parts(op, section, masks.defer(content, &mut d)));
    }
    // маски текущей ревизии — через операции: перенесённые без правки проверяются в итоге
    let mut keep = secret_restore::secret_ranges(current);
    let text = apply_ops_tracking(base, current, cur_rev, &raw_ops, &mut keep)
        .map_err(|e| shown_error(e, &back))?;
    Ok(d.finish(&text, &keep)
        .into_restored(secret_restore::literals_in(current)))
}

/// Адрес секции и содержимое операции.
fn op_parts(op: &PatchOp) -> (Option<&str>, &str) {
    match op {
        PatchOp::ReplaceSection { section, content }
        | PatchOp::AppendToSection { section, content }
        | PatchOp::InsertAfterHeading { section, content } => (Some(section), content),
        PatchOp::Append { content } | PatchOp::Prepend { content } => (None, content),
    }
}

/// Та же операция с адресом `section` (у операций с секцией) и содержимым `content`.
fn with_parts(op: &PatchOp, section: Option<String>, content: String) -> PatchOp {
    let s = || section.clone().unwrap_or_default();
    match op {
        PatchOp::ReplaceSection { .. } => PatchOp::ReplaceSection {
            section: s(),
            content,
        },
        PatchOp::AppendToSection { .. } => PatchOp::AppendToSection {
            section: s(),
            content,
        },
        PatchOp::InsertAfterHeading { .. } => PatchOp::InsertAfterHeading {
            section: s(),
            content,
        },
        PatchOp::Append { .. } => PatchOp::Append { content },
        PatchOp::Prepend { .. } => PatchOp::Prepend { content },
    }
}

/// Ошибка операции по исходнику — агенту: адрес секции — как он его прислал (`back`: адрес с
/// исходником → адрес агента), прочий путь из исходника — с маской.
fn shown_error(e: zam_store::WriteError, back: &[(String, String)]) -> zam_store::WriteError {
    use zam_markdown::PatchError;
    use zam_store::WriteError;
    let fix = |s: String| {
        back.iter()
            .find(|(raw, _)| *raw == s)
            .map_or_else(|| zam_markdown::mask_text(&s), |(_, shown)| shown.clone())
    };
    match e {
        WriteError::Patch(p) => WriteError::Patch(match p {
            PatchError::SectionNotFound(s) => PatchError::SectionNotFound(fix(s)),
            PatchError::SectionNotFoundTail { section, full } => PatchError::SectionNotFoundTail {
                section: fix(section),
                full: fix(full),
            },
            PatchError::Ambiguous(s) => PatchError::Ambiguous(fix(s)),
            PatchError::Broken(s) => PatchError::Broken(fix(s)),
        }),
        WriteError::SectionChanged {
            section,
            current_rev,
        } => WriteError::SectionChanged {
            section: fix(section),
            current_rev,
        },
        e => e,
    }
}

/// Тестовая точка гонки `PATCH` агента по маске (фича `test-hooks`, круг правок 2 задачи 6,
/// B6): узел → сколько раз между подсчётом и записью «чужая» правка дописывает строку в конец
/// заметки (новая ревизия — запись видит её и считает заново).
#[cfg(feature = "test-hooks")]
pub static TEST_PATCH_RACES: std::sync::Mutex<Vec<(zam_core::Id, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// Гонка [`TEST_PATCH_RACES`] для узла `id` ревизии `rev`, если задана.
#[cfg(feature = "test-hooks")]
async fn patch_race(db: &zam_store::Db, id: zam_core::Id, rev: i64, a: &zam_store::Attribution) {
    let hit = {
        let mut g = TEST_PATCH_RACES.lock().unwrap_or_else(|e| e.into_inner());
        match g.iter_mut().find(|(n, c)| *n == id && *c > 0) {
            Some((_, c)) => {
                *c -= 1;
                true
            }
            None => false,
        }
    };
    if !hit {
        return;
    }
    let (_, text) = node_content(db, id, Some(rev))
        .await
        .expect("гонка: текст ревизии");
    update_note_keyed(
        db,
        id,
        rev,
        format!("{text}гонка {rev}\n"),
        None,
        a.clone(),
        None,
    )
    .await
    .expect("гонка: правка");
}

#[derive(Deserialize)]
pub struct Move {
    path: String,
}

/// `POST /nodes/{id}/move {path}` → 200 `Node`.
pub async fn move_to(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    live(&sc)?;
    let req: Move = json_body(body)?;
    let path = NodePath::parse(&req.path)?;
    let author = attribution(&p, &headers, None)?.author_kind;
    let node = move_node(&sc.db, sc.node.id, path, author).await?;
    st.render_gen.bump_space(sc.owner, sc.space.id);
    // свёртка L0 старого родителя: задания индексации знают только новое положение
    if let Some(old) = sc.node.parent_id
        && node.parent_id != Some(old)
    {
        st.index.rollup_hint(sc.owner, old);
    }
    st.index.notify();
    st.docs.notify();
    Ok(Json(agent_view::node_json(&p, &node)?))
}

/// `DELETE /nodes/{id}` — мягкое удаление поддерева → 204; аудит `node.delete`.
pub async fn remove(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    let author = attribution(&p, &headers, None)?.author_kind;
    let count = delete_node(&sc.db, sc.node.id, author).await?;
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    st.docs.notify();
    let details = json!({ "space": sc.space.id, "path": sc.node.path, "nodes": count });
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "node.delete",
        sc.node.id,
        details,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /nodes/{id}/restore` → 200 `Node`; аудит `node.restore`.
pub async fn restore(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    client: Client,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sc = node_scope(&st, &p, &headers, &id, Mode::Write).await?;
    let author = attribution(&p, &headers, None)?.author_kind;
    let node = restore_node(&sc.db, sc.node.id, author).await?;
    st.render_gen.bump_space(sc.owner, sc.space.id);
    st.index.notify();
    st.docs.notify();
    let details = json!({ "space": sc.space.id, "path": node.path });
    audit_node(
        &st,
        &p,
        &client,
        sc.user.id,
        sc.owner,
        "node.restore",
        node.id,
        details,
    )
    .await?;
    Ok(Json(agent_view::node_json(&p, &node)?))
}

#[derive(Deserialize)]
pub struct TrashQuery {
    space: Option<String>,
}

/// `GET /trash?space=` → `{items}`: корни пакетов удаления, новые первыми.
/// Без `space` — все пространства, доступные принципалу (по токену — без Human).
pub async fn trash(
    State(st): State<AppState>,
    p: Principal,
    headers: HeaderMap,
    q: Result<Query<TrashQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let q = query(q)?;
    let items = match q.space.as_deref() {
        Some(s) => {
            let sc = space_scope(&st, &p, &headers, s, Mode::Read).await?;
            list_trash(&sc.db, Some(sc.space.id)).await?
        }
        None => {
            let (_, db) = user_db(&st, &p, &headers).await?;
            let allowed = readable_space_ids(&db, &p).await?;
            let mut all = list_trash(&db, None).await?;
            all.retain(|n| allowed.contains(&n.space_id));
            all
        }
    };
    let mut items = json!(items);
    agent_view::view_json(&p, &mut items, agent_view::NODE_FIELDS);
    Ok(Json(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn if_match_forms() {
        let mut h = HeaderMap::new();
        assert!(matches!(
            if_match(&h),
            Err(ApiError::PreconditionRequired(_))
        ));
        for (v, want) in [("3", 3), ("\"3\"", 3), ("W/\"12\"", 12), (" 4 ", 4)] {
            h.insert(header::IF_MATCH, HeaderValue::from_static(v));
            assert_eq!(if_match(&h).unwrap(), want, "{v}");
        }
        h.insert(header::IF_MATCH, HeaderValue::from_static("*"));
        assert!(matches!(if_match(&h), Err(ApiError::BadRequest(_))));
    }
}
