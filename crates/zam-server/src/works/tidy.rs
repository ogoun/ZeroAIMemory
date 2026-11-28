//! Вид внутренней работы `tidy` — уборка памяти (M7.5 §8.7 пп. 2, 4): кандидаты сборщика
//! [`zam_store::tidy`] (почти-дубли, противоречия, устаревшие, шумные, непроходящие пробы),
//! инструкция и схема итога, проверка операции — у агента и модели одни (решение 32).
//!
//! - **Сборщик** ([`collect`]): кандидаты всего запуска собираются **один раз** — первым
//!   пространством запуска (кэш в отсеве запуска [`Sifted`], §8.7 п. 4): курсор почти-дублей
//!   сдвигается раз за запуск, срез до [`zam_store::tidy::NIGHT_MAX`] за ночь с порядком типов — по
//!   всем решениям и Knowledge пользователя ([`room`]); каждое пространство получает свои, без пар
//!   отсева. Элемент пакета — кандидат: `kind` — тип кандидата, заголовок — что за кандидат,
//!   текст — записи кандидата (ссылка, вид, ревизия, «решение человека» у `axiom`, заголовок, L1)
//!   в виде агента (маска до обрезки), сигналы — сигналы сборщика и допустимые операции `ops`;
//!   остальные записи — `refs` элемента.
//! - **Итог** — одна операция на кандидата с доводом ([`SCHEMA`]): `merge {keep, drop, text?}`,
//!   `retract {target}`, `revisit {target}`, `triggers {target, triggers}`, `term {target?, title,
//!   definition, aliases}`, `append {target, text}`, `keep`; удаления нет. Проверка ([`accept`]):
//!   операция — из допустимых кандидата, ссылки — только из кандидата, `reason` —
//!   [`REASON_CHARS`] символов, тексты — до `limits.note_max_bytes` (маска секрета — общая
//!   проверка итога), триггеры — по правилам триггеров insight, термин — по правилам словаря;
//!   запись кандидата изменилась после пакета — отказ (кандидат вернётся с новой ревизией).
//! - **Записи итога** — строка `proposals` типа `tidy` ([`ItemWrite::Tidy`]): операция —
//!   `proposed` (решает человек, `POST /proposals/{id}/accept`), `keep` и отказ `{n, skip}` —
//!   `skipped` (человеку не показывается, отсев до новой ревизии записей). Тексты записей в
//!   `payload` — исходник (L1, человеку — как есть; токену — с маской при выдаче).
//!
//! Итог не меняет знания (§8.7 п. 3): операцию применяет только человек в WebUI.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use serde_json::{Value, json};
use zam_core::Id;
use zam_store::proposals::{ProposalAuthor, ProposalStatus, SourceRef};
use zam_store::tidy::{self as tstore, CandidateType, Op, TidyCandidate, TidyRecord};
use zam_store::works::{ItemWrite, Origin, WorkItem, WorkKind};
use zam_store::{Certainty, Db, EntryKind, EntryStatus, StoreError};

use super::kinds::{Author, Candidate, ItemCtx, Reject, Sifted};
use super::night::NightCtx;
use super::package::agent_text;
use super::queue::Plan;
use crate::agent_setup::Lang;
use crate::app::AppState;

/// Довод операции, символов (§8.7 п. 2).
pub const REASON_CHARS: RangeInclusive<usize> = 20..=2000;

/// Инструкция вида (ru), ≤ 300 токенов.
pub const INSTRUCTION_RU: &str = "Каждая запись пакета — кандидат уборки памяти: kind — тип \
     (contradiction, probe_fail, stale, near_duplicate, noisy), в тексте — записи кандидата \
     ([ссылка · вид · ревизия] заголовок и текст), сигналы и допустимые операции ops. Для \
     каждого кандидата выбери одну операцию из ops и дай довод reason (20–2000 символов): merge \
     {keep, drop, text?} — drop отзывается как дубль keep, text — объединённый текст вместо keep; \
     retract {target} — отозвать; revisit {target} — на пересмотр; triggers {target, triggers} — \
     новый набор триггеров; term {target?, title, definition, aliases} — новый термин или \
     псевдонимы к t:<id>; append {target, text} — дописать; keep — оставить как есть. Ссылки — \
     только из записей кандидата; удаления нет. Запись «решение человека» меняй только при явной \
     ошибке. Итог не меняет память: операцию применяет человек.";
