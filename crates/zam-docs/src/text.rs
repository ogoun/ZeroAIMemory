//! Простой текст: абзацы через пустую строку, `Locator::Para(n)` — номер абзаца с 1.

use crate::Locator;
use crate::Unit;
use crate::ctx::{Ctx, Units};

pub(crate) fn extract(bytes: &[u8], ctx: &mut Ctx) -> Vec<Unit> {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(b);
    let mut units = Units::default();
    let mut para = String::new();
    let mut n = 0u32;
    let flush = |para: &mut String, n: &mut u32, units: &mut Units, ctx: &mut Ctx| {
        if !para.trim().is_empty() {
            *n += 1;
            units.block(para, Locator::Para(*n), ctx);
        }
        para.clear();
    };
    for line in text.lines() {
        if !ctx.tick() {
            break;
        }
        if line.trim().is_empty() {
            flush(&mut para, &mut n, &mut units, ctx);
        } else {
            if !para.is_empty() {
                para.push('\n');
            }
            para.push_str(line);
        }
        if ctx.stopped() {
            break;
        }
    }
    flush(&mut para, &mut n, &mut units, ctx);
    units.finish()
}
