//! Полнотекстовый поиск FTS5: санитизация запроса, каналы bm25 и trigram, RRF,
//! фильтры области, лимит чанков на узел, сниппеты.

use std::path::PathBuf;
use std::time::Instant;

use zam_core::Id;
use zam_store::search::{FtsQuery, MARK_END, MARK_START, sanitize_fts, search_fts};
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
        Some(r#""NEAR"* "a"* "b"*"#)
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
    // идентификатор внутри слова: unicode61 даёт токен «gitcore», не «core»
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

// ---------- нагрузка ----------

/// 1000 заметок по 3 секции.
async fn seed_1000(name: &str) -> (Db, Id) {
    let (db, k, _) = setup(name).await;
    for i in 0..1000 {
        let body = format!(
            "# Установка {i}\n\nПакет pkg{i} ставится скриптом install.sh, версия 1.{i}.\n\n\
             # Настройка\n\nПараметр core.autocrlf и файл config{i}.toml для узла {i}.\n\n\
             # Заметки\n\nПрочие сведения о сборке номер {i} и проверке.\n"
        );
        add(&db, k, &format!("Раздел{}/заметка {i}", i % 10), &body).await;
    }
    (db, k)
}

const BENCH_QUERIES: &[&str] = &[
    "установка пакет",
    "core.autocrlf",
    "config42.toml",
    "сборке проверке",
    "скрипт install",
    "несуществующее слово",
];

#[tokio::test]
async fn search_over_1000_notes_completes() {
    let (db, k) = seed_1000("load").await;
    for s in BENCH_QUERIES {
        let hits = search_fts(&db, q(s, &[k])).await.unwrap();
        assert!(hits.len() <= 20);
    }
    let hits = search_fts(&db, q("config42.toml", &[k])).await.unwrap();
    assert_eq!(hits[0].path, "Раздел2/заметка 42");
}

/// Замер p95: `cargo test -p zam-store --release --test search -- --ignored bench_fts_1000_notes --nocapture`.
#[tokio::test]
#[ignore]
async fn bench_fts_1000_notes() {
    let (db, k) = seed_1000("bench").await;
    let mut times = Vec::new();
    for _ in 0..50 {
        for s in BENCH_QUERIES {
            let t = Instant::now();
            search_fts(&db, q(s, &[k])).await.unwrap();
            times.push(t.elapsed());
        }
    }
    times.sort();
    let p = |x: f64| times[((times.len() as f64 * x) as usize).min(times.len() - 1)];
    println!(
        "FTS 1000×3: n={} p50={:?} p95={:?} max={:?}",
        times.len(),
        p(0.50),
        p(0.95),
        times[times.len() - 1]
    );
}
