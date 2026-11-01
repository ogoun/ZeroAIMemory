// Плагин сборки: данные pdf.js для документа просмотрщика PDF (M5) — воркер, CMap (CJK),
// декодеры WebAssembly и профиль ICC — файлами в dist/pdfjs/ вместе с их лицензиями. Список и
// причины выбора — src/viewer-pdf/assets.ts. Модуль на JavaScript с объявлением типов рядом
// (pdfjs-plugin.d.mts): vite.config.ts проверяется без типов Node.
import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/**
 * @param {{ dir: string, version: string, files: (list: (dir: string) => string[]) => [string, string][] }} o
 * @returns {import('vite').Plugin}
 */
export function pdfjsAssets(o) {
  const root = fileURLToPath(new URL('../node_modules/pdfjs-dist/', import.meta.url));
  return {
    name: 'zam-pdfjs-assets',
    apply: 'build',
    generateBundle() {
      const { version } = JSON.parse(readFileSync(`${root}package.json`, 'utf8'));
      if (version !== o.version) this.error(`pdfjs-dist ${version}, а PDFJS_VERSION — ${o.version}`);
      for (const [from, to] of o.files((d) => readdirSync(`${root}${d}`))) {
        this.emitFile({ type: 'asset', fileName: `${o.dir}/${to}`, source: readFileSync(`${root}${from}`) });
      }
    },
  };
}
