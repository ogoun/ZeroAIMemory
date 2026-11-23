//! Прогон ночной проверки памяти пользователя (M7.3 §6.5 пп. 1–4, 6).
//!
//! - **Тем же путём, без следов.** Каждая проба — `/search mode=brief` ([`search_with`]) и
//!   `/recall(prompt)` ([`recall_with`]) изнутри процесса с [`Accounting::Off`]; принципал — агент
//!   пользователя без сессии (`ro`, маска `agent_view`); решение — пространство пробы
//!   (`X-Zam-Solution`; у Knowledge и Rules — без решения).
//! - **Основной поиск — с умолчаниями агента** (`top_k`, `search.budget_tokens`, реранк — как у
//!   агента без параметров): ранг цели — номер карточки в бюджете выдачи, карточка `[cache]` с
//!   целью в источниках — попадание на её месте; `search_cut` — карточек в бюджете. **Глубокий
//!   поиск** (`top_k = 50`, бюджет без среза) — только для проб вне бюджета: различает `none` и
//!   `beyond`, в задержки не входит.
//! - **Посторонние вопросы** — подсказками к запросу в области каждого решения пользователя, у
//!   которого есть пробы, и без решения, если таких решений нет; `hooks.prompt = false` у
//!   пользователя — подсказки не считаются (`hit_prompt`, `negatives_shown` — `null`), посторонние
//!   вопросы не идут.
//! - **Пределы**: пробы последовательно с паузой [`super::PROBE_PAUSE`]; прогон — не дольше
//!   `check_max_min` (превышен — `timeout`, метрики по пройденным, без вердикта); идёт
//!   переиндексация — `skipped`; копия, начавшаяся посреди прогона, — флаг `backup_overlap`
//!   (задержки этого прогона не сравниваются); сервер останавливается — `failed`
//!   (`interrupted`).
//! - **Отпечаток** `fp_hash` — модель эмбеддингов и устройство (без загруженной модели — как у
//!   прошлого прогона: деградация — не смена отпечатка), реранкер, режим Qdrant, `check_version`;
//!   **контекст** — версия сервера и хэши секций `[search]`, `[recall]`.
//! - **Провалы** (до [`FAILING_MAX`]) с советом «что чинить» — в `metrics.failing`: данные отчёта
//!   и будущей уборки (M7.5).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use jiff::civil;
use serde_json::{Value, json};
use zam_core::Id;
use zam_search::eval::{ProbeObs, Split, check_metrics, overlap};
use zam_store::check::{CheckResult, CheckRun, RunFinish, RunStatus, Trigger};
use zam_store::probes::{Probe, ProbeKind, ProbeTarget};
use zam_store::{Access, AuthToken, Db, EntryKind, Space, SpaceKind, StoreError};

use super::verdict::{self, Key, Kind, Outcome, RunMetrics, RunSummary, VerdictCtx};
use super::{Accounting, CHECK_VERSION, PROBE_PAUSE};
use crate::api::recall::recall_with;
use crate::api::search::{SOLUTION_HEADER, search_with};
use crate::app::AppState;
use crate::auth::Principal;
use crate::works::NightCtx;

/// `top_k` глубокого поиска.
pub const DEEP_TOP_K: usize = 50;
/// Бюджет глубокого поиска, токенов: выдача не срезается бюджетом.
pub const DEEP_BUDGET: usize = 200_000;
/// Проваленных проб с советом в метриках прогона.
pub const FAILING_MAX: usize = 20;
/// Прежних ночных прогонов, из которых собирается база.
pub const HISTORY_RUNS: usize = 60;
/// Флаг прогона: копия началась посреди прогона.
pub const FLAG_BACKUP_OVERLAP: &str = "backup_overlap";
/// Флаг прогона: поиск шёл без векторов.
pub const FLAG_DEGRADED: &str = "degraded";

