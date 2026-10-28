//! Вопросы к человеку (спецификация M4.5 §4): записи `question` — вопрос в `title`, контекст в
//! `body`, варианты и ответ в `data`.
//!
//! - [`QuestionData`] — `data` вопроса: `options` (до [`OPTIONS_MAX`]) и `recommendation` (ключ
//!   одного из вариантов) ведёт спросивший; `answer` — только сервер (клиентская `data` с ним
//!   отклоняется). Правка `data` — слияние на верхнем уровне (`null` снимает поле);
//! - статусы: `open` → `answered` (только ответом, [`answer_question`]) или `obsolete`
//!   (правка статуса, [`check_question_transition`]: с причиной; из `open` и `answered`);
//! - [`answer_question`] — одна транзакция писателя: вопрос → `answered` с `data.answer` (новая
//!   ревизия), новый insight `decision` (`certainty: axiom` при `by: human`, иначе `hypothesis`;
//!   `data.decision.decided_by`) и ребро `derived_from` решение → вопрос. Повторный ответ —
//!   [`QuestionError::AlreadyAnswered`] с id решения;
//! - [`question_duplicates`] — вопросы `open` и `answered` с тем же нормализованным заголовком
//!   (для `answered` — с ответом);
//! - чтение для дайджеста: [`recent_answered`], [`open_questions`].

use jiff::{SignedDuration, Timestamp};
use rusqlite::{params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zam_core::{Id, TargetKind, TargetRef};

use crate::entries::{
    Certainty, EntryKind, EntryStatus, NewEntry, TITLE_MAX_CHARS, load_entry, normalize_title,
};
use crate::graph::EdgeType;
use crate::nodes::Attribution;
use crate::time::{fmt_ts, ser_ts};
use crate::{Db, Entry, StoreError};

/// Вариантов ответа у вопроса.
pub const OPTIONS_MAX: usize = 8;
/// Длина ключа варианта, символов.
pub const OPTION_KEY_MAX_CHARS: usize = 32;
/// Длина текста варианта, символов.
pub const OPTION_TEXT_MAX_CHARS: usize = 500;
/// Предел текста ответа, символов **[решение исполнителя]**: в спецификации не назван.
pub const ANSWER_TEXT_MAX_CHARS: usize = 10_000;
/// Кратко ответа в заголовке решения: символов свободного текста (спецификация §4 и бриф).
pub const TITLE_ANSWER_CHARS: usize = 80;
/// Предел текста варианта в заголовке решения **[решение исполнителя]**: заголовок — до 500.
const TITLE_OPTION_CHARS: usize = 200;
/// Кандидатов-дублей вопроса.
pub const QUESTION_DUPLICATES_MAX: usize = 5;

/// Вариант ответа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QOption {
    pub key: String,
    pub text: String,
}

/// Кто ответил: человек (в том числе слова человека, записанные агентом) или агент сам.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerBy {
    Human,
    Agent,
}

impl AnswerBy {
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerBy::Human => "human",
            AnswerBy::Agent => "agent",
        }
    }
}

/// Канал ответа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Webui,
    Chat,
    Cli,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Webui => "webui",
            Channel::Chat => "chat",
            Channel::Cli => "cli",
        }
    }
}

/// Ответ на вопрос (`data.answer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub text: Option<String>,
    pub option: Option<String>,
    pub by: AnswerBy,
    pub channel: Channel,
    #[serde(serialize_with = "ser_ts")]
    pub answered_at: Timestamp,
    pub decision_id: Id,
}

impl Answer {
    /// Ответ кратко: текст выбранного варианта, иначе свободный текст, иначе ключ варианта.
    pub fn brief(&self, options: &[QOption]) -> String {
        if let Some(o) = self
            .option
            .as_deref()
            .and_then(|k| options.iter().find(|o| o.key == k))
        {
            return o.text.clone();
        }
        self.text
            .clone()
            .or_else(|| self.option.clone())
            .unwrap_or_default()
    }
}

/// Разобранная `data` вопроса.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionData {
    pub options: Vec<QOption>,
    pub recommendation: Option<String>,
    pub answer: Option<Answer>,
}

