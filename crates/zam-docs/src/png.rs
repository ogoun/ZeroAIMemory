//! Минимальный кодер PNG (RGBA 8 бит, фильтр `None`, zlib через `flate2`) для миниатюр PDF:
//! без отдельной зависимости `png`/`image` ради одного формата вывода.

use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;

/// PNG из пикселей RGBA (`w × h × 4` байт, строки сверху вниз).
pub(crate) fn encode_rgba(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let row = (w as usize)
        .checked_mul(4)
        .ok_or_else(|| "слишком широкое изображение".to_string())?;
    if w == 0 || h == 0 || rgba.len() != row * h as usize {
        return Err(format!("размер буфера {} не равен {w}×{h}×4", rgba.len()));
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
    for line in rgba.chunks_exact(row) {
        // тип фильтра строки: 0 (None)
        z.write_all(&[0]).map_err(|e| e.to_string())?;
        z.write_all(line).map_err(|e| e.to_string())?;
    }
    let idat = z.finish().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(idat.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    // 8 бит, цвет 6 (RGBA), сжатие 0, фильтр 0, без чересстрочности
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(ty);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_of_two_pixels() {
        let png = encode_rgba(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&png[12..16], b"IHDR");
        assert!(png.ends_with(&[0xAE, 0x42, 0x60, 0x82]), "CRC IEND");
        assert!(encode_rgba(2, 2, &[0; 8]).is_err());
    }
}
