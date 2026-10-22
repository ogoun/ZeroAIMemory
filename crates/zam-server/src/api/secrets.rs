//! Предупреждение `secrets_detected` в ответах записи заметок и записей журналов (§7.6).
//!
//! Сканер (`zam_markdown::scan_secrets`) находит в сохранённом тексте значения, похожие на
//! ключи и пароли. Текст при этом сохраняется как есть, а в индекс (FTS, эмбеддинги, Qdrant)
//! уходит замаскированным. Ответ получает `warnings: ["secrets_detected"]`,
//! `secrets_detected: [{kind, line}]` (без значений) и подсказку про Vault.

use serde_json::{Value, json};
use zam_markdown::scan_secrets;
use zam_store::Entry;

/// Предупреждение в массиве `warnings`.
pub const SECRETS_DETECTED: &str = "secrets_detected";

/// Подсказка, куда класть секреты.
pub const SECRETS_HINT: &str = "В тексте найдены значения, похожие на секреты. В индекс и поиск \
они не попадают (маскируются), но текст сохранён как есть. Храните секреты в Vault: \
`zam vault set <имя>`, а в тексте оставляйте ссылку на имя.";

/// Добавляет в ответ-объект `v` предупреждение и список находок по тексту `text`;
/// без находок ответ не меняется.
pub fn attach(v: &mut Value, text: &str) {
    let hits = scan_secrets(text);
    if hits.is_empty() {
        return;
    }
    let Value::Object(o) = v else { return };
    let w = o.entry("warnings").or_insert_with(|| json!([]));
    if let Value::Array(a) = w
        && !a.iter().any(|x| x == SECRETS_DETECTED)
    {
        a.push(json!(SECRETS_DETECTED));
    }
    let list: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "kind": h.kind.as_str(), "line": h.line }))
        .collect();
    o.insert(SECRETS_DETECTED.into(), Value::Array(list));
    o.insert("secrets_hint".into(), json!(SECRETS_HINT));
}

/// Документ записи, как его режет хранилище (`# <title>\n\n<body>`): номера строк находок
/// отсчитываются в нём.
pub fn entry_text(e: &Entry) -> String {
    if e.body.trim().is_empty() {
        format!("# {}\n", e.title)
    } else {
        format!("# {}\n\n{}", e.title, e.body)
    }
}

/// [`attach`] по тексту записи.
pub fn attach_entry(v: &mut Value, e: &Entry) {
    attach(v, &entry_text(e));
}
