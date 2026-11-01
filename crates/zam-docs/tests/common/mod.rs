//! Общие помощники тестов `zam-docs`: сборка zip в памяти и подделка его заголовков.

#![allow(dead_code)]

pub mod sheets;

use std::io::Write;

use zip::write::SimpleFileOptions;

pub const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;

/// Zip из записей; `stored` — имена, которые пишутся без сжатия.
pub fn zip_with(entries: &[(&str, Vec<u8>)], stored: &[&str]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        let method = if stored.contains(name) {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        let o = SimpleFileOptions::default()
            .compression_method(method)
            .large_file(data.len() >= u32::MAX as usize);
        z.start_file(*name, o).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

/// Zip из записей со сжатием deflate.
pub fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    zip_with(entries, &[])
}

fn u16_at(b: &[u8], i: usize) -> usize {
    usize::from(u16::from_le_bytes([b[i], b[i + 1]]))
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

fn eocd(b: &[u8]) -> usize {
    (0..=b.len() - 22)
        .rev()
        .find(|&i| b[i..i + 4] == *b"PK\x05\x06")
        .expect("EOCD")
}

/// Записи центрального каталога: (смещение, длина, имя).
fn central(b: &[u8]) -> Vec<(usize, usize, Vec<u8>)> {
    let e = eocd(b);
    let (mut at, end) = (u32_at(b, e + 16) as usize, e);
    let mut out = Vec::new();
    while at + 46 <= end && b[at..at + 4] == *b"PK\x01\x02" {
        let (n, m, k) = (u16_at(b, at + 28), u16_at(b, at + 30), u16_at(b, at + 32));
        let len = 46 + n + m + k;
        out.push((at, len, b[at + 46..at + 46 + n].to_vec()));
        at += len;
    }
    out
}

/// Увеличивает заявленный `compressed_size` записи `name` на `extra` в локальном и центральном
/// заголовках (запись без дескриптора данных, без zip64).
pub fn forge_compressed(b: &mut [u8], name: &str, extra: u32) {
    for (at, _, n) in central(b) {
        if n == name.as_bytes() {
            let v = u32_at(b, at + 20) + extra;
            b[at + 20..at + 24].copy_from_slice(&v.to_le_bytes());
            let local = u32_at(b, at + 42) as usize;
            assert_eq!(&b[local..local + 4], b"PK\x03\x04");
            let v = u32_at(b, local + 18) + extra;
            b[local + 18..local + 22].copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// Пересобирает центральный каталог: `edit` получает записи (байты) и возвращает новый набор.
fn rewrite_central(b: &[u8], edit: impl FnOnce(Vec<Vec<u8>>) -> Vec<Vec<u8>>) -> Vec<u8> {
    let e = eocd(b);
    let cd = central(b);
    let start = cd.first().map_or(e, |c| c.0);
    let records = edit(
        cd.iter()
            .map(|(at, len, _)| b[*at..at + len].to_vec())
            .collect(),
    );
    let mut out = b[..start].to_vec();
    let mut size = 0usize;
    for r in &records {
        size += r.len();
        out.extend_from_slice(r);
    }
    let mut tail = b[e..].to_vec();
    let count = u16::try_from(records.len()).unwrap().to_le_bytes();
    tail[8..10].copy_from_slice(&count);
    tail[10..12].copy_from_slice(&count);
    tail[12..16].copy_from_slice(&u32::try_from(size).unwrap().to_le_bytes());
    out.extend_from_slice(&tail);
    out
}

/// Убирает запись `name` из центрального каталога: её данные остаются «дырой» в файле.
pub fn drop_central(b: &[u8], name: &str) -> Vec<u8> {
    rewrite_central(b, |rs| {
        rs.into_iter()
            .filter(|r| &r[46..46 + u16_at(r, 28)] != name.as_bytes())
            .collect()
    })
}

/// Добавляет в центральный каталог копию записи `name` под именем `new_name` той же длины:
/// две записи указывают на одни данные (бомба Фифилда) или повторяют имя.
pub fn dup_central(b: &[u8], name: &str, new_name: &str) -> Vec<u8> {
    assert_eq!(name.len(), new_name.len());
    rewrite_central(b, |mut rs| {
        let mut copy = rs
            .iter()
            .find(|r| &r[46..46 + u16_at(r, 28)] == name.as_bytes())
            .expect("запись")
            .clone();
        copy[46..46 + name.len()].copy_from_slice(new_name.as_bytes());
        rs.push(copy);
        rs
    })
}
