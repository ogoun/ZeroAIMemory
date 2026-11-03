//! Экстрактор символов tree-sitter (фича `parse`): эталоны, `qual`, `sig`, `body_hash`, ссылки,
//! пределы и враждебные входы (спецификация M5.6 §3, §12, §13, §17 п. 1 и 7).
#![cfg(feature = "parse")]

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use zam_code::{
    Extracted, Lang, Limits, ParseState, RefKind, SkipReason, Symbol, SymbolKind, extract, sniff,
    symbol_hashes,
};

fn lim() -> Limits {
    Limits {
        parse_timeout: Duration::from_secs(5),
        ..Limits::default()
    }
}

fn run(lang: Lang, src: &str) -> Extracted {
    let out = extract(lang, src.as_bytes(), &lim());
    assert_eq!(out.state, ParseState::Ok, "{:?}", out.skip);
    out
}

fn sym<'a>(out: &'a Extracted, qual: &str) -> &'a Symbol {
    out.symbols
        .iter()
        .find(|s| s.qual == qual)
        .unwrap_or_else(|| panic!("нет {qual}: {:?}", quals(out)))
}

fn quals(out: &Extracted) -> Vec<String> {
    out.symbols
        .iter()
        .map(|s| format!("{} {}", s.kind.as_str(), s.qual))
        .collect()
}

fn has_ref(out: &Extracted, name: &str, kind: RefKind) -> bool {
    out.refs.iter().any(|r| r.name == name && r.kind == kind)
}

const FIXTURES: [(Lang, &str, &str); 8] = [
    (Lang::Rust, "rust", "sample.rs"),
    (Lang::TypeScript, "typescript", "sample.ts"),
    (Lang::Tsx, "tsx", "sample.tsx"),
    (Lang::JavaScript, "javascript", "sample.js"),
    (Lang::Python, "python", "sample.py"),
    (Lang::Go, "go", "sample.go"),
    (Lang::Java, "java", "sample.java"),
    (Lang::CSharp, "csharp", "sample.cs"),
];

#[test]
fn fixtures_precision_recall_at_least_095() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut failed = Vec::new();
    for (lang, dir, file) in FIXTURES {
        let src = std::fs::read(root.join(dir).join(file)).unwrap();
        let expected: BTreeSet<String> =
            std::fs::read_to_string(root.join(dir).join("expected.txt"))
                .unwrap()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
        assert!(expected.len() >= 40, "{dir}: эталон меньше 40 определений");
        let out = extract(lang, &src, &lim());
        assert_eq!(out.state, ParseState::Ok, "{dir}");
        let got: BTreeSet<String> = out
            .symbols
            .iter()
            .map(|s| format!("{} {} {}", s.kind.as_str(), s.qual, s.line))
            .collect();
        let tp = got.intersection(&expected).count() as f64;
        let precision = tp / got.len().max(1) as f64;
        let recall = tp / expected.len() as f64;
        println!(
            "{dir}: эталон {}, найдено {}, точность {precision:.3}, полнота {recall:.3}",
            expected.len(),
            got.len()
        );
        for fp in got.difference(&expected) {
            println!("  лишнее: {fp}");
        }
        for miss in expected.difference(&got) {
            println!("  пропущено: {miss}");
        }
        if precision < 0.95 || recall < 0.95 {
            failed.push(dir);
        }
    }
    assert!(failed.is_empty(), "ниже 0,95: {failed:?}");
}

