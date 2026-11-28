//! Предупреждение `secrets_detected` в ответах записи заметок и записей журналов (§7.6).
//!
//! Сканер (`zam_markdown::scan_secrets`, крейт `zam-secrets`) находит в сохранённом тексте
//! значения, похожие на ключи и пароли. Текст при этом сохраняется как есть, а в индекс (FTS,
//! эмбеддинги, Qdrant) и в выдачу агенту по токену (задача 5 M7.1, [`super::agent_view`]) уходит
//! замаскированным; исходник видит человек в WebUI. Ответ получает `warnings: ["secrets_detected"]`,
//! `secrets_detected: [{kind, line}]` (без значений) и подсказку про Vault. Скан упёрся в предел
//! повторов (`ScanOutcome::depth_capped`, задача 15b M6) — ещё `warnings: ["secrets_scan_depth"]`
//! и WARN в журнал сервера без текста находок (число находок и повторов, длина текста).

use serde_json::{Value, json};
use zam_markdown::{ScanOutcome, scan_secrets_ext};
use zam_store::Entry;

/// Предупреждение в массиве `warnings`.
pub const SECRETS_DETECTED: &str = "secrets_detected";

/// Предупреждение в `warnings`: скан упёрся в предел повторов (`zam_markdown::secrets::
/// FIXPOINT_MAX`) — неустойчивые находки замаскированы с соседним текстом (до пробелов, строки или
/// весь текст; задача 3 M7.1), в индекс значения не уходят, но поиск по этому месту хуже.
pub const SECRETS_SCAN_DEPTH: &str = "secrets_scan_depth";

/// Подсказка, куда класть секреты.
pub const SECRETS_HINT: &str = "В тексте найдены значения, похожие на секреты. Текст сохранён как \
есть (исходник — в WebUI), но в индекс, поиск и выдачу агенту значения не попадают: вместо них \
‹secret›. Храните секреты в Vault: `zam vault set <имя>`, а в тексте оставляйте ссылку на имя.";

/// Добавляет в ответ-объект `v` предупреждение и список находок по тексту `text`;
/// без находок ответ не меняется.
pub fn attach(v: &mut Value, text: &str) {
    attach_scan(v, text.len(), scan_secrets_ext(text));
}

/// [`attach`] для правки агента по маске (круг правок 1 задачи 6 M7.1, M4): только находки,
/// значений которых нет в прежнем тексте `base`, — восстановленные из-под `‹secret›` и
/// оставшиеся прежними значения агенту не новость (ключа он не знает, перенести его в Vault не
/// может).
pub fn attach_new(v: &mut Value, text: &str, base: &str) {
    let scan = new_scan(text, Some(base));
    if scan.hits.is_empty() {
        return;
    }
    attach_scan(v, text.len(), scan);
}

/// Находки текста `text`, которые ответ записи назвал бы в `secrets_detected`: все, а с прежним
/// текстом `base` — только значения, которых в нём нет ([`attach_new`]).
fn new_scan(text: &str, base: Option<&str>) -> ScanOutcome {
    let scan = scan_secrets_ext(text);
    let Some(base) = base.filter(|_| !scan.hits.is_empty()) else {
        return scan;
    };
    let known: std::collections::HashSet<&str> = zam_markdown::scan_secrets(base)
        .into_iter()
        .filter_map(|h| base.get(h.range))
        .collect();
    let hits = scan
        .hits
        .into_iter()
        .filter(|h| text.get(h.range.clone()).is_none_or(|s| !known.contains(s)))
        .collect();
    ScanOutcome { hits, ..scan }
}

/// Отказ записи при секрете (M7.5 §8.9 п. 5, `reject_secrets: true`): находки текста `text` — те
/// же, что попали бы в `secrets_detected` ответа (с `base` — только новые, [`attach_new`]); есть —
/// 422 `/problems/secrets-detected` с `[{kind, line}]` и без значений, запись не выполняется.
pub fn reject(text: &str, base: Option<&str>) -> Result<(), crate::error::ApiError> {
    let hits = new_scan(text, base).hits;
    if hits.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = hits
        .iter()
        .map(|h| format!("{} (строка {})", h.kind.as_str(), h.line))
        .collect();
    Err(crate::error::ApiError::SecretsDetected {
        detail: format!(
            "в тексте найдены значения, похожие на секреты: {} — ничего не записано \
             (reject_secrets); уберите их из текста, а ключи храните в Vault (`zam vault set \
             <имя>`, memory_vault)",
            list.join(", ")
        ),
        hits: hits
            .iter()
            .map(|h| json!({ "kind": h.kind.as_str(), "line": h.line }))
            .collect(),
    })
}

/// [`attach_new`] по текстам записи и её прежнего вида.
pub fn attach_entry_new(v: &mut Value, e: &Entry, base: &Entry) {
    attach_new(v, &entry_text(e), &entry_text(base));
}

/// [`attach`] по готовому итогу скана текста длиной `text_bytes`: сам текст сюда не передаётся,
/// поэтому WARN упора без текста находок по построению. Отдельно от [`attach`] — ради теста этого
/// свойства: входа, который упирается в предел, с задачи 3 M7.1 нет (ревью, Minor 5).
pub fn attach_scan(v: &mut Value, text_bytes: usize, scan: ScanOutcome) {
    let hits = scan.hits;
    if hits.is_empty() {
        return;
    }
    if scan.depth_capped {
        // без текста находок и самого текста: только счётчики
        tracing::warn!(
            hits = hits.len(),
            passes = scan.passes,
            text_bytes,
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
