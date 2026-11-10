// Печать и PDF отчёта (M5.5, задача 7, спецификация §9): «PDF / печать» собирает печатную
// вёрстку и вызывает `window.print()` (здесь — шпион) только после догрузки ленивых модулей
// KaTeX и Mermaid, картинок и шрифтов; ошибка модуля — сообщение без печати; печать светлая
// при тёмной теме ОС; A4 без навигации и кнопок; разрывы не режут картинки, формулы и строки
// таблиц; `data.refs` и прочие файлы — текстом. PDF строит Chromium (`page.pdf`), проверка —
// pdfjs-dist (уже зависимость WebUI). PDF и снимки — в .tmp/e2e/ (внутри проекта).
import { expect, test, type Page } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import {
  LINK_FILE,
  LINK_FILE_AUTO,
  LINK_HTTPS,
  LINK_VSCODE,
  CLI_NODE_A,
  CLI_NODE_B,
  CLI_REPORT_TITLE,
  PRINT_FILE,
  PRINT_LONG_CODE,
  PRINT_NODE_A,
  PRINT_NODE_B,
  REPORT_COMMIT,
  REPORT_MACHINE,
  REPORT_SESSION,
  SOLUTION_GUID,
  SOLUTION_NAME,
} from './seed';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const out = fileURLToPath(new URL('../../.tmp/e2e/print/', import.meta.url));
mkdirSync(out, { recursive: true });

test.use({ storageState: ADMIN_STATE });

/** Снимок печатного контейнера в момент вызова `window.print()`. */
interface PrintCall {
  katex: number;
  diagrams: number;
  imgs: boolean[];
  fontsLoading: number;
  katexFontsLoaded: number;
}

declare global {
  interface Window {
    __printCalls: PrintCall[];
  }
}

/** Шпион вместо `window.print()`: фиксирует, что было готово в момент вызова. */
async function spyPrint(page: Page) {
  await page.addInitScript(() => {
    window.__printCalls = [];
    window.print = () => {
      const root = document.querySelector('.print-root');
      const fonts = [...document.fonts];
      window.__printCalls.push({
        katex: root?.querySelectorAll('.katex').length ?? 0,
        diagrams: root?.querySelectorAll('.mermaid-diagram svg').length ?? 0,
        // загружена: растр — с шириной, SVG без размеров может иметь naturalWidth = 0 (Firefox)
        imgs: [...(root?.querySelectorAll('img') ?? [])].map((x) => {
          const i = x as HTMLImageElement;
          return i.complete && (i.naturalWidth > 0 || /\.svg$/.test(new URL(i.src).pathname));
        }),
        fontsLoading: fonts.filter((f) => f.status === 'loading').length,
        katexFontsLoaded: fonts.filter((f) => f.family.includes('KaTeX') && f.status === 'loaded').length,
      });
    };
  });
}

let adminId = '';
async function openPrintReport(page: Page) {
  if (!adminId) {
    const r = await page.request.get(`${base()}api/v1/auth/me`);
    adminId = ((await r.json()) as { user_id: string }).user_id;
  }
  const id = env('E2E_REPORT_PRINT');
  await page.goto(`${base()}#/u/${adminId}/s/${SOLUTION_GUID}/j/reports/${id}`);
  await expect(page.locator(`article.report[data-report="${id}"] .doc`)).toBeVisible();
}

const pdfButton = (page: Page) => page.getByRole('button', { name: 'PDF / печать', exact: true });

/** Открыть отчёт, нажать «PDF / печать» и дождаться вызова печати. */
async function printed(page: Page): Promise<PrintCall> {
  await spyPrint(page);
  await openPrintReport(page);
  await pdfButton(page).click();
  await expect.poll(() => page.evaluate(() => window.__printCalls.length), { timeout: 30_000 }).toBe(1);
  return (await page.evaluate(() => window.__printCalls))[0];
}

const KATEX_CHUNK = /\/assets\/katex-[\w-]+\.js$/;
const MERMAID_CHUNK = /\/assets\/mermaid-[\w-]+\.js$/;

