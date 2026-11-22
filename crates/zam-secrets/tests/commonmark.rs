//! Сверка участков кода [`zam_secrets::code`] с эталоном CommonMark — comrak (той же версии, что
//! у `zam-markdown`; только `dev-dependencies`) — круг правок 5 задачи 5 M7.1.
//!
//! - **строго:** каждая непустая строка содержимого fenced-блока comrak входит в
//!   [`mask_code_regions`] (маска секретов не теряет кода против CommonMark), и каждый участок
//!   круга 2 ([`code_regions_round2`]) — тоже (не теряет и против круга 2);
//! - **мера:** расхождения [`code_regions`] (блоки) с comrak построчно в обе стороны — печатаются
//!   (числа — в журнале этапа);
//! - фиксированные случаи ревьюеров кругов 3–5: шесть примеров круга 4, семь примеров сдвига пар
//!   (N1), 35 случаев генератора ревьюера, где маска круга 4 оставляла секрет, который comrak
//!   считает кодом (N2: 23 — базовый набор атомов, 12 — расширенный).
use std::ops::Range;

use comrak::nodes::NodeValue;
use zam_secrets::code::{
    CodeKind, CodeRegion, code_regions, code_regions_round2, fence_blocks, lines_of,
    mask_code_regions,
};
use zam_secrets::mask_text;

/// Фиктивный пароль собирается из частей (push protection).
fn pw() -> String {
    ["S3c", "ret", "9x"].concat()
}

/// Индексы строк (с 0) содержимого fenced-блоков comrak: строки после открывающей, по числу
/// строк содержимого (закрывающая не входит).
fn cm_content_lines(t: &str) -> Vec<usize> {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, t, &comrak::Options::default());
    let mut out = Vec::new();
    for n in root.descendants() {
        let d = n.data.borrow();
        if let NodeValue::CodeBlock(cb) = &d.value
            && cb.fenced
        {
            let open = d.sourcepos.start.line - 1;
            // строк содержимого: последняя — и без перевода строки в конце текста; концы строк
            // литерала — любые (`\n`, `\r\n`, `\r`)
            let lit = &cb.literal;
            let k = if lit.is_empty() {
                0
            } else {
                let n = lines_of(lit).len();
                if lit.ends_with(['\n', '\r']) {
                    n - 1
                } else {
                    n
                }
            };
            out.extend(open + 1..=open + k);
        }
    }
    out
}

/// По comrak: вхождений `needle` в коде (блок, инлайн) и вне кода (текст, строка информации).
fn cm_counts(t: &str, needle: &str) -> (usize, usize) {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, t, &comrak::Options::default());
    let (mut code, mut prose) = (0, 0);
    for n in root.descendants() {
        let d = n.data.borrow();
        match &d.value {
            NodeValue::CodeBlock(cb) => {
                code += cb.literal.matches(needle).count();
                prose += cb.info.matches(needle).count();
            }
            NodeValue::Code(c) => code += c.literal.matches(needle).count(),
            NodeValue::Text(s) => prose += s.matches(needle).count(),
            _ => {}
        }
    }
    (code, prose)
}

/// Значимая часть строки `line` (без отступа, маркеров цитаты и хвостовых пробелов) или `None`.
fn body(t: &str, line: &Range<usize>) -> Option<Range<usize>> {
    let s = &t[line.clone()];
    let lead = s.len() - s.trim_start_matches([' ', '\t', '>']).len();
    let tail = s.len() - s.trim_end().len();
    (lead + tail < s.len()).then(|| line.start + lead..line.end - tail)
}