/// Поля, которые ведёт спросивший.
const CLIENT_KEYS: [&str; 2] = ["options", "recommendation"];

fn options_field(v: &Value) -> Result<Vec<QOption>, String> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    let a = v
        .as_array()
        .ok_or_else(|| "data.options: ожидается массив {key, text}".to_string())?;
    if a.len() > OPTIONS_MAX {
        return Err(format!("data.options: не больше {OPTIONS_MAX} вариантов"));
    }
    let mut out: Vec<QOption> = Vec::with_capacity(a.len());
    for (i, x) in a.iter().enumerate() {
        let o = x
            .as_object()
            .filter(|o| o.keys().all(|k| k == "key" || k == "text"))
            .ok_or_else(|| format!("data.options[{i}]: объект {{key, text}}"))?;
        let get = |k: &str| o.get(k).and_then(Value::as_str).map(str::trim);
        let key = get("key")
            .filter(|s| {
                !s.is_empty()
                    && s.chars().count() <= OPTION_KEY_MAX_CHARS
                    && !s.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                format!("data.options[{i}].key: от 1 до {OPTION_KEY_MAX_CHARS} символов")
            })?;
        let text = get("text")
            .filter(|s| !s.is_empty() && s.chars().count() <= OPTION_TEXT_MAX_CHARS)
            .ok_or_else(|| {
                format!("data.options[{i}].text: от 1 до {OPTION_TEXT_MAX_CHARS} символов")
            })?;
        if out.iter().any(|o| o.key == key) {
            return Err(format!("data.options[{i}].key: «{key}» повторяется"));
        }
        out.push(QOption {
            key: key.to_string(),
            text: text.to_string(),
        });
    }
    Ok(out)
}

impl QuestionData {
    /// `data` из базы; повреждённые поля пропускаются (запись всегда читается).
    pub fn from_stored(data: &Value) -> QuestionData {
        let get = |k: &str| data.get(k).filter(|v| !v.is_null());
        QuestionData {
            options: get("options")
                .and_then(|v| options_field(v).ok())
                .unwrap_or_default(),
            recommendation: get("recommendation")
                .and_then(Value::as_str)
                .map(str::to_string),
            answer: get("answer").and_then(|v| serde_json::from_value(v.clone()).ok()),
        }
    }

    /// Применяет клиентский патч `data`: `options`, `recommendation`; `null` снимает поле.
    /// `answer` ведёт сервер, прочие ключи неизвестны — ошибка `data.<поле>: …` (422).
    /// `recommendation` должна быть ключом варианта (проверяется на итоговом наборе).
    pub fn apply_patch(&mut self, patch: &Value) -> Result<(), String> {
        let o = patch
            .as_object()
            .ok_or_else(|| "data должно быть JSON-объектом".to_string())?;
        for k in o.keys() {
            if k == "answer" {
                return Err("data.answer: поле ведёт сервер (ответ — POST …/answer)".into());
            }
            if !CLIENT_KEYS.contains(&k.as_str()) {
                return Err(format!("data.{k}: неизвестное поле"));
            }
        }
        let mut next = self.clone();
        for (k, v) in o {
            match k.as_str() {
                "options" => next.options = options_field(v)?,
                "recommendation" => {
                    next.recommendation = match v {
                        Value::Null => None,
                        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                        Value::String(_) => None,
                        _ => return Err("data.recommendation: ожидается ключ варианта".into()),
                    }
                }
                _ => unreachable!("ключи проверены выше"),
            }
        }
        if let Some(r) = &next.recommendation
            && !next.options.iter().any(|o| &o.key == r)
        {
            return Err(format!(
                "data.recommendation: «{r}» — не ключ ни одного из вариантов"
            ));
        }
        *self = next;
        Ok(())
    }

    /// Нормализованная `data` для записи: только заданные поля.
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        if !self.options.is_empty() {
            o.insert("options".into(), json!(self.options));
        }
        if let Some(r) = &self.recommendation {
            o.insert("recommendation".into(), json!(r));
        }
        if let Some(a) = &self.answer {
            o.insert("answer".into(), json!(a));
        }
        Value::Object(o)
    }
}

