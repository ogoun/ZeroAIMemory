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
//! - текст и JSON, у которых строк больше [`BLOCKING_FROM`], маскируются в `spawn_blocking`
//!   ([`view_owned`], [`view_json_owned`]): `mask_text` — до 2 мс на 100 КБ, заметка 5 МБ — около
//!   75 мс; пул снят (остановка сервера) — 503, а не текст без маски или с заглушкой.
//!
//! Флаги командной строки (`-pXXX`, `--password X`, `-u user:pass`, `Authorization:`) маскирует
//! сам сканер — внутри кода Markdown (блоки ```` ``` ````/`~~~` и `` `…` ``,
//! `zam_secrets::scan_flags_in_code`, круг правок 1 задачи 5); проза вне кода не трогается.
//!
//! Запись обратно (восстановление значения под `‹secret›`, §4.3.2) — задача 6 M7.1
//! (`api::secret_restore`), она решает по [`masks`].
use std::borrow::Cow;

use serde_json::Value;
use zam_markdown::{SECRET_MARK, mask_secrets, scan_secrets};

use crate::auth::Principal;
use crate::error::ApiError;

/// Текст длиннее этого маскируется в пуле блокирующих задач ([`view_owned`]), байт (64 КиБ).
pub const BLOCKING_FROM: usize = 64 * 1024;

/// Поля записи журнала (`Entry`, ревизия записи, `TaskIdea`) в ответах агенту: заголовок, тело,
/// строки `data` (все, рекурсивно), теги и область, причины отзыва и смены статуса; у задачи и
/// идеи — заголовки связанных записей (`idea`, `ideas`, `task`). Ссылки и пути — без изменений: в
/// них сканер секрет найдёт, только если он там действительно есть.
pub const ENTRY_FIELDS: &[&str] = &[
    "title",
    "body",
    "data",
    "tags",
    "area",
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

/// Маска `s`, если она что-то заменила (иначе `None`: копии нет).
fn masked(s: &str) -> Option<String> {
    let hits = scan_secrets(s);
    (!hits.is_empty()).then(|| mask_secrets(s, &hits))
}

/// Текст `s` с маской на месте секретов; без находок — заимствование без копии. То же, что
/// [`zam_markdown::mask_text`].
pub fn mask(s: &str) -> Cow<'_, str> {
    masked(s).map_or(Cow::Borrowed(s), Cow::Owned)
}

/// Текст для зрителя `p`: агенту — с маской ([`mask`]), человеку — исходник.
pub fn view<'a>(p: &Principal, s: &'a str) -> Cow<'a, str> {
    if masks(p) { mask(s) } else { Cow::Borrowed(s) }
}

/// Своя строка для зрителя `p` на месте: агенту — маска, человеку — без изменений (поля структур
/// хранилища: заголовки узлов графа, триггеров, карточек поиска, выжимок).
pub fn view_in_place(p: &Principal, s: &mut String) {
    if masks(p) {
        mask_in_place(s);
    }
}

/// Маска своей строки на месте (зритель уже проверен: обходы [`view_with`]); `true` — что-то
/// заменено.
pub fn mask_in_place(s: &mut String) -> bool {
    match masked(s) {
        Some(m) => {
            *s = m;
            true
        }
        None => false,
    }
}

/// [`view`] для своего текста: человеку — без изменений; агенту — маска, длинный текст (больше
/// [`BLOCKING_FROM`]) — в `spawn_blocking`. Пул снят — 503 ([`blocking`]).
pub async fn view_owned(p: &Principal, s: String) -> Result<String, ApiError> {
    if masks(p) { mask_owned(s).await } else { Ok(s) }
}

/// Маска своего текста (см. [`view_owned`]): без находок — тот же `String` без копии.
pub async fn mask_owned(s: String) -> Result<String, ApiError> {
    if s.len() <= BLOCKING_FROM {
        return Ok(mask_string(s));
    }
    blocking(move || mask_string(s)).await
}

/// `f` в пуле блокирующих задач: паника — паника обработчика (как и без пула); задача снята
/// (рантайм останавливается) — 503: ни текста без маски, ни заглушки вместо документа с 200.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(t) => Ok(t),
        Err(e) => match e.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(_) => Err(ApiError::Unavailable(
                "маска секретов прервана: сервер останавливается".into(),
            )),
        },
    }
}

