//! Миниатюры изображений для превью (M5, задача 8): PNG, JPEG, GIF, WebP → PNG.
//!
//! Только в рабочем процессе `zam-server doc-worker` (лимит памяти и срок процесса). Файл
//! недоверенный, поэтому:
//! - вид — по сигнатуре ([`crate::sniff_image`]), формат декодеру задаётся явно, без угадывания;
//! - размеры читаются из заголовка **до** декодирования: сторона не больше [`IMAGE_MAX_SIDE`],
//!   пикселей не больше [`IMAGE_MAX_PIXELS`]; сверх того декодеру задан `image::Limits`
//!   (`max_image_width/height`, `max_alloc` = [`IMAGE_MAX_ALLOC`]) — бомба размеров (PNG
//!   100 000 × 100 000 в сотне байт) отклоняется без выделения памяти;
//! - декодированный буфер — не больше `IMAGE_MAX_ALLOC`; 16-битные и прочие виды пикселей
//!   переводятся в 8 бит (RGBA ≤ 4 × `IMAGE_MAX_PIXELS` = 200 МБ); ресайз (`fast_image_resize`,
//!   Lanczos3, альфа учитывается) пишет в буфер миниатюры ≤ 1024 × 1024 × 4;
//! - результат всегда перекодируется в свой PNG: исходные байты (метаданные, полиглоты) наружу
//!   не попадают. Ориентация EXIF применяется. GIF и WebP — первый кадр;
//! - сжатые служебные чанки PNG (`iCCP`, `zTXt`, `iTXt`) вырезаются до декодирования: декодер
//!   раскрывал бы их целиком (бомба сжатия в профиле цвета).
//!
//! Пик памяти — декодированный буфер, его 8-битная копия и миниатюра с PNG; замер —
//! `tests/memory_peak.rs` (`image_thumb_peak_within_bound`).

use std::io::Cursor;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, IntoImageView, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageReader,
};

use crate::{DocError, ImageKind, sniff_image};

/// Наибольшая сторона миниатюры, пикселей (параметр `px` превью).
pub const THUMB_MAX_PX: u32 = 1024;
/// Наибольшая сторона исходного изображения, пикселей.
pub const IMAGE_MAX_SIDE: u32 = 16_384;
/// Пикселей исходного изображения (50 Мпикс: фото 8160 × 6120 проходит).
pub const IMAGE_MAX_PIXELS: u64 = 50_000_000;
/// Предел выделения декодера `image`, байт: 16-битный RGBA на `IMAGE_MAX_PIXELS` — 400 МБ.
pub const IMAGE_MAX_ALLOC: u64 = 512 * 1024 * 1024;

fn bad(m: impl std::fmt::Display) -> DocError {
    DocError::Malformed(format!("image: {m}"))
}

fn format_of(k: ImageKind) -> ImageFormat {
    match k {
        ImageKind::Png => ImageFormat::Png,
        ImageKind::Jpeg => ImageFormat::Jpeg,
        ImageKind::Gif => ImageFormat::Gif,
        ImageKind::Webp => ImageFormat::WebP,
    }
}

fn reader(bytes: &[u8], f: ImageFormat) -> ImageReader<Cursor<&[u8]>> {
    let mut r = ImageReader::with_format(Cursor::new(bytes), f);
    let mut l = image::Limits::default();
    l.max_image_width = Some(IMAGE_MAX_SIDE);
    l.max_image_height = Some(IMAGE_MAX_SIDE);
    l.max_alloc = Some(IMAGE_MAX_ALLOC);
    r.limits(l);
    r
}

/// Размеры миниатюры: большая сторона не больше `max_px`, мелкое не растягивается.
fn fit(w: u32, h: u32, max_px: u32) -> (u32, u32) {
    let side = w.max(h);
    if side <= max_px {
        return (w, h);
    }
    let scale = |v: u32| {
        let s = (u64::from(v) * u64::from(max_px) + u64::from(side) / 2) / u64::from(side);
        u32::try_from(s).unwrap_or(max_px).clamp(1, max_px)
    };
    (scale(w), scale(h))
}

