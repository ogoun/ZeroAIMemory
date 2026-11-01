//! Протокол обмена с рабочим процессом `zam-server doc-worker` по stdin/stdout.
//!
//! Кадр — длина тела `u32` big-endian и тело JSON (`serde_json`). Родитель пишет [`Request`],
//! процесс отвечает одним [`Response`] на каждый запрос, по порядку. Файл передаётся в base64
//! внутри JSON (кадр самодостаточен и читается одним `from_slice`). Пустой ввод (EOF на границе
//! кадра) — штатное завершение процесса. Тело длиннее предела стороны — ошибка кадра, процесс
//! перезапускается.
//!
//! Тестовые запросы (`Crash`, `Sleep`, `Alloc`) есть только в сборке с фичей `test-hooks`.

use std::borrow::Cow;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use zam_docs::{DocError, DocKind, Extracted, Limits, Locator, Unit};

/// Записывает кадр: длина `u32` BE и тело.
pub fn write_frame(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "кадр больше 4 ГиБ"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// Читает кадр не длиннее `max` байт; `Ok(None)` — EOF до начала кадра.
pub fn read_frame(r: &mut impl Read, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("кадр {len} байт больше предела {max}"),
        ));
    }
    let mut body = Vec::new();
    body.try_reserve_exact(len)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "нет памяти под кадр"))?;
    r.take(len as u64).read_to_end(&mut body)?;
    if body.len() != len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(body))
}

/// Предел кадра запроса для файла не больше `max_bytes`: base64 (4/3) и 64 КиБ на поля.
pub fn request_frame_max(max_bytes: u64) -> usize {
    usize::try_from(max_bytes.div_ceil(3) * 4 + 64 * 1024).unwrap_or(usize::MAX)
}

/// Предел кадра ответа: до 6 байт JSON на символ выдачи (`\u00XX`) и 64 МиБ на остальное
/// (миниатюра PNG не больше 2048×2048 RGBA в base64).
pub fn response_frame_max(max_chars: u64) -> usize {
    usize::try_from(max_chars.saturating_mul(6) + 64 * 1024 * 1024).unwrap_or(usize::MAX)
}

/// Тело кадра запроса: `req` (с пустым `bytes`) и файл `file` в base64 — без промежуточной
/// строки base64 размером с файл.
pub fn request_body(req: &Request<'_>, file: Option<&[u8]>) -> Vec<u8> {
    use base64::Engine as _;
    let mut body = serde_json::to_vec(req).expect("запрос сериализуется");
    if let Some(file) = file {
        const TAIL: &[u8] = br#""bytes":""}"#;
        debug_assert!(body.ends_with(TAIL), "bytes — последнее поле и пустое");
        body.truncate(body.len() - 2);
        let mut s = String::from_utf8(body).expect("JSON — UTF-8");
        s.reserve(file.len().div_ceil(3) * 4 + 2);
        base64::engine::general_purpose::STANDARD.encode_string(file, &mut s);
        s.push_str("\"}");
        body = s.into_bytes();
    }
    body
}

/// Лимиты разбора в запросе.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireLimits {
    pub max_bytes: u64,
    pub max_unpacked: u64,
    pub max_ratio: u32,
    pub max_entries: u32,
    pub max_xml_depth: u32,
    pub max_cells: u64,
    pub max_chars: u64,
    pub max_pages: u32,
}

impl From<&Limits> for WireLimits {
    fn from(l: &Limits) -> Self {
        WireLimits {
            max_bytes: l.max_bytes,
            max_unpacked: l.max_unpacked,
            max_ratio: l.max_ratio,
            max_entries: l.max_entries,
            max_xml_depth: l.max_xml_depth,
            max_cells: l.max_cells,
            max_chars: l.max_chars,
            max_pages: l.max_pages,
        }
    }
}

