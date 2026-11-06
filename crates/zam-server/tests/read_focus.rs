//! Явное фокусное чтение `POST /read {items: [{id, focus}]}` (M6 §6, задача 7b).
//!
//! Сервер без модели: фокус лексический (`focus lexical`). Откат по сроку и замер p95 с моделью —
//! `read_focus_ml.rs` (модель меняет счётчик токенов процесса).
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::attach::{db, pptx, upload, wait_done, world};
use common::read::{
    focus_sentence, l2_answers, l2_answers_with, l2_fixture, l2_world, sections_note,
};
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{SeenKey, SeenTarget, WHOLE_TARGET, count_tokens, node_leaves, seen_revs};

const READ: &str = "/api/v1/read";
/// Предупреждение: фокус проигнорирован (цель короче `read.focus_min_tokens` или неделима).
const SKIPPED: &str = "focus skipped: short";
/// Предупреждение: фокус только по FTS-лексике листов.
const LEXICAL: &str = "focus lexical";
/// Начало пометки пропуска (§6, триггер дочитывания §3.4).
const SKIP_OPEN: &str = "[пропущено: ";
/// Начало строки оглавления фокуса.
const TOC_OPEN: &str = "Оглавление: ";

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn read(a: &Api, body: Value) -> Resp {
    a.t(Method::POST, READ, Some(body)).await
}

