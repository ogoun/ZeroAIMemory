//! Оценка токенов без модели для бюджетов выдачи (M6, задача 5, решение контролёра B; правки
//! ревью I-2): [`zam_markdown::UpperCounter`] не занижает счёт токенизатора bge-m3 на корпусе:
//! заметки eval целиком, карточки поиска с полными id (26 символов Crockford base32 — около 19
//! токенов), идентификаторы и пути исходников проекта, строки карты кода, синтетика C#/Java/TS,
//! CJK, корейский, греческий и другие письменности, смешанные строки. Прежняя оценка
//! `ceil(символы / 3,5)` на выдаче сервера занижала ~15 % (журнал M6, задачи 4 и 5).

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

/// Генератор xorshift (детерминированные выборки корпуса).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[(self.next() % v.len() as u64) as usize]
    }
}

/// Файлы с расширениями `exts` под `dir` (рекурсивно, без `target`/`node_modules`).
fn code_files(dir: &Path, exts: &[&str], out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !matches!(name, "target" | "node_modules" | "dist" | ".git") {
                code_files(&p, exts, out);
            }
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(p);
        }
    }
}

/// Идентификаторы (`[A-Za-z_][A-Za-z0-9_]{3,}`) и пути исходников проекта, отсортированы.
fn code_corpus() -> (Vec<String>, Vec<String>) {
    let root = repo_root();
    let mut files = Vec::new();
    code_files(&root.join("crates"), &["rs"], &mut files);
    code_files(&root.join("web").join("src"), &["ts", "svelte"], &mut files);
    let mut idents = std::collections::BTreeSet::new();
    let mut paths = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f);
        paths.push(rel.to_string_lossy().replace('\\', "/"));
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        for w in src.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if w.len() >= 4 && !w.starts_with(|c: char| c.is_ascii_digit()) {
                idents.insert(w.to_string());
            }
        }
    }
    (idents.into_iter().collect(), paths)
}

