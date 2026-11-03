//! Упаковка карты кода в бюджет токенов (спецификация M5.6 §7).

use proptest::prelude::*;
use zam_code::{MapFile, MapSym, SymbolKind, map_line, render_map};

/// Формула `ApproxCounter` сервера (`zam-markdown`): ⌈символы / 3,5⌉.
fn approx(text: &str) -> usize {
    (text.chars().count() * 2).div_ceil(7)
}

fn half_chars(text: &str) -> usize {
    text.chars().count().div_ceil(2)
}

fn lines(text: &str) -> usize {
    text.lines().count()
}

fn sym(kind: SymbolKind, qual: &str, sig: Option<&str>, line: u32) -> MapSym {
    MapSym {
        kind,
        qual: qual.to_string(),
        sig: sig.map(str::to_string),
        line,
    }
}

fn big_map(files: usize, syms: usize) -> Vec<MapFile> {
    (0..files)
        .map(|f| MapFile {
            path: format!("crates/module_{f}/src/файл_{f}.rs"),
            syms: (0..syms)
                .map(|s| {
                    let sig = format!(
                        "pub fn function_{f}_{s}(arg: &str, ctx: &mut Context) -> Result<()>"
                    );
                    sym(
                        SymbolKind::Function,
                        &format!("Type{f}::function_{s}"),
                        (s % 3 != 0).then_some(sig.as_str()),
                        (s * 10 + 1) as u32,
                    )
                })
                .collect(),
        })
        .collect()
}

/// Счётчик токенов, как его принимает `render_map`.
type Counter = dyn Fn(&str) -> usize;

const HEADER: &str = "# Карта кода: main @ 3070f72, 50 файлов, 500 символов";

#[test]
fn render_map_never_over_budget() {
    let map = big_map(50, 10);
    let counters: [(&str, &Counter); 2] = [("approx", &approx), ("half", &half_chars)];
    for (name, count) in counters {
        let mut prev_shown = 0;
        for budget in [1, 10, 200, 500, 1000, 2000] {
            let p = render_map(HEADER, &map, budget, count);
            assert!(p.tokens <= budget, "{name} {budget}: {}", p.tokens);
            assert_eq!(p.tokens, count(&p.text), "{name} {budget}");
            assert_eq!(p.symbols_shown + p.symbols_left, 500, "{name} {budget}");
            assert_eq!(p.files_shown + p.files_left, 50, "{name} {budget}");
            assert!(
                p.symbols_shown >= prev_shown,
                "{name} {budget}: монотонность"
            );
            prev_shown = p.symbols_shown;
            if budget >= 200 {
                assert!(p.text.starts_with(HEADER), "{name} {budget}");
                assert!(p.symbols_shown > 0, "{name} {budget}");
                // запас меньше одной строки символа — иначе бинарный поиск недобрал
                let line_cost = count(&map_line(&map[0].syms[1])) + 2;
                assert!(
                    budget - p.tokens <= line_cost * 2,
                    "{name} {budget}: {}",
                    p.tokens
                );
            }
        }
    }
}

#[test]
fn render_map_tail_line_counts() {
    let map: Vec<MapFile> = ["a.rs", "b.rs", "c.rs"]
        .iter()
        .map(|p| MapFile {
            path: p.to_string(),
            syms: vec![
                sym(SymbolKind::Struct, "Store", None, 1),
                sym(SymbolKind::Function, "open", None, 9),
            ],
        })
        .collect();
    // заголовок, путь, 2 символа, хвост = 5 строк
    let p = render_map("# Карта", &map, 5, &lines);
    assert_eq!(
        p.text,
        "# Карта\na.rs\n  struct Store :1\n  function open :9\n\
         … ещё 2 файла, 4 символа — `memory_code action=map focus=…`"
    );
    assert_eq!(
        (p.files_shown, p.symbols_shown, p.files_left, p.symbols_left),
        (1, 2, 2, 4)
    );
    assert_eq!(p.tokens, 5);
    // файл показан частично: остаток его символов — в хвосте
    let p = render_map("# Карта", &map, 7, &lines);
    assert_eq!(
        (p.files_shown, p.symbols_shown, p.files_left, p.symbols_left),
        (2, 3, 1, 3)
    );
    assert!(
        p.text
            .ends_with("… ещё 1 файл, 3 символа — `memory_code action=map focus=…`")
    );
    // всё влезло — хвоста нет
    let p = render_map("# Карта", &map, 100, &lines);
    assert_eq!(
        (p.files_shown, p.symbols_shown, p.files_left, p.symbols_left),
        (3, 6, 0, 0)
    );
    assert!(!p.text.contains("ещё"));
    assert_eq!(p.tokens, 10);
}