#[test]
fn qual_chain_containers_per_lang() {
    let rs = run(
        Lang::Rust,
        "struct Store;\nimpl Store {\n    fn open() {}\n}\n",
    );
    assert_eq!(sym(&rs, "Store::open").kind, SymbolKind::Method);
    let ts = run(Lang::TypeScript, "class App {\n  start() {}\n}\n");
    assert_eq!(sym(&ts, "App::start").kind, SymbolKind::Method);
    let js = run(Lang::JavaScript, "class App {\n  start() {}\n}\n");
    assert_eq!(sym(&js, "App::start").line, 2);
    let go = run(
        Lang::Go,
        "package a\n\nfunc (s *Store) Open() error { return nil }\n",
    );
    let open = sym(&go, "Store::Open");
    assert_eq!(
        (open.name.as_str(), open.kind, open.line),
        ("Open", SymbolKind::Method, 3)
    );
    let py = run(
        Lang::Python,
        "class A:\n    class B:\n        def m(self):\n            pass\n",
    );
    assert_eq!(sym(&py, "A::B::m").kind, SymbolKind::Method);
    let java = run(
        Lang::Java,
        "class A {\n  class B {\n    void m() {}\n  }\n}\n",
    );
    assert_eq!(sym(&java, "A::B::m").kind, SymbolKind::Method);
    let cs = run(
        Lang::CSharp,
        "namespace N;\nclass A {\n  struct B {\n    void M() {}\n  }\n}\n",
    );
    assert_eq!(sym(&cs, "A::B::M").kind, SymbolKind::Method);
    assert_eq!(sym(&cs, "N").kind, SymbolKind::Module);
    let rs = run(
        Lang::Rust,
        "mod a {\n    mod b {\n        trait T {\n            fn f(&self);\n        }\n    }\n}\n",
    );
    assert_eq!(sym(&rs, "a::b::T::f").kind, SymbolKind::Method);
}

#[test]
fn duplicate_quals_get_suffix_in_file_order() {
    let src = "struct S;\nimpl S {\n    fn f() {}\n}\nimpl Tr for S {\n    fn f() {}\n}\nimpl S {\n    fn f() {}\n}\n";
    let out = run(Lang::Rust, src);
    let fs: Vec<(String, u32)> = out
        .symbols
        .iter()
        .filter(|s| s.name == "f")
        .map(|s| (s.qual.clone(), s.line))
        .collect();
    assert_eq!(
        fs,
        [
            ("S::f".to_string(), 3),
            ("S::f#2".to_string(), 6),
            ("S::f#3".to_string(), 9)
        ]
    );
}

#[test]
fn sig_and_line_from_name_line_not_decorator() {
    let py = run(
        Lang::Python,
        "class A:\n    @property\n    def size(self) -> int:\n        return 0\n",
    );
    let s = sym(&py, "A::size");
    assert_eq!((s.line, s.end_line), (3, 4));
    assert_eq!(s.sig.as_deref(), Some("def size(self) -> int:"));
    let java = run(
        Lang::Java,
        "class A {\n    @Override\n    public void run() {\n    }\n}\n",
    );
    assert_eq!(
        sym(&java, "A::run").sig.as_deref(),
        Some("public void run() {")
    );
    let rs = run(
        Lang::Rust,
        "#[derive(Debug)]\npub struct S {\n    a: u8,\n}\n",
    );
    let s = sym(&rs, "S");
    assert_eq!(
        (s.line, s.end_line, s.sig.as_deref()),
        (2, 4, Some("pub struct S {"))
    );
}

#[test]
fn body_hash_same_for_crlf_lf_and_bom() {
    let lf = "pub struct S;\n\nimpl S {\n    /// doc\n    pub fn open(&self) -> u32 {\n        1\n    }\n}\n";
    let crlf = lf.replace('\n', "\r\n");
    let bom = format!("\u{feff}{crlf}");
    let a = run(Lang::Rust, lf);
    let b = run(Lang::Rust, &crlf);
    let c = run(Lang::Rust, &bom);
    assert_eq!(a.symbols, b.symbols);
    assert_eq!(a.symbols, c.symbols);
    assert_eq!(sym(&a, "S").sig.as_deref(), Some("pub struct S;"));
    assert_eq!(sym(&a, "S::open").line, 5);
    // правка тела меняет хэш только у этого символа
    let edited = lf.replace("        1\n", "        2\n");
    let d = run(Lang::Rust, &edited);
    assert_ne!(sym(&a, "S::open").body_hash, sym(&d, "S::open").body_hash);
    assert_eq!(sym(&a, "S").body_hash, sym(&d, "S").body_hash);
    // переотступ тела хэш не меняет
    let reindented = lf.replace("        1\n", "  1\n");
    assert_eq!(
        sym(&a, "S::open").body_hash,
        sym(&run(Lang::Rust, &reindented), "S::open").body_hash
    );
}

