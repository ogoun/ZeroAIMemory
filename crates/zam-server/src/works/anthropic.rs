//! Родной протокол Anthropic Messages подключённой модели (M7.6, спецификация §9.2; сверено с
//! документацией Messages API на 2026-11-28, версия `2026-10-21`). Маршрут, агент `ureq`,
//! заголовок ключа, пределы и журнал отправок — общие с OpenAI-совместимым протоколом
//! ([`super::model::call_chat_blocking`]); здесь — то, чем протоколы различаются.
//!
//! - **Адрес** — как у SDK: `https://api.anthropic.com` или с `/v1` на конце; запрос в обоих
//!   случаях — `POST <база>/v1/messages`, `/v1` не удваивается ([`url`], решение 147).
//! - **Заголовки** — `x-api-key: <ключ>` (чувствительный, как `Authorization`; у `auth = "none"`
//!   — без него: локальные серверы со входом Messages), всегда `anthropic-version:`
//!   [`API_VERSION`] и `content-type: application/json`.
//! - **Тело** — `{model, max_tokens, system, messages, stream: false}` и `body_extra` верхним
//!   уровнем ([`request_body`]): системное сообщение беседы — строкой в `system`, прочие — в
//!   `messages`. Зарезервированы [`RESERVED_BODY_KEYS`], в том числе параметры выборки: версия
//!   `2026-10-21` отвергает `temperature`, `top_p`, `top_k` у всех моделей ([`check_sampling`],
//!   решение 148). Глубина размышлений — рецептами руководства (`output_config.effort`).
//! - **Ответ** ([`parse_response`]) — склейка блоков `content[]` с `type: "text"`; `thinking`,
//!   `redacted_thinking` и прочие блоки ответом не считаются. `stop_reason`: `end_turn`,
//!   `stop_sequence` — итог; `max_tokens` — `max_tokens`, а без текста при блоке размышлений —
//!   `thinking`; `model_context_window_exceeded` — `max_tokens`; `refusal` — `refused`
//!   (категория `stop_details.category` — в [`CallError::refusal_category`], `explanation` — нет);
//!   `pause_turn`, `tool_use` и иное — `bad_response` (решение 149). Учёт: вход —
//!   `input_tokens` + `cache_creation_input_tokens` + `cache_read_input_tokens`, выход —
//!   `output_tokens`; без `usage` — оценка (`estimated`).
//! - **Ошибки** — тело `{"type": "error", "error": {"type", "message"}}`: класс — по коду HTTP
//!   таблицы §6.2 п. 5; 429 с `error.details.error_code = enforced_spend_limit_reached` (месячный
//!   предел расходов) — `quota` и при `retry-after`. Текст ответа в ошибку не попадает.

use serde_json::{Map, Value, json};

use super::egress::{BaseUrl, v1_endpoint};
use super::model::{
    CallClass, CallError, ChatMessage, ModelConn, ModelReply, Role, Usage, estimate_input,
    parse_retry_after, status_class,
};

/// Версия API в заголовке `anthropic-version` (решение 147: новая версия — правка кода и выпуск).
pub const API_VERSION: &str = "2026-10-21";
/// Параметры выборки: версия [`API_VERSION`] отвергает их у всех моделей.
pub const SAMPLING_KEYS: [&str; 3] = ["temperature", "top_p", "top_k"];
/// Ключи тела, которые задаёт сервер, и параметры выборки: в `body_extra` — 422.
pub const RESERVED_BODY_KEYS: [&str; 8] = [
    "model",
    "messages",
    "system",
    "stream",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
];
/// Код месячного предела расходов в `error.details.error_code` ответа 429.
pub const SPEND_LIMIT_CODE: &str = "enforced_spend_limit_reached";

/// Категория отказа `stop_details.category` (документация Messages API на 2026-11-28; набор может
/// расти — незнакомая категория — [`RefusalCategory::Other`]). Перечисление, а не строка: в журнал
/// сервера идёт только имя из этого списка, произвольный текст ответа — никогда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCategory {
    Cyber,
    Bio,
    FrontierLlm,
    ReasoningExtraction,
    GeneralHarms,
    Other,
}

