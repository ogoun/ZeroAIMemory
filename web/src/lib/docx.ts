// Рендер DOCX для просмотра (M5, Review Focus 5): docx-preview строит DOM из файла, DOMPurify
// чистит его, результат — `srcdoc` sandbox-iframe без allow-scripts и allow-same-origin
// (непрозрачный источник: ни скриптов, ни cookie, ни API WebUI) со своей CSP в `<meta>`
// (только встроенные стили, картинки и шрифты `data:`). Ссылки документа не открываются: у
// внешних снимается `href` (адрес — в подсказке), остаются только якоря `#…`.
//
// Модуль грузится лениво вместе с DocxViewer (docx-preview, jszip — отдельный чанк).
import { renderAsync } from 'docx-preview';
import JSZip from 'jszip';
import DOMPurify from 'dompurify';

/** Пределы файла в браузере: zip-бомба не должна съесть память вкладки. */
export const DOCX_LIMITS = {
  /** Размер файла, байт. */
  maxBytes: 50 * 1024 * 1024,
  /** Записей архива. */
  maxEntries: 10_000,
  /** Сумма фактически распакованных байт всех записей (проверка потоком), байт. */
  maxUnpacked: 256 * 1024 * 1024,
};

/** CSP документа во фрейме: сверх унаследованной политики WebUI — ничего внешнего. */
export const DOCX_FRAME_CSP = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:";

/** Отрисованный документ. */
export interface DocxRender {
  srcdoc: string;
  /** Абзацев тела (нумерация `para:n`). */
  paras: number;
  /** Абзац `para:n` найден и отмечен (`#zam-target`). */
  target: boolean;
}

/** Поток распакованных байт записи jszip (`internalStream` — вне типов пакета). */
interface ChunkStream {
  on(ev: 'data', fn: (c: Uint8Array) => void): ChunkStream;
  on(ev: 'error', fn: (e: Error) => void): ChunkStream;
  on(ev: 'end', fn: () => void): ChunkStream;
  pause(): ChunkStream;
  resume(): ChunkStream;
}

const TOO_BIG = 'архив документа слишком велик для просмотра в браузере, скачайте файл';

/**
 * Распаковывает запись потоком, не накапливая её: число байт; больше `budget` — отказ сразу,
 * поток останавливается. Ложный размер в каталоге jszip отвергает в конце записи.
 */
function drain(f: JSZip.JSZipObject, budget: number): Promise<number> {
  return new Promise((resolve, reject) => {
    let n = 0;
    let done = false;
    const s = (f as unknown as { internalStream(t: string): ChunkStream }).internalStream('uint8array');
    s.on('data', (c) => {
      if (done) return;
      n += c.length;
      if (n > budget) {
        done = true;
        s.pause();
        reject(new Error(TOO_BIG));
      }
    })
      .on('error', (e) => {
        if (done) return;
        done = true;
        reject(new Error(`архив документа повреждён: ${e.message}`));
      })
      .on('end', () => {
        if (done) return;
        done = true;
        resolve(n);
      })
      .resume();
  });
}

/**
 * Проверка архива до разбора docx-preview: размер файла, число записей и **фактический** объём
 * распаковки — потоком по всем записям (размерам центрального каталога верить нельзя: zip-бомба
 * заявляет 100 Б и распаковывается в сотни МиБ; раунд правок 1 задачи 9, I-2).
 */
export async function checkZip(data: ArrayBuffer): Promise<void> {
  if (data.byteLength > DOCX_LIMITS.maxBytes) {
    throw new Error('файл больше 50 МиБ — просмотр в браузере недоступен, скачайте файл');
  }
  const zip = await JSZip.loadAsync(data, { checkCRC32: false });
  const files = Object.values(zip.files).filter((f) => !f.dir);
  if (files.length > DOCX_LIMITS.maxEntries) throw new Error(TOO_BIG);
  let left = DOCX_LIMITS.maxUnpacked;
  for (const f of files) left -= await drain(f, left);
}

