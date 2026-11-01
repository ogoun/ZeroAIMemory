//! Миниатюры изображений (M5, задача 8): `image` + `fast_image_resize`, лимиты до декодирования.
//!
//! Фикстуры кодируются в тесте тем же `image`; враждебные файлы (PNG с огромными размерами при
//! крошечном файле, JPEG и GIF с поддельными размерами, мусор) собираются вручную.

mod common;

use common::images::{encode, gif_forged, gradient, jpeg_forged, png_bomb, png_info};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use proptest::prelude::*;
use zam_docs::thumb::{IMAGE_MAX_PIXELS, IMAGE_MAX_SIDE, THUMB_MAX_PX, image_thumb};
use zam_docs::{DocError, ImageKind, sniff_image};

#[test]
fn sniff_image_by_signature() {
    let img = gradient(4, 4);
    for (f, k) in [
        (ImageFormat::Png, ImageKind::Png),
        (ImageFormat::Jpeg, ImageKind::Jpeg),
        (ImageFormat::Gif, ImageKind::Gif),
        (ImageFormat::WebP, ImageKind::Webp),
    ] {
        assert_eq!(sniff_image(&encode(&img, f)), Some(k), "{f:?}");
    }
    assert_eq!(sniff_image(b"%PDF-1.7"), None);
    assert_eq!(
        sniff_image(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
        None
    );
    assert_eq!(sniff_image(b""), None);
}

/// Все четыре формата → PNG не больше `px` по большей стороне, пропорции сохранены, мелкое не
/// растягивается.
#[test]
fn thumb_formats_fit_and_keep_aspect() {
    let img = gradient(300, 100);
    for f in [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::Gif,
        ImageFormat::WebP,
    ] {
        let png = image_thumb(&encode(&img, f), 64).unwrap();
        assert_eq!(png_info(&png), (64, 21), "{f:?}");
    }
    let small = image_thumb(&encode(&gradient(10, 30), ImageFormat::Png), 64).unwrap();
    assert_eq!(png_info(&small), (10, 30));
    // прозрачность сохраняется
    let rgba = DynamicImage::ImageRgba8(RgbaImage::from_pixel(40, 40, Rgba([255, 0, 0, 0])));
    let png = image_thumb(&encode(&rgba, ImageFormat::Png), 16).unwrap();
    let back = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(back.dimensions(), (16, 16));
    assert_eq!(back.get_pixel(8, 8)[3], 0);
    // предел px
    let e = image_thumb(&encode(&img, ImageFormat::Png), THUMB_MAX_PX + 1);
    assert!(matches!(e, Err(DocError::Malformed(_))), "{e:?}");
    assert!(image_thumb(&encode(&img, ImageFormat::Png), 0).is_err());
}

/// Размеры проверяются до декодирования: бомбы отклоняются сразу, не выделяя память.
#[test]
fn hostile_dimensions_rejected_before_decode() {
    let t = std::time::Instant::now();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("png 100000²", png_bomb(100_000, 100_000)),
        ("png сторона", png_bomb(IMAGE_MAX_SIDE + 1, 1)),
        ("png пикселей", png_bomb(10_000, 10_000)),
        ("jpeg 65000²", jpeg_forged(65_000, 65_000)),
        ("gif 65535²", gif_forged(65_535, 65_535)),
    ];
    const { assert!(10_000u64 * 10_000 > IMAGE_MAX_PIXELS) };
    for (name, b) in cases {
        let e = image_thumb(&b, 256);
        assert!(matches!(e, Err(DocError::Malformed(_))), "{name}: {e:?}");
    }
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
}

#[test]
fn garbage_and_truncated_are_errors() {
    let good = encode(&gradient(50, 50), ImageFormat::Png);
    let jpg = encode(&gradient(50, 50), ImageFormat::Jpeg);
    let cases: [&[u8]; 5] = [
        b"",
        b"not an image",
        &good[..good.len() / 2],
        &good[..40],
        &jpg[..jpg.len() / 3],
    ];
    for b in cases {
        // обрезанный файл: ошибка или частичное изображение, но не паника
        let _ = image_thumb(b, 64);
    }
    assert!(image_thumb(b"not an image", 64).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Случайная порча байтов валидных файлов — ошибка или PNG, но не паника.
    #[test]
    fn thumb_never_panics(fmt in 0usize..4, flips in proptest::collection::vec((0usize..4096, any::<u8>()), 1..16)) {
        let f = [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::WebP][fmt];
        let mut b = encode(&gradient(37, 23), f);
        for (i, v) in flips {
            let n = b.len();
            b[i % n] = v;
        }
        if let Ok(png) = image_thumb(&b, 32) {
            let (w, h) = png_info(&png);
            prop_assert!(w <= 32 && h <= 32 && w > 0 && h > 0);
        }
    }
}