/// Нормализация клиентской `data` нового вопроса: `{}` + патч.
pub fn check_question(data: &Value) -> Result<QuestionData, String> {
    let mut q = QuestionData::default();
    q.apply_patch(data)?;
    Ok(q)
}

/// Смена статуса вопроса правкой: только `obsolete`, из `open` и `answered`, с причиной. `Err` —
/// машинный код: `reason-required`, `answer-only` (`answered` ставится ответом),
/// `transition-not-allowed` (из `obsolete` статус не меняется) или `not-a-question-status`.
pub fn check_question_transition(
    from: EntryStatus,
    to: EntryStatus,
    reason: Option<&str>,
) -> Result<(), &'static str> {
    use EntryStatus::*;
    if !matches!(from, Open | Answered | Obsolete) || !matches!(to, Open | Answered | Obsolete) {
        return Err("not-a-question-status");
    }
    if from == to {
        return Ok(());
    }
    match (from, to) {
        (_, Answered) => Err("answer-only"),
        (Obsolete, _) | (Answered, Open) => Err("transition-not-allowed"),
        (_, Obsolete) => {
            if reason.is_some_and(|r| !r.trim().is_empty()) {
                Ok(())
            } else {
                Err("reason-required")
            }
        }
        _ => Err("transition-not-allowed"),
    }
}

/// Ответ на вопрос, как его присылает клиент.
#[derive(Debug, Clone)]
pub struct NewAnswer {
    pub text: Option<String>,
    pub option: Option<String>,
    pub by: AnswerBy,
    pub channel: Channel,
}

