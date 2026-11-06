//! Оценка токенов без модели для размеров и бюджетов выдачи (M6, задача 5, решение контролёра B;
//! повторное ревью I-2): [`zam_markdown::EstimateCounter`] — **несмещённая** оценка счёта
//! токенизатора bge-m3 (`Nt`, `tokens`, `~N tok`), гарантия бюджета — на ответ целиком через
//! [`TokenCounter::fits`] (`m·оценка + c ≤ бюджет`, [`ESTIMATE_M`], [`ESTIMATE_C`]).
//!
//! Слои корпуса: заметки eval целиком, карточки поиска с полными id (26 символов Crockford
//! base32), идентификаторы и пути исходников проекта, строки карты кода, синтетика C#/Java/TS,
//! CJK, корейский, греческий и другие письменности, смешанные строки. Ответы целиком —
//! фикстура `fixtures/estimate_responses.json` (отложенная половина подбора, обезличена: dev-
//! прогоны ТС-1/ТС-2, синтетика кода и письменностей; holdout не входит).

mod common;

use std::path::Path;

use common::*;
use zam_markdown::{ESTIMATE_C, ESTIMATE_M, EstimateCounter, TokenCounter};

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

/// Слова письменностей кроме латиницы и кириллицы: `(язык, без пробелов между словами, слова)`.
const SCRIPT_WORDS: [(&str, bool, &[&str]); 8] = [
    (
        "zh",
        true,
        &[
            "数据库",
            "迁移",
            "部署",
            "失败",
            "缓存",
            "策略",
            "并发",
            "写入",
            "读取",
            "服务器",
            "配置",
            "文件",
            "索引",
            "查询",
            "错误",
            "日志",
            "用户",
            "权限",
            "网络",
            "超时",
        ],
    ),
    (
        "ja",
        true,
        &[
            "データベース",
            "の",
            "移行",
            "が",
            "デプロイ",
            "時",
            "に",
            "失敗",
            "しました",
            "キャッシュ",
            "サーバー",
            "設定",
            "ファイル",
            "を",
            "確認",
            "する",
            "エラー",
            "ログ",
        ],
    ),
    (
        "ko",
        false,
        &[
            "데이터베이스",
            "마이그레이션이",
            "배포",
            "중에",
            "실패했습니다",
            "캐시",
            "무효화",
            "전략은",
            "서버",
            "설정",
            "파일을",
            "확인",
            "오류",
            "로그",
            "사용자",
            "권한",
        ],
    ),
    (
        "el",
        false,
        &[
            "Η",
            "μετάβαση",
            "της",
            "βάσης",
            "δεδομένων",
            "απέτυχε",
            "κατά",
            "την",
            "ανάπτυξη",
            "σύστημα",
            "μνήμη",
            "αρχείο",
            "ρύθμιση",
            "σφάλμα",
            "χρήστης",
            "και",
            "για",
            "διακομιστής",
            "εφαρμογή",
            "αναζήτηση",
            "ευρετήριο",
            "λειτουργία",
            "πρόσβαση",
            "δικαιώματα",
            "ενημέρωση",
            "έκδοση",
            "αποτέλεσμα",
            "όταν",
            "δεν",
            "είναι",
        ],
    ),
    (
        "he",
        false,
        &[
            "שלום",
            "עולם",
            "בדיקת",
            "מטמון",
            "מסד",
            "הנתונים",
            "נכשל",
            "במהלך",
            "ההעברה",
            "שרת",
            "קובץ",
            "הגדרות",
            "שגיאה",
            "משתמש",
        ],
    ),
    (
        "ar",
        false,
        &[
            "مرحبا",
            "بالعالم",
            "اختبار",
            "التخزين",
            "المؤقت",
            "فشل",
            "ترحيل",
            "قاعدة",
            "البيانات",
            "أثناء",
            "النشر",
            "الخادم",
            "ملف",
            "خطأ",
            "المستخدم",
        ],
    ),
    (
        "th",
        true,
        &[
            "ภาษาไทย",
            "ทดสอบ",
            "การแคช",
            "การย้าย",
            "ฐานข้อมูล",
            "ล้มเหลว",
            "ระหว่าง",
            "เซิร์ฟเวอร์",
            "ไฟล์",
            "ข้อผิดพลาด",
            "ผู้ใช้",
        ],
    ),
    (
        "hi",
        false,
        &[
            "हिन्दी",
            "में",
            "परीक्षण",
            "कैश",
            "डेटाबेस",
            "माइग्रेशन",
            "तैनाती",
            "के",
            "दौरान",
            "विफल",
            "रहा",
            "सर्वर",
            "फ़ाइल",
            "त्रुटि",
            "उपयोगकर्ता",
        ],
    ),
];

