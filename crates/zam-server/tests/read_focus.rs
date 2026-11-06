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

/// Пометка пропуска: адрес, подпись, `N`.
#[derive(Debug, Clone)]
struct Skip {
    addr: String,
    label: String,
    tokens: usize,
}

/// Пометки `[пропущено: <адрес> (<подпись>) · ~N t]` в тексте.
fn skips(text: &str) -> Vec<Skip> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(SKIP_OPEN) else {
            continue;
        };
        let inner = rest.strip_suffix(" t]").unwrap_or_else(|| panic!("{line}"));
        let (head, n) = inner
            .rsplit_once(" · ~")
            .unwrap_or_else(|| panic!("{line}"));
        let (addr, label) = head.split_once(" (").unwrap_or_else(|| panic!("{line}"));
        out.push(Skip {
            addr: addr.to_string(),
            label: label.strip_suffix(')').unwrap_or(label).to_string(),
            tokens: n.parse().unwrap_or_else(|_| panic!("{line}")),
        });
    }
    out
}

fn toc_line(text: &str) -> &str {
    text.lines()
        .find(|l| l.starts_with(TOC_OPEN))
        .unwrap_or_else(|| panic!("нет оглавления: {text}"))
}

/// `range:r<rev>:S-E` → `(rev, S, E)`.
fn range_of(addr: &str) -> (i64, usize, usize) {
    let (rev, span) = addr
        .strip_prefix("range:r")
        .and_then(|r| r.split_once(':'))
        .unwrap_or_else(|| panic!("не range: {addr}"));
    let (a, b) = span.split_once('-').unwrap();
    (rev.parse().unwrap(), a.parse().unwrap(), b.parse().unwrap())
}

/// Дочитывание по адресу пометки: элемент `/read {id, range}` — ровно байты текста `full`,
/// `~N` пометки — токены этих байт счётчиком сервера.
async fn reread_exact(a: &Api, id: &str, full: &str, s: &Skip) -> String {
    let (rev, x, y) = range_of(&s.addr);
    let r = read_ok(
        a,
        json!({"items": [{"id": id, "range": format!("r{rev}:{x}-{y}")}], "budget_tokens": 20000}),
    )
    .await;
    let it = &r["items"][0];
    assert!(it["error"].is_null(), "{s:?}: {it}");
    let got = it["text"].as_str().unwrap();
    assert_eq!(got, &full[x..y], "{s:?}");
    assert_eq!(s.tokens, count_tokens(got), "{s:?}");
    got.to_string()
}

/// `focus_parts` — по порядку, без пересечений и дыр, от 0 до конца текста; выданные — в тексте
/// ответа как есть.
fn check_parts(it: &Value, full: &str) {
    let parts = it["focus_parts"]
        .as_array()
        .unwrap_or_else(|| panic!("{it}"));
    let text = it["text"].as_str().unwrap();
    let mut at = 0usize;
    for p in parts {
        let (_, x, y) = range_of(p["part"].as_str().unwrap());
        assert_eq!(x, at, "{it}");
        if p["shown"] == true {
            assert!(
                text.contains(&full[x..y]),
                "кусок {x}-{y} не выдан как есть: {text}"
            );
        }
        at = y;
    }
    assert_eq!(at, full.len(), "{it}");
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
/// заголовков`; заголовок выданного отмечен `*`; отмеченный за пределом — окном (`…ещё N выше`).
#[tokio::test]
async fn focus_toc_le_150_tokens() {
    let a = api("focus-toc", |_| {}).await;
    for (hot, window) in [(3, false), (55, true)] {
        let n = id(&a
            .note(
                "knowledge",
                &format!("Длинное {hot}"),
                &sections_note(60, 10, hot),
            )
            .await);
        let j = read_ok(
            &a,
            json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}],
                   "budget_tokens": 20000}),
        )
        .await;
        let it = &j["items"][0];
        assert_eq!(it["tier"], "focus", "{it}");
        let toc = toc_line(it["text"].as_str().unwrap());
        assert!(
            count_tokens(toc) <= 150,
            "{} токенов: {toc}",
            count_tokens(toc)
        );
        let items: Vec<&str> = toc[TOC_OPEN.len()..].split(" · ").collect();
        let tail = items.last().unwrap();
        // окно у конца — без хвоста
        let more: usize = tail
            .strip_prefix("…ещё ")
            .and_then(|r| r.strip_suffix(" заголовков"))
            .map_or(0, |n| n.parse().unwrap());
        assert!(window || more > 0, "нет «…ещё N заголовков»: {toc}");
        let above: usize = items[0]
            .strip_prefix("…ещё ")
            .and_then(|r| r.strip_suffix(" выше"))
            .map_or(0, |n| n.parse().unwrap());
        let shown = items.len() - usize::from(more > 0) - usize::from(above > 0);
        assert!(shown <= 40, "{shown}: {toc}");
        // 61 заголовок H1–H2 (Руководство и 60 разделов)
        assert_eq!(above + shown + more, 61, "{toc}");
        assert_eq!(above > 0, window, "{toc}");
        assert!(toc.contains(&format!("*Раздел {hot} ·")), "{toc}");
        assert_eq!(toc.matches('*').count(), 1, "{toc}");
    }
}

