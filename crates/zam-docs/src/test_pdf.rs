//! Простой писатель PDF для тестов (фича `test-pdf`, только dev-зависимости): страницы с
//! текстом стандартным шрифтом Helvetica и, по желанию, картинкой; бинарных фикстур в git нет.

/// Строка текста страницы: позиция низа строки (пункты, ось y вверх), кегль, текст.
#[derive(Debug, Clone)]
pub struct Line {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: String,
}

/// Страница: строки в порядке потока содержимого и картинка 2×2 на пол-страницы.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub lines: Vec<Line>,
    pub image: bool,
    /// Размер страницы, пунктов; `None` — US Letter.
    pub media: Option<(f32, f32)>,
}

impl Page {
    /// Добавляет строку кеглем 12.
    pub fn line(mut self, x: f32, y: f32, text: &str) -> Self {
        self.lines.push(Line {
            x,
            y,
            size: 12.0,
            text: text.to_string(),
        });
        self
    }

    /// Добавляет картинку.
    pub fn image(mut self) -> Self {
        self.image = true;
        self
    }
}

/// Ширина и высота страницы (US Letter), пунктов.
pub const PAGE_W: f32 = 612.0;
pub const PAGE_H: f32 = 792.0;

/// Строка PDF в скобках: `\`, `(`, `)` экранируются; байты не-ASCII пишутся как есть (WinAnsi —
/// кириллица и CJK так не кодируются, pdfium должен лишь не падать).
fn pdf_string(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for &b in s.as_bytes() {
        if matches!(b, b'\\' | b'(' | b')') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

/// PDF из страниц.
pub fn build(pages: &[Page]) -> Vec<u8> {
    // объекты: 1 — каталог, 2 — дерево страниц, 3 — шрифт, 4 — картинка, далее по 2 на страницу
    let n = pages.len();
    let mut objs: Vec<Vec<u8>> = Vec::new();
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes());
    objs.push(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    let mut img = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
/BitsPerComponent 8 /Length 4 >>\nstream\n"
        .to_vec();
    img.extend_from_slice(&[0x00, 0xFF, 0xFF, 0x00]);
    img.extend_from_slice(b"\nendstream");
    objs.push(img);
    for (i, p) in pages.iter().enumerate() {
        let mut content = Vec::new();
        if p.image {
            content.extend_from_slice(b"q 300 0 0 300 150 250 cm /Im1 Do Q\n");
        }
        for l in &p.lines {
            content
                .extend_from_slice(format!("BT /F1 {} Tf {} {} Td ", l.size, l.x, l.y).as_bytes());
            content.extend_from_slice(&pdf_string(&l.text));
            content.extend_from_slice(b" Tj ET\n");
        }
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << \
/Font << /F1 3 0 R >> /XObject << /Im1 4 0 R >> >> /Contents {} 0 R >>",
                6 + 2 * i,
                w = p.media.map_or(PAGE_W, |m| m.0),
                h = p.media.map_or(PAGE_H, |m| m.1),
            )
            .into_bytes(),
        );
        let mut s = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        s.extend_from_slice(&content);
        s.extend_from_slice(b"\nendstream");
        objs.push(s);
    }
    assemble(&objs)
}

/// PDF из тел объектов по порядку номеров с 1 (корень — объект 1): заголовок, объекты, таблица
/// `xref` и `trailer` (враждебные PDF тестов собирают свои словари).
pub fn assemble(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (k, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", k + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Путь к pdfium из `cargo xtask fetch-deps` (`target/deps/<платформа>/pdfium/`) относительно
/// каталога крейта `manifest_dir`; `None` — библиотеки нет (тест пропускается).
pub fn pdfium_path(manifest_dir: &str) -> Option<std::path::PathBuf> {
    let (plat, name) = if cfg!(windows) {
        ("windows-x64", "pdfium.dll")
    } else {
        ("linux-x64", "libpdfium.so")
    };
    let p = std::path::Path::new(manifest_dir)
        .join("../../target/deps")
        .join(plat)
        .join("pdfium")
        .join(name);
    p.is_file().then_some(p)
}