/// Тексты письменностей кроме латиницы и кириллицы: фразы и случайные строки диапазонов.
fn scripts_corpus() -> Vec<String> {
    let mut out: Vec<String> = [
        "数据库迁移在部署时失败，因为锁表时间过长。",
        "缓存失效策略需要考虑并发写入和读取的情况。",
        "データベースの移行がデプロイ時に失敗しました。ロックの時間が長すぎます。",
        "데이터베이스 마이그레이션이 배포 중에 실패했습니다. 잠금 시간이 너무 깁니다.",
        "캐시 무효화 전략은 동시 쓰기와 읽기를 고려해야 합니다.",
        "Η μετάβαση της βάσης δεδομένων απέτυχε κατά την ανάπτυξη.",
        "Ωμέγα λάμδα σίγμα δέλτα",
        "שלום עולם, בדיקת מטמון",
        "مرحبا بالعالم اختبار التخزين المؤقت",
        "ภาษาไทยทดสอบการแคช",
        "हिन्दी में परीक्षण कैश",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    out.push("αβγ".repeat(10));
    let mut r = Rng(0x5eed_2026_1106);
    for (base, n) in [
        (0x4E00u32, 400u32),
        (0x3040, 80),
        (0xAC00, 400),
        (0x0391, 50),
        (0x0600, 40),
    ] {
        for _ in 0..60 {
            let len = 3 + r.next() % 38;
            out.push(
                (0..len)
                    .filter_map(|_| char::from_u32(base + (r.next() % n as u64) as u32))
                    .collect(),
            );
        }
    }
    out
}

/// Синтетические идентификаторы и пути C#, Java, TypeScript.
fn synthetic_idents() -> Vec<String> {
    const PARTS: [&str; 28] = [
        "get",
        "set",
        "User",
        "Id",
        "And",
        "Tenant",
        "From",
        "Cache",
        "Http",
        "Client",
        "Factory",
        "Options",
        "Service",
        "Repository",
        "Impl",
        "Async",
        "Handler",
        "XML",
        "JSON",
        "URL",
        "Dto",
        "Map",
        "List",
        "Count",
        "V2",
        "Ex",
        "Base",
        "Abstract",
    ];
    let mut out: Vec<String> = [
        "getUserByIdAndTenantFromCache",
        "HttpClientFactoryOptions",
        "IServiceCollectionExtensions",
        "System.Collections.Generic.Dictionary<string, List<int>>",
        "com.example.app.UserRepositoryImpl",
        "org.springframework.boot.autoconfigure.SpringBootApplication",
        "useQueryClient",
        "React.FC<Props>",
        "src/components/UserProfile/UserProfileCard.tsx",
        "XMLHttpRequest",
        "parseHTTPResponse2xx",
        "getHTTPSURLv2Ex",
        "MAX_RETRY_COUNT_PER_SECOND",
        "snake_case_name_with_numbers_123",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut r = Rng(0xc0de_2026_1106);
    for _ in 0..1500 {
        let n = 2 + (r.next() % 5) as usize;
        let parts: Vec<&str> = (0..n).map(|_| *r.pick(&PARTS)).collect();
        let name = parts.concat();
        let ext = *r.pick(&[".ts", ".cs", ".java", ".tsx"]);
        let dir: Vec<String> = parts.iter().take(3).map(|p| p.to_lowercase()).collect();
        out.push(format!("src/{}/{name}{ext}", dir.join("/")));
        out.push(name);
    }
    out
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
    let (idents, paths) = code_corpus();
    assert!(idents.len() > 1000, "идентификаторов {}", idents.len());
    let mut r = Rng(0x11_2026);
    // строки карты кода: путь, символы, ссылки
    let map_lines: Vec<String> = paths
        .iter()
        .map(|p| {
            format!(
                "{p}: fn {} · {} — {} ссылок",
                r.pick(&idents),
                r.pick(&idents),
                r.next() % 900
            )
        })
        .collect();
    let mut mixed: Vec<String> = [
        "Скрипт с CRLF падает",
        "Скрипт с CRLF падает в bash",
        "Настрой git config core.autocrlf",
        "используй HttpClient вместо WebRequest",
        "ошибка E0502 в borrowChecker",
        "pg_dump --format=custom быстрее",
        "файл src/api/search/mod.rs › pack_items",
        "ключ ZAM_SEARCH__BUDGET_TOKENS=800",
        "Сервер 0.0.11-m6 на Windows",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let syn = synthetic_idents();
    for i in 0..400 {
        let words: Vec<&str> = docs[i % docs.len()].split_whitespace().take(40).collect();
        mixed.push(format!(
            "{} {} {}",
            r.pick(&words),
            r.pick(&syn),
            r.pick(&idents)
        ));
    }
    let groups: [(&str, Vec<String>); 8] = [
        ("заметка", docs),
        ("карточка", cards),
        ("идентификатор", idents),
        ("путь", paths),
        ("строка карты кода", map_lines),
        ("C#/Java/TS", syn),
        ("письменности", scripts_corpus()),
        ("смешанная", mixed),
    ];
    let mut over_prose = 0.0;
    for (kind, set) in &groups {
        let (mut exact, mut upper, mut approx) = (0usize, 0usize, 0usize);
        for t in set {
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
        let over = upper as f64 / exact as f64;
        eprintln!(
            "{kind}: {} текстов, bge-m3 {exact}, оценка сверху {upper} (×{over:.3}), прежняя {approx} (×{:.3})",
            set.len(),
            approx as f64 / exact as f64
        );
        if *kind == "заметка" {
            over_prose = over;
        }
    }
    // запас ограничен: без модели бюджет прозы не должен сжиматься вдвое
    assert!(over_prose < 1.7, "завышение прозы ×{over_prose:.3}");
    assert!(!UpperCounter.is_exact());
}

/// Оценка сверху на краях: пусто, пробелы, знаки, id, письменности, примеры ревью задачи 5.
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
    // CamelCase (bge-m3 — 12), греческий (21), кириллица с латиницей (8): не ниже
    assert!(c.count("getUserByIdAndTenantFromCache") >= 12);
    assert!(
        c.count(&"αβγ".repeat(10)) >= 30,
        "не меньше токена на символ"
    );
    assert!(c.count("Скрипт с CRLF падает") >= 8);
    // монотонность на префиксах (её ждёт усечение по токенам бинарным поиском)
    for s in [
        "n:01M6NYKVK8E7DVZVME2RR071DW Скрипт с CRLF падает в bash: $'\\r'",
        "getHTTPSURLv2Ex src/api/search/mod.rs#Packed::item snake_case_x 数据库 αβγ",
    ] {
        let ends: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        let mut prev = 0;
        for &i in &ends {
            let n = c.count(&s[..i]);
            assert!(n >= prev, "немонотонно на {i}: «{}»", &s[..i]);
            prev = n;
        }
    }
}