impl From<&WireLimits> for Limits {
    fn from(l: &WireLimits) -> Self {
        Limits {
            max_bytes: l.max_bytes,
            max_unpacked: l.max_unpacked,
            max_ratio: l.max_ratio,
            max_entries: l.max_entries,
            max_xml_depth: l.max_xml_depth,
            max_cells: l.max_cells,
            max_chars: l.max_chars,
            max_pages: l.max_pages,
        }
    }
}

/// Запрос рабочему процессу.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request<'a> {
    /// Извлечь структуру документа вида `kind` (`DocKind::as_str`); `soft_ms` — срок разбора
    /// внутри процесса (частичный результат), меньше срока убийства процесса.
    Extract {
        kind: String,
        limits: WireLimits,
        soft_ms: u64,
        /// Последнее поле: [`request_body`] дописывает base64 прямо в тело кадра.
        #[serde(borrow)]
        bytes: Cow<'a, str>,
    },
    /// Миниатюра страницы `page` (с 1) PDF в PNG, большая сторона — `max_px`.
    Thumb {
        page: u32,
        max_px: u32,
        #[serde(borrow)]
        bytes: Cow<'a, str>,
    },
    /// Проверка: процесс жив; загружен ли pdfium.
    Ping,
    /// Тест: аварийное завершение процесса.
    #[cfg(feature = "test-hooks")]
    Crash,
    /// Тест: зависание на `ms` мс.
    #[cfg(feature = "test-hooks")]
    Sleep { ms: u64 },
    /// Тест: выделение и запись `mb` МиБ (выше лимита памяти — OOM).
    #[cfg(feature = "test-hooks")]
    Alloc { mb: u64 },
}

/// Ошибка разбора в ответе.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireError {
    /// `too_large | unsupported | malformed | timeout | internal`.
    pub kind: String,
    pub message: String,
}

impl From<&DocError> for WireError {
    fn from(e: &DocError) -> Self {
        let (kind, message) = match e {
            DocError::TooLarge => ("too_large", String::new()),
            DocError::Unsupported => ("unsupported", String::new()),
            DocError::Malformed(m) => ("malformed", m.clone()),
            DocError::Timeout => ("timeout", String::new()),
            DocError::Internal(m) => ("internal", m.clone()),
        };
        WireError {
            kind: kind.into(),
            message,
        }
    }
}

impl From<WireError> for DocError {
    fn from(e: WireError) -> Self {
        match e.kind.as_str() {
            "too_large" => DocError::TooLarge,
            "unsupported" => DocError::Unsupported,
            "malformed" => DocError::Malformed(e.message),
            "timeout" => DocError::Timeout,
            _ => DocError::Internal(e.message),
        }
    }
}

/// Единица текста в ответе; `locator` — `Locator::render`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireUnit {
    pub locator: String,
    pub heading_path: Vec<String>,
    pub text: String,
}

/// Результат извлечения в ответе.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireExtracted {
    pub kind: String,
    pub units: Vec<WireUnit>,
    pub warnings: Vec<String>,
    pub ocr_needed: bool,
}

impl From<Extracted> for WireExtracted {
    fn from(x: Extracted) -> Self {
        WireExtracted {
            kind: x.kind.as_str().into(),
            units: x
                .units
                .into_iter()
                .map(|u| WireUnit {
                    locator: u.locator.render(),
                    heading_path: u.heading_path,
                    text: u.text,
                })
                .collect(),
            warnings: x.warnings,
            ocr_needed: x.ocr_needed,
        }
    }
}

impl TryFrom<WireExtracted> for Extracted {
    type Error = DocError;

    fn try_from(x: WireExtracted) -> Result<Self, DocError> {
        let kind = parse_kind(&x.kind)
            .ok_or_else(|| DocError::Internal(format!("неизвестный вид {}", x.kind)))?;
        let units = x
            .units
            .into_iter()
            .map(|u| {
                Ok(Unit {
                    locator: Locator::parse(&u.locator)?,
                    heading_path: u.heading_path,
                    text: u.text,
                })
            })
            .collect::<Result<Vec<_>, DocError>>()?;
        Ok(Extracted {
            kind,
            units,
            warnings: x.warnings,
            ocr_needed: x.ocr_needed,
        })
    }
}

