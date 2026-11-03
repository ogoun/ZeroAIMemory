// Печать и PDF отчёта (M5.5, спецификация §9): PDF делает браузер — «Скачать PDF» собирает
// печатный контейнер `.print-root` из HTML сервера (шапка, тело, `data.refs` и прочие файлы
// текстом), «оживляет» его как экран (KaTeX, Mermaid в светлой теме `default`) и вызывает
// `window.print()` только после готовности всего: картинок (загрузка, `decode`), ленивых модулей
// и шрифтов (`document.fonts.ready`). Не готово — печати нет, причина уходит вызывающему. Готовая
// копия получает класс `ready` и остаётся до `clearPrint()` (смена отчёта, уход, повторная
// печать): печать в Safari и Chrome Android асинхронна, предпросмотр пересчитывается, и Ctrl+P
// после кнопки печатает тот же отчёт. Вёрстка печати — `print.css` (A4, только готовая копия,
// светлые цвета). Модуль грузится по клику (динамический `import()` в ReportView).
import '../print.css';
import { hydrateRendered, type HydrateOpts } from './hydrate/render';
// без импорта из чанка ReportsView (lib/reports): иначе сборщик выносит общий код entry в
// отдельный чанк (замер задачи 7, раунд 1)
import { PRINT_READY_CLASS, PRINT_ROOT_CLASS, clearPrint } from './printRoot';

export interface PrintMeta {
  title: string;
  /** Тело начинается с `<h1>` заголовка — в шапке он не повторяется. */
  titleInBody: boolean;
  date: string;
  author: string;
  project: string;
  /** `data.refs` строками («Коммиты: a, b»). */
  refs: string[];
  /** Прочие файлы (не картинки) — текстом с именем: в PDF их не открыть. */
  files: string[];
}

export interface PrintOpts extends HydrateOpts {
  /** Причина, по которой печать не вызвана («PDF не готов: …» показывает вызывающий). */
  onfail?: (reason: string) => void;
}

/** Сколько ждать картинки, модули и шрифты: зависшая загрузка не держит кнопку вечно. */
export const READY_TIMEOUT_MS = 30_000;

/** Префикс id печатной копии: id экрана не дублируются, ссылки `#…` PDF ведут внутрь копии. */
const ID_PREFIX = 'print-';

/** Исход загрузки картинки копии; подписка — до вставки в документ (событие не теряется). */
const loaded = new WeakMap<HTMLImageElement, Promise<boolean>>();