async fn read_ok(a: &Api, body: Value) -> Value {
    let r = read(a, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn read_md(a: &Api, body: Value) -> String {
    let mut body = body;
    body["format"] = json!("md");
    let r = read(a, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.text()
}

fn warnings(it: &Value) -> Vec<String> {
    it["warnings"]
        .as_array()
        .map(|w| w.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// Пометки пропуска в тексте: `(путь, N)`.
fn skips(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(SKIP_OPEN) else {
            continue;
        };
        let inner = rest.strip_suffix(" t]").unwrap_or_else(|| panic!("{line}"));
        let (path, n) = inner
            .rsplit_once(" · ~")
            .unwrap_or_else(|| panic!("{line}"));
        out.push((
            path.to_string(),
            n.parse().unwrap_or_else(|_| panic!("{line}")),
        ));
    }
    out
}

fn toc_line(text: &str) -> &str {
    text.lines()
        .find(|l| l.starts_with(TOC_OPEN))
        .unwrap_or_else(|| panic!("нет оглавления: {text}"))
}

// ---- без фокуса — как раньше ----

/// Без `focus` ответ побайтово как эталон задачи 7a (`read_l2_m57.json`), и с явным `tier: "L2"`
/// — так же (критерий §17 п. 8: «без него ответы `/read` побайтово как с `tier=L2`»).
#[tokio::test]
async fn read_without_focus_byte_equal_l2() {
    let want = l2_fixture();
    let (a, ids) = l2_world("focus-eq").await;
    let got = l2_answers(&a, &ids, None, &["json", "md"]).await;
    let l2 = l2_answers_with(
        &a,
        &ids,
        None,
        &["json", "md"],
        |i| json!({"id": i, "tier": "L2"}),
    )
    .await;
    for f in ["json", "md"] {
        assert_eq!(got[f], want[f], "{f}");
        assert_eq!(l2[f], want[f], "tier L2, {f}");
    }
}

// ---- оглавление и пропуски ----

/// Оглавление — одна строка: не больше 40 заголовков и 150 токенов, остальное — `…ещё N
/// заголовков`; заголовок выбранного листа отмечен `*`.
#[tokio::test]
async fn focus_toc_le_150_tokens() {
    let a = api("focus-toc", |_| {}).await;
    let n = id(&a
        .note("knowledge", "Длинное", &sections_note(60, 10, 3))
        .await);
    let j = read_ok(
        &a,
        json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["tier"], "focus", "{it}");
    let text = it["text"].as_str().unwrap();
    let toc = toc_line(text);
    assert!(
        count_tokens(toc) <= 150,
        "{} токенов: {toc}",
        count_tokens(toc)
    );
    let tail = toc.rsplit(" · ").next().unwrap();
    let more: usize = tail
        .strip_prefix("…ещё ")
        .and_then(|r| r.strip_suffix(" заголовков"))
        .unwrap_or_else(|| panic!("нет «…ещё N заголовков»: {toc}"))
        .parse()
        .unwrap();
    let shown = toc[TOC_OPEN.len()..].split(" · ").count() - 1;
    assert!(shown <= 40, "{shown}: {toc}");
    // 61 заголовок H1–H2 (Руководство и 60 разделов)
    assert_eq!(shown + more, 61, "{toc}");
    assert!(toc.contains("*Раздел 3 ·"), "{toc}");
    assert_eq!(toc.matches('*').count(), 1, "{toc}");
}

/// Листы с лучшими баллами — в порядке документа, между ними и по краям — `[пропущено: <путь
/// заголовков> · ~N t]` (путь секции, годный для `section`); сумма пропусков и выданных листов —
/// весь текст.
#[tokio::test]
async fn focus_skip_marker_format() {
    let a = api("focus-skip", |_| {}).await;
    let text = sections_note(8, 10, 5);
    let n = id(&a.note("knowledge", "Восемь", &text).await);
    let item = json!({"id": format!("n:{n}"), "focus": "детектор нейтрино"});
    let j = read_ok(&a, json!({"items": [item.clone()], "budget_tokens": 20000})).await;
    let it = &j["items"][0];
    // сервер без модели — лексический фокус
    assert_eq!(warnings(it), [LEXICAL], "{it}");
    let got = it["text"].as_str().unwrap();
    assert!(got.contains("детектор нейтрино"), "{got}");
    assert!(!got.contains("раздела 4 "), "{got}");
    assert!(!got.contains("раздела 6 "), "{got}");
    // пометки — по секции каждой группы невыданных листов подряд (нарезка — счётчиком сервера)
    let d = db(&a, a.st.admin_id).await;
    let leaves = node_leaves(&d, n.parse().unwrap()).await.unwrap();
    let mut want: Vec<String> = Vec::new();
    let mut prev_given = true;
    for l in &leaves {
        if l.text.contains("нейтрино") {
            prev_given = true;
            continue;
        }
        if prev_given || want.last() != Some(&l.heading_path) {
            want.push(l.heading_path.clone());
        }
        prev_given = false;
    }
    let s = skips(got);
    let paths: Vec<String> = s.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(paths, want, "{got}");
    for k in [1, 2, 3, 4, 6, 7, 8] {
        let p = format!("Руководство > Раздел {k}");
        assert!(paths.contains(&p), "{p}: {got}");
    }
    assert_eq!(paths[0], "Руководство", "{got}");
    assert!(s.iter().all(|(_, n)| *n > 0), "{got}");
    // путь пометки — секция для дочитывания
    for (p, _) in &s[1..] {
        let r = read_ok(
            &a,
            json!({"items": [{"id": format!("n:{n}"), "section": p}], "budget_tokens": 20000}),
        )
        .await;
        assert!(r["items"][0]["error"].is_null(), "{p}: {r}");
    }
    // md: заголовок элемента, оглавление, лист, пропуски
    let md = read_md(&a, json!({"items": [item], "budget_tokens": 20000})).await;
    assert!(
        md.starts_with(&format!("### n:{n} Knowledge/Восемь")),
        "{md}"
    );
    assert!(md.contains(&format!("(warning: {LEXICAL})")), "{md}");
    assert!(toc_line(&md).contains("*Раздел 5"), "{md}");
    assert_eq!(skips(&md).len(), want.len(), "{md}");
}

// ---- целиком без фокуса ----

/// Цель не длиннее `read.focus_min_tokens` — полный текст L2 и `focus skipped: short`.
#[tokio::test]
async fn focus_short_target_skipped() {
    let a = api("focus-short", |_| {}).await;
    let text = "# Коротко\n\nДетектор нейтрино и калибровка.\n## Ещё\nТекст.\n";
    let n = id(&a.note("knowledge", "Коротко", text).await);
    let j = read_ok(
        &a,
        json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}]}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["text"], text, "{it}");
    assert_eq!(it["tier"], "L2", "{it}");
    assert_eq!(warnings(it), [SKIPPED], "{it}");
    // порог — настройка: `focus_min_tokens = 0` фокусирует и короткую
    let a = api("focus-short-0", |c| c.read.focus_min_tokens = 0).await;
    let n = id(&a.note("knowledge", "Коротко", text).await);
    let j = read_ok(
        &a,
        json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}]}),
    )
    .await;
    assert_eq!(j["items"][0]["tier"], "focus", "{j}");
}

