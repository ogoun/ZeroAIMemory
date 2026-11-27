//! Вид внутренней работы `probes` (M7.3 §6.4 п. 2): сборщик кандидатов по слоям, инструкция и
//! схема итога, проверка пробного вопроса — у агента и модели одни (решение координатора 32).
//!
//! - **Сборщик** ([`collect`], [`collect_on`]): записи пространства **без активной пробы** —
//!   insight, термины и заметки ([`zam_store::probes::probe_sources`]: только записи владельца
//!   базы, без замещённых, отозванных и исключённых из индекса; задачи, вопросы, идеи, changelog,
//!   выпуски и отчёты не берутся никогда — они быстро устаревают, и агент не ищет их как знание).
//!   Слои — insight по `data.kind` (`insight:<вид>`), заметки (в Rules — заметки Rules), термины;
//!   по кругу слоёв, порядок слоёв в круге — `blake3("probes" ‖ ночь ‖ "layer:" ‖ слой)`, внутри
//!   слоя — `blake3("probes" ‖ ночь ‖ id)` ([`order`]): детерминированно и каждую ночь иначе, при
//!   малом пределе ни один слой не голодает. Пары «ссылка, ревизия» из итогов прежних работ
//!   (`secret_in_payload` второго скана, явный `skip` исполнителя) до новой ревизии записи не
//!   предлагаются (решение 72); отправленные в пакетах этого запуска (ночь, сутки ручных) — до
//!   следующего (`exclude`, [`zam_store::works::offered_refs`]). Ночь — дата последнего слота
//!   `[works] at`/`tz` не позже «сейчас» ([`current_night`]): ночью — дата ночи, днём (ручной
//!   запуск) — прошедшей.
//! - **Элемент пакета** — заголовок (у заметки — первый H1 и путь), текст вида агента (маска, до
//!   1500 символов); запись длиннее [`LONG_RECORD_CHARS`] символов — сигнал `вопросов: 2`.
//! - **Итог** — `{"items": [{"target": "<ref из пакета>", "question": "…"} | {"target": "…",
//!   "skip": "…"}]}` ([`SCHEMA`]; номер `n` вместо `target` тоже принимается). Проверка элемента
//!   ([`accept`], [`check_question`]): 8–25 слов и не больше 300 символов; нет пути (`/` или `\`
//!   с расширением файла), ссылки `<буква>:<26 знаков base32>`, даты `ГГГГ-ММ-ДД`, URL, обратных
//!   кавычек; доля предметных слов вопроса среди слов заголовка цели
//!   ([`zam_search::eval::overlap`]) больше [`OVERLAP_MAX`] — отказ «пересказ заголовка» (4 из
//!   8 — принят, 13 из 25 — нет); одна проба на запись, две — у длинной; повтор нормализованного
//!   вопроса (в итоге и у активных проб пространства) — отказ; маска секрета — отказ (общая
//!   проверка итога и здесь).
//! - **Пределы** — на пользователя ([`quota`]): новых проб — [`PROBES_PER_NIGHT`] за ночь у
//!   ночных работ и [`PROBES_PER_DAY_MANUAL`] за сутки UTC у ручных (`works.origin`,
//!   [`zam_store::probes::new_probes_since`]): ручной запуск днём не съедает ночь; активных —
//!   [`zam_store::probes::PROBES_MAX`]. При записи (транзакция закрытия работы) пределы и повтор
//!   сверяются ещё раз ([`zam_store::works::ItemWrite::Probe`]): не записанная проба — строка
//!   `warnings` итога и ответа исполнителю (финальное ревью M7.3, Minor 1).
//! - **Принятая проба** — строка `probes` вида `probe`: цель — `[{ref, rev}]` элемента пакета,
//!   часть — [`zam_search::eval::probe_split`] по цели, `overlap`, автор — сессия агента или
//!   `model:<имя модели>`, работа. Знания не меняются.

use std::collections::{BTreeMap, HashSet};
use std::ops::RangeInclusive;

use jiff::{Timestamp, civil};
use serde_json::Value;
use zam_core::Id;
use zam_store::probes::{
    NewProbe, PROBES_MAX, ProbeAuthor, ProbeKind, ProbeSource, ProbeTarget, probe_sources,
    source_text,
};
use zam_store::works::{ItemWrite, Origin, WorkItem, WorkKind};
use zam_store::{Db, StoreError, normalize_query};

