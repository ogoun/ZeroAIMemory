//! XSS-набор: каждый вектор из `xss_vectors.txt` проходит `render_html`
//! с разными резолверами; проверяется итоговый санитизированный HTML.

use zam_markdown::{
    LinkResolution, NoResolve, Resolver, render_html, render_report_html, sanitize_html,
    sanitize_report_html,
};

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

#[test]
fn sanitizer_data_callout_unicode_only_letters_digits() {
    let out = sanitize_html(r#"<div class="callout" data-callout="заметка_1-x">t</div>"#);
    assert!(out.contains(r#"data-callout="заметка_1-x""#), "{out}");
    for bad in [
        r#"<div data-callout="a b">t</div>"#,
        r#"<div data-callout="тип:x">t</div>"#,
        r#"<div data-callout="">t</div>"#,
        "<div data-callout=\"тип\u{202e}\">t</div>",
    ] {
        let out = sanitize_html(bad);
        assert!(!out.contains("data-callout"), "{bad}: {out}");
    }
}

// ---------- Рендер отчёта: схемы ссылок (M6 §16 п. 15) ----------

/// Каждый URL-атрибут вывода отчёта: схемы нет или она из общего списка; `vscode:` и `file:` —
/// только у `<a href>`.
fn assert_report_schemes(vector: &str, html: &str) {
    for (tag, name, value) in attributes(html) {
        if !matches!(name.as_str(), "href" | "src" | "poster" | "action" | "data") {
            continue;
        }
        let value = value.replace("&amp;", "&").replace("&quot;", "\"");
        let Some(scheme) = zam_core::link::url_scheme(&value) else {
            continue;
        };
        assert!(
            zam_core::link::report_scheme_allowed(&scheme),
            "вектор {vector:?}: схема {scheme} в {tag}[{name}]\n{html}"
        );
        assert!(
            (tag == "a" && name == "href") || !matches!(scheme.as_str(), "vscode" | "file"),
            "вектор {vector:?}: {scheme}: вне ссылки — {tag}[{name}]\n{html}"
        );
    }
}

/// XSS-набор через рендер отчёта: расширение схем не открыло ни одного вектора.
#[test]
fn report_xss_vectors_are_neutralized() {
    let resolvers: [(&str, &dyn Resolver); 3] =
        [("NoResolve", &NoResolve), ("Echo", &Echo), ("Evil", &Evil)];
    let schemes = [
        "javascript",
        "vbscript",
        "data",
        "ftp",
        "blob",
        "file",
        "vscode",
    ];
    let mut extra = Vec::new();
    for s in schemes {
        extra.push(format!("![x]({s}:alert(1))"));
        extra.push(format!(
            "<img src=\"{s}://x/a.png\"> <video src=\"{s}://x\"></video>"
        ));
        extra.push(format!(
            "<a href=\"{s}:alert(1)\">x</a> [y]({s}:alert(1)) <{s}:alert(1)>"
        ));
        extra.push(format!("[r]\n\n[r]: {s}:alert(1)\n"));
    }
    for v in file_vectors()
        .into_iter()
        .chain(generated_vectors())
        .chain(extra)
    {
        for (name, r) in resolvers {
            let out = render_report_html(&v, r);
            assert_safe(&format!("отчёт, {name}: {v}"), &out.html);
            assert_report_schemes(&format!("отчёт, {name}: {v}"), &out.html);
        }
    }
}

/// Ссылки отчёта: пять схем — с `href` и `rel`; прочая схема — без `href`; относительная и якорь
/// — без `rel`. Регистр, пробел и табуляция в схеме решения не меняют.
#[test]
fn report_link_schemes_href_and_rel() {
    const REL: &str = "rel=\"noopener noreferrer\"";
    let src = "[h](https://e.org/a) [p](http://e.org) [m](mailto:a@b.c) \
               [v](vscode://file/C:/proj/a.rs:10:5) [f](file:///C:/data/a.csv) \
               <file:///etc/hosts> [V](VSCode://file/x.rs) [r](rel/a.md) [я](#h-x) [n](//e.org/x)\n";
    let out = render_report_html(src, &NoResolve).html;
    for url in [
        "https://e.org/a",
        "http://e.org",
        "mailto:a@b.c",
        "vscode://file/C:/proj/a.rs:10:5",
        "file:///C:/data/a.csv",
        "file:///etc/hosts",
        "VSCode://file/x.rs",
        "//e.org/x",
    ] {
        let a = format!("href=\"{url}\" {REL}");
        assert!(out.contains(&a), "{a}\n{out}");
    }
    assert!(out.contains("href=\"rel/a.md\">r</a>"), "{out}");
    assert!(out.contains("href=\"#h-x\">я</a>"), "{out}");
    assert_eq!(out.matches(REL).count(), 8, "{out}");

    for bad in [
        "[x](ftp://e.org/a)",
        "<ftp://e.org/a>",
        "[x](javascript:alert(1))",
        "[x](JaVaScRiPt:alert(1))",
        "[x]( javascript:alert(1))",
        "[x](<java\tscript:alert(1)>)",
        "[x](<\tjavascript:alert(1)>)",
        "[x](java&Tab;script:alert(1))",
        "[x](&#106;avascript:alert(1))",
        "[x](data:text/html;base64,PGI+)",
        "[x](vbscript:msgbox(1))",
        "[x](vscode-insiders://file/x)",
        "[x](blob:https://e.org/1)",
        "[x][r]\n\n[r]: javascript:alert(1)\n",
    ] {
        let out = render_report_html(bad, &NoResolve).html;
        assert_report_schemes(bad, &out);
        assert_safe(bad, &out);
        for (_, name, value) in attributes(&out) {
            assert!(
                name != "href" || value.is_empty(),
                "{bad}: href={value:?}\n{out}"
            );
        }
    }
}

/// `vscode:` и `file:` — только ссылке отчёта: у картинки, видео и аудио (в том числе сырым
/// HTML) адреса нет; рендер заметки этих схем не знает вовсе.
#[test]
fn report_schemes_only_on_links_and_only_in_reports() {
    let src = "![a](file:///C:/a.png) ![b](vscode://file/b.png)\n\n\
               <img src=\"file:///C:/c.png\" alt=\"c\"> <img src=\"VSCODE://file/d.png\">\n\n\
               <video src=\"file:///C:/v.mp4\"></video> <audio src=\" file:///C:/s.mp3\"></audio>\n\n\
               <a href=\"file:///C:/ok.txt\">ok</a> <a href=\"vscode://file/ok.rs\">ok2</a>\n";
    let out = render_report_html(src, &NoResolve).html;
    for (tag, name, value) in attributes(&out) {
        assert!(
            name != "src" || value.is_empty(),
            "{tag}[src]={value}\n{out}"
        );
    }
    // сырой HTML тела рендер не пропускает вовсе (comrak без `unsafe`): ссылок из него нет
    assert!(!out.contains("ok.txt") && !out.contains("ok.rs"), "{out}");
    // санитайзер отчёта сам по себе (HTML, собранный вне comrak): схемы сверх обычных — только
    // у `<a href>`, в том числе с регистром и ведущим пробелом
    let raw = "<img src=\"file:///C:/c.png\" alt=\"c\"><img src=\"VSCODE://file/d.png\">\
               <video src=\"file:///C:/v.mp4\"></video><audio src=\" file:///C:/s.mp3\"></audio>\
               <a href=\"file:///C:/ok.txt\">ok</a><a href=\"vscode://file/ok.rs\">ok2</a>\
               <a href=\"JaVaScRiPt:alert(1)\">j</a><a href=\" javascript:alert(1)\">s</a>\
               <a href=\"java&#9;script:alert(1)\">t</a><a href=\"data:text/html,x\">d</a>";
    let out = sanitize_report_html(raw);
    assert_safe(raw, &out);
    assert_report_schemes(raw, &out);
    for (tag, name, value) in attributes(&out) {
        assert!(name != "src", "{tag}[src]={value}\n{out}");
    }
    assert!(out.contains("href=\"file:///C:/ok.txt\""), "{out}");
    assert!(out.contains("href=\"vscode://file/ok.rs\""), "{out}");
    assert_eq!(out.matches("href=").count(), 2, "{out}");
    // текстовые атрибуты с двоеточием — не адреса: подпись и заголовок остаются
    let out = sanitize_report_html(
        "<img src=\"a.png\" alt=\"Figure: loss\" title=\"note: x\">\
         <a href=\"https://e.org\" title=\"file: имя\">t</a>",
    );
    for keep in [
        "src=\"a.png\"",
        "alt=\"Figure: loss\"",
        "title=\"note: x\"",
        "title=\"file: имя\"",
    ] {
        assert!(out.contains(keep), "{keep}\n{out}");
    }
    let out = render_report_html("[t](https://e.org \"note: see\")\n", &NoResolve).html;
    assert!(out.contains("title=\"note: see\""), "{out}");

    // заметка: как до M6 — `vscode:` и `file:` без href, `mailto:` без rel
    let note = "[v](vscode://file/x.rs) [f](file:///C:/a.txt) [m](mailto:a@b.c) \
                [h](https://e.org)\n\n\
                <a href=\"vscode://file/y.rs\">y</a> <a href=\"file:///C:/z.txt\">z</a>\n";
    let out = render_html(note, &NoResolve).html;
    for (tag, name, value) in attributes(&out) {
        assert!(
            !value.contains("vscode:") && !value.contains("file:"),
            "{tag}[{name}]={value}\n{out}"
        );
    }
    assert!(out.contains("href=\"mailto:a@b.c\">m</a>"), "{out}");
    assert!(
        out.contains("href=\"https://e.org\" rel=\"noopener noreferrer\""),
        "{out}"
    );
    assert_eq!(
        sanitize_html("<a href=\"vscode://file/y.rs\">y</a>"),
        "<a>y</a>"
    );
    assert_eq!(
        sanitize_report_html("<a href=\"vscode://file/y.rs\">y</a><a href=\"ftp://x\">f</a>"),
        "<a href=\"vscode://file/y.rs\">y</a><a>f</a>"
    );
}