/// Пропуск — один на промежуток между выданными кусками и по краям (не больше `focus_leaves` + 1),
/// адрес `range:r<rev>:S-E` ведёт ровно к пропущенному, `~N` — его токены; подпись — первая и последняя
/// секция и их число; куски и пропуски вместе — весь текст (`focus_parts`).
#[tokio::test]
async fn focus_skip_marker_format() {
    let a = api("focus-skip", |_| {}).await;
    // 7 предложений — по листу на раздел и с оценкой чанкера, и с bge-m3
    let text = sections_note(8, 7, 5);
    let n = id(&a.note("knowledge", "Восемь", &text).await);
    let nid = format!("n:{n}");
    let item = json!({"id": &nid, "focus": "детектор нейтрино"});
    let j = read_ok(&a, json!({"items": [item.clone()], "budget_tokens": 20000})).await;
    let it = &j["items"][0];
    // сервер без модели — лексический фокус
    assert_eq!(warnings(it), [LEXICAL], "{it}");
    let got = it["text"].as_str().unwrap();
    assert!(got.contains("детектор нейтрино"), "{got}");
    assert!(
        got.contains("## Раздел 5"),
        "кусок начинается со строки заголовка: {got}"
    );
    assert!(!got.contains("раздела 4 "), "{got}");
    assert!(!got.contains("раздела 6 "), "{got}");
    check_parts(it, &text);
    let s = skips(got);
    assert!((1..=4).contains(&s.len()), "{got}");
    // до выданного — свой текст H1 и разделы 1–4 одной пометкой
    assert_eq!(
        s[0].label, "Руководство — Руководство > Раздел 4, секций: 5",
        "{got}"
    );
    let before = reread_exact(&a, &nid, &text, &s[0]).await;
    assert!(before.starts_with("# Руководство"), "{before}");
    assert!(
        before.contains("## Раздел 4") && !before.contains("## Раздел 5"),
        "{before}"
    );
    assert_eq!(
        s.last().unwrap().label.split(" — ").last().unwrap(),
        "Руководство > Раздел 8, секций: 3"
    );
    for k in &s {
        reread_exact(&a, &nid, &text, k).await;
    }
    // md: заголовок элемента, оглавление, куски, пропуски
    let md = read_md(&a, json!({"items": [item], "budget_tokens": 20000})).await;
    assert!(
        md.starts_with(&format!("### n:{n} Knowledge/Восемь")),
        "{md}"
    );
    assert!(md.contains(" · фокус · ~"), "{md}");
    assert!(md.contains(&format!("(warning: {LEXICAL})")), "{md}");
    assert!(toc_line(&md).contains("*Раздел 5"), "{md}");
    assert_eq!(skips(&md).len(), s.len(), "{md}");
}