impl RefusalCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            RefusalCategory::Cyber => "cyber",
            RefusalCategory::Bio => "bio",
            RefusalCategory::FrontierLlm => "frontier_llm",
            RefusalCategory::ReasoningExtraction => "reasoning_extraction",
            RefusalCategory::GeneralHarms => "general_harms",
            RefusalCategory::Other => "other",
        }
    }

    /// Категория по имени ответа; незнакомое — `Other`.
    pub fn parse(s: &str) -> RefusalCategory {
        [
            RefusalCategory::Cyber,
            RefusalCategory::Bio,
            RefusalCategory::FrontierLlm,
            RefusalCategory::ReasoningExtraction,
            RefusalCategory::GeneralHarms,
        ]
        .into_iter()
        .find(|c| c.as_str() == s)
        .unwrap_or(RefusalCategory::Other)
    }
}

/// Точка входа Messages: `<база>/v1/messages`, у базы с `/v1` — `<база>/messages`.
pub fn url(base: &BaseUrl) -> String {
    v1_endpoint(base, "/messages")
}

/// Параметры выборки в `body_extra` — 422 с пояснением про версию API.
pub fn check_sampling(extra: &Map<String, Value>) -> Result<(), String> {
    match extra.keys().find(|k| SAMPLING_KEYS.contains(&k.as_str())) {
        Some(k) => Err(format!(
            "ключ «{k}» в body_extra запрещён: версия API Anthropic {API_VERSION} отвергает \
             параметры выборки ({}) у всех моделей",
            SAMPLING_KEYS.join(", ")
        )),
        None => Ok(()),
    }
}

