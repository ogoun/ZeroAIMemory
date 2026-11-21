//! Текст для зрителя (спецификация M7 §4.3.1, задача 5 M7.1; решение владельца 2026-11-06,
//! п. 9): агенту — запросу по токену (`Principal::Token`: MCP, хуки, CLI `zam`) — значения,
//! похожие на секреты, приходят меткой `‹secret›` ([`zam_markdown::mask_text`]); человеку в
//! сессии WebUI ([`Principal::is_human`]) — исходник. Решение — по принципалу запроса, не по
//! полю `by` записи; человек, работающий через CLI по токену, тоже получает маску (исходник — в
//! WebUI). Исходник в базе не меняется; ключи агент берёт из Vault: раскрытие значения
//! (`GET /vault/{id}?reveal=1`, MCP `memory_vault get`) не маскируется — оно и есть способ его
//! получить. Признак `/health.features` — `agent_text_masked`.
//!
//! Правила выдачи:
//! - маска накладывается на **весь** текст ревизии до нарезки (секция, L1, фокус, `range`, дифф
//!   `since_rev` — каждая ревизия до диффа): маска куска ≠ кусок маски — правило соседнего
//!   контекста (`token: <значение>`) по куску не сработает, а смещения `range` и пропусков фокуса
//!   считаются по маскированному тексту;
//! - пути узлов и имена файлов — адреса и не маскируются (секрет в пути находит перепроверка
//!   `secrets_delta`); сам файл вложения (`GET /blobs/…`) — как есть;
//! - [`zam_markdown::mask_text`] идемпотентна (задача 3 M7.1): уже замаскированное (L0 узла,
//!   чанки, подписи листов) повторная маска не меняет;
//! - текст длиннее [`BLOCKING_FROM`] маскируется в `spawn_blocking` ([`view_owned`]): `mask_text`
//!   — до 2 мс на 100 КБ, заметка 5 МБ — около 100 мс.
//!
//! Правила флагов командной строки (`zam_secrets::mask_flags`: `-pXXX`, `--password X`,
//! `Authorization:`) здесь **не** применяются: выдача — проза и Markdown, где «basic», «bearer» и
//! `-u` — не пароль (их применяет только поле команд снимка сессии, задача 17a M6) **[решение
//! исполнителя, задача 5]**.
//!
//! Запись обратно (восстановление значения под `‹secret›`, §4.3.2) — задача 6 M7.1
//! (`api::secret_restore`), она решает по [`masks`].
use std::borrow::Cow;

use serde_json::Value;
use zam_markdown::{SECRET_MARK, mask_secrets, scan_secrets};

use crate::auth::Principal;

/// Текст длиннее этого маскируется в пуле блокирующих задач ([`view_owned`]), байт (64 КиБ).
pub const BLOCKING_FROM: usize = 64 * 1024;

/// Поля записи журнала (`Entry`, ревизия записи, `TaskIdea`) в ответах агенту: заголовок, тело,
/// строки `data` (все, рекурсивно), причины отзыва и смены статуса; у задачи и идеи —
/// заголовки связанных записей (`idea`, `ideas`, `task`). Ссылки и пути — без изменений: в них
/// сканер секрет найдёт, только если он там действительно есть.
pub const ENTRY_FIELDS: &[&str] = &[
    "title",
    "body",
    "data",
    "retract_reason",
    "reason",
    "idea.title",
    "ideas.title",
    "task.title",
];

/// Поля узла `Node` в ответах агенту: L0 и `frontmatter` — описание, теги, L0 (путь, имя и
/// `refs` — адреса). L0 хранится маскированным, кроме ручного L0 папки старого импорта
/// (`zam-store/src/import.rs`): повторная маска его закрывает.
pub const NODE_FIELDS: &[&str] = &[
    "l0",
    "frontmatter.description",
    "frontmatter.tags",
    "frontmatter.l0",
];

/// Выдача принципалу `p` маскируется: запрос по токену (агент); сессия WebUI — человек.
pub fn masks(p: &Principal) -> bool {
    !p.is_human()
}

/// Текст `s` с маской на месте секретов; без находок — заимствование без копии. То же, что
/// [`zam_markdown::mask_text`].
pub fn mask(s: &str) -> Cow<'_, str> {
    let hits = scan_secrets(s);
    if hits.is_empty() {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(mask_secrets(s, &hits))
    }
}

