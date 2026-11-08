//! Мир сессии ТС-2 (`eval/sessions.toml` и корпус `eval/sessions-corpus/`) на тестовом сервере —
//! для приёмки дайджеста тяжёлого профиля (M6 §8, задача 8a): Rules, задачи, вопросы с ответами,
//! запросы доступа, словарь, insight, идеи и карта кода — так же, как их грузит
//! `cargo xtask eval --sessions` (`xtask/src/eval/sessions.rs`, `load_world`). Обязательные строки —
//! как в ТС-2 (`required_lines`): ссылки `n:`/`k:`/`q:`/`w:` на id загруженных записей и строка
//! запросов доступа.
use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::api::Api;

/// Решение мира (как `EVAL_SOLUTION` замера).
pub const SOL: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eaa";
/// Строка дайджеста об ожидающих запросах доступа (признак ТС-2 — текст).
pub const ACCESS_LINE: &str = "запросы доступа к вашим решениям ждут ответа";

#[derive(Deserialize)]
struct SetFile {
    session: Vec<Spec>,
}

/// Сессия набора (нужные поля).
#[derive(Deserialize)]
pub struct Spec {
    pub id: String,
    pub profile: String,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub access: Vec<String>,
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub insights: Vec<String>,
    #[serde(default)]
    pub ideas: Vec<String>,
    #[serde(default)]
    pub code_map: bool,
    #[serde(default)]
    pub digest_ideas: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct Rule {
    key: String,
    path: String,
    text: String,
}

#[derive(Deserialize, Clone)]
struct Task {
    key: String,
    title: String,
    #[serde(default)]
    body: String,
    status: String,
    next_step: Option<String>,
    blocked_by: Option<String>,
}

#[derive(Deserialize, Clone)]
struct QOption {
    key: String,
    text: String,
}

#[derive(Deserialize, Clone)]
struct Answer {
    option: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Question {
    key: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    options: Vec<QOption>,
    recommendation: Option<String>,
    answer: Option<Answer>,
}

#[derive(Deserialize, Clone)]
struct Access {
    key: String,
    user: String,
}

#[derive(Deserialize, Clone)]
struct Term {
    key: String,
    title: String,
    definition: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct Insight {
    key: String,
    kind: String,
    scope: String,
    area: Option<String>,
    title: String,
    body: String,
    #[serde(default)]
    triggers: Vec<Value>,
    certainty: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Idea {
    key: String,
    title: String,
    #[serde(default)]
    body: String,
    priority: Option<String>,
    origin: String,
    state: String,
    horizon_days: Option<i64>,
}

#[derive(Deserialize, Default)]
struct Corpus {
    #[serde(default)]
    rule: Vec<Rule>,
    #[serde(default)]
    task: Vec<Task>,
    #[serde(default)]
    question: Vec<Question>,
    #[serde(default)]
    access: Vec<Access>,
    #[serde(default)]
    term: Vec<Term>,
    #[serde(default)]
    insight: Vec<Insight>,
    #[serde(default)]
    idea: Vec<Idea>,
}

fn eval_dir() -> PathBuf {
    super::ml::repo().join("eval")
}

fn read_toml<T: serde::de::DeserializeOwned>(p: PathBuf) -> T {
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Сессия `id` набора `eval/sessions.toml`.
pub fn spec(id: &str) -> Spec {
    let f: SetFile = read_toml(eval_dir().join("sessions.toml"));
    f.session
        .into_iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("нет сессии {id} в eval/sessions.toml"))
}

fn corpus() -> Corpus {
    let d = eval_dir().join("sessions-corpus");
    let mut c = Corpus::default();
    for f in [
        "rules.toml",
        "journal.toml",
        "glossary.toml",
        "insights.toml",
    ] {
        let part: Corpus = read_toml(d.join(f));
        c.rule.extend(part.rule);
        c.task.extend(part.task);
        c.question.extend(part.question);
        c.access.extend(part.access);
        c.term.extend(part.term);
        c.insight.extend(part.insight);
    }
    let ideas: Corpus = read_toml(d.join("ideas").join("ideas.toml"));
    c.idea = ideas.idea;
    c
}

/// Выбор записей по ключам сессии (`"*"` — все).
fn pick<T: Clone>(keys: &[String], all: &[T], key: impl Fn(&T) -> &str) -> Vec<T> {
    if keys.iter().any(|k| k == "*") {
        return all.to_vec();
    }
    keys.iter()
        .map(|k| {
            all.iter()
                .find(|x| key(x) == k)
                .unwrap_or_else(|| panic!("{k}: нет в корпусе ТС-2"))
                .clone()
        })
        .collect()
}

/// Обязательная строка ТС-2: вид, ключ набора и признак (ссылка `n:`/`k:`/`q:`/`w:` или текст).
#[derive(Debug, Clone)]
pub struct Required {
    pub kind: &'static str,
    pub key: String,
    pub needle: String,
}

/// Загруженный мир: обязательные строки и id ловушек (`gotcha`/`failure`).
pub struct World {
    pub required: Vec<Required>,
    pub traps: Vec<String>,
    pub rules: usize,
}

async fn post(a: &Api, path: &str, body: Value, want: StatusCode) -> Value {
    let r = a.t(Method::POST, path, Some(body.clone())).await;
    assert_eq!(r.status, want, "POST {path} {body}: {}", r.text());
    r.json()
}

fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

async fn patch(a: &Api, id: &str, body: Value) {
    let r = a
        .tok_rev(
            &a.token,
            Method::PATCH,
            &format!("/api/v1/entries/{id}"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "PATCH {id} {body}: {}", r.text());
}

fn utc_date_plus(days: i64) -> String {
    zam_store::time::utc_today()
        .checked_add(jiff::Span::new().days(days))
        .unwrap()
        .to_string()
}

/// Загружает мир сессии `s` на сервер `a` (токен администратора, `rules.agent_write` включается
/// здесь). Идеи — по плану загрузчика ТС-2 (`load_plan`), горизонт «сегодня» — прямым SQL.
pub async fn load(a: &Api, s: &Spec) -> World {
    let c = corpus();
    let r = a
        .adm(
            Method::PATCH,
            "/api/v1/admin/settings",
            Some(json!({"rules": {"agent_write": true}})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let reg = json!({"agent_solution_id": SOL, "name": "eval"});
    let r = a
        .t(Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let mut required = Vec::new();
    let mut need = |kind: &'static str, key: &str, needle: String| {
        required.push(Required {
            kind,
            key: key.to_string(),
            needle,
        })
    };
    let rules = pick(&s.rules, &c.rule, |r| &r.key);
    for r in &rules {
        let v = a.note("rules", &r.path, &r.text).await;
        need("rule", &r.key, format!("n:{}", id_of(&v)));
    }
    let feed = format!("/api/v1/spaces/{SOL}/entries");
    let mut traps = Vec::new();
    for i in pick(&s.insights, &c.insight, |i| &i.key) {
        let mut body = json!({"kind": "insight", "title": i.title, "body": i.body,
                              "data": {"kind": i.kind}, "triggers": i.triggers,
                              "force": true, "scope": i.scope});
        if let Some(ar) = &i.area {
            body["area"] = json!(ar);
        }
        if let Some(ce) = &i.certainty {
            body["certainty"] = json!(ce);
        }
        let v = post(a, &feed, body, StatusCode::CREATED).await;
        if matches!(i.kind.as_str(), "gotcha" | "failure") {
            traps.push(format!("i:{}", id_of(&v)));
        }
    }
    for t in pick(&s.tasks, &c.task, |t| &t.key) {
        let mut data = json!({});
        if let Some(n) = &t.next_step {
            data["next_step"] = json!(n);
        }
        if let Some(b) = &t.blocked_by {
            data["blocked_by"] = json!(b);
        }
        let v = post(
            a,
            &feed,
            json!({"kind": "task", "title": t.title, "body": t.body, "data": data}),
            StatusCode::CREATED,
        )
        .await;
        let id = id_of(&v);
        if t.status != "open" {
            patch(a, &id, json!({"status": t.status})).await;
        }
        if matches!(t.status.as_str(), "in_progress" | "blocked") {
            need("task", &t.key, format!("k:{id}"));
        }
    }
    for q in pick(&s.questions, &c.question, |q| &q.key) {
        let mut data = json!({});
        if !q.options.is_empty() {
            data["options"] = json!(
                q.options
                    .iter()
                    .map(|o| json!({"key": o.key, "text": o.text}))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(r) = &q.recommendation {
            data["recommendation"] = json!(r);
        }
        let v = post(
            a,
            &feed,
            json!({"kind": "question", "title": q.title, "body": q.body, "data": data}),
            StatusCode::CREATED,
        )
        .await;
        let id = id_of(&v);
        if let Some(ans) = &q.answer {
            let mut body = json!({"by": "human"});
            if let Some(o) = &ans.option {
                body["option"] = json!(o);
            }
            if let Some(t) = &ans.text {
                body["text"] = json!(t);
            }
            post(
                a,
                &format!("/api/v1/entries/{id}/answer"),
                body,
                StatusCode::OK,
            )
            .await;
            need("answer", &q.key, format!("q:{id}"));
        }
    }
    for t in pick(&s.terms, &c.term, |t| &t.key) {
        post(
            a,
            &feed,
            json!({"kind": "term", "title": t.title, "body": t.definition,
                   "data": {"definition": t.definition, "aliases": t.aliases}}),
            StatusCode::CREATED,
        )
        .await;
    }
    let mut idea_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut due = Vec::new();
    for i in pick(&s.ideas, &c.idea, |i| &i.key) {
        let mut data = json!({"origin": i.origin});
        if let Some(p) = &i.priority {
            data["priority"] = json!(p);
        }
        let v = post(
            a,
            &feed,
            json!({"kind": "idea", "title": i.title, "body": i.body, "data": data}),
            StatusCode::CREATED,
        )
        .await;
        let id = id_of(&v);
        let decide = |body: Value| {
            let id = id.clone();
            async move {
                post(
                    a,
                    &format!("/api/v1/entries/{id}/decide"),
                    body,
                    StatusCode::OK,
                )
                .await
            }
        };
        match i.state.as_str() {
            "new" => {}
            "proposed" => {
                post(
                    a,
                    &format!("/api/v1/entries/{id}/propose"),
                    json!({"text": "пора обсудить"}),
                    StatusCode::OK,
                )
                .await;
            }
            "declined" => {
                decide(json!({"decision": "decline", "by": "human"})).await;
            }
            "accepted" | "released" | "done" => {
                let v = decide(json!({"decision": "accept", "by": "human"})).await;
                let task = v["task"]["id"].as_str().unwrap().to_string();
                match i.state.as_str() {
                    "released" => {
                        patch(
                            a,
                            &task,
                            json!({"status": "dropped", "reason": "не до того"}),
                        )
                        .await
                    }
                    "done" => patch(a, &task, json!({"status": "done"})).await,
                    _ => {}
                }
            }
            "deferred" => {
                let days = i.horizon_days.expect("у deferred нужен horizon_days");
                decide(json!({"decision": "defer", "by": "human",
                              "horizon": utc_date_plus(days.max(1))}))
                .await;
                if days == 0 {
                    due.push(id.clone());
                }
            }
            other => panic!("idea {}: состояние {other}", i.key),
        }
        idea_ids.insert(i.key.clone(), id);
    }
    if !due.is_empty() {
        let db = a.st.users.get(a.st.admin_id).await.unwrap();
        let today = utc_date_plus(0);
        db.write(move |tx| {
            for id in &due {
                let n = tx.execute(
                    "UPDATE entries SET data = json_set(data, '$.horizon', ?2) WHERE id = ?1",
                    rusqlite::params![id, today],
                )?;
                assert_eq!(n, 1, "горизонт идеи {id}");
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    for k in &s.digest_ideas {
        if let Some(id) = idea_ids.get(k) {
            need("idea", k, format!("w:{id}"));
        }
    }
    for x in pick(&s.access, &c.access, |x| &x.key) {
        let (_, tok) = a.new_user(&x.user).await;
        let r = a
            .tok(&tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}: {}", x.key, r.text());
        need("access", &x.key, ACCESS_LINE.to_string());
    }
    if s.code_map {
        load_code_map(a).await;
    }
    World {
        required,
        traps,
        rules: rules.len(),
    }
}

/// Файл карты кода: путь, язык, символы `(имя, вид, строка)`.
type CodeFile = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str, u32)],
);

/// Карта кода фикстуры ТС-2 (`eval/sessions-corpus/code`): файлы и их главные символы — как
/// после `zam code sync` (имена — из исходников фикстуры).
async fn load_code_map(a: &Api) {
    let files: &[CodeFile] = &[
        (
            "services/orders/src/export.rs",
            "rust",
            &[("OrderRow", "struct", 6), ("export_csv", "function", 13)],
        ),
        (
            "services/orders/src/pagination.rs",
            "rust",
            &[
                ("KeysetCursor", "struct", 5),
                ("Page", "struct", 11),
                ("after_clause", "function", 17),
                ("page_limit", "function", 22),
            ],
        ),
        (
            "web/storefront/src/auth/refresh.ts",
            "typescript",
            &[
                ("TokenPair", "interface", 5),
                ("SingleTabRefresher", "class", 11),
            ],
        ),
    ];
    let h = |n: usize| format!("{n:064x}");
    let manifest: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, (p, _, _))| json!({"path": p, "hash": h(i + 1)}))
        .collect();
    let r = a
        .t(
            Method::POST,
            "/api/v1/code/sync",
            Some(
                json!({"solution": SOL, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "manifest": manifest}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let up: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, (p, lang, syms))| {
            let symbols: Vec<Value> = syms
                .iter()
                .map(|(name, kind, line)| {
                    json!({"name": name, "qual": name, "kind": kind, "line": line,
                           "end_line": line + 3, "sig": null,
                           "body_hash": format!("{line:016x}"), "exported": true})
                })
                .collect();
            json!({"path": p, "lang": lang, "hash": h(i + 1), "size": 100,
                   "parse_state": "ok", "symbols": symbols, "refs": []})
        })
        .collect();
    let r = a
        .t(
            Method::PUT,
            "/api/v1/code/files",
            Some(
                json!({"solution": SOL, "commit": "abcdef1234", "branch": "main",
                        "machine": "box", "files": up}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Нет ли в `text` обязательной строки `r` (ссылка — как отдельное слово).
pub fn found(text: &str, r: &Required) -> bool {
    if r.kind == "access" {
        return text.contains(&r.needle);
    }
    text.split(|c: char| c.is_whitespace())
        .any(|w| w == r.needle)
}

/// Проверка дайджеста тяжёлой сессии: все обязательные строки ТС-2, строка карты кода (если
/// карта есть), раздел ловушек с хотя бы одной ловушкой мира.
pub fn check_digest(what: &str, text: &str, w: &World, code_map: bool) {
    check_required(what, text, w);
    check_extras(what, text, w, code_map);
}

/// Все обязательные строки ТС-2 (правила — блоком L1 или строкой L0 с той же ссылкой).
pub fn check_required(what: &str, text: &str, w: &World) {
    let missing: Vec<String> = w
        .required
        .iter()
        .filter(|r| !found(text, r))
        .map(|r| format!("{} {}", r.kind, r.key))
        .collect();
    assert!(
        missing.is_empty(),
        "{what}: нет обязательных строк {missing:?} из {}:\n{text}",
        w.required.len()
    );
}

/// Строка карты кода (если карта есть) и раздел ловушек с ловушкой мира.
pub fn check_extras(what: &str, text: &str, w: &World, code_map: bool) {
    if code_map {
        assert!(
            text.lines()
                .any(|l| l.starts_with("ZeroAIMemory: карта кода:")),
            "{what}: нет строки карты кода:\n{text}"
        );
    }
    assert!(
        text.contains("### Ловушки и сбои"),
        "{what}: нет раздела ловушек:\n{text}"
    );
    assert!(
        w.traps.iter().any(|t| found(
            text,
            &Required {
                kind: "trap",
                key: String::new(),
                needle: t.clone()
            }
        )),
        "{what}: нет ни одной ловушки:\n{text}"
    );
}

/// Сводка вывода тяжёлой сессии в пределе канала: правил блоком L1, строкой L0, из них с «…»;
/// ловушек мира; есть ли строка карты кода.
pub fn shape(text: &str, w: &World) -> String {
    let rules = text
        .split("\n### Правила\n")
        .nth(1)
        .map(|r| r.split("\n\n").next().unwrap_or_default())
        .unwrap_or_default();
    // строка правила (`<путь> n:<id>`, задача 8c) — L1, если под ней текст с отступом, иначе L0
    let lines: Vec<&str> = rules.lines().collect();
    let head = |k: usize| {
        let l = lines[k];
        !l.starts_with("ZeroAIMemory:") && !l.starts_with(' ') && l.contains(" n:")
    };
    let has_body = |k: usize| lines.get(k + 1).is_some_and(|n| n.starts_with(' '));
    let l1 = (0..lines.len()).filter(|&k| head(k) && has_body(k)).count();
    let l0: Vec<&str> = (0..lines.len())
        .filter(|&k| head(k) && !has_body(k))
        .map(|k| lines[k])
        .collect();
    let cut = l0.iter().filter(|l| l.contains('…')).count();
    let traps = w
        .traps
        .iter()
        .filter(|t| text.split_whitespace().any(|x| x == t.as_str()))
        .count();
    format!(
        "правил L1 {l1}, L0 {} (с «…» {cut}), ловушек {traps}/{}, карта кода {}",
        l0.len(),
        w.traps.len(),
        text.contains("ZeroAIMemory: карта кода:")
    )
}

/// Повторное ревью 8a (2в): снятое пределом вывода не пропадает молча — у ловушек число
/// непоказанных называет строка «не показано — ловушек: N», у снятых строк-указателей (открытые
/// вопросы, словарь) — строка со своим адресом (`kinds=[question]`, `kinds=[term]`).
pub fn check_omits(what: &str, text: &str, w: &World) {
    let shown = w
        .traps
        .iter()
        .filter(|t| text.split_whitespace().any(|x| x == t.as_str()))
        .count();
    let missing = w.traps.len() - shown;
    if missing > 0 {
        assert!(
            text.contains(&format!("ZeroAIMemory: не показано — ловушек: {missing} (")),
            "{what}: {missing} ловушек сняты молча:\n{text}"
        );
    }
    // снятые строки-указатели — со своим адресом
    for (line, omit) in [
        (
            "### Открытые вопросы",
            "строк открытых вопросов: 1 (memory_search kinds=[question])",
        ),
        ("Словарь:", "строк словаря: 1 (memory_search kinds=[term])"),
    ] {
        if !text.contains(line) {
            assert!(
                text.contains(&format!("не показано — {omit}")),
                "{what}: «{line}» снята молча:\n{text}"
            );
        }
    }
}
