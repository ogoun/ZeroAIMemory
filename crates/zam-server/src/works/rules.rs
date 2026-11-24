//! Кандидаты в Rules (M7.3 §6.7 пп. 2–3): проверка кандидата — **одна** для спонтанного
//! кандидата агента (`POST /proposals`, `memory_write … candidate`, `zam rules candidate`) и для
//! элемента работы вида `rules` (агент и модель, решение координатора 32), — и сам вид работы:
//! сборщик, инструкция и схема, проверка элемента итога, отказ исполнителя, контекст пакета.
//!
//! **Проверка** ([`check_candidate`]): `content` — до [`RULE_MAX_TOKENS`] токенов счётчиком
//! выдачи (правило читается L1 в каждом дайджесте, ТЗ §7.1), без маски секрета (§6.3 п. 4), без
//! URL и без блоков кода — огороженных, отступом и HTML-блоков `<pre>`/`<code>`, по разбору
//! Markdown ([`zam_markdown::has_code_block`], решение координатора 58: правило «для всех
//! проектов» их не требует, а они — главный канал внедрения команд); `reason` — [`REASON_CHARS`]
//! символов; путь — допустимый и свободный путь Rules ([`rule_path`]); не почти-дубль заметки
//! Rules: тот же нормализованный **явный** заголовок (имя заметки не сравнивается) или
//! dense-косинус не ниже `insights.dedup_threshold` (`nearest_dense` по Rules владельца — как
//! дедупликация insight, `api::insights::dedup`); без модели — только заголовок. Сбой хранилища
//! при проверке — сбой сервера (REST 500), а не отказ кандидату.
//!
//! **Сборщик** ([`collect`]; §6.7 п. 3): источники без предложения — действующие (`active`)
//! insight `decision` с `certainty = axiom` (решения владельца) из Knowledge и решений базы
//! пользователя и insight `gotcha | pattern` **Knowledge** с `useful ≥ 2` или `opened ≥ 3`
//! (ловушка решения — факт проекта); записи, к которым приложил руку участник общего решения, не
//! берутся (`zam_store::works::owner_entries`, решение 62); источник, выброшенный вторым сканом
//! (`secret_in_payload`), — до новой ревизии (`excluded_refs`, решение 72); источник с
//! предложением — никогда (кроме отозванного автором), с отказом исполнителя (`skipped`) — до новой
//! ревизии (`zam_store::proposals::existing_keys`, решение 64). Сколько кандидатов — не больше
//! свободных мест до [`zam_store::proposals::PROPOSALS_OPEN_MAX`] открытых. Пакет получает ещё
//! запись `n="0"` — действующие заметки Rules (до [`RULES_TITLES_MAX`], путь и L0 до
//! [`CONTEXT_L0_MAX_CHARS`], вся запись — не больше четверти бюджета пакета и
//! [`CONTEXT_MAX_TOKENS`], [`context`]), чтобы исполнитель их не повторял; итога для неё нет.
//! Элемент, которому нет места или у источника которого уже есть предложение, отклоняется
//! проверкой ([`accept`]); то же, случившееся между проверкой и закрытием работы, — предупреждение
//! итога (`warnings`).
//!
//! Итог не меняет знания (решение координатора 33): принятый элемент — строка `proposals`
//! ([`ItemWrite::Rule`]), заметку Rules создаёт человек, принимая кандидата
//! (`api::proposals::accept`).

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;

use serde_json::{Value, json};
use zam_core::{Id, TargetKind};
use zam_index::{VectorFilter, VectorScope};
use zam_store::proposals::{self as pr, ProposalAuthor, ProposalType};
use zam_store::works::{self as store, ItemWrite, WorkKind};
use zam_store::{Db, NodeKind, NodePath, SpaceKind, StoreError};

use super::kinds::{Author, Candidate, ItemCtx, Reject};
use super::night::NightCtx;
use super::package::{agent_text, check_mask_literal};
use crate::agent_setup::Lang;
use crate::app::AppState;
use crate::error::ApiError;

/// Предел текста правила, токенов счётчика выдачи (§6.7 п. 3).
pub const RULE_MAX_TOKENS: usize = 200;
/// Длина довода кандидата, символов (§6.7 п. 2).
pub const REASON_CHARS: RangeInclusive<usize> = 20..=2000;
/// Ссылок основания (`basis`) у кандидата — не больше.
pub const BASIS_MAX: usize = 20;
/// Заметок Rules в контексте пакета (L0) — не больше (§6.7 п. 3).
pub const RULES_TITLES_MAX: usize = 50;
/// «Полезно» у ловушки или приёма — источник от стольких (§6.7 п. 3).
pub const USEFUL_MIN: i64 = 2;
/// Открытий подсказки у ловушки или приёма — источник от стольких (§6.7 п. 3).
pub const OPENED_MIN: i64 = 3;
/// Ближайших точек на запрос проверки почти-дубля (у заметки несколько чанков).
const SIMILAR_NEAREST: usize = 20;
/// Ссылка записи контекста пакета (`<record n="0" ref="rules" …>`).
pub const CONTEXT_REF: &str = "rules";

