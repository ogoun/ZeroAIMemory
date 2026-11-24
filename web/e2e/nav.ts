// Разделы шапки в e2e (M7.3, ревью r1 задачи 8, I2): разделы, которые не помещаются в шапку, — в
// меню «Ещё ▾» (`NavMore.svelte`), поэтому раздел ищется в полосе, а нет его там — в меню.
import { expect, type Locator, type Page } from '@playwright/test';

/** Полоса разделов шапки (на телефоне её нет — разделы в выдвижной панели). */
export const sectionsNav = (page: Page): Locator => page.locator('header.topbar').getByRole('navigation', { name: 'Разделы' });

/** Кнопка «Ещё ▾» полосы разделов. */
export const moreButton = (page: Page): Locator => sectionsNav(page).getByRole('button', { name: /^Ещё/ });

/**
 * Раздел `name` шапки: ссылка полосы, а если раздел в «Ещё» — пункт меню (меню открывается). Полоса
 * дожидается замера: видна ссылка или кнопка «Ещё».
 */
export async function sectionItem(page: Page, name: string): Promise<Locator> {
  const nav = sectionsNav(page);
  const link = nav.getByRole('link', { name, exact: true });
  await expect(link.or(moreButton(page)).first()).toBeVisible();
  if (await link.isVisible()) return link;
  const more = moreButton(page);
  if ((await more.getAttribute('aria-expanded')) !== 'true') await more.click();
  const item = nav.getByRole('menu').getByRole('menuitem', { name, exact: true });
  await expect(item).toBeVisible();
  return item;
}

/** Перейти в раздел шапки (через «Ещё», если раздел там). */
export async function openSection(page: Page, name: string): Promise<void> {
  await (await sectionItem(page, name)).click();
}

/** Имена всех разделов шапки по порядку: полоса, затем меню «Ещё» (открытое или нет). */
export function sectionNames(page: Page): Promise<string[]> {
  return sectionsNav(page)
    .locator('a[href]')
    .evaluateAll((els) =>
      els.map(
        (e) =>
          e.getAttribute('aria-label') ??
          [...e.childNodes]
            .filter((n) => !(n instanceof Element && n.getAttribute('aria-hidden') === 'true'))
            .map((n) => n.textContent ?? '')
            .join('')
            .trim(),
      ),
    );
}
