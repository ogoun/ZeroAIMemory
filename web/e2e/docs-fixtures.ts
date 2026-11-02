// Документы для E2E просмотрщиков (M5): PDF, DOCX, XLSX, PPTX собираются в тесте — бинарных
// фикстур в git нет. Писатели повторяют помощники Rust-тестов: PDF — `zam_docs::test_pdf`
// (Helvetica, WinAnsi), OOXML — `crates/zam-server/tests/common/attach.rs`.
import JSZip from 'jszip';
import { deflateRawSync } from 'node:zlib';

// ---- PDF ----

export interface PdfPage {
  /** Строки сверху вниз (кегль 12). */
  lines?: string[];
  /** Картинка на пол-страницы (страница без текста — «скан»). */
  image?: boolean;
}

const pdfString = (s: string) => `(${s.replace(/[\\()]/g, (c) => `\\${c}`)})`;

/**
 * PDF из страниц US Letter; `js` — враждебное `OpenAction` JavaScript (просмотрщик не должен его
 * исполнять).
 */
export function makePdf(pages: PdfPage[], js?: string): Buffer {
  const objs: Buffer[] = [];
  const n = pages.length;
  const open = js ? ` /OpenAction << /S /JavaScript /JS ${pdfString(js)} >>` : '';
  objs.push(Buffer.from(`<< /Type /Catalog /Pages 2 0 R${open} >>`, 'latin1'));
  const kids = pages.map((_, i) => `${5 + 2 * i} 0 R`).join(' ');
  objs.push(Buffer.from(`<< /Type /Pages /Kids [${kids}] /Count ${n} >>`, 'latin1'));
  objs.push(Buffer.from('<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>', 'latin1'));
  objs.push(
    Buffer.concat([
      Buffer.from(
        '<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 4 >>\nstream\n',
        'latin1',
      ),
      Buffer.from([0x00, 0xff, 0xff, 0x00]),
      Buffer.from('\nendstream', 'latin1'),
    ]),
  );
  pages.forEach((p, i) => {
    let content = p.image ? 'q 300 0 0 300 150 250 cm /Im1 Do Q\n' : '';
    (p.lines ?? []).forEach((l, k) => {
      content += `BT /F1 12 Tf 72 ${720 - k * 18} Td ${pdfString(l)} Tj ET\n`;
    });
    objs.push(
      Buffer.from(
        `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> ` +
          `/XObject << /Im1 4 0 R >> >> /Contents ${6 + 2 * i} 0 R >>`,
        'latin1',
      ),
    );
    const body = Buffer.from(content, 'latin1');
    objs.push(
      Buffer.concat([
        Buffer.from(`<< /Length ${body.length} >>\nstream\n`, 'latin1'),
        body,
        Buffer.from('\nendstream', 'latin1'),
      ]),
    );
  });
  const parts: Buffer[] = [Buffer.from('%PDF-1.4\n%\xE2\xE3\xCF\xD3\n', 'latin1')];
  let len = parts[0].length;
  const offsets: number[] = [];
  objs.forEach((o, k) => {
    offsets.push(len);
    const b = Buffer.concat([Buffer.from(`${k + 1} 0 obj\n`, 'latin1'), o, Buffer.from('\nendobj\n', 'latin1')]);
    parts.push(b);
    len += b.length;
  });
  const xref =
    `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n` +
    offsets.map((o) => `${String(o).padStart(10, '0')} 00000 n \n`).join('') +
    `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${len}\n%%EOF\n`;
  parts.push(Buffer.from(xref, 'latin1'));
  return Buffer.concat(parts);
}

// ---- OOXML ----

const REL = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
const PKG_REL = 'http://schemas.openxmlformats.org/package/2006/relationships';
const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function rels(items: [id: string, type: string, target: string, external?: boolean][]): string {
  return (
    `<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="${PKG_REL}">` +
    items
      .map(
        ([id, ty, target, ext]) =>
          `<Relationship Id="${id}" Type="${ty.includes('/') ? ty : `${REL}/${ty}`}" Target="${esc(target)}"${ext ? ' TargetMode="External"' : ''}/>`,
      )
      .join('') +
    '</Relationships>'
  );
}

