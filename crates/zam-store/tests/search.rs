//! Полнотекстовый поиск FTS5: санитизация запроса, каналы bm25 и trigram, RRF,
//! фильтры области, лимит чанков на узел, сниппеты.

use std::path::PathBuf;
use std::time::Instant;

use zam_core::Id;
use zam_store::search::{FtsQuery, MARK_END, MARK_START, sanitize_fts, search_fts};
use zam_store::time::fmt_ts;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SpaceKind, StoreError, UserDbs,
    create_note, delete_node, list_spaces,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("search-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя, id пространств Knowledge и Human.
async fn setup(name: &str) -> (Db, Id, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().id;
    let (k, h) = (find(SpaceKind::Knowledge), find(SpaceKind::Human));
    (db, k, h)
}

fn attrib() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Human,
        token_id: None,
        session_id: None,
        machine: None,
        message: None,
    }
}

async fn add(db: &Db, space: Id, path: &str, content: &str) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: content.into(),
            l0: None,
            attrib: attrib(),
        },
    )
    .await
    .unwrap()
    .id
}

fn q(text: &str, spaces: &[Id]) -> FtsQuery {
    FtsQuery {
        text: text.into(),
        space_ids: spaces.to_vec(),
        path_prefix: None,
        top_k: 20,
    }
}

// ---------- sanitize_fts ----------