/// Текст для зрителя `p`: агенту — с маской ([`mask`]), человеку — исходник.
pub fn view<'a>(p: &Principal, s: &'a str) -> Cow<'a, str> {
    if masks(p) { mask(s) } else { Cow::Borrowed(s) }
}

/// [`view`] для своего текста: человеку — без изменений; агенту — маска, длинный текст (больше
/// [`BLOCKING_FROM`]) — в `spawn_blocking`.
pub async fn view_owned(p: &Principal, s: String) -> String {
    if masks(p) { mask_owned(s).await } else { s }
}

/// Маска своего текста (см. [`view_owned`]): без находок — тот же `String` без копии. Пул
/// блокирующих задач снят (остановка сервера) — одна метка вместо текста: значение не выдаётся
/// и в этом случае; паника сканера — паника обработчика, как и без пула.
pub async fn mask_owned(s: String) -> String {
    if s.len() <= BLOCKING_FROM {
        return mask_string(s);
    }
    match tokio::task::spawn_blocking(move || mask_string(s)).await {
        Ok(t) => t,
        Err(e) => match e.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(_) => SECRET_MARK.to_string(),
        },
    }
}

/// Маска своего текста в текущем потоке: без находок — тот же `String`.
pub fn mask_string(s: String) -> String {
    mask_tracked(s, &mut false)
}

/// [`mask_string`], отмечающая в `changed`, что маска что-то заменила (опись экспорта:
/// `secrets_masked`).
pub fn mask_tracked(s: String, changed: &mut bool) -> String {
    let masked = match mask(&s) {
        Cow::Owned(m) => Some(m),
        Cow::Borrowed(_) => None,
    };
    match masked {
        Some(m) => {
            *changed = true;
            m
        }
        None => s,
    }
}

/// Маска строковых полей `fields` значения `v` для зрителя `p` (человеку — без изменений); см.
/// [`mask_fields`].
pub fn view_json(p: &Principal, v: &mut Value, fields: &[&str]) {
    if masks(p) {
        mask_fields(v, fields);
    }
}

/// Маска полей `fields` значения `v`: поле — путь ключей через точку (`frontmatter.tags`);
/// массив на любом шаге (и сам `v`) проходится поэлементно — список записей и `ideas.title`
/// задаются тем же путём; найденное значение маскируется целиком ([`mask_value`]). Нет поля —
/// ничего. `true` — маска что-то заменила.
pub fn mask_fields(v: &mut Value, fields: &[&str]) -> bool {
    let mut changed = false;
    for f in fields {
        let path: Vec<&str> = f.split('.').collect();
        changed |= mask_path(v, &path);
    }
    changed
}

fn mask_path(v: &mut Value, path: &[&str]) -> bool {
    if let Value::Array(a) = v {
        let mut changed = false;
        for x in a {
            changed |= mask_path(x, path);
        }
        return changed;
    }
    match path.split_first() {
        None => mask_value(v),
        Some((k, rest)) => v.get_mut(*k).is_some_and(|x| mask_path(x, rest)),
    }
}

/// Маска всех строк значения `v` (рекурсивно, ключи объектов — нет): каждая строка — свой текст.
/// `true` — маска что-то заменила.
pub fn mask_value(v: &mut Value) -> bool {
    match v {
        Value::String(s) => {
            let masked = match mask(s) {
                Cow::Owned(m) => Some(m),
                Cow::Borrowed(_) => None,
            };
            match masked {
                Some(m) => {
                    *s = m;
                    true
                }
                None => false,
            }
        }
        Value::Array(a) => a.iter_mut().fold(false, |c, x| mask_value(x) | c),
        Value::Object(o) => o.values_mut().fold(false, |c, x| mask_value(x) | c),
        _ => false,
    }
}

