//! Предупреждение `secrets_detected` в ответах записи заметок и записей журналов (§7.6).
//!
//! Сканер (`zam_markdown::scan_secrets`, крейт `zam-secrets`) находит в сохранённом тексте
//! значения, похожие на ключи и пароли. Текст при этом сохраняется как есть, а в индекс (FTS,
//! эмбеддинги, Qdrant) уходит замаскированным. Ответ получает `warnings: ["secrets_detected"]`,
//! `secrets_detected: [{kind, line}]` (без значений) и подсказку про Vault. Скан упёрся в предел
//! повторов (`ScanOutcome::depth_capped`, задача 15b M6) — ещё `warnings: ["secrets_scan_depth"]`
//! и WARN в журнал сервера без текста находок (число находок и повторов, длина текста).

use serde_json::{Value, json};
use zam_markdown::scan_secrets_ext;
use zam_store::Entry;

/// Предупреждение в массиве `warnings`.
pub const SECRETS_DETECTED: &str = "secrets_detected";

/// Предупреждение в `warnings`: скан упёрся в предел повторов (`zam_markdown::secrets::
/// FIXPOINT_MAX`) — часть значений может уйти в индекс незамаскированной.
pub const SECRETS_SCAN_DEPTH: &str = "secrets_scan_depth";

/// Подсказка, куда класть секреты.
pub const SECRETS_HINT: &str = "В тексте найдены значения, похожие на секреты. В индекс и поиск \
они не попадают (маскируются), но текст сохранён как есть. Храните секреты в Vault: \
`zam vault set <имя>`, а в тексте оставляйте ссылку на имя.";

/// Добавляет в ответ-объект `v` предупреждение и список находок по тексту `text`;
/// без находок ответ не меняется.
pub fn attach(v: &mut Value, text: &str) {
    let scan = scan_secrets_ext(text);
    let hits = scan.hits;
    if hits.is_empty() {
        return;
    }
    if scan.depth_capped {
        // без текста находок и самого текста: только счётчики
        tracing::warn!(
            hits = hits.len(),
            passes = scan.passes,
            text_bytes = text.len(),
            "сканер секретов упёрся в предел повторов (secrets_scan_depth)"
        );
    }
    let Value::Object(o) = v else { return };
    let w = o.entry("warnings").or_insert_with(|| json!([]));
    if let Value::Array(a) = w {
        let mut add = |code: &str| {
            if !a.iter().any(|x| x == code) {
                a.push(json!(code));
            }
        };
        add(SECRETS_DETECTED);
        if scan.depth_capped {
            add(SECRETS_SCAN_DEPTH);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Ревью задачи 9A, M2: слипшиеся ключи — две находки детектора. Ключи без энтропии
    /// (повторы), чтобы находки не слились правилом энтропии: второй находит только продолжение
    /// от конца первого.
    #[test]
    fn attach_lists_glued_keys() {
        let t = format!(
            "x {}{}{}{} y",
            "AK",
            "IA1111111111111111",
            "npm_",
            "a".repeat(36)
        );
        let mut v = json!({});
        attach(&mut v, &t);
        assert_eq!(v[SECRETS_DETECTED].as_array().unwrap().len(), 2, "{v}");
    }
}