#[test]
fn exported_flags_per_lang() {
    let rs = run(
        Lang::Rust,
        "pub fn a() {}\nfn b() {}\npub(crate) struct C;\n",
    );
    assert_eq!(sym(&rs, "a").exported, Some(true));
    assert_eq!(sym(&rs, "b").exported, Some(false));
    assert_eq!(sym(&rs, "C").exported, Some(true));
    let ts = run(
        Lang::TypeScript,
        "export function a() {}\nfunction b() {}\nexport class C {\n  m() {}\n}\n",
    );
    assert_eq!(sym(&ts, "a").exported, Some(true));
    assert_eq!(sym(&ts, "b").exported, Some(false));
    assert_eq!(sym(&ts, "C::m").exported, Some(true));
    let js = run(
        Lang::JavaScript,
        "export const a = () => 1;\nconst b = () => 2;\n",
    );
    assert_eq!(sym(&js, "a").exported, Some(true));
    assert_eq!(sym(&js, "b").exported, Some(false));
    let go = run(
        Lang::Go,
        "package a\nfunc Open() {}\nfunc open() {}\ntype T struct{}\n",
    );
    // ревью I1: граница экспорта — функция, блок, пространство имён; методы класса — да
    let ts = run(
        Lang::TypeScript,
        "export function main() {\n  function inner() {}\n  const loc = () => 1;\n}\nexport namespace G {\n  function hidden() {}\n  export function shown() {}\n  export class K {\n    m() {}\n  }\n}\nexport const o = { k: () => 1 };\nexport class E {\n  f() {}\n  g = () => 1;\n}\n",
    );
    for (q, want) in [
        ("main", true),
        ("inner", false),
        ("loc", false),
        ("G", true),
        ("G::hidden", false),
        ("G::shown", true),
        ("G::K", true),
        ("G::K::m", true),
        ("k", true),
        ("E::f", true),
        ("E::g", true),
    ] {
        assert_eq!(sym(&ts, q).exported, Some(want), "{q}");
    }
    assert_eq!(sym(&go, "Open").exported, Some(true));
    assert_eq!(sym(&go, "open").exported, Some(false));
    assert_eq!(sym(&go, "T").exported, Some(true));
    let java = run(
        Lang::Java,
        "public class A {\n  public void x() {}\n  void y() {}\n}\n",
    );
    assert_eq!(sym(&java, "A").exported, Some(true));
    assert_eq!(sym(&java, "A::x").exported, Some(true));
    assert_eq!(sym(&java, "A::y").exported, Some(false));
    let cs = run(
        Lang::CSharp,
        "public class A {\n  public void X() {}\n  private void Y() {}\n}\n",
    );
    assert_eq!(sym(&cs, "A::X").exported, Some(true));
    assert_eq!(sym(&cs, "A::Y").exported, Some(false));
    let py = run(Lang::Python, "def a():\n    pass\n");
    assert_eq!(sym(&py, "a").exported, None);
}

