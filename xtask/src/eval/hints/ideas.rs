//! Эталон подсказок-идей M5.7 (`cargo xtask eval --idea-hints <набор>`, задача 1a2 M6;
//! спецификация M6 §16 «Долги M5.7», эквивалентность задач 9 и 16).
//!
//! Набор `eval/idea-hints.toml` — запросы `prompt` ([`IdeaQuery`]) к 50 идеям корпуса ТС-2
//! (`eval/sessions-corpus/ideas/ideas.toml`). Прогон поднимает сервер без моделей (FTS — как
//! тест `ideas_recall`, где эталон сверяется), загружает идеи через API ([`load_plan`]: переходы
//! состояний с `by: human`, горизонт `horizon_days`) и для каждого запроса в **своей** сессии
//! повторяет `POST /recall {event: prompt}`, пока сервер отдаёт новые подсказки-идеи (показанное
//! сессии не повторяется), но не больше [`TOP`]: так при поставляемом `ideas.prompt_max = 1`
//! получается порядок топ-5 обоих проходов (кандидаты `new`/`deferred`, затем принятые со снятой
//! задачей). Ссылки `w:<id>` записываются ключами корпуса `idea:<key>` (id случайны у каждой
//! загрузки).
//!
//! «Наступивший» горизонт (`horizon_days = 0`) API не принимает (только с завтрашнего дня):
//! загрузчик ставит завтрашний, останавливает сервер, переносит `data.horizon` на сегодня (UTC)
//! прямым SQL — как тест `never_proposed_kinds_matrix` — и запускает сервер на том же каталоге.
//!
//! Эталон ([`IdeaHintsFixture`]) хранит blake3 файлов идей и набора ([`file_blake3`]): правка
//! любого после снятия ловит тест `idea_hints_fixture_is_wellformed` (`zam-server`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::eval::caps;
use crate::eval::corpus::EVAL_SOLUTION;
use crate::eval::run::{self, RunSpec, Session};
use crate::eval::sessions_corpus::{self, Idea, IdeaState, SessionsCorpus};
use crate::eval::{Device, EvalArgs, Paths};
use crate::smoke::{agent_with, api_with};

/// Ссылка на идею корпуса ТС-2 в наборе и эталоне: `idea:<key>`.
pub const IDEA_REF: &str = "idea:";
/// Подсказок-идей на запрос в эталоне, не больше («топ-5»).
pub const TOP: usize = 5;
/// Не меньше стольких запросов в наборе (спецификация M6 §16: ≥ 40).
pub const MIN_QUERIES: usize = 40;
/// Не меньше стольких положительных и отрицательных запросов.
pub const MIN_PER_CLASS: usize = 15;
/// Длинный запрос — больше стольких слов (запросы Claude Code).
pub const LONG_WORDS: usize = 30;
/// Не меньше стольких длинных запросов.
pub const MIN_LONG: usize = 4;
/// Конфигурация прогона — явно, а не умолчаниями сервера (ревью задачи 1a2, I-2): умолчания
/// v0.0.10-m5.7 для подсказок-идей и `recall.timeout_ms = 60000` (просрочка дала бы «тишину по
/// идеям» и нестабильный эталон). Тест эквивалентности (задачи 9, 16) ставит те же значения.
pub const RUN_CONFIG: [(&str, &str); 5] = [
    ("ideas.proactive", "true"),
    ("ideas.prompt_max", "1"),
    ("ideas.prompt_top_k", "5"),
    ("recall.prompt_min_score", "0.6"),
    ("recall.timeout_ms", "60000"),
];
/// Ключ M6 (§9, задача 9): у v0.0.10-m5.7 его нет — порог FTS-only там тот же
/// `recall.prompt_min_score`; в эталоне — `null`.
pub const ABSENT_IN_M57: &str = "recall.prompt_min_score_fts";
/// Предупреждение сервера о просрочке сбора `/recall`.
const WARN_RECALL_TIMEOUT: &str = "recall timeout";