/// Начало текста `s` для агента: маска — по **всему** `s` (секрет у границы она видит целиком;
/// маска куска ≠ кусок маски), показ — не дальше `max` байт исходника и не длиннее `max` байт,
/// по границе символа; секрет, который пересекает границу исходника, — меткой целиком, метка не
/// разрезается. Второе — «показано не всё». Результат — начало [`mask`] от `s`.
pub fn mask_head(s: &str, max: usize) -> (String, bool) {
    let mut raw_cut = max.min(s.len());
    while !s.is_char_boundary(raw_cut) {
        raw_cut -= 1;
    }
    let mut out = String::with_capacity(raw_cut + SECRET_MARK.len());
    let mut pos = 0;
    for h in scan_secrets(s) {
        let r = h.range;
        if r.start >= raw_cut {
            break;
        }
        // как `mask_secrets`: неверный диапазон пропускается
        if r.start < pos
            || r.end > s.len()
            || !s.is_char_boundary(r.start)
            || !s.is_char_boundary(r.end)
        {
            continue;
        }
        if !push_fit(&mut out, &s[pos..r.start], max) || out.len() + SECRET_MARK.len() > max {
            return (out, true);
        }
        out.push_str(SECRET_MARK);
        pos = r.end;
    }
    if pos < raw_cut && !push_fit(&mut out, &s[pos..raw_cut], max) {
        return (out, true);
    }
    (out, pos.max(raw_cut) < s.len())
}

/// Дописывает `piece` к `out`, сколько влезает в `max` байт (по границе символа); `true` —
/// целиком.
fn push_fit(out: &mut String, piece: &str, max: usize) -> bool {
    if out.len() + piece.len() <= max {
        out.push_str(piece);
        return true;
    }
    let mut n = max.saturating_sub(out.len());
    while !piece.is_char_boundary(n) {
        n -= 1;
    }
    out.push_str(&piece[..n]);
    false
}

/// Маска своего текста в текущем потоке: без находок — тот же `String`.
pub fn mask_string(mut s: String) -> String {
    mask_in_place(&mut s);
    s
}

/// [`mask_string`], отмечающая в `changed`, что маска что-то заменила (опись экспорта:
/// `secrets_masked`).
pub fn mask_tracked(mut s: String, changed: &mut bool) -> String {
    *changed |= mask_in_place(&mut s);
    s
}

/// Маска строковых полей `fields` значения `v` для зрителя `p` (человеку — без изменений) в
/// текущем потоке — для малых значений (узел, заголовки); см. [`mask_fields`]. Запись журнала и
/// списки — [`view_json_owned`].
pub fn view_json(p: &Principal, v: &mut Value, fields: &[&str]) {
    if masks(p) {
        mask_fields(v, fields);
    }
}

/// [`view_json`] для своего значения в async-обработчике: строк в полях больше
/// [`BLOCKING_FROM`] — маска в `spawn_blocking` (тело записи до 1 МБ, лента, ревизии); пул снят —
/// 503.
pub async fn view_json_owned(
    p: &Principal,
    v: Value,
    fields: &'static [&'static str],
) -> Result<Value, ApiError> {
    let size = if masks(p) { fields_size(&v, fields) } else { 0 };
    view_with(p, v, size, move |v| {
        mask_fields(v, fields);
    })
    .await
}

/// [`view_json_owned`] с обходом `mask` вместо списка полей (в ответе несколько групп полей:
/// сравнение релизов); порог пула — по всем строкам значения.
pub async fn view_json_with(
    p: &Principal,
    v: Value,
    mask: impl FnOnce(&mut Value) + Send + 'static,
) -> Result<Value, ApiError> {
    let size = if masks(p) { strings_len(&v) } else { 0 };
    view_with(p, v, size, mask).await
}

/// Ответ из нескольких записей под ключами `keys` (эхо операций идей и ответа на вопрос:
/// `entry`, `question`, `task`, …) для зрителя `p`: у каждой — [`ENTRY_FIELDS`]; строк больше
/// [`BLOCKING_FROM`] — в пуле.
pub async fn view_entries_in(
    p: &Principal,
    v: Value,
    keys: &'static [&'static str],
) -> Result<Value, ApiError> {
    view_json_with(p, v, move |v| {
        for k in keys {
            if let Some(e) = v.get_mut(*k) {
                mask_fields(e, ENTRY_FIELDS);
            }
        }
    })
    .await
}

/// Общий путь маски своего значения для зрителя `p`: человеку — без изменений; агенту — `mask`
/// в текущем потоке, а при `size` (байт маскируемого текста) больше [`BLOCKING_FROM`] — в пуле
/// блокирующих задач; пул снят — 503.
pub async fn view_with<T: Send + 'static>(
    p: &Principal,
    mut v: T,
    size: usize,
    mask: impl FnOnce(&mut T) + Send + 'static,
) -> Result<T, ApiError> {
    if !masks(p) {
        return Ok(v);
    }
    if size <= BLOCKING_FROM {
        mask(&mut v);
        return Ok(v);
    }
    blocking(move || {
        mask(&mut v);
        v
    })
    .await
}

