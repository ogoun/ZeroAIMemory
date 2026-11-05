//! Оценка токенов без модели для бюджетов выдачи (M6, задача 5, решение контролёра B):
//! [`zam_markdown::UpperCounter`] не занижает счёт токенизатора bge-m3 на корпусе eval —
//! на заметках целиком и на карточках поиска с полными id (они дороже всего: 26 символов
//! Crockford base32 — около 19 токенов). Прежняя оценка `ceil(символы / 3,5)` на выдаче
//! сервера занижала ~15 % (журнал M6, задачи 4 и 5).

mod common;

use std::path::Path;

use common::*;
use zam_markdown::{ApproxCounter, TokenCounter, UpperCounter};

/// Токенизатор bge-m3 (только `tokenizer.json`, без ORT).
const BGE: &str = "bge-m3-int8";

/// Все `*.md` каталога (рекурсивно).
fn md_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            md_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            out.push(std::fs::read_to_string(&p).unwrap());
        }
    }
}

/// Псевдослучайный id (UUIDv7 в Crockford base32, 26 символов), детерминированно по `seed`.
fn fake_id(seed: u64) -> String {
    const B32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut s = String::from("01M");
    for _ in 0..23 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push(B32[(x % 32) as usize] as char);
    }
    s
}

#[test]
fn upper_estimate_not_below_bge_on_eval_corpus() {
    let path = models_dir().join(BGE).join("tokenizer.json");
    if !path.is_file() {
        assert!(
            std::env::var_os("ZAM_REQUIRE_ML").is_none(),
            "нет {}: cargo xtask fetch-models --id {BGE}",
            path.display()
        );
        eprintln!("пропуск: нет токенизатора {BGE}");
        return;
    }
    let bge = zam_ml::BgeTokenCounter::load(&models_dir(), BGE).unwrap();
    let mut docs = Vec::new();
    md_files(&repo_root().join("eval").join("corpus"), &mut docs);
    md_files(&repo_root().join("eval").join("sessions-corpus"), &mut docs);
    assert!(docs.len() >= 100, "корпус eval: {} заметок", docs.len());
    // карточки поиска v2: полный id, путь, секция, хвост и фрагмент заметки
    let cards: Vec<String> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let title: String = d.lines().next().unwrap_or("").chars().take(60).collect();
            let snippet: String = d.chars().skip(100).take(240).collect();
            format!(
                "{}. n:{} Knowledge/ru/x/{} › Причина  [.82] r7 · 2026-10-10 14:03:22 · 340t\n   {}\n\
                 {}. i:{} insight/gotcha «{title}»  [.77] hyp · 2026-09-28 08:00:01 · 120t\n",
                2 * i + 1,
                fake_id(i as u64),
                title.replace(' ', "-"),
                snippet.replace('\n', " "),
                2 * i + 2,
                fake_id(1000 + i as u64),
            )
        })
        .collect();
    let (mut exact, mut upper, mut approx) = (0usize, 0usize, 0usize);
    for (kind, set) in [("заметка", &docs), ("карточка", &cards)] {
        for t in set.iter() {
            let (e, u) = (bge.count(t), UpperCounter.count(t));
            assert!(
                u >= e,
                "{kind}: оценка {u} < bge-m3 {e}: «{}»",
                t.chars().take(120).collect::<String>()
            );
            exact += e;
            upper += u;
            approx += ApproxCounter.count(t);
        }
    }
    let over = upper as f64 / exact as f64;
    eprintln!(
        "bge-m3 {exact}, оценка сверху {upper} (×{over:.3}), прежняя оценка {approx} (×{:.3})",
        approx as f64 / exact as f64
    );
    // запас умеренный: бюджет в режиме без модели не должен сжиматься вдвое
    assert!(over < 1.45, "завышение ×{over:.3}");
    assert!(!UpperCounter.is_exact());
}

/// Оценка сверху на краях: пусто, пробелы, знаки, id, кириллица.
#[test]
fn upper_estimate_edges() {
    let c = UpperCounter;
    assert_eq!(c.count(""), 0);
    assert_eq!(c.count("   \n\t"), 0);
    assert!(
        c.count("01M6NYKVK8E7DVZVME2RR071DW") >= 24,
        "id 26 символов"
    );
    assert!(c.count("Привет") >= 1 && c.count("…—«»") >= 4);
    // монотонность на префиксах (её ждёт усечение по токенам бинарным поиском)
    let s = "n:01M6NYKVK8E7DVZVME2RR071DW Скрипт с CRLF падает в bash: $'\\r'";
    let ends: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
    let mut prev = 0;
    for &i in &ends {
        let n = c.count(&s[..i]);
        assert!(n >= prev, "немонотонно на {i}");
        prev = n;
    }
}