/// Класс запроса набора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryClass {
    /// Автор ждёт подсказку (одну из `expect`).
    Positive,
    /// Автор ждёт тишину.
    Negative,
}

/// Запрос набора `eval/idea-hints.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeaQuery {
    pub id: String,
    pub prompt: String,
    pub class: QueryClass,
    /// Ожидание автора (справочно): идеи `idea:<key>`, любая из них.
    #[serde(default)]
    pub expect: Vec<String>,
    /// Идеи, на которые запрос похож, но которые подсказываться не должны.
    #[serde(default)]
    pub about: Vec<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueriesFile {
    #[serde(default)]
    query: Vec<IdeaQuery>,
}

/// Читает набор запросов.
pub fn load_queries(path: &Path) -> Result<Vec<IdeaQuery>> {
    let text = fs::read_to_string(path).with_context(|| format!("чтение {}", path.display()))?;
    let f: QueriesFile =
        toml::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok(f.query)
}

/// Итог проверки набора.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct QueriesSummary {
    pub queries: usize,
    pub positive: usize,
    pub negative: usize,
    pub long: usize,
    /// Состояния идей корпуса, затронутые `expect` и `about`.
    pub states: BTreeSet<String>,
}

fn state_name(s: IdeaState) -> &'static str {
    match s {
        IdeaState::New => "new",
        IdeaState::Deferred => "deferred",
        IdeaState::Proposed => "proposed",
        IdeaState::Declined => "declined",
        IdeaState::Accepted => "accepted",
        IdeaState::Released => "released",
        IdeaState::Done => "done",
    }
}

/// Идея может прийти подсказкой к `prompt` (M5.7): человека; `new`, `deferred` с наступившим
/// горизонтом или принятая со снятой задачей.
pub fn hint_candidate(i: &Idea) -> bool {
    i.origin == "human"
        && match i.state {
            IdeaState::New | IdeaState::Released => true,
            IdeaState::Deferred => i.horizon_days == Some(0),
            _ => false,
        }
}

/// Проверка набора против корпуса идей: объём, классы, длинные запросы, ссылки и все семь
/// состояний в `expect`/`about`; положительный ждёт только кандидатов подсказки.
pub fn validate_queries(qs: &[IdeaQuery], corpus: &SessionsCorpus) -> Result<QueriesSummary> {
    let ideas: BTreeMap<&str, &Idea> = corpus.ideas.iter().map(|i| (i.key.as_str(), i)).collect();
    let mut errs = Vec::new();
    let mut ids = BTreeSet::new();
    let mut s = QueriesSummary {
        queries: qs.len(),
        ..Default::default()
    };
    for q in qs {
        if !ids.insert(q.id.as_str()) {
            errs.push(format!("{}: повтор id", q.id));
        }
        if q.prompt.trim().is_empty() || q.prompt.contains('\r') {
            errs.push(format!("{}: пустой prompt или CRLF", q.id));
        }
        if q.prompt.split_whitespace().count() > LONG_WORDS {
            s.long += 1;
        }
        match q.class {
            QueryClass::Positive => {
                s.positive += 1;
                if q.expect.is_empty() {
                    errs.push(format!("{}: у positive пустой expect", q.id));
                }
            }
            QueryClass::Negative => {
                s.negative += 1;
                if !q.expect.is_empty() {
                    errs.push(format!("{}: у negative непустой expect", q.id));
                }
            }
        }
        for (field, refs) in [("expect", &q.expect), ("about", &q.about)] {
            for r in refs {
                let Some(i) = r.strip_prefix(IDEA_REF).and_then(|k| ideas.get(k)) else {
                    errs.push(format!("{}: {field} «{r}» — нет такой idea:<key>", q.id));
                    continue;
                };
                s.states.insert(state_name(i.state).to_string());
                if field == "expect" && !hint_candidate(i) {
                    errs.push(format!(
                        "{}: expect {r} — не кандидат подсказки ({}, {})",
                        q.id,
                        state_name(i.state),
                        i.origin
                    ));
                }
            }
        }
    }
    if s.queries < MIN_QUERIES {
        errs.push(format!("запросов {} < {MIN_QUERIES}", s.queries));
    }
    if s.positive < MIN_PER_CLASS || s.negative < MIN_PER_CLASS {
        errs.push(format!(
            "положительных {}, отрицательных {} — нужно по ≥ {MIN_PER_CLASS}",
            s.positive, s.negative
        ));
    }
    if s.long < MIN_LONG {
        errs.push(format!(
            "длинных запросов (> {LONG_WORDS} слов) {} < {MIN_LONG}",
            s.long
        ));
    }
    let all = [
        IdeaState::New,
        IdeaState::Deferred,
        IdeaState::Proposed,
        IdeaState::Declined,
        IdeaState::Accepted,
        IdeaState::Released,
        IdeaState::Done,
    ];
    let missing: Vec<&str> = all
        .iter()
        .map(|st| state_name(*st))
        .filter(|n| !s.states.contains(*n))
        .collect();
    if !missing.is_empty() {
        errs.push(format!("идеи состояний {missing:?} не затронуты набором"));
    }
    if !errs.is_empty() {
        bail!("набор подсказок-идей:\n  {}", errs.join("\n  "));
    }
    Ok(s)
}

