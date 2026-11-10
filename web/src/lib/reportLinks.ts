// Ссылки тела отчёта (M6, задача 15a; долг M5.5): тело пишет агент — содержимое недоверенное.
// Схема ссылки проверяется тем же правилом, что в `zam report` и на сервере
// (`crates/zam-core/src/link.rs`, общий набор примеров `tests/fixtures/link_schemes.json`), и
// сверяется с разбором URL самого браузера: расхождение или чужая схема — ссылка без `href`.
// `file:` браузер с http(s)-страницы не открывает — ссылка без перехода; `file:` и `vscode:`
// получают кнопку «копировать путь». Разметка строится через DOM (`createElement`,
// `textContent`), без вставки HTML.

/** Схемы внешних ссылок отчёта — как `zam_core::REPORT_LINK_SCHEMES`. */
export const REPORT_LINK_SCHEMES: readonly string[] = ['http', 'https', 'mailto', 'vscode', 'file'];

/** `rel` внешних ссылок. */
export const EXTERNAL_REL = 'noopener noreferrer';
/** Подпись кнопки копирования пути (`data-copy-path` — признак для делегирования клика). */
export const COPY_PATH_LABEL = 'копировать путь';

/** Адрес без того, что отбрасывает браузер: края до U+0020 включительно, TAB/LF/CR — везде. */
function clean(target: string): string {
  let a = 0;
  let b = target.length;
  while (a < b && target.charCodeAt(a) <= 0x20) a++;
  while (b > a && target.charCodeAt(b - 1) <= 0x20) b--;
  return target.slice(a, b).replace(/[\t\n\r]/g, '');
}

/**
 * Схема URL в нижнем регистре, как её прочтёт браузер (WHATWG URL), или `null` — схемы нет.
 * То же правило, что `zam_core::link::url_scheme`: ` javascript:`, `JaVaScRiPt:` и
 * `java\tscript:` — схема `javascript`; `%6Aavascript:` — относительный путь.
 */
export function urlScheme(target: string): string | null {
  const t = clean(target);
  let scheme = '';
  for (let i = 0; i < t.length; i++) {
    const c = t.charCodeAt(i);
    if (c === 0x3a) return scheme || null;
    const lower = c | 0x20;
    if (c < 0x80 && lower >= 0x61 && lower <= 0x7a) scheme += String.fromCharCode(lower);
    else if (scheme && ((c >= 0x30 && c <= 0x39) || c === 0x2b || c === 0x2d || c === 0x2e)) scheme += t[i];
    else return null;
  }
  return null;
}

/** Схема по разбору URL браузера; относительный адрес — `http` (схема базы); `null` — не URL. */
function browserScheme(target: string): string | null {
  try {
    return new URL(target, 'http://zam.invalid/').protocol.slice(0, -1);
  } catch {
    return null;
  }
}

/**
 * Ссылке отчёта можно оставить `href`: схема из списка либо схемы нет — и браузер читает адрес
 * так же (защита от расхождения разборов: при несогласии — отказ).
 */
export function reportLinkAllowed(target: string): boolean {
  const scheme = urlScheme(target);
  const seen = browserScheme(target);
  if (scheme === null) return seen === 'http';
  return REPORT_LINK_SCHEMES.includes(scheme) && seen === scheme;
}

/** Внешняя ссылка: со схемой или `//host` (браузер читает `\` как `/`). */
function isExternal(target: string, scheme: string | null): boolean {
  return scheme !== null || /^[/\\]{2}/.test(clean(target));
}

/** Адрес `vscode:`, открывающий файл: `vscode://file/<путь>[:строка[:столбец]]`. */
const VSCODE_FILE = /^vscode:\/\/file\//i;

/** Символы, которым не место в буфере обмена: управляющие, разделители строк, bidi. */
const UNSAFE_TEXT = /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u200e\u200f\u202a-\u202e\u2066-\u2069]/;