/// Почти-дубль кандидата: заметка Rules и причина (`title` | `vector`).
#[derive(Debug, Clone, PartialEq)]
pub struct Similar {
    pub node: Id,
    pub path: String,
    pub reason: &'static str,
    pub score: Option<f32>,
}

/// Отказ проверки кандидата.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleError {
    /// Путь — не путь Rules (400).
    BadPath(String),
    /// Текст или довод не проходят проверку (422).
    Invalid(String),
    /// Путь занят заметкой (409 `code: path_taken`).
    PathTaken { path: String, node: Id },
    /// Почти-дубль заметки Rules (409 `code: rule_duplicate`).
    Duplicate(Vec<Similar>),
    /// Сбой хранилища при проверке (REST — 500; элемент работы — отказ «повторите позже»);
    /// подробности — в журнал.
    Store(String),
}

impl RuleError {
    /// Текст отказа: `detail` ответа и причина отказа элемента работы.
    pub fn reason(&self) -> String {
        match self {
            RuleError::BadPath(m) | RuleError::Invalid(m) => m.clone(),
            RuleError::Store(_) => "проверка кандидата не завершилась — повторите позже".into(),
            RuleError::PathTaken { path, node } => format!(
                "путь rules:/{path} занят заметкой n:{node} — предложите другой путь (например, \
                 {path}-2) или дополните ту заметку"
            ),
            RuleError::Duplicate(s) => {
                let list: Vec<String> = s
                    .iter()
                    .map(|x| format!("n:{} rules:/{}", x.node, x.path))
                    .collect();
                format!(
                    "похоже на заметку Rules ({}): проверьте, нет ли там этого правила, или \
                     предложите другое (путь и текст)",
                    list.join(", ")
                )
            }
        }
    }
}

impl From<RuleError> for ApiError {
    fn from(e: RuleError) -> ApiError {
        let detail = e.reason();
        match e {
            RuleError::BadPath(m) => ApiError::BadRequest(m),
            RuleError::Invalid(m) => ApiError::Unprocessable(m),
            // сбой сервера, а не клиента: не 422 «не повторяй»
            RuleError::Store(m) => ApiError::Internal(format!("проверка кандидата в Rules: {m}")),
            RuleError::PathTaken { path, node } => {
                let mut extra = serde_json::Map::new();
                extra.insert("code".into(), json!("path_taken"));
                extra.insert("path".into(), json!(path));
                extra.insert("node".into(), json!(format!("n:{node}")));
                ApiError::Conflict { detail, extra }
            }
            RuleError::Duplicate(s) => {
                let mut extra = serde_json::Map::new();
                extra.insert("code".into(), json!("rule_duplicate"));
                extra.insert(
                    "similar".into(),
                    json!(
                        s.iter()
                            .map(|x| json!({"ref": format!("n:{}", x.node), "path": x.path,
                                            "reason": x.reason, "score": x.score}))
                            .collect::<Vec<_>>()
                    ),
                );
                ApiError::Conflict { detail, extra }
            }
        }
    }
}

impl From<StoreError> for RuleError {
    fn from(e: StoreError) -> RuleError {
        // сбой хранилища при проверке — сбой сервера (подробности — в журнал)
        tracing::warn!(error = %e, "кандидат в Rules: проверка не завершилась");
        RuleError::Store(e.to_string())
    }
}

/// Путь заметки Rules из `raw`: без `rules:` и `/` по краям; префикс другого пространства
/// (`knowledge:/…`, `<GUID>:…`) и недопустимый путь — [`RuleError::BadPath`].
pub fn rule_path(raw: &str) -> Result<NodePath, RuleError> {
    use zam_core::space::{SpaceSel, split_space};
    let bad = || {
        RuleError::BadPath(format!(
            "path: путь заметки Rules (rules:/<путь>), а не «{raw}» — кандидат бывает только в \
             Rules"
        ))
    };
    // префикс пространства — по правилу CLI и MCP (`zam_core::space::split_space`)
    let s = match split_space(raw.trim()) {
        (SpaceSel::Fixed("rules"), p) => p,
        (SpaceSel::Default, p) => p.trim_matches('/'),
        _ => return Err(bad()),
    };
    // `<слово>:/…` с неизвестным словом — не путь, а чужое пространство (как у MCP)
    if s.contains(":/") {
        return Err(bad());
    }
    NodePath::parse(s).map_err(|e| RuleError::BadPath(format!("path: {e}")))
}

/// В тексте есть URL: `<схема>://`, `www.<…>`, `mailto:`.
pub fn has_url(text: &str) -> bool {
    let lower = text.to_lowercase();
    let b = lower.as_bytes();
    let boundary = |i: usize| i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.');
    for (i, _) in lower.match_indices("://") {
        // схема — буквы перед `://`
        let mut j = i;
        while j > 0 && (b[j - 1].is_ascii_alphanumeric() || matches!(b[j - 1], b'+' | b'-' | b'.'))
        {
            j -= 1;
        }
        if j < i && b[j].is_ascii_alphabetic() {
            return true;
        }
    }
    let after_ok = |k: usize| b.get(k).is_some_and(u8::is_ascii_alphanumeric);
    lower
        .match_indices("www.")
        .any(|(i, _)| boundary(i) && after_ok(i + 4))
        || lower
            .match_indices("mailto:")
            .any(|(i, _)| boundary(i) && after_ok(i + 7))
}

