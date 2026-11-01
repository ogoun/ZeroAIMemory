import type { Plugin } from 'vite';

/** Плагин сборки: данные pdf.js в `dist/<dir>/` (см. pdfjs-plugin.mjs). */
export function pdfjsAssets(o: {
  dir: string;
  version: string;
  files: (list: (dir: string) => string[]) => [string, string][];
}): Plugin;