/// Инструкция вида (en), ≤ 300 токенов.
pub const INSTRUCTION_EN: &str = "Each package record is a memory tidy candidate: kind is its \
     type (contradiction, probe_fail, stale, near_duplicate, noisy), the text holds the \
     candidate's records ([reference · kind · revision] title and text), then signals and the \
     allowed operations ops. For each candidate pick one operation from ops and give a reason \
     (20–2000 characters): merge {keep, drop, text?} — drop is retracted as a duplicate of keep, \
     text is the merged text replacing keep; retract {target}; revisit {target} — to review; \
     triggers {target, triggers} — a new trigger set; term {target?, title, definition, aliases} \
     — a new term or aliases for t:<id>; append {target, text}; keep — leave as is. References \
     only from the candidate's records; there is no delete. Change a “human decision” record only \
     for a clear error. The result does not change memory: a human applies the operation.";
/// Схема итога вида.
pub const SCHEMA: &str = r#"{"items": [{"n": 1, "op": "merge|retract|revisit|triggers|term|append|keep", "keep": "i:…", "drop": "i:…", "target": "i:…", "text": "…", "triggers": [{"type": "keyword", "pattern": "…"}], "title": "…", "definition": "…", "aliases": ["…"], "reason": "…"}], "skip": "<причина>"?}"#;

/// Сигнал элемента пакета: допустимые операции кандидата.
pub const SIGNAL_OPS: &str = "ops";

/// Инструкция вида на языке `lang`.
pub fn instruction(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => INSTRUCTION_RU,
        Lang::En => INSTRUCTION_EN,
    }
}

/// Модель векторов кэша `embeddings` (почти-дубли): загруженный эмбеддер сервера; в тестах —
/// тестовая точка [`test_hooks::set_model`].
fn model_id(st: &AppState) -> Option<String> {
    #[cfg(feature = "test-hooks")]
    if let Some(m) = test_hooks::model() {
        return Some(m);
    }
    st.index.model_id().map(str::to_string)
}

/// Остаток кандидатов уборки запуска `plan`: [`zam_store::tidy::NIGHT_MAX`] за ночь (у ручных
/// запусков — за сутки UTC) за вычетом поставленных (элементы работ вида этого запуска).
pub async fn room(db: &Db, plan: &Plan) -> Result<usize, StoreError> {
    let night = plan.night.filter(|_| plan.origin == Origin::Nightly);
    let made =
        zam_store::works::count_items(db, WorkKind::Tidy, plan.origin, night, plan.since).await?;
    Ok(tstore::NIGHT_MAX.saturating_sub(made))
}

/// Название типа кандидата для заголовка элемента пакета.
fn type_title(t: CandidateType) -> &'static str {
    match t {
        CandidateType::Contradiction => "Неразрешённое противоречие",
        CandidateType::ProbeFail => "Непроходящая проба: запись не находится",
        CandidateType::Stale => "Возможно устарело",
        CandidateType::NearDuplicate => "Почти-дубли",
        CandidateType::Noisy => "Шумная подсказка: показывается, но не открывается",
    }
}

/// Строка записи в тексте элемента: `[ссылка · вид · ревизия · достоверность]`, заголовок, L1.
fn record_text(r: &TidyRecord) -> String {
    let mut head = vec![r.target.clone(), r.kind.as_str().to_string()];
    if let Some(k) = &r.insight_kind {
        head.push(k.clone());
    }
    head.push(format!("rev {}", r.rev));
    if let Some(s) = r.status.filter(|s| *s != EntryStatus::Active) {
        head.push(s.as_str().to_string());
    }
    match r.certainty {
        Some(Certainty::Axiom) => head.push("axiom — решение человека".into()),
        Some(c) => head.push(c.as_str().to_string()),
        None => {}
    }
    format!(
        "[{}]\n{}\n{}",
        head.join(" · "),
        r.title.trim(),
        r.l1.trim()
    )
}

