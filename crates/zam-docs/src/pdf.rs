//! PDF через pdfium (спецификация M5 §1–2): только в рабочем процессе `zam-server doc-worker`.
//!
//! pdfium (C++) нельзя прервать посреди вызова, и он регулярно получает CVE переполнения кучи,
//! поэтому эти функции вызываются только в отдельном процессе с лимитом памяти и сроком.
//!
//! - Текст — постранично: символы страницы (`FPDFText`) с прямоугольниками → строки в порядке
//!   чтения по XY-cut ([`crate::pdf_layout`]); единица — страница, `Locator::Page(n)`.
//! - Колонтитулы — строки в верхних/нижних 8 % высоты, повторяющиеся на ≥ 60 % страниц.
//! - Страница без текста с изображением — `ocr_needed` (OCR — вне M5).
//! - JavaScript документа не исполняется (форма инициализируется без обработчиков, действия
//!   документа не вызываются, сборка pdfium — без V8), вложения PDF не читаются.
//! - Лимиты: `max_bytes` (отказ), `max_pages`, `max_chars` (весь накопленный текст страниц
//!   вместе с путями и служебным весом единиц, как у прочих форматов), срок `deadline` —
//!   частичный результат `truncated: <лимит>`; символов страницы — не больше
//!   [`PAGE_GLYPHS_MAX`] (`truncated: page_chars`).

use std::path::Path;
use std::time::Instant;

use pdfium_render::prelude::{PdfPage, PdfPageObject, PdfPageObjectsCommon, PdfRenderConfig};

pub use pdfium_render::prelude::Pdfium;

use crate::ctx::{Ctx, Units};
use crate::pdf_layout::{self, Clock, Glyph, PageText};
use crate::{DocError, DocKind, Extracted, Limits, Locator};

/// Символов одной страницы, не больше: прямоугольники символов страницы держатся в памяти
/// целиком (20 байт на символ) для раскладки.
pub const PAGE_GLYPHS_MAX: usize = 262_144;
/// Объектов страницы, которые просматриваются в поиске изображения (страница без текста).
const PAGE_OBJECTS_SCAN: usize = 100_000;
/// Глубина вложенности форм XObject в поиске изображения.
const FORM_DEPTH: u8 = 4;
/// Наибольшая сторона миниатюры, пикселей.
pub const THUMB_MAX_PX: u32 = 2048;

/// Загружает pdfium из `path`. Библиотека привязывается один раз на процесс (ограничение
/// `pdfium-render`): повторный вызов — ошибка.
pub fn bind(path: &Path) -> Result<Pdfium, String> {
    let bindings = Pdfium::bind_to_library(path).map_err(|e| format!("{e:?}"))?;
    Ok(Pdfium::new(bindings))
}

fn pdf_err(e: pdfium_render::prelude::PdfiumError) -> DocError {
    DocError::Malformed(format!("pdf: {e:?}"))
}

/// Извлекает текст PDF постранично в пределах `limits` и срока `deadline`.
pub fn extract_pdf(
    lib: &Pdfium,
    bytes: &[u8],
    limits: &Limits,
    deadline: Instant,
) -> Result<Extracted, DocError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(DocError::TooLarge);
    }
    let doc = lib.load_pdf_from_byte_slice(bytes, None).map_err(pdf_err)?;
    let mut ctx = Ctx::new(limits, deadline);
    let mut clock = Clock::new(deadline);
    let pages = doc.pages();
    let total = u32::try_from(pages.len()).unwrap_or(0);
    let take = total.min(limits.max_pages);
    if total > limits.max_pages {
        ctx.truncated("max_pages");
    }
    // накопленный, но ещё не выданный текст страниц (учёт `max_chars`, как в OOXML)
    let mut pending = 0u64;
    let mut collected: Vec<(u32, PageText)> = Vec::new();
    let mut ocr_needed = false;
    for i in 0..take {
        if Instant::now() >= deadline {
            ctx.stop("timeout_ms");
            break;
        }
        let page = match pages.get(i as i32) {
            Ok(p) => p,
            Err(_) => {
                ctx.warn(format!("skipped: page {}", i + 1));
                continue;
            }
        };
        let left = limits.max_chars.saturating_sub(pending);
        let cap = usize::try_from(left)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .min(PAGE_GLYPHS_MAX);
        let glyphs = page_glyphs(&page, cap, &mut ctx, &mut clock);
        if glyphs.len() >= PAGE_GLYPHS_MAX {
            ctx.truncated("page_chars");
        }
        let visible = glyphs
            .iter()
            .any(|g| !g.c.is_whitespace() && !g.c.is_control());
        if !visible {
            if !ocr_needed && has_image(&page, &mut clock) {
                ocr_needed = true;
            }
            continue;
        }
        let (bottom, top) = page_span(&page);
        let rows = pdf_layout::page_rows(&glyphs, &mut clock);
        drop(glyphs);
        let mut text = pdf_layout::join_rows(&rows, bottom, top);
        drop(rows);
        // учёт в `max_chars`: обрезка — по остатку, разбор останавливается
        let mut kept = String::new();
        ctx.append(&mut kept, &text.text, &mut pending);
        if kept.len() < text.text.len() {
            text.edges.retain(|(r, _)| r.end <= kept.len());
            text.text = kept;
        }
        collected.push((i + 1, text));
        if ctx.chars_full() || clock.expired() {
            if clock.expired() {
                ctx.stop("timeout_ms");
            }
            break;
        }
    }
    let repeated = pdf_layout::repeated_edges(collected.iter().map(|(_, p)| p), take as usize);
    let mut units = Units::default();
    // текст уже учтён в `pending`; `push` считает выдачу заново, с путём и служебным весом
    let mut out_ctx = Ctx::new(limits, deadline);
    for (no, page) in collected {
        let text = pdf_layout::strip_edges(page, &repeated);
        units.push(Locator::Page(no), Vec::new(), text, &mut out_ctx);
        if out_ctx.stopped() {
            ctx.truncated("max_chars");
            break;
        }
    }
    let units = units.finish(&mut out_ctx);
    let mut warnings = ctx.into_warnings();
    for w in out_ctx.into_warnings() {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    Ok(Extracted {
        kind: DocKind::Pdf,
        units,
        warnings,
        ocr_needed,
    })
}