use super::kinds::{Author, Candidate, ItemCtx, Reject, space_kinds};
use super::night::NightCtx;
use super::package::{agent_text, check_mask_literal};
use crate::app::AppState;
use crate::config::WorksSection;

/// Новых проб на пользователя за ночь (ночные работы, `origin = nightly`; §6.4 п. 1).
pub const PROBES_PER_NIGHT: u32 = 40;
/// Новых проб на пользователя за сутки UTC у ручных запусков (`origin = manual`; §6.3 п. 2).
pub const PROBES_PER_DAY_MANUAL: u32 = 40;
/// Слов в пробном вопросе.
pub const PROBE_WORDS: RangeInclusive<usize> = 8..=25;
/// Символов в пробном вопросе — не больше.
pub const PROBE_MAX_CHARS: usize = 300;
/// Доля предметных слов вопроса среди слов заголовка цели — больше этого отказ «пересказ
/// заголовка» (решение координатора 46).
pub const OVERLAP_MAX: f64 = 0.5;
/// Запись длиннее стольких символов получает две пробы.
pub const LONG_RECORD_CHARS: usize = 2000;
/// Сигнал элемента пакета у длинной записи: `вопросов: 2`.
pub const SIGNAL_QUESTIONS: &str = "вопросов";
/// Соль порядка записей внутри слоя.
pub const ORDER_SALT: &str = "probes";

/// Схема итога вида (§6.4 п. 2).
pub const SCHEMA: &str =
    r#"{"items": [{"target": "<ref из пакета>", "question": "…"} | {"target": "…", "skip": "…"}]}"#;

/// Инструкция вида, ru (≤ 300 токенов; правила claude-note — обзор §2.4).
pub const INSTRUCTION_RU: &str = "Пробные вопросы ночной проверки памяти: по ним сервер \
     проверяет, находит ли поиск запись. Для каждой записи напиши вопрос, который коллега задал \
     бы, ещё не зная о ней: своими словами, на языке записи, 8–25 слов, до 300 символов; без \
     путей файлов, ссылок и id записей, дат, URL и кода в обратных кавычках; не пересказывай \
     заголовок (больше половины значимых слов вопроса из заголовка — отказ). Одна проба на \
     запись; у записи с сигналом «вопросов: 2» (длиннее 2000 символов) — два разных вопроса о \
     разных её частях. target — значение ref элемента. О записи нельзя спросить — \
     {\"target\": \"<ref>\", \"skip\": \"<почему>\"}.";

/// Инструкция вида, en.
pub const INSTRUCTION_EN: &str = "Probe questions for the nightly memory check: the server uses \
     them to check whether search finds the record. For each record write a question a \
     colleague would ask before knowing about the record: in your own words, in the record's \
     language, 8–25 words, up to 300 characters; no file paths, record references or ids, \
     dates, URLs or code in backticks; do not retell the title (more than half of the \
     question's meaningful words taken from the title — rejected). One probe per record; a \
     record with the signal «вопросов: 2» (longer than 2000 characters) gets two different \
     questions about different parts of it. target is the ref value of the item. If you \
     cannot ask about a record — {\"target\": \"<ref>\", \"skip\": \"<why>\"}.";

/// Причина отказа: вопрос пересказывает заголовок цели.
pub const REASON_RETELL: &str =
    "пересказ заголовка: больше половины значимых слов вопроса — из заголовка записи";
/// Причина отказа: тот же нормализованный вопрос уже есть.
pub const REASON_DUPLICATE: &str = "такой вопрос уже есть: нужен другой";
/// Причина отказа второй пробы записи не длиннее [`LONG_RECORD_CHARS`] символов.
pub const REASON_ONE_PER_RECORD: &str =
    "одна проба на запись: вторая — только у записи длиннее 2000 символов (вопросов: 2)";
/// Причина отказа третьей пробы длинной записи.
pub const REASON_TWO_PER_RECORD: &str = "не больше двух проб на запись";