/// Вид документа по `DocKind::as_str`.
pub fn parse_kind(s: &str) -> Option<DocKind> {
    [
        DocKind::Pdf,
        DocKind::Docx,
        DocKind::Pptx,
        DocKind::Xlsx,
        DocKind::Xls,
        DocKind::Ods,
        DocKind::Csv,
        DocKind::Text,
    ]
    .into_iter()
    .find(|k| k.as_str() == s)
}

/// Ответ рабочего процесса.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Response {
    Extracted(WireExtracted),
    /// PNG в base64.
    Thumb {
        png: String,
    },
    /// `pdfium` — `None`, если библиотека загружена, иначе причина.
    Pong {
        pdfium: Option<String>,
    },
    /// Ошибка разбора; у `unsupported` из-за pdfium `message` — причина загрузки.
    Error(WireError),
    /// Тест: `Sleep`/`Alloc` выполнены.
    #[cfg(feature = "test-hooks")]
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip_and_limits() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"op\":\"ping\"}").unwrap();
        write_frame(&mut buf, b"").unwrap();
        let mut r = &buf[..];
        assert_eq!(
            read_frame(&mut r, 64).unwrap().unwrap(),
            b"{\"op\":\"ping\"}"
        );
        assert_eq!(read_frame(&mut r, 64).unwrap().unwrap(), b"");
        assert!(read_frame(&mut r, 64).unwrap().is_none());
        // длиннее предела и оборванный кадр — ошибки
        assert!(read_frame(&mut &buf[..], 4).is_err());
        assert!(read_frame(&mut &buf[..6], 64).is_err());
        assert!(read_frame(&mut &buf[..2], 64).is_err());
    }

    #[test]
    fn request_json_shape() {
        let r = Request::Thumb {
            page: 1,
            max_px: 64,
            bytes: Cow::Borrowed(""),
        };
        let body = request_body(&r, Some(b"ABC"));
        let s = std::str::from_utf8(&body).unwrap();
        assert_eq!(s, r#"{"op":"thumb","page":1,"max_px":64,"bytes":"QUJD"}"#);
        let back: Request<'_> = serde_json::from_slice(&body).unwrap();
        let Request::Thumb { bytes, .. } = back else {
            panic!()
        };
        assert_eq!(bytes, "QUJD");
        let x = Request::Extract {
            kind: "pdf".into(),
            limits: WireLimits::from(&Limits::default()),
            soft_ms: 5,
            bytes: Cow::Borrowed(""),
        };
        let body = request_body(&x, Some(&[0u8, 0xFF, 7]));
        assert!(matches!(
            serde_json::from_slice::<Request<'_>>(&body).unwrap(),
            Request::Extract { soft_ms: 5, .. }
        ));
        let e: Response = serde_json::from_str(r#"{"op":"pong","pdfium":null}"#).unwrap();
        assert!(matches!(e, Response::Pong { pdfium: None }));
    }

    #[test]
    fn extracted_roundtrip() {
        let x = Extracted {
            kind: DocKind::Pdf,
            units: vec![Unit {
                locator: Locator::Page(3),
                heading_path: vec![],
                text: "t".into(),
            }],
            warnings: vec!["truncated: max_pages".into()],
            ocr_needed: true,
        };
        let w = WireExtracted::from(x.clone());
        let json = serde_json::to_vec(&Response::Extracted(w)).unwrap();
        let Response::Extracted(w) = serde_json::from_slice(&json).unwrap() else {
            panic!()
        };
        assert_eq!(Extracted::try_from(w).unwrap(), x);
        for e in [
            DocError::TooLarge,
            DocError::Unsupported,
            DocError::Malformed("m".into()),
            DocError::Timeout,
            DocError::Internal("i".into()),
        ] {
            assert_eq!(DocError::from(WireError::from(&e)), e);
        }
    }
}