test('print_waits_for_lazy_modules_images_fonts', async ({ page }) => {
  // «ворота»: чанки KaTeX и Mermaid, шрифты KaTeX и одна картинка держатся до отпускания
  let open!: () => void;
  const gate = new Promise<void>((r) => (open = r));
  const held = async (route: import('@playwright/test').Route) => {
    await gate;
    await route.continue();
  };
  await page.route(KATEX_CHUNK, held);
  await page.route(MERMAID_CHUNK, held);
  await page.route(/\/assets\/KaTeX_[\w-]+\.woff2$/, held);
  await page.route(/\/attachments\/[0-9a-f]{64}\/p2\.png/, held);
  await spyPrint(page);
  await openPrintReport(page);
  await pdfButton(page).click();
  await expect(page.getByRole('button', { name: 'Готовлю PDF…' })).toBeDisabled();
  // копия собрана, но не готова: печати нет, в печать (Ctrl+P) она не попадает — печатается
  // страница отчёта без навигации и кнопок
  await expect(page.locator('.print-root')).toHaveCount(1);
  await expect(page.locator('.print-root')).not.toHaveClass(/\bready\b/);
  await page.emulateMedia({ media: 'print' });
  await expect(page.locator('.print-root')).toBeHidden();
  await expect(page.locator('article.report .doc')).toBeVisible();
  await expect(page.locator('.topbar, .sidebar')).toHaveCount(2);
  for (const sel of ['.topbar', '.sidebar', 'article.report button']) {
    await expect(page.locator(sel).first(), sel).toBeHidden();
  }
  await page.emulateMedia({ media: null });
  await page.waitForTimeout(500);
  expect(await page.evaluate(() => window.__printCalls.length)).toBe(0);
  open();
  await expect.poll(() => page.evaluate(() => window.__printCalls.length), { timeout: 30_000 }).toBe(1);
  const call = (await page.evaluate(() => window.__printCalls))[0];
  // формулы (строчная и выключная), диаграмма, картинки (три PNG и SVG без размеров), шрифты KaTeX
  expect(call.katex).toBe(2);
  expect(call.diagrams).toBe(1);
  expect(call.imgs).toEqual([true, true, true, true]);
  expect(call.fontsLoading).toBe(0);
  expect(call.katexFontsLoaded).toBeGreaterThan(0);
  await expect(pdfButton(page)).toBeEnabled();
  await expect(page.locator('.print-root.ready')).toHaveCount(1);
});

test('print_svg_without_size_not_blocking', async ({ page }) => {
  // ветка Firefox: SVG только с viewBox (без width/height) там даёт naturalWidth = 0 — Chromium
  // подменяется геттером, растр не трогается
  await page.addInitScript(() => {
    const d = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'naturalWidth')!;
    Object.defineProperty(HTMLImageElement.prototype, 'naturalWidth', {
      configurable: true,
      get(this: HTMLImageElement) {
        return /\.svg$/i.test(new URL(this.src, location.href).pathname) ? 0 : d.get!.call(this);
      },
    });
  });
  const call = await printed(page);
  // печать вызвана, картинка в копии, её naturalWidth — 0
  const svg = page.locator('.print-root img[src*="nosize.svg"]');
  await expect(svg).toHaveCount(1);
  expect(await svg.evaluate((i: HTMLImageElement) => [i.complete, i.naturalWidth])).toEqual([true, 0]);
  expect(call.imgs.length).toBe(4);
});

