// Печать и PDF отчёта (M5.5, спецификация §9): PDF делает браузер — «Скачать PDF» собирает
// печатный контейнер `.print-root` из HTML сервера (шапка, тело, `data.refs` и прочие файлы
// текстом), «оживляет» его как экран (KaTeX, Mermaid в светлой теме `default`) и вызывает
// `window.print()` только после готовности всего: картинок (`decode`), ленивых модулей и шрифтов
// (`document.fonts.ready`). Не готово — печати нет, причина уходит вызывающему. После `afterprint`
// контейнер удаляется. Вёрстка печати — `print.css` (A4, только `.print-root`, светлые цвета).
import '../print.css';
import { hydrateRendered, type HydrateOpts } from './hydrate/render';
import { bodyStartsWithTitle } from './reports';

export interface PrintMeta {
  title: string;
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

const ROOT_CLASS = 'print-root';

/** Убирает печатный контейнер (уход со страницы отчёта, повторная печать). */
export function clearPrint(): void {
  for (const el of document.querySelectorAll(`body > .${ROOT_CLASS}`)) el.remove();
}

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
    const modules = await hydrateRendered(doc, { ...o, mermaidTheme: 'default' });
    if (!modules) return 'не загрузился модуль формул или диаграмм — обновите страницу';
    return notReady(root);
  })();
  const why = await Promise.race([work, timer]);
  clearTimeout(handle);
  if (!o.alive() || !root.isConnected) return fail('страница отчёта закрыта');
  if (why === 'timeout') return fail('картинки, формулы или диаграммы не загрузились за 30 с');
  if (why) return fail(why);
  // кнопка копирования кода и якоря заголовков — не для бумаги
  for (const el of root.querySelectorAll('button, a.heading-anchor')) el.remove();
  window.addEventListener('afterprint', () => root.remove(), { once: true });
  window.print();
  return 'printed';
}

/** `true` — всё догружено: картинки декодированы, модули отработали, шрифты готовы. */
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

async function imageReady(img: HTMLImageElement): Promise<boolean> {
  if (!img.complete) {
    await new Promise<void>((r) => {
      img.addEventListener('load', () => r(), { once: true });
      img.addEventListener('error', () => r(), { once: true });
    });
  }
  if (img.naturalWidth === 0) return false;
  try {
    await img.decode();
    return true;
  } catch {
    return false;
  }
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls: string, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function build(html: string, m: PrintMeta): HTMLElement {
  const root = el('div', ROOT_CLASS);
  // на экране контейнер невидим (print.css), печатается только он
  root.setAttribute('aria-hidden', 'true');
  const head = el('header', 'print-head');
  if (!bodyStartsWithTitle(html, m.title)) head.append(el('h1', 'print-title', m.title));
  head.append(el('p', 'print-meta', [m.date, m.author, m.project && `проект ${m.project}`].filter(Boolean).join(' · ')));
  const doc = el('div', 'doc');
  // HTML ответа сервера: санитизирован ammonia, картинки — файлы этой записи (как на экране)
  doc.innerHTML = html;
  // ленивые картинки в невидимом контейнере не загрузились бы никогда
  for (const img of doc.querySelectorAll('img')) img.loading = 'eager';
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
