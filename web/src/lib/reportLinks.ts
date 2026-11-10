// Ссылки тела отчёта (M6, задача 15a; долг M5.5): тело пишет агент — содержимое недоверенное.
//
// Первая линия — сервер: в HTML страницы `href` есть только у ссылок, по которым разрешён
// переход (`http`, `https`, `mailto`, `vscode://file/<путь>`); ссылка `file:` и прочие `vscode:`
// приходят без `href`, с адресом в `data-url` (`zam_markdown::render_report_html`). Этот модуль —
// вторая линия и оформление: те же правила (`crates/zam-core/src/link.rs`; общие наборы
// `tests/fixtures/link_schemes.json` и `invisible_chars.json` читают тесты обеих сторон) плюс
// сверка схемы с разбором URL самого браузера — при расхождении или чужой схеме `href`
// снимается. Ссылкам `file:` и `vscode:` — адрес в подсказке и в печати и, если путь безопасно
// вставлять, кнопка «копировать путь». Разметка строится через DOM (`createElement`,
// `setAttribute`, `textContent`), без вставки HTML.

/** Схемы внешних ссылок отчёта — как `zam_core::REPORT_LINK_SCHEMES`. */
export const REPORT_LINK_SCHEMES: readonly string[] = ['http', 'https', 'mailto', 'vscode', 'file'];

/** `rel` внешних ссылок. */
export const EXTERNAL_REL = 'noopener noreferrer';
/** Подпись кнопки копирования пути (`data-copy-path` — признак для делегирования клика). */
export const COPY_PATH_LABEL = 'копировать путь';
/** Предел длины адреса в подсказке и в печати, символов; длиннее — с «…». */
export const SHOWN_MAX = 200;

/** Невидимые символы и управление форматом — как `zam_core::text::INVISIBLE_RANGES`. */
export const INVISIBLE_RANGES: readonly (readonly [number, number])[] = [
  [0x00ad, 0x00ad],
  [0x034f, 0x034f],
  [0x061c, 0x061c],
  [0x115f, 0x1160],
  [0x17b4, 0x17b5],
  [0x180b, 0x180f],
  [0x200b, 0x200f],
  [0x2028, 0x202e],
  [0x2060, 0x206f],
  [0x3164, 0x3164],
  [0xfe00, 0xfe0f],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff9, 0xfffb],
  [0x1bca0, 0x1bca3],
  [0x1d173, 0x1d17a],
  [0xe0000, 0xe0fff],
];

/** Символ (код) из {@link INVISIBLE_RANGES}. */
export function isInvisible(cp: number): boolean {
  return INVISIBLE_RANGES.some(([a, b]) => cp >= a && cp <= b);
}

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
 * Схема ссылки допустима и браузер читает адрес так же: схема из списка либо схемы нет (защита
 * от расхождения разборов: при несогласии — отказ).
 */
export function reportLinkAllowed(target: string): boolean {
  const scheme = urlScheme(target);
  const seen = browserScheme(target);
  if (scheme === null) return seen === 'http';
  return REPORT_LINK_SCHEMES.includes(scheme) && seen === scheme;
}

/**
 * По ссылке разрешён переход — как `zam_core::link::report_link_clickable`: схемы нет, `http`,
 * `https`, `mailto` или `vscode://file/<путь>` (схема без учёта регистра, `//file/` — буквально,
 * путь непуст и не сетевой: не начинается с `/`, `\`, `%2F`, `%5C`). `file:` и прочие адреса
 * `vscode:` (команды расширений редактора) — нет.
 */
export function reportLinkClickable(target: string): boolean {
  const t = clean(target);
  const scheme = urlScheme(t);
  if (scheme === null || scheme === 'http' || scheme === 'https' || scheme === 'mailto') return true;
  if (scheme !== 'vscode') return false;
  const rest = t.slice(7);
  if (!rest.startsWith('//file/')) return false;
  const p = rest.slice(7).toLowerCase();
  return !(p === '' || p.startsWith('/') || p.startsWith('\\') || p.startsWith('%2f') || p.startsWith('%5c'));
}