#[test]
fn refs_aggregated_call_type_import_impl() {
    let src = "use x::Db;\nuse y::{Conn, z::Pool};\n\nfn f(db: Db) -> Db {\n    let s = Store::open();\n    Store::open();\n    helper(1);\n    s.close();\n    db\n}\n\nimpl Trait for T {}\nimpl fmt::Display for T {}\n";
    let out = run(Lang::Rust, src);
    assert!(has_ref(&out, "Db", RefKind::Import), "{:?}", out.refs);
    assert!(has_ref(&out, "Conn", RefKind::Import));
    assert!(has_ref(&out, "Pool", RefKind::Import));
    assert!(has_ref(&out, "Db", RefKind::Type));
    assert!(has_ref(&out, "Store", RefKind::Type));
    assert!(has_ref(&out, "helper", RefKind::Call));
    assert!(has_ref(&out, "close", RefKind::Call));
    assert!(has_ref(&out, "Trait", RefKind::Impl));
    assert!(has_ref(&out, "Display", RefKind::Impl));
    let open = out
        .refs
        .iter()
        .find(|r| r.name == "open" && r.kind == RefKind::Call)
        .unwrap();
    assert_eq!((open.count, open.first_line), (2, 5));
    let db = out
        .refs
        .iter()
        .find(|r| r.name == "Db" && r.kind == RefKind::Type)
        .unwrap();
    assert_eq!((db.count, db.first_line), (2, 4));
    // одна строка на (имя, вид)
    let mut keys: Vec<_> = out.refs.iter().map(|r| (r.name.clone(), r.kind)).collect();
    let n = keys.len();
    keys.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.as_str().cmp(b.1.as_str())));
    keys.dedup();
    assert_eq!(keys.len(), n);

    let ts = run(
        Lang::TypeScript,
        "import { Store, type Db } from './s';\nclass A extends B implements C {\n  m(db: Db): Store { return new Store(); }\n}\n",
    );
    assert!(has_ref(&ts, "Store", RefKind::Import));
    assert!(has_ref(&ts, "Db", RefKind::Import));
    assert!(has_ref(&ts, "Db", RefKind::Type));
    assert!(has_ref(&ts, "Store", RefKind::Call));
    assert!(has_ref(&ts, "B", RefKind::Impl));
    assert!(has_ref(&ts, "C", RefKind::Impl));

    let cs = run(
        Lang::CSharp,
        "using System.Text;\nclass A : Base, IFoo<int> {\n  void M(Store s) { var x = new Logger(); s.Open(); Do(); Db d = null; }\n}\n",
    );
    for (name, kind) in [
        ("Text", RefKind::Import),
        ("Base", RefKind::Impl),
        ("IFoo", RefKind::Impl),
        ("Store", RefKind::Type),
        ("Logger", RefKind::Call),
        ("Open", RefKind::Call),
        ("Do", RefKind::Call),
        ("Db", RefKind::Type),
    ] {
        assert!(
            has_ref(&cs, name, kind),
            "C#: {name} {kind:?} в {:?}",
            cs.refs
        );
    }

    let py = run(
        Lang::Python,
        "from a.b import Store, Db as D\nimport os.path\nclass A(Base):\n    def m(self, s: Store):\n        s.open()\n        run()\n",
    );
    for (name, kind) in [
        ("Store", RefKind::Import),
        ("Db", RefKind::Import),
        ("path", RefKind::Import),
        ("Base", RefKind::Impl),
        ("Store", RefKind::Type),
        ("open", RefKind::Call),
        ("run", RefKind::Call),
    ] {
        assert!(
            has_ref(&py, name, kind),
            "Python: {name} {kind:?} в {:?}",
            py.refs
        );
    }

    let go = run(
        Lang::Go,
        "package a\nimport (\n  \"fmt\"\n  h \"net/http\"\n)\nfunc f(s Store) { fmt.Println(); g() }\n",
    );
    for (name, kind) in [
        ("fmt", RefKind::Import),
        ("http", RefKind::Import),
        ("Store", RefKind::Type),
        ("Println", RefKind::Call),
        ("g", RefKind::Call),
    ] {
        assert!(
            has_ref(&go, name, kind),
            "Go: {name} {kind:?} в {:?}",
            go.refs
        );
    }

    let java = run(
        Lang::Java,
        "import java.util.List;\nclass A extends B implements C {\n  Store m(Db d) { new Store(); d.open(); return null; }\n}\n",
    );
    for (name, kind) in [
        ("List", RefKind::Import),
        ("B", RefKind::Impl),
        ("C", RefKind::Impl),
        ("Store", RefKind::Call),
        ("Db", RefKind::Type),
        ("open", RefKind::Call),
    ] {
        assert!(
            has_ref(&java, name, kind),
            "Java: {name} {kind:?} в {:?}",
            java.refs
        );
    }
}