/// blake3 файла (полные 64 знака) по правилу отпечатков M6: у текстовых файлов `\r\n` → `\n`
/// (не зависит от `core.autocrlf`).
pub fn file_blake3(path: &Path) -> Result<String> {
    let b = fs::read(path).with_context(|| format!("чтение {}", path.display()))?;
    let mut out = Vec::with_capacity(b.len());
    for (i, c) in b.iter().enumerate() {
        if *c == b'\r' && b.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(*c);
    }
    Ok(blake3::hash(&out).to_hex().to_string())
}

/// Шаг загрузки идеи через API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStep {
    /// `POST /spaces/{решение}/entries {kind: idea, data: {origin, priority}}`.
    Create,
    /// `POST /entries/{id}/propose {text}`.
    Propose,
    /// `POST /entries/{id}/decide {decision: accept, by: human}` — задача «начать».
    Accept,
    /// `decide decline, by: human`.
    Decline,
    /// `decide defer, by: human, horizon: сегодня + days` (API — не раньше завтра).
    Defer { days: u32 },
    /// `PATCH /entries/{задача}` с `If-Match`: `dropped` (с причиной) или `done`.
    TaskStatus(&'static str),
    /// Горизонт — сегодня (UTC): прямой SQL при остановленном сервере.
    DueToday,
}

/// Шаги загрузки идеи до её состояния ([`IdeaState`]).
pub fn load_plan(i: &Idea) -> Result<Vec<LoadStep>> {
    use LoadStep::*;
    if i.origin != "human" && i.state != IdeaState::New {
        bail!("idea {}: идея агента — только new", i.key);
    }
    Ok(match i.state {
        IdeaState::New => vec![Create],
        IdeaState::Proposed => vec![Create, Propose],
        IdeaState::Declined => vec![Create, Decline],
        IdeaState::Accepted => vec![Create, Accept],
        IdeaState::Released => vec![Create, Accept, TaskStatus("dropped")],
        IdeaState::Done => vec![Create, Accept, TaskStatus("done")],
        IdeaState::Deferred => match i.horizon_days {
            Some(0) => vec![Create, Defer { days: 1 }, DueToday],
            Some(d) => vec![Create, Defer { days: d }],
            None => bail!("idea {}: у deferred нужен horizon_days", i.key),
        },
    })
}

/// Строка эталона: запрос и подсказки-идеи по порядку (ключи корпуса).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaHintRow {
    pub id: String,
    pub query: String,
    pub ideas: Vec<String>,
}