#[test]
fn sanitize_builds_quoted_prefix_terms() {
    assert_eq!(
        sanitize_fts("Скрипт bash").as_deref(),
        Some(r#""Скрипт"* "bash"*"#)
    );
    assert_eq!(
        sanitize_fts("core.autocrlf").as_deref(),
        Some(r#""core"* "autocrlf"*"#)
    );
    assert_eq!(sanitize_fts("AND").as_deref(), Some(r#""AND"*"#));
    assert_eq!(
        sanitize_fts("NEAR(a b)").as_deref(),
        Some(r#""NEAR"* "a" "b""#)
    );
    // слова короче 3 символов — точные термы: префикс «с»* раскрывается в тысячи термов
    assert_eq!(
        sanitize_fts("сборка с проверкой").as_deref(),
        Some(r#""сборка"* "с" "проверкой"*"#)
    );
    for s in ["", "   ", "\"", "*", "(", "-:", "'; --"] {
        assert_eq!(sanitize_fts(s), None, "запрос {s:?}");
    }
}

// ---------- поиск ----------

#[tokio::test]
async fn russian_word_found_by_prefix_and_case() {
    let (db, k, _) = setup("ru").await;
    let id = add(
        &db,
        k,
        "Инструменты/Bash скрипты",
        "# Обзор\n\nПолезные скрипты для bash.\n",
    )
    .await;
    let hits = search_fts(&db, q("Скрипт", &[k])).await.unwrap();
    assert_eq!(hits.len(), 1);
    let h = &hits[0];
    assert_eq!(h.node_id, id);
    assert_eq!(h.space_id, k);
    assert_eq!(h.path, "Инструменты/Bash скрипты");
    assert_eq!(h.heading_path, "Обзор");
    assert_eq!(h.rev, 1);
    assert!(h.score > 0.0);
    assert!(h.text.contains("Полезные скрипты"));
    assert!(
        h.snippet
            .contains(&format!("{MARK_START}скрипты{MARK_END}")),
        "сниппет: {:?}",
        h.snippet
    );
}

#[tokio::test]
async fn uppercase_query_finds_lowercase() {
    let (db, k, _) = setup("crlf").await;
    add(&db, k, "git", "# Переводы строк\n\nНастройка crlf в git.\n").await;
    let hits = search_fts(&db, q("CRLF", &[k])).await.unwrap();
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn identifier_found_via_trigram() {
    let (db, k, _) = setup("tri").await;
    // unicode61 режет «mygit.core.autocrlf» на «mygit», «core», «autocrlf»:
    // слово «git» в нём не найти, подстроку — только через trigram
    add(
        &db,
        k,
        "git/config",
        "# Конфиг\n\nКлюч mygit.core.autocrlf=true задаёт переводы.\n",
    )
    .await;
    add(
        &db,
        k,
        "other",
        "# Прочее\n\nЗдесь про core и autocrlf ничего общего.\n",
    )
    .await;
    let hits = search_fts(&db, q("git.core.autocrlf", &[k])).await.unwrap();
    assert!(!hits.is_empty());
    // RRF: совпадение в обоих каналах выше совпадения только в bm25 (OR)
    assert_eq!(hits[0].path, "git/config");
    assert!(hits[0].snippet.contains(MARK_START));

    // и обычный идентификатор находится, первым — точное совпадение
    let hits = search_fts(&db, q("core.autocrlf", &[k])).await.unwrap();
    assert_eq!(hits[0].path, "git/config");

    // подстрока внутри слова — только trigram; сниппет строится из текста чанка
    let hits = search_fts(&db, q("OCRLF", &[k])).await.unwrap();
    assert_eq!(hits.len(), 2);
    for h in &hits {
        assert!(
            h.snippet.contains(&format!("{MARK_START}ocrlf{MARK_END}")),
            "сниппет: {:?}",
            h.snippet
        );
    }
}

#[tokio::test]
async fn trigram_snippet_is_char_boundary_safe() {
    let (db, k, _) = setup("trisnip").await;
    let long = "длинный текст ".repeat(30);
    add(&db, k, "n", &format!("# A\n\n{long}ключёжик42 {long}\n")).await;
    let hits = search_fts(&db, q("ЁЖИК4", &[k])).await.unwrap();
    assert_eq!(hits.len(), 1);
    let s = &hits[0].snippet;
    assert!(s.contains(&format!("{MARK_START}ёжик4{MARK_END}")), "{s:?}");
    assert!(s.starts_with('…') && s.ends_with('…'), "{s:?}");
    assert!(s.chars().count() <= 170, "{s:?}");
}

#[tokio::test]
async fn and_falls_back_to_or() {
    let (db, k, _) = setup("or").await;
    add(&db, k, "a", "# A\n\nтолько слово альфа\n").await;
    let hits = search_fts(&db, q("альфа отсутствующее", &[k]))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn fts_syntax_in_query_is_not_an_error() {
    let (db, k, _) = setup("syntax").await;
    add(
        &db,
        k,
        "n",
        "# Заголовок\n\na b x and near drop table nodes\n",
    )
    .await;
    for s in [
        "\"",
        "a*",
        "AND",
        "-x",
        "a:b",
        "(",
        "NEAR(a b)",
        "'; DROP TABLE nodes;--",
        "\"\"\"",
        "**",
        "a\"b",
    ] {
        let r = search_fts(&db, q(s, &[k])).await;
        assert!(r.is_ok(), "запрос {s:?}: {r:?}");
    }
    // таблица на месте
    assert!(!search_fts(&db, q("drop", &[k])).await.unwrap().is_empty());
}

#[tokio::test]
async fn space_filter() {
    let (db, k, h) = setup("space").await;
    add(&db, k, "a", "# A\n\nобщее слово тест\n").await;
    add(&db, h, "b", "# B\n\nобщее слово тест\n").await;
    let hits = search_fts(&db, q("тест", &[k])).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].space_id, k);
    let hits = search_fts(&db, q("тест", &[k, h])).await.unwrap();
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn path_prefix_filter() {
    let (db, k, _) = setup("prefix").await;
    add(&db, k, "Linux/net", "# A\n\nслово ключ\n").await;
    add(&db, k, "Linux/sub/deep", "# B\n\nслово ключ\n").await;
    add(&db, k, "LinuxOld/x", "# C\n\nслово ключ\n").await;
    add(&db, k, "linux/y", "# D\n\nслово ключ\n").await;
    let mut query = q("ключ", &[k]);
    query.path_prefix = Some("Linux".into());
    let mut paths: Vec<_> = search_fts(&db, query)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.path)
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["Linux/net", "Linux/sub/deep"]);

    // префикс, равный пути заметки, и завершающий `/`
    for (prefix, want) in [("Linux/net", 1), ("Linux/", 2), ("Lin", 0)] {
        let mut query = q("ключ", &[k]);
        query.path_prefix = Some(prefix.into());
        assert_eq!(
            search_fts(&db, query).await.unwrap().len(),
            want,
            "{prefix}"
        );
    }
}

#[tokio::test]
async fn deleted_node_not_found() {
    let (db, k, _) = setup("deleted").await;
    let id = add(&db, k, "gone", "# A\n\nисчезающее слово\n").await;
    assert_eq!(
        search_fts(&db, q("исчезающее", &[k])).await.unwrap().len(),
        1
    );
    delete_node(&db, id).await.unwrap();
    assert!(
        search_fts(&db, q("исчезающее", &[k]))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn at_most_three_chunks_per_node() {
    let (db, k, _) = setup("cap").await;
    let body: String = (1..=5)
        .map(|i| format!("# Раздел {i}\n\nповторяемое слово номер {i}\n\n"))
        .collect();
    add(&db, k, "many", &body).await;
    add(&db, k, "single", "# Один\n\nповторяемое слово\n").await;
    let hits = search_fts(&db, q("повторяемое", &[k])).await.unwrap();
    assert_eq!(hits.iter().filter(|h| h.path == "many").count(), 3);
    assert_eq!(hits.iter().filter(|h| h.path == "single").count(), 1);
    // упорядочено по убыванию RRF
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
}

#[tokio::test]
async fn top_k_limits_and_validates() {
    let (db, k, _) = setup("topk").await;
    for i in 0..5 {
        add(&db, k, &format!("n{i}"), "# A\n\nобщее\n").await;
    }
    let mut query = q("общее", &[k]);
    query.top_k = 2;
    assert_eq!(search_fts(&db, query).await.unwrap().len(), 2);

    for top_k in [0, 101] {
        let mut query = q("общее", &[k]);
        query.top_k = top_k;
        assert!(matches!(
            search_fts(&db, query).await,
            Err(StoreError::BadInput(_))
        ));
    }
    assert!(matches!(
        search_fts(&db, q("общее", &[])).await,
        Err(StoreError::BadInput(_))
    ));
    // запрос без слов — пустая выдача, не ошибка
    assert!(search_fts(&db, q("  ", &[k])).await.unwrap().is_empty());
}

#[tokio::test]
async fn mixed_query_uses_identifier_in_trigram() {
    let (db, k, _) = setup("mixed").await;
    // слова «настройка» здесь нет: строгий bm25 не находит, ослабленный (OR) —
    // по «mygit» и «core», trigram — по идентификатору «mygit.core»
    add(
        &db,
        k,
        "git/attrs",
        "# Ключи\n\nЗначение mygit.core.autocrlf=input.\n",
    )
    .await;
    for i in 0..3 {
        add(
            &db,
            k,
            &format!("other{i}"),
            "# Общее\n\nНастройка окружения и прочее.\n",
        )
        .await;
    }
    let hits = search_fts(&db, q("настройка mygit.core", &[k]))
        .await
        .unwrap();
    assert_eq!(hits[0].path, "git/attrs", "{hits:?}");
    assert!(hits.iter().any(|h| h.path.starts_with("other")));
    // найден обоими каналами: балл выше максимума одного канала bm25 (1/61),
    // сниппет — от bm25
    assert!(hits[0].score > 1.0 / 61.0, "{}", hits[0].score);
    assert!(hits[0].snippet.contains(MARK_START));
}

#[tokio::test]
async fn short_words_and_long_query() {
    let (db, k, _) = setup("short").await;
    add(&db, k, "n", "# A\n\nсборка с проверкой в git\n").await;
    // короткие слова ищутся точно и не ломают строгий запрос
    let hits = search_fts(&db, q("сборка с проверкой", &[k]))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    // ровно 512 символов — можно, 513 — BadInput
    let ok = "я".repeat(512);
    assert!(search_fts(&db, q(&ok, &[k])).await.is_ok());
    let long = "я".repeat(513);
    assert!(matches!(
        search_fts(&db, q(&long, &[k])).await,
        Err(StoreError::BadInput(_))
    ));
}

// ---------- нагрузка ----------

/// Генератор псевдослучайных чисел (LCG) для воспроизводимого корпуса.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

const WORDS: &[&str] = &[
    "сборка",
    "проверка",
    "установка",
    "пакет",
    "скрипт",
    "файл",
    "настройка",
    "сервер",
    "клиент",
    "журнал",
    "ошибка",
    "запрос",
    "ответ",
    "память",
    "заметка",
    "каталог",
    "ключ",
    "значение",
    "версия",
    "сеть",
    "в",
    "с",
    "и",
    "на",
    "по",
    "для",
    "не",
    "от",
    "build",
    "test",
    "deploy",
    "config",
    "cache",
    "index",
    "query",
    "git",
    "rust",
    "cargo",
    "linux",
];
const SYLLABLES: &[&str] = &[
    "ка", "ро", "с", "ти", "на", "ле", "ми", "со", "пр", "ва", "ст", "ко", "ре", "де", "по", "ло",
    "та", "ни", "ск", "ры",
];

/// Текст секции: слова словаря вперемешку с синтетическими (много разных термов,
/// в том числе на «с», — нагрузка на префиксный поиск).
fn section_text(rng: &mut Lcg, words: usize) -> String {
    let mut out = Vec::with_capacity(words);
    for _ in 0..words {
        if rng.next(3) == 0 {
            out.push(WORDS[rng.next(WORDS.len())].to_string());
        } else {
            let n = 2 + rng.next(3);
            out.push(
                (0..n)
                    .map(|_| SYLLABLES[rng.next(SYLLABLES.len())])
                    .collect(),
            );
        }
    }
    out.join(" ")
}

/// Вставляет `notes` заметок прямо в таблицы одной транзакцией (FTS заполняют
/// триггеры `chunks`); `sections(i)` — пары (заголовок, текст) заметки `i`.
async fn insert_notes(
    db: &Db,
    space: Id,
    dir: &'static str,
    notes: usize,
    mut sections: impl FnMut(usize) -> Vec<(String, String)> + Send + 'static,
) {
    db.write(move |tx| {
        let now = fmt_ts(jiff::Timestamp::now());
        let mut node = tx.prepare(
            "INSERT INTO nodes (id, space_id, kind, name, path, current_rev, index_state,
                                created_at, updated_at)
             VALUES (?1, ?2, 'note', ?3, ?4, 1, 'indexed', ?5, ?5)",
        )?;
        let mut chunk = tx.prepare(
            "INSERT INTO chunks (id, node_id, rev, level, heading_path, ordinal, text,
                                 context_prefix, token_count, text_hash)
             VALUES (?1, ?2, 1, 'section', ?3, ?4, ?5, ?6, 40, ?1)",
        )?;
        for i in 0..notes {
            let id = Id::new();
            let name = format!("заметка {i}");
            let path = format!("{dir}{}/{name}", i % 10);
            node.execute(rusqlite::params![id, space, name, path, now])?;
            for (ord, (head, text)) in sections(i).iter().enumerate() {
                chunk.execute(rusqlite::params![
                    format!("{id}-{ord}"),
                    id,
                    head,
                    ord as i64,
                    text,
                    format!("[{dir}/{path}] {name} > {head}"),
                ])?;
            }
        }
        Ok(())
    })
    .await
    .unwrap();
}

/// Корпус `notes` заметок по 3 секции в Knowledge.
async fn seed(name: &str, notes: usize) -> (Db, Id) {
    let (db, k, _) = setup(name).await;
    let mut rng = Lcg(42);
    insert_notes(&db, k, "Раздел", notes, move |i| {
        vec![
            (
                "Установка".into(),
                format!(
                    "Пакет pkg{i} ставится install.sh. {}",
                    section_text(&mut rng, 40)
                ),
            ),
            (
                "Настройка".into(),
                format!(
                    "Файл config{i}.toml, ключ mygit.core.autocrlf. {}",
                    section_text(&mut rng, 40)
                ),
            ),
            ("Заметки".into(), section_text(&mut rng, 40)),
        ]
    })
    .await;
    (db, k)
}

#[tokio::test]
async fn scope_filter_exact_when_most_matches_are_outside() {
    // 450 совпадений вне области (больше предвыборки top 50 × 8) и 2 — в ней,
    // вставлены последними: в глобальном top-400 их нет, нужен полный запрос
    let (db, k, h) = setup("oversample").await;
    let one = |text: &'static str| move |_| vec![("A".to_string(), text.to_string())];
    insert_notes(&db, h, "чужое", 450, one("редкоеслово ab_cd1")).await;
    insert_notes(&db, k, "своё", 2, one("редкоеслово ab_cd1")).await;
    for s in ["редкоеслово", "ab_cd1"] {
        let hits = search_fts(&db, q(s, &[k])).await.unwrap();
        assert_eq!(hits.len(), 2, "{s}");
        assert!(hits.iter().all(|x| x.space_id == k));
    }
    // а в области из обоих пространств — top_k
    let hits = search_fts(&db, q("редкоеслово", &[k, h])).await.unwrap();
    assert_eq!(hits.len(), 20);
}

const BENCH_QUERIES: &[&str] = &[
    "установка пакет",
    "core.autocrlf",
    "config42.toml",
    "сборка с проверкой",
    "в файле с ключом",
    "настройка mygit.core",
    "скрипт install",
    "несуществующее слово",
    "сто",
];

#[tokio::test]
async fn search_over_1000_notes_completes() {
    let (db, k) = seed("load", 1000).await;
    for s in BENCH_QUERIES {
        let hits = search_fts(&db, q(s, &[k])).await.unwrap();
        assert!(hits.len() <= 20);
    }
    let hits = search_fts(&db, q("config42.toml", &[k])).await.unwrap();
    assert_eq!(hits[0].path, "Раздел2/заметка 42");
}

async fn bench(notes: usize) {
    let (db, k) = seed(&format!("bench-{notes}"), notes).await;
    let mut all = Vec::new();
    for s in BENCH_QUERIES {
        let mut times = Vec::new();
        for _ in 0..30 {
            let t = Instant::now();
            search_fts(&db, q(s, &[k])).await.unwrap();
            times.push(t.elapsed());
        }
        times.sort();
        println!(
            "  {s:?}: p50={:?} max={:?}",
            times[times.len() / 2],
            times[times.len() - 1]
        );
        all.extend(times);
    }
    all.sort();
    let p = |x: f64| all[((all.len() as f64 * x) as usize).min(all.len() - 1)];
    println!(
        "FTS {notes}×3: n={} p50={:?} p95={:?} max={:?}",
        all.len(),
        p(0.50),
        p(0.95),
        all[all.len() - 1]
    );
}

/// Замер p95: `cargo test -p zam-store --release --test search -- --ignored bench_fts --nocapture`.
#[tokio::test]
#[ignore]
async fn bench_fts_1000_notes() {
    bench(1000).await;
}

/// То же на 30 тысячах чанков.
#[tokio::test]
#[ignore]
async fn bench_fts_10000_notes() {
    bench(10_000).await;
}