#[test]
fn definition_name_not_counted_as_ref() {
    let rs = run(
        Lang::Rust,
        "pub struct Store;\npub trait Backend {}\npub type Id = u32;\n",
    );
    assert!(
        rs.refs
            .iter()
            .all(|r| r.name != "Store" && r.name != "Backend" && r.name != "Id"),
        "{:?}",
        rs.refs
    );
    let ts = run(
        Lang::TypeScript,
        "interface Opts {}\nclass App {}\ntype Id = string;\n",
    );
    assert!(ts.refs.is_empty(), "{:?}", ts.refs);
    let go = run(Lang::Go, "package a\ntype Store struct{}\n");
    assert!(go.refs.iter().all(|r| r.name != "Store"), "{:?}", go.refs);
}

#[test]
fn too_large_binary_minified_skipped() {
    let small = Limits {
        max_file_bytes: 1024,
        ..lim()
    };
    let big = "fn a() {}\n".repeat(103);
    let out = extract(Lang::Rust, big.as_bytes(), &small);
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Skipped, Some(SkipReason::TooLarge))
    );
    assert!(out.symbols.is_empty() && out.refs.is_empty());

    let mut bin = b"fn a() {}\n".to_vec();
    bin.push(0);
    let out = extract(Lang::Rust, &bin, &lim());
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Skipped, Some(SkipReason::Binary))
    );
    assert_eq!(sniff(&bin), Some(SkipReason::Binary));

    let long_line = format!("var a = '{}';\n", "x".repeat(10_001));
    assert_eq!(sniff(long_line.as_bytes()), Some(SkipReason::Minified));
    // строка ровно 10 000 символов среди коротких: средняя ниже 300 — не минифицирован
    let ok_line = format!("{}\n{}", "x".repeat(10_000), "short line\n".repeat(50));
    assert_eq!(sniff(ok_line.as_bytes()), None);
    let avg301 = format!("{}\n", "y".repeat(301)).repeat(20);
    assert_eq!(sniff(avg301.as_bytes()), Some(SkipReason::Minified));
    let avg300 = format!("{}\n", "y".repeat(300)).repeat(20);
    assert_eq!(sniff(avg300.as_bytes()), None);
    let out = extract(Lang::JavaScript, avg301.as_bytes(), &lim());
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Skipped, Some(SkipReason::Minified))
    );
    // NUL дальше первых 8 КиБ — не бинарный
    let mut late = "x\n".repeat(5000).into_bytes();
    late.push(0);
    assert_eq!(sniff(&late), None);
}

#[test]
fn invalid_utf8_parses_without_panic() {
    let mut src = b"fn caf\xe9() {}\nfn ok() {}\nstruct \xff\xfe;\n".to_vec();
    src.extend_from_slice(b"fn tail() {}\n");
    let out = extract(Lang::Rust, &src, &lim());
    assert_ne!(out.skip, Some(SkipReason::ParserCrash));
    assert!(
        out.symbols.iter().any(|s| s.qual == "ok"),
        "{:?}",
        out.symbols
    );
    assert!(out.symbols.iter().all(|s| !s.name.is_empty()));
}

#[test]
fn deep_nesting_100k_no_stack_overflow() {
    let src = "(\n".repeat(100_000);
    let l = Limits {
        max_file_bytes: 16 << 20,
        ..lim()
    };
    let out = extract(Lang::JavaScript, src.as_bytes(), &l);
    assert!(
        out.state == ParseState::Ok || out.skip == Some(SkipReason::Timeout),
        "{:?} {:?}",
        out.state,
        out.skip
    );
    let src = "[".repeat(100_000);
    let mut lines = String::new();
    for chunk in src.as_bytes().chunks(50) {
        lines.push_str(std::str::from_utf8(chunk).unwrap());
        lines.push('\n');
    }
    let out = extract(Lang::Python, lines.as_bytes(), &l);
    assert_ne!(out.skip, Some(SkipReason::ParserCrash));
}

