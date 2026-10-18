//! XSS-набор: каждый вектор из `xss_vectors.txt` проходит `render_html`
//! с разными резолверами; проверяется итоговый санитизированный HTML.

use zam_markdown::{LinkResolution, NoResolve, Resolver, render_html, sanitize_html};

const VECTORS: &str = include_str!("xss_vectors.txt");

/// Векторы из файла: блоки через строку `---8<---`.
fn file_vectors() -> Vec<String> {
    VECTORS
        .split("---8<---")
        .map(|b| b.trim_matches(['\n', '\r']).to_string())
        .filter(|b| !b.trim().is_empty())
        .collect()
}

/// Векторы с управляющими и невидимыми символами, которых нет в текстовом файле.
fn generated_vectors() -> Vec<String> {
    let mut v = Vec::new();
    for sep in [
        "\u{0}", "\t", "\n", "\r", "\u{b}", "\u{c}", "\u{200b}", "\u{feff}", "\u{a0}",
    ] {
        v.push(format!("[x](java{sep}script:alert(1))"));
        v.push(format!("[x]({sep}javascript:alert(1))"));
        v.push(format!("[[java{sep}script:alert(1)]]"));
        v.push(format!("<a href=\"java{sep}script:alert(1)\">x</a>"));
    }
    v
}

/// Резолвер, который вставляет присланные данные в URL как есть.
struct Echo;

impl Resolver for Echo {
    fn wiki(&self, target: &str, anchor: Option<&str>) -> Option<LinkResolution> {
        Some(LinkResolution {
            href: format!("notes/{target}#{}", anchor.unwrap_or("")),
            exists: true,
        })
    }
    fn attachment(&self, sha16: &str, filename: &str) -> Option<String> {
        Some(format!("api/v1/blobs/{sha16}/{filename}"))
    }
}

/// Резолвер, который возвращает опасные URL.
struct Evil;

impl Resolver for Evil {
    fn wiki(&self, _: &str, _: Option<&str>) -> Option<LinkResolution> {
        Some(LinkResolution {
            href: "javascript:alert(1)".into(),
            exists: false,
        })
    }
    fn attachment(&self, _: &str, _: &str) -> Option<String> {
        Some("data:text/html,<script>alert(1)</script>".into())
    }
}

/// Атрибуты всех тегов вывода: `(тег, имя, значение)`.
fn attributes(html: &str) -> Vec<(String, String, String)> {
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' || i + 1 >= b.len() || !b[i + 1].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        i += 1;
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
            i += 1;
        }
        let tag = html[start..i].to_ascii_lowercase();
        loop {
            while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
                i += 1;
            }
            if i >= b.len() || b[i] == b'>' {
                break;
            }
            let ns = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/')
            {
                i += 1;
            }
            let name = html[ns..i].to_ascii_lowercase();
            let mut value = String::new();
            if i < b.len() && b[i] == b'=' {
                i += 1;
                if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                    let q = b[i];
                    i += 1;
                    let vs = i;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    value = html[vs..i].to_string();
                    i += 1;
                } else {
                    let vs = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = html[vs..i].to_string();
                }
            }
            out.push((tag.clone(), name, value));
        }
    }
    out
}

/// Значение атрибута так, как его увидит браузер при разборе URL: без
/// пробельных и управляющих символов, в нижнем регистре.
fn normalized(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control() && *c != '\u{200b}' && *c != '\u{feff}')
        .collect::<String>()
        .to_lowercase()
}

fn assert_safe(vector: &str, html: &str) {
    let lower = html.to_lowercase();
    for tag in [
        "<script",
        "<iframe",
        "<object",
        "<embed",
        "<form",
        "<style",
        "<base",
        "<meta",
        "<link",
        "<svg",
        "<math",
        "<textarea",
        "<noscript",
        "<xmp",
        "<input type=\"text",
    ] {
        assert!(
            !lower.contains(tag),
            "вектор {vector:?}: в выводе {tag}\n{html}"
        );
    }
    for (tag, name, value) in attributes(html) {
        assert!(
            !(name.starts_with("on") && name.len() > 2),
            "вектор {vector:?}: атрибут {name} у <{tag}>\n{html}"
        );
        assert!(
            name != "style",
            "вектор {vector:?}: style у <{tag}>\n{html}"
        );
        let v = normalized(&value);
        for bad in ["javascript:", "vbscript:", "data:text/html"] {
            assert!(
                !v.contains(bad),
                "вектор {vector:?}: {bad} в {tag}[{name}]\n{html}"
            );
        }
    }
}

#[test]
fn at_least_forty_vectors() {
    let n = file_vectors().len();
    assert!(n >= 40, "векторов {n}");
}