/// Эталон подсказок-идей (`crates/zam-server/tests/fixtures/idea_hints_m57.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeaHintsFixture {
    pub server_version: String,
    pub server_git: Option<String>,
    pub date: String,
    /// blake3 `eval/sessions-corpus/ideas/ideas.toml` и набора ([`file_blake3`]).
    pub ideas_blake3: String,
    pub queries_blake3: String,
    /// Как снят: режим поиска, сессии, повторы, переопределения конфигурации.
    pub protocol: String,
    /// Действующие ключи прогона (`null` — ключа у сервера нет).
    pub config: BTreeMap<String, Value>,
    pub rows: Vec<IdeaHintRow>,
}

/// Ссылки `w:<id>` подсказок-идей ответа `/recall` (`items[].reason == "idea"`) по порядку.
pub fn idea_hints(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["reason"] == "idea")
        .filter_map(|i| i["id"].as_str().map(str::to_string))
        .collect()
}

/// Повторяет вызов `call` (один `/recall` сессии запроса), пока он отдаёт новые ссылки, не
/// больше [`TOP`]; ссылки переводятся в ключи `keys` (`w:<id>` → `idea:<key>`). Повтор уже
/// показанной ссылки или чужая ссылка — ошибка (показ сессии не учтён, эталон был бы ложным).
pub fn collect_top(
    keys: &BTreeMap<String, String>,
    mut call: impl FnMut() -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    while out.len() < TOP {
        let got = call()?;
        if got.is_empty() {
            break;
        }
        for w in got {
            let key = keys
                .get(&w)
                .with_context(|| format!("подсказка {w} — не идея корпуса"))?;
            if out.contains(key) {
                bail!("подсказка {key} повторилась в той же сессии — показ не учтён");
            }
            if out.len() < TOP {
                out.push(key.clone());
            }
        }
    }
    Ok(out)
}

/// Дата UTC через `days` дней, `YYYY-MM-DD`.
fn utc_date_plus(days: u32) -> Result<String> {
    let d = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(i64::from(days)))?;
    Ok(d.to_string())
}

/// `PATCH /entries/{id}` с `If-Match` текущей ревизии.
fn patch_entry(s: &Session, id: &str, body: &Value) -> Result<(u16, Value)> {
    let path = format!("/api/v1/entries/{id}");
    let (st, cur) = api_with(
        &s.base,
        s.token(),
        &path,
        None,
        &[],
        Duration::from_secs(30),
    )?;
    if st != 200 {
        bail!("GET {path}: HTTP {st}: {cur}");
    }
    let rev = cur["rev"].to_string();
    let mut r = agent_with(Duration::from_secs(30))
        .patch(&format!("{}{path}", s.base))
        .header("Authorization", &format!("Bearer {}", s.token()))
        .header("If-Match", &rev)
        .content_type("application/json")
        .send(body.to_string())
        .with_context(|| format!("PATCH {path}"))?;
    let st = r.status().as_u16();
    let text = r.body_mut().read_to_string()?;
    Ok((
        st,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    ))
}

/// POST `path` с телом; ожидаемый статус `want`, иначе ошибка с контекстом идеи.
fn post_ok(s: &Session, key: &str, path: &str, body: &Value, want: u16) -> Result<Value> {
    let (st, v) = api_with(
        &s.base,
        s.token(),
        path,
        Some(body),
        &[],
        Duration::from_secs(30),
    )?;
    if st != want {
        bail!("idea {key}: POST {path}: HTTP {st}: {v}");
    }
    Ok(v)
}