/// Ошибки операций над вопросом.
#[derive(Debug, thiserror::Error)]
pub enum QuestionError {
    /// Вопрос уже отвечен: 409 `already-answered`, `decision_id` — решение из первого ответа.
    #[error("вопрос уже отвечен")]
    AlreadyAnswered { decision_id: Option<Id> },
    /// Вопрос устарел (`obsolete`): 409.
    #[error("вопрос устарел: отвечать на него нельзя")]
    Obsolete,
    /// `option` — не ключ ни одного из вариантов: 422.
    #[error("option: «{0}» — нет такого варианта")]
    UnknownOption(String),
    /// Нет ни `text`, ни `option`: 422.
    #[error("ответ пуст: нужен text или option")]
    Empty,
    /// Запись не вопрос: 422.
    #[error("запись не является вопросом")]
    NotQuestion,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Усечение до `max` символов с многоточием; переводы строк — пробелами.
fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Заголовок решения: «<вопрос> → <ответ кратко>», не длиннее [`TITLE_MAX_CHARS`].
fn decision_title(question: &str, brief: &str, from_option: bool) -> String {
    let max = if from_option {
        TITLE_OPTION_CHARS
    } else {
        TITLE_ANSWER_CHARS
    };
    let answer = clip(brief, max);
    let room = TITLE_MAX_CHARS.saturating_sub(answer.chars().count() + 3);
    format!("{} → {answer}", clip(question, room.max(1)))
}

/// Тело решения: вопрос с контекстом, варианты, ответ.
fn decision_body(q: &Entry, qd: &QuestionData, a: &Answer) -> String {
    let mut s = format!("Вопрос: {}\n", q.title.trim());
    if !q.body.trim().is_empty() {
        s.push_str(&format!("\n{}\n", q.body.trim()));
    }
    if !qd.options.is_empty() {
        s.push_str("\nВарианты:\n");
        for o in &qd.options {
            let star = if qd.recommendation.as_deref() == Some(o.key.as_str()) {
                " (рекомендовано)"
            } else {
                ""
            };
            s.push_str(&format!("- {}: {}{star}\n", o.key, o.text));
        }
    }
    s.push_str("\nОтвет: ");
    let mut parts: Vec<String> = Vec::new();
    if let Some(k) = &a.option {
        let t = qd.options.iter().find(|o| &o.key == k).map(|o| &o.text);
        parts.push(match t {
            Some(t) => format!("{k} — {t}"),
            None => k.clone(),
        });
    }
    if let Some(t) = &a.text {
        parts.push(t.clone());
    }
    s.push_str(&parts.join("\n"));
    s.push_str(&format!(
        "\n\nОтветил: {} (канал: {}).",
        match a.by {
            AnswerBy::Human => "человек",
            AnswerBy::Agent => "агент",
        },
        a.channel.as_str()
    ));
    s
}

/// Отвечает на вопрос одной транзакцией: статус `answered` и `data.answer` (новая ревизия),
/// новый insight `decision` и ребро `derived_from` решение → вопрос. Возвращает вопрос и
/// решение. Ошибки — [`QuestionError`]; второй ответ — `AlreadyAnswered` с id решения первого.
pub async fn answer_question(
    db: &Db,
    id: Id,
    a: NewAnswer,
    attrib: &Attribution,
) -> Result<(Entry, Entry), QuestionError> {
    let attrib = attrib.clone();
    let text = a
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let option = a
        .option
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    if text.is_none() && option.is_none() {
        return Err(QuestionError::Empty);
    }
    if text
        .as_deref()
        .is_some_and(|t| t.chars().count() > ANSWER_TEXT_MAX_CHARS)
    {
        return Err(StoreError::BadInput(format!(
            "text: не больше {ANSWER_TEXT_MAX_CHARS} символов"
        ))
        .into());
    }
    db.write(move |tx| {
        let q = load_entry(tx, id)?;
        if q.deleted_at.is_some() {
            return Err(StoreError::NotFound);
        }
        if q.kind != EntryKind::Question {
            return Ok(Err(QuestionError::NotQuestion));
        }
        let mut qd = QuestionData::from_stored(&q.data);
        match q.status.unwrap_or(EntryStatus::Open) {
            EntryStatus::Answered => {
                return Ok(Err(QuestionError::AlreadyAnswered {
                    decision_id: qd.answer.map(|a| a.decision_id),
                }));
            }
            EntryStatus::Obsolete => return Ok(Err(QuestionError::Obsolete)),
            _ => {}
        }
        if let Some(k) = &option
            && !qd.options.iter().any(|o| &o.key == k)
        {
            return Ok(Err(QuestionError::UnknownOption(k.clone())));
        }
        let now_ts = Timestamp::now();
        let now = fmt_ts(now_ts);
        let mut answer = Answer {
            text,
            option,
            by: a.by,
            channel: a.channel,
            answered_at: now_ts,
            // подставляется ниже: id решения известен после его вставки
            decision_id: id,
        };
        let from_option = answer
            .option
            .as_deref()
            .is_some_and(|k| qd.options.iter().any(|o| o.key == k));
        let brief = answer.brief(&qd.options);
        let decision = NewEntry {
            refs: Vec::new(),
            space_id: q.space_id,
            kind: EntryKind::Insight,
            title: decision_title(&q.title, &brief, from_option),
            body: decision_body(&q, &qd, &answer),
            data: json!({"kind": "decision", "decision": {"decided_by": a.by.as_str()}}),
            area: q.area.clone(),
            tags: q.tags.clone(),
            status: None,
            certainty: Some(match a.by {
                AnswerBy::Human => Certainty::Axiom,
                AnswerBy::Agent => Certainty::Hypothesis,
            }),
            idempotency_key: None,
            triggers: Vec::new(),
            links: vec![(
                EdgeType::DerivedFrom,
                TargetRef {
                    kind: TargetKind::Entry,
                    id,
                },
            )],
        };
        let decision_id = crate::entries::insert_entry(tx, decision, None, &attrib, None, &now)?;
        answer.decision_id = decision_id;
        qd.answer = Some(answer);
        crate::entries::revise(
            tx,
            &q,
            Some(EntryStatus::Answered),
            qd.to_value(),
            &attrib,
            &now,
        )?;
        Ok(Ok((load_entry(tx, id)?, load_entry(tx, decision_id)?)))
    })
    .await?
}

/// Кандидат-дубль вопроса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionCandidate {
    pub id: Id,
    pub title: String,
    pub status: EntryStatus,
    /// Ответ — у `answered`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    pub reason: &'static str,
}

