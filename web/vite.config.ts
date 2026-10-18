import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import license, { type Dependency } from 'rollup-plugin-license';

// Путь от каталога web/ (npm запускает vite из него).
const notices = 'dist/THIRD-PARTY-NOTICES.txt';
const ALLOWED = new Set(['MIT', 'ISC', 'BSD-2-Clause', 'BSD-3-Clause', 'Apache-2.0', '0BSD']);

// Лицензия пакета; если поле license в package.json не задано (svelte-toolbelt),
// она определяется по файлу LICENSE.
function licenseOf(d: Dependency): string | null {
  if (d.license) return d.license;
  return d.licenseText?.trimStart().startsWith('MIT License') ? 'MIT' : null;
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
  plugins: [
    svelte(),
    // Тексты лицензий именно тех пакетов, код которых попал в бандл.
    license({
      thirdParty: {
        includePrivate: false,
        allow: {
          test: (d) => ALLOWED.has(licenseOf(d) ?? ''),
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
  build: { outDir: 'dist', emptyOutDir: true },
});