/// Символы страницы в порядке потока, не больше `cap`.
fn page_glyphs(page: &PdfPage<'_>, cap: usize, ctx: &mut Ctx, clock: &mut Clock) -> Vec<Glyph> {
    let Ok(text) = page.text() else {
        return Vec::new();
    };
    let chars = text.chars();
    let n = chars.len();
    let mut out = Vec::with_capacity(n.min(cap).min(16_384));
    for i in 0..n {
        if out.len() >= cap {
            if cap < PAGE_GLYPHS_MAX {
                ctx.truncated("max_chars");
            }
            break;
        }
        if clock.expired() {
            break;
        }
        let Ok(ch) = chars.get(i) else {
            continue;
        };
        let Some(c) = char::from_u32(ch.unicode_value()) else {
            continue;
        };
        let (x0, y0, x1, y1) = match ch.tight_bounds() {
            Ok(r) => (
                r.left().value,
                r.bottom().value,
                r.right().value,
                r.top().value,
            ),
            Err(_) => (0.0, 0.0, 0.0, 0.0),
        };
        out.push(Glyph { c, x0, y0, x1, y1 });
    }
    out
}

/// Вертикальные границы страницы: `MediaBox`, иначе `[0, высота]`.
fn page_span(page: &PdfPage<'_>) -> (f32, f32) {
    match page.boundaries().media() {
        Ok(b) => (b.bounds.bottom().value, b.bounds.top().value),
        Err(_) => (0.0, page.height().value),
    }
}

/// На странице есть изображение (в том числе внутри форм XObject).
fn has_image(page: &PdfPage<'_>, clock: &mut Clock) -> bool {
    let objects = page.objects();
    let n = objects.len().min(PAGE_OBJECTS_SCAN);
    let mut seen = 0usize;
    (0..n).any(|i| {
        objects
            .get(i)
            .is_ok_and(|o| object_has_image(&o, FORM_DEPTH, &mut seen, clock))
    })
}

fn object_has_image(o: &PdfPageObject<'_>, depth: u8, seen: &mut usize, clock: &mut Clock) -> bool {
    *seen += 1;
    if *seen > PAGE_OBJECTS_SCAN || clock.expired() {
        return false;
    }
    match o {
        PdfPageObject::Image(_) => true,
        PdfPageObject::XObjectForm(f) if depth > 0 => {
            let n = f.len().min(PAGE_OBJECTS_SCAN);
            (0..n).any(|i| {
                f.get(i)
                    .is_ok_and(|c| object_has_image(&c, depth - 1, seen, clock))
            })
        }
        _ => false,
    }
}

/// Миниатюра страницы `page` (с 1) в PNG: большая сторона — `max_px` (1..=[`THUMB_MAX_PX`]).
pub fn render_thumb(
    lib: &Pdfium,
    bytes: &[u8],
    page: u32,
    max_px: u32,
) -> Result<Vec<u8>, DocError> {
    let doc = lib.load_pdf_from_byte_slice(bytes, None).map_err(pdf_err)?;
    let index = page
        .checked_sub(1)
        .and_then(|p| i32::try_from(p).ok())
        .ok_or_else(|| DocError::Malformed("pdf: страницы нет".into()))?;
    let pages = doc.pages();
    if index >= pages.len() {
        return Err(DocError::Malformed("pdf: страницы нет".into()));
    }
    let p = pages.get(index).map_err(pdf_err)?;
    let px = max_px.clamp(1, THUMB_MAX_PX) as i32;
    let cfg = PdfRenderConfig::new()
        .set_maximum_width(px)
        .set_maximum_height(px)
        .set_target_width(px);
    let bitmap = p.render_with_config(&cfg).map_err(pdf_err)?;
    let (w, h) = (bitmap.width().max(0) as u32, bitmap.height().max(0) as u32);
    let rgba = bitmap.as_rgba_bytes();
    crate::png::encode_rgba(w, h, &rgba).map_err(|e| DocError::Internal(format!("png: {e}")))
}
