//! Изображения для тестов миниатюр: кодирование тем же `image` и враждебные заголовки.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

pub fn encode(img: &DynamicImage, f: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, f).unwrap();
    out.into_inner()
}

pub fn gradient(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    }))
}

/// Размеры PNG миниатюры.
pub fn png_info(png: &[u8]) -> (u32, u32) {
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "не PNG");
    let img = image::load_from_memory_with_format(png, ImageFormat::Png).unwrap();
    (img.width(), img.height())
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

pub fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = ty.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// PNG с заявленными размерами `w × h` (серый 8 бит) и одним крошечным IDAT: бомба размеров.
pub fn png_bomb(w: u32, h: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    // zlib пустого потока: заголовок, «последний блок без сжатия» нулевой длины, adler32
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0, 0, 0, 1],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Валидный JPEG `8 × 8` с подменёнными размерами в SOF.
pub fn jpeg_forged(w: u16, h: u16) -> Vec<u8> {
    let mut b = encode(&gradient(8, 8), ImageFormat::Jpeg);
    let sof = b
        .windows(2)
        .position(|p| p == [0xFF, 0xC0])
        .expect("JPEG без SOF0");
    // FF C0, длина (2), точность (1), высота (2), ширина (2)
    b[sof + 5..sof + 7].copy_from_slice(&h.to_be_bytes());
    b[sof + 7..sof + 9].copy_from_slice(&w.to_be_bytes());
    b
}

/// GIF с логическим экраном `w × h` и кадром 1 × 1.
pub fn gif_forged(w: u16, h: u16) -> Vec<u8> {
    let mut b = encode(&gradient(1, 1), ImageFormat::Gif);
    b[6..8].copy_from_slice(&w.to_le_bytes());
    b[8..10].copy_from_slice(&h.to_le_bytes());
    b
}