/// Маска полей `fields` значения `v`: поле — путь ключей через точку (`frontmatter.tags`);
/// массив на любом шаге (и сам `v`) проходится поэлементно — список записей и `ideas.title`
/// задаются тем же путём; найденное значение маскируется целиком ([`mask_value`]). Нет поля —
/// ничего. `true` — маска что-то заменила.
pub fn mask_fields(v: &mut Value, fields: &[&str]) -> bool {
    let mut changed = false;
    for f in fields {
        let path: Vec<&str> = f.split('.').collect();
        changed |= walk(v, &path, &mut |x| mask_value(x));
    }
    changed
}

/// Суммарная длина строк в полях `fields` значения `v` (те же пути, что у [`mask_fields`]).
fn fields_size(v: &Value, fields: &[&str]) -> usize {
    let mut total = 0usize;
    for f in fields {
        let path: Vec<&str> = f.split('.').collect();
        walk_ref(v, &path, &mut |x| total += strings_len(x));
    }
    total
}

/// Обход значений по пути `path` (массивы — поэлементно на любом шаге); `f` — над найденными;
/// `true` — хоть одно `f` вернуло `true`.
fn walk(v: &mut Value, path: &[&str], f: &mut dyn FnMut(&mut Value) -> bool) -> bool {
    if let Value::Array(a) = v {
        let mut changed = false;
        for x in a {
            changed |= walk(x, path, f);
        }
        return changed;
    }
    match path.split_first() {
        None => f(v),
        Some((k, rest)) => v.get_mut(*k).is_some_and(|x| walk(x, rest, f)),
    }
}

/// [`walk`] без изменения значений.
fn walk_ref(v: &Value, path: &[&str], f: &mut dyn FnMut(&Value)) {
    if let Value::Array(a) = v {
        for x in a {
            walk_ref(x, path, f);
        }
        return;
    }
    match path.split_first() {
        None => f(v),
        Some((k, rest)) => {
            if let Some(x) = v.get(*k) {
                walk_ref(x, rest, f);
            }
        }
    }
}

/// Длина всех строк значения (рекурсивно).
fn strings_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Array(a) => a.iter().map(strings_len).sum(),
        Value::Object(o) => o.values().map(strings_len).sum(),
        _ => 0,
    }
}

/// Маска всех строк значения `v` (рекурсивно, ключи объектов — нет): каждая строка — свой текст.
/// `true` — маска что-то заменила.
pub fn mask_value(v: &mut Value) -> bool {
    match v {
        Value::String(s) => mask_in_place(s),
        Value::Array(a) => a.iter_mut().fold(false, |c, x| mask_value(x) | c),
        Value::Object(o) => o.values_mut().fold(false, |c, x| mask_value(x) | c),
        _ => false,
    }
}

/// Узел `Node` как JSON для зрителя `p` ([`NODE_FIELDS`]); сбой сериализации — 500.
pub fn node_json(p: &Principal, node: &zam_store::Node) -> Result<Value, ApiError> {
    let mut v = serde_json::to_value(node)
        .map_err(|e| ApiError::Internal(format!("узел не сериализуется: {e}")))?;
    view_json(p, &mut v, NODE_FIELDS);
    Ok(v)
}