/// Вопросы пространства `space` в статусах `open` и `answered` с тем же нормализованным
/// заголовком — новые первыми, не больше [`QUESTION_DUPLICATES_MAX`]. Заголовок без букв и
/// цифр дублей не имеет.
pub async fn question_duplicates(
    db: &Db,
    space: Id,
    title: &str,
) -> Result<Vec<QuestionCandidate>, StoreError> {
    let norm = normalize_title(title);
    if norm.is_empty() {
        return Ok(Vec::new());
    }
    db.read(move |c| {
        let mut st = c.prepare_cached(&format!(
            "SELECT id FROM entries
             WHERE space_id = ?1 AND kind = 'question' AND title_norm = ?2
               AND deleted_at IS NULL AND status IN ('open', 'answered')
             ORDER BY created_at DESC, id DESC LIMIT {QUESTION_DUPLICATES_MAX}"
        ))?;
        let ids = st
            .query_map(params![space, norm], |r| r.get::<_, Id>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let e = load_entry(c, id)?;
                Ok(QuestionCandidate {
                    id,
                    title: e.title,
                    status: e.status.unwrap_or(EntryStatus::Open),
                    answer: QuestionData::from_stored(&e.data).answer,
                    reason: "title",
                })
            })
            .collect()
    })
    .await
}

