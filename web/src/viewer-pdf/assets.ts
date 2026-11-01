// Файлы pdf.js рядом с документом просмотрщика PDF (M5): общий список для сборки
// (vite.config.ts копирует их в dist/pdfjs/) и для просмотрщика (адреса воркера и данных).
//
// Что берётся из pdfjs-dist и почему:
// - воркер `pdf.worker.min.mjs` — под именем `.js` (MIME модуля воркера не зависит от того,
//   знает ли сервер `.mjs`) и с версией в имени (кэш браузера не держит старый воркер);
// - `cmaps/*.bcmap` — CMap шрифтов CJK без встроенной кодировки (Adobe, BSD-3-Clause);
// - `wasm/{openjpeg,jbig2,qcms_bg}.wasm` — декодеры JPEG 2000, JBIG2 и цветовых профилей
//   (BSD-2-Clause, BSD-3-Clause PDFium, MIT): ради них CSP просмотрщика — `'wasm-unsafe-eval'`;
// - `iccs/` — профиль CMYK (CC0).
// Не берутся: `wasm/quickjs-eval.*` — песочница JavaScript PDF (скрипты PDF выключены),
// `*_nowasm_fallback.js` — запасные декодеры без WebAssembly, `standard_fonts/` — шрифты
// Liberation под GPL-2.0 с исключением (вместо них — системные шрифты браузера).

/** Версия pdfjs-dist: должна совпадать с package.json (проверка — в pdfjsFiles). */
export const PDFJS_VERSION = '6.4.299';

/** Каталог данных pdf.js в dist/ (относительно `<base href>`). */
export const PDFJS_DIR = 'pdfjs';

/** Имя файла воркера в каталоге данных. */
export const WORKER_FILE = `pdf.worker-${PDFJS_VERSION}.min.js`;

const WASM = ['openjpeg.wasm', 'jbig2.wasm', 'qcms_bg.wasm'];

/**
 * Пары (путь в pdfjs-dist, путь в каталоге данных); `list` — имена файлов каталога пакета.
 * Лицензии копируются рядом с данными.
 */
export function pdfjsFiles(list: (dir: string) => string[]): [string, string][] {
  const out: [string, string][] = [['build/pdf.worker.min.mjs', WORKER_FILE]];
  for (const f of list('cmaps')) if (f.endsWith('.bcmap') || f === 'LICENSE') out.push([`cmaps/${f}`, `cmaps/${f}`]);
  for (const f of list('wasm')) {
    if (WASM.includes(f) || f.startsWith('LICENSE_')) out.push([`wasm/${f}`, `wasm/${f}`]);
  }
  for (const f of list('iccs')) out.push([`iccs/${f}`, `iccs/${f}`]);
  return out;
}