#[test]
fn attribute_scanner_sees_attributes() {
    // самопроверка сканера: иначе assert_safe ничего не проверяет
    let a = attributes(r#"<a href="javascript:x" onclick=y><img src='z' alt>"#);
    assert_eq!(
        a,
        vec![
            ("a".into(), "href".into(), "javascript:x".into()),
            ("a".into(), "onclick".into(), "y".into()),
            ("img".into(), "src".into(), "z".into()),
            ("img".into(), "alt".into(), String::new()),
        ]
    );
}

#[test]
fn xss_vectors_are_neutralized() {
    let resolvers: [(&str, &dyn Resolver); 3] =
        [("NoResolve", &NoResolve), ("Echo", &Echo), ("Evil", &Evil)];
    for v in file_vectors().into_iter().chain(generated_vectors()) {
        for (name, r) in resolvers {
            let out = render_html(&v, r);
            assert_safe(&format!("{name}: {v}"), &out.html);
        }
    }
}

#[test]
fn xss_vectors_neutralized_inside_containers() {
    // те же векторы внутри callout, списка и сноски
    for v in file_vectors() {
        let quoted: String = v.lines().map(|l| format!("> {l}\n")).collect();
        let wrapped = [
            format!("> [!info]- заголовок\n{quoted}"),
            format!("> [!warning]\n{quoted}"),
            format!("- {}", v.replace('\n', "\n  ")),
            format!("текст[^a]\n\n[^a]: {}", v.replace('\n', "\n    ")),
        ];
        for w in wrapped {
            assert_safe(&w, &render_html(&w, &Echo).html);
        }
    }
}

// ---------- Allowlist санитайзера напрямую ----------

#[test]
fn sanitizer_removes_raw_dangerous_html() {
    for raw in [
        "<script>alert(1)</script>",
        "<img src=x onerror=alert(1)>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<a href=\"vbscript:x\">x</a>",
        "<a href=\"data:text/html,x\">x</a>",
        "<iframe src=x></iframe><object></object><embed>",
        "<form action=x><input type=text></form>",
        "<style>*{}</style><link rel=x><meta http-equiv=refresh><base href=x>",
        "<div style=\"color:red\" onclick=x>t</div>",
        "<svg onload=alert(1)><a xlink:href=\"javascript:x\">t</a></svg>",
        "<math><mi xlink:href=\"javascript:x\">t</mi></math>",
        "<span data-target=\"javascript:alert(1)\">t</span>",
        "<video src=\"javascript:x\" poster=\"javascript:x\"></video>",
    ] {
        assert_safe(raw, &sanitize_html(raw));
    }
}

#[test]
fn sanitizer_class_prefix_filter() {
    let out = sanitize_html(
        r#"<div class="callout evil hl-source x-hl-y wikilink broken math footnotes task-list-item mermaid-src attachment">t</div>"#,
    );
    assert!(
        out.contains(r#"class="callout hl-source wikilink broken math footnotes task-list-item mermaid-src attachment""#),
        "{out}"
    );
    let out = sanitize_html(r#"<div class="evil">t</div>"#);
    assert_eq!(out, "<div>t</div>");
}

#[test]
fn sanitizer_ids_only_with_known_prefixes() {
    let out = sanitize_html(r#"<h2 id="h-a">a</h2><h2 id="evil">b</h2><p id="h-c">c</p>"#);
    assert_eq!(out, r#"<h2 id="h-a">a</h2><h2>b</h2><p>c</p>"#);
    let out = sanitize_html(
        r##"<li id="fn-1">a</li><a id="fnref-1" href="#fn-1">b</a><li id="x">c</li>"##,
    );
    assert!(out.contains(r#"<li id="fn-1">"#), "{out}");
    assert!(out.contains(r#"id="fnref-1""#), "{out}");
    assert!(out.contains("<li>c</li>"), "{out}");
}

#[test]
fn sanitizer_rel_and_schemes() {
    let out = sanitize_html(
        r#"<a href="https://x.example" rel="noopener noreferrer">a</a><a href="mailto:a@b">m</a><a href="rel/p.md" rel="opener">r</a><a href="ftp://x">f</a>"#,
    );
    assert!(out.contains(r#"href="https://x.example""#), "{out}");
    assert!(out.contains(r#"rel="noopener noreferrer""#), "{out}");
    assert!(out.contains(r#"href="mailto:a@b""#), "{out}");
    assert!(out.contains(r#"<a href="rel/p.md">r</a>"#), "{out}");
    assert!(!out.contains("ftp:"), "{out}");
    assert!(!out.contains("opener\""), "{out}");
}

#[test]
fn sanitizer_input_only_checkbox() {
    let out = sanitize_html(
        r#"<input type="checkbox" checked="" disabled=""><input type="text" value="x">"#,
    );
    assert!(out.contains(r#"type="checkbox""#), "{out}");
    assert!(!out.contains("text"), "{out}");
    assert!(!out.contains("value"), "{out}");
}
