//! `zam attach <target-заметки> <файл>` — загрузка вложения потоком.
//!
//! Multipart собирается вручную: префикс (поле `node`, заголовок части `file`),
//! файл как reader, суффикс; длина известна заранее (`Content-Length`, без chunked).
//! Печатается ссылка `_attachments/…` для вставки в Markdown (`--json` — ответ сервера).
use std::io::Read;
use std::path::Path;

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, read_json};

/// Части тела multipart вокруг содержимого файла.
pub struct Multipart {
    pub content_type: String,
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
}

/// Имя файла для заголовка части: кавычки, `\` и управляющие символы недопустимы.
fn check_name(name: &str) -> Result<(), CliError> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return Err(CliError::input(format!(
            "недопустимое имя файла вложения: {name:?}"
        )));
    }
    Ok(())
}

pub fn multipart(boundary: &str, node: &str, filename: &str) -> Multipart {
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\n{node}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    );
    Multipart {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        head: head.into_bytes(),
        tail: format!("\r\n--{boundary}--\r\n").into_bytes(),
    }
}

pub fn run(ctx: &Ctx, raw: &str, file: &Path) -> Result<(), CliError> {
    let filename = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CliError::input(format!("нет имени файла: {}", file.display())))?
        .to_string();
    check_name(&filename)?;
    let f = std::fs::File::open(file)
        .map_err(|e| CliError::input(format!("открытие {}: {e}", file.display())))?;
    let len = f
        .metadata()
        .map_err(|e| CliError::input(format!("{}: {e}", file.display())))?
        .len();
    let loc = ctx.locate(raw)?;
    let node = ctx.node_id(&loc)?;
    // граница — случайный id: в содержимом файла не встречается на практике
    let boundary = format!("zam-{}", zam_core::Id::new());
    let mp = multipart(&boundary, &node, &filename);
    let total = mp.head.len() as u64 + len + mp.tail.len() as u64;
    let body = std::io::Cursor::new(mp.head)
        .chain(f.take(len))
        .chain(std::io::Cursor::new(mp.tail));
    let v = read_json(
        ctx.client
            .send_multipart("/blobs", &mp.content_type, total, body)?,
    )?;
    if ctx.json {
        return emit_json(&v);
    }
    let link = v["link"].as_str().unwrap_or_default();
    emit(format!("{link}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_layout() {
        let mp = multipart("B", "ID", "a b.png");
        let head = String::from_utf8(mp.head).unwrap();
        assert!(head.starts_with(
            "--B\r\nContent-Disposition: form-data; name=\"node\"\r\n\r\nID\r\n--B\r\n"
        ));
        assert!(head.contains("name=\"file\"; filename=\"a b.png\"\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
        assert_eq!(mp.tail, b"\r\n--B--\r\n");
        assert_eq!(mp.content_type, "multipart/form-data; boundary=B");
    }

    #[test]
    fn bad_names() {
        for n in ["", "a\"b", "a\\b", "a\nb"] {
            assert!(check_name(n).is_err(), "{n:?}");
        }
        assert!(check_name("схема 1.png").is_ok());
    }
}