const contentTypes = (overrides: [string, string][], defaults: [string, string][] = []) =>
  '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
  '<Default Extension="xml" ContentType="application/xml"/>' +
  '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
  defaults.map(([e, c]) => `<Default Extension="${e}" ContentType="${c}"/>`).join('') +
  overrides.map(([p, c]) => `<Override PartName="${p}" ContentType="${c}"/>`).join('') +
  '</Types>';

async function zip(entries: Record<string, string | Buffer>): Promise<Buffer> {
  const z = new JSZip();
  for (const [name, data] of Object.entries(entries)) z.file(name, data);
  return z.generateAsync({ type: 'nodebuffer', compression: 'DEFLATE' });
}

/** Абзац DOCX: текст, заголовок или внешняя ссылка. */
export type DocxPara = string | { heading: string } | { text: string; link: string };

/**
 * DOCX из абзацев тела; `altChunkHtml` — враждебная HTML-вставка (`w:altChunk`) со скриптом:
 * просмотр не должен её исполнять.
 */
export async function makeDocx(paras: DocxPara[], altChunkHtml?: string): Promise<Buffer> {
  const w = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" ' + `xmlns:r="${REL}"`;
  const docRels: [string, string, string, boolean?][] = [['rIdStyles', 'styles', 'styles.xml']];
  let body = '';
  let link = 0;
  for (const p of paras) {
    if (typeof p === 'string') {
      body += `<w:p><w:r><w:t xml:space="preserve">${esc(p)}</w:t></w:r></w:p>`;
    } else if ('heading' in p) {
      body += `<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>${esc(p.heading)}</w:t></w:r></w:p>`;
    } else {
      const id = `rIdL${++link}`;
      docRels.push([id, 'hyperlink', p.link, true]);
      body += `<w:p><w:hyperlink r:id="${id}"><w:r><w:t>${esc(p.text)}</w:t></w:r></w:hyperlink></w:p>`;
    }
  }
  const files: Record<string, string | Buffer> = {};
  if (altChunkHtml) {
    docRels.push(['rIdAlt', 'aFChunk', 'alt.html']);
    body += '<w:altChunk r:id="rIdAlt"/>';
    files['word/alt.html'] = altChunkHtml;
  }
  const styles =
    `<?xml version="1.0" encoding="UTF-8"?><w:styles ${w}>` +
    '<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr>' +
    '<w:rPr><w:b/><w:sz w:val="36"/></w:rPr></w:style></w:styles>';
  return zip({
    '[Content_Types].xml': contentTypes(
      [
        ['/word/document.xml', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'],
        ['/word/styles.xml', 'application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml'],
      ],
      [['html', 'text/html']],
    ),
    '_rels/.rels': rels([['rId1', 'officeDocument', 'word/document.xml']]),
    'word/_rels/document.xml.rels': rels(docRels),
    'word/document.xml': `<?xml version="1.0" encoding="UTF-8"?><w:document ${w}><w:body>${body}</w:body></w:document>`,
    'word/styles.xml': styles,
    ...files,
  });
}

/** DOCX с DTD и сущностью (XXE) в теле: сервер отказывает в разборе — `failed parse_error`. */
export async function makeBrokenDocx(): Promise<Buffer> {
  const good = await JSZip.loadAsync(await makeDocx(['текст']));
  good.file(
    'word/document.xml',
    '<?xml version="1.0"?><!DOCTYPE w [<!ENTITY x SYSTEM "file:///etc/passwd">]>' +
      '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>&x;</w:t></w:r></w:p></w:body></w:document>',
  );
  return good.generateAsync({ type: 'nodebuffer', compression: 'DEFLATE' });
}

/** XLSX с листами `(имя, строки)`; ячейки — встроенные строки. */
export async function makeXlsx(sheets: [string, string[][]][]): Promise<Buffer> {
  const sml = 'http://schemas.openxmlformats.org/spreadsheetml/2006/main';
  const files: Record<string, string> = {};
  const overrides: [string, string][] = [
    ['/xl/workbook.xml', 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml'],
  ];
  let wb = `<workbook xmlns="${sml}" xmlns:r="${REL}"><sheets>`;
  const wbRels: [string, string, string][] = [];
  sheets.forEach(([name, rows], i) => {
    const n = i + 1;
    wb += `<sheet name="${esc(name)}" sheetId="${n}" r:id="rId${n}"/>`;
    wbRels.push([`rId${n}`, 'worksheet', `worksheets/sheet${n}.xml`]);
    overrides.push([`/xl/worksheets/sheet${n}.xml`, 'application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml']);
    const data = rows
      .map(
        (row, r) =>
          `<row r="${r + 1}">` +
          row
            .map((v, c) => `<c r="${String.fromCharCode(65 + c)}${r + 1}" t="inlineStr"><is><t>${esc(v)}</t></is></c>`)
            .join('') +
          '</row>',
      )
      .join('');
    files[`xl/worksheets/sheet${n}.xml`] =
      `<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="${sml}"><sheetData>${data}</sheetData></worksheet>`;
  });
  wb += '</sheets></workbook>';
  return zip({
    '[Content_Types].xml': contentTypes(overrides),
    '_rels/.rels': rels([['rId1', 'officeDocument', 'xl/workbook.xml']]),
    'xl/workbook.xml': wb,
    'xl/_rels/workbook.xml.rels': rels(wbRels),
    ...files,
  });
}

/** PPTX со слайдами `(заголовок, текст)` по порядку показа. */
export async function makePptx(slides: [string, string][]): Promise<Buffer> {
  const p =
    'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" ' +
    `xmlns:r="${REL}" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"`;
  const files: Record<string, string> = {};
  let ids = '';
  const presRels: [string, string, string][] = [];
  slides.forEach(([title, text], i) => {
    const n = i + 1;
    ids += `<p:sldId id="${255 + n}" r:id="rId${n}"/>`;
    presRels.push([`rId${n}`, 'slide', `slides/slide${n}.xml`]);
    const sp = (id: number, ph: string, t: string) =>
      `<p:sp><p:nvSpPr><p:cNvPr id="${id}" name="S${id}"/><p:cNvSpPr/><p:nvPr><p:ph ${ph}/></p:nvPr></p:nvSpPr>` +
      `<p:txBody><a:p><a:r><a:t>${esc(t)}</a:t></a:r></a:p></p:txBody></p:sp>`;
    files[`ppt/slides/slide${n}.xml`] =
      `<p:sld ${p}><p:cSld><p:spTree>${sp(2, 'type="title"', title)}${sp(3, 'idx="1"', text)}</p:spTree></p:cSld></p:sld>`;
  });
  return zip({
    '[Content_Types].xml': contentTypes([
      ['/ppt/presentation.xml', 'application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml'],
    ]),
    '_rels/.rels': rels([['rId1', 'officeDocument', 'ppt/presentation.xml']]),
    'ppt/presentation.xml': `<p:presentation ${p}><p:sldIdLst>${ids}</p:sldIdLst></p:presentation>`,
    'ppt/_rels/presentation.xml.rels': rels(presRels),
    ...files,
  });
}

/**
 * Zip-бомба под видом DOCX (порт зонда ревью задачи 9): `word/document.xml` из `real` пробелов
 * (сжато — сотни КБ), а в локальном заголовке и центральном каталоге заявлено `claimed` байт.
 * Просмотр в браузере должен отказать по фактическому объёму распаковки, не исчерпав память.
 */
export function makeZipBombDocx(real: number, claimed = 100): Buffer {
  const comp = deflateRawSync(Buffer.alloc(real, 0x20), { level: 9 });
  const name = Buffer.from('word/document.xml');
  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50, 0);
  local.writeUInt16LE(20, 4);
  local.writeUInt16LE(8, 8);
  local.writeUInt32LE(comp.length, 18);
  local.writeUInt32LE(claimed, 22);
  local.writeUInt16LE(name.length, 26);
  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50, 0);
  central.writeUInt16LE(20, 4);
  central.writeUInt16LE(20, 6);
  central.writeUInt16LE(8, 10);
  central.writeUInt32LE(comp.length, 20);
  central.writeUInt32LE(claimed, 24);
  central.writeUInt16LE(name.length, 28);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(1, 8);
  eocd.writeUInt16LE(1, 10);
  eocd.writeUInt32LE(central.length + name.length, 12);
  eocd.writeUInt32LE(local.length + name.length + comp.length, 16);
  return Buffer.concat([local, name, comp, central, name, eocd]);
}
