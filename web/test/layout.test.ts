// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Ширины вёрстки WebUI (M7.2, спецификация §5.1 п. 1): один источник — `src/lib/layout.ts`. В CSS
// переменные в `@media`/`@container` не работают, поэтому числа повторяются — и этот тест пропускает
// в стилях только ширины из `layout.ts`; `matchMedia`/`MediaQuery` с шириной — только через `MEDIA`.
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  BOARD_MIN,
  BP_DESKTOP,
  BP_NARROW,
  BP_TABLET,
  BP_WIDE,
  MEDIA,
  SPLIT_MIN,
  widthClass,
} from '../src/lib/layout.ts';

const SRC = fileURLToPath(new URL('../src/', import.meta.url));

/** Файлы `src/**` с расширением из `exts`. */
function files(exts: string[]): string[] {
  return (readdirSync(SRC, { recursive: true }) as string[])
    .filter((f) => exts.some((e) => f.endsWith(e)))
    .map((f) => join(SRC, f));
}

/** Пара «граница − 1 / граница»: `max-width: 719px` и `min-width: 720px`. */
const pair = (bp: number) => [bp - 1, bp];

/** Ширины в прелюдиях `@media`/`@container` файла: `{rule, width, unit, line}`. */
function widths(text: string) {
  const out: { rule: string; width: number; unit: string; line: number }[] = [];
  const at = /@(media|container)\b([^{;]*)[{;]/g;
  for (const m of text.matchAll(at)) {
    const prelude = m[2];
    const line = text.slice(0, m.index).split('\n').length;
    // `(max-width: 719px)`, `(min-width: 720px)`, `(width < 720px)`, `(720px <= width)`
    const w = /(?:(?:min-|max-)?width\s*:\s*([\d.]+)([a-z%]*))|(?:width\s*[<>]=?\s*([\d.]+)([a-z%]*))|(?:([\d.]+)([a-z%]*)\s*[<>]=?\s*width)/g;
    for (const x of prelude.matchAll(w)) {
      const num = x[1] ?? x[3] ?? x[5];
      const unit = x[2] ?? x[4] ?? x[6] ?? '';
      out.push({ rule: m[1], width: Number(num), unit, line });
    }
  }
  return out;
}

test('media_widths_from_layout', () => {
  const media = new Set([BP_NARROW, BP_TABLET, BP_DESKTOP, BP_WIDE].flatMap(pair));
  const container = new Set([SPLIT_MIN, BOARD_MIN].flatMap(pair));
  const bad: string[] = [];
  const sheets = files(['.svelte', '.css']);
  assert.ok(sheets.length > 20, `стили не найдены в ${SRC}`);
  for (const f of sheets) {
    for (const w of widths(readFileSync(f, 'utf8'))) {
      const allowed = w.rule === 'media' ? media : container;
      if (w.unit !== 'px' || !allowed.has(w.width)) {
        bad.push(`${relative(SRC, f)}:${w.line}: @${w.rule} ${w.width}${w.unit}`);
      }
    }
  }
  assert.deepEqual(
    bad,
    [],
    `ширины вне layout.ts (@media — ${[...media].join('/')}, @container — ${[...container].join('/')})`,
  );
});

/** Медиазапросы строкой в коде: `matchMedia('…')` и `new MediaQuery('…')` (svelte/reactivity). */
function mediaCalls(text: string): { api: string; query: string }[] {
  return [...text.matchAll(/\b(matchMedia|MediaQuery)\(\s*(['"`])([^'"`]*)\2/g)].map((m) => ({ api: m[1], query: m[3] }));
}

test('match_media_widths_from_layout', () => {
  // ширина в медиазапросе кода — только из MEDIA/BP_* layout.ts, не строкой в компоненте
  const bad: string[] = [];
  for (const f of files(['.svelte', '.ts'])) {
    if (f.endsWith(join('lib', 'layout.ts'))) continue;
    for (const c of mediaCalls(readFileSync(f, 'utf8'))) {
      if (/width/.test(c.query)) bad.push(`${relative(SRC, f)}: ${c.api}('${c.query}')`);
    }
  }
  assert.deepEqual(bad, []);
});

test('media_calls_parser_sees_media_query', () => {
  const got = mediaCalls(
    "matchMedia('(max-width: 999px)'); new MediaQuery('max-width: 600px'); new MediaQuery(MEDIA.phone);",
  );
  assert.deepEqual(got, [
    { api: 'matchMedia', query: '(max-width: 999px)' },
    { api: 'MediaQuery', query: 'max-width: 600px' },
  ]);
});

test('width_class_boundaries', () => {
  assert.equal(widthClass(360), 'phone');
  assert.equal(widthClass(719), 'phone');
  assert.equal(widthClass(720), 'tablet');
  assert.equal(widthClass(1023), 'tablet');
  assert.equal(widthClass(1024), 'desktop');
  assert.equal(widthClass(1440), 'desktop');
  assert.deepEqual(MEDIA, {
    phone: '(max-width: 719px)',
    drawer: '(max-width: 1023px)',
    coarse: '(pointer: coarse)',
  });
});

test('widths_parser_sees_ranges_and_units', () => {
  // сам разбор: диапазонный синтаксис и единицы не проходят мимо проверки
  const got = widths(
    '@media (max-width: 999px) {}\n@media screen and (prefers-color-scheme: dark) {}\n' +
      '@container (width >= 840px) {}\n@media (40em <= width) {}\n@media print {}',
  );
  assert.deepEqual(
    got.map((w) => `${w.rule}:${w.width}${w.unit}`),
    ['media:999px', 'container:840px', 'media:40em'],
  );
});