function decode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** Путь без запроса и якоря (`?…`, `#…` — не часть пути файла; в имени они кодируются `%XX`). */
const pathOnly = (s: string) => s.replace(/[?#].*$/s, '');

/**
 * Текст для «копировать путь» или `null` — кнопки нет.
 * - `file:` → путь ОС: `file:///C:/a/b.txt` → `C:\a\b.txt`, `file:///home/u/a` → `/home/u/a`,
 *   `file://server/share/a` → `\\server\share\a`;
 * - `vscode:` → путь после `vscode://file/` (с `:строка:столбец`, если есть); иной адрес
 *   `vscode:` (команда расширения редактора) — не путь, кнопки нет.
 * Путь с управляющими символами после декодирования (`%0A` — вставка в терминал выполнила бы
 * следующую строку) не копируется.
 */
export function linkPath(target: string): string | null {
  const t = clean(target);
  const scheme = urlScheme(t);
  let out: string;
  if (scheme === 'file') {
    let rest = pathOnly(t.slice(5)).replace(/\\/g, '/');
    let host = '';
    if (rest.startsWith('//')) {
      const end = rest.indexOf('/', 2);
      host = decode(end < 0 ? rest.slice(2) : rest.slice(2, end));
      rest = end < 0 ? '' : rest.slice(end);
    }
    const path = decode(rest);
    if (host && host.toLowerCase() !== 'localhost') out = `\\\\${host}${path.replace(/\//g, '\\')}`;
    else if (/^\/?[A-Za-z][:|](\/|$)/.test(path)) out = path.replace(/^\//, '').replace('|', ':').replace(/\//g, '\\');
    else out = path.startsWith('/') ? path : `/${path}`;
  } else if (scheme === 'vscode') {
    if (!VSCODE_FILE.test(t)) return null;
    const p = decode(pathOnly(t.slice(14)));
    out = /^[A-Za-z]:/.test(p) || p.startsWith('/') ? p : `/${p}`;
  } else return null;
  return out === '' || out === '/' || UNSAFE_TEXT.test(out) ? null : out;
}

/**
 * Ссылки отрендеренного тела отчёта (экран и печатная копия):
 * - чужая схема или расхождение с разбором браузера — `href` снимается;
 * - внешним — `rel="noopener noreferrer"`;
 * - `file:` — без `href` (перехода нет), класс `local-link`; так же — `vscode:` не вида
 *   `vscode://file/…`;
 * - `file:` и `vscode:` — `data-url` (адрес текстом в печати, если он не текст самой ссылки) и
 *   кнопка «копировать путь» (`data-copy-path`).
 */
export function hydrateReportLinks(root: HTMLElement): void {
  for (const a of root.querySelectorAll<HTMLAnchorElement>('a[href]')) {
    const raw = a.getAttribute('href') ?? '';
    if (!reportLinkAllowed(raw)) {
      a.removeAttribute('href');
      continue;
    }
    const scheme = urlScheme(raw);
    if (!isExternal(raw, scheme)) continue;
    a.setAttribute('rel', EXTERNAL_REL);
    if (scheme !== 'file' && scheme !== 'vscode') continue;
    const url = clean(raw);
    if ((a.textContent ?? '').trim() !== url) a.dataset.url = url;
    // переход — только у `vscode://file/…` (открыть файл); прочие адреса `vscode:` — команды
    // расширений редактора (`vscode://vscode.git/clone?…`), из недоверенного отчёта не кликаются
    if (scheme === 'file' || !VSCODE_FILE.test(url)) {
      a.removeAttribute('href');
      a.classList.add('local-link');
    }
    const path = linkPath(raw);
    a.title = path ?? url;
    if (path === null) continue;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'copy-path';
    btn.dataset.copyPath = path;
    btn.title = path;
    btn.textContent = COPY_PATH_LABEL;
    a.after(btn);
  }
}