/// Узел `Node` как JSON для зрителя `p` ([`NODE_FIELDS`]).
pub fn node_json(p: &Principal, node: &zam_store::Node) -> Value {
    let mut v = serde_json::to_value(node).unwrap_or(Value::Null);
    view_json(p, &mut v, NODE_FIELDS);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Ключ из частей: в исходнике его нет целиком (push protection).
    fn key() -> String {
        format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5")
    }

    /// Поля по путям: строки — маской, массивы на любом шаге — поэлементно, прочее и ключи
    /// объектов — без изменений, нет поля — ничего.
    #[test]
    fn mask_fields_paths() {
        let k = key();
        let mut v = json!([
            {"title": format!("t {k}"), "body": format!("token: {k}"), "path": format!("a/{k}"),
             "data": {"n": 5, "deep": [format!("x {k}"), {"y": format!("y {k}")}]},
             "ideas": [{"title": format!("i {k}"), "id": "1"}],
             "frontmatter": {"tags": [format!("t-{k}"), "ops"], "refs": [format!("r/{k}")]}}
        ]);
        mask_fields(
            &mut v,
            &[
                "title",
                "body",
                "data",
                "ideas.title",
                "frontmatter.tags",
                "нет.поля",
            ],
        );
        let s = v.to_string();
        assert_eq!(s.matches(&k).count(), 2, "путь и refs — как есть: {s}");
        assert_eq!(v[0]["title"], "t ‹secret›");
        assert_eq!(v[0]["body"], "token: ‹secret›");
        assert_eq!(v[0]["data"]["n"], 5);
        assert_eq!(v[0]["data"]["deep"][1]["y"], "y ‹secret›");
        assert_eq!(v[0]["ideas"][0]["title"], "i ‹secret›");
        // тег — целиком маской (сканер берёт и префикс `t-`), второй — как есть
        let tag = v[0]["frontmatter"]["tags"][0].as_str().unwrap();
        assert!(!tag.contains(&k) && tag.contains(SECRET_MARK), "{tag}");
        assert_eq!(v[0]["frontmatter"]["tags"][1], "ops");
    }

    /// Без находок — заимствование и тот же текст; маска идемпотентна; длинный текст — в пуле.
    #[tokio::test]
    async fn mask_borrowed_and_owned() {
        assert!(matches!(mask("просто текст"), Cow::Borrowed(_)));
        let t = format!("token: {}\n", key());
        let m = mask(&t).into_owned();
        assert_eq!(m, "token: ‹secret›\n");
        assert_eq!(mask(&m), Cow::<str>::Borrowed(&m));
        let big = t.repeat(BLOCKING_FROM / t.len() + 2);
        assert!(big.len() > BLOCKING_FROM);
        let out = mask_owned(big.clone()).await;
        assert_eq!(out, zam_markdown::mask_text(&big));
        assert_eq!(mask_owned("чисто".into()).await, "чисто");
    }

    /// Цена маски L2 в 1 МБ (журнал этапа, задача 5 M7.1): Markdown заметки — разделы с прозой
    /// на русском и английском, код, таблица, ключ-значение, URL с паролем и известные префиксы
    /// (по ключу на ~2 КБ). Медиана 9 прогонов [`mask_owned`] (пул блокирующих задач — как в
    /// `/read`). Только в release: `cargo test --release -p zam-server --lib -- --ignored
    /// bench_mask_l2_1mb --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn bench_mask_l2_1mb() {
        let k = key();
        let mut t = String::from("# Заметка о стенде\n\n");
        let mut i = 0usize;
        while t.len() < 1024 * 1024 {
            t.push_str(&format!(
                "## Раздел {i}\n\nСборка идёт скриптом `deploy.sh`, журнал — в `/var/log/app`. \
                 The service reads its config at start and restarts on change. Пункт {i} описывает \
                 порядок работы с конфигурацией и журналами сервера.\n\n```bash\ncurl -s \
                 https://api.example.com/v1/items?page={i}\n```\n\n| поле | значение |\n|---|---|\n\
                 | host | 192.0.2.{} |\n\ntoken: {k}\n\nСсылка: https://user:pw{i}x@db.example.com/\n\n",
                i % 250
            ));
            i += 1;
        }
        let mut times = Vec::new();
        let mut marks = 0;
        for _ in 0..9 {
            let s = t.clone();
            let t0 = std::time::Instant::now();
            let out = mask_owned(s).await;
            times.push(t0.elapsed());
            marks = out.matches(SECRET_MARK).count();
        }
        times.sort();
        println!(
            "маска L2 {} байт ({i} разделов, {marks} ‹secret›): медиана {:?}, мин {:?}, макс {:?}",
            t.len(),
            times[4],
            times[0],
            times[8]
        );
    }
}
