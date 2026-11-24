// Снимки страниц M7.3 «до/после» (задача 8; образец — режим съёмки `forms.spec.ts`, §6.14 п. 9):
// при `E2E_FORMS_SNAP=before|after` — страница в трёх ширинах (`phone` с касанием, `tablet`,
// `desktop`) в `.tmp/e2e/screens/forms/<режим>/page-<имя>-<ширина>.png` и строка в `list.tsv`
// рядом, без проверок нового поведения: «до» — на сборке без страниц M7.3 (адрес ведёт на стартовую),
// «после» — в конце задачи. Без `E2E_FORMS_SNAP` тестов нет.
import { expect, test, type Page } from '@playwright/test';
import { appendFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { env, pageCtx, type PageCtx } from './mobile-pages';
import { VIEWPORTS, touch, type ViewportName } from './viewports';

const SNAP = process.env.E2E_FORMS_SNAP ?? '';
const WIDTHS: { name: ViewportName; touch: boolean }[] = [
  { name: 'phone', touch: true },
  { name: 'tablet', touch: false },
  { name: 'desktop', touch: false },
];

/**
 * Снимки страницы `id` (`path` — адрес): `prepare` — до перехода (подстановки `page.route`),
 * `after` — после (раскрыть фильтр, открыть запись); сбой `after` — строкой в списке, не ошибкой.
 */
export function pageSnapshots(
  id: string,
  path: (c: PageCtx) => string,
  opts: { prepare?: (p: Page) => Promise<void>; after?: (p: Page) => Promise<void> } = {},
): void {
  if (!SNAP) return;
  const dir = fileURLToPath(new URL(`../../.tmp/e2e/screens/forms/${SNAP}/`, import.meta.url));
  for (const w of WIDTHS) {
    test.describe(`снимки ${SNAP} · страница ${id} · ${w.name}`, () => {
      test.use({ viewport: VIEWPORTS[w.name], ...(w.touch ? touch : {}) });
      test(`page_snapshot ${id} ${w.name}`, async ({ page }) => {
        mkdirSync(dir, { recursive: true });
        page.setDefaultTimeout(8_000);
        await opts.prepare?.(page);
        await page.goto(`${env('E2E_BASE_URL')}${path(await pageCtx(page))}`);
        await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
        await page.waitForTimeout(800);
        let state = 'страница';
        try {
          await opts.after?.(page);
          await page.waitForTimeout(400);
        } catch (e) {
          state = `не открылась: ${(e as Error).message.split('\n')[0]}`;
        }
        const heading = ((await page.locator('main h1, main h2').first().textContent().catch(() => '')) ?? '').trim();
        await page.screenshot({ path: `${dir}page-${id}-${w.name}.png`, fullPage: true });
        appendFileSync(`${dir}list.tsv`, `page-${id}\t${w.name}\t${heading}\t${state}\n`);
      });
    });
  }
}