/// Тело запроса Messages: `body_extra` верхним уровнем, поверх — поля сервера (`model`,
/// `max_tokens`, `system`, `messages`, `stream: false`): даже непроверенный `body_extra` их и
/// параметры выборки не передаст. Системные сообщения беседы — строкой `system` (через пустую
/// строку; нет — поля нет), прочие — `messages` по порядку.
pub fn request_body(req: &ModelConn, messages: &[ChatMessage]) -> Value {
    let mut body: Map<String, Value> = req
        .body_extra
        .iter()
        .filter(|(k, _)| !RESERVED_BODY_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    body.insert("model".into(), Value::String(req.model.clone()));
    body.insert("max_tokens".into(), json!(req.max_tokens));
    let system: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    if !system.is_empty() {
        body.insert("system".into(), Value::String(system.join("\n\n")));
    }
    body.insert(
        "messages".into(),
        Value::Array(
            messages
                .iter()
                .filter(|m| m.role != Role::System)
                .map(|m| json!({"role": m.role.as_str(), "content": m.content}))
                .collect(),
        ),
    );
    body.insert("stream".into(), Value::Bool(false));
    Value::Object(body)
}

/// Месячный предел расходов: `error.details.error_code = enforced_spend_limit_reached`.
fn spend_limit(v: &Value) -> bool {
    v["error"]["details"]["error_code"].as_str() == Some(SPEND_LIMIT_CODE)
}

/// Категория отказа `stop_details.category` (`null` и нет поля — `None`).
fn refusal_category(v: &Value) -> Option<RefusalCategory> {
    v["stop_details"]["category"]
        .as_str()
        .map(RefusalCategory::parse)
}

/// Учёт ответа: вход — `input_tokens` и токены кэша (запись и чтение), выход — `output_tokens`;
/// недостающее — оценкой (выход — ответ и текст размышлений: оба оплачены).
fn usage_of(v: &Value, sent: &[ChatMessage], text: &str, thinking: &str) -> Usage {
    let u = &v["usage"];
    let cache = u["cache_creation_input_tokens"]
        .as_u64()
        .unwrap_or(0)
        .saturating_add(u["cache_read_input_tokens"].as_u64().unwrap_or(0));
    let input = u["input_tokens"].as_u64().map(|n| n.saturating_add(cache));
    let output = u["output_tokens"].as_u64();
    let count = |s: &str| {
        if s.is_empty() {
            0
        } else {
            zam_store::count_tokens(s) as u64
        }
    };
    Usage {
        input_tokens: input.unwrap_or_else(|| estimate_input(sent)),
        output_tokens: output.unwrap_or_else(|| count(text) + count(thinking)),
        estimated: input.is_none() || output.is_none(),
    }
}

/// Разбор ответа HTTP Messages (см. модуль): `status` и `retry-after` ответа, тело, отправленная
/// беседа (оценка входа без `usage`). `ModelReply::reasoning` — в ответе был блок размышлений.
pub fn parse_response(
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
    sent: &[ChatMessage],
) -> Result<ModelReply, CallError> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    if !(200..300).contains(&status) {
        if status == 429 && json.as_ref().is_some_and(spend_limit) {
            return Err(CallError::http(CallClass::Quota, status));
        }
        let ra = retry_after.and_then(|v| parse_retry_after(v, zam_store::time::now()));
        // `insufficient_quota` — признак OpenAI: у Anthropic тело на класс влияет только кодом
        return Err(status_class(status, None, ra));
    }
    let bad = || CallError::http(CallClass::BadResponse, status);
    let Some(v) = json else {
        return Err(bad());
    };
    let Some(blocks) = v["content"].as_array() else {
        return Err(bad());
    };
    let of_type = |t: &'static str| blocks.iter().filter(move |b| b["type"].as_str() == Some(t));
    let text: String = of_type("text").filter_map(|b| b["text"].as_str()).collect();
    let thinking_text: String = of_type("thinking")
        .filter_map(|b| b["thinking"].as_str())
        .collect();
    let has_thinking = blocks
        .iter()
        .any(|b| matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
    let usage = usage_of(&v, sent, &text, &thinking_text);
    let fail = |class: CallClass| CallError {
        usage: Some(usage),
        ..CallError::http(class, status)
    };
    let empty = text.trim().is_empty();
    match v["stop_reason"].as_str() {
        Some("end_turn" | "stop_sequence") if !empty => {}
        Some("max_tokens") if empty && has_thinking => return Err(fail(CallClass::Thinking)),
        Some("max_tokens" | "model_context_window_exceeded") => {
            return Err(fail(CallClass::MaxTokens));
        }
        Some("refusal") => {
            return Err(CallError {
                refusal_category: refusal_category(&v),
                ..fail(CallClass::Refused)
            });
        }
        // пустой итог, `pause_turn`, `tool_use`, нет `stop_reason` и иное
        _ => return Err(fail(CallClass::BadResponse)),
    }
    Ok(ModelReply {
        text,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        estimated: usage.estimated,
        reasoning: has_thinking,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::works::model::{ModelAuth, Protocol, TokenField};

    fn sent() -> Vec<ChatMessage> {
        vec![ChatMessage::system("с"), ChatMessage::user("п")]
    }

    /// Ответ Messages с блоками `content` и `stop_reason`, `usage` 10/5 без кэша.
    fn reply(content: Value, stop: &str) -> Value {
        json!({"type": "message", "role": "assistant", "content": content, "stop_reason": stop,
               "usage": {"input_tokens": 10, "output_tokens": 5}})
    }

    fn parse(v: Value) -> Result<ModelReply, CallError> {
        parse_response(200, None, v.to_string().as_bytes(), &sent())
    }

    fn class(v: Value) -> CallClass {
        parse(v).unwrap_err().class
    }

    fn conn() -> ModelConn {
        ModelConn {
            protocol: Protocol::Anthropic.as_str().into(),
            vault: zam_core::Id::new(),
            model: "claude-mock".into(),
            auth: ModelAuth::Key,
            max_tokens: 4000,
            token_field: TokenField::Auto,
            body_extra: Map::new(),
            pinned: None,
            external: None,
        }
    }

    /// Тело: системные сообщения — строкой `system` (несколько — через пустую строку), без них
    /// поля нет; прочие — `messages` по порядку; зарезервированное из `body_extra` не уходит.
    #[test]
    fn request_body_system_apart() {
        let convo = [
            ChatMessage::system("а"),
            ChatMessage::user("в"),
            ChatMessage::assistant("г"),
            ChatMessage::system("б"),
        ];
        let b = request_body(&conn(), &convo);
        assert_eq!(b["system"], "а\n\nб");
        assert_eq!(
            b["messages"],
            json!([{"role": "user", "content": "в"}, {"role": "assistant", "content": "г"}])
        );
        let b = request_body(&conn(), &[ChatMessage::user("в")]);
        assert!(b.get("system").is_none());
        let mut c = conn();
        c.body_extra = json!({"top_p": 0.5, "thinking": {"type": "disabled"}})
            .as_object()
            .unwrap()
            .clone();
        let b = request_body(&c, &sent());
        assert!(b.get("top_p").is_none());
        assert_eq!(b["thinking"], json!({"type": "disabled"}));
    }

    /// Проверка `body_extra`: выборка — с пояснением про версию; поля сервера — списком; размер.
    #[test]
    fn check_body_extra_anthropic() {
        let p = Protocol::Anthropic;
        for k in SAMPLING_KEYS {
            let mut m = Map::new();
            m.insert(k.into(), json!(1));
            let e = p.check_body_extra(&m).unwrap_err();
            assert!(e.contains(k) && e.contains(API_VERSION), "{e}");
        }
        let mut m = Map::new();
        m.insert("system".into(), json!("x"));
        assert!(p.check_body_extra(&m).unwrap_err().contains("system"));
        let ok = json!({"output_config": {"effort": "low"}});
        assert!(p.check_body_extra(ok.as_object().unwrap()).is_ok());
        let mut big = Map::new();
        big.insert("x".into(), json!("a".repeat(5000)));
        assert!(p.check_body_extra(&big).is_err());
    }

    /// Ответ — склейка блоков `text`; `thinking`, `redacted_thinking`, `tool_use` и прочие ответом
    /// не считаются; блок размышлений — признак `reasoning`; `stop_sequence` — итог.
    #[test]
    fn anthropic_parse_text_blocks_only() {
        let r = parse(reply(
            json!([
                {"type": "thinking", "thinking": "думаю", "signature": "s"},
                {"type": "text", "text": "{\"a\":"},
                {"type": "redacted_thinking", "data": "зашифровано"},
                {"type": "text", "text": "1}", "citations": null}
            ]),
            "end_turn",
        ))
        .unwrap();
        assert_eq!(r.text, "{\"a\":1}");
        assert!(r.reasoning);
        assert_eq!(
            (r.input_tokens, r.output_tokens, r.estimated),
            (10, 5, false)
        );
        let r = parse(reply(
            json!([{"type": "text", "text": "ок"}]),
            "stop_sequence",
        ))
        .unwrap();
        assert_eq!(r.text, "ок");
        assert!(!r.reasoning);
        // пустой итог — иной ответ, и при блоке размышлений
        assert_eq!(
            class(reply(json!([{"type": "text", "text": "  "}]), "end_turn")),
            CallClass::BadResponse
        );
        assert_eq!(
            class(reply(
                json!([{"type": "thinking", "thinking": "", "signature": "s"}]),
                "end_turn"
            )),
            CallClass::BadResponse
        );
        // не Messages: нет `content`, не JSON, Chat Completions
        assert_eq!(class(json!({"type": "message"})), CallClass::BadResponse);
        assert_eq!(
            class(json!({"choices": [{"message": {"content": "x"}, "finish_reason": "stop"}]})),
            CallClass::BadResponse
        );
        let e = parse_response(200, None, b"<html>", &sent()).unwrap_err();
        assert_eq!(
            (e.class, e.http_status),
            (CallClass::BadResponse, Some(200))
        );
    }

    /// `max_tokens` без текстовых блоков при блоке размышлений — `thinking` (с учётом); с
    /// текстом — `max_tokens`; без размышлений — `max_tokens`; подсказка — про `effort`.
    #[test]
    fn anthropic_max_tokens_without_text_is_thinking() {
        let e = parse(reply(
            json!([{"type": "thinking", "thinking": "", "signature": "s"}]),
            "max_tokens",
        ))
        .unwrap_err();
        assert_eq!(e.class, CallClass::Thinking);
        assert_eq!(e.usage.map(|u| u.output_tokens), Some(5));
        let hint = e.class.hint_for(Protocol::Anthropic).unwrap();
        assert!(
            hint.contains("effort") && hint.contains("max_tokens"),
            "{hint}"
        );
        assert!(!hint.contains("chat_template_kwargs"));
        assert_eq!(
            class(reply(
                json!([{"type": "redacted_thinking", "data": "x"}]),
                "max_tokens"
            )),
            CallClass::Thinking
        );
        assert_eq!(
            class(reply(
                json!([{"type": "thinking", "thinking": "", "signature": "s"},
                       {"type": "text", "text": "{\"items\": ["}]),
                "max_tokens"
            )),
            CallClass::MaxTokens
        );
        assert_eq!(class(reply(json!([]), "max_tokens")), CallClass::MaxTokens);
    }

    /// `refusal` — `refused`; категория `stop_details.category` — короткое имя, `explanation`
    /// не берётся; странная категория — `other`; учёт отказа сохраняется.
    #[test]
    fn anthropic_refusal_class() {
        let mut v = reply(json!([]), "refusal");
        v["stop_details"] = json!({"type": "refusal", "category": "cyber",
                                   "explanation": "секретное пояснение"});
        let e = parse(v.clone()).unwrap_err();
        assert_eq!(e.class, CallClass::Refused);
        assert_eq!(e.refusal_category, Some(RefusalCategory::Cyber));
        assert!(e.usage.is_some());
        assert!(!format!("{e:?}").contains("секретное"), "{e:?}");
        v["stop_details"]["category"] = Value::Null;
        assert_eq!(parse(v.clone()).unwrap_err().refusal_category, None);
        v["stop_details"]["category"] = json!("Не категория\n");
        assert_eq!(
            parse(v.clone()).unwrap_err().refusal_category,
            Some(RefusalCategory::Other)
        );
        // отказ при частичном тексте — тоже отказ
        let mut v = reply(json!([{"type": "text", "text": "начал"}]), "refusal");
        v["stop_details"] = json!({"type": "refusal", "category": "general_harms"});
        let e = parse(v).unwrap_err();
        assert_eq!(
            (e.class, e.refusal_category.map(RefusalCategory::as_str)),
            (CallClass::Refused, Some("general_harms"))
        );
    }

    /// `model_context_window_exceeded` — `max_tokens` (и без текста, и при размышлениях).
    #[test]
    fn anthropic_context_window_exceeded_is_max_tokens() {
        for content in [
            json!([{"type": "text", "text": "{\"items\": ["}]),
            json!([{"type": "thinking", "thinking": "", "signature": "s"}]),
            json!([]),
        ] {
            let e = parse(reply(content, "model_context_window_exceeded")).unwrap_err();
            assert_eq!(e.class, CallClass::MaxTokens);
            assert!(e.usage.is_some());
        }
    }

    /// `pause_turn`, `tool_use`, незнакомая причина и её отсутствие — `bad_response`, даже с
    /// текстом.
    #[test]
    fn anthropic_pause_turn_bad_response() {
        let text = json!([{"type": "text", "text": "{\"ok\": true}"}]);
        for stop in ["pause_turn", "tool_use", "something_new"] {
            assert_eq!(
                class(reply(text.clone(), stop)),
                CallClass::BadResponse,
                "{stop}"
            );
        }
        let mut v = reply(text, "end_turn");
        v.as_object_mut().unwrap().remove("stop_reason");
        assert_eq!(class(v), CallClass::BadResponse);
    }

    /// Учёт: вход — `input_tokens` + запись и чтение кэша, выход — `output_tokens`; `null` у полей
    /// кэша — ноль; без `usage` — оценка (выход — текст и размышления), частично — смешанно.
    #[test]
    fn anthropic_usage_with_cache_fields() {
        let mut v = reply(json!([{"type": "text", "text": "ответ"}]), "end_turn");
        v["usage"] = json!({"input_tokens": 100, "cache_creation_input_tokens": 2051,
                            "cache_read_input_tokens": 4000, "output_tokens": 42,
                            "cache_creation": {"ephemeral_5m_input_tokens": 2051,
                                               "ephemeral_1h_input_tokens": 0}});
        let r = parse(v.clone()).unwrap();
        assert_eq!(
            (r.input_tokens, r.output_tokens, r.estimated),
            (6151, 42, false)
        );
        v["usage"]["cache_creation_input_tokens"] = Value::Null;
        v["usage"]["cache_read_input_tokens"] = Value::Null;
        let r = parse(v.clone()).unwrap();
        assert_eq!((r.input_tokens, r.estimated), (100, false));
        // без `usage` (часть локальных серверов) — оценка
        let mut v = reply(
            json!([{"type": "thinking", "thinking": "рассуждение", "signature": "s"},
                   {"type": "text", "text": "ответ"}]),
            "end_turn",
        );
        v.as_object_mut().unwrap().remove("usage");
        let r = parse(v.clone()).unwrap();
        assert!(r.estimated);
        assert_eq!(r.input_tokens, zam_store::count_tokens("с\nп") as u64);
        assert_eq!(
            r.output_tokens,
            (zam_store::count_tokens("ответ") + zam_store::count_tokens("рассуждение")) as u64
        );
        v["usage"] = json!({"input_tokens": 7});
        let r = parse(v).unwrap();
        assert!(r.estimated);
        assert_eq!(r.input_tokens, 7);
        // учёт обрезанного ответа — с кэшем
        let mut v = reply(json!([{"type": "text", "text": "обре"}]), "max_tokens");
        v["usage"] = json!({"input_tokens": 1, "cache_read_input_tokens": 2, "output_tokens": 3});
        let u = parse(v).unwrap_err().usage.unwrap();
        assert_eq!(
            (u.input_tokens, u.output_tokens, u.estimated),
            (3, 3, false)
        );
    }

    /// Классы ошибок разбора (полный набор — `anthropic_error_classes` с подставной моделью):
    /// предел расходов — `quota` и с `retry-after`; тело на прочие классы не влияет.
    #[test]
    fn error_status_classes() {
        let spend = json!({"type": "error", "error": {"type": "rate_limit_error", "message": "x",
                           "details": {"error_code": SPEND_LIMIT_CODE}}});
        let e = parse_response(429, Some("5"), spend.to_string().as_bytes(), &sent()).unwrap_err();
        assert_eq!((e.class, e.retry_after), (CallClass::Quota, None));
        let rate = json!({"type": "error", "error": {"type": "rate_limit_error", "message": "x"}});
        let e = parse_response(429, Some("5"), rate.to_string().as_bytes(), &sent()).unwrap_err();
        assert_eq!((e.class, e.retry_after), (CallClass::Rate, Some(5)));
        // `insufficient_quota` — признак OpenAI, у Anthropic не учитывается
        let oai = json!({"error": {"code": "insufficient_quota"}});
        let e = parse_response(400, None, oai.to_string().as_bytes(), &sent()).unwrap_err();
        assert_eq!(e.class, CallClass::BadRequest);
        let e = parse_response(529, None, b"", &sent()).unwrap_err();
        assert_eq!(
            (e.class, e.http_status),
            (CallClass::Unavailable, Some(529))
        );
    }
}