/// Элемент пакета из кандидата (см. модуль).
fn to_candidate(c: &TidyCandidate) -> Candidate {
    let first = &c.records[0];
    let text = c
        .records
        .iter()
        .map(record_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut signals = c.signals.clone();
    let ops: Vec<&str> = c.ops().iter().map(|o| o.as_str()).collect();
    signals.push((SIGNAL_OPS.to_string(), ops.join(", ")));
    Candidate {
        target: first.target.clone(),
        rev: first.rev,
        kind: c.ctype.as_str().to_string(),
        title: format!("{} — записей: {}", type_title(c.ctype), c.records.len()),
        text: agent_text(&text),
        signals,
        refs: c.records[1..]
            .iter()
            .map(|r| (r.target.clone(), r.rev))
            .collect(),
    }
}

/// Кандидаты вида `tidy` пространства `space` пользователя `user` (см. модуль): кандидаты
/// запуска — один раз на запуск (`sifted`), из них — этого пространства без пар отсева, до
/// `limit`. Пространство не вида работы (Human, Rules) — пусто.
pub async fn collect(
    ctx: &NightCtx,
    user: Id,
    space: Id,
    limit: usize,
    sifted: &Sifted,
) -> Result<Vec<Candidate>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let st = ctx.st();
    let db = st.users.get(user).await?;
    let all = sifted
        .tidy
        .get_or_try_init(|| async {
            let spaces = super::queue::kind_spaces(&db, WorkKind::Tidy, None).await?;
            tstore::collect(
                &db,
                tstore::Collect {
                    owner: user,
                    spaces,
                    model: model_id(st),
                    dedup_threshold: st.cfg.insights.dedup_threshold,
                    limit: sifted.tidy_room.unwrap_or(tstore::NIGHT_MAX),
                    exclude: sifted.pairs.clone(),
                    declined: sifted.declined.clone(),
                },
            )
            .await
        })
        .await?;
    Ok(all
        .iter()
        .filter(|c| c.space == space)
        .filter(|c| c.refs().iter().all(|r| !sifted.pairs.contains(r)))
        .take(limit)
        .map(to_candidate)
        .collect())
}

/// Записи кандидата элемента `it` — живые, той же ревизии, что в пакете, по порядку элемента;
/// иначе — отказ (запись изменилась: кандидат вернётся с новой ревизией).
async fn item_records(ctx: &ItemCtx<'_>, it: &WorkItem) -> Result<Vec<TidyRecord>, Reject> {
    let refs = it.all_refs();
    let ids: Vec<Id> = refs
        .iter()
        .filter_map(|(r, _)| r.split_once(':').and_then(|(_, id)| id.parse().ok()))
        .collect();
    let found = tstore::records(ctx.db, ctx.user, &ids).await.map_err(|e| {
        tracing::warn!(error = %e, "уборка: проверка операции не дочитала базу");
        Reject::new(super::probes::REASON_STORE)
    })?;
    let by_ref: HashMap<&str, &TidyRecord> = found.iter().map(|r| (r.target.as_str(), r)).collect();
    let mut out = Vec::with_capacity(refs.len());
    for (r, rev) in &refs {
        match by_ref.get(r.as_str()) {
            Some(x) if x.rev == *rev => out.push((*x).clone()),
            _ => {
                return Err(Reject::new(format!(
                    "запись {r} изменилась после пакета — кандидат вернётся следующей ночью с \
                     новой ревизией"
                )));
            }
        }
    }
    Ok(out)
}

/// Текст операции (`text` у `merge` и `append`): непустой, до `limits.note_max_bytes` байт.
fn check_text(st: &AppState, field: &str, v: Option<&Value>) -> Result<String, Reject> {
    let t = v.and_then(Value::as_str).unwrap_or("").trim();
    if t.is_empty() {
        return Err(Reject::new(format!("{field}: непустой текст")));
    }
    let max = st.cfg.limits.note_max_bytes;
    if t.len() > max {
        return Err(Reject::new(format!(
            "{field}: {} байт — предел {max}",
            t.len()
        )));
    }
    Ok(t.to_string())
}

/// Строки массива `field` элемента.
fn strings(item: &Value, field: &str) -> Result<Vec<String>, Reject> {
    match item.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| Reject::new(format!("{field}: массив строк")))
            })
            .collect(),
        Some(_) => Err(Reject::new(format!("{field}: массив строк"))),
    }
}

/// Проверенная операция элемента: операция, её JSON (`payload.op`) и довод.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedOp {
    pub op: Op,
    pub json: Value,
    pub reason: String,
}