test('print_again_ready_copy_immediate', async ({ page }) => {
  await printed(page);
  // повторное нажатие: готовая копия того же отчёта печатается сразу, без пересборки и ожидания
  // (пользовательская активация Safari не успевает истечь)
  await page.locator('.print-root.ready').evaluate((r) => ((r as HTMLElement).dataset.mark = 'первая'));
  // любые загрузки после клика держатся: печать не должна их ждать
  await page.route(/\/assets\/|\/attachments\//, () => undefined);
  await pdfButton(page).click();
  expect(await page.evaluate(() => window.__printCalls.length)).toBe(2);
  await expect(page.locator('.print-root')).toHaveCount(1);
  await expect(page.locator('.print-root.ready')).toHaveAttribute('data-mark', 'первая');
  await expect(pdfButton(page)).toBeEnabled();
});

test('print_not_called_when_image_fails', async ({ page }) => {
  await page.route(/\/attachments\/[0-9a-f]{64}\/p2\.png/, (route) => route.fulfill({ status: 404, body: 'нет' }));
  await spyPrint(page);
  await openPrintReport(page);
  await pdfButton(page).click();
  await expect(page.getByRole('status').filter({ hasText: 'PDF не готов:' })).toContainText('Вторая');
  await expect(pdfButton(page)).toBeEnabled();
  expect(await page.evaluate(() => window.__printCalls.length)).toBe(0);
  await expect(page.locator('.print-root')).toHaveCount(0);
});

test('print_not_called_when_module_fails', async ({ page }) => {
  await page.route(MERMAID_CHUNK, (route) => route.fulfill({ status: 500, body: 'нет' }));
  await spyPrint(page);
  await openPrintReport(page);
  await pdfButton(page).click();
  await expect(page.getByRole('status').filter({ hasText: 'PDF не готов:' })).toBeVisible();
  await expect(pdfButton(page)).toBeEnabled();
  expect(await page.evaluate(() => window.__printCalls.length)).toBe(0);
  await expect(page.locator('.print-root')).toHaveCount(0);
});

test.describe('тёмная тема ОС', () => {
  test.use({ colorScheme: 'dark' });

  test('print_light_theme_even_in_dark_os', async ({ page }) => {
    await spyPrint(page);
    await openPrintReport(page);
    // экранная диаграмма — в тёмной теме страницы
    const screenNode = page.locator('article.report .mermaid-diagram .node .label-container').first();
    await expect(screenNode).toBeVisible();
    const fill = (sel: string) =>
      page.evaluate((s) => getComputedStyle(document.querySelector(s)!).fill, sel);
    const screenFill = await fill('article.report .mermaid-diagram .node .label-container');
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).not.toBe('rgb(255, 255, 255)');
    await pdfButton(page).click();
    await expect.poll(() => page.evaluate(() => window.__printCalls.length), { timeout: 30_000 }).toBe(1);
    await page.emulateMedia({ media: 'print' });
    // фон белый, текст тёмный, код на светлом фоне, диаграмма — тема Mermaid `default`
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe('rgb(255, 255, 255)');
    expect(await page.evaluate(() => getComputedStyle(document.querySelector('.print-root')!).color)).toBe('rgb(28, 32, 38)');
    expect(await page.evaluate(() => getComputedStyle(document.querySelector('.print-root pre')!).backgroundColor)).toBe(
      'rgb(243, 244, 247)',
    );
    const printFill = await fill('.print-root .mermaid-diagram .node .label-container');
    expect(printFill).toBe('rgb(236, 236, 255)');
    expect(printFill).not.toBe(screenFill);
    // подсветка кода сервера: тёмная тема — только для экрана
    const css = await (await page.request.get(`${base()}api/v1/render/highlight.css`)).text();
    expect(css).toContain('@media screen and (prefers-color-scheme: dark)');
    await page.screenshot({ path: `${out}print-dark-os.png`, fullPage: true });
  });
});

test('print_layout_a4_no_nav_buttons', async ({ page }) => {
  await printed(page);
  await page.emulateMedia({ media: 'print' });
  // видна только печатная вёрстка: без навигации, дерева, кнопок
  await expect(page.locator('.print-root')).toBeVisible();
  await expect(page.locator('#app')).toBeHidden();
  await expect(page.locator('button:visible, nav:visible')).toHaveCount(0);
  const head = page.locator('.print-root .print-head');
  const meta = head.locator('.print-meta');
  await expect(meta).toContainText(`агент, сессия ${REPORT_SESSION}, ${REPORT_MACHINE}`);
  await expect(meta).toContainText(`проект ${SOLUTION_NAME}`);
  await expect(meta).toContainText(/\d{2}\.\d{2}\.\d{4}, \d{2}:\d{2}/);
  // заголовок — первый `#` тела, в шапке не повторяется
  await expect(page.locator('.print-root h1')).toHaveCount(1);
  // размер страницы — из `@page { size: A4 }` печатной вёрстки
  const pdf = await page.pdf({ preferCSSPageSize: true });
  const facts = await pdfFacts(pdf);
  expect(facts.sizes.length).toBeGreaterThan(0);
  for (const [w, h] of facts.sizes) {
    expect(Math.abs(w - 595.28)).toBeLessThan(1.5);
    expect(Math.abs(h - 841.89)).toBeLessThan(1.5);
  }
  // после печати копия остаётся (асинхронная печать, пересчёт предпросмотра, повторный Ctrl+P):
  // на экране она невидима, в печати — снова отчёт
  await page.evaluate(() => window.dispatchEvent(new Event('afterprint')));
  await expect(page.locator('.print-root.ready')).toHaveCount(1);
  await expect(page.locator('.print-root')).toBeVisible();
  await page.emulateMedia({ media: 'screen' });
  await expect(page.locator('.print-root')).toBeHidden();
  await expect(page.locator('#app')).toBeVisible();
  expect(await page.locator('.print-root').evaluate((e) => getComputedStyle(e).visibility)).toBe('hidden');
  // уход со страницы отчёта убирает копию
  await page.locator('article.report a.back').click();
  await expect(page.locator('.print-root')).toHaveCount(0);
});