/** Печатный контейнер из серверного HTML; печать — только после готовности всего. */
export async function printReport(html: string, meta: PrintMeta, o: PrintOpts): Promise<'printed' | 'failed'> {
  clearPrint();
  const root = build(html, meta);
  const doc = root.querySelector<HTMLElement>('.doc')!;
  document.body.append(root);
  const fail = (reason: string): 'failed' => {
    root.remove();
    o.onfail?.(reason);
    return 'failed';
  };
  let handle = 0;
  const timer = new Promise<'timeout'>((r) => (handle = window.setTimeout(() => r('timeout'), READY_TIMEOUT_MS)));
  const work = (async () => {
    // якоря копии уже с префиксом (`build`) — ссылки остаются внутри неё
    const modules = await hydrateRendered(doc, { ...o, self: (a) => `#${a}`, mermaidTheme: 'default' });
    if (!modules) return 'не загрузился модуль формул или диаграмм — обновите страницу';
    return notReady(root);
  })();
  let why: string | null;
  try {
    why = await Promise.race([work, timer]);
  } catch (e) {
    return fail(`ошибка подготовки: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    clearTimeout(handle);
  }
  if (!o.alive() || !root.isConnected) return fail('страница отчёта закрыта');
  if (why === 'timeout') {
    return fail(`картинки, формулы или диаграммы не загрузились за ${Math.round(READY_TIMEOUT_MS / 1000)} с`);
  }
  if (why) return fail(why);
  // кнопка копирования кода и якоря заголовков — не для бумаги
  for (const el of root.querySelectorAll('button, a.heading-anchor')) el.remove();
  root.classList.add(PRINT_READY_CLASS);
  window.print();
  return 'printed';
}

/** `true` — всё догружено: картинки загружены и декодированы, модули отработали, шрифты готовы. */
export async function whenReady(root: HTMLElement): Promise<boolean> {
  return (await notReady(root)) === null;
}

/** Причина неготовности или `null`. */
async function notReady(root: HTMLElement): Promise<string | null> {
  // раскладка запускает загрузку шрифтов (KaTeX) до ожидания `fonts.ready`
  void root.offsetHeight;
  const imgs = [...root.querySelectorAll<HTMLImageElement>('img')];
  const bad = (await Promise.all(imgs.map(async (i) => ((await imageReady(i)) ? null : i)))).find((x) => x);
  if (bad) return `картинка «${bad.alt || bad.getAttribute('src') || ''}» не загрузилась`;
  if (root.querySelector('.math:not([data-zam-math])')) return 'формулы не отрисованы';
  if (root.querySelector('pre.mermaid-src:not([data-zam-mermaid])')) return 'диаграммы не отрисованы';
  void root.offsetHeight;
  await document.fonts.ready;
  return null;
}

const isSvg = (img: HTMLImageElement) => /\.svg$/i.test(new URL(img.src, location.href).pathname);

/**
 * Отказ картинки — только событие `error` (без подписки — `complete` с нулевой шириной у растра).
 * SVG без своих размеров (только `viewBox`) в Firefox даёт `naturalWidth = 0`, а очень большой
 * растр отклоняет `decode()` и при этом рисуется — ни то ни другое печать не блокирует.
 */
async function imageReady(img: HTMLImageElement): Promise<boolean> {
  const settled = loaded.get(img);
  const ok = settled ? await settled : !img.complete || img.naturalWidth > 0 || isSvg(img);
  if (!ok) return false;
  // декодирование до печати; отказ `decode()` при успешной загрузке — не отказ картинки
  await img.decode().catch(() => undefined);
  return true;
}

/** Подписка на исход загрузки — сразу после разбора HTML, до первых событий. */
function watchLoad(img: HTMLImageElement): void {
  loaded.set(
    img,
    new Promise<boolean>((r) => {
      img.addEventListener('load', () => r(true), { once: true });
      img.addEventListener('error', () => r(false), { once: true });
    }),
  );
}

/** id копии — с префиксом, ссылки `#id` на них — тоже (сноски, оглавление в PDF). */
function prefixIds(doc: HTMLElement): void {
  const ids = new Set<string>();
  for (const e of doc.querySelectorAll<HTMLElement>('[id]')) {
    ids.add(e.id);
    e.id = ID_PREFIX + e.id;
  }
  for (const a of doc.querySelectorAll<HTMLAnchorElement>('a[href^="#"]')) {
    const raw = a.getAttribute('href')!.slice(1);
    if (raw.startsWith('/')) continue;
    let id = raw;
    try {
      id = decodeURIComponent(raw);
    } catch {
      /* битое %-кодирование — как есть */
    }
    if (ids.has(id)) a.setAttribute('href', `#${encodeURIComponent(ID_PREFIX + id)}`);
  }
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls: string, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function build(html: string, m: PrintMeta): HTMLElement {
  const root = el('div', PRINT_ROOT_CLASS);
  // на экране контейнер невидим (print.css), печатается только готовая копия
  root.setAttribute('aria-hidden', 'true');
  const head = el('header', 'print-head');
  if (!m.titleInBody) head.append(el('h1', 'print-title', m.title));
  head.append(el('p', 'print-meta', [m.date, m.author, m.project && `проект ${m.project}`].filter(Boolean).join(' · ')));
  const doc = el('div', 'doc');
  // HTML ответа сервера: санитизирован ammonia, картинки — файлы этой записи (как на экране)
  doc.innerHTML = html;
  prefixIds(doc);
  // ленивые картинки в невидимом контейнере не загрузились бы никогда
  for (const img of doc.querySelectorAll('img')) {
    watchLoad(img);
    img.loading = 'eager';
  }
  root.append(head, doc);
  const list = (cls: string, title: string, rows: string[]) => {
    if (!rows.length) return;
    const s = el('section', cls);
    s.append(el('h2', '', title));
    const ul = el('ul', '');
    for (const r of rows) ul.append(el('li', '', r));
    s.append(ul);
    root.append(s);
  };
  list('print-refs', 'Ссылки', m.refs);
  list('print-files', 'Файлы', m.files);
  return root;
}
