import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import license, { type Dependency } from 'rollup-plugin-license';

// Путь от каталога web/ (npm запускает vite из него).
const notices = 'dist/THIRD-PARTY-NOTICES.txt';
const ALLOWED = new Set(['MIT', 'ISC', 'BSD-2-Clause', 'BSD-3-Clause', 'Apache-2.0', '0BSD']);

// Выражение SPDX вида `(A OR B)` допустимо, если допустима хотя бы одна альтернатива
// (DOMPurify: `(MPL-2.0 OR Apache-2.0)` — используется по Apache-2.0).
function allowed(license: string | null): boolean {
  if (!license) return false;
  return license
    .replace(/^\(|\)$/g, '')
    .split(/\s+OR\s+/)
    .some((l) => ALLOWED.has(l.trim()));
}

// Лицензия пакета; если поле license в package.json не задано (svelte-toolbelt, khroma),
// она определяется по первой строке файла LICENSE (MIT или ISC).
function licenseOf(d: Dependency): string | null {
  if (d.license) return d.license;
  const head = d.licenseText?.trimStart() ?? '';
  if (/^(The )?MIT License/.test(head)) return 'MIT';
  if (/^ISC License/.test(head)) return 'ISC';
  return null;
}

function notice(d: Dependency): string {
  const head = [`${d.name}@${d.version}`, `Лицензия: ${licenseOf(d) ?? 'не указана'}`];
  const repo = typeof d.repository === 'string' ? d.repository : d.repository?.url;
  if (repo) head.push(`Репозиторий: ${repo}`);
  const body = [(d.licenseText ?? 'Текст лицензии в пакете не найден.').trim()];
  if (d.noticeText) body.push(d.noticeText.trim());
  return ['='.repeat(72), ...head, '', ...body, ''].join('\n');
}

// Относительные пути ассетов: работает под любым base_path сервера.
export default defineConfig({
  base: './',
  resolve: {
    // elkjs (EPL-2.0) в бандл не входит: раскладка ELK у Mermaid заменена заглушкой.
    alias: [
      {
        find: /^elkjs\/lib\/elk\.bundled\.js$/,
        // путь от корня проекта Vite (web/)
        replacement: '/src/lib/hydrate/elk-stub.ts',
      },
    ],
  },
  plugins: [
    svelte(),
    // Тексты лицензий именно тех пакетов, код которых попал в бандл.
    license({
      thirdParty: {
        includePrivate: false,
        allow: {
          test: (d) => allowed(licenseOf(d)),
          failOnUnlicensed: true,
          failOnViolation: true,
        },
        output: {
          file: notices,
          template: (deps) =>
            [
              'Сторонние компоненты WebUI ZeroAIMemory (код в составе бандла).',
              '',
              ...[...deps].sort((a, b) => (a.name ?? '').localeCompare(b.name ?? '')).map(notice),
            ].join('\n'),
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
      output: {
        // Начальный чанк — с узнаваемым именем: size-limit проверяет только его;
        // KaTeX, Mermaid и DOMPurify — ленивые чанки через import().
        entryFileNames: 'assets/entry-[hash].js',
        // DOMPurify — отдельным файлом (иначе он попадает в общий чанк Mermaid).
        codeSplitting: {
          groups: [{ name: 'dompurify', test: /[\\/]node_modules[\\/]dompurify[\\/]/ }],
        },
      },
    },
  },
});
