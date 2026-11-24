// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Сверка строк стартовой загрузки size-limit с dist/index.html (M7.3, ревью r2 задачи 10a, N1).
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';
import { checkStart, globRe, staticAssets, type LimitLine } from './start-budget.ts';

const HTML = `<!doctype html><head>
<script type="module" crossorigin src="./assets/entry-AB12.js"></script>
<link rel="modulepreload" crossorigin href="./assets/api-CD34.js">
<link rel="modulepreload" crossorigin href="./assets/router-EF56.js">
<link rel="stylesheet" crossorigin href="./assets/entry-XY.css">
</head>`;
const FILES = ['dist/assets/entry-AB12.js', 'dist/assets/api-CD34.js', 'dist/assets/router-EF56.js', 'dist/assets/IdeasView-GH78.js'];
const lines = (start: string[], api = ['dist/assets/api-*.js'], all = [...start, ...api]): LimitLine[] => [
  { name: 'entry', path: 'dist/assets/entry-*.js' },
  { name: 'start (старт без api)', path: start },
  { name: 'api (REST-клиент)', path: api },
  { name: 'start+api (весь статический старт)', path: all },
];

test('start_static_assets_from_index_html', () => {
  assert.deepEqual(staticAssets(HTML), ['dist/assets/api-CD34.js', 'dist/assets/entry-AB12.js', 'dist/assets/router-EF56.js']);
  assert.equal(globRe('dist/assets/viewport.svelte-*.js').test('dist/assets/viewport.svelte-D-YQ.js'), true);
  assert.equal(globRe('dist/assets/time-*.js').test('dist/assets/timeline-x/y.js'), false);
});

test('start_lines_cover_static_chunks', () => {
  assert.deepEqual(checkStart(HTML, lines(['dist/assets/entry-*.js', 'dist/assets/router-*.js']), FILES), []);
  // новый общий чанк старта, не попавший ни в одну строку, — ошибка
  const miss = checkStart(HTML, lines(['dist/assets/entry-*.js']), FILES);
  assert.match(miss.join('\n'), /router-EF56\.js — ни в строке «start», ни в «api»/);
  // шаблон без файлов и шаблон ленивого чанка — ошибки
  const stale = checkStart(HTML, lines(['dist/assets/entry-*.js', 'dist/assets/router-*.js', 'dist/assets/gone-*.js']), FILES);
  assert.match(stale.join('\n'), /gone-\*\.js не находит файлов/);
  const lazy = checkStart(HTML, lines(['dist/assets/entry-*.js', 'dist/assets/router-*.js', 'dist/assets/IdeasView-*.js']), FILES);
  assert.match(lazy.join('\n'), /IdeasView-GH78\.js, а он не грузится при старте/);
  // чанк в двух строках, «start+api» не равна объединению, строки нет
  assert.match(checkStart(HTML, lines(['dist/assets/*.js']), FILES).join('\n'), /и в «start», и в «api»/);
  assert.match(
    checkStart(HTML, lines(['dist/assets/entry-*.js', 'dist/assets/router-*.js'], ['dist/assets/api-*.js'], ['dist/assets/entry-*.js']), FILES).join('\n'),
    /ровно шаблонами/,
  );
  assert.deepEqual(checkStart(HTML, [], FILES).length, 3);
});

test('start_lines_match_current_dist', (t) => {
  // после сборки — настоящие dist/index.html и .size-limit.json (CI: test:build идёт до сборки —
  // тогда сверку делает плагин сборки `zam-start-budget`, vite.config.ts)
  const web = new URL('../', import.meta.url);
  const html = new URL('dist/index.html', web);
  if (!existsSync(html)) {
    t.skip('нет dist/index.html — сверку сделает сборка');
    return;
  }
  const files = readdirSync(new URL('dist/assets/', web)).map((f) => `dist/assets/${f}`);
  const config = JSON.parse(readFileSync(new URL('.size-limit.json', web), 'utf8')) as LimitLine[];
  assert.deepEqual(checkStart(readFileSync(html, 'utf8'), config, files), []);
});