fn in_list(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Отвеченные вопросы пространств `spaces`, чей ответ дан не раньше `since` — по моменту
/// ответа, новые первыми, не больше `limit`.
pub async fn recent_answered(
    db: &Db,
    spaces: &[Id],
    since: Timestamp,
    limit: usize,
) -> Result<Vec<Entry>, StoreError> {
    if spaces.is_empty() {
        return Ok(Vec::new());
    }
    let spaces = spaces.to_vec();
    let since = fmt_ts(since);
    db.read(move |c| {
        let n = spaces.len();
        let mut st = c.prepare(&format!(
            "SELECT id FROM entries
             WHERE kind = 'question' AND status = 'answered' AND deleted_at IS NULL
               AND space_id IN ({}) AND json_extract(data, '$.answer.answered_at') >= ?{}
             ORDER BY json_extract(data, '$.answer.answered_at') DESC, id DESC LIMIT {limit}",
            in_list(n),
            n + 1
        ))?;
        let mut args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
        args.push(since);
        let ids = st
            .query_map(params_from_iter(args), |r| r.get::<_, Id>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| load_entry(c, id)).collect()
    })
    .await
}

/// Открытые вопросы пространств `spaces`: их число и `(created_at, заголовок)` до `titles`
/// новейших (новые первыми). Вызывающий, объединяющий несколько баз, сортирует пары сам.
pub async fn open_questions(
    db: &Db,
    spaces: &[Id],
    titles: usize,
) -> Result<(usize, Vec<(Timestamp, String)>), StoreError> {
    if spaces.is_empty() {
        return Ok((0, Vec::new()));
    }
    let spaces = spaces.to_vec();
    db.read(move |c| {
        let n = spaces.len();
        let args: Vec<String> = spaces.iter().map(ToString::to_string).collect();
        let count: i64 = c
            .prepare(&format!(
                "SELECT COUNT(*) FROM entries
                 WHERE kind = 'question' AND status = 'open' AND deleted_at IS NULL
                   AND space_id IN ({})",
                in_list(n)
            ))?
            .query_row(params_from_iter(args.clone()), |r| r.get(0))?;
        let mut st = c.prepare(&format!(
            "SELECT created_at, title FROM entries
             WHERE kind = 'question' AND status = 'open' AND deleted_at IS NULL
               AND space_id IN ({})
             ORDER BY created_at DESC, id DESC LIMIT {titles}",
            in_list(n)
        ))?;
        let t = st
            .query_map(params_from_iter(args), |r| {
                Ok((crate::time::ts_at(r, 0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((usize::try_from(count).unwrap_or(0), t))
    })
    .await
}

/// Отметка срока: `since` для «последних N дней».
pub fn days_ago(now: Timestamp, days: i64) -> Timestamp {
    now - SignedDuration::from_hours(24 * days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use EntryStatus::*;

    #[test]
    fn data_rules() {
        let q = check_question(&json!({
            "options": [{"key": " a ", "text": "Да"}, {"key": "b", "text": "Нет"}],
            "recommendation": "a"
        }))
        .unwrap();
        assert_eq!(q.options[0].key, "a");
        assert_eq!(q.recommendation.as_deref(), Some("a"));
        assert_eq!(QuestionData::from_stored(&q.to_value()), q);
        assert_eq!(QuestionData::default().to_value(), json!({}));
        let nine: Vec<Value> = (0..9)
            .map(|i| json!({"key": format!("k{i}"), "text": "t"}))
            .collect();
        for bad in [
            json!({"options": nine}),
            json!({"options": [{"key": "a", "text": "x"}, {"key": "a", "text": "y"}]}),
            json!({"options": [{"key": "", "text": "x"}]}),
            json!({"options": [{"key": "k".repeat(33), "text": "x"}]}),
            json!({"options": [{"key": "a", "text": "x".repeat(501)}]}),
            json!({"options": [{"key": "a"}]}),
            json!({"options": "a"}),
            json!({"recommendation": "z"}),
            json!({"options": [{"key": "a", "text": "x"}], "recommendation": "z"}),
            json!({"answer": {}}),
            json!({"foo": 1}),
            json!([]),
        ] {
            assert!(check_question(&bad).is_err(), "{bad}");
        }
        // патч: рекомендация проверяется по итоговому набору, null снимает
        let mut q = check_question(&json!({"options": [{"key": "a", "text": "x"}]})).unwrap();
        q.apply_patch(&json!({"recommendation": "a"})).unwrap();
        assert!(q.apply_patch(&json!({"options": null})).is_err());
        q.apply_patch(&json!({"recommendation": null, "options": null}))
            .unwrap();
        assert_eq!(q, QuestionData::default());
    }

    #[test]
    fn transitions() {
        assert_eq!(check_question_transition(Open, Obsolete, Some("r")), Ok(()));
        assert_eq!(
            check_question_transition(Answered, Obsolete, Some("r")),
            Ok(())
        );
        assert_eq!(
            check_question_transition(Open, Obsolete, None),
            Err("reason-required")
        );
        assert_eq!(
            check_question_transition(Open, Obsolete, Some(" ")),
            Err("reason-required")
        );
        assert_eq!(
            check_question_transition(Open, Answered, Some("r")),
            Err("answer-only")
        );
        assert_eq!(
            check_question_transition(Obsolete, Open, Some("r")),
            Err("transition-not-allowed")
        );
        assert_eq!(
            check_question_transition(Answered, Open, Some("r")),
            Err("transition-not-allowed")
        );
        assert_eq!(
            check_question_transition(Open, Active, None),
            Err("not-a-question-status")
        );
    }

    #[test]
    fn titles() {
        assert_eq!(
            decision_title("Какая БД?", "SQLite", true),
            "Какая БД? → SQLite"
        );
        let long = "я".repeat(300);
        let t = decision_title("Вопрос", &long, false);
        assert_eq!(
            t.chars().count(),
            "Вопрос → ".chars().count() + TITLE_ANSWER_CHARS
        );
        assert!(t.ends_with('…'));
        let q = "в".repeat(600);
        assert!(decision_title(&q, &long, true).chars().count() <= TITLE_MAX_CHARS);
    }

    #[test]
    fn brief_prefers_option_text() {
        let o = vec![QOption {
            key: "a".into(),
            text: "Вариант А".into(),
        }];
        let mut a = Answer {
            text: Some("свой".into()),
            option: Some("a".into()),
            by: AnswerBy::Human,
            channel: Channel::Webui,
            answered_at: Timestamp::UNIX_EPOCH,
            decision_id: Id::new(),
        };
        assert_eq!(a.brief(&o), "Вариант А");
        a.option = None;
        assert_eq!(a.brief(&o), "свой");
    }
}