#[test]
fn minified_js_10mb_skipped_fast() {
    let mut src = String::with_capacity(10 << 20);
    while src.len() < 10 << 20 {
        src.push_str("function a(b){return b+1};var c=a(2);");
    }
    let l = Limits {
        max_file_bytes: 16 << 20,
        ..lim()
    };
    let t = Instant::now();
    let out = extract(Lang::JavaScript, src.as_bytes(), &l);
    let el = t.elapsed();
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Skipped, Some(SkipReason::Minified))
    );
    assert!(el <= Duration::from_secs(1), "{el:?}");
}

#[test]
fn timeout_reported() {
    let mut src = String::new();
    let mut i = 0;
    while src.len() < 2 << 20 {
        src.push_str(&format!(
            "pub fn f{i}(a: u32) -> u32 {{\n    g(a) + h(a)\n}}\n"
        ));
        i += 1;
    }
    let l = Limits {
        max_file_bytes: 16 << 20,
        parse_timeout: Duration::from_millis(1),
        ..Limits::default()
    };
    let t = Instant::now();
    let out = extract(Lang::Rust, src.as_bytes(), &l);
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Skipped, Some(SkipReason::Timeout))
    );
    assert!(out.symbols.is_empty() && out.refs.is_empty());
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn limits_partial_symbols_and_refs() {
    let src: String = (0..50)
        .map(|i| format!("fn f{i}() {{ g{i}(); }}\n"))
        .collect();
    let l = Limits {
        max_symbols: 10,
        ..lim()
    };
    let out = extract(Lang::Rust, src.as_bytes(), &l);
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Partial, Some(SkipReason::Limit))
    );
    assert_eq!(out.symbols.len(), 10);
    assert_eq!(out.symbols[0].qual, "f0");
    assert_eq!(out.symbols[9].qual, "f9");
    assert_eq!(out.refs.len(), 50);

    let l = Limits {
        max_refs: 7,
        ..lim()
    };
    let out = extract(Lang::Rust, src.as_bytes(), &l);
    assert_eq!(
        (out.state, out.skip),
        (ParseState::Partial, Some(SkipReason::Limit))
    );
    assert_eq!(out.symbols.len(), 50);
    let names: Vec<&str> = out.refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["g0", "g1", "g2", "g3", "g4", "g5", "g6"]);

    let out = extract(Lang::Rust, src.as_bytes(), &lim());
    assert_eq!((out.state, out.skip), (ParseState::Ok, None));
}

#[test]
fn symbol_hashes_match_extract_and_none_when_skipped() {
    let src = "struct S;\nimpl S {\n    fn a() {}\n    fn b() {}\n}\n";
    let out = run(Lang::Rust, src);
    let hs = symbol_hashes(Lang::Rust, src.as_bytes(), &lim()).unwrap();
    let pairs: Vec<(String, String)> = hs.into_iter().map(|h| (h.qual, h.body_hash)).collect();
    let want: Vec<(String, String)> = out
        .symbols
        .iter()
        .map(|s| (s.qual.clone(), s.body_hash.clone()))
        .collect();
    assert_eq!(pairs, want);
    assert!(symbol_hashes(Lang::Rust, b"fn a() {}\0", &lim()).is_none());
    // усечённый пределом список хук принял бы за удаление лишних символов (ревью, M4)
    let many: String = (0..20).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let l = Limits {
        max_symbols: 5,
        ..lim()
    };
    assert_eq!(
        extract(Lang::Rust, many.as_bytes(), &l).state,
        ParseState::Partial
    );
    assert!(symbol_hashes(Lang::Rust, many.as_bytes(), &l).is_none());
}