/** Внешняя ссылка: со схемой или `//host` (браузер читает `\` как `/`). */
function isExternal(target: string, scheme: string | null): boolean {
  return scheme !== null || /^[/\\]{2}/.test(clean(target));
}

/**
 * Знаки, которые оболочка при вставке пути аргументом исполнит или раскроет, а не передаст как
 * текст (bash, PowerShell, cmd) — даже внутри двойных кавычек:
 * - `` ` `` и `$` — подстановка команды и переменной (bash, PowerShell: `$(…)`, `${…}`, `$x`);
 * - `;`, `|`, `&` — следующая команда, конвейер (все три оболочки);
 * - `<`, `>` — перенаправление в файл;
 * - `"`, `'` — закрывают кавычки, в которые человек заключил путь;
 * - `(`, `)` — подвыражение PowerShell в начале слова (`a (calc)` после пробела в пути), подоболочка bash;
 * - `%` — переменная cmd (`%COMSPEC%`); `^` — экранирование cmd; `!` — история bash и
 *   отложенные переменные cmd.
 * Пробел, `#`, `~`, `*`, `?`, `[`, `{` не исполняют ничего (худшее — путь разобьётся или
 * раскроется шаблоном) и остаются: они обычны в именах файлов.
 */
const SHELL_META = '`$;|&<>"\'()%^!';

/**
 * Текст небезопасно класть в буфер обмена как путь: управляющие символы (перевод строки при
 * вставке в терминал выполняет строку), невидимые и bidi ({@link INVISIBLE_RANGES}), знаки
 * оболочки ({@link SHELL_META}).
 */
export function unsafePathText(s: string): boolean {
  for (const ch of s) {
    const cp = ch.codePointAt(0)!;
    if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f) || isInvisible(cp) || SHELL_META.includes(ch)) return true;
  }
  return false;
}

