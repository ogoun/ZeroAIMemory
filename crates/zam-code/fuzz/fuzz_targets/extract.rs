//! Фазз-цель: `extract` на случайных байтах не паникует (хук паники libfuzzer срабатывает до
//! `catch_unwind` экстрактора) и соблюдает пределы протокола. Язык — первый байт входа.

#![no_main]

use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use zam_code::{Lang, Limits, NAME_MAX_CHARS, ParseState, QUAL_MAX_CHARS, extract};

fuzz_target!(|data: &[u8]| {
    let Some((&first, src)) = data.split_first() else {
        return;
    };
    let lang = Lang::ALL[first as usize % Lang::ALL.len()];
    let lim = Limits { parse_timeout: Duration::from_millis(100), ..Limits::default() };
    let out = extract(lang, src, &lim);
    if out.state == ParseState::Skipped {
        assert!(out.symbols.is_empty() && out.refs.is_empty());
    }
    assert!(out.symbols.len() <= lim.max_symbols && out.refs.len() <= lim.max_refs);
    for s in &out.symbols {
        assert!(!s.name.is_empty() && s.name.chars().count() <= NAME_MAX_CHARS);
        assert!(s.qual.chars().count() <= QUAL_MAX_CHARS);
        assert!(s.line >= 1 && s.end_line >= s.line && s.body_hash.len() == 16);
        assert!(s.sig.as_ref().is_none_or(|g| g.chars().count() <= zam_code::SIG_MAX_CHARS));
    }
    for r in &out.refs {
        assert!(!r.name.is_empty() && r.count >= 1 && r.first_line >= 1);
    }
});