test('print_breaks_avoid_inside', async ({ page }) => {
  await printed(page);
  await page.emulateMedia({ media: 'print' });
  const css = (sel: string, prop: string) =>
    page.evaluate(
      ([s, p]) => [...document.querySelectorAll(s)].map((e) => getComputedStyle(e).getPropertyValue(p)),
      [sel, prop] as const,
    );
  for (const sel of ['.print-root img', '.print-root .katex-display', '.print-root .mermaid-diagram', '.print-root tr']) {
    const v = await css(sel, 'break-inside');
    expect(v.length, sel).toBeGreaterThan(0);
    expect(new Set(v), sel).toEqual(new Set(['avoid']));
  }
  expect(await css('.print-root thead', 'display')).toEqual(['table-header-group']);
  // размер текста печати — явно (11pt), не экранные 16px `.doc` и не мобильные правила doc.css
  expect(await css('.print-root .doc', 'font-size')).toEqual(['14.6667px']);
  expect(await css('.print-root .doc table', 'display')).toEqual(['table']);
  expect(new Set(await css('.print-root h1, .print-root h2', 'break-after'))).toEqual(new Set(['avoid']));
  // длинный код переносится, а не уходит за поле
  const pre = page.locator('.print-root pre', { hasText: PRINT_LONG_CODE.slice(0, 40) });
  expect(await pre.evaluate((e) => getComputedStyle(e).whiteSpace)).toBe('pre-wrap');
  expect(await pre.evaluate((e) => getComputedStyle(e).overflowWrap)).toBe('anywhere');
  expect(await pre.evaluate((e) => e.scrollWidth <= e.clientWidth + 1)).toBe(true);
});

test('print_refs_and_files_as_text', async ({ page }) => {
  await printed(page);
  await page.emulateMedia({ media: 'print' });
  const refs = page.locator('.print-root .print-refs');
  await expect(refs).toContainText(`Коммиты: ${REPORT_COMMIT}`);
  await expect(refs).toContainText('Файлы: src/print.py');
  const files = page.locator('.print-root .print-files');
  await expect(files).toContainText(PRINT_FILE);
  await expect(files).toContainText('text/csv');
  // текстом, не ссылками
  await expect(page.locator('.print-root .print-refs a, .print-root .print-files a')).toHaveCount(0);
  // id копии не повторяют id экрана, ссылки на сноски ведут внутрь копии
  const ids = await page.evaluate(() => {
    const inside = [...document.querySelectorAll('.print-root [id]')].map((e) => e.id);
    const outside = new Set(
      [...document.querySelectorAll('[id]')].filter((e) => !e.closest('.print-root')).map((e) => e.id),
    );
    const links = [...document.querySelectorAll<HTMLAnchorElement>('.print-root a[href^="#"]')].map((a) =>
      decodeURIComponent(a.getAttribute('href')!.slice(1)),
    );
    return { dup: inside.filter((i) => outside.has(i)), links, inside };
  });
  expect(ids.dup).toEqual([]);
  expect(ids.links.length).toBeGreaterThan(0);
  for (const l of ids.links) expect(ids.inside, l).toContain(l);
});

test('print_button_pdf_print_with_hint', async ({ page }) => {
  // M6, задача 15a (ответ владельца по M5.5, п. 6): кнопка «PDF / печать» и подсказка рядом
  await spyPrint(page);
  await openPrintReport(page);
  const btn = pdfButton(page);
  await expect(btn).toBeVisible();
  await expect(page.getByRole('button', { name: 'Скачать PDF' })).toHaveCount(0);
  const hint = page.locator('article.report .actions .hint');
  await expect(hint).toBeVisible();
  await expect(hint).toHaveText('В диалоге печати выберите „Сохранить как PDF“');
  // подсказка связана с кнопкой (описание для вспомогательных технологий)
  await expect(btn).toHaveAccessibleDescription('В диалоге печати выберите „Сохранить как PDF“');
  // в печати — ни кнопки, ни подсказки
  await page.emulateMedia({ media: 'print' });
  await expect(hint).toBeHidden();
  await expect(btn).toBeHidden();
  // Ctrl+P без кнопки печатает страницу отчёта: у `file:` виден адрес (`::after`), кнопок
  // «копировать путь» на бумаге нет
  const fileLink = page.locator('article.report .doc a', { hasText: /^данные$/ });
  await expect(fileLink).toHaveAttribute('data-url', LINK_FILE);
  expect(await fileLink.evaluate((a) => getComputedStyle(a, '::after').content)).not.toBe('none');
  await expect(page.locator('article.report .doc button.copy-path').first()).toBeHidden();
  await page.emulateMedia({ media: null });
  expect(await fileLink.evaluate((a) => getComputedStyle(a, '::after').content)).toBe('none');
  await btn.click();
  await expect.poll(() => page.evaluate(() => window.__printCalls.length), { timeout: 30_000 }).toBe(1);
});