function decode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** Путь без запроса и якоря (`?…`, `#…` — не часть пути файла; в имени они кодируются `%XX`). */
const pathOnly = (s: string) => s.replace(/[?#].*$/s, '');

/** Имя сервера сетевого пути: без `@`, `:`, пробелов и прочего, что выдаёт не имя, а подмену. */
const HOST = /^[A-Za-z0-9._-]+$/;

/**
 * Текст для «копировать путь» или `null` — кнопки нет (адрес остаётся текстом в подсказке:
 * человек может выделить его сам).
 * - `file:` → путь ОС: `file:///C:/a/b.txt` → `C:\a\b.txt`, `file:///home/u/a` → `/home/u/a`;
 *   сетевой путь — в виде UNC, в какой бы форме он ни был записан: `file://server/share/a`,
 *   `file:////server/share/a`, `file:///%5C%5Cserver%5Cshare%5Ca` → `\\server\share\a`;
 *   «сервер» с `@`, `:` и другими знаками не из имени (`file://C:/data`,
 *   `file://user:pw@server:445/…`) — пути нет: он вводил бы в заблуждение;
 * - `vscode:` → путь после `vscode://file/` (с `:строка:столбец`, если есть); иной адрес
 *   `vscode:` (команда расширения редактора) — не путь.
 * Путь, который небезопасно вставлять ({@link unsafePathText}), не копируется.
 */
export function linkPath(target: string): string | null {
  const t = clean(target);
  const scheme = urlScheme(t);
  let out: string;
  if (scheme === 'file') {
    const rest = pathOnly(t.slice(5));
    const m = /^[/\\]{2}([^/\\]*)(.*)$/s.exec(rest);
    let host = m ? decode(m[1]) : '';
    let path = decode(m ? m[2] : rest).replace(/\\/g, '/');
    if (host.toLowerCase() === 'localhost') host = '';
    if (!host && /^\/{2,}[^/]/.test(path)) {
      // сетевой путь без имени сервера в адресе: `file:////server/share`, `file:///\\server\share`
      const unc = path.replace(/^\/+/, '');
      const cut = unc.indexOf('/');
      host = cut < 0 ? unc : unc.slice(0, cut);
      path = cut < 0 ? '' : unc.slice(cut);
    }
    if (host) {
      if (!HOST.test(host)) return null;
      out = `\\\\${host}${path.replace(/\//g, '\\')}`;
    } else if (/^\/?[A-Za-z][:|](\/|$)/.test(path)) {
      out = path.replace(/^\//, '').replace('|', ':').replace(/\//g, '\\');
    } else out = path.startsWith('/') ? path : `/${path}`;
  } else if (scheme === 'vscode') {
    const rest = t.slice(7);
    if (!rest.startsWith('//file/')) return null;
    // путь URL начинается с `/` после `file`: `/C:/x` → `C:/x`, `/home/u`, `//server/share`
    const p = decode(pathOnly(rest.slice(6)));
    out = /^\/[A-Za-z]:/.test(p) ? p.slice(1) : p;
  } else return null;
  return out === '' || out === '/' || unsafePathText(out) ? null : out;
}

/** Адрес для показа человеку (подсказка, печать): не длиннее {@link SHOWN_MAX}, дальше — «…». */
export function shorten(s: string): string {
  const chars = [...s];
  return chars.length <= SHOWN_MAX ? s : `${chars.slice(0, SHOWN_MAX).join('')}…`;
}

/** Ссылки, уже оформленные {@link hydrateReportLinks} (метку в HTML отчёта подделать нельзя). */
const done = new WeakSet<object>();

/**
 * Ссылки отрендеренного тела отчёта (экран и печатная копия); повторный проход по тому же DOM
 * ничего не меняет (оформленные ссылки помнит {@link done}; `href` проверяется каждый раз):
 * - `href` с чужой схемой, с расхождением разборов или без права перехода (`file:`, `vscode:` не
 *   вида `vscode://file/…`) — снимается: вторая линия после сервера;
 * - внешним ссылкам с `href` — `rel="noopener noreferrer"`;
 * - ссылке `file:`/`vscode:` (адрес — `data-url` с сервера или её `href`): без `href` — класс
 *   `local-link`; адрес — в `title` и, если он не текст самой ссылки, в `data-print` (печать:
 *   `a[data-print]::after`), оба не длиннее {@link SHOWN_MAX}; при безопасном пути — кнопка
 *   «копировать путь» (`data-copy-path`).
 */
export function hydrateReportLinks(root: ParentNode): void {
  for (const a of root.querySelectorAll<HTMLElement>('a[href], a[data-url]')) {
    const again = done.has(a);
    done.add(a);
    const raw = a.getAttribute('href');
    let url = a.getAttribute('data-url');
    // оформленную ссылку проход не трогает — кроме `href`, который успели подменить
    if (again && (raw === null || (reportLinkAllowed(raw) && reportLinkClickable(raw)))) continue;
    if (raw !== null) {
      const scheme = urlScheme(raw);
      const allowed = reportLinkAllowed(raw);
      if (!allowed || !reportLinkClickable(raw)) {
        a.removeAttribute('href');
        if (allowed && url === null) url = raw;
      } else {
        if (isExternal(raw, scheme)) a.setAttribute('rel', EXTERNAL_REL);
        if (scheme === 'vscode') url = raw;
      }
    }
    if (url === null || again) continue;
    const scheme = urlScheme(url);
    if ((scheme !== 'file' && scheme !== 'vscode') || !reportLinkAllowed(url)) {
      a.removeAttribute('data-url');
      continue;
    }
    url = clean(url);
    a.setAttribute('data-url', url);
    if (!a.hasAttribute('href')) a.classList.add('local-link');
    if ((a.textContent ?? '').trim() !== url) a.setAttribute('data-print', shorten(url));
    const path = linkPath(url);
    a.setAttribute('title', shorten(path ?? url));
    if (path === null) continue;
    const btn = document.createElement('button');
    btn.setAttribute('type', 'button');
    btn.setAttribute('class', 'copy-path');
    btn.setAttribute('data-copy-path', path);
    btn.setAttribute('title', shorten(path));
    btn.textContent = COPY_PATH_LABEL;
    a.after(btn);
  }
}