/// Свой текст родителя (вступление под H1, текст H2 над H3) — пометка ведёт ровно к нему, без
/// подсекций (ревью 7b, I-1): по адресу дочитывается тот же текст и столько же токенов.
#[tokio::test]
async fn focus_parent_own_text_exact() {
    let a = api("focus-own", |_| {}).await;
    let mut text = String::from("# Корень\n\n");
    for k in 0..12 {
        text.push_str(&focus_sentence(0, k));
    }
    text.push_str("\n\n## Глава\n\n");
    for k in 0..12 {
        text.push_str(&focus_sentence(1, k));
    }
    text.push_str("\n\n### Подглава\n\nЗдесь детектор нейтрино. ");
    for k in 0..12 {
        text.push_str(&focus_sentence(2, k));
    }
    text.push_str("\n\n### Другая\n\n");
    for k in 0..12 {
        text.push_str(&focus_sentence(3, k));
    }
    text.push('\n');
    let n = id(&a.note("knowledge", "Своё", &text).await);
    let nid = format!("n:{n}");
    let a2 = api("focus-own-1", |c| c.read.focus_leaves = 1).await;
    let n2 = id(&a2.note("knowledge", "Своё", &text).await);
    for (a, nid) in [(&a, nid), (&a2, format!("n:{n2}"))] {
        let j = read_ok(
            a,
            json!({"items": [{"id": &nid, "focus": "детектор нейтрино"}],
                   "budget_tokens": 20000}),
        )
        .await;
        let it = &j["items"][0];
        if it["tier"] != "focus" {
            // листов не больше focus_leaves — целиком
            assert_eq!(warnings(it), [SKIPPED], "{it}");
            continue;
        }
        check_parts(it, &text);
        let s = skips(it["text"].as_str().unwrap());
        let first = reread_exact(a, &nid, &text, &s[0]).await;
        // свой текст корня и главы — без подглав
        assert!(first.starts_with("# Корень"), "{first}");
        assert!(!first.contains("### Подглава"), "{first}");
        assert_eq!(s[0].label, "Корень — Корень > Глава, секций: 2", "{s:?}");
    }
}

/// Текст до первого заголовка — подпись `начало`, адрес дочитывает его (ревью 7b, I-2).
#[tokio::test]
async fn focus_preamble_range() {
    let a = api("focus-pre", |_| {}).await;
    let mut text = String::new();
    for k in 0..12 {
        text.push_str(&focus_sentence(0, k));
    }
    text.push_str("\n\n");
    text.push_str(&sections_note(6, 10, 4));
    let n = id(&a.note("knowledge", "Преамбула", &text).await);
    let nid = format!("n:{n}");
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    check_parts(it, &text);
    let s = skips(it["text"].as_str().unwrap());
    assert!(s[0].label.starts_with("начало — "), "{s:?}");
    let pre = reread_exact(&a, &nid, &text, &s[0]).await;
    assert!(pre.starts_with("Пункт 0 раздела 0"), "{pre}");
}

/// Склейки чанкера (секции короче `merge_below` — с соседней, путь листа — первой) на подпись не
/// влияют: подпись — секции, чей текст в промежутке, их число — по заголовкам (ревью 7b, I-3).
#[tokio::test]
async fn focus_merged_short_sections_label() {
    let a = api("focus-merge", |_| {}).await;
    let mut text = String::from("# Справка\n\n");
    for i in 1..=30 {
        // коротко: склеиваются чанкером
        text.push_str(&format!("## Пункт {i}\n\nКоротко о пункте {i}.\n\n"));
    }
    text.push_str("## Важное\n\n");
    for k in 0..12 {
        text.push_str(&focus_sentence(9, k));
    }
    text.push_str("Детектор нейтрино.\n\n");
    for i in 31..=60 {
        text.push_str(&format!("## Пункт {i}\n\nКоротко о пункте {i}.\n\n"));
    }
    let n = id(&a.note("knowledge", "Склейки", &text).await);
    let nid = format!("n:{n}");
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["tier"], "focus", "{it}");
    check_parts(it, &text);
    for s in skips(it["text"].as_str().unwrap()) {
        let gap = reread_exact(&a, &nid, &text, &s).await;
        let heads = gap.lines().filter(|l| l.starts_with("## ")).count();
        let lead = gap.split("\n## ").next().unwrap_or("");
        let own = usize::from(!lead.trim().is_empty() && !lead.trim_start().starts_with("## "));
        let k: usize = s
            .label
            .rsplit_once("секций: ")
            .map_or(1, |(_, k)| k.parse().unwrap());
        assert_eq!(k, heads + own, "{s:?}: {gap}");
    }
}

