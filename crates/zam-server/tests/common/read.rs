//! Эталон `POST /read` L2 (M6 §7.3, задача 7a, шаг 0): мир из 10 целей и снятие ответов с метками
//! вместо id. Общий для `read_batch` (7a) и `read_focus` (7b: без `focus` — побайтово как эталон).
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{Api, api};

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn read(a: &Api, body: Value) -> super::api::Resp {
    a.t(Method::POST, "/api/v1/read", Some(body)).await
}

/// Длинный текст: `n` строк по ~10 токенов.
pub fn long_text(title: &str, n: usize) -> String {
    let mut t = format!("# {title}\n\n");
    for i in 0..n {
        t.push_str(&format!("Строка номер {i} про квазар и далёкие звёзды.\n"));
    }
    t
}

/// Эталон ответа `POST /read` без сессии на 10 целях L2 (снят
/// `read_batch::snapshot_read_l2_fixture` до правки 7a): id заменены метками `<t0>`…`<t9>`, итог `total_tokens` — меткой `<total>`.
/// Пометка «уже выдавалось» без сессии не появляется — ответ побайтово прежний.
pub const READ_L2_FIXTURE: &str = "tests/fixtures/read_l2_m57.json";

/// 10 целей L2: шесть заметок (заголовки, код, таблица, длинный текст, вложенная папка, пустая
/// секция) и четыре записи (три insight разных видов и term; changelog — только в решении);
/// `(сервер, id по порядку)`.
pub async fn l2_world(name: &str) -> (Api, Vec<String>) {
    let a = api(&format!("read-l2-{name}"), |_| {}).await;
    let mut ids = Vec::new();
    for (path, text) in [
        (
            "Инструменты/Bash",
            "# Bash\n\nСкрипты пишите с LF.\n## Ловушки\nCRLF ломает shebang.\n",
        ),
        (
            "Код/Пример",
            "# Пример\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n",
        ),
        ("Таблица", "# Таблица\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"),
        ("Длинная", &long_text("Длинная", 30)),
        (
            "Папка/Вложенная/Заметка",
            "# Вложенная\n\nТекст вложенной заметки.\n",
        ),
        ("Пустая секция", "# Пусто\n## Ничего\n## Что-то\nтекст\n"),
    ] {
        ids.push(format!("n:{}", id(&a.note("knowledge", path, text).await)));
    }
    let entry = |body: Value| {
        let a = &a;
        async move {
            let r = a
                .t(Method::POST, "/api/v1/spaces/knowledge/entries", Some(body))
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            id(&r.json())
        }
    };
    let i1 = entry(
        json!({"kind": "insight", "title": "LF в bash", "body": "Используйте LF.",
                         "data": {"kind": "gotcha"}}),
    )
    .await;
    let c = entry(
        json!({"kind": "insight", "title": "Сбой разбора", "body": "Разбор падал.",
                        "data": {"kind": "failure"}}),
    )
    .await;
    let t = entry(
        json!({"kind": "term", "title": "Выжимка", "body": "Подробности.",
                        "data": {"definition": "Сводка ответа агента", "aliases": []}}),
    )
    .await;
    let i2 = entry(
        json!({"kind": "insight", "title": "Миграции", "body": "Ставьте lock_timeout.",
                         "data": {"kind": "pattern"}}),
    )
    .await;
    ids.push(format!("i:{i1}"));
    ids.push(format!("i:{c}"));
    ids.push(format!("t:{t}"));
    ids.push(format!("i:{i2}"));
    (a, ids)
}

/// Сырые ответы (тело как есть) в форматах `formats` с метками вместо id: `{формат: текст}`;
/// `session` — с этой сессией. Итог (`total_tokens`, строка `~N tok`) зависит от токенизации
/// случайных id в заголовках — он заменён меткой `<total>`.
pub async fn l2_answers(a: &Api, ids: &[String], session: Option<&str>, formats: &[&str]) -> Value {
    l2_answers_with(a, ids, session, formats, |i| json!({"id": i})).await
}

/// [`l2_answers`] с элементом запроса `item(id)` (например, `{id, tier: "L2"}`).
pub async fn l2_answers_with(
    a: &Api,
    ids: &[String],
    session: Option<&str>,
    formats: &[&str],
    item: impl Fn(&str) -> Value,
) -> Value {
    let items: Vec<Value> = ids.iter().map(|i| item(i)).collect();
    let mut out = serde_json::Map::new();
    for &format in formats {
        let mut body = json!({"items": items, "format": format, "budget_tokens": 5000});
        if let Some(s) = session {
            body["session_id"] = json!(s);
        }
        let r = read(a, body).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let t = r.text();
        let mut text = if format == "json" {
            let at = t.rfind("\"total_tokens\":").unwrap();
            let end = at + t[at..].find('}').unwrap();
            format!("{}\"total_tokens\":\"<total>\"{}", &t[..at], &t[end..])
        } else {
            let last = t.trim_end().rfind('\n').map_or(0, |i| i + 1);
            assert!(t[last..].starts_with('~'), "{t}");
            format!("{}~<total> tok\n", &t[..last])
        };
        for (k, i) in ids.iter().enumerate() {
            let bare = i.split_once(':').unwrap().1;
            text = text.replace(bare, &format!("<t{k}>"));
        }
        out.insert(format.to_string(), json!(text));
    }
    Value::Object(out)
}

/// Эталон [`READ_L2_FIXTURE`]: `{json, md}`.
pub fn l2_fixture() -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(READ_L2_FIXTURE);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

// ---- заметки явного фокуса (M6 §6, задача 7b) ----

/// Предложение-наполнитель раздела `i` (~12 токенов), без слов фокуса.
pub fn focus_sentence(i: usize, k: usize) -> String {
    format!("Пункт {k} раздела {i} описывает порядок работы с конфигурацией и журналами сервера. ")
}

/// Заметка из `n` разделов H2 по `per` предложений; раздел `hot` содержит слово «нейтрино».
pub fn sections_note(n: usize, per: usize, hot: usize) -> String {
    // введение и разделы — не короче `merge_below` (100) и не длиннее `target_max` (500) чанкера:
    // по листу на раздел, без склеек
    let mut t = String::from("# Руководство\n\n");
    for k in 0..10 {
        t.push_str(&focus_sentence(0, k));
    }
    t.push_str("\n\n");
    for i in 1..=n {
        t.push_str(&format!("## Раздел {i}\n\n"));
        for k in 0..per {
            t.push_str(&focus_sentence(i, k));
        }
        if i == hot {
            t.push_str("Здесь же детектор нейтрино и калибровка детектора нейтрино. ");
        }
        t.push_str("\n\n");
    }
    t
}
