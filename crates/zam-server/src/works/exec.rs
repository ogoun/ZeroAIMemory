//! Исполнение пакета работы подключённой моделью (M7.3, спецификация §6.2 пп. 4, 6; §6.3 п. 3):
//! пакет → второй скан → вызов → разбор JSON с одним повтором.
//!
//! - **Пакет** ([`Payload`]) — инструкция и схема итога (системное сообщение) и элементы
//!   (пользовательское); элементы собирает построитель пакета вида работы (маска агента, границы
//!   `<record>` — задача 3).
//! - **Второй скан** ([`second_scan`]): тело запроса проходит `zam_secrets::scan_secrets`;
//!   элемент с находкой выбрасывается, остальное уходит (решение координатора п. 39); находка в
//!   постоянном тексте инструкции — ошибка кода (`secret_in_payload`).
//! - **JSON** ([`extract_json`], [`json_step`]): первый блок кода `json` или первый
//!   сбалансированный объект текста; не разобрался — **один** повтор той же беседы с сообщением
//!   «ответ не JSON: <ошибка разбора>; верни только JSON по схеме», второй сбой — `bad_json`.
//!
//! Этап 1 задачи 2 — чистые части без настроек; `run_payload` (пределы, журнал отправок, пауза,
//! аудит `key_use`, повторы `rate`/`unavailable`, вызов в `spawn_blocking`) — этап 2.

use std::collections::BTreeSet;

use serde_json::Value;

use super::model::{CallClass, CallError, ChatMessage, ModelReply};

/// Разделитель элементов пользовательского сообщения.
const ITEM_SEPARATOR: &str = "\n\n";
/// Предел текста ошибки разбора в сообщении повтора, символов.
const REPAIR_ERROR_MAX_CHARS: usize = 200;

/// Пакет работы для модели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// Инструкция вида (постоянный текст).
    pub instruction: String,
    /// Схема итога (постоянный текст).
    pub schema: String,
    pub items: Vec<PayloadItem>,
}

/// Элемент пакета: номер `n` (как в `<record n="…">`) и готовый текст элемента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadItem {
    pub n: usize,
    pub text: String,
}

impl Payload {
    /// Системное сообщение: инструкция и схема.
    pub fn system(&self) -> String {
        format!("{}{ITEM_SEPARATOR}{}", self.instruction, self.schema)
    }

    /// Пользовательское сообщение: элементы через пустую строку.
    pub fn user(&self) -> String {
        assemble(&self.items).0
    }
}

/// Пакет после второго скана: что уходит и номера выброшенных элементов (по возрастанию).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub payload: Payload,
    pub dropped: Vec<usize>,
}

/// Пользовательское сообщение и байтовые диапазоны элементов в нём.
fn assemble(items: &[PayloadItem]) -> (String, Vec<std::ops::Range<usize>>) {
    let mut out = String::new();
    let mut spans = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(ITEM_SEPARATOR);
        }
        let start = out.len();
        out.push_str(&it.text);
        spans.push(start..out.len());
    }
    (out, spans)
}

/// Второй скан тела запроса (§6.2 п. 6): находка в системном сообщении — `secret_in_payload`
/// (ошибка кода: постоянный текст); элементы, которых касается находка в собранном
/// пользовательском сообщении, выбрасываются, и скан повторяется по новому телу, пока находок
/// нет — уходит ровно то, что сканер признал чистым.
pub fn second_scan(mut payload: Payload) -> Result<Scanned, CallError> {
    if !zam_secrets::scan_secrets(&payload.system()).is_empty() {
        return Err(CallError::new(CallClass::SecretInPayload));
    }
    let mut dropped = Vec::new();
    loop {
        let (user, spans) = assemble(&payload.items);
        let hits = zam_secrets::scan_secrets(&user);
        if hits.is_empty() {
            break;
        }
        let touched: BTreeSet<usize> = hits
            .iter()
            .flat_map(|h| {
                spans
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| h.range.start < s.end && s.start < h.range.end)
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>()
            })
            .collect();
        // находка только в разделителях невозможна (постоянный текст); на всякий случай — как
        // находка в инструкции
        if touched.is_empty() {
            return Err(CallError::new(CallClass::SecretInPayload));
        }
        for i in touched.iter().rev() {
            dropped.push(payload.items.remove(*i).n);
        }
    }
    dropped.sort_unstable();
    Ok(Scanned { payload, dropped })
}

/// Рассуждение `<think>…</think>` в начале ответа (локальные серверы без разбора рассуждений)
/// ответом не считается.
fn strip_think(text: &str) -> &str {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<think>")
        && let Some((_, after)) = rest.split_once("</think>")
    {
        return after;
    }
    text
}

/// Содержимое первого блока кода с языком `json`.
fn first_json_fence(t: &str) -> Option<&str> {
    let mut rest = t;
    while let Some(i) = rest.find("```") {
        let (info, body) = rest[i + 3..].split_once('\n')?;
        let end = body.find("```")?;
        if info.trim().eq_ignore_ascii_case("json") {
            return Some(&body[..end]);
        }
        rest = &body[end + 3..];
    }
    None
}