/// Бюджет по умолчанию (1500) на длинной заметке (30 × H2 × 3 × H3): лист в конце — целиком, без
/// обрезки; пометок не больше `focus_leaves` + 1 (ревью 7b, I-4).
#[tokio::test]
async fn focus_budget_default_last_leaf_whole() {
    let a = api("focus-budget", |_| {}).await;
    let mut text = String::from("# Том\n\n");
    for i in 1..=30 {
        text.push_str(&format!("## Часть {i}\n\n"));
        for j in 1..=3 {
            text.push_str(&format!("### Глава {i}.{j}\n\n"));
            for k in 0..6 {
                text.push_str(&focus_sentence(i * 10 + j, k));
            }
            if i == 30 && j == 3 {
                text.push_str("Последний детектор нейтрино.");
            }
            text.push_str("\n\n");
        }
    }
    let n = id(&a.note("knowledge", "Том", &text).await);
    let nid = format!("n:{n}");
    let j = read_ok(&a, json!({"items": [{"id": &nid, "focus": "нейтрино"}]})).await;
    let it = &j["items"][0];
    assert_eq!(it["tier"], "focus", "{it}");
    assert!(it.get("truncated").is_none(), "{it}");
    let got = it["text"].as_str().unwrap();
    // размер выдачи — в бюджете по умолчанию (1500), счётчиком сервера
    assert!(count_tokens(got) <= 1500, "{} токенов", count_tokens(got));
    assert_eq!(
        it["tokens"].as_u64().unwrap() as usize,
        count_tokens(got),
        "{it}"
    );
    let d = db(&a, a.st.admin_id).await;
    let hot = node_leaves(&d, n.parse().unwrap())
        .await
        .unwrap()
        .into_iter()
        .find(|l| l.text.contains("Последний детектор"))
        .unwrap();
    assert!(got.contains(hot.text.trim()), "{got}");
    assert!((1..=4).contains(&skips(got).len()), "{got}");
    check_parts(it, &text);
}

/// Повторяющиеся пути секций: `*` — у той, чей текст выдан; адрес байтовый — однозначный
/// (ревью 7b, M-4).
#[tokio::test]
async fn focus_duplicate_section_paths() {
    let a = api("focus-dup", |_| {}).await;
    let mut text = String::from("# Заметка\n\n");
    for (i, hot) in [(1, false), (2, false), (3, true), (4, false)] {
        text.push_str("## Повтор\n\n");
        for k in 0..10 {
            text.push_str(&focus_sentence(i, k));
        }
        if hot {
            text.push_str("Детектор нейтрино. ");
        }
        text.push_str("\n\n");
    }
    let n = id(&a.note("knowledge", "Повторы", &text).await);
    let nid = format!("n:{n}");
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    let toc = toc_line(it["text"].as_str().unwrap());
    assert_eq!(
        toc, "Оглавление: Заметка · Повтор · Повтор · *Повтор · Повтор",
        "{it}"
    );
    check_parts(it, &text);
    for s in skips(it["text"].as_str().unwrap()) {
        reread_exact(&a, &nid, &text, &s).await;
    }
}