test('print_shows_link_address_for_vscode_and_file', async ({ page }) => {
  // M6, задача 15a: в печати и PDF у ссылок `vscode:` и `file:` виден текст адреса
  await printed(page);
  await page.emulateMedia({ media: 'print' });
  const root = page.locator('.print-root');
  // адрес дописывает `::after` из `data-url` (значение `content` движок может отдать и как
  // строку, и как `attr(data-url)` — сам текст проверяется ниже по PDF)
  const after = (scope: typeof root, text: string | RegExp) =>
    scope
      .locator('a', { hasText: typeof text === 'string' ? new RegExp(`^${text}$`) : text })
      .evaluate((a) => ({ url: a.getAttribute('data-url'), content: getComputedStyle(a, '::after').content }));
  const code = await after(root, 'код');
  expect(code.url).toBe(LINK_VSCODE);
  expect(code.content).not.toBe('none');
  const data = await after(root, 'данные');
  expect(data.url).toBe(LINK_FILE);
  expect(data.content).not.toBe('none');
  // http(s) — без дописанного адреса (обычная ссылка PDF); автоссылка — адрес уже в тексте
  expect(await after(root, 'сайт')).toEqual({ url: null, content: 'none' });
  expect(await after(root, new RegExp(LINK_FILE_AUTO.replace(/[/.]/g, '\\$&')))).toEqual({ url: null, content: 'none' });
  // `file:` и в копии без перехода; кнопок в копии нет
  await expect(root.locator('a[href^="file:" i]')).toHaveCount(0);
  await expect(root.locator('button')).toHaveCount(0);
  await expect(root.locator('a', { hasText: /^сайт$/ })).toHaveAttribute('href', LINK_HTTPS);
  // страница отчёта несёт тот же `data-url` (Ctrl+P без кнопки — print_button_pdf_print_with_hint)
  const screen = await page
    .locator('article.report .doc a', { hasText: /^данные$/ })
    .evaluate((a) => ({ url: a.getAttribute('data-url'), content: getComputedStyle(a, '::after').content }));
  expect(screen.url).toBe(LINK_FILE);
  // на экране адрес не дописывается
  await page.emulateMedia({ media: 'screen' });
  expect(
    await page
      .locator('article.report .doc a', { hasText: /^данные$/ })
      .evaluate((a) => getComputedStyle(a, '::after').content),
  ).toBe('none');
  await page.emulateMedia({ media: 'print' });
  const pdf = await page.pdf({ preferCSSPageSize: true });
  writeFileSync(`${out}report-links.pdf`, pdf);
  const text = (await pdfFacts(pdf)).text.replace(/\s+/g, '');
  expect(text).toContain(LINK_VSCODE);
  expect(text).toContain(LINK_FILE);
  expect(text).toContain(LINK_FILE_AUTO);
});

test('report_pdf_contains_images_formula_diagram', async ({ page }) => {
  await printed(page);
  const pdf = await page.pdf({ format: 'A4' });
  writeFileSync(`${out}report.pdf`, pdf);
  const facts = await pdfFacts(pdf);
  // подписи узлов диаграммы — текстом SVG, исходника Mermaid в PDF нет
  expect(facts.text).toContain(PRINT_NODE_A);
  expect(facts.text).toContain(PRINT_NODE_B);
  expect(facts.text).not.toContain('graph LR');
  // формула набрана шрифтами KaTeX
  expect(facts.text).toContain('F1');
  expect(facts.fonts.some((f) => f.includes('KaTeX')), facts.fonts.join(', ')).toBe(true);
  // три PNG отчёта — изображениями на страницах
  expect(facts.images).toBeGreaterThanOrEqual(3);
  // прочие файлы и ссылки — текстом
  expect(facts.text).toContain(PRINT_FILE);
  expect(facts.text).toContain(REPORT_COMMIT);
});