/// Совет к проваленной пробе: цель не нашлась и в глубоком поиске.
pub const ADVICE_TERM: &str = "term";
/// Совет: цель нашлась в глубоком поиске за границей бюджета.
pub const ADVICE_RANKING: &str = "ranking";
/// Совет: выше цели — запись того же вида с похожим заголовком.
pub const ADVICE_NEIGHBOR: &str = "neighbor";
/// Совет: к постороннему вопросу показаны подсказки.
pub const ADVICE_PROMPT_THRESHOLD: &str = "prompt_threshold";

/// Свойства пробы для исходов вердикта.
#[derive(Debug, Clone, Copy)]
struct Info {
    split: Split,
    negative: bool,
    hintable: bool,
}

/// Как закончилось тело прогона.
enum End {
    Done,
    /// Сервер останавливается: прогон — `failed` (`interrupted`).
    Stopped,
}

/// Прогон `id` пользователя `user` (строка `running` пишется здесь): итог — в `check_runs` и
/// `check_results`.
pub(crate) async fn execute(
    ctx: &NightCtx,
    user: Id,
    trigger: Trigger,
    night: Option<civil::Date>,
    id: Id,
) -> Result<Id, StoreError> {
    let st = ctx.st();
    let db = st.users.get(user).await?;
    super::ensure_marked(st, user, &db).await?;
    zam_store::check::start_run(&db, id, trigger, night, zam_store::time::now()).await?;
    let end = body(ctx, &db, user, trigger, id).await;
    let error = match &end {
        Ok(End::Done) => return Ok(id),
        Ok(End::Stopped) => zam_store::check::ERROR_INTERRUPTED.to_string(),
        Err(e) => e.to_string(),
    };
    let f = RunFinish {
        status: RunStatus::Failed,
        fingerprint: None,
        fp_hash: None,
        context: None,
        metrics: None,
        verdict: None,
        flags: Vec::new(),
        episode: None,
        error: Some(error),
    };
    if let Err(e) = zam_store::check::finish_run(&db, id, f, Vec::new()).await {
        tracing::warn!(run = %id, error = %e, "проверка памяти: итог прогона не записан");
    }
    end.map(|_| id)
}

fn finish_plain(status: RunStatus, error: Option<String>) -> RunFinish {
    RunFinish {
        status,
        fingerprint: None,
        fp_hash: None,
        context: None,
        metrics: None,
        verdict: None,
        flags: Vec::new(),
        episode: None,
        error,
    }
}

/// Агент пользователя без сессии: токен `ro` (маска `agent_view`); токена в базе нет — его id
/// никуда не пишется.
fn agent(user: Id) -> Principal {
    Principal::Token(AuthToken {
        token_id: Id::new(),
        user_id: user,
        access: Access::Ro,
    })
}

fn reindexing(st: &AppState) -> bool {
    #[cfg(feature = "test-hooks")]
    if let Some(v) = *super::test_hooks::lock(&super::test_hooks::REINDEXING) {
        return v;
    }
    st.index.reindexing()
}

fn max_run(st: &AppState) -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = *super::test_hooks::lock(&super::test_hooks::MAX_RUN) {
        return d;
    }
    Duration::from_secs(60 * u64::from(st.cfg.works.check_max_min))
}

fn pause() -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = *super::test_hooks::lock(&super::test_hooks::PAUSE) {
        return d;
    }
    PROBE_PAUSE
}

fn degraded_of(v: &Value) -> bool {
    #[cfg(feature = "test-hooks")]
    if let Some(d) = *super::test_hooks::lock(&super::test_hooks::DEGRADED) {
        return d;
    }
    v["degraded"].as_bool() == Some(true)
}

