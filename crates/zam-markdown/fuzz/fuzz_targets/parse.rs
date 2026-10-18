//! Фазз-цель: `parse` не паникует, все диапазоны лежат в исходнике на границах символов.

#![no_main]

use std::ops::Range;

use libfuzzer_sys::fuzz_target;
use zam_markdown::{LineIndex, first_paragraph, outline, parse};

fn check(src: &str, r: &Range<usize>) {
    assert!(r.start <= r.end && r.end <= src.len(), "{r:?} вне 0..{}", src.len());
    assert!(src.is_char_boundary(r.start) && src.is_char_boundary(r.end), "{r:?}");
}

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let doc = parse(src);
    assert!(doc.body_start <= src.len() && src.is_char_boundary(doc.body_start));
    for s in &doc.sections {
        check(src, &s.range);
        check(src, &s.body);
        assert!(s.range.start <= s.body.start && s.body.end == s.range.end);
    }
    for h in &doc.headings {
        check(src, &h.range);
    }
    for l in &doc.links {
        check(src, &l.range);
    }
    let li = LineIndex::new(src);
    for line in 1..=li.line_count() {
        let off = li.line_start(line);
        assert!(off <= src.len() && li.line_of(off) == line);
    }
    let _ = outline(&doc);
    let _ = first_paragraph(src, &doc);
});