test('acceptance_pdf_from_cli_report', async ({ page }) => {
  // приёмка M5.5 (спецификация §12 п. 1): отчёт записан настоящим `zam report` (global-setup) —
  // страница в WebUI и PDF печатью браузера
  const problems: string[] = [];
  page.on('pageerror', (e) => problems.push(e.message));
  await spyPrint(page);
  if (!adminId) {
    const r = await page.request.get(`${base()}api/v1/auth/me`);
    adminId = ((await r.json()) as { user_id: string }).user_id;
  }
  const id = env('E2E_REPORT_CLI');
  await page.goto(`${base()}#/u/${adminId}/s/${SOLUTION_GUID}/j/reports/${id}`);
  const doc = page.locator(`article.report[data-report="${id}"] .doc`);
  await expect(doc.locator('h1').first()).toContainText(CLI_REPORT_TITLE);
  const imgs = doc.locator('img');
  await expect(imgs).toHaveCount(3);
  for (let i = 0; i < 3; i++) {
    await expect(imgs.nth(i)).toHaveAttribute('src', /\/attachments\/[0-9a-f]{64}\/(loss|acc|lr)\.png$/);
    await expect.poll(() => imgs.nth(i).evaluate((x: HTMLImageElement) => x.complete && x.naturalWidth > 0)).toBe(true);
  }
  await expect(doc.locator('.katex')).toHaveCount(2);
  await expect(doc.locator('.mermaid-diagram svg')).toHaveCount(1);
  await expect(doc.locator('.mermaid-diagram')).toContainText(CLI_NODE_A);
  await pdfButton(page).click();
  await expect.poll(() => page.evaluate(() => window.__printCalls.length), { timeout: 30_000 }).toBe(1);
  const call = (await page.evaluate(() => window.__printCalls))[0];
  expect(call.katex).toBe(2);
  expect(call.diagrams).toBe(1);
  expect(call.imgs).toEqual([true, true, true]);
  const pdf = await page.pdf({ preferCSSPageSize: true });
  writeFileSync(`${out}cli-report.pdf`, pdf);
  const facts = await pdfFacts(pdf);
  expect(facts.text).toContain(CLI_NODE_A);
  expect(facts.text).toContain(CLI_NODE_B);
  expect(facts.text).not.toContain('graph LR');
  expect(facts.fonts.some((f) => f.includes('KaTeX')), facts.fonts.join(', ')).toBe(true);
  expect(facts.images).toBeGreaterThanOrEqual(3);
  expect(problems, problems.join('\n')).toEqual([]);
});

/** Текст, шрифты, число изображений и размеры страниц PDF (pdfjs-dist, без отрисовки). */
async function pdfFacts(buf: Buffer) {
  const pdfjs = await import('pdfjs-dist/legacy/build/pdf.mjs');
  const task = pdfjs.getDocument({
    data: new Uint8Array(buf),
    isEvalSupported: false,
    verbosity: 0,
    // имена шрифтов (`AAAAAA+KaTeX_Main-Regular`) — в объектах шрифтов страницы
    fontExtraProperties: true,
  });
  const doc = await task.promise;
  const imageOps = new Set([
    pdfjs.OPS.paintImageXObject,
    pdfjs.OPS.paintInlineImageXObject,
    pdfjs.OPS.paintImageXObjectRepeat,
  ]);
  let text = '';
  let images = 0;
  const fonts = new Set<string>();
  const sizes: [number, number][] = [];
  try {
    for (let n = 1; n <= doc.numPages; n++) {
      const p = await doc.getPage(n);
      const vp = p.getViewport({ scale: 1 });
      sizes.push([vp.width, vp.height]);
      const tc = await p.getTextContent();
      text += tc.items.map((i) => ('str' in i ? i.str : '')).join('') + '\n';
      const ops = await p.getOperatorList();
      ops.fnArray.forEach((fn, k) => {
        if (imageOps.has(fn)) images++;
        if (fn === pdfjs.OPS.setFont) {
          try {
            fonts.add((p.commonObjs.get(ops.argsArray[k][0]) as { name: string }).name);
          } catch {
            /* шрифт не загружен — не важен */
          }
        }
      });
    }
  } finally {
    await task.destroy();
  }
  return { text, images, fonts: [...fonts], sizes };
}