/// Ночь сборщика и начало её счёта: дата и момент последнего слота `[works] at`/`tz`, не
/// позже `now` (ночью — ночь окна; днём — прошедшая ночь).
pub fn current_night(now: Timestamp, w: &WorksSection) -> (civil::Date, Timestamp) {
    let tz = w.time_zone();
    let today = now.to_zoned(tz.clone()).date();
    let at = w.at_hm();
    let mut day = today;
    for _ in 0..3 {
        let start = crate::schedule::slot(day, at, &tz);
        if start <= now {
            return (day, start);
        }
        match day.yesterday() {
            Ok(d) => day = d,
            Err(_) => break,
        }
    }
    (day, crate::schedule::slot(day, at, &tz))
}

/// Ключ порядка записи `id` внутри слоя в ночь `night`: `blake3("probes" ‖ ночь ‖ id)`.
fn order_key(night: civil::Date, id: Id) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(ORDER_SALT.as_bytes());
    h.update(night.to_string().as_bytes());
    h.update(id.to_string().as_bytes());
    *h.finalize().as_bytes()
}

/// Ключ места слоя `layer` в круге ночи `night`: `blake3("probes" ‖ ночь ‖ "layer:" ‖ слой)`
/// (ревью r1, M2: круг не начинается всегда с первого по имени слоя).
fn layer_key(night: civil::Date, layer: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(ORDER_SALT.as_bytes());
    h.update(night.to_string().as_bytes());
    h.update(b"layer:");
    h.update(layer.as_bytes());
    *h.finalize().as_bytes()
}

/// Источник с ключом порядка внутри слоя.
type Keyed = ([u8; 32], ProbeSource);