fn version(st: &AppState) -> String {
    #[cfg(feature = "test-hooks")]
    if let Some(v) = super::test_hooks::lock(&super::test_hooks::VERSION).clone() {
        return v;
    }
    st.version.to_string()
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Первые 16 знаков blake3 JSON.
fn hash16(v: &Value) -> String {
    blake3::hash(v.to_string().as_bytes()).to_hex()[..16].to_string()
}

/// Отпечаток прогона: модель и устройство (без загруженной модели — как у прошлого прогона
/// `prev`), реранкер, режим Qdrant, версия правил проверки.
fn fingerprint(st: &AppState, prev: Option<&Value>) -> Value {
    let was = |k: &str| prev.and_then(|p| p[k].as_str()).map(str::to_string);
    let model = st
        .index
        .model_id()
        .map(str::to_string)
        .or_else(|| was("model"))
        .unwrap_or_else(|| "none".into());
    let device = st.index.device.as_ref().map(|d| d.device);
    let device_s = device
        .map(|d| d.to_string())
        .or_else(|| was("device"))
        .unwrap_or_else(|| "none".into());
    json!({
        "model": model,
        "device": device_s,
        "reranker": st.rerank.model_for(device),
        "qdrant": st.cfg.qdrant.mode,
        "check_version": CHECK_VERSION,
    })
}

/// Контекст прогона: версия сервера и хэши итоговых секций `[search]`, `[recall]`.
fn context(st: &AppState) -> Value {
    json!({
        "server": version(st),
        "search": hash16(&json!(st.cfg.search)),
        "recall": hash16(&json!(st.cfg.recall)),
    })
}

/// Id записей карточки выдачи: у выжимки кэша — источники действующей выжимки.
fn card_ids(it: &Value) -> Vec<Id> {
    if it["kind"] == "cache" {
        if it["state"] != "valid" {
            return Vec::new();
        }
        return it["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["id"].as_str())
            .filter_map(|r| r.parse::<zam_core::TargetRef>().ok())
            .map(|t| t.id)
            .collect();
    }
    ["entry_id", "node_id"]
        .iter()
        .filter_map(|k| it[*k].as_str())
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// Место (с 1) первой карточки `items` с целью из `targets`.
fn rank_of(items: &[Value], targets: &HashSet<Id>) -> Option<u32> {
    items
        .iter()
        .position(|it| card_ids(it).iter().any(|i| targets.contains(i)))
        .map(|i| u32::try_from(i + 1).unwrap_or(u32::MAX))
}

/// Ссылка карточки: запись — по букве вида, заметка — `n:`.
fn card_ref(it: &Value) -> Option<String> {
    if let Some(e) = it["entry_id"].as_str() {
        let letter = it["kind"]
            .as_str()
            .and_then(EntryKind::parse)
            .map_or("e", EntryKind::ref_letter);
        return Some(format!("{letter}:{e}"));
    }
    it["node_id"].as_str().map(|n| format!("n:{n}"))
}

/// Заголовок карточки: у записи — `title`, у заметки — путь.
fn card_title(it: &Value) -> String {
    it["title"]
        .as_str()
        .or_else(|| it["path"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Соседняя запись выше цели глубокого поиска: того же вида, заголовок похож (доля слов > 0,5).
fn neighbor(items: &[Value], rank: u32) -> Option<String> {
    let at = usize::try_from(rank).ok()?.checked_sub(1)?;
    let target = items.get(at)?;
    let title = card_title(target);
    items[..at]
        .iter()
        .filter(|it| it["kind"] == target["kind"] && it["kind"] != "cache")
        .find(|it| overlap(&card_title(it), &title) > 0.5)
        .and_then(card_ref)
}

/// Внутренний запрос поиска: элементы выдачи и признак «без векторов»; сбой — пустая выдача
/// (WARN без текста запроса).
async fn search(
    st: &AppState,
    p: &Principal,
    guid: Option<&str>,
    body: Value,
) -> (Vec<Value>, bool) {
    let mut h = HeaderMap::new();
    if let Some(g) = guid.and_then(|g| HeaderValue::from_str(g).ok()) {
        h.insert(SOLUTION_HEADER, g);
    }
    let r = search_with(
        State(st.clone()),
        p.clone(),
        h,
        None,
        Ok(Bytes::from(body.to_string())),
        None,
        Accounting::Off,
    )
    .await;
    let resp = match r {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "проверка памяти: поиск пробы не удался");
            return (Vec::new(), false);
        }
        Err(e) => {
            tracing::warn!(error = %e, "проверка памяти: поиск пробы не удался");
            return (Vec::new(), false);
        }
    };
    let v: Value = match axum::body::to_bytes(resp.into_body(), usize::MAX).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    let items = v["items"].as_array().cloned().unwrap_or_default();
    (items, degraded_of(&v))
}

/// Подсказки к запросу: ссылки показанных подсказок по порядку; сбой — пусто (WARN).
async fn prompt(st: &AppState, p: &Principal, guid: Option<&str>, question: &str) -> Vec<String> {
    let mut body = json!({"event": "prompt", "prompt": question});
    if let Some(g) = guid {
        body["solution"] = json!(g);
    }
    let r = recall_with(
        State(st.clone()),
        p.clone(),
        HeaderMap::new(),
        None,
        Ok(Bytes::from(body.to_string())),
        Accounting::Off,
    )
    .await;
    match r {
        Ok(v) => v.0["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|it| it["id"].as_str().map(str::to_string))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "проверка памяти: подсказки к пробе не получены");
            Vec::new()
        }
    }
}

/// Запрос пробы — не длиннее предела запроса поиска.
fn query_of(q: &str) -> String {
    q.chars().take(zam_store::search::QUERY_MAX_CHARS).collect()
}

/// Цель может прийти подсказкой: insight или заметка Rules (ТЗ §10.3).
fn hintable(p: &Probe, rules: Option<Id>) -> bool {
    p.targets
        .iter()
        .any(|t| t.letter() == "i" || (t.letter() == "n" && p.space.is_some() && p.space == rules))
}

/// Пауза между пробами; `false` — сервер останавливается.
async fn rest(ctx: &NightCtx, d: Duration) -> bool {
    tokio::select! {
        () = ctx.stopped() => false,
        () = tokio::time::sleep(d) => true,
    }
}

fn as_u32(v: Option<f64>) -> Option<u32> {
    v.map(|x| x.round().clamp(0.0, f64::from(u32::MAX)) as u32)
}

async fn body(
    ctx: &NightCtx,
    db: &Db,
    user: Id,
    trigger: Trigger,
    id: Id,
) -> Result<End, StoreError> {
    let st = ctx.st();
    // выдача неполна — сравнение бессмысленно (§6.5 п. 1)
    if reindexing(st) {
        zam_store::check::finish_run(db, id, finish_plain(RunStatus::Skipped, None), Vec::new())
            .await?;
        return Ok(End::Done);
    }
    zam_store::probes::retire_gone(db).await?;
    zam_store::probes::sync_negatives(db, super::negatives::set()).await?;
    let probes = zam_store::probes::active_probes(db).await?;
    let spaces: HashMap<Id, Space> = zam_store::list_spaces(db)
        .await?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    let rules = spaces
        .values()
        .find(|s| s.kind == SpaceKind::Rules)
        .map(|s| s.id);
    let prompt_on = match crate::api::user_settings::cached_user_flags(st, user).await {
        Ok(f) => f.hooks.enabled("prompt"),
        Err(e) => {
            tracing::warn!(error = %e, "проверка памяти: настройки пользователя не прочитаны");
            true
        }
    };
    let p = agent(user);
    let limit = Instant::now() + max_run(st);
    let pause = pause();
    let mut backup_overlap = false;
    let mut degraded = false;
    let mut timeout = false;
    let mut obs: Vec<ProbeObs> = Vec::new();
    let mut rows: Vec<CheckResult> = Vec::new();
    let mut failing: Vec<Value> = Vec::new();
    let mut info: HashMap<Id, Info> = HashMap::new();
    let mut solutions: BTreeSet<Id> = BTreeSet::new();
    for pr in probes.iter().filter(|p| p.kind != ProbeKind::Negative) {
        if ctx.stopping() {
            return Ok(End::Stopped);
        }
        if Instant::now() >= limit {
            timeout = true;
            break;
        }
        // копия посреди прогона (ночной прогон её дождался; у ручного она может идти и с начала)
        backup_overlap |= st.backups.state().running.is_some();
        let space = pr.space.and_then(|s| spaces.get(&s));
        let guid = space.and_then(|s| s.solution_guid.as_deref());
        if let (Some(s), Some(_)) = (space, guid) {
            solutions.insert(s.id);
        }
        let targets: HashSet<Id> = pr.targets.iter().filter_map(ProbeTarget::id).collect();
        let q = query_of(&pr.question);
        let t = Instant::now();
        let (items, deg) = search(
            st,
            &p,
            guid,
            json!({"query": q, "mode": "brief", "format": "json"}),
        )
        .await;
        let search_ms = ms_since(t);
        degraded |= deg;
        let rank = rank_of(&items, &targets);
        let mut deep_rank = None;
        let mut near = None;
        if rank.is_none() {
            let (deep, _) = search(
                st,
                &p,
                guid,
                json!({
                    "query": q, "mode": "brief", "format": "json", "top_k": DEEP_TOP_K,
                    "budget_tokens": DEEP_BUDGET, "brief_min_rel": 0.0, "no_cache": true,
                }),
            )
            .await;
            deep_rank = rank_of(&deep, &targets);
            near = deep_rank.and_then(|r| neighbor(&deep, r));
        }
        let hint = hintable(pr, rules);
        let (prompt_rank, prompt_ms) = if prompt_on {
            let t = Instant::now();
            let refs = prompt(st, &p, guid, &q).await;
            let r = refs
                .iter()
                .position(|r| {
                    r.split_once(':')
                        .and_then(|(_, id)| id.parse::<Id>().ok())
                        .is_some_and(|i| targets.contains(&i))
                })
                .map(|i| u32::try_from(i + 1).unwrap_or(u32::MAX));
            (r, Some(ms_since(t)))
        } else {
            (None, None)
        };
        info.insert(
            pr.id,
            Info {
                split: pr.split,
                negative: false,
                hintable: hint,
            },
        );
        obs.push(ProbeObs {
            split: pr.split,
            negative: false,
            hintable: hint,
            overlap: pr.overlap,
            search_rank: rank,
            deep_rank,
            prompt_rank,
            search_ms: Some(search_ms),
            prompt_ms,
        });
        rows.push(CheckResult {
            probe_id: pr.id,
            space: pr.space,
            search_rank: rank,
            search_cut: u32::try_from(items.len()).ok(),
            deep_rank,
            prompt_rank,
            search_ms: as_u32(Some(search_ms)),
            prompt_ms: as_u32(prompt_ms),
        });
        if rank.is_none() && failing.len() < FAILING_MAX {
            let advice = match (deep_rank, &near) {
                (None, _) => ADVICE_TERM,
                (Some(_), Some(_)) => ADVICE_NEIGHBOR,
                (Some(_), None) => ADVICE_RANKING,
            };
            let mut f = json!({
                "probe": pr.reference(),
                "question": pr.question,
                "kind": pr.kind.as_str(),
                "split": pr.split.as_str(),
                "target": pr.targets.first().map(|t| t.target.clone()),
                "rank": deep_rank,
                "advice": advice,
            });
            if let Some(n) = near {
                f["neighbor"] = json!(n);
            }
            failing.push(f);
        }
        if !rest(ctx, pause).await {
            return Ok(End::Stopped);
        }
    }
    // посторонние вопросы — в области каждого решения с пробами, без решения — если их нет
    if prompt_on && !timeout {
        let areas: Vec<Option<Id>> = if solutions.is_empty() {
            vec![None]
        } else {
            solutions.iter().copied().map(Some).collect()
        };
        'negatives: for area in areas {
            let guid = area
                .and_then(|a| spaces.get(&a))
                .and_then(|s| s.solution_guid.as_deref());
            for np in probes.iter().filter(|p| p.kind == ProbeKind::Negative) {
                if ctx.stopping() {
                    return Ok(End::Stopped);
                }
                if Instant::now() >= limit {
                    timeout = true;
                    break 'negatives;
                }
                backup_overlap |= st.backups.state().running.is_some();
                let t = Instant::now();
                let shown = prompt(st, &p, guid, &query_of(&np.question)).await.len();
                let ms = ms_since(t);
                let n = u32::try_from(shown).unwrap_or(u32::MAX);
                info.insert(
                    np.id,
                    Info {
                        split: np.split,
                        negative: true,
                        hintable: false,
                    },
                );
                obs.push(ProbeObs {
                    split: np.split,
                    negative: true,
                    hintable: false,
                    overlap: None,
                    search_rank: None,
                    deep_rank: None,
                    prompt_rank: Some(n),
                    search_ms: None,
                    prompt_ms: Some(ms),
                });
                rows.push(CheckResult {
                    probe_id: np.id,
                    space: area,
                    search_rank: None,
                    search_cut: None,
                    deep_rank: None,
                    prompt_rank: Some(n),
                    search_ms: None,
                    prompt_ms: as_u32(Some(ms)),
                });
                if n > 0 && failing.len() < FAILING_MAX {
                    failing.push(json!({
                        "probe": np.reference(),
                        "question": np.question,
                        "kind": np.kind.as_str(),
                        "split": np.split.as_str(),
                        "shown": n,
                        "advice": ADVICE_PROMPT_THRESHOLD,
                    }));
                }
                if !rest(ctx, pause).await {
                    return Ok(End::Stopped);
                }
            }
        }
    }
    let m = check_metrics(&obs, prompt_on);
    let mut metrics = serde_json::to_value(&m).unwrap_or(Value::Null);
    metrics["degraded"] = json!(degraded);
    metrics["prompt_on"] = json!(prompt_on);
    metrics["failing"] = json!(failing);
    let mut flags = Vec::new();
    if backup_overlap {
        flags.push(FLAG_BACKUP_OVERLAP.to_string());
    }
    if degraded {
        flags.push(FLAG_DEGRADED.to_string());
    }
    let history = zam_store::check::nightly_done(db, HISTORY_RUNS).await?;
    let fp = fingerprint(st, history.first().and_then(|r| r.fingerprint.as_ref()));
    let fp_hash = hash16(&fp);
    let ctx_v = context(st);
    let mut f = RunFinish {
        status: if timeout {
            RunStatus::Timeout
        } else {
            RunStatus::Done
        },
        fingerprint: Some(fp.clone()),
        fp_hash: Some(fp_hash.clone()),
        context: Some(ctx_v.clone()),
        metrics: Some(metrics),
        verdict: None,
        flags,
        episode: None,
        error: None,
    };
    if !timeout {
        let cur = RunMetrics {
            id,
            fp_hash: fp_hash.clone(),
            fingerprint: fp,
            context: ctx_v,
            probes: outcomes(&rows, &info, prompt_on),
            p95: m.search_p95_ms,
            degraded,
            backup_overlap,
        };
        let prev = summaries(db, &history, &info, &fp_hash).await?;
        let vctx = VerdictCtx {
            nightly: trigger == Trigger::Nightly,
            version: version(st),
        };
        let v = verdict::decide(&cur, &prev, &vctx);
        f.episode = v.episode;
        f.verdict = Some(v.to_json());
    }
    zam_store::check::finish_run(db, id, f, rows).await?;
    Ok(End::Done)
}

/// Исходы проб прогона по строкам результатов (пробы вне `info` — снятые — не входят).
fn outcomes(
    rows: &[CheckResult],
    info: &HashMap<Id, Info>,
    prompt_on: bool,
) -> HashMap<Key, Outcome> {
    rows.iter()
        .filter_map(|r| {
            let i = info.get(&r.probe_id)?;
            Some((
                (r.probe_id, r.space),
                Outcome {
                    split: i.split,
                    negative: i.negative,
                    found: r.search_rank.is_some(),
                    prompt_hit: (prompt_on && i.hintable && !i.negative)
                        .then_some(r.prompt_rank.is_some()),
                    neg_shown: (prompt_on && i.negative).then_some(r.prompt_rank.unwrap_or(0) > 0),
                },
            ))
        })
        .collect()
}

/// Сводка прежнего прогона без исходов проб.
fn summary_head(r: &CheckRun) -> RunSummary {
    let v = r.verdict.clone().unwrap_or(Value::Null);
    RunSummary {
        id: r.id,
        fp_hash: r.fp_hash.clone().unwrap_or_default(),
        fingerprint: r.fingerprint.clone().unwrap_or(Value::Null),
        context: r.context.clone().unwrap_or(Value::Null),
        kind: v["kind"].as_str().and_then(Kind::parse),
        episode: r.episode,
        accepted: r.accepted_at.is_some(),
        after_update: v["after_update"].as_bool().unwrap_or(false),
        after_update_version: v["after_update_version"].as_str().map(str::to_string),
        degraded: r.flags.iter().any(|f| f == FLAG_DEGRADED),
        backup_overlap: r.flags.iter().any(|f| f == FLAG_BACKUP_OVERLAP),
        p95: r.metrics.as_ref().and_then(|m| m["search_p95_ms"].as_f64()),
        probes: HashMap::new(),
    }
}

/// Прежние законченные ночные прогоны (новые первыми) с исходами проб у тех, что нужны вердикту:
/// база текущего отпечатка, база отпечатка последнего прогона и сам последний.
async fn summaries(
    db: &Db,
    history: &[CheckRun],
    info: &HashMap<Id, Info>,
    fp: &str,
) -> Result<Vec<RunSummary>, StoreError> {
    let mut v: Vec<RunSummary> = history.iter().map(summary_head).collect();
    let mut need: Vec<usize> = verdict::base_of(&v, fp);
    if let Some(l) = v.first() {
        need.extend(verdict::base_of(&v, &l.fp_hash));
        need.push(0);
    }
    need.sort_unstable();
    need.dedup();
    let ids: Vec<Id> = need.iter().map(|i| v[*i].id).collect();
    let mut res = zam_store::check::results_of(db, ids).await?;
    for i in need {
        let prompt_on = history[i]
            .metrics
            .as_ref()
            .and_then(|m| m["prompt_on"].as_bool())
            .unwrap_or(false);
        let rows = res.remove(&v[i].id).unwrap_or_default();
        v[i].probes = outcomes(&rows, info, prompt_on);
    }
    Ok(v)
}

/// Совет к проваленной пробе или к прогону — текст (`zam check report`, WebUI).
pub fn advice_text(code: &str, version: Option<&str>) -> String {
    match code {
        ADVICE_TERM => {
            "нет слов спрашивающего: термин или псевдоним (term), дописать запись".into()
        }
        ADVICE_RANKING => "ранжирование: запись есть, но ниже предела".into(),
        ADVICE_NEIGHBOR => "соседняя запись: supersede или слияние".into(),
        ADVICE_PROMPT_THRESHOLD => "порог recall.prompt_min_score".into(),
        "latency" => "задержка".into(),
        "index" => "проверьте индекс: GET /index/status, zam-server reindex".into(),
        "after_update" => format!(
            "ухудшение после обновления до {}: сообщите разработчику",
            version.unwrap_or("?")
        ),
        other => other.into(),
    }
}

/// Советы к прогону (§6.5 п. 6): по флагам вердикта и метрикам — коды и тексты.
pub fn run_advice(verdict: Option<&Value>, metrics: Option<&Value>) -> Vec<Value> {
    let mut codes: Vec<&str> = Vec::new();
    let flags: Vec<&str> = verdict
        .and_then(|v| v["flags"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|f| f["name"].as_str())
        .collect();
    let degraded = metrics.is_some_and(|m| m["degraded"] == true);
    if degraded || flags.contains(&"lost_steady") || flags.contains(&"degraded") {
        codes.push("index");
    }
    if flags.contains(&"p95") {
        codes.push("latency");
    }
    if flags.contains(&"negatives_shown") {
        codes.push(ADVICE_PROMPT_THRESHOLD);
    }
    let after = verdict.filter(|v| v["after_update"] == true);
    if after.is_some() {
        codes.push("after_update");
    }
    let share = |k: &str| metrics.and_then(|m| m["all"][k].as_f64()).unwrap_or(0.0);
    if share("none") > 0.0 {
        codes.push(ADVICE_TERM);
    }
    if share("beyond") > 0.0 {
        codes.push(ADVICE_RANKING);
    }
    let version = after.and_then(|v| v["after_update_version"].as_str());
    codes
        .into_iter()
        .map(|c| json!({"code": c, "text": advice_text(c, version)}))
        .collect()
}