/// `r` целиком покрыт объединением участков `regions` (по порядку, без пересечений).
fn covered(regions: &[CodeRegion], r: &Range<usize>) -> bool {
    let mut pos = r.start;
    for g in regions {
        if g.range.end <= pos {
            continue;
        }
        if g.range.start > pos {
            return false;
        }
        pos = g.range.end;
        if pos >= r.end {
            return true;
        }
    }
    pos >= r.end
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Атомы генератора сверки: списки разной вложенности и маркеров (в том числе с табуляцией),
/// заборы ```` ``` ````/`~~~`/```` ```` ```` на разных отступах и строках маркера, цитаты (и
/// вложенные), табуляции, `\r`, `\r\n`, инлайн-код и текст.
const ATOMS: &[&str] = &[
    "- ",
    "  - ",
    "    - ",
    "* ",
    "+ ",
    "1. ",
    "   1. ",
    "10) ",
    "-   ",
    "-\t",
    "1.\t",
    " ",
    "  ",
    "   ",
    "    ",
    "      ",
    "\t",
    "```",
    "```sh",
    "~~~",
    "````",
    "> ",
    ">",
    "> > ",
    "текст",
    "x",
    "# h",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n\n",
    "\r",
    "\r\n",
    "`",
];

/// Итог сверки на `n` текстах генератора с зерном `seed`.
struct Report {
    texts: u64,
    cm_lines: u64,
    cm_not_masked: u64,
    r2_not_masked: u64,
    cm_not_code: u64,
    code_not_cm: u64,
    code_lines: u64,
    examples: Vec<String>,
}

fn cross_check(n: u64, seed: u64) -> Report {
    let mut rep = Report {
        texts: 0,
        cm_lines: 0,
        cm_not_masked: 0,
        r2_not_masked: 0,
        cm_not_code: 0,
        code_not_cm: 0,
        code_lines: 0,
        examples: Vec::new(),
    };
    let mut rng = Rng(seed);
    for _ in 0..n {
        let k = rng.pick(30);
        let mut t = String::new();
        for _ in 0..k {
            t.push_str(ATOMS[rng.pick(ATOMS.len())]);
        }
        rep.texts += 1;
        let lines = lines_of(&t);
        let mask = mask_code_regions(&t);
        let code: Vec<CodeRegion> = code_regions(&t)
            .into_iter()
            .filter(|r| r.kind == CodeKind::Fenced)
            .collect();
        let cm = cm_content_lines(&t);
        for &li in &cm {
            let Some(b) = lines.get(li).and_then(|l| body(&t, &l.content)) else {
                continue;
            };
            rep.cm_lines += 1;
            if !covered(&mask, &b) {
                rep.cm_not_masked += 1;
                if rep.examples.len() < 10 {
                    rep.examples.push(format!("строка {li}: {t:?}"));
                }
            }
            if !covered(&code, &b) {
                rep.cm_not_code += 1;
            }
        }
        for r in code_regions_round2(&t) {
            if !covered(&mask, &r.range) {
                rep.r2_not_masked += 1;
                if rep.examples.len() < 10 {
                    rep.examples.push(format!("круг 2 {:?}: {t:?}", r.range));
                }
            }
        }
        for (li, l) in lines.iter().enumerate() {
            if let Some(b) = body(&t, &l.content)
                && covered(&code, &b)
            {
                rep.code_lines += 1;
                if !cm.contains(&li) {
                    rep.code_not_cm += 1;
                }
            }
        }
    }
    rep
}

fn print(rep: &Report) {
    let pct = |a: u64, b: u64| 100.0 * a as f64 / b.max(1) as f64;
    println!(
        "сверка с comrak: текстов {}, строк содержимого fenced-блоков comrak {}; не в маске — {}, \
         участков круга 2 не в маске — {}; code_regions: строк comrak не в блоках — {} ({:.2} %), \
         строк блоков не у comrak — {} из {} ({:.2} %)",
        rep.texts,
        rep.cm_lines,
        rep.cm_not_masked,
        rep.r2_not_masked,
        rep.cm_not_code,
        pct(rep.cm_not_code, rep.cm_lines),
        rep.code_not_cm,
        rep.code_lines,
        pct(rep.code_not_cm, rep.code_lines)
    );
}

/// Строго: маска секретов покрывает каждую строку содержимого fenced-блоков comrak и каждый
/// участок круга 2 — на 20 000 текстах генератора; мера расхождений `code_regions` с comrak —
/// печатается.
#[test]
fn mask_code_regions_cover_commonmark_and_round2() {
    let rep = cross_check(20_000, 0x5EED_0005_C0DE_CAFE);
    print(&rep);
    assert_eq!(rep.cm_not_masked, 0, "{:#?}", rep.examples);
    assert_eq!(rep.r2_not_masked, 0, "{:#?}", rep.examples);
}

/// То же на 300 000 текстах (числа журнала). Release: `cargo test -p zam-secrets --release --test
/// commonmark -- --ignored --nocapture`.
#[test]
#[ignore]
fn mask_code_regions_cover_commonmark_large() {
    let rep = cross_check(300_000, 0x5EED_0005_C0DE_F00D);
    print(&rep);
    assert_eq!(rep.cm_not_masked, 0, "{:#?}", rep.examples);
    assert_eq!(rep.r2_not_masked, 0, "{:#?}", rep.examples);
}

/// Блоки [`fence_blocks`]: `(первая строка, последняя, закрыт)`.
fn blocks(t: &str) -> Vec<(usize, usize, bool)> {
    let l = lines_of(t);
    fence_blocks(t, &l)
        .iter()
        .map(|b| (b.open, b.last, b.closed))
        .collect()
}

/// N1 (круг правок 5): забор на строке маркера закрывается забором на колонке содержимого
/// пункта — блоки как у comrak; проза после блока — проза, пары следующих заборов не сдвигаются
/// (секрет в коде маскируется).
#[test]
fn marker_line_fence_closes_at_item_column() {
    let pw = pw();
    let t = format!("- a\n  - ```\n    x\n    ```\n\nПроза: mysql -p{pw} в тексте\n");
    assert_eq!(blocks(&t), [(1, 3, true)]);
    assert_eq!(cm_counts(&t, &pw), (0, 1));
    let t = format!("10) ```\n    x\n    ```\n\nПроза: mysql -p{pw}\n");
    assert_eq!(blocks(&t), [(0, 2, true)]);
    for (t, want) in [
        (
            format!(
                "- a\n  - ```\n    x\n    ```\n\n```\na\n```\n\n```\nmysql -uroot -p{pw}\n```\n"
            ),
            vec![(1, 3, true), (5, 7, true), (9, 11, true)],
        ),
        (
            format!("10) ```\n    x\n    ```\n\n```\na\n```\n\n```\nmysql -uroot -p{pw}\n```\n"),
            vec![(0, 2, true), (4, 6, true), (8, 10, true)],
        ),
        (
            format!("-   ```sh\n    x\n    ```\n\n```\na\n```\n\n```\nmysql -uroot -p{pw}\n```\n"),
            vec![(0, 2, true), (4, 6, true), (8, 10, true)],
        ),
        (
            format!("-\t```sh\n    x\n    ```\n\n```\na\n```\n\n```\nmysql -uroot -p{pw}\n```\n"),
            vec![(0, 2, true), (4, 6, true), (8, 10, true)],
        ),
        (
            format!(
                "1. a\n   1. ```\n      x\n      ```\n\n```\na\n```\n\n```\ncurl -u admin:{pw} https://h\n```\n"
            ),
            vec![(1, 3, true), (5, 7, true), (9, 11, true)],
        ),
        (
            format!(
                "- a\n  - ~~~\n    x\n    ~~~\n\n~~~\na\n~~~\n\n~~~\nmysql -uroot -p{pw}\n~~~\n"
            ),
            vec![(1, 3, true), (5, 7, true), (9, 11, true)],
        ),
        (
            format!(
                "   1. ```\n      x\n      ```\n\n```\na\n```\n\n```\nmysql -uroot -p{pw}\n```\n"
            ),
            vec![(0, 2, true), (4, 6, true), (8, 10, true)],
        ),
        (
            format!("- a\n  - ```\n    mysql -p{pw}\n  ```\n"),
            vec![(1, 2, false), (3, 4, false)],
        ),
    ] {
        assert_eq!(blocks(&t), want, "{t:?}");
        let m = mask_text(&t);
        assert!(!m.contains(&pw), "{t:?} → {m:?}");
        assert_eq!(mask_text(&m), m);
    }
}

/// N2 (круг правок 5): строка-забор левее колонки пункта закрывает пункт и открывает новый блок
/// (как у comrak): секрет после неё — код; и 35 случаев генератора ревьюера (маска круга 4
/// оставляла секрет, который comrak считает кодом): маска оставляет не больше, чем comrak видит
/// вне кода.
#[test]
fn fence_left_of_item_opens_new_block() {
    let pw = pw();
    for t in [
        format!("1. ~~~\n~~~\nmysql -p{pw}\n"),
        format!("- ```\n```\nmysql -p{pw}\n"),
        format!("- a\n  ```\n  mysql -p{pw}\n```\nmysql -p{pw}\n"),
    ] {
        assert_eq!(cm_counts(&t, &pw).1, 0, "comrak: всё — код: {t:?}");
        let m = mask_text(&t);
        assert!(!m.contains(&pw), "{t:?} → {m:?}");
    }
    for case in N2_CASES {
        let t = case.replace("{pw}", &pw);
        let (_, prose) = cm_counts(&t, &pw);
        let left = mask_text(&t).matches(&pw).count();
        assert!(
            left <= prose,
            "открыто {left}, вне кода у comrak {prose}: {t:?}"
        );
    }
}

/// Шесть примеров круга 4 и прочие случаи пробы ревьюера: маска их закрывает (как круг 2).
#[test]
fn round4_examples_stay_masked() {
    let pw = pw();
    for t in [
        format!("- Подключение:\n  ```\nmysql -uroot -p{pw}\n  ```\n"),
        format!("1. Run:\n   ```sh\n  mysql -uroot -p{pw}\n   ```"),
        format!("1. Run:\n   ```sh\nmysql -uroot -p{pw}\n   ```"),
        format!("- Шаг:\n  ```sh\n  cd /x\nmysql -uroot -p{pw}\n  ```"),
        format!("- a\n> цитата\n  ```\nmysql -p{pw}\n  ```"),
        format!("- a\n>   ```\n> mysql -p{pw}\n> ```"),
        format!("- ```sh\nmysql -p{pw}\n```"),
        format!("-\t```sh\n  mysql -p{pw}\n  ```"),
        format!("10) ```\nmysql -p{pw}\n```"),
        format!("- a\n  > заметка\n\n    ```sh\n    mysql -p{pw}\n    ```"),
    ] {
        let m = mask_text(&t);
        assert!(!m.contains(&pw), "{t:?} → {m:?}");
        assert_eq!(mask_text(&m), m, "повторно: {t:?}");
    }
}

/// Случаи генератора `gen_cmp` пробы ревьюера `t5r4probe` (атомы и зерно — его), где маска
/// круга 4 оставляла секрет, который comrak считает кодом (секрет — `{pw}`). Первые 23 —
/// базовый набор атомов, остальные 12 — расширенный.
const N2_CASES: &[&str] = &[
    "1. ```sh  ~~~\n  curl -u admin:{pw} https://h\n   ```\n    curl -u admin:{pw} https://h10) ```\n* \n\nтекстcurl -u admin:{pw} https://h> ",
    "> \n10) ~~~``````\n~~~   \n```curl -u admin:{pw} https://h10)    curl -u admin:{pw} https://hx --password {pw}\n\n\n\n1.     10) ",
    "1. ~~~\n\n~~~\n   \n# h   \n\nmysql -uroot -p{pw}> \n\n   ",
    "\n- ```1. \n\n```\n  curl -u admin:{pw} https://hcurl -u admin:{pw} https://h# h>   x --password {pw}```sh\n      ~~~",
    "1. ```10) \n~~~~~~текст\n```\n# h            # hcurl -u admin:{pw} https://h",
    "10) ~~~\n```\n\n~~~\n# hcurl -u admin:{pw} https://h- curl -u admin:{pw} https://h- \n```sh10) \n\n    # hx --password {pw}",
    "1. ~~~\n~~~\n# h\t```текстcurl -u admin:{pw} https://h    * текст> `",
    "- ```\n\n> \n```\ncurl -u admin:{pw} https://h```  \n   curl -u admin:{pw} https://h```sh",
    "10) ```sh\n```      текст*       ```sh# hтекст\n\n\n\n```\n* # h\n\ncurl -u admin:{pw} https://h",
    "* ```sh\n```      \n\nmysql -uroot -p{pw}      curl -u admin:{pw} https://h\n\n\n\n# h\n> `- \n\n   ",
    "\n* ~~~\n~~~\n> curl -u admin:{pw} https://h1. текст      x --password {pw}```\n# h",
    "1. ~~~1. - \n~~~\n\nтекст```sh\n\n> curl -u admin:{pw} https://h\n   \n\n\n   ",
    "10) ```sh\n```\n\nmysql -uroot -p{pw}x --password {pw}текст1. \n# h",
    "1. ~~~\n\n* \n\n\n\nтекст1.       * \n~~~\n      \n```\tcurl -u admin:{pw} https://h",
    "текст* ```sh\n10)    ~~~    curl -u admin:{pw} https://h```\n\n\n\n```",
    "10) \n\n     ```   mysql -uroot -p{pw}\n~~~      \tтекст-       \n\tx --password {pw}```10) ~~~10)   `",
    "\n\n1. ```sh# h\t- \n\n`\n\n\n\n```\ncurl -u admin:{pw} https://h\n  ",
    "-     \n\n   \n\n* ~~~>     \t\n~~~\nmysql -uroot -p{pw}``\n```* ~~~тексттекст- ",
    "10) \n\n\n\n>       10) ```   > x --password {pw}x --password {pw}\n`      \n```sh> ",
    "\n\n- ```sh~~~\n\n\n```\n`      x --password {pw}```sh      ``````текст\n~~~\t```",
    "`\n10) ```sh10)          \n  ```\n\nтекст`10) 1. текст# h\n   mysql -uroot -p{pw}curl -u admin:{pw} https://h",
    "# h      ```sh\n\n\n* ```# h- ~~~- \n\n\n``````\n\n\n10) curl -u admin:{pw} https://h10) > ",
    "1. ~~~1. \n\n~~~\n\ncurl -u admin:{pw} https://h~~~  ",
    "-   ```sh   1. \n```\n-\t# h```sh> mysql -uroot -p{pw}-   -   ",
    "10) ~~~```\n```\n\n~~~\n10) curl -u admin:{pw} https://h```sh```*     \t\n",
    "- ~~~```sh-   \n> - \n~~~\nx --password {pw}\n```\n",
    "- ```-   mysql -uroot -p{pw}mysql -uroot -p{pw}\n# h      \n```\n\nтекст> x --password {pw}      ```sh   1. \t-\t```sh",
    "  - ~~~  - текст\n~~~\n\n\n\n     1.        текстcurl -u admin:{pw} https://h        ```\n1. x --password {pw}",
    "> \t  - 1. -\t   \n\n  - ```sh* > -\t\n\n  - ~~~-       * \n```\nx --password {pw}",
    "* ```sh\n```\nx --password {pw}x --password {pw}   1. \t   1. 1. # h  -   > ",
    "\n-\t```\n\n```\n10) \n`\n\n\nmysql -uroot -p{pw}    -\t",
    "  - ```sh\t\n\n  \n``````\n\nmysql -uroot -p{pw}# hcurl -u admin:{pw} https://h1. `curl -u admin:{pw} https://h\n-       тексттекст-\t",
    "* ~~~\n~~~\n* \n```   10) ```-   \n\n      \n-\tcurl -u admin:{pw} https://h-\t",
    "\n\n\n\n- ```sh\n\n  1. \n```\n```shx --password {pw}\n\nx --password {pw}      - ",
    "-\t```sh# hтекст   \n\n```\n        - -   x --password {pw}-\t\n`-         -   -\tx --password {pw}\n10) x --password {pw}",
];