/// Ревью C1: подъём к корню через `Node::parent()` был O(глубины²) — 80 КБ вложенных блоков
/// разбирались 14 с при отсечке 200 мс. Теперь один обход; отсечка соблюдается.
#[test]
fn deep_blocks_definition_respects_timeout() {
    let n = 20_000;
    let cases = [
        (
            Lang::JavaScript,
            format!("{}function f() {{}}\n{}", "{\n".repeat(n), "}\n".repeat(n)),
        ),
        (
            Lang::Rust,
            format!("{}fn f() {{}}\n{}", "mod a {\n".repeat(n), "}\n".repeat(n)),
        ),
        (
            Lang::CSharp,
            format!(
                "{}class C {{ void M() {{}} }}\n{}",
                "namespace A {\n".repeat(n),
                "}\n".repeat(n)
            ),
        ),
        (
            Lang::JavaScript,
            format!("{}{}", "class A { m() {\n".repeat(n), "}}\n".repeat(n)),
        ),
    ];
    for (lang, src) in cases {
        let l = Limits {
            max_file_bytes: 16 << 20,
            parse_timeout: Duration::from_millis(50),
            ..Limits::default()
        };
        let t = Instant::now();
        let out = extract(lang, src.as_bytes(), &l);
        let el = t.elapsed();
        assert!(
            out.state == ParseState::Ok || out.skip == Some(SkipReason::Timeout),
            "{lang:?}: {:?} {:?}",
            out.state,
            out.skip
        );
        assert!(el < Duration::from_millis(150), "{lang:?}: {el:?}");
        let t = Instant::now();
        let _ = symbol_hashes(lang, src.as_bytes(), &l);
        assert!(
            t.elapsed() < Duration::from_millis(150),
            "{lang:?} hashes: {:?}",
            t.elapsed()
        );
    }
}

/// Ревью C2: рекурсивный разбор типа `impl` переполнял стек (catch_unwind его не ловит).
#[test]
fn rust_impl_deep_reference_type_no_stack_overflow() {
    for n in [10_000, 100_000] {
        let src = format!("impl X for\n{}T {{ fn f() {{}} }}\n", "&\n".repeat(n));
        let l = Limits {
            max_file_bytes: 16 << 20,
            ..lim()
        };
        let out = extract(Lang::Rust, src.as_bytes(), &l);
        assert_ne!(out.skip, Some(SkipReason::ParserCrash));
        let generic = format!(
            "impl X for\n{}T{} {{ fn f() {{}} }}\n",
            "V<\n".repeat(n),
            ">\n".repeat(n)
        );
        let out = extract(Lang::Rust, generic.as_bytes(), &l);
        assert_ne!(out.skip, Some(SkipReason::ParserCrash));
    }
}

/// Ревью M1: `impl` для среза, кортежа, `dyn` — тоже контейнер, функции в нём — методы.
#[test]
fn rust_impl_for_any_type_is_container() {
    let src = "impl X for [u8] {\n    fn f(&self) {}\n}\nimpl<T> X for Box<dyn Tr<T>> {\n    fn g(&self) {}\n}\nimpl X for (A, B) {\n    fn h(&self) {}\n}\nimpl X for &mut S {\n    fn k(&self) {}\n}\n";
    let out = run(Lang::Rust, src);
    for name in ["f", "g", "h", "k"] {
        let s = out.symbols.iter().find(|s| s.name == name).unwrap();
        assert_eq!(s.kind, SymbolKind::Method, "{name}: {:?}", quals(&out));
        assert!(s.qual.ends_with(&format!("::{name}")), "{}", s.qual);
    }
    assert_eq!(sym(&out, "Box::g").line, 5);
    assert_eq!(sym(&out, "S::k").line, 11);
}