/// Порядок источников ночи `night`: по кругу слоёв (порядок слоёв в круге — [`layer_key`]),
/// внутри слоя — по [`order_key`].
pub fn order(night: civil::Date, sources: Vec<ProbeSource>) -> Vec<ProbeSource> {
    let mut layers: BTreeMap<String, Vec<Keyed>> = BTreeMap::new();
    for s in sources {
        let key = order_key(night, s.id);
        layers.entry(s.layer.clone()).or_default().push((key, s));
    }
    let mut layers: Vec<(String, Vec<Keyed>)> = layers.into_iter().collect();
    layers.sort_by_cached_key(|(name, _)| (layer_key(night, name), name.clone()));
    let mut queues: Vec<std::vec::IntoIter<Keyed>> = layers
        .into_iter()
        .map(|(_, mut v)| {
            v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
            v.into_iter()
        })
        .collect();
    let mut out = Vec::new();
    loop {
        let before = out.len();
        for q in &mut queues {
            if let Some((_, s)) = q.next() {
                out.push(s);
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// Кандидаты вида `probes` пространства `space` пользователя `user` в текущую ночь
/// ([`current_night`]), до `limit`, без пар `exclude` (отправленные в этом запуске).
pub async fn collect(
    ctx: &NightCtx,
    user: Id,
    space: Id,
    limit: usize,
    exclude: &HashSet<(String, i64)>,
) -> Result<Vec<Candidate>, StoreError> {
    let (night, _) = current_night(zam_store::time::now(), &ctx.st().cfg.works);
    collect_on(ctx, user, space, night, limit, exclude).await
}

/// Кандидаты вида `probes` ночи `night` (см. модуль): пространство не вида работы (Human) —
/// пусто; пары `exclude` отсеиваются до `limit`. Принятые элементы прежних итогов отдельно не
/// отсеиваются (ревью r1, M1): запись с активной пробой и так не источник, а запись, чья проба
/// не записалась (сверка в транзакции закрытия), должна вернуться в кандидаты.
pub async fn collect_on(
    ctx: &NightCtx,
    user: Id,
    space: Id,
    night: civil::Date,
    limit: usize,
    exclude: &HashSet<(String, i64)>,
) -> Result<Vec<Candidate>, StoreError> {
    let db = ctx.st().users.get(user).await?;
    let allowed = space_kinds(WorkKind::Probes);
    let ok_space = zam_store::list_spaces(&db)
        .await?
        .into_iter()
        .any(|s| s.id == space && allowed.contains(&s.kind));
    if !ok_space || limit == 0 {
        return Ok(Vec::new());
    }
    let skip = zam_store::works::excluded_refs(&db, WorkKind::Probes).await?;
    let sources: Vec<ProbeSource> = probe_sources(&db, user, space)
        .await?
        .into_iter()
        .filter(|s| {
            let pair = (s.target.clone(), s.rev);
            !skip.contains(&pair) && !exclude.contains(&pair)
        })
        .collect();
    let mut out = Vec::new();
    for s in order(night, sources) {
        if out.len() >= limit {
            break;
        }
        let Some(t) = source_text(&db, &s).await? else {
            continue;
        };
        if t.text.is_empty() {
            // о пустой записи спросить нечего
            continue;
        }
        let mut signals = Vec::new();
        if t.text.chars().count() > LONG_RECORD_CHARS {
            signals.push((SIGNAL_QUESTIONS.to_string(), "2".to_string()));
        }
        out.push(Candidate {
            target: s.target,
            rev: s.rev,
            kind: s.kind,
            title: t.title,
            text: agent_text(&t.text),
            signals,
        });
    }
    Ok(out)
}

/// Проб на запись элемента `it`: две — у записи с сигналом `вопросов: 2`, иначе одна.
pub fn questions_for(it: &WorkItem) -> usize {
    if it
        .signals
        .iter()
        .any(|(k, v)| k == SIGNAL_QUESTIONS && v == "2")
    {
        2
    } else {
        1
    }
}

/// Слова вопроса: куски между пробелами, в которых есть буква или цифра.
fn words(q: &str) -> usize {
    q.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
}

/// Кусок текста без окружающих знаков препинания и кавычек.
fn trim_punct(w: &str) -> &str {
    w.trim_matches(|c: char| {
        !(c.is_alphanumeric() || c == '/' || c == '\\' || c == '.' || c == '_')
    })
    .trim_end_matches('.')
}

/// Путь файла: кусок с `/` или `\` и расширением в последней части (`src/main.rs`,
/// `C:\x\y.txt`, `config/.env`).
fn has_path(q: &str) -> bool {
    q.split_whitespace().map(trim_punct).any(|w| {
        let Some(cut) = w.rfind(['/', '\\']) else {
            return false;
        };
        let last = &w[cut + 1..];
        last.rsplit_once('.').is_some_and(|(_, ext)| {
            (1..=10).contains(&ext.chars().count())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
    })
}

/// Знак Crockford base32 (id `zam_core::Id`), без учёта регистра.
fn base32(c: char) -> bool {
    c.is_ascii_digit()
        || (c.is_ascii_alphabetic() && !matches!(c.to_ascii_uppercase(), 'I' | 'L' | 'O' | 'U'))
}

/// Ссылка записи `<буква>:<26 знаков base32>` (`i:01M7…`).
fn has_ref(q: &str) -> bool {
    let cs: Vec<char> = q.chars().collect();
    (1..cs.len()).any(|i| {
        cs[i] == ':'
            && cs[i - 1].is_ascii_alphabetic()
            && (i < 2 || !cs[i - 2].is_alphanumeric())
            && cs.len() >= i + 27
            && cs[i + 1..i + 27].iter().all(|c| base32(*c))
            && cs.get(i + 27).is_none_or(|c| !c.is_alphanumeric())
    })
}

/// Дата `ГГГГ-ММ-ДД`.
fn has_date(q: &str) -> bool {
    let cs: Vec<char> = q.chars().collect();
    let digit = |i: usize| cs.get(i).is_some_and(char::is_ascii_digit);
    (0..cs.len()).any(|i| {
        (i == 0 || !digit(i - 1))
            && (0..4).all(|k| digit(i + k))
            && cs.get(i + 4) == Some(&'-')
            && (5..7).all(|k| digit(i + k))
            && cs.get(i + 7) == Some(&'-')
            && (8..10).all(|k| digit(i + k))
            && !digit(i + 10)
    })
}

/// URL: `схема://…` или `www.…`.
fn has_url(q: &str) -> bool {
    q.contains("://")
        || q.split_whitespace()
            .map(trim_punct)
            .any(|w| w.to_lowercase().starts_with("www."))
}

/// Проверка текста пробного вопроса `question` к записи с заголовком `title` (§6.4 п. 2):
/// слова, длина, маска секрета, обратные кавычки, URL, путь, ссылка, дата, пересказ заголовка.
/// `Ok` — доля предметных слов вопроса среди слов заголовка (`overlap` пробы).
pub fn check_question(question: &str, title: &str) -> Result<f64, Reject> {
    let q = question.trim();
    let n = words(q);
    if !PROBE_WORDS.contains(&n) {
        return Err(Reject::new(format!(
            "слов в вопросе: {n} — нужно {}–{}",
            PROBE_WORDS.start(),
            PROBE_WORDS.end()
        )));
    }
    let chars = q.chars().count();
    if chars > PROBE_MAX_CHARS {
        return Err(Reject::new(format!(
            "символов в вопросе: {chars} — не больше {PROBE_MAX_CHARS}"
        )));
    }
    check_mask_literal(q)?;
    if q.contains('`') {
        return Err(Reject::new(
            "обратные кавычки: код в вопросе не нужен — спроси словами",
        ));
    }
    if has_url(q) {
        return Err(Reject::new("URL в вопросе: спроси словами, без адреса"));
    }
    if has_path(q) {
        return Err(Reject::new("путь файла в вопросе: спроси о сути, без пути"));
    }
    if has_ref(q) {
        return Err(Reject::new(
            "ссылка или id записи в вопросе: коллега их ещё не знает",
        ));
    }
    if has_date(q) {
        return Err(Reject::new("дата в вопросе: спроси без даты"));
    }
    let ov = zam_search::eval::overlap(q, title);
    if ov > OVERLAP_MAX {
        return Err(Reject::new(REASON_RETELL));
    }
    Ok(ov)
}

/// Предел новых проб работы: постановка (`works.origin`), начало счёта и предел — ночью с начала
/// текущей ночи ([`current_night`]) [`PROBES_PER_NIGHT`], у ручных — с начала суток UTC
/// [`PROBES_PER_DAY_MANUAL`].
pub fn quota(origin: Origin, now: Timestamp, w: &WorksSection) -> (Timestamp, u32) {
    match origin {
        Origin::Nightly => (current_night(now, w).1, PROBES_PER_NIGHT),
        Origin::Manual => (zam_store::time::day_start_utc(now), PROBES_PER_DAY_MANUAL),
    }
}

/// Остаток пределов проб пользователя базы `db` в запуске постановки `origin` сейчас: новых
/// ([`quota`]) и активных ([`PROBES_MAX`]) — меньший. Ноль — ночь или ручной запуск вида
/// кандидатов не ставит.
pub async fn room(st: &AppState, db: &Db, origin: Origin) -> Result<usize, StoreError> {
    let (since, cap) = quota(origin, zam_store::time::now(), &st.cfg.works);
    let made = zam_store::probes::new_probes_since(db, origin.as_str(), since).await?;
    let active = zam_store::probes::active_targeted(db).await?;
    Ok(cap
        .saturating_sub(made)
        .min(PROBES_MAX.saturating_sub(active)) as usize)
}

/// Причина отказа элемента при сбое хранилища во время проверки: работа закрывается с прочими
/// элементами (второй итог — 409), а отклонённая запись в отсев не попадает — вернётся в одну из
/// следующих работ (ревью r1, M4).
pub const REASON_STORE: &str =
    "временный сбой хранилища при проверке — запись вернётся в одну из следующих работ";

/// Сбой хранилища при проверке элемента — отказ элемента ([`REASON_STORE`]).
fn store_reject(e: StoreError) -> Reject {
    tracing::warn!(error = %e, "внутренние работы: проверка пробы не дочитала базу");
    Reject::new(REASON_STORE)
}

/// Проба из записи итога (только `ItemWrite::Probe`).
fn probe_of(w: &ItemWrite) -> Option<&NewProbe> {
    match w {
        ItemWrite::Probe { probe, .. } => Some(probe),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Проверка элемента итога `{target|n, question}` номер `n` (общие проверки — номер, цель из
/// пакета, маска, явный `skip` — уже прошли); `prior` — записи принятых раньше элементов этого
/// итога (повтор вопроса, проб на запись, пределы). Принятый — одна проба ([`ItemWrite::Probe`]).
pub async fn accept(
    ctx: &ItemCtx<'_>,
    n: usize,
    item: &Value,
    prior: &[ItemWrite],
) -> Result<Vec<ItemWrite>, Reject> {
    let Some(it) = ctx.work.item(n) else {
        return Err(Reject::new("нет элемента с таким n в пакете"));
    };
    let question = match item.get("question") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim(),
        _ => {
            return Err(Reject::new(
                "нет question: пробный вопрос к записи (или skip с причиной)",
            ));
        }
    };
    let probes: Vec<&NewProbe> = prior.iter().filter_map(probe_of).collect();
    let same = probes
        .iter()
        .filter(|p| p.targets.first().is_some_and(|t| t.target == it.target))
        .count();
    let cap = questions_for(it);
    if same >= cap {
        return Err(Reject::new(if cap == 1 {
            REASON_ONE_PER_RECORD
        } else {
            REASON_TWO_PER_RECORD
        }));
    }
    let overlap = check_question(question, &it.title)?;
    let norm = normalize_query(question);
    if probes.iter().any(|p| normalize_query(&p.question) == norm)
        || zam_store::probes::question_taken(ctx.db, ctx.work.space_id, question)
            .await
            .map_err(store_reject)?
    {
        return Err(Reject::new(REASON_DUPLICATE));
    }
    let now = zam_store::time::now();
    let (since, limit) = quota(ctx.work.origin, now, &ctx.st.cfg.works);
    let made = zam_store::probes::new_probes_since(ctx.db, ctx.work.origin.as_str(), since)
        .await
        .map_err(store_reject)?;
    if made as usize + probes.len() >= limit as usize {
        return Err(Reject::new(match ctx.work.origin {
            Origin::Nightly => {
                format!(
                    "предел новых проб за ночь — {PROBES_PER_NIGHT}: запись получит пробу позже"
                )
            }
            Origin::Manual => {
                format!("предел новых проб ручных запусков — {PROBES_PER_DAY_MANUAL} за сутки UTC")
            }
        }));
    }
    let active = zam_store::probes::active_targeted(ctx.db)
        .await
        .map_err(store_reject)?;
    if active as usize + probes.len() >= PROBES_MAX as usize {
        return Err(Reject::new(format!(
            "активных проб — {PROBES_MAX}: новые не принимаются"
        )));
    }
    let (author_kind, author) = match ctx.author {
        Author::Agent(s) => (ProbeAuthor::Agent, s.clone()),
        Author::Model(_) => (ProbeAuthor::Model, ctx.author.label()),
    };
    Ok(vec![ItemWrite::Probe {
        probe: NewProbe {
            space: Some(ctx.work.space_id),
            question: question.to_string(),
            kind: ProbeKind::Probe,
            targets: vec![ProbeTarget {
                target: it.target.clone(),
                rev: it.rev,
            }],
            split: zam_search::eval::probe_split(&it.target),
            overlap: Some(overlap),
            author_kind,
            author: Some(author),
            work_id: Some(ctx.work.id),
        },
        since,
        cap: limit,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Инструкции вида — не длиннее 300 токенов (§6.3 п. 3), обе называют сигнал длинной записи.
    #[test]
    fn instruction_short() {
        for s in [INSTRUCTION_RU, INSTRUCTION_EN] {
            let t = zam_store::count_tokens(s);
            assert!(t <= 300, "{t} токенов: {s}");
            assert!(s.contains("«вопросов: 2»"), "{s}");
        }
    }

    /// Путь, ссылка, дата, URL — границы распознавания.
    #[test]
    fn text_patterns() {
        assert!(has_path("где лежит src/main.rs сейчас"));
        assert!(has_path("файл C:\\x\\y.txt"));
        assert!(has_path("(config/.env)"));
        assert!(!has_path("и/или 1/2 вариант"));
        assert!(has_ref("ссылка i:01M7Z8KVMZEMRB634C4VMPY7FQ в тексте"));
        assert!(!has_ref("ссылка i:01M7Z8KVMZEMRB634C4VMPY7F в тексте"));
        assert!(!has_ref("время 10:30 и счёт 2:1"));
        assert!(has_date("с 2026-11-23 года"));
        assert!(!has_date("версия 12026-11-233"));
        assert!(has_url("см. https://x"));
        assert!(has_url("сайт www.example.com"));
        assert!(!has_url("слово wwwx"));
    }

    /// Ночь сборщика — последний слот не позже «сейчас».
    #[test]
    fn current_night_last_slot() {
        let w = WorksSection {
            at: "04:30".into(),
            tz: "UTC".into(),
            ..WorksSection::default()
        };
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        assert_eq!(
            current_night(at("2026-11-23T05:00:00Z"), &w),
            (civil::date(2026, 11, 23), at("2026-11-23T04:30:00Z"))
        );
        assert_eq!(
            current_night(at("2026-11-23T03:00:00Z"), &w),
            (civil::date(2026, 11, 22), at("2026-11-22T04:30:00Z"))
        );
    }
}
