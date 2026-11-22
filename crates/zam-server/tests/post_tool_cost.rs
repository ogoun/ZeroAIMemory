//! Цена подсказки к ошибке `/recall(post_tool)` на большом решении (M7.1 §4.6 п. 3, ревью
//! задачи 8, M-3): частота слов ошибки среди insight считается на каждый запрос, поэтому замер —
//! на синтетическом корпусе ≈ 2 000 insight и ≈ 10 000 прочих чанков (1 000 заметок по 10
//! разделов). Ошибки — все случаи `post_tool` набора `eval/hints.toml`, три прохода после
//! прогревочного; срок сбора — `recall.timeout_ms` по умолчанию (200 мс). Это замер, а не
//! проверка (числа — в журнал этапа), поэтому `#[ignore]` и release:
//! `cargo test --release -p zam-server --test post_tool_cost -- --ignored --nocapture`.
mod common;

use std::time::Instant;

use axum::http::{Method, StatusCode};
use common::api::api;
use serde_json::{Value, json};

/// Insight и заметок корпуса, разделов в заметке.
const INSIGHTS: usize = 2_000;
const NOTES: usize = 1_000;
const SECTIONS: usize = 10;
/// Проходов замера по всем ошибкам набора (после прогревочного).
const PASSES: usize = 3;

/// Слова корпуса: первые — частые слова вывода и разработки (их берут чаще всех), дальше — редкие.
const WORDS: &[&str] = &[
    "test",
    "build",
    "error",
    "file",
    "cannot",
    "the",
    "cargo",
    "server",
    "config",
    "run",
    "web",
    "index",
    "code",
    "found",
    "failed",
    "path",
    "open",
    "access",
    "directory",
    "target",
    "тест",
    "сборка",
    "ошибка",
    "файл",
    "сервер",
    "запуск",
    "каталог",
    "индекс",
    "память",
    "поиск",
    "timeout",
    "network",
    "proxy",
    "certificate",
    "database",
    "locked",
    "port",
    "linker",
    "module",
    "python",
    "encoding",
    "utf",
    "windows",
    "linux",
    "docker",
    "qdrant",
    "onnxruntime",
    "cuda",
    "кэш",
    "ключ",
    "токен",
    "решение",
    "запрос",
    "журнал",
    "хук",
    "подсказка",
    "сессия",
    "снимок",
    "migration",
    "schema",
    "sqlite",
    "busy",
    "writer",
    "reader",
    "checkpoint",
    "wal",
    "vacuum",
    "frontend",
    "vite",
    "playwright",
    "node",
    "npm",
    "package",
    "json",
    "yaml",
    "toml",
    "lock",
    "git",
    "branch",
    "commit",
    "rebase",
    "merge",
    "push",
    "remote",
    "origin",
    "hook",
    "worktree",
];

/// Детерминированный генератор (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Слово: с вероятностью ~1/2 — из первых 20 частых, иначе — любое; изредка — редкое
    /// слово-код корпуса.
    fn word(&mut self) -> String {
        let r = self.next();
        match r % 16 {
            0..=7 => WORDS[(r >> 8) as usize % 20].to_string(),
            8..=14 => WORDS[(r >> 8) as usize % WORDS.len()].to_string(),
            _ => format!("w{}", (r >> 8) % 50_000),
        }
    }

    fn text(&mut self, n: usize) -> String {
        (0..n).map(|_| self.word()).collect::<Vec<_>>().join(" ")
    }
}

fn quantile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() as f64 * q).ceil() as usize).clamp(1, v.len()) - 1]
}

/// Ошибки случаев `post_tool` набора подсказок.
fn errors() -> Vec<String> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/hints.toml");
    let v: toml::Value = toml::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v["case"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["event"].as_str() == Some("post_tool"))
        .map(|c| c["input"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[ignore = "замер на большом корпусе: release, --ignored --nocapture"]
async fn post_tool_cost_large_solution() {
    let a = api("post-tool-cost", |_| {}).await;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let t0 = Instant::now();
    for i in 0..INSIGHTS {
        let kind = if i % 2 == 0 { "gotcha" } else { "failure" };
        let body = json!({"kind": "insight", "title": format!("Запись {i}: {}", rng.text(6)),
                          "body": rng.text(45), "data": {"kind": kind}});
        let r = a
            .tok(
                &a.token,
                Method::POST,
                "/api/v1/spaces/knowledge/entries",
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    for i in 0..NOTES {
        let mut content = format!("# Заметка {i}\n\n");
        for s in 0..SECTIONS {
            content.push_str(&format!("## Раздел {s}\n\n{}\n\n", rng.text(90)));
        }
        let r = a
            .tok(
                &a.token,
                Method::POST,
                "/api/v1/nodes",
                Some(json!({"space": "knowledge", "path": format!("notes/n{i}"),
                            "kind": "note", "content": content})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let load = t0.elapsed();
    // состав корпуса — по базе пользователя
    let users = a.st.cfg.data_dir.join("users");
    let db = std::fs::read_dir(&users)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "db"))
        .unwrap();
    let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
    let insight_chunks = count(
        "SELECT count(*) FROM chunks WHERE entry_id IS NOT NULL AND level IN ('chunk', 'note')",
    );
    let other_chunks = count(
        "SELECT count(*) FROM chunks WHERE node_id IS NOT NULL AND level IN ('chunk', 'note')",
    );
    drop(c);
    let errors = errors();
    let mut client = Vec::new();
    let mut server = Vec::new();
    let mut timeouts = 0usize;
    let mut shown = 0usize;
    for pass in 0..=PASSES {
        for e in &errors {
            let t = Instant::now();
            let r = a
                .tok(
                    &a.token,
                    Method::POST,
                    "/api/v1/recall",
                    Some(json!({"event": "post_tool", "tool": "Bash", "error": e})),
                )
                .await;
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            if pass == 0 {
                continue;
            }
            let v: Value = r.json();
            client.push(ms);
            server.extend(v["timings"]["total_ms"].as_f64());
            timeouts += usize::from(
                v["warnings"]
                    .as_array()
                    .is_some_and(|w| w.iter().any(|x| x == "recall timeout")),
            );
            shown += v["items"].as_array().map_or(0, Vec::len);
        }
    }
    let n = client.len();
    eprintln!(
        "ZAM_COST post_tool: insight {INSIGHTS} (чанков {insight_chunks}), прочих чанков \
         {other_chunks}, загрузка {:.0} с; запросов {n} ({} ошибок × {PASSES}); клиент p50 {:.1} \
         мс, p95 {:.1} мс; сервер p50 {:.1} мс, p95 {:.1} мс; `recall timeout` {timeouts}; \
         подсказок {shown}",
        load.as_secs_f64(),
        errors.len(),
        quantile(&mut client.clone(), 0.5),
        quantile(&mut client, 0.95),
        quantile(&mut server.clone(), 0.5),
        quantile(&mut server, 0.95),
    );
}