/// Тексты письменностей кроме латиницы и кириллицы: фразы и фразы из слов [`SCRIPT_WORDS`]
/// (случайные строки кодовых точек — не текст: токенизатор режет их почти по символу).
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
    let mut r = Rng(0x5eed_2026_1106);
    for (_, nospace, words) in SCRIPT_WORDS {
        for _ in 0..60 {
            let n = 1 + (r.next() % 12) as usize;
            let w: Vec<&str> = (0..n).map(|_| *r.pick(words)).collect();
            out.push(w.join(if nospace { "" } else { " " }) + ".");
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
fn estimate_unbiased_by_layer() {
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
    for (kind, set) in &groups {
        let (mut exact, mut est) = (0usize, 0usize);
        for t in set {
            exact += bge.count(t);
            est += EstimateCounter.count(t);
        }
        let r = est as f64 / exact as f64;
        eprintln!(
            "{kind}: {} текстов, bge-m3 {exact}, оценка {est} (×{r:.3})",
            set.len()
        );
        // несмещённость: сумма по слою — около bge-m3 (подбор: основные слои ×1,00–1,05,
        // вспомогательные ×0,97–1,05, фразы письменностей ×0,95–1,06; журнал задачи 5)
        let (lo, hi) = if matches!(*kind, "заметка" | "идентификатор") {
            (0.97, 1.06)
        } else {
            (0.93, 1.09)
        };
        assert!((lo..=hi).contains(&r), "{kind}: ×{r:.3} вне {lo}..{hi}");
    }
    assert!(!EstimateCounter.is_exact());
}

/// Ответ из фикстуры `fixtures/estimate_responses.json`.
#[derive(serde::Deserialize)]
struct Response {
    layer: String,
    text: String,
}

/// Гарантия на уровне ответов (ревью I-2, критерии c): для каждого ответа берётся наименьший
/// бюджет `B`, при котором [`TokenCounter::fits`] его пропускает, и сравнивается со счётом
/// bge-m3. Превышений `bge > B` — не больше 1 %, наибольшее превышение — не больше 5 %.
#[test]
fn estimate_fits_responses() {
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
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/estimate_responses.json"),
    )
    .unwrap();
    let set: Vec<Response> = serde_json::from_str(&raw).unwrap();
    assert!(set.len() >= 100, "ответов {}", set.len());
    let c = EstimateCounter;
    let (mut over, mut worst, mut under, mut n_under) = (0usize, 0.0f64, 0.0f64, 0usize);
    let mut layers = std::collections::BTreeMap::<&str, (usize, usize)>::new();
    for r in &set {
        let est = c.count(&r.text);
        let exact = bge.count(&r.text);
        let e = layers.entry(r.layer.as_str()).or_default();
        e.0 += exact;
        e.1 += est;
        // наименьший бюджет, который пропускает ответ
        let mut b = est;
        while !c.fits(&r.text, b) {
            b += 1;
        }
        assert!(b == 0 || !c.fits(&r.text, b - 1));
        if exact > b {
            over += 1;
            worst = worst.max(exact as f64 / b as f64 - 1.0);
        }
        if (400..=2000).contains(&exact) {
            under += 1.0 - exact as f64 / b as f64;
            n_under += 1;
        }
    }
    for (layer, (exact, est)) in &layers {
        eprintln!(
            "{layer}: bge-m3 {exact}, оценка {est} (×{:.3})",
            *est as f64 / *exact as f64
        );
    }
    let share = over as f64 / set.len() as f64;
    let under = if n_under > 0 {
        under / n_under as f64
    } else {
        0.0
    };
    eprintln!(
        "ответов {}, m={ESTIMATE_M}‰ c={ESTIMATE_C}: превышений {over} ({:.2} %), наибольшее {:.1} %, \
         недогрузка 400–2000 токенов {:.1} % ({n_under} ответов)",
        set.len(),
        share * 100.0,
        worst * 100.0,
        under * 100.0
    );
    assert!(share <= 0.01, "превышений {:.2} %", share * 100.0);
    assert!(
        worst <= 0.05,
        "наибольшее превышение {:.1} %",
        worst * 100.0
    );
}

/// Оценка на краях: пусто, пробелы, знаки, id, письменности, примеры ревью задачи 5 —
/// около счёта bge-m3; монотонность на префиксах.
#[test]
fn estimate_edges() {
    let c = EstimateCounter;
    assert_eq!(c.count(""), 0);
    assert_eq!(c.count("   \n\t"), 0);
    // примеры ревью (счёт bge-m3 рядом): одиночная строка — в пределах ±35 %. Повтор `αβγ`
    // (bge-m3 режет почти по символу) оценка занижает втрое: гарантия — на ответ целиком
    // (`estimate_fits_responses`), а не на строку бессмысленных символов
    let far: Vec<String> = [
        ("01M6NYKVK8E7DVZVME2RR071DW", 18usize),
        ("getUserByIdAndTenantFromCache", 12),
        ("Скрипт с CRLF падает", 8),
        ("используй HttpClient вместо WebRequest", 14),
    ]
    .iter()
    .filter_map(|&(s, exact)| {
        let n = c.count(s);
        (n * 100 < exact * 65 || n * 100 > exact * 135)
            .then(|| format!("«{s}»: оценка {n}, bge-m3 {exact}"))
    })
    .collect();
    assert!(far.is_empty(), "{far:?}");
    assert!(c.count("Привет") >= 1 && c.count("…—«»") >= 2);
    // монотонность на префиксах (её ждёт усечение по токенам бинарным поиском)
    for s in [
        "n:01M6NYKVK8E7DVZVME2RR071DW Скрипт с CRLF падает в bash: $'\\r'",
        "getHTTPSURLv2Ex src/api/search/mod.rs#Packed::item snake_case_x 数据库 αβγ",
        "public async Task<IActionResult> GetUserById(Guid id) => Ok(await _repo.FindAsync(id));",
        "Map<String, List<Integer>> m = new HashMap<>(); // Java 17",
        "const x: Record<string, number[]> = {}; export default UserProfileCard;",
        "데이터베이스 마이그레이션 データベース 数据库迁移 Ωμέγα שלום مرحبا ภาษาไทย हिन्दी",
    ] {
        let ends: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        let mut prev = 0;
        for &i in &ends {
            let n = c.count(&s[..i]);
            assert!(n >= prev, "немонотонно на {i}: «{}»", &s[..i]);
            prev = n;
        }
    }
    // fits согласован с budget_cap и монотонен по бюджету
    let t = "n:01M6NYKVK8E7DVZVME2RR071DW Knowledge/ru/x › Причина";
    let b0 = (0..200).find(|&b| c.fits(t, b)).unwrap();
    assert!((b0..b0 + 50).all(|b| c.fits(t, b)));
    assert!(b0 > c.count(t), "запас бюджета: {b0} > {}", c.count(t));
}