/// Лист с секретом (нарезка — по маскированному тексту) не находится в тексте — целиком с
/// `focus skipped: not indexed` (ревью 7b, M-7); листов не больше `focus_leaves` — целиком с
/// `focus skipped: short` без эмбеддинга (M-9).
#[tokio::test]
async fn focus_not_indexed_and_few_leaves() {
    let a = api("focus-noidx", |_| {}).await;
    let key = ["AK", "IA", "Z7Q4", "MRT2", "WXPH", "BK9C"].concat();
    let mut text = sections_note(8, 10, 5);
    text.push_str(&format!("## Ключи\n\nКлюч доступа {key} для склада.\n"));
    let n = id(&a.note("knowledge", "Секрет", &text).await);
    let j = read_ok(
        &a,
        json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}],
               "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    // ключ AWS сканер маскирует — лист с ним в тексте не находится
    assert_eq!(it["tier"], "L2", "{it}");
    assert_eq!(it["text"], text.as_str(), "{it}");
    assert_eq!(warnings(it), ["focus skipped: not indexed"], "{it}");
    let a = api("focus-few", |c| c.read.focus_leaves = 10).await;
    let text = sections_note(8, 7, 5);
    let n = id(&a.note("knowledge", "Мало", &text).await);
    let j = read_ok(
        &a,
        json!({"items": [{"id": format!("n:{n}"), "focus": "нейтрино"}],
               "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["tier"], "L2", "{it}");
    assert_eq!(it["text"], text.as_str(), "{it}");
    assert_eq!(warnings(it), [SKIPPED], "{it}");
}

/// `range` — `r<rev>:S-E`, байты L2 ревизии: без ревизии, с `section`, `since_rev`, `focus`,
/// другим `tier` и у вложения — 400; за концом текста и на середине символа — `invalid` с
/// причиной; ревизии ещё нет — `invalid`.
#[tokio::test]
async fn read_range_validation() {
    let a = api("focus-range", |_| {}).await;
    let text = "# Ёж\n\nтекст\n";
    let n = format!("n:{}", id(&a.note("knowledge", "Ёж", text).await));
    let j = read_ok(&a, json!({"items": [{"id": &n, "range": "r1:0-8"}]})).await;
    assert_eq!(j["items"][0]["text"], &text[0..8], "{j}");
    assert_eq!(j["items"][0]["range"], "r1:0-8", "{j}");
    let md = read_md(&a, json!({"items": [{"id": &n, "range": "range:r1:0-8"}]})).await;
    assert!(md.contains(" › range:r1:0-8  rev1"), "{md}");
    for (r, why) in [
        ("r1:0-999", "за концом текста"),
        ("r1:0-3", "середина символа"),
        ("r5:0-3", "ревизии 5 нет"),
    ] {
        let j = read_ok(&a, json!({"items": [{"id": &n, "range": r}]})).await;
        let it = &j["items"][0];
        assert_eq!(it["error"], "invalid", "{r}: {j}");
        assert!(it["detail"].as_str().unwrap().contains(why), "{r}: {j}");
    }
    for item in [
        json!({"id": &n, "range": "0-8"}),
        json!({"id": &n, "range": "r1:8-2"}),
        json!({"id": &n, "range": "x"}),
        json!({"id": &n, "range": "r1:0-8", "section": "Ёж"}),
        json!({"id": &n, "range": "r1:0-8", "since_rev": 1}),
        json!({"id": &n, "range": "r1:0-8", "tier": "L1"}),
        json!({"id": &n, "range": "r1:0-8", "focus": "x"}),
        json!({"id": format!("d:{}", Id::new()), "range": "r1:0-8"}),
    ] {
        let r = read(&a, json!({"items": [item.clone()]})).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{item}: {}", r.text());
    }
}

/// Правка между фокусом и дочитыванием: адрес ведёт в ту ревизию, по которой выдан фокус —
/// её байты и предупреждение `range: rev1, текущая rev2`; ревизия снята сроком хранения — ошибка
/// элемента «ревизия 1 недоступна — перечитайте с focus» (повторное ревью 7b, I-A).
#[tokio::test]
async fn range_after_edit_and_removed_revision() {
    let a = api("focus-range-rev", |_| {}).await;
    let text = sections_note(8, 7, 5);
    let n = id(&a.note("knowledge", "Правка", &text).await);
    let nid = format!("n:{n}");
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let s = skips(j["items"][0]["text"].as_str().unwrap());
    let (rev, x, y) = range_of(&s[0].addr);
    assert_eq!(rev, 1, "{s:?}");
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{n}"),
            Some(json!({"content": format!("# Новое начало\n\n{text}")})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "range": format!("r1:{x}-{y}")}], "budget_tokens": 20000}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["text"], &text[x..y], "байты ревизии 1: {it}");
    assert_eq!(it["rev"], 1, "{it}");
    assert_eq!(warnings(it), ["range: rev1, текущая rev2"], "{it}");
    // ревизии нет (ревизии узлов только дописываются — снятие моделируется в обход триггера
    // append-only: путь «ревизия недоступна» защитный, как у `since_rev`)
    let d = db(&a, a.st.admin_id).await;
    let node: Id = n.parse().unwrap();
    d.write(move |t| {
        t.execute_batch("DROP TRIGGER revisions_no_delete")?;
        t.execute(
            "DELETE FROM revisions WHERE node_id = ?1 AND rev = 1",
            [node.to_string()],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "range": format!("r1:{x}-{y}")}]}),
    )
    .await;
    let it = &j["items"][0];
    assert_eq!(it["error"], "not_found", "{it}");
    assert_eq!(
        it["detail"], "ревизия 1 недоступна — перечитайте с focus",
        "{it}"
    );
}