/// Загружает идеи корпуса в решение набора по [`load_plan`]: `w:<id>` → `idea:<key>` и id идей с
/// шагом [`LoadStep::DueToday`].
pub fn load_ideas(s: &Session, ideas: &[Idea]) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let reg = json!({ "agent_solution_id": EVAL_SOLUTION, "name": "eval" });
    let (st, v) = api_with(
        &s.base,
        s.token(),
        "/api/v1/solutions",
        Some(&reg),
        &[],
        Duration::from_secs(30),
    )?;
    if st != 201 && st != 200 {
        bail!("POST /solutions: HTTP {st}: {v}");
    }
    let mut keys = BTreeMap::new();
    let mut due = Vec::new();
    let feed = format!("/api/v1/spaces/{EVAL_SOLUTION}/entries");
    for i in ideas {
        let mut id = String::new();
        let mut task = String::new();
        for step in load_plan(i)? {
            let decide = |body: Value| {
                post_ok(
                    s,
                    &i.key,
                    &format!("/api/v1/entries/{id}/decide"),
                    &body,
                    200,
                )
            };
            match step {
                LoadStep::Create => {
                    let mut data = json!({ "origin": i.origin });
                    if let Some(p) = &i.priority {
                        data["priority"] = json!(p);
                    }
                    let body = json!({ "kind": "idea", "title": i.title, "body": i.body,
                                       "data": data });
                    let v = post_ok(s, &i.key, &feed, &body, 201)?;
                    id = v["id"]
                        .as_str()
                        .with_context(|| format!("idea {}: нет id в {v}", i.key))?
                        .to_string();
                }
                LoadStep::Propose => {
                    post_ok(
                        s,
                        &i.key,
                        &format!("/api/v1/entries/{id}/propose"),
                        &json!({ "text": "пора обсудить" }),
                        200,
                    )?;
                }
                LoadStep::Accept => {
                    let v = decide(json!({ "decision": "accept", "by": "human" }))?;
                    task = v["task"]["id"]
                        .as_str()
                        .with_context(|| format!("idea {}: нет задачи в {v}", i.key))?
                        .to_string();
                }
                LoadStep::Decline => {
                    decide(json!({ "decision": "decline", "by": "human" }))?;
                }
                LoadStep::Defer { days } => {
                    decide(json!({ "decision": "defer", "by": "human",
                                   "horizon": utc_date_plus(days)? }))?;
                }
                LoadStep::TaskStatus(status) => {
                    let mut body = json!({ "status": status });
                    if status == "dropped" {
                        body["reason"] = json!("не до того");
                    }
                    let (st, v) = patch_entry(s, &task, &body)?;
                    if st != 200 {
                        bail!("idea {}: задача {task} → {status}: HTTP {st}: {v}", i.key);
                    }
                }
                LoadStep::DueToday => due.push(id.clone()),
            }
        }
        keys.insert(format!("w:{id}"), format!("{IDEA_REF}{}", i.key));
    }
    Ok((keys, due))
}

/// Горизонт идей `ids` — сегодня (UTC), прямым SQL по базе пользователя (сервер остановлен).
fn set_horizon_today(data: &Path, ids: &[String]) -> Result<()> {
    let db = crate::eval::gate::user_db(data)?;
    let c = rusqlite::Connection::open(&db).with_context(|| db.display().to_string())?;
    let today = utc_date_plus(0)?;
    for id in ids {
        let n = c.execute(
            "UPDATE entries SET data = json_set(data, '$.horizon', ?2) WHERE id = ?1",
            rusqlite::params![id, today],
        )?;
        if n != 1 {
            bail!("горизонт идеи {id}: обновлено строк {n}");
        }
    }
    Ok(())
}