/// Ревью I2: литералы объектов внутри функций не дают символов (и не сдвигают `#N`).
#[test]
fn object_literal_members_only_at_top_level() {
    let src = "export const api = {\n  list() {},\n  remove: (id) => id,\n};\nfunction make() {\n  return { cb: () => 2, m() {} };\n}\nclass K {\n  f() { return { inner: () => 1 }; }\n}\n";
    for lang in [Lang::JavaScript, Lang::TypeScript] {
        let out = run(lang, src);
        assert_eq!(sym(&out, "list").kind, SymbolKind::Method);
        assert_eq!(sym(&out, "remove").kind, SymbolKind::Function);
        for q in ["cb", "m", "inner", "K::inner"] {
            assert!(
                out.symbols.iter().all(|s| s.qual != q),
                "{lang:?}: {q} в {:?}",
                quals(&out)
            );
        }
    }
}

/// Ревью M6: имя с `/` или `#` (ключ литерала `"a/b": () => {}`) — не символ.
#[test]
fn names_with_slash_or_hash_rejected() {
    let out = run(
        Lang::JavaScript,
        "const o = {\n  \"a/b\": () => 1,\n  \"c#d\": () => 2,\n  ok: () => 3,\n};\n",
    );
    assert_eq!(quals(&out), ["function ok"]);
}

/// `symbol_hashes` (хуки) идёт отдельным запросом только определений — итог тот же, что у
/// `extract`, на всех эталонах.
#[test]
fn symbol_hashes_equal_extract_on_all_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (lang, dir, file) in FIXTURES {
        let src = std::fs::read(root.join(dir).join(file)).unwrap();
        let want: Vec<(String, String)> = extract(lang, &src, &lim())
            .symbols
            .into_iter()
            .map(|s| (s.qual, s.body_hash))
            .collect();
        let got: Vec<(String, String)> = symbol_hashes(lang, &src, &lim())
            .unwrap()
            .into_iter()
            .map(|h| (h.qual, h.body_hash))
            .collect();
        assert_eq!(got, want, "{dir}");
    }
}

#[test]
fn every_lang_extracts_names_within_limits() {
    for (lang, src) in [
        (Lang::Rust, "fn a() {}\n"),
        (Lang::TypeScript, "function a() {}\n"),
        (Lang::Tsx, "function a() { return <div/>; }\n"),
        (Lang::JavaScript, "function a() {}\n"),
        (Lang::Python, "def a():\n    pass\n"),
        (Lang::Go, "package p\nfunc a() {}\n"),
        (Lang::Java, "class a {}\n"),
        (Lang::CSharp, "class a {}\n"),
    ] {
        let out = run(lang, src);
        assert_eq!(out.symbols.len(), 1, "{lang:?}: {:?}", quals(&out));
        assert_eq!(out.symbols[0].name, "a");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn extract_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..4096), li in 0usize..8) {
        let l = Limits { parse_timeout: Duration::from_millis(50), ..Limits::default() };
        let out = extract(Lang::ALL[li], &bytes, &l);
        prop_assert_ne!(out.skip, Some(SkipReason::ParserCrash));
        for s in &out.symbols {
            prop_assert!(!s.name.is_empty() && s.name.chars().count() <= zam_code::NAME_MAX_CHARS);
            prop_assert!(s.qual.chars().count() <= zam_code::QUAL_MAX_CHARS);
            prop_assert!(s.line >= 1 && s.end_line >= s.line);
            prop_assert_eq!(s.body_hash.len(), 16);
        }
    }

    #[test]
    fn extract_never_panics_on_code_like_text(
        parts in proptest::collection::vec(
            prop_oneof![
                Just("fn "), Just("class "), Just("def "), Just("func "), Just("{"), Just("}"),
                Just("("), Just(")"), Just("\n"), Just("impl "), Just("x"), Just(":"), Just("=>"),
                Just("\""), Just("#"), Just("@"), Just("<"), Just(">"), Just(";"), Just("namespace "),
            ],
            0..400,
        ),
        li in 0usize..8,
    ) {
        let src: String = parts.concat();
        let l = Limits { parse_timeout: Duration::from_millis(50), ..Limits::default() };
        let out = extract(Lang::ALL[li], src.as_bytes(), &l);
        prop_assert_ne!(out.skip, Some(SkipReason::ParserCrash));
    }
}