/// Запись журнала — всегда целиком: фокус игнорируется с `focus skipped: short`.
#[tokio::test]
async fn focus_journal_entry_whole() {
    let a = api("focus-entry", |_| {}).await;
    let body = sections_note(8, 10, 5);
    let r = a
        .t(
            Method::POST,
            "/api/v1/spaces/knowledge/entries",
            Some(
                json!({"kind": "insight", "title": "Длинный insight", "body": body,
                        "data": {"kind": "pattern"}}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let e = id(&r.json());
    let j = read_ok(
        &a,
        json!({"items": [{"id": format!("i:{e}"), "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["text"], body, "{it}");
    assert_eq!(warnings(it), [SKIPPED], "{it}");
}

/// Неверные сочетания — 400 до чтения: `focus` пуст, `tier=focus` без `focus`, `focus` с
/// `section`, `locator`, `since_rev` или другим `tier`.
#[tokio::test]
async fn focus_invalid_combinations_400() {
    let a = api("focus-400", |_| {}).await;
    let n = format!("n:{}", id(&a.note("knowledge", "x", "# X\nтекст\n").await));
    for (item, want) in [
        (json!({"id": &n, "focus": "  "}), "нужен текст focus"),
        (json!({"id": &n, "tier": "focus"}), "нужен текст focus"),
        (json!({"id": &n, "focus": "x", "section": "X"}), "focus"),
        (json!({"id": &n, "focus": "x", "since_rev": 1}), "focus"),
        (json!({"id": &n, "focus": "x", "tier": "L1"}), "focus"),
        (
            json!({"id": format!("d:{}", Id::new()), "focus": "x", "locator": "page:1"}),
            "focus",
        ),
    ] {
        let r = read(&a, json!({"items": [item.clone()]})).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{item}: {}", r.text());
        assert!(r.text().contains(want), "{item}: {}", r.text());
    }
    // `tier: "focus"` с текстом — то же, что `focus` без `tier`
    let r = read(
        &a,
        json!({"items": [{"id": &n, "tier": "focus", "focus": "x"}]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

// ---- вложения ----

/// `d:` и `b:` с фокусом — фрагменты документа: оглавление из `locator_text` (до 40 частей,
/// выбранная — `*`), пропуски — подписи частей.
#[tokio::test]
async fn focus_doc_blob_locator() {
    let w = world("focus-doc", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let host = id(&a.note("knowledge", "Слайды", "# Слайды\n").await);
    let bodies: Vec<String> = (1..=50)
        .map(|i| {
            let mut b = String::new();
            for k in 0..5 {
                b.push_str(&focus_sentence(i, k));
            }
            if i == 7 {
                b.push_str("Детектор нейтрино на слайде. ");
            }
            b
        })
        .collect();
    let titles: Vec<String> = (1..=50).map(|i| format!("Тема {i}")).collect();
    let slides: Vec<(&str, &str)> = titles
        .iter()
        .zip(&bodies)
        .map(|(t, b)| (t.as_str(), b.as_str()))
        .collect();
    let up = upload(a, &a.token, &host, "deck.pptx", &pptx(&slides)).await;
    let doc = wait_done(a, user, &host, "deck.pptx").await;
    let sha = up["sha256"]
        .as_str()
        .unwrap_or(&doc.blob_sha256)
        .to_string();
    for raw in [format!("d:{}", doc.id), format!("b:{sha}")] {
        let j = read_ok(
            a,
            json!({"items": [{"id": &raw, "focus": "нейтрино"}], "budget_tokens": 20000}),
        )
        .await;
        let it = &j["items"][0];
        assert_eq!(it["tier"], "focus", "{raw}: {it}");
        let text = it["text"].as_str().unwrap();
        assert!(text.contains("Детектор нейтрино"), "{raw}: {text}");
        assert!(!text.contains("раздела 8 "), "{raw}: {text}");
        let toc = toc_line(text);
        assert!(count_tokens(toc) <= 150, "{toc}");
        assert!(toc.contains("*слайд 7"), "{toc}");
        assert!(toc.contains("…ещё "), "{toc}");
        let shown = toc[TOC_OPEN.len()..].split(" · ").count() - 1;
        assert!(shown <= 40, "{toc}");
        let s = skips(text);
        assert_eq!(
            s.first().map(|p| p.0.as_str()),
            Some("слайд 1 — слайд 6"),
            "{text}"
        );
        assert_eq!(
            s.last().map(|p| p.0.as_str()),
            Some("слайд 8 — слайд 50"),
            "{text}"
        );
    }
}

/// `a:<chunk>` — фрагмент целиком (он и есть единица): текст как без фокуса и
/// `focus skipped: short`.
#[tokio::test]
async fn focus_chunk_whole() {
    let w = world("focus-chunk", false).await;
    let a = &w.a;
    let user = a.st.admin_id;
    let host = id(&a.note("knowledge", "Слайды", "# Слайды\n").await);
    let body: String = (0..60).map(|k| focus_sentence(1, k)).collect();
    upload(a, &a.token, &host, "one.pptx", &pptx(&[("Тема", &body)])).await;
    let doc = wait_done(a, user, &host, "one.pptx").await;
    let chunk = common::attach::chunk_locators(a, user, doc.id).await[0]
        .0
        .clone();
    let raw = format!("a:{chunk}");
    let plain = read_ok(a, json!({"items": [{"id": &raw}], "budget_tokens": 20000})).await;
    let j = read_ok(
        a,
        json!({"items": [{"id": &raw, "focus": "журналы"}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["text"], plain["items"][0]["text"], "{it}");
    assert_eq!(warnings(it), [SKIPPED], "{it}");
}

// ---- выданное сессии ----

/// В `session_seen` — только выданные листы (`part = chunk_id`), не цель целиком.
#[tokio::test]
async fn focus_marks_only_leaves_seen() {
    let a = api("focus-seen", |_| {}).await;
    let n = id(&a
        .note("knowledge", "Восемь", &sections_note(8, 10, 5))
        .await);
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            READ,
            Some(
                json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}],
                        "budget_tokens": 20000}),
            ),
            &[("x-zam-session", "s-focus")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let text = r.json()["items"][0]["text"].as_str().unwrap().to_string();
    let user: Id = a.st.admin_id;
    let d = db(&a, user).await;
    let nid: Id = n.parse().unwrap();
    let target: SeenTarget = TargetRef {
        kind: TargetKind::Node,
        id: nid,
    }
    .into();
    let key = |part: &str| SeenKey {
        owner_id: user,
        target: target.clone(),
        part: part.to_string(),
    };
    let leaves = node_leaves(&d, nid).await.unwrap();
    let mut keys: Vec<SeenKey> = leaves.iter().map(|l| key(&l.chunk_id)).collect();
    keys.push(key(WHOLE_TARGET));
    let revs = seen_revs(&d, "s-focus", 0, &keys).await.unwrap();
    assert!(
        !revs.contains_key(&key(WHOLE_TARGET)),
        "цель целиком не выдана"
    );
    let mut given = 0;
    for l in &leaves {
        let shown = text.contains(l.text.trim());
        assert_eq!(
            revs.get(&key(&l.chunk_id)).copied(),
            shown.then_some(1),
            "{}: {}",
            l.heading_path,
            text
        );
        given += usize::from(shown);
    }
    assert!(
        given >= 1 && given < leaves.len(),
        "{given} из {}",
        leaves.len()
    );
}