/// `cargo xtask eval --idea-hints <набор>`.
pub fn run(a: &EvalArgs, p: &Paths, queries_path: &Path) -> Result<()> {
    let queries = load_queries(queries_path)?;
    let corpus = sessions_corpus::load(&p.sessions_corpus)?;
    sessions_corpus::validate(&corpus)?;
    let qs = validate_queries(&queries, &corpus)?;
    let ideas_path = p.sessions_corpus.join("ideas").join("ideas.toml");
    println!(
        "набор подсказок-идей: запросов {} (положительных {}, отрицательных {}, длинных {}), \
         идей {}; поиск — FTS (без моделей)",
        qs.queries,
        qs.positive,
        qs.negative,
        qs.long,
        corpus.ideas.len()
    );
    let server = run::build_server(&p.repo, a.server_bin.as_deref())?;
    let sets: Vec<(String, String)> = RUN_CONFIG
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let spec = RunSpec {
        name: "idea-hints".into(),
        model: None,
        device: Device::Cpu,
        reranker: None,
        sets: sets.clone(),
        keep_cuda_path: false,
        ctrl_break: false,
    };
    let mut s = Session::start(p, &spec, &server, true)?;
    let caps = caps::probe(&s.base)?;
    if !caps.ideas {
        bail!("сервер {}: вида idea нет (M5.7+)", caps.tag());
    }
    let (keys, due) = load_ideas(&s, &corpus.ideas)?;
    if !due.is_empty() {
        let data = s.data.clone();
        s.stop()?;
        set_horizon_today(&data, &due)?;
        s = Session::start(p, &spec, &server, false)?;
    }
    // FTS записей — в транзакции записи: ждать нечего (векторные задания без модели стоят в
    // очереди и не выполняются)
    let mut rows = Vec::with_capacity(queries.len());
    for q in &queries {
        let session = format!("idea-hints-{}", q.id);
        let body = json!({ "event": "prompt", "solution": EVAL_SOLUTION, "prompt": q.prompt });
        let ideas = collect_top(&keys, || {
            let (st, v) = api_with(
                &s.base,
                s.token(),
                "/api/v1/recall",
                Some(&body),
                &[("X-Zam-Session", session.as_str())],
                Duration::from_secs(90),
            )?;
            if st != 200 {
                bail!("{}: POST /recall: HTTP {st}: {v}", q.id);
            }
            let timeout = v["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|w| w.as_str().is_some_and(|w| w.contains(WARN_RECALL_TIMEOUT)));
            if timeout {
                bail!("{}: {WARN_RECALL_TIMEOUT} — эталон был бы неполным", q.id);
            }
            Ok(idea_hints(&v))
        })
        .with_context(|| q.id.clone())?;
        rows.push(IdeaHintRow {
            id: q.id.clone(),
            query: q.prompt.clone(),
            ideas,
        });
    }
    s.stop()?;
    let fx = IdeaHintsFixture {
        server_version: caps.tag(),
        server_git: caps.git.clone(),
        date: run::today(),
        ideas_blake3: file_blake3(&ideas_path)?,
        queries_blake3: file_blake3(queries_path)?,
        protocol: format!(
            "FTS без моделей; решение {EVAL_SOLUTION}; идеи — через API (by: human), \
             horizon_days = 0 — SQL data.horizon = сегодня UTC; на запрос — своя сессия \
             X-Zam-Session idea-hints-<id>, POST /recall {{event: prompt}} повторяется, пока \
             приходят новые подсказки-идеи (items[].reason = idea), не больше {TOP}; \
             w:<id> → idea:<key>"
        ),
        config: run_config(),
        rows,
    };
    print_summary(&queries, &fx);
    let out = match &a.out {
        Some(o) => o.clone(),
        None => p.repo.join("eval").join("results").join(format!(
            "{}-idea-hints-srv{}.json",
            fx.date, fx.server_version
        )),
    };
    if let Some(d) = out.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(&out, serde_json::to_string_pretty(&fx)? + "\n")?;
    println!("  {}", out.display());
    Ok(())
}

/// Поле `config` эталона: [`RUN_CONFIG`] (числа и логические — JSON-значениями) и
/// [`ABSENT_IN_M57`] = `null`.
pub fn run_config() -> BTreeMap<String, Value> {
    let mut m: BTreeMap<String, Value> = RUN_CONFIG
        .iter()
        .map(|(k, v)| {
            let val = serde_json::from_str::<Value>(v).unwrap_or_else(|_| json!(v));
            (k.to_string(), val)
        })
        .collect();
    m.insert(ABSENT_IN_M57.to_string(), Value::Null);
    m
}

/// Сводка эталона против ожиданий автора (справочно).
fn print_summary(queries: &[IdeaQuery], fx: &IdeaHintsFixture) {
    let mut with = 0;
    let mut pos_hit = 0;
    let mut neg_silent = 0;
    let mut deep = 0;
    for (q, r) in queries.iter().zip(&fx.rows) {
        if !r.ideas.is_empty() {
            with += 1;
        }
        if r.ideas.len() > 1 {
            deep += 1;
        }
        let as_expected = match q.class {
            QueryClass::Positive => r.ideas.first().is_some_and(|f| q.expect.contains(f)),
            QueryClass::Negative => r.ideas.is_empty(),
        };
        match (q.class, as_expected) {
            (QueryClass::Positive, true) => pos_hit += 1,
            (QueryClass::Negative, true) => neg_silent += 1,
            _ => {}
        }
        let mark = if as_expected { "=" } else { "≠" };
        println!("  {} {mark} {:?} — {}", r.id, r.ideas, q.note);
    }
    let pos = queries
        .iter()
        .filter(|q| q.class == QueryClass::Positive)
        .count();
    println!(
        "сервер {}: запросов {}, с подсказками {with} (больше одной — {deep}); первая подсказка \
         из expect у {pos_hit} из {pos} положительных, тишина у {neg_silent} из {} \
         отрицательных",
        fx.server_version,
        fx.rows.len(),
        queries.len() - pos
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idea(key: &str, origin: &str, state: IdeaState, horizon: Option<u32>) -> Idea {
        Idea {
            key: key.into(),
            lang: crate::eval::corpus::Lang::Ru,
            title: format!("Идея {key}"),
            body: String::new(),
            priority: None,
            origin: origin.into(),
            state,
            horizon_days: horizon,
        }
    }

    #[test]
    fn load_plan_reaches_every_state() {
        use LoadStep::*;
        let cases = [
            (IdeaState::New, None, vec![Create]),
            (IdeaState::Proposed, None, vec![Create, Propose]),
            (IdeaState::Declined, None, vec![Create, Decline]),
            (IdeaState::Accepted, None, vec![Create, Accept]),
            (
                IdeaState::Released,
                None,
                vec![Create, Accept, TaskStatus("dropped")],
            ),
            (
                IdeaState::Done,
                None,
                vec![Create, Accept, TaskStatus("done")],
            ),
            (
                IdeaState::Deferred,
                Some(0),
                vec![Create, Defer { days: 1 }, DueToday],
            ),
            (
                IdeaState::Deferred,
                Some(30),
                vec![Create, Defer { days: 30 }],
            ),
        ];
        for (st, h, want) in cases {
            assert_eq!(
                load_plan(&idea("x", "human", st, h)).unwrap(),
                want,
                "{st:?}"
            );
        }
        assert!(load_plan(&idea("x", "human", IdeaState::Deferred, None)).is_err());
        assert!(load_plan(&idea("x", "agent", IdeaState::Accepted, None)).is_err());
        assert_eq!(
            load_plan(&idea("x", "agent", IdeaState::New, None)).unwrap(),
            vec![Create]
        );
    }

    #[test]
    fn collect_top_follows_session_and_maps_keys() {
        let keys: BTreeMap<String, String> = (1..=7)
            .map(|n| (format!("w:{n}"), format!("idea:i0{n}")))
            .collect();
        // по одной за вызов, затем тишина
        let mut answers = vec![vec![], vec!["w:3".to_string()], vec!["w:1".to_string()]];
        let got = collect_top(&keys, || Ok(answers.pop().unwrap())).unwrap();
        assert_eq!(got, ["idea:i01", "idea:i03"]);
        // не больше TOP, даже если сервер отдаёт ещё
        let mut n = 0;
        let got = collect_top(&keys, || {
            n += 1;
            Ok(vec![format!("w:{n}")])
        })
        .unwrap();
        assert_eq!(got.len(), TOP);
        assert_eq!(n, TOP);
        // повтор показанной — ошибка; чужая ссылка — ошибка
        assert!(collect_top(&keys, || Ok(vec!["w:2".to_string()])).is_err());
        assert!(collect_top(&keys, || Ok(vec!["i:9".to_string()])).is_err());
    }

    #[test]
    fn idea_hints_takes_idea_items_in_order() {
        let v = json!({"items": [
            {"id": "w:b", "reason": "idea"},
            {"id": "i:x", "reason": "gotcha"},
            {"id": "w:a", "reason": "idea"}
        ]});
        assert_eq!(idea_hints(&v), ["w:b", "w:a"]);
        assert!(idea_hints(&json!({})).is_empty());
    }

    #[test]
    fn run_config_is_explicit() {
        let c = run_config();
        assert_eq!(c["ideas.prompt_max"], json!(1));
        assert_eq!(c["ideas.prompt_top_k"], json!(5));
        assert_eq!(c["ideas.proactive"], json!(true));
        assert_eq!(c["recall.prompt_min_score"], json!(0.6));
        assert_eq!(c["recall.timeout_ms"], json!(60000));
        assert!(c["recall.prompt_min_score_fts"].is_null());
        // ключи прогона знает и B5.7 (иначе старт упал бы на deny_unknown_fields)
        let env: Vec<(String, String)> = RUN_CONFIG
            .iter()
            .map(|(k, v)| (crate::eval::run::env_key(k), v.to_string()))
            .collect();
        assert!(caps::check_known(&env).is_ok());
    }

    #[test]
    fn file_blake3_ignores_crlf() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("unit-tmp")
            .join("idea-hints-blake3");
        fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.toml"), dir.join("b.toml"), dir.join("c.toml"));
        fs::write(&a, "x = 1\ny = 2\n").unwrap();
        fs::write(&b, "x = 1\r\ny = 2\r\n").unwrap();
        fs::write(&c, "x = 1\ny = 3\n").unwrap();
        let h = file_blake3(&a).unwrap();
        assert_eq!(h.len(), 64);
        assert_eq!(h, file_blake3(&b).unwrap());
        assert_ne!(h, file_blake3(&c).unwrap());
        assert_eq!(h, blake3::hash(b"x = 1\ny = 2\n").to_hex().to_string());
    }

    /// Набор `eval/idea-hints.toml` проходит проверку на корпусе идей ТС-2.
    #[test]
    fn idea_hints_set_is_valid() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let corpus = sessions_corpus::load(&repo.join("eval").join("sessions-corpus")).unwrap();
        let qs = load_queries(&repo.join("eval").join("idea-hints.toml")).unwrap();
        let s = validate_queries(&qs, &corpus).unwrap();
        assert!(s.queries >= MIN_QUERIES && s.long >= MIN_LONG, "{s:?}");
        assert_eq!(s.states.len(), 7, "{s:?}");
    }

    #[test]
    fn validate_queries_catches_bad_markup() {
        let corpus = SessionsCorpus {
            ideas: vec![
                idea("i01", "human", IdeaState::New, None),
                idea("i02", "human", IdeaState::Declined, None),
            ],
            ..Default::default()
        };
        let q = |id: &str, class: QueryClass, expect: &[&str]| IdeaQuery {
            id: id.into(),
            prompt: "экспорт".into(),
            class,
            expect: expect.iter().map(|s| s.to_string()).collect(),
            about: Vec::new(),
            note: String::new(),
        };
        let err = validate_queries(
            &[
                q("a", QueryClass::Positive, &["idea:i02"]),
                q("a", QueryClass::Negative, &["idea:i01"]),
                q("b", QueryClass::Positive, &["idea:zz"]),
            ],
            &corpus,
        )
        .unwrap_err()
        .to_string();
        for want in [
            "повтор id",
            "не кандидат подсказки",
            "у negative непустой expect",
            "нет такой idea",
            "запросов 3 < 40",
            "не затронуты",
        ] {
            assert!(err.contains(want), "{want}: {err}");
        }
    }
}