/// Есть ли в тексте команды shell (пометка карточки для человека, §6.7 п. 4): строка вида
/// `$ …`, `cmd /c`, `powershell`.
pub fn has_shell(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.lines().any(|l| l.trim_start().starts_with("$ "))
        || lower.contains("cmd /c")
        || lower.contains("cmd.exe")
        || lower.contains("powershell")
}

/// Проверка текста правила (без пути и места): непустой, до [`RULE_MAX_TOKENS`] токенов, без
/// маски секрета, URL и блоков кода.
pub fn check_text(content: &str) -> Result<(), RuleError> {
    if content.trim().is_empty() {
        return Err(RuleError::Invalid("content: пустой текст правила".into()));
    }
    check_mask_literal(content).map_err(|r| RuleError::Invalid(r.reason))?;
    let n = zam_store::count_tokens(content);
    if n > RULE_MAX_TOKENS {
        return Err(RuleError::Invalid(format!(
            "content: {n} токенов — правило не длиннее {RULE_MAX_TOKENS} токенов (оно читается \
             в каждом дайджесте)"
        )));
    }
    if has_url(content) {
        return Err(RuleError::Invalid(
            "content: в правиле есть URL — правило для всех проектов пишется без адресов (ссылку \
             человек допишет сам при принятии)"
                .into(),
        ));
    }
    if zam_markdown::has_code_block(content) {
        return Err(RuleError::Invalid(
            "content: в правиле есть блок кода — правило пишется словами, без блоков кода \
             (однострочный `код` допустим)"
                .into(),
        ));
    }
    Ok(())
}

/// Довод кандидата: [`REASON_CHARS`] символов (без пробелов по краям).
pub fn check_reason(reason: &str) -> Result<String, RuleError> {
    let r = reason.trim();
    let n = r.chars().count();
    if !REASON_CHARS.contains(&n) {
        return Err(RuleError::Invalid(format!(
            "reason: {n} символов — нужно от {} до {}: почему это правило для всех проектов",
            REASON_CHARS.start(),
            REASON_CHARS.end()
        )));
    }
    Ok(r.to_string())
}

/// Пространство Rules базы.
pub async fn rules_space(db: &Db) -> Result<Id, StoreError> {
    Ok(zam_store::space_of_kind(db, SpaceKind::Rules).await?.id)
}