/// `range` идёт тем же путём доступа и по тому же тексту, что L2: байты заметки с секретом — те
/// же, что в L2 (L2 не маскируется, маскирует только нарезка индекса); чужой токен — как у L2.
#[tokio::test]
async fn range_same_text_and_access_as_l2() {
    let a = api("focus-range-acl", |_| {}).await;
    let key = ["AK", "IA", "Z7Q4", "MRT2", "WXPH", "BK9C"].concat();
    let text = format!("# Склад\n\nКлюч доступа {key} для склада. Ещё текст.\n");
    let n = id(&a.note("knowledge", "Склад", &text).await);
    let nid = format!("n:{n}");
    let l2 = read_ok(&a, json!({"items": [{"id": &nid}]})).await;
    let l2 = l2["items"][0]["text"].as_str().unwrap().to_string();
    let (x, y) = (l2.find("Ключ").unwrap(), l2.find(" Ещё").unwrap());
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "range": format!("r1:{x}-{y}")}]}),
    )
    .await;
    assert_eq!(j["items"][0]["text"], &l2[x..y], "{j}");
    let (_, tok) = a.new_user("other7b").await;
    let plain = a
        .tok(
            &tok,
            Method::POST,
            READ,
            Some(json!({"items": [{"id": &nid}]})),
        )
        .await;
    let ranged = a
        .tok(
            &tok,
            Method::POST,
            READ,
            Some(json!({"items": [{"id": &nid, "range": format!("r1:{x}-{y}")}]})),
        )
        .await;
    assert_eq!(plain.status, ranged.status);
    assert_eq!(
        plain.json()["items"][0]["error"],
        ranged.json()["items"][0]["error"],
        "{}",
        ranged.text()
    );
    assert!(
        ranged.json()["items"][0]["text"].is_null(),
        "{}",
        ranged.text()
    );
}

