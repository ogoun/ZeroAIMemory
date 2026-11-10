// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Ссылки отчёта (M6, задача 15a): схема URL — то же правило, что `zam_core::link::url_scheme`
// (общий набор примеров `crates/zam-core/tests/fixtures/link_schemes.json` читают оба теста), и
// сверка с разбором URL самого движка; путь для кнопки «копировать путь».
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { REPORT_LINK_SCHEMES, linkPath, reportLinkAllowed, urlScheme } from '../src/lib/reportLinks.ts';

interface Vector {
  t: string;
  scheme: string | null;
  report: boolean;
}

const vectors = JSON.parse(
  readFileSync(new URL('../../crates/zam-core/tests/fixtures/link_schemes.json', import.meta.url), 'utf8'),
) as Vector[];

test('REPORT_LINK_SCHEMES — как zam_core::REPORT_LINK_SCHEMES', () => {
  assert.deepEqual([...REPORT_LINK_SCHEMES], ['http', 'https', 'mailto', 'vscode', 'file']);
});

test('urlScheme — общий набор примеров с zam-core', () => {
  assert.ok(vectors.length >= 50, String(vectors.length));
  for (const v of vectors) {
    const what = JSON.stringify(v.t);
    assert.equal(urlScheme(v.t), v.scheme, what);
    const allowed = v.scheme !== null && REPORT_LINK_SCHEMES.includes(v.scheme);
    assert.equal(allowed, v.report, what);
  }
});

test('urlScheme согласна с разбором URL движка (WHATWG)', () => {
  for (const v of vectors) {
    const what = JSON.stringify(v.t);
    let seen: string | null;
    try {
      seen = new URL(v.t, 'http://zam.invalid/').protocol.slice(0, -1);
    } catch {
      seen = null;
    }
    // схемы нет — движок читает адрес относительным (схема базы)
    if (v.scheme === null) assert.equal(seen, 'http', what);
    // схема есть — движок видит её же (или отвергает адрес целиком)
    else assert.ok(seen === null || seen === v.scheme, `${what}: ${seen}`);
  }
});

test('reportLinkAllowed: пять схем и относительные — да, прочее — нет', () => {
  for (const v of vectors) {
    const what = JSON.stringify(v.t);
    if (v.scheme === null) assert.equal(reportLinkAllowed(v.t), true, what);
    else assert.equal(reportLinkAllowed(v.t), v.report, what);
  }
  for (const bad of [
    'javascript:alert(1)',
    'JaVaScRiPt:alert(1)',
    ' javascript:alert(1)',
    'java\tscript:alert(1)',
    'java\nscript:alert(1)',
    '\u0000javascript:alert(1)',
    'data:text/html,<script>alert(1)</script>',
    'ftp://example.org/a',
    'vbscript:x',
    'blob:https://example.org/1',
  ])
    assert.equal(reportLinkAllowed(bad), false, JSON.stringify(bad));
  // адрес, который движок не разбирает, — отказ
  assert.equal(reportLinkAllowed('http://'), false);
});

test('linkPath: file: — путь ОС', () => {
  const cases: [string, string | null][] = [
    ['file:///C:/data/report.csv', 'C:\\data\\report.csv'],
    ['FILE:///c:/Users/u/a%20b.txt', 'c:\\Users\\u\\a b.txt'],
    ['file:///C:/%D0%9C%D0%BE%D0%B8/a.txt', 'C:\\Мои\\a.txt'],
    ['file:///C|/data/a.txt', 'C:\\data\\a.txt'],
    ['file:/C:/data/a.txt', 'C:\\data\\a.txt'],
    ['file:C:/data/a.txt', 'C:\\data\\a.txt'],
    ['file:///C:\\data\\a.txt', 'C:\\data\\a.txt'],
    ['file:///home/u/proj/a.rs', '/home/u/proj/a.rs'],
    ['file://localhost/etc/hosts', '/etc/hosts'],
    ['file:/etc/hosts', '/etc/hosts'],
    ['file://server/share/dir/a.txt', '\\\\server\\share\\dir\\a.txt'],
    ['file:///home/u/a.md#L10', '/home/u/a.md'],
    ['file:///home/u/a.md?x=1', '/home/u/a.md'],
    ['file:///home/u/a%23b.md', '/home/u/a#b.md'],
    ['  file:///home/u/a.md\n', '/home/u/a.md'],
    // битое %-кодирование — как есть
    ['file:///home/u/100%.txt', '/home/u/100%.txt'],
    // пустой путь — копировать нечего
    ['file:///', null],
    ['file:', null],
  ];
  for (const [url, want] of cases) assert.equal(linkPath(url), want, url);
});

test('linkPath: vscode: — путь после vscode://file/', () => {
  const cases: [string, string | null][] = [
    ['vscode://file/C:/proj/src/main.rs:10:5', 'C:/proj/src/main.rs:10:5'],
    ['vscode://file/c:/proj/a%20b.rs', 'c:/proj/a b.rs'],
    ['vscode://file/home/u/proj/a.rs:3', '/home/u/proj/a.rs:3'],
    ['vscode://file//home/u/proj/a.rs', '/home/u/proj/a.rs'],
    ['VSCode://FILE/C:/x.rs', 'C:/x.rs'],
    ['vscode://file/C:/x.rs?windowId=1', 'C:/x.rs'],
    // не файл (команда расширения редактора) — не путь
    ['vscode://vscode.git/clone?url=x', null],
    ['vscode:extension/a.b', null],
    ['vscode://file', null],
    ['vscode://file/', null],
  ];
  for (const [url, want] of cases) assert.equal(linkPath(url), want, url);
});

test('linkPath: управляющие символы и bidi после декодирования — пути нет', () => {
  for (const url of [
    'file:///tmp/x%0Arm%20-rf%20~',
    'file:///tmp/x%0D%0Acalc',
    'file:///tmp/a%00b',
    'file:///tmp/a%1Bb',
    'file:///tmp/%E2%80%AEgpj.exe',
    'file:///tmp/a%E2%80%A8b',
    'vscode://file/C:/x%0Acalc.exe',
    'vscode://file/C:/x%7Fy',
    'file://ho%0Ast/share/a',
  ])
    assert.equal(linkPath(url), null, url);
  // прочие схемы — не путь
  for (const url of ['https://example.org/a', 'mailto:a@b.c', 'figs/a.png', 'javascript:alert(1)', ''])
    assert.equal(linkPath(url), null, url);
});
