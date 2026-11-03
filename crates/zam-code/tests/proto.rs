//! Протокол карты кода и нормализация имён (без фичи `parse`: эти функции берёт и сервер).

use zam_code::{
    Lang, MISSING_HASH, ParseState, RefKind, SIG_MAX_CHARS, SkipReason, SymbolKind, body_hash,
    fts_terms, lang_of, sig_line, split_ident, split_ref,
};

#[test]
fn lang_of_extensions() {
    let table = [
        ("src/lib.rs", Lang::Rust),
        ("a.ts", Lang::TypeScript),
        ("a.mts", Lang::TypeScript),
        ("a.cts", Lang::TypeScript),
        ("types/a.d.ts", Lang::TypeScript),
        ("a.tsx", Lang::Tsx),
        ("a.js", Lang::JavaScript),
        ("a.mjs", Lang::JavaScript),
        ("a.cjs", Lang::JavaScript),
        ("a.jsx", Lang::JavaScript),
        ("a.py", Lang::Python),
        ("a.pyi", Lang::Python),
        ("cmd/main.go", Lang::Go),
        ("A.java", Lang::Java),
        ("A.cs", Lang::CSharp),
        ("SRC\\STORE.RS", Lang::Rust),
        ("Web/App.TSX", Lang::Tsx),
    ];
    for (path, lang) in table {
        assert_eq!(lang_of(path), Some(lang), "{path}");
    }
    for path in [
        "a.svelte",
        "a.c",
        "a.h",
        "a.vue",
        "Makefile",
        "rs",
        ".rs",
        "a.rs/",
        "dir.rs/file",
        "a.",
        "",
    ] {
        assert_eq!(lang_of(path), None, "{path}");
    }
}

#[test]
fn lang_as_str_and_parse_roundtrip() {
    let all = [
        (Lang::Rust, "rust"),
        (Lang::TypeScript, "typescript"),
        (Lang::Tsx, "tsx"),
        (Lang::JavaScript, "javascript"),
        (Lang::Python, "python"),
        (Lang::Go, "go"),
        (Lang::Java, "java"),
        (Lang::CSharp, "csharp"),
    ];
    for (lang, s) in all {
        assert_eq!(lang.as_str(), s);
        assert_eq!(Lang::parse(s), Some(lang));
        assert_eq!(serde_json::to_string(&lang).unwrap(), format!("\"{s}\""));
    }
    assert_eq!(Lang::parse("Rust"), None);
    assert_eq!(Lang::ALL.len(), 8);
}

#[test]
fn enums_serialize_as_spec() {
    let s = |v: serde_json::Value| v.as_str().unwrap().to_string();
    assert_eq!(
        s(serde_json::to_value(SymbolKind::Function).unwrap()),
        "function"
    );
    assert_eq!(
        s(serde_json::to_value(SymbolKind::Interface).unwrap()),
        "interface"
    );
    assert_eq!(s(serde_json::to_value(RefKind::Impl).unwrap()), "impl");
    assert_eq!(
        s(serde_json::to_value(ParseState::Partial).unwrap()),
        "partial"
    );
    assert_eq!(
        s(serde_json::to_value(SkipReason::TooLarge).unwrap()),
        "too_large"
    );
    assert_eq!(
        s(serde_json::to_value(SkipReason::ParserCrash).unwrap()),
        "parser_crash"
    );
    let k: SymbolKind = serde_json::from_str("\"macro\"").unwrap();
    assert_eq!(k, SymbolKind::Macro);
    assert_eq!(SymbolKind::Const.as_str(), "const");
    assert_eq!(SymbolKind::parse("trait"), Some(SymbolKind::Trait));
    assert_eq!(RefKind::parse("other"), Some(RefKind::Other));
    assert_eq!(SkipReason::Lang.as_str(), "lang");
    assert_eq!(ParseState::Skipped.as_str(), "skipped");
}