/// Дочитывание `range` в сессии отмечает выданными листы, целиком лежащие в диапазоне
/// (повторное ревью 7b, M-new-1).
#[tokio::test]
async fn range_marks_leaves_within_seen() {
    let a = api("focus-range-seen", |_| {}).await;
    let text = sections_note(8, 7, 5);
    let n = id(&a.note("knowledge", "Отметки", &text).await);
    let nid = format!("n:{n}");
    let j = read_ok(
        &a,
        json!({"items": [{"id": &nid, "focus": "нейтрино"}], "budget_tokens": 20000}),
    )
    .await;
    let s = skips(j["items"][0]["text"].as_str().unwrap());
    let (rev, x, y) = range_of(&s[0].addr);
    let r = a
        .tok_h(
            &a.token,
            Method::POST,
            READ,
            Some(
                json!({"items": [{"id": &nid, "range": format!("r{rev}:{x}-{y}")}],
                        "budget_tokens": 20000}),
            ),
            &[("x-zam-session", "s-range")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let user: Id = a.st.admin_id;
    let d = db(&a, user).await;
    let nid_: Id = n.parse().unwrap();
    let target: SeenTarget = TargetRef {
        kind: TargetKind::Node,
        id: nid_,
    }
    .into();
    let leaves = node_leaves(&d, nid_).await.unwrap();
    let keys: Vec<SeenKey> = leaves
        .iter()
        .map(|l| SeenKey {
            owner_id: user,
            target: target.clone(),
            part: l.chunk_id.clone(),
        })
        .collect();
    let revs = seen_revs(&d, "s-range", 0, &keys).await.unwrap();
    let gap = &text[x..y];
    let mut marked = 0;
    for (l, k) in leaves.iter().zip(&keys) {
        let inside = gap.contains(l.text.trim());
        if inside {
            assert_eq!(revs.get(k).copied(), Some(1), "{}", l.heading_path);
            marked += 1;
        } else {
            assert!(!revs.contains_key(k), "{}", l.heading_path);
        }
    }
    assert!(marked >= 4, "{marked}");
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
    // порог — настройка: `focus_min_tokens = 0` и один лист на выдачу фокусируют и короткую
    let a = api("focus-short-0", |c| {
        c.read.focus_min_tokens = 0;
        c.read.focus_leaves = 1;
    })
    .await;
    let n = id(&a.note("knowledge", "Восемь", &sections_note(8, 7, 5)).await);
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
/// `section`, `locator`, `since_rev`, `range` или другим `tier`.
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
/// выбранная — `*`), пропуски — адреса `locator` подряд идущих частей.
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
        // адрес — `locator` первой и последней части, подпись — справочно (ревью 7b, M-2)
        let s = skips(text);
        assert_eq!(s.len(), 2, "{text}");
        assert_eq!(s[0].addr, "slide:1 — slide:6", "{text}");
        assert_eq!(s[0].label, "слайд 1 — слайд 6", "{text}");
        assert_eq!(s[1].addr, "slide:8 — slide:50", "{text}");
        let parts = it["focus_parts"].as_array().unwrap();
        assert_eq!(parts.len(), 3, "{it}");
        assert_eq!(parts[1]["part"], "slide:7", "{it}");
        // по адресу дочитывается часть
        let r = read_ok(
            a,
            json!({"items": [{"id": &raw, "locator": "slide:6"}], "budget_tokens": 20000}),
        )
        .await;
        assert!(
            r["items"][0]["text"]
                .as_str()
                .unwrap()
                .contains("раздела 6 "),
            "{r}"
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

// ---- замер FTS-части на корпусе eval ----

fn md_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            md_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

/// Замер для журнала (ревью 7b, M-6): p95 FTS-части фокуса (`zam_store::focus_leaves` — листы
/// цели и совпадения слов в диапазоне `rowid` цели) на корпусе `eval/corpus` (212 заметок, база
/// на HDD каталога сборки). Запуск: `cargo test -p zam-server --test read_focus
/// focus_fts_p95_eval_corpus -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "замер для журнала: FTS фокуса на корпусе eval"]
async fn focus_fts_p95_eval_corpus() {
    let a = api("focus-fts-eval", |_| {}).await;
    let root = common::ml::repo().join("eval").join("corpus");
    let mut files = Vec::new();
    md_files(&root, &mut files);
    files.sort();
    let mut ids: Vec<Id> = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().with_extension("");
        let path = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let text = std::fs::read_to_string(f).unwrap().replace("\r\n", "\n");
        ids.push(
            id(&a.note("knowledge", &path, &text).await)
                .parse()
                .unwrap(),
        );
    }
    let d = db(&a, a.st.admin_id).await;
    let queries = [
        "timeout retry policy",
        "как настроить журналы",
        "database migration lock",
        "сертификат и ротация",
        "incident escalation",
    ];
    let mut ms = Vec::new();
    for round in 0..3 {
        for (k, nid) in ids.iter().enumerate() {
            let q = queries[(k + round) % queries.len()];
            let t = std::time::Instant::now();
            let leaves = zam_store::focus_leaves(
                &d,
                TargetRef {
                    kind: TargetKind::Node,
                    id: *nid,
                },
                zam_store::query_needles(q),
            )
            .await
            .unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(!leaves.is_empty());
        }
    }
    ms.sort_by(f64::total_cmp);
    let p = |x: f64| ms[((ms.len() as f64 - 1.0) * x).round() as usize];
    eprintln!(
        "T7B-METRIC focus_fts eval/corpus ({} заметок, {} вызовов, HDD): p50 {:.2} мс, p95 {:.2} мс, max {:.2} мс",
        ids.len(),
        ms.len(),
        p(0.5),
        p(0.95),
        ms[ms.len() - 1]
    );
}