/**
 * Абзацы тела в порядке документа — как нумерует `para:n` сервер (`crates/zam-docs/src/docx.rs`):
 * каждый абзац тела без предка-абзаца (в том числе пустые и в ячейках таблиц); колонтитулы и
 * сноски вне `<article>` docx-preview, абзацы надписей вложены в абзац и номера не получают.
 * Расхождения (удалённые правкой абзацы, `mc:Fallback`) — см. журнал этапа, задача 9.
 */
export function bodyParagraphs(root: ParentNode): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('article p')].filter((p) => !p.parentElement?.closest('p'));
}

const COLOR = /^(#[0-9a-f]{3,8}|rgba?\([0-9., /%]+\))$/i;

function frameCss(bg: string): string {
  const back = COLOR.test(bg) ? bg : '#f6f7f9';
  return [
    `html,body{margin:0;background:${back}}`,
    '.docx-wrapper{background:transparent!important;padding:16px 0!important}',
    // overflow:hidden страницы docx-preview делает её контейнером прокрутки, и
    // scroll-initial-target прокручивал бы её, а не документ; clip режет так же, но без прокрутки
    '.docx-wrapper>section.docx{box-shadow:0 1px 4px rgb(0 0 0/.25);overflow:clip!important}',
    '#zam-target{scroll-initial-target:nearest;scroll-margin-top:24px;outline:3px solid #f5c400;' +
      'outline-offset:2px;background:rgb(255 214 10/.28)}',
    'a.zam-link{text-decoration:underline dotted;cursor:help}',
  ].join('\n');
}

/** Санитайзер со своими хуками (общий экземпляр DOMPurify используют диаграммы Mermaid). */
function purifier() {
  const p = DOMPurify(window);
  p.addHook('afterSanitizeAttributes', (node) => {
    const el = node as Element;
    if (el.localName !== 'a') return;
    const href = el.getAttribute('href') ?? '';
    el.removeAttribute('target');
    if (href && !href.startsWith('#')) {
      el.removeAttribute('href');
      el.setAttribute('class', `${el.getAttribute('class') ?? ''} zam-link`.trim());
      el.setAttribute('title', `Ссылка документа (из просмотра не открывается): ${href.slice(0, 300)}`);
    }
  });
  return p;
}

/**
 * DOCX → `srcdoc` фрейма; `para` — абзац из выдачи (`para:n`), он отмечается и прокручивается
 * в видимую часть (`scroll-initial-target`). `background` — цвет фона WebUI.
 */
export async function renderDocx(data: ArrayBuffer, para: number | null, background: string): Promise<DocxRender> {
  await checkZip(data);
  // DOM строится вне документа и не вставляется в него: картинки — data: (useBase64URL),
  // вставки altChunk (HTML внутри DOCX) не отрисовываются вовсе
  const body = document.createElement('div');
  const style = document.createElement('div');
  await renderAsync(data, body, style, {
    inWrapper: true,
    breakPages: true,
    ignoreLastRenderedPageBreak: true,
    useBase64URL: true,
    renderAltChunks: false,
    renderComments: false,
    renderChanges: false,
    experimental: false,
    className: 'docx',
  });
  const paras = bodyParagraphs(body);
  const target = para !== null && para >= 1 && para <= paras.length ? paras[para - 1] : null;
  if (target) target.id = 'zam-target';
  const clean = purifier().sanitize(style.innerHTML + body.innerHTML, {
    FORCE_BODY: true,
    FORBID_TAGS: ['form', 'input', 'button', 'textarea', 'select', 'iframe', 'frame', 'object', 'embed', 'link', 'meta', 'base'],
    FORBID_ATTR: ['srcset', 'action', 'formaction', 'ping'],
    ALLOW_DATA_ATTR: false,
  });
  const srcdoc =
    '<!doctype html><html lang="ru"><head><meta charset="utf-8">' +
    `<meta http-equiv="Content-Security-Policy" content="${DOCX_FRAME_CSP}">` +
    `<style>${frameCss(background.trim())}</style></head><body>${clean}</body></html>`;
  return { srcdoc, paras: paras.length, target: target !== null };
}
