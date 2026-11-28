// Страница Vault: создание, правка и удаление записи в WebUI (M7.6, решение 167; окно записи
// `VaultForm`). Значение — только для записи: после сохранения не показывается, при правке пусто =
// прежнее значение (сервер перешифровывает его под новую ревизию) — проверяется «Показать».
import { expect, test, type Page } from '@playwright/test';
import { pageCtx } from './mobile-pages';
import { SOLUTION_GUID } from './seed';
import { ADMIN_STATE } from './session';

test.use({ storageState: ADMIN_STATE });

const base = () => {
  const v = process.env.E2E_BASE_URL;
  if (!v) throw new Error('E2E_BASE_URL не задан (global-setup)');
  return v;
};

/** Ошибки JS и HTTP ≥ 400 страницы (кроме ожидаемых). */
function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected = (r.status() === 401 && path.endsWith('/api/v1/auth/me')) || (r.status() === 404 && path.endsWith('/favicon.ico'));
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

test('vault_create_edit_keeps_value_delete', async ({ page }) => {
  const problems = watch(page);
  const c = await pageCtx(page);
  await page.goto(`${base()}#/u/${c.user}/s/${SOLUTION_GUID}/j/vault`);
  await expect(page.getByRole('heading', { level: 1, name: 'Vault' })).toBeVisible();
  const title = `E2E пароль ${Date.now().toString(36)}`;
  // значение — склейкой (фиктивный секрет не целиком в коде)
  const value = ['e2e', 'vault', 'form', 'value'].join('-');

  await page.getByRole('button', { name: 'Новая запись', exact: true }).click();
  const d = page.getByRole('dialog', { name: 'Новая запись Vault', exact: true });
  await expect(d.getByLabel('Название')).toBeFocused();
  await d.getByLabel('Название').fill(title);
  await d.getByLabel('Тип').selectOption({ label: 'пароль' });
  await d.getByLabel('Пользователь').fill('e2e-user');
  await d.getByLabel('Адрес (ссылка)').fill('https://service.example.com');
  await expect(d.getByLabel('Значение')).toHaveAttribute('type', 'password');
  await d.getByLabel('Значение').fill(value);
  await d.getByRole('button', { name: 'Создать', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: `Запись «${title}» создана.` })).toBeVisible();
  const item = page.locator('li.item', { hasText: title });
  await expect(item).toContainText('e2e-user');
  await expect(item).toContainText('https://service.example.com');
  expect(await page.content()).not.toContain(value);

  // правка без значения: адрес новый, значение прежнее
  await item.getByRole('button', { name: `Изменить ${title}`, exact: true }).click();
  const e = page.getByRole('dialog', { name: 'Изменить запись Vault', exact: true });
  await expect(e.getByLabel('Значение')).toHaveValue('');
  await e.getByLabel('Адрес (ссылка)').fill('https://service2.example.com');
  await e.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await expect(e).toBeHidden();
  await expect(item).toContainText('https://service2.example.com');
  await expect(item).toContainText('ревизия 2');
  await item.getByRole('button', { name: 'Показать', exact: true }).click();
  await expect(item.getByLabel(`Значение ${title}`)).toHaveValue(value);
  await item.getByRole('button', { name: 'Скрыть', exact: true }).click();

  // телефон: без горизонтальной прокрутки, окно — во весь экран
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await item.getByRole('button', { name: `Изменить ${title}`, exact: true }).click();
  await expect(e).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await e.getByRole('button', { name: 'Закрыть', exact: true }).first().click();
  await expect(e).toBeHidden();
  await page.setViewportSize({ width: 1280, height: 900 });

  // удаление — с подтверждением
  await item.getByRole('button', { name: `Удалить ${title}`, exact: true }).click();
  const r = page.getByRole('dialog', { name: 'Удалить запись Vault', exact: true });
  await expect(r).toContainText(title);
  await r.getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect(r).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: `Запись «${title}» удалена.` })).toBeVisible();
  await expect(page.locator('li.item', { hasText: title })).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});