/// Ошибка для зрителя `p`: у 409 `term-conflict` заголовки найденных терминов и совпавшие имена
/// агенту — с маской (`conflicts[]` и текст `detail` строятся из них; имя — эхо запроса, как эхо
/// записи); прочие — как есть.
pub fn view_error(p: &Principal, e: ApiError) -> ApiError {
    match e {
        ApiError::TermConflict(mut c) if masks(p) => {
            for x in &mut c {
                mask_in_place(&mut x.title);
                mask_in_place(&mut x.matched);
            }
            ApiError::TermConflict(c)
        }
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Начало текста: ключ на границе — меткой целиком (не префиксом), метка не разрезается,
    /// показ не длиннее `max`; результат — начало маски всего текста; короткий — целиком.
    #[test]
    fn mask_head_boundary() {
        let k = key();
        let s = format!("{} {k} хвост", "а".repeat(50));
        let full = mask(&s).into_owned();
        // граница посреди ключа (исходник; ключ — с байта 101): метка целиком, хвоста нет
        let (h, cut) = mask_head(&s, 120);
        assert!(!h.contains("ghp_"), "{h}");
        assert!(h.ends_with(SECRET_MARK) && full.starts_with(&h), "{h}");
        assert!(cut);
        // граница посреди метки (выдача): метки нет совсем
        let (h, cut) = mask_head(&s, 101 + SECRET_MARK.len() - 2);
        assert!(!h.contains('‹') && full.starts_with(&h), "{h}");
        assert!(cut);
        // кириллица — по границе символа
        let (h, cut) = mask_head(&s, 7);
        assert_eq!(h, "ааа");
        assert!(cut);
        let (h, cut) = mask_head(&s, s.len());
        assert_eq!(h, full);
        assert!(!cut);
        // много коротких секретов: метки длиннее исходника — показ всё равно не длиннее `max`
        let many = "https://u:pw1x@h.example.com/\n".repeat(40);
        let (h, cut) = mask_head(&many, many.len());
        assert!(h.len() <= many.len() && !h.contains("pw1x") && cut, "{h}");
        assert!(mask(&many).starts_with(&h));
    }

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
        let size = fields_size(&v, &["title", "frontmatter.tags"]);
        assert_eq!(size, format!("t {k}").len() + format!("t-{k}").len() + 3);
        let changed = mask_fields(
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
        assert!(changed);
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
        assert!(
            !mask_fields(&mut v, &["title"]),
            "повторно — ничего не заменено"
        );
    }

    /// Без находок — заимствование (вариант `Cow`, а не только содержимое) и тот же текст; маска
    /// идемпотентна; длинный текст и JSON — в пуле.
    #[tokio::test]
    async fn mask_borrowed_and_owned() {
        assert!(matches!(mask("просто текст"), Cow::Borrowed(_)));
        let t = format!("token: {}\n", key());
        assert!(matches!(mask(&t), Cow::Owned(_)));
        let m = mask(&t).into_owned();
        assert_eq!(m, "token: ‹secret›\n");
        assert!(
            matches!(mask(&m), Cow::Borrowed(_)),
            "повторная маска — без копии"
        );
        let big = t.repeat(BLOCKING_FROM / t.len() + 2);
        assert!(big.len() > BLOCKING_FROM);
        let out = mask_owned(big.clone()).await.unwrap();
        assert_eq!(out, zam_markdown::mask_text(&big));
        assert_eq!(mask_owned("чисто".into()).await.unwrap(), "чисто");
        let token = Principal::Token(zam_store::AuthToken {
            token_id: zam_core::Id::new(),
            user_id: zam_core::Id::new(),
            access: zam_store::Access::Rw,
        });
        let human = Principal::Admin {
            session_id: "s".into(),
        };
        let v = view_json_owned(&token, json!({"body": big.clone()}), &["body"])
            .await
            .unwrap();
        assert_eq!(v["body"], zam_markdown::mask_text(&big));
        let v = view_json_owned(&human, json!({"body": big.clone()}), &["body"])
            .await
            .unwrap();
        assert_eq!(v["body"], big, "человеку — исходник");
        let mut s = t.clone();
        view_in_place(&token, &mut s);
        assert_eq!(s, m);
        let mut s = t.clone();
        view_in_place(&human, &mut s);
        assert_eq!(s, t);
    }

    /// 409 `term-conflict`: заголовки терминов агенту — с маской (и в `detail`), человеку — как
    /// есть; прочие ошибки — без изменений.
    #[test]
    fn view_error_term_conflict() {
        let k = key();
        let conflict = || {
            ApiError::TermConflict(vec![zam_store::TermConflict {
                id: zam_core::Id::new(),
                title: format!("токен {k}"),
                matched: "имя".into(),
            }])
        };
        let token = Principal::Token(zam_store::AuthToken {
            token_id: zam_core::Id::new(),
            user_id: zam_core::Id::new(),
            access: zam_store::Access::Rw,
        });
        let human = Principal::Admin {
            session_id: "s".into(),
        };
        let e = view_error(&token, conflict());
        assert!(!e.to_string().contains(&k), "{e}");
        let ApiError::TermConflict(c) = e else {
            panic!("тот же вид ошибки");
        };
        assert_eq!(c[0].title, "токен ‹secret›");
        assert_eq!(c[0].matched, "имя");
        assert!(view_error(&human, conflict()).to_string().contains(&k));
        assert!(matches!(
            view_error(&token, ApiError::NotFound),
            ApiError::NotFound
        ));
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
            let out = mask_owned(s).await.unwrap();
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