/// Путь `path` в Rules свободен: живого узла по нему нет.
pub async fn check_free(db: &Db, rules: Id, path: &NodePath) -> Result<(), RuleError> {
    match zam_store::get_node_by_path(db, rules, path).await {
        Ok(n) => Err(RuleError::PathTaken {
            path: path.as_str().to_string(),
            node: n.id,
        }),
        Err(StoreError::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Явный заголовок правила для сравнения — первый заголовок Markdown текста; без него — `None`
/// (имя заметки не сравнивается: `git/style` и `rust/style` — разные правила, ревью r1, Minor 2;
/// тот же путь — уже «путь занят»).
pub fn rule_title(content: &str) -> Option<String> {
    zam_markdown::parse(content)
        .headings
        .first()
        .map(|h| h.text.clone())
        .filter(|t| !zam_store::normalize_title(t).is_empty())
}

/// Живые заметки Rules с текстом текущей ревизии (по пути).
async fn rule_notes(db: &Db, rules: Id) -> Result<Vec<(zam_store::Node, String)>, StoreError> {
    let mut out = Vec::new();
    for n in zam_store::list_children(db, rules, None, 16).await? {
        if n.kind != NodeKind::Note {
            continue;
        }
        match zam_store::node_content(db, n.id, None).await {
            Ok((_, text)) => out.push((n, text)),
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    out.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    Ok(out)
}

/// Почти-дубли кандидата `path`/`content` среди заметок Rules владельца `owner`: тот же
/// нормализованный заголовок; при доступном векторном канале — ещё dense-косинус не ниже
/// `insights.dedup_threshold` (сбой канала — WARN, только заголовок).
pub async fn similar_rules(
    st: &AppState,
    db: &Db,
    owner: Id,
    rules: Id,
    path: &str,
    content: &str,
) -> Result<Vec<Similar>, StoreError> {
    let title = rule_title(content);
    let norm = title.as_deref().map(zam_store::normalize_title);
    let notes = rule_notes(db, rules).await?;
    let mut out: Vec<Similar> = notes
        .iter()
        .filter(|(n, text)| {
            n.path != path
                && norm.is_some()
                && rule_title(text).map(|t| zam_store::normalize_title(&t)) == norm
        })
        .map(|(n, _)| Similar {
            node: n.id,
            path: n.path.clone(),
            reason: "title",
            score: None,
        })
        .collect();
    let (Ok(()), Some(emb), Some(vector)) = (
        st.index.vector_available(),
        st.index.embedder.clone(),
        st.index.vector.clone(),
    ) else {
        return Ok(out);
    };
    // как у дедупликации insight — заголовок и текст
    let text = match &title {
        Some(t) => format!("{}\n{content}", t.trim()),
        None => content.to_string(),
    };
    let e = match tokio::task::spawn_blocking(move || emb.embed_query(&text)).await {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "эмбеддинг кандидата в Rules не удался: только заголовок");
            return Ok(out);
        }
        Err(j) => {
            tracing::error!(
                panic = j.is_panic(),
                "эмбеддинг кандидата в Rules аварийно завершился"
            );
            return Ok(out);
        }
    };
    let filter = VectorFilter {
        scopes: vec![VectorScope {
            user_id: owner,
            space_ids: vec![rules],
        }],
        kinds: vec!["note".into()],
        ..VectorFilter::default()
    };
    let hits = match vector
        .nearest_dense(&e.dense, &filter, SIMILAR_NEAREST)
        .await
    {
        Ok(h) => h,
        Err(err) => {
            tracing::warn!(error = %err, "векторный поиск дублей правила не удался: только заголовок");
            return Ok(out);
        }
    };
    let threshold = st.cfg.insights.dedup_threshold;
    let live: HashMap<Id, &str> = notes.iter().map(|(n, _)| (n.id, n.path.as_str())).collect();
    let mut best: HashMap<Id, f32> = HashMap::new();
    for h in hits
        .iter()
        .filter(|h| h.user_id == owner && h.target.kind == TargetKind::Node && h.score >= threshold)
    {
        let s = best.entry(h.target.id).or_insert(h.score);
        *s = s.max(h.score);
    }
    let mut ranked: Vec<(Id, f32)> = best.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (id, score) in ranked {
        // источник истины — SQLite: точка могла пережить удаление или перенос
        let Some(&p) = live.get(&id) else {
            continue;
        };
        if p == path {
            continue;
        }
        match out.iter_mut().find(|x| x.node == id) {
            Some(x) => x.score = Some(score),
            None => out.push(Similar {
                node: id,
                path: p.to_string(),
                reason: "vector",
                score: Some(score),
            }),
        }
    }
    Ok(out)
}

/// Проверенный кандидат: путь Rules, текст и довод.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub path: NodePath,
    pub content: String,
    pub reason: String,
}

/// Проверка кандидата (§6.7 пп. 2–3) — общая для спонтанного кандидата и элемента работы: путь,
/// текст, довод, свободный путь, почти-дубль. Ничего не пишет.
pub async fn check_candidate(
    st: &AppState,
    db: &Db,
    owner: Id,
    path: &str,
    content: &str,
    reason: &str,
) -> Result<Checked, RuleError> {
    let path = rule_path(path)?;
    let content = content.trim().to_string();
    check_text(&content)?;
    let reason = check_reason(reason)?;
    let rules = rules_space(db).await?;
    check_free(db, rules, &path).await?;
    let similar = similar_rules(st, db, owner, rules, path.as_str(), &content).await?;
    if !similar.is_empty() {
        return Err(RuleError::Duplicate(similar));
    }
    Ok(Checked {
        path,
        content,
        reason,
    })
}

/// Оценка Rules в дайджесте, токенов: сумма строк правил (`<путь> n:<id>` и L1 до
/// `RULE_L1_TOKENS`, как в дайджесте) и та же сумма с кандидатом `add` (путь, текст) —
/// «Rules в каждой сессии: now → after» окна «Принять» (§6.7 п. 4).
pub async fn rules_tokens(
    db: &Db,
    rules: Id,
    add: Option<(&str, &str)>,
) -> Result<(usize, usize), StoreError> {
    let line = |path: &str, id: &str, text: &str| {
        let (l1, _) = crate::api::recall::rule_l1(text, path);
        zam_store::count_tokens(&format!("{path} n:{id}\n  {l1}"))
    };
    let mut now = 0;
    for (n, text) in rule_notes(db, rules).await? {
        now += line(&n.path, &n.id.to_string(), &text);
    }
    let after = now
        + add
            .map(|(p, c)| line(p, &"0".repeat(26), c))
            .unwrap_or_default();
    Ok((now, after))
}

/// Сообщение ревизии принятой заметки (решение координатора 60): «из предложения o:<id>
/// (model:<имя> | сессия <id>)»; автор без сессии — «агент».
pub fn origin_message(p: &pr::Proposal) -> String {
    let who = match (p.author_kind, p.author.as_deref(), p.session_id.as_deref()) {
        (ProposalAuthor::Model, Some(a), _) => a.to_string(),
        (ProposalAuthor::Model, None, _) => "model".to_string(),
        (ProposalAuthor::Agent, _, Some(s)) => format!("сессия {s}"),
        (ProposalAuthor::Agent, Some(a), None) if !a.is_empty() => format!("сессия {a}"),
        (ProposalAuthor::Agent, _, None) => "агент".to_string(),
    };
    format!("из предложения o:{} ({who})", p.id)
}

// ---------- вид работы `rules` ----------

/// Инструкция вида (ru), ≤ 300 токенов.
pub const INSTRUCTION_RU: &str = "Для каждой записи реши, есть ли в ней правило для всех \
     проектов агента — поведение, а не факт одного проекта. Да — элемент {n, rule: {path, title, \
     content}, reason}: path — свободный путь заметки Rules (сегменты через «/»), title — короткое \
     имя правила, content — повелительно, до 200 токенов, без путей проекта, URL и блоков кода, \
     без противоречия заметкам Rules; reason — почему это правило для всех проектов (20–2000 \
     символов). Нет — {n, skip: \"почему\"}. Запись n=\"0\" — действующие заметки Rules: не \
     повторяй их, итога для неё нет. Итог не меняет память: кандидата принимает человек.";
/// Инструкция вида (en), ≤ 300 токенов.
pub const INSTRUCTION_EN: &str = "For each record decide whether it holds a rule for all of the \
     agent's projects — a behaviour, not a fact of one project. Yes — an item {n, rule: {path, \
     title, content}, reason}: path — a free Rules note path (segments joined by “/”), title — a \
     short rule name, content — imperative, up to 200 tokens, without project paths, URLs and code \
     blocks, not contradicting Rules notes; reason — why it is a rule for all projects (20–2000 \
     characters). No — {n, skip: \"why\"}. Record n=\"0\" lists the current Rules notes: do not \
     repeat them, it takes no result. The result does not change memory: a human accepts the \
     candidate.";
/// Схема итога вида.
pub const SCHEMA: &str = r#"{"items": [{"n": 1, "rule": {"path": "…", "title": "…", "content": "…"}, "reason": "…"}], "skip": "<причина>"?}"#;

/// Инструкция вида на языке `lang`.
pub fn instruction(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => INSTRUCTION_RU,
        Lang::En => INSTRUCTION_EN,
    }
}

/// Пространства-источники вида в базе пользователя: Knowledge, затем решения (по id) — Human и
/// Rules источниками не бывают; с видом пространства (ловушки и приёмы — только из Knowledge).
async fn source_spaces(db: &Db) -> Result<Vec<(Id, SpaceKind)>, StoreError> {
    let mut spaces: Vec<(u8, Id, SpaceKind)> = zam_store::list_spaces(db)
        .await?
        .into_iter()
        .filter_map(|s| match s.kind {
            SpaceKind::Knowledge => Some((0, s.id, s.kind)),
            SpaceKind::Solution => Some((1, s.id, s.kind)),
            _ => None,
        })
        .collect();
    spaces.sort_by_key(|(o, id, _)| (*o, *id));
    Ok(spaces.into_iter().map(|(_, id, k)| (id, k)).collect())
}

/// Вид insight `ik` — источник кандидата в пространстве вида `space` (§6.7 п. 3): решение с
/// `certainty = axiom` — из Knowledge и решений («решения владельца»), ловушка и приём — только
/// из Knowledge (полезная ловушка решения — обычно факт проекта; ревью r1, Important 1).
fn source_kind(ik: &str, axiom: bool, space: SpaceKind) -> bool {
    match ik {
        "decision" => axiom,
        "gotcha" | "pattern" => space == SpaceKind::Knowledge,
        _ => false,
    }
}

/// Кандидаты вида `rules` пространства Rules `space` пользователя `user` (§6.7 п. 3, см.
/// модуль): детерминированно (пространства — Knowledge, затем решения; записи — старые первыми),
/// не больше `limit` и свободных мест до предела открытых кандидатов, без пар `exclude` —
/// отправленных в пакетах этого запуска.
pub async fn collect(
    ctx: &NightCtx,
    user: Id,
    space: Id,
    limit: usize,
    exclude: &HashSet<(String, i64)>,
) -> Result<Vec<Candidate>, StoreError> {
    let db = ctx.st().users.get(user).await?;
    if rules_space(&db).await? != space {
        return Ok(Vec::new());
    }
    let open = pr::counts(&db).await?.rules;
    let free = pr::PROPOSALS_OPEN_MAX.saturating_sub(open) as usize;
    let limit = limit.min(free);
    if limit == 0 {
        return Ok(Vec::new());
    }
    // отсев принятых — строками `proposals` (ключ по источнику, отзыв автором не в счёт), а не
    // `accepted_refs`: принятый элемент, чью строку закрытие не записало (предел, повтор), и
    // отозванный автором источник снова кандидаты (ревью r1, Minor 5–6)
    let mut skip = store::excluded_refs(&db, WorkKind::Rules).await?;
    // отправленное в пакетах этого запуска (ночь, сутки ручных запусков) — до `limit`, как у
    // `probes` (`zam_store::works::offered_refs`, решение координатора 80)
    skip.extend(exclude.iter().cloned());
    let mut out = Vec::new();
    for (src, src_kind) in source_spaces(&db).await? {
        if out.len() >= limit || ctx.stopping() {
            break;
        }
        let rows = store::owner_entries(&db, user, src, &["insight"], &skip, usize::MAX).await?;
        if rows.is_empty() {
            continue;
        }
        let ids: Vec<Id> = rows.iter().map(|e| e.id).collect();
        let full = zam_store::live_entries_by_ids(&db, &ids).await?;
        // (запись, вид insight) — действующие решения-аксиомы, ловушки и приёмы
        let mut picked = Vec::new();
        for e in rows {
            let Some(x) = full.get(&e.id) else {
                continue;
            };
            if x.status != Some(zam_store::EntryStatus::Active) {
                continue;
            }
            let ik = x.data.get("kind").and_then(Value::as_str).unwrap_or("");
            let axiom = x.certainty == Some(zam_store::Certainty::Axiom);
            if source_kind(ik, axiom, src_kind) {
                picked.push((e, ik.to_string()));
            }
        }
        let signal_ids: Vec<Id> = picked
            .iter()
            .filter(|(_, ik)| ik != "decision")
            .map(|(e, _)| e.id)
            .collect();
        let usage = zam_store::usage_counts_many(&db, TargetKind::Entry, &signal_ids).await?;
        let opened = zam_store::opened_counts_many(&db, TargetKind::Entry, &signal_ids).await?;
        let keys: Vec<String> = picked
            .iter()
            .flat_map(|(e, _)| {
                [
                    pr::key_source(ProposalType::Rule, &e.target),
                    pr::key_source_rev(ProposalType::Rule, &e.target, e.rev),
                ]
            })
            .collect();
        let seen: HashSet<String> = pr::existing_keys(&db, ProposalType::Rule, &keys).await?;
        for (e, ik) in picked {
            if out.len() >= limit {
                break;
            }
            if seen.contains(&pr::key_source(ProposalType::Rule, &e.target))
                || seen.contains(&pr::key_source_rev(ProposalType::Rule, &e.target, e.rev))
            {
                continue;
            }
            let mut signals = vec![("insight".to_string(), ik.clone())];
            if ik == "decision" {
                signals.push(("certainty".into(), "axiom".into()));
            } else {
                let u = usage.get(&e.id).map_or(0, |c| c.useful);
                let o = opened.get(&e.id).copied().unwrap_or(0);
                if u < USEFUL_MIN && o < OPENED_MIN {
                    continue;
                }
                signals.push(("useful".into(), u.to_string()));
                signals.push(("opened".into(), o.to_string()));
            }
            out.push(Candidate {
                target: e.target,
                rev: e.rev,
                kind: e.kind,
                title: e.title,
                text: agent_text(&e.body),
                signals,
            });
        }
    }
    Ok(out)
}

/// L0 заметки в контексте пакета — не длиннее, символов (одна строка; путь — всегда целиком).
pub const CONTEXT_L0_MAX_CHARS: usize = 160;

/// Контекст пакета — не больше этой доли бюджета пакета (`budget_tokens / 4`; ревью r2, Minor 1).
pub const CONTEXT_BUDGET_SHARE: usize = 4;
/// Контекст пакета — не больше, токенов счётчика выдачи (при бюджете по умолчанию 6000 — четверть).
pub const CONTEXT_MAX_TOKENS: usize = 1500;

/// Запись контекста пакета (`n="0"`): действующие заметки Rules (до [`RULES_TITLES_MAX`]) в пределе
/// `min(budget / CONTEXT_BUDGET_SHARE, CONTEXT_MAX_TOKENS)` токенов **всей записи** (ревью r2,
/// Minor 1: запрошенный бюджет пакета контекст не съедает): сначала пути — пока помещаются, хвост —
/// строкой «ещё заметок Rules: N»; затем L0 (до [`CONTEXT_L0_MAX_CHARS`]) — пока помещаются.
/// Маска — до обрезки L0 и на весь текст (вид агента). Заметок нет или не помещается даже
/// заголовок — `None`.
pub async fn context(db: &Db, rules: Id, budget: usize) -> Result<Option<Candidate>, StoreError> {
    use crate::api::agent_view::mask;
    let notes: Vec<zam_store::Node> = zam_store::list_children(db, rules, None, 16)
        .await?
        .into_iter()
        .filter(|n| n.kind == NodeKind::Note)
        .take(RULES_TITLES_MAX)
        .collect();
    if notes.is_empty() {
        return Ok(None);
    }
    let cap = (budget / CONTEXT_BUDGET_SHARE).min(CONTEXT_MAX_TOKENS);
    // (строка пути, строка с L0)
    let lines: Vec<(String, Option<String>)> = notes
        .iter()
        .map(|n| {
            let path = format!("- {}", mask(&n.path));
            // маска — до обрезки (маска куска ≠ кусок маски)
            let l0 =
                n.l0.as_deref()
                    .map(|l| mask(l).into_owned())
                    .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|l| !l.is_empty())
                    .map(|l0| match l0.char_indices().nth(CONTEXT_L0_MAX_CHARS) {
                        Some((i, _)) => format!("{path} — {}…", &l0[..i]),
                        None => format!("{path} — {l0}"),
                    });
            (path, l0)
        })
        .collect();
    let candidate = |text: String| Candidate {
        target: CONTEXT_REF.into(),
        rev: 0,
        kind: "rules".into(),
        title: "Действующие заметки Rules — не повторяй их (итога для этой записи нет)".into(),
        text,
        signals: Vec::new(),
    };
    // токены записи целиком (как она уйдёт в пакет); `shown` — строк, `full` — с L0
    let size = |shown: usize, full: &[bool]| {
        let mut text: Vec<&str> = lines[..shown]
            .iter()
            .zip(full)
            .map(|((p, l), f)| {
                if *f {
                    l.as_deref().unwrap_or(p)
                } else {
                    p.as_str()
                }
            })
            .collect();
        let tail =
            (shown < lines.len()).then(|| format!("- ещё заметок Rules: {}", lines.len() - shown));
        if let Some(t) = &tail {
            text.push(t);
        }
        let text = text.join("\n");
        let tokens = zam_store::count_tokens(&super::package::record(0, &candidate(text.clone())));
        (tokens, text)
    };
    let mut shown = 0;
    let mut full = vec![false; lines.len()];
    while shown < lines.len() && size(shown + 1, &full).0 <= cap {
        shown += 1;
    }
    if shown == 0 && size(0, &full).0 > cap {
        return Ok(None);
    }
    let with_l0: Vec<usize> = (0..shown).filter(|&i| lines[i].1.is_some()).collect();
    for i in with_l0 {
        full[i] = true;
        if size(shown, &full).0 > cap {
            full[i] = false;
            break;
        }
    }
    Ok(Some(candidate(size(shown, &full).1)))
}