/// Первый сбалансированный объект `{…}` текста (строки JSON с экранированием учитываются).
fn first_balanced_object(t: &str) -> Option<&str> {
    let start = t.find('{')?;
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (i, &c) in t.as_bytes().iter().enumerate().skip(start) {
        if in_str {
            match c {
                _ if esc => esc = false,
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&t[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_object(s: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(s.trim()) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err("ответ — не JSON-объект".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// JSON-объект ответа (§6.2 п. 4): первый блок кода `json`, иначе первый сбалансированный объект
/// текста; ошибка — текст для сообщения повтора.
pub fn extract_json(text: &str) -> Result<Value, String> {
    let t = strip_think(text);
    if let Some(block) = first_json_fence(t) {
        return parse_object(block);
    }
    match first_balanced_object(t) {
        Some(obj) => parse_object(obj),
        None => Err("в ответе нет JSON-объекта".into()),
    }
}

/// Сообщение повтора (§6.2 п. 4).
pub fn repair_prompt(err: &str) -> String {
    let err: String = err.chars().take(REPAIR_ERROR_MAX_CHARS).collect();
    format!("ответ не JSON: {err}; верни только JSON по схеме")
}

/// Что делать после ответа модели.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonStep {
    /// Итог разобран.
    Done(Value),
    /// Повтор той же беседы: ответ модели и просьба вернуть JSON.
    Retry(Vec<ChatMessage>),
    /// Второй сбой — `bad_json` (с учётом токенов ответа).
    Failed(CallError),
}

/// Шаг разбора JSON с одним повтором: `convo` — отправленная беседа, `reply` — ответ на неё,
/// `retried` — это уже ответ на повтор.
pub fn json_step(convo: &[ChatMessage], reply: &ModelReply, retried: bool) -> JsonStep {
    match extract_json(&reply.text) {
        Ok(v) => JsonStep::Done(v),
        Err(e) if !retried => {
            let mut next = convo.to_vec();
            next.push(ChatMessage::assistant(reply.text.clone()));
            next.push(ChatMessage::user(repair_prompt(&e)));
            JsonStep::Retry(next)
        }
        Err(_) => JsonStep::Failed(CallError {
            usage: Some(reply.usage()),
            ..CallError::new(CallClass::BadJson)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(n: usize, text: &str) -> PayloadItem {
        PayloadItem {
            n,
            text: text.into(),
        }
    }

    fn secret() -> String {
        format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5")
    }

    /// Блок `json`, объект в тексте, строки со скобками, `<think>`, не объект, нет объекта.
    #[test]
    fn extract_json_cases() {
        assert_eq!(extract_json("{\"a\": 1}").unwrap(), json!({"a": 1}));
        assert_eq!(
            extract_json("Итог:\n```json\n{\"a\": [1, 2]}\n```\nконец").unwrap(),
            json!({"a": [1, 2]})
        );
        // блок без языка пропускается, объект — из текста
        assert_eq!(
            extract_json("```\nкод\n```\nи {\"b\": true}").unwrap(),
            json!({"b": true})
        );
        // блок json сильнее объекта раньше него
        assert_eq!(
            extract_json("{\"x\": 0} потом ```JSON\n{\"y\": 1}\n```").unwrap(),
            json!({"y": 1})
        );
        assert_eq!(
            extract_json("Вот: {\"q\": \"скобка } и { и \\\" кавычка\"} — всё").unwrap(),
            json!({"q": "скобка } и { и \" кавычка"})
        );
        assert_eq!(
            extract_json("<think>думаю {не JSON}</think>\n{\"ok\": true}").unwrap(),
            json!({"ok": true})
        );
        assert!(extract_json("[1, 2]").is_err());
        assert!(
            extract_json("```json\n[1]\n```")
                .unwrap_err()
                .contains("не JSON-объект")
        );
        assert!(
            extract_json("просто текст")
                .unwrap_err()
                .contains("нет JSON-объекта")
        );
        assert!(extract_json("{\"a\": ").is_err());
        assert!(extract_json("{\"a\": 1,}").is_err());
    }

    /// Повтор — беседа плюс ответ и просьба; ошибка разбора усечена.
    #[test]
    fn json_step_retry_then_fail() {
        let convo = vec![ChatMessage::system("с"), ChatMessage::user("п")];
        let reply = ModelReply {
            text: "не JSON".into(),
            input_tokens: 5,
            output_tokens: 3,
            estimated: false,
            reasoning: false,
        };
        let JsonStep::Retry(next) = json_step(&convo, &reply, false) else {
            panic!("повтор");
        };
        assert_eq!(next.len(), 4);
        assert_eq!(next[2], ChatMessage::assistant("не JSON"));
        assert!(next[3].content.starts_with("ответ не JSON: "));
        let JsonStep::Failed(e) = json_step(&next, &reply, true) else {
            panic!("отказ");
        };
        assert_eq!(e.class, CallClass::BadJson);
        assert_eq!(e.usage.unwrap().output_tokens, 3);
        let long = repair_prompt(&"x".repeat(1000));
        assert!(long.chars().count() < 260, "{long}");
    }

    /// Второй скан: выброшены только элементы с находкой; находка в инструкции или схеме —
    /// `secret_in_payload`; чистый пакет не меняется.
    #[test]
    fn second_scan_drops_items() {
        let p = Payload {
            instruction: "инструкция".into(),
            schema: "{}".into(),
            items: vec![
                item(1, "чисто"),
                item(2, &format!("ключ {}", secret())),
                item(3, "тоже чисто"),
                item(4, &format!("и здесь {}", secret())),
            ],
        };
        let s = second_scan(p.clone()).unwrap();
        assert_eq!(s.dropped, vec![2, 4]);
        assert_eq!(s.payload.user(), "чисто\n\nтоже чисто");
        assert!(zam_secrets::scan_secrets(&s.payload.user()).is_empty());
        let clean = Payload {
            items: vec![item(1, "чисто")],
            ..p.clone()
        };
        assert_eq!(second_scan(clean.clone()).unwrap().payload, clean);
        let mut bad = p.clone();
        bad.schema = format!("{{\"k\": \"{}\"}}", secret());
        assert_eq!(
            second_scan(bad).unwrap_err().class,
            CallClass::SecretInPayload
        );
        assert_eq!(p.system(), "инструкция\n\n{}");
    }
}