#[test]
fn split_ref_rules() {
    assert_eq!(split_ref("a.rs#Store::open"), ("a.rs", Some("Store::open")));
    assert_eq!(
        split_ref("crates/x/src/lib.rs#foo#2"),
        ("crates/x/src/lib.rs", Some("foo#2"))
    );
    assert_eq!(split_ref("src/c#/x.rs#Foo"), ("src/c#/x.rs", Some("Foo")));
    assert_eq!(split_ref("docs/a#b.md"), ("docs/a#b.md", None));
    assert_eq!(split_ref("a.rs#"), ("a.rs#", None));
    assert_eq!(split_ref("a.rs#x/y"), ("a.rs#x/y", None));
    assert_eq!(split_ref("a.rs"), ("a.rs", None));
    assert_eq!(split_ref("a.md#Title"), ("a.md#Title", None));
    assert_eq!(split_ref("a.rs#x\u{7}y"), ("a.rs#x\u{7}y", None));
    let long = format!("a.rs#{}", "x".repeat(513));
    assert_eq!(split_ref(&long), (long.as_str(), None));
    let max = format!("a.rs#{}", "я".repeat(512));
    assert_eq!(split_ref(&max).1.map(|q| q.chars().count()), Some(512));
}

#[test]
fn split_ident_camel_snake_acronym() {
    assert_eq!(split_ident("getUserById"), ["get", "user", "by", "id"]);
    assert_eq!(split_ident("HTTPServer"), ["http", "server"]);
    assert_eq!(
        split_ident("parse_http_request"),
        ["parse", "http", "request"]
    );
    assert_eq!(split_ident("MAX_ITEMS"), ["max", "items"]);
    assert_eq!(split_ident("__init__"), ["init"]);
    assert_eq!(split_ident("XMLHttpRequest2"), ["xml", "http", "request2"]);
    assert_eq!(split_ident("utf8Decode"), ["utf8", "decode"]);
    assert_eq!(split_ident("ПолучитьДанные"), ["получить", "данные"]);
    assert!(split_ident("___").is_empty());
}

#[test]
fn fts_terms_name_and_qual_parts() {
    assert_eq!(fts_terms("open", "Store::open"), "open store");
    assert_eq!(
        fts_terms("getUserById", "UserRepo::getUserById#2"),
        "get user by id repo"
    );
    assert_eq!(fts_terms("run", "run"), "run");
}

#[test]
fn missing_hash_is_64_zeros() {
    assert_eq!(MISSING_HASH.len(), 64);
    assert!(MISSING_HASH.bytes().all(|b| b == b'0'));
}

#[test]
fn sig_first_line_trimmed_160_with_ellipsis() {
    assert_eq!(
        sig_line("\n  \n  fn open(&self) {  \n    body\n}"),
        Some("  fn open(&self) {".into())
    );
    assert_eq!(sig_line("fn a() {}\r\n"), Some("fn a() {}".into()));
    assert_eq!(sig_line(" \n\t\n"), None);
    let exact = "x".repeat(SIG_MAX_CHARS);
    assert_eq!(sig_line(&exact), Some(exact.clone()));
    let long = "y".repeat(SIG_MAX_CHARS + 1);
    let cut = sig_line(&long).unwrap();
    assert_eq!(cut.chars().count(), SIG_MAX_CHARS);
    assert_eq!(cut, format!("{}…", "y".repeat(SIG_MAX_CHARS - 1)));
    let cyr = "ж".repeat(200);
    let cut = sig_line(&cyr).unwrap();
    assert_eq!(cut.chars().count(), SIG_MAX_CHARS);
    assert!(cut.ends_with("ж…"));
}

#[test]
fn body_hash_ignores_indent_and_blank_lines() {
    let a = "fn open() {\n    let x = 1;\n\n    x\n}";
    let b = "fn open() {\n\t\tlet x = 1;\n        x   \n\n\n}";
    let c = "fn open() {\n    let x = 2;\n\n    x\n}";
    assert_eq!(body_hash(a), body_hash(b));
    assert_ne!(body_hash(a), body_hash(c));
    assert_eq!(body_hash(a).len(), 16);
    assert!(
        body_hash(a)
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(body_hash("a\r\nb"), body_hash("a\nb"));
}
