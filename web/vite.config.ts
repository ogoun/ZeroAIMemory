import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import license, { type Dependency } from 'rollup-plugin-license';
import { allowed, licenseOf } from './build/license-policy';
import { embeddedDeps } from './build/embedded-notices.mjs';
import { pdfjsAssets } from './build/pdfjs-plugin.mjs';
import { PDFJS_DIR, PDFJS_VERSION, pdfjsFiles } from './src/viewer-pdf/assets';

// Путь от каталога web/ (npm запускает vite из него).
// Лицензии бандла проверяет build/license-policy.ts (EPL-2.0 — только elkjs).
const notices = 'dist/THIRD-PARTY-NOTICES.txt';
const SOURCE_URL: Record<string, string> = { elkjs: 'https://github.com/kieler/elkjs' };

function notice(d: Dependency): string {
  const head = [`${d.name}@${d.version}`, `Лицензия: ${licenseOf(d) ?? 'не указана'}`];
  const repo = typeof d.repository === 'string' ? d.repository : d.repository?.url;
  if (repo) head.push(`Репозиторий: ${repo}`);
  // EPL-2.0 §3: исходный код распространяемой версии должен быть доступен
  if (d.name && d.name in SOURCE_URL) head.push(`Исходный код (версия ${d.version}): ${SOURCE_URL[d.name]}`);
  const body = [(d.licenseText ?? 'Текст лицензии в пакете не найден.').trim()];
  if (d.noticeText) body.push(d.noticeText.trim());
  return ['='.repeat(72), ...head, '', ...body, ''].join('\n');
}

// Относительные пути ассетов: работает под любым base_path сервера.
export default defineConfig({
  base: './',
  plugins: [
    svelte(),
    // данные pdf.js (воркер, CMap, WebAssembly, ICC) для /viewer/pdf — без песочницы JS PDF
    pdfjsAssets({ dir: PDFJS_DIR, version: PDFJS_VERSION, files: pdfjsFiles }),
    // Тексты лицензий именно тех пакетов, код которых попал в бандл.
    license({
      thirdParty: {
        includePrivate: false,
        allow: {
          test: (d) => allowed(d),
          failOnUnlicensed: true,
          failOnViolation: true,
        },
        output: {
          file: notices,
          template: (deps) => {
            // пакеты внутри чужих готовых сборок (pako в jszip) — той же политикой лицензий
            const embedded = embeddedDeps() as Dependency[];
            const bad = embedded.filter((d) => !allowed(d));
            if (bad.length) throw new Error(`лицензия не допускается: ${bad.map((d) => d.name).join(', ')}`);
            return [
              'Сторонние компоненты WebUI ZeroAIMemory (код в составе бандла).',
              `Данные pdf.js (воркер, CMap, WebAssembly, ICC) — файлами в ${PDFJS_DIR}/, лицензии рядом с ними.`,
              '',
              ...[...deps, ...embedded].sort((a, b) => (a.name ?? '').localeCompare(b.name ?? '')).map(notice),
            ].join('\n');
          },
        },
      },
    }),
  ],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // Шрифты KaTeX — только файлами: data:-URI шрифтов запрещены CSP (font-src → default-src 'self').
    assetsInlineLimit: (file) => (/\.(woff2?|ttf|otf|eot)$/i.test(file) ? false : undefined),
    rolldownOptions: {
      // Две страницы: WebUI и документ просмотрщика PDF (`/viewer/pdf`, своя CSP — web.rs).
      input: { index: 'index.html', 'viewer-pdf': 'viewer-pdf.html' },
      output: {
        // Начальный чанк — с узнаваемым именем: size-limit проверяет его и ленивые чанки
        // просмотрщиков; KaTeX, Mermaid (и ELK для layout: elk), DOMPurify, просмотрщики
        // документов (docx-preview, jszip) — ленивые чанки через import().
        entryFileNames: (c) => (c.name === 'viewer-pdf' ? 'assets/viewer-pdf-[hash].js' : 'assets/entry-[hash].js'),
        // DOMPurify — отдельным файлом (иначе он попадает в общий чанк Mermaid).
        codeSplitting: {
          groups: [{ name: 'dompurify', test: /[\\/]node_modules[\\/]dompurify[\\/]/ }],
        },
      },
    },
  },
});