#[test]
fn render_map_tail_plural_forms() {
    let one = |n: usize| MapFile {
        path: format!("f{n}.rs"),
        syms: vec![sym(SymbolKind::Function, "f", None, 1)],
    };
    let map: Vec<MapFile> = (0..12).map(one).collect();
    let p = render_map("h", &map, 2, &lines);
    assert!(
        p.text
            .ends_with("… ещё 12 файлов, 12 символов — `memory_code action=map focus=…`"),
        "{}",
        p.text
    );
    let p = render_map("h", &map[..6], 2, &lines);
    assert!(p.text.contains("ещё 6 файлов, 6 символов"), "{}", p.text);
    let p = render_map("h", &map[..2], 4, &lines);
    assert!(p.text.contains("ещё 1 файл, 1 символ —"), "{}", p.text);
}

#[test]
fn render_map_sig_or_qual() {
    let with_sig = sym(
        SymbolKind::Function,
        "Store::open",
        Some("pub fn open(path: &Path) -> Store"),
        12,
    );
    assert_eq!(
        map_line(&with_sig),
        "  function pub fn open(path: &Path) -> Store :12"
    );
    let no_sig = sym(SymbolKind::Struct, "Store", None, 3);
    assert_eq!(map_line(&no_sig), "  struct Store :3");
    let map = [MapFile {
        path: "src/store.rs".into(),
        syms: vec![no_sig, with_sig],
    }];
    let p = render_map("H", &map, 1000, &approx);
    assert_eq!(
        p.text,
        "H\nsrc/store.rs\n  struct Store :3\n  function pub fn open(path: &Path) -> Store :12"
    );
}

#[test]
fn render_map_empty_files_header_only() {
    let p = render_map(HEADER, &[], 1000, &approx);
    assert_eq!(p.text, HEADER);
    assert_eq!(
        (p.files_shown, p.symbols_shown, p.files_left, p.symbols_left),
        (0, 0, 0, 0)
    );
    assert_eq!(p.tokens, approx(HEADER));
    // файл без символов — только путь
    let map = [MapFile {
        path: "README.rs".into(),
        syms: vec![],
    }];
    let p = render_map("H", &map, 1000, &approx);
    assert_eq!(p.text, "H\nREADME.rs");
    assert_eq!((p.files_shown, p.files_left), (1, 0));
}

#[test]
fn render_map_long_header_truncated() {
    let header = "Ж".repeat(100);
    let map = big_map(3, 3);
    let p = render_map(&header, &map, 10, &half_chars);
    assert!(p.tokens <= 10);
    assert!(header.starts_with(p.text.lines().next().unwrap()));
    assert_eq!(p.symbols_shown, 0);
    let p = render_map(&header, &map, 1, &half_chars);
    assert!(p.tokens <= 1 && !p.text.is_empty(), "{:?}", p.text);
}

fn arb_map() -> impl Strategy<Value = Vec<MapFile>> {
    let arb_sym = (
        0..SymbolKind::ALL.len(),
        "[a-zA-Zа-я_:]{1,40}",
        proptest::option::of("[ -~а-яё]{0,160}"),
        0u32..100_000,
    )
        .prop_map(|(k, qual, sig, line)| MapSym {
            kind: SymbolKind::ALL[k],
            qual,
            sig,
            line,
        });
    let arb_file = (
        "[a-z/._А-Я]{1,60}",
        proptest::collection::vec(arb_sym, 0..20),
    )
        .prop_map(|(path, syms)| MapFile { path, syms });
    proptest::collection::vec(arb_file, 0..30)
}

proptest! {
    #[test]
    fn render_map_budget_property(
        map in arb_map(),
        header in "[ -~а-я]{0,300}",
        budget in 1usize..20_000,
        half in any::<bool>(),
    ) {
        let count: &dyn Fn(&str) -> usize = if half { &half_chars } else { &approx };
        let p = render_map(&header, &map, budget, count);
        prop_assert!(p.tokens <= budget);
        prop_assert_eq!(p.tokens, count(&p.text));
        let total: usize = map.iter().map(|f| f.syms.len()).sum();
        prop_assert_eq!(p.symbols_shown + p.symbols_left, total);
        prop_assert_eq!(p.files_shown + p.files_left, map.len());
    }
}

#[test]
fn render_map_counts_only_texts_near_answer() {
    // точный счётчик сервера дорог: полная карта 2000 × 20 счётчику не показывается
    let map = big_map(2000, 20);
    let longest = std::cell::Cell::new(0usize);
    let count = |t: &str| {
        longest.set(longest.get().max(t.chars().count()));
        approx(t)
    };
    let p = render_map(HEADER, &map, 500, &count);
    assert!(p.tokens <= 500);
    let shown = p.text.chars().count();
    assert!(
        longest.get() <= shown * 3,
        "длиннейший {} при ответе {shown}",
        longest.get()
    );
}