/// Проверка операции элемента `item` кандидата типа `t` с записями `records` (§8.7 п. 2, см.
/// модуль). Ничего не пишет.
pub fn check_op(
    st: &AppState,
    t: CandidateType,
    records: &[TidyRecord],
    item: &Value,
) -> Result<CheckedOp, Reject> {
    let ops = tstore::ops_for(t, records);
    let names = ops
        .iter()
        .map(|o| o.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let raw = item.get("op").and_then(Value::as_str).unwrap_or("").trim();
    let Some(op) = Op::parse(raw) else {
        return Err(Reject::new(format!(
            "op «{raw}»: одна из операций кандидата ({names}); удаления нет — отзыв, замещение, \
             пересмотр и дописка оставляют историю"
        )));
    };
    if !ops.contains(&op) {
        return Err(Reject::new(format!(
            "операция {} недопустима для кандидата {}: допустимы {names}",
            op.as_str(),
            t.as_str()
        )));
    }
    let reason = item
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !REASON_CHARS.contains(&reason.chars().count()) {
        return Err(Reject::new(format!(
            "reason — довод операции, от {} до {} символов",
            REASON_CHARS.start(),
            REASON_CHARS.end()
        )));
    }
    let refs = records
        .iter()
        .map(|r| r.target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let rec = |field: &str| -> Result<&TidyRecord, Reject> {
        let v = item.get(field).and_then(Value::as_str).unwrap_or("").trim();
        records.iter().find(|r| r.target == v).ok_or_else(|| {
            Reject::new(format!(
                "{field}: ссылка — только из записей кандидата ({refs})"
            ))
        })
    };
    let json = match op {
        Op::Merge => {
            let (keep, drop) = (rec("keep")?, rec("drop")?);
            if keep.id == drop.id {
                return Err(Reject::new("merge: keep и drop — разные записи кандидата"));
            }
            let mut j = json!({"op": "merge", "keep": keep.target, "drop": drop.target});
            if item.get("text").is_some_and(|v| !v.is_null()) {
                j["text"] = json!(check_text(st, "text", item.get("text"))?);
            }
            j
        }
        Op::Retract => json!({"op": "retract", "target": rec("target")?.target}),
        Op::Revisit => {
            let r = rec("target")?;
            if r.kind != EntryKind::Insight || r.status != Some(EntryStatus::Active) {
                return Err(Reject::new(
                    "revisit: только действующий (active) insight кандидата",
                ));
            }
            json!({"op": "revisit", "target": r.target})
        }
        Op::Triggers => {
            let r = rec("target")?;
            if r.kind != EntryKind::Insight {
                return Err(Reject::new("triggers: только у insight"));
            }
            let raw = match item.get("triggers") {
                Some(Value::Array(a)) => a.clone(),
                _ => {
                    return Err(Reject::new(
                        "triggers: массив {type, pattern, tools?} — новый набор целиком",
                    ));
                }
            };
            let parsed = crate::api::insights::parse_triggers(raw)
                .map_err(|e| Reject::new(e.to_string()))?;
            json!({"op": "triggers", "target": r.target, "triggers": parsed
                .iter()
                .map(|t| json!({"type": t.kind, "pattern": t.pattern, "tools": t.tools}))
                .collect::<Vec<_>>()})
        }
        Op::Term => {
            let aliases = strings(item, "aliases")?;
            match item.get("target").and_then(Value::as_str) {
                Some(_) => {
                    let r = rec("target")?;
                    if r.kind != EntryKind::Term {
                        return Err(Reject::new("term: target — термин t:<id> кандидата"));
                    }
                    if aliases.is_empty() {
                        return Err(Reject::new("term: aliases — псевдонимы к термину"));
                    }
                    let mut all = r.aliases.clone();
                    all.extend(aliases.iter().cloned());
                    zam_store::check_term(&r.title, &json!({"definition": "-", "aliases": all}))
                        .map_err(Reject::new)?;
                    json!({"op": "term", "target": r.target, "aliases": aliases})
                }
                None => {
                    let title = item
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let definition = item
                        .get("definition")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let (title, td) = zam_store::check_term(
                        title,
                        &json!({"definition": definition, "aliases": aliases}),
                    )
                    .map_err(Reject::new)?;
                    json!({"op": "term", "title": title, "definition": td.definition,
                           "aliases": td.aliases})
                }
            }
        }
        Op::Append => {
            let r = rec("target")?;
            json!({"op": "append", "target": r.target,
                   "text": check_text(st, "text", item.get("text"))?})
        }
        Op::Keep => json!({"op": "keep"}),
    };
    Ok(CheckedOp { op, json, reason })
}

/// Запись кандидата в `payload.records` предложения (исходник; токену — с маской при выдаче).
pub fn record_json(r: &TidyRecord) -> Value {
    json!({
        "ref": r.target,
        "rev": r.rev,
        "kind": r.kind.as_str(),
        "insight_kind": r.insight_kind,
        "space": r.space,
        "title": r.title,
        "l1": r.l1,
        "status": r.status.map(EntryStatus::as_str),
        "certainty": r.certainty.map(Certainty::as_str),
        "human_decision": r.certainty == Some(Certainty::Axiom),
    })
}

/// Автор строки предложения.
fn author_of(a: &Author) -> (ProposalAuthor, String, Option<String>) {
    match a {
        Author::Agent(s) => (ProposalAuthor::Agent, a.label(), Some(s.clone())),
        Author::Model(_) => (ProposalAuthor::Model, a.label(), None),
    }
}

/// Тип кандидата элемента.
fn ctype_of(it: &WorkItem) -> Result<CandidateType, Reject> {
    CandidateType::parse(&it.kind).ok_or_else(|| Reject::new("элемент — не кандидат уборки"))
}

/// Проверка элемента итога `{n, op, …, reason}` номер `n` (общие проверки — номер, цель из
/// пакета, маска, явный `skip` — уже прошли). Принятый — одна строка предложения
/// ([`ItemWrite::Tidy`]): `proposed`, у `keep` — `skipped`.
pub async fn accept(ctx: &ItemCtx<'_>, n: usize, item: &Value) -> Result<Vec<ItemWrite>, Reject> {
    let Some(it) = ctx.work.item(n) else {
        return Err(Reject::new("нет элемента с таким n в пакете"));
    };
    let t = ctype_of(it)?;
    let records = item_records(ctx, it).await?;
    let c = check_op(ctx.st, t, &records, item)?;
    let refs = it.all_refs();
    let signals: Vec<Value> = it
        .signals
        .iter()
        .filter(|(k, _)| k != SIGNAL_OPS)
        .map(|(k, v)| json!({"name": k, "value": v}))
        .collect();
    let ops: Vec<&str> = tstore::ops_for(t, &records)
        .iter()
        .map(|o| o.as_str())
        .collect();
    let payload = json!({
        "candidate": t.as_str(),
        "signals": signals,
        "records": records.iter().map(record_json).collect::<Vec<_>>(),
        "ops": ops,
        "op": c.json,
    });
    let (author_kind, author, session) = author_of(ctx.author);
    Ok(vec![ItemWrite::Tidy {
        status: if c.op == Op::Keep {
            ProposalStatus::Skipped
        } else {
            ProposalStatus::Proposed
        },
        targets: refs
            .iter()
            .map(|(r, rev)| SourceRef {
                target: r.clone(),
                rev: *rev,
            })
            .collect(),
        payload,
        reason: c.reason,
        dedup_key: tstore::dedup_key(t, &refs),
        author_kind,
        author,
        session,
    }])
}

/// Отказ исполнителя от кандидата `{n, skip: "почему"}` — как операция `keep`: строка `skipped`
/// (отсев до новой ревизии записей; человеку не показывается).
pub fn skip(ctx: &ItemCtx<'_>, item: &WorkItem, why: &str) -> Vec<ItemWrite> {
    let Ok(t) = ctype_of(item) else {
        return Vec::new();
    };
    let refs = item.all_refs();
    let (author_kind, author, session) = author_of(ctx.author);
    vec![ItemWrite::Tidy {
        status: ProposalStatus::Skipped,
        targets: refs
            .iter()
            .map(|(r, rev)| SourceRef {
                target: r.clone(),
                rev: *rev,
            })
            .collect(),
        payload: json!({"candidate": t.as_str(), "op": {"op": "keep"}}),
        reason: why.to_string(),
        dedup_key: tstore::dedup_key(t, &refs),
        author_kind,
        author,
        session,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Инструкции вида — в пределе 300 токенов; в них — все операции и «удаления нет».
    #[test]
    fn instruction_within_budget() {
        for t in [INSTRUCTION_RU, INSTRUCTION_EN] {
            let n = zam_store::count_tokens(t);
            assert!(n <= 300, "{n}");
            for op in Op::ALL {
                assert!(t.contains(op.as_str()), "{}", op.as_str());
            }
        }
        assert!(INSTRUCTION_RU.contains("удаления нет"));
        assert!(INSTRUCTION_EN.contains("no delete"));
        assert!(!SCHEMA.contains("delete"));
    }
}

/// Тестовые точки вида (фича `test-hooks`): модель векторов почти-дублей вместо эмбеддера
/// сервера (в тестах модели нет, векторы кладутся в кэш `embeddings` прямо).
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::Mutex;

    static MODEL: Mutex<Option<String>> = Mutex::new(None);

    /// Модель векторов почти-дублей (`None` — эмбеддер сервера).
    pub fn set_model(m: Option<&str>) {
        *MODEL.lock().unwrap_or_else(|e| e.into_inner()) = m.map(str::to_string);
    }

    pub(super) fn model() -> Option<String> {
        MODEL.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