/// Строка поля `key` объекта `v` (нет или не строка — пусто).
fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Текст правила из элемента: `title` — заголовком `# …`, если в `content` своего заголовка нет.
fn compose(title: &str, content: &str) -> String {
    let (t, c) = (title.trim(), content.trim());
    if t.is_empty() || !zam_markdown::parse(c).headings.is_empty() {
        return c.to_string();
    }
    format!("# {t}\n\n{c}")
}

/// Сессия и автор строки `proposals` по автору итога.
fn author_of(a: &Author) -> (ProposalAuthor, String, Option<String>) {
    match a {
        Author::Agent(s) => (ProposalAuthor::Agent, s.clone(), Some(s.clone())),
        Author::Model(_) => (ProposalAuthor::Model, a.label(), None),
    }
}

/// Проверка элемента итога `{n, rule: {path, title?, content}, reason, target?}` — та же, что у
/// спонтанного кандидата ([`check_candidate`]); `prior` — записи принятых раньше элементов этого
/// итога (тот же путь — отказ, место до предела — вместе с ними); принятый — [`ItemWrite::Rule`]
/// (запись — в транзакции закрытия работы).
pub async fn accept(
    ctx: &ItemCtx<'_>,
    n: usize,
    item: &Value,
    prior: &[ItemWrite],
) -> Result<Vec<ItemWrite>, Reject> {
    let Some(it) = ctx.work.item(n) else {
        return Err(Reject::new("нет элемента с таким n в пакете"));
    };
    if let Some(t) = item.get("target").and_then(Value::as_str)
        && t != it.target
    {
        return Err(Reject::new(format!(
            "target {t} — не запись элемента {n} пакета"
        )));
    }
    let Some(rule) = item.get("rule").filter(|r| r.is_object()) else {
        return Err(Reject::new(
            "нет rule: {path, title, content} — или {n, skip: \"почему\"}, если правила нет",
        ));
    };
    let content = compose(str_of(rule, "title"), str_of(rule, "content"));
    let reason = match str_of(item, "reason") {
        "" => str_of(rule, "reason"),
        r => r,
    };
    let c = check_candidate(
        ctx.st,
        ctx.db,
        ctx.user,
        str_of(rule, "path"),
        &content,
        reason,
    )
    .await
    .map_err(|e| Reject::new(e.reason()))?;
    // запись, которой не будет, — отказ с причиной уже здесь (ревью r1, Minor 6); гонка между
    // проверкой и закрытием работы — предупреждение итога (`zam_store::works::finish_claimed`)
    let store_failed = |e: StoreError| Reject::new(RuleError::from(e).reason());
    // принятые раньше кандидаты этого итога (`prior`): тот же путь — повтор внутри итога, место —
    // вместе с ними
    let earlier: Vec<&str> = prior
        .iter()
        .filter_map(|x| match x {
            ItemWrite::Rule { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    if earlier.contains(&c.path.as_str()) {
        return Err(Reject::new(format!(
            "путь rules:/{} уже у другого элемента этого итога — предложите другой путь",
            c.path
        )));
    }
    let open = pr::counts(ctx.db).await.map_err(store_failed)?.rules as usize;
    if open + earlier.len() >= pr::PROPOSALS_OPEN_MAX as usize {
        return Err(Reject::new(format!(
            "открытых кандидатов в Rules — {}: новый не записывается, пока человек не решит по \
             прежним",
            pr::PROPOSALS_OPEN_MAX
        )));
    }
    let key = pr::key_source(ProposalType::Rule, &it.target);
    if !pr::existing_keys(ctx.db, ProposalType::Rule, std::slice::from_ref(&key))
        .await
        .map_err(store_failed)?
        .is_empty()
    {
        return Err(Reject::new(format!(
            "у источника {} уже есть предложение — повтор не записывается",
            it.target
        )));
    }
    let (author_kind, author, session) = author_of(ctx.author);
    Ok(vec![ItemWrite::Rule {
        target: it.target.clone(),
        rev: it.rev,
        path: c.path.as_str().to_string(),
        content: c.content,
        reason: c.reason,
        author_kind,
        author,
        session,
    }])
}

/// Отказ исполнителя от элемента `{n, skip}` — строка `skipped` (отсев до новой ревизии
/// источника, §6.7 п. 3).
pub fn skip(ctx: &ItemCtx<'_>, item: &store::WorkItem, why: &str) -> Vec<ItemWrite> {
    let (author_kind, author, session) = author_of(ctx.author);
    vec![ItemWrite::RuleSkip {
        target: item.target.clone(),
        rev: item.rev,
        why: why.to_string(),
        author_kind,
        author,
        session,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// URL — схема с `://`, `www.`, `mailto:`; похожие слова — нет.
    #[test]
    fn url_cases() {
        for yes in [
            "см. https://example.com",
            "HTTP://EXAMPLE.COM",
            "ftp://h/x",
            "<https://example.com>",
            "[док](https://example.com/a)",
            "иди на www.example.com",
            "пиши mailto:a@example.com",
        ] {
            assert!(has_url(yes), "{yes}");
        }
        for no in [
            "правило без адресов",
            "://без схемы",
            "awww.нет",
            "путь a/b.rs и `cargo test`",
            "e-mail без схемы",
        ] {
            assert!(!has_url(no), "{no}");
        }
    }

    /// Путь Rules: префикс `rules:` снимается, чужое пространство и недопустимый путь — отказ.
    #[test]
    fn rule_path_cases() {
        assert_eq!(rule_path("rules:/a/b").unwrap().as_str(), "a/b");
        assert_eq!(rule_path("/a/b/").unwrap().as_str(), "a/b");
        assert_eq!(rule_path("a").unwrap().as_str(), "a");
        for bad in ["knowledge:/x", "knowledge:x", "", "a//b", "a/../b", "x:/y"] {
            assert!(
                matches!(rule_path(bad), Err(RuleError::BadPath(_))),
                "{bad}"
            );
        }
    }

    /// Текст: пустой, маска, URL, блок кода — отказ; список с отступом и инлайн-код — нет.
    #[test]
    fn check_text_cases() {
        assert!(check_text("Запускай `cargo test` перед коммитом.").is_ok());
        assert!(check_text("- a\n    - b").is_ok());
        for bad in [
            "  ",
            "ключ ‹secret›",
            "см. https://e.example",
            "a\n\n```\nx\n```",
        ] {
            assert!(check_text(bad).is_err(), "{bad}");
        }
    }

    /// Пометка команд shell для человека.
    #[test]
    fn shell_cases() {
        assert!(has_shell("Запусти:\n$ cargo test"));
        assert!(has_shell("через PowerShell"));
        assert!(has_shell("cmd /c dir"));
        assert!(!has_shell("Пиши итог в память."));
    }

    /// Заголовок в текст — только если своего нет.
    #[test]
    fn compose_cases() {
        assert_eq!(compose("Имя", "Текст."), "# Имя\n\nТекст.");
        assert_eq!(compose("Имя", "# Свой\n\nТекст."), "# Свой\n\nТекст.");
        assert_eq!(compose("", "Текст."), "Текст.");
        // имя заметки — не заголовок (ревью r1, Minor 2)
        assert_eq!(rule_title("Текст."), None);
        assert_eq!(
            rule_title("# Ревью кода\n\nТекст.").as_deref(),
            Some("Ревью кода")
        );
        assert_eq!(rule_title("# —\n\nТекст."), None);
    }

    /// Источники: аксиома-решение — из Knowledge и решений, ловушка и приём — только из Knowledge
    /// (ревью r1, Important 1); решение без аксиомы и прочие виды — нет.
    #[test]
    fn source_kind_cases() {
        let (k, s) = (SpaceKind::Knowledge, SpaceKind::Solution);
        assert!(source_kind("decision", true, k) && source_kind("decision", true, s));
        assert!(!source_kind("decision", false, k));
        assert!(source_kind("gotcha", false, k) && source_kind("pattern", false, k));
        assert!(!source_kind("gotcha", false, s) && !source_kind("pattern", true, s));
        assert!(!source_kind("discovery", true, k));
    }

    /// Сбой хранилища при проверке — 500 у REST (не 422 «не повторяй»), у элемента работы —
    /// причина «повторите позже» (ревью r1, Minor 3).
    #[test]
    fn store_failure_is_server_error() {
        let e = RuleError::from(StoreError::Conflict("x".into()));
        assert!(e.reason().contains("повторите позже"), "{}", e.reason());
        assert!(matches!(ApiError::from(e), ApiError::Internal(_)));
    }

    /// Инструкции вида — в пределе 300 токенов.
    #[test]
    fn instruction_within_budget() {
        for t in [INSTRUCTION_RU, INSTRUCTION_EN] {
            assert!(
                zam_store::count_tokens(t) <= 300,
                "{}",
                zam_store::count_tokens(t)
            );
        }
    }
}