/// Миниатюра изображения PNG/JPEG/GIF/WebP в PNG, большая сторона — не больше `max_px`
/// (1..=[`THUMB_MAX_PX`]). Не изображение, превышение размеров или порча — `Malformed`.
pub fn image_thumb(bytes: &[u8], max_px: u32) -> Result<Vec<u8>, DocError> {
    if !(1..=THUMB_MAX_PX).contains(&max_px) {
        return Err(bad(format!("px вне 1..={THUMB_MAX_PX}")));
    }
    let kind = sniff_image(bytes).ok_or_else(|| bad("не PNG, JPEG, GIF или WebP"))?;
    let f = format_of(kind);
    // сжатые служебные чанки PNG (профиль цвета, текст) декодер раскрывает целиком
    let stripped = (kind == ImageKind::Png).then(|| strip_png_chunks(bytes));
    let bytes = stripped.as_deref().unwrap_or(bytes);
    // размеры из заголовка — до декодирования
    let (w, h) = reader(bytes, f).into_dimensions().map_err(bad)?;
    if w == 0 || h == 0 || w > IMAGE_MAX_SIDE || h > IMAGE_MAX_SIDE {
        return Err(bad(format!("размеры {w}×{h} вне предела {IMAGE_MAX_SIDE}")));
    }
    if u64::from(w) * u64::from(h) > IMAGE_MAX_PIXELS {
        return Err(bad(format!("{w}×{h} больше {IMAGE_MAX_PIXELS} пикселей")));
    }
    let mut dec = reader(bytes, f).into_decoder().map_err(bad)?;
    let orientation = dec.orientation().ok();
    let mut img = DynamicImage::from_decoder(dec).map_err(bad)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    // 8 бит на канал: так проще кодер и предсказуем буфер
    let img = match img {
        DynamicImage::ImageLuma8(_)
        | DynamicImage::ImageLumaA8(_)
        | DynamicImage::ImageRgb8(_)
        | DynamicImage::ImageRgba8(_) => img,
        other if other.color().has_alpha() => DynamicImage::ImageRgba8(other.into_rgba8()),
        other => DynamicImage::ImageRgb8(other.into_rgb8()),
    };
    let color = match &img {
        DynamicImage::ImageLuma8(_) => ExtendedColorType::L8,
        DynamicImage::ImageLumaA8(_) => ExtendedColorType::La8,
        DynamicImage::ImageRgb8(_) => ExtendedColorType::Rgb8,
        _ => ExtendedColorType::Rgba8,
    };
    let (sw, sh) = (img.width(), img.height());
    let (tw, th) = fit(sw, sh, max_px);
    let mut out = Vec::new();
    let enc = PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilter::Adaptive);
    if (tw, th) == (sw, sh) {
        enc.write_image(img.as_bytes(), sw, sh, color)
            .map_err(bad)?;
        return Ok(out);
    }
    let pt = img
        .pixel_type()
        .ok_or_else(|| bad("вид пикселей не поддерживается"))?;
    let mut dst = Image::new(tw, th, pt);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&img, &mut dst, &opts)
        .map_err(|e| bad(format!("ресайз: {e}")))?;
    drop(img);
    enc.write_image(dst.buffer(), tw, th, color).map_err(bad)?;
    Ok(out)
}

/// Сжатые служебные чанки PNG, которые декодер `png` распаковывает целиком в память до
/// пикселей: профиль цвета `iCCP` и текст `zTXt`/`iTXt`. PNG в 1 МБ с `iCCP` из гигабайта нулей
/// занимал 529 МиБ (ревью задачи 8 M5, M-4). Миниатюре они не нужны (цвет — sRGB по умолчанию).
const PNG_DROP: [&[u8; 4]; 3] = [b"iCCP", b"zTXt", b"iTXt"];

/// Копия PNG без чанков [`PNG_DROP`]: простой проход по чанкам (длина, тип, данные, CRC; CRC не
/// проверяется — это делает декодер). Неполный или неверный по длине чанк — остаток копируется
/// как есть: ошибку покажет декодер.
fn strip_png_chunks(png: &[u8]) -> Vec<u8> {
    const SIG: usize = 8;
    let mut out = Vec::with_capacity(png.len());
    out.extend_from_slice(&png[..SIG.min(png.len())]);
    let mut at = SIG;
    while at < png.len() {
        let Some(head) = png.get(at..at + 8) else {
            break;
        };
        let len = u32::from_be_bytes(head[..4].try_into().expect("4 байта")) as usize;
        let Some(end) = (at + 12).checked_add(len).filter(|e| *e <= png.len()) else {
            break;
        };
        if !PNG_DROP.iter().any(|t| head[4..8] == t[..]) {
            out.extend_from_slice(&png[at..end]);
        }
        at = end;
    }
    out.extend_from_slice(&png[at.min(png.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{fit, strip_png_chunks};

    #[test]
    fn png_chunks_stripped() {
        let chunk = |t: &[u8; 4], d: &[u8]| {
            let mut v = (d.len() as u32).to_be_bytes().to_vec();
            v.extend_from_slice(t);
            v.extend_from_slice(d);
            v.extend_from_slice(&[0, 0, 0, 0]);
            v
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(chunk(b"IHDR", &[1; 13]));
        png.extend(chunk(b"iCCP", &[2; 100]));
        png.extend(chunk(b"IDAT", &[3; 5]));
        png.extend(chunk(b"zTXt", &[4; 7]));
        png.extend(chunk(b"IEND", &[]));
        let s = strip_png_chunks(&png);
        assert_eq!(s.len(), png.len() - (12 + 100) - (12 + 7));
        assert!(!s.windows(4).any(|w| w == b"iCCP" || w == b"zTXt"));
        // обрезанный чанк — остаток как есть
        let cut = &png[..png.len() - 3];
        assert_eq!(strip_png_chunks(cut).len(), cut.len() - 112 - 19);
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit(300, 100, 64), (64, 21));
        assert_eq!(fit(100, 300, 64), (21, 64));
        assert_eq!(fit(10, 30, 64), (10, 30));
        assert_eq!(fit(16_384, 1, 1024), (1024, 1));
        assert_eq!(fit(1024, 1024, 1024), (1024, 1024));
    }
}
