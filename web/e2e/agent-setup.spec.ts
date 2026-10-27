// «Подключение агента» (§15.5) и «Настройки → Шаблон инструкции» (задача 13 M4): опции меняют
// фрагменты, «Копировать» по HTTP (без Clipboard API), выпуск токена — сразу переход на страницу
// с токеном в командах установки, типы подсказок хуков пользователя на сервере (R5).
import { expect, test, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const screens = fileURLToPath(new URL('../../.tmp/e2e/screens/', import.meta.url));

test.use({ storageState: ADMIN_STATE });

function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected =
      (r.status() === 401 && path.endsWith('/api/v1/auth/me')) ||
      (r.status() === 404 && path.endsWith('/favicon.ico')) ||
      // предпросмотр шаблона без {url} — ожидаемый 400 с причиной
      (r.status() === 400 && path.endsWith('/api/v1/admin/agent-setup/preview'));
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

const frag = (page: Page, title: string | RegExp) => page.getByRole('region', { name: title });
const GUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/;

test('Подключение агента: опции меняют фрагменты, копирование, хуки пользователя', async ({ page, context }) => {
  const problems = watch(page);
  // WebUI не передаёт токен в адресе запроса (fix round 1, I-1); ответ не кэшируется
  const setupUrls: string[] = [];
  const cacheControl: string[] = [];
  page.on('response', (r) => {
    if (new URL(r.url()).pathname.endsWith('/api/v1/agent-setup')) {
      setupUrls.push(r.url());
      cacheControl.push(r.headers()['cache-control'] ?? '');
    }
  });
  await page.goto(base());
  await page.getByRole('link', { name: 'Подключение агента' }).click();
  await expect(page).toHaveURL(/#\/agent-setup$/);
  await expect(page.getByRole('heading', { level: 2, name: 'Подключение агента' })).toBeVisible();

  // Claude Code по умолчанию: блок с маркером версии, MCP и хуки Claude Code
  const block = frag(page, 'Текст для CLAUDE.md');
  await expect(block).toContainText('<!-- zeroaimemory:begin v');
  await expect(block).toContainText('ZeroAIMemoryUrl=');
  await expect(block).not.toContainText('zam_');
  await expect(frag(page, 'Фрагмент .mcp.json')).toContainText('Bearer ${ZAM_TOKEN}');
  await expect(frag(page, 'Хуки Claude Code')).toContainText('PostToolUseFailure');
  await expect(frag(page, 'solution.toml')).toHaveCount(0);
  await expect(frag(page, /zam init — Linux/)).toContainText('zam init');
  await expect(page.getByRole('link', { name: 'Windows x64' })).toHaveAttribute('href', 'downloads/zam/windows-x64');

  // Codex: AGENTS.md и хуки Codex; AGENTS.md без хуков — хуков нет
  await page.getByLabel('Codex (AGENTS.md и хуки Codex)').check();
  await expect(frag(page, 'Текст для AGENTS.md')).toBeVisible();
  await expect(frag(page, 'Хуки Codex')).toContainText('zam hook post-tool --agent codex');
  await expect(frag(page, 'Хуки Claude Code')).toHaveCount(0);
  await page.getByLabel('AGENTS.md (Codex, Cursor и др.)').check();
  await expect(frag(page, 'Хуки Codex')).toHaveCount(0);
  await expect(frag(page, /zam init — Linux/)).toContainText('--no-hooks');
  await page.getByLabel('Claude Code (CLAUDE.md)').check();
  await expect(frag(page, 'Хуки Claude Code')).toBeVisible();

  // MCP выключен — фрагмента нет
  await page.getByLabel('MCP (.mcp.json)').uncheck();
  await expect(frag(page, 'Фрагмент .mcp.json')).toHaveCount(0);
  await page.getByLabel('MCP (.mcp.json)').check();
  await expect(frag(page, 'Фрагмент .mcp.json')).toBeVisible();

  // новое решение: GUID один и тот же при смене опций (его выдал сервер при первом запросе)
  await page.getByLabel('Новое (GUID выдаст сервер)').check();
  await page.getByLabel('Имя решения').fill('Проект E2E');
  const toml = frag(page, 'solution.toml');
  await expect(toml).toContainText('Проект E2E');
  const guid = ((await toml.textContent()) ?? '').match(GUID)?.[0];
  expect(guid).toBeTruthy();
  await expect(frag(page, 'Фрагмент .mcp.json')).toContainText(guid!);
  await page.getByLabel('Язык инструкции').selectOption('en');
  await expect(block).toContainText('ZeroAIMemory memory');
  await expect(toml).toContainText(guid!);
  await expect(block).toContainText(guid!);
  await page.getByLabel('Язык инструкции').selectOption('ru');
  await expect(block).toContainText('Память ZeroAIMemory');

  // запись в Rules: явный выбор меняет текст блока
  await page.getByLabel('Запись в Rules').selectOption('off');
  const off = (await block.locator('pre').textContent()) ?? '';
  await page.getByLabel('Запись в Rules').selectOption('on');
  await expect(block.locator('pre')).not.toHaveText(off);

  // «Копировать» по HTTP: запасной путь, в буфере — текст блока
  await block.getByRole('button', { name: 'Копировать' }).click();
  await expect(block.getByRole('button', { name: 'Скопировано' })).toBeVisible();
  await expect(block.getByRole('status')).toHaveText('Скопировано в буфер обмена.');
  const text = (await block.locator('pre').textContent()) ?? '';
  const port = new URL(base()).port;
  await context.grantPermissions(['clipboard-read'], { origin: `http://localhost:${port}` });
  const probe = await context.newPage();
  await probe.goto(`http://localhost:${port}/`);
  // буфер Windows хранит переводы строк как CRLF
  expect((await probe.evaluate(() => navigator.clipboard.readText())).replace(/\r\n/g, '\n')).toBe(text);
  await probe.close();

  // снимок — до выпуска токена: в артефактах прогона токена нет (fix round 1, I-3)
  await page.screenshot({ path: `${screens}agent-setup.png`, fullPage: true });

  // токен тут же: окно токена, токен — в командах установки и zam init, не в блоке
  await page.getByLabel('Имя токена').fill('e2e-setup');
  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  const dialog = page.getByRole('dialog');
  const plain = await dialog.getByLabel('Токен').inputValue();
  expect(plain).toMatch(/^zam_/);
  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  await expect(frag(page, /zam init — Windows/)).toContainText(plain);
  // Windows задаёт ZAM_TOKEN пользователя; Linux — только совет (токен сохраняет zam init)
  await expect(frag(page, /Установка zam — Windows/)).toContainText(plain);
  await expect(frag(page, /Установка zam — Linux/)).not.toContainText(plain);
  await expect(block).not.toContainText(plain);
  await expect(frag(page, /zam init — Linux/)).toContainText(`--token '${plain}'`);
  expect(setupUrls.length).toBeGreaterThan(3);
  expect(setupUrls.filter((u) => /[?&]token=/.test(u))).toEqual([]);
  expect(setupUrls.some((u) => u.includes('token_placeholder=true'))).toBe(true);
  expect(setupUrls.every((u) => !u.includes(plain))).toBe(true);
  expect(new Set(cacheControl)).toEqual(new Set(['no-store']));

  // подсказки хуков: настройка пользователя на сервере
  const flags = () => page.getByRole('group', { name: 'Типы подсказок' });
  const prompt = () => flags().getByLabel(/Подсказки к запросу пользователя/);
  await expect(prompt()).toBeChecked();
  const saved = page.waitForResponse((r) => r.url().includes('/settings') && r.request().method() === 'PATCH');
  await prompt().uncheck();
  expect((await (await saved).json()).hooks.prompt).toBe(false);
  await page.reload();
  await expect(prompt()).not.toBeChecked();
  const me = await page.request.get(`${base()}api/v1/auth/settings`);
  expect((await me.json()).hooks).toMatchObject({ prompt: false, session_start: true });
  await prompt().check();
  await expect(prompt()).toBeChecked();
  await expect
    .poll(async () => (await (await page.request.get(`${base()}api/v1/auth/settings`)).json()).hooks.prompt)
    .toBe(true);

  expect(problems, problems.join('\n')).toEqual([]);
});

test('Выпуск токена в «Пользователях» — сразу «Подключение агента»', async ({ page }) => {
  const problems = watch(page);
  await page.goto(`${base()}#/admin`);
  const row = page.getByRole('row').filter({ hasText: '(админ)' }).first();
  await row.getByRole('button', { name: 'Токены' }).click();
  await page.getByPlaceholder('Имя токена').fill('e2e-jump');
  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  await expect(page).toHaveURL(/#\/agent-setup$/);
  const dialog = page.getByRole('dialog');
  const plain = await dialog.getByLabel('Токен').inputValue();
  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  await expect(frag(page, /zam init — Linux/)).toContainText(plain);
  // токен живёт только в памяти вкладки: после F5 в командах его нет, окно не открывается
  await page.reload();
  await expect(frag(page, /zam init — Linux/)).toBeVisible();
  await expect(frag(page, /zam init — Linux/)).not.toContainText(plain);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Шаблон инструкции: предпросмотр, сохранение с новой версией, сброс', async ({ page }) => {
  const problems = watch(page);
  await page.goto(`${base()}#/admin/settings`);
  const tpl = page.getByRole('region', { name: 'Шаблон инструкции' });
  await expect(tpl.getByTestId('tpl-version')).toHaveText(/^v\d+$/);
  const v0 = Number(((await tpl.getByTestId('tpl-version').textContent()) ?? '').slice(1));
  await expect(tpl.getByTestId('tpl-preview')).toContainText(`<!-- zeroaimemory:begin v${v0} -->`);
  await expect(tpl.getByRole('button', { name: 'Сохранить' })).toBeDisabled();

  // без {url} — ошибка в предпросмотре, сохранить нельзя
  const area = tpl.getByLabel('Текст шаблона');
  await area.fill('Память без адреса');
  await expect(tpl.getByRole('alert')).toContainText('{url}');
  await expect(tpl.getByRole('button', { name: 'Сохранить' })).toBeDisabled();

  // черновик не теряется при смене языка (fix round 1, M-6)
  await area.fill('## Черновик ru {url}');
  await tpl.getByLabel('Язык шаблона').selectOption('en');
  await expect(area).toHaveValue(/ZeroAIMemory memory/);
  await tpl.getByLabel('Язык шаблона').selectOption('ru');
  await expect(area).toHaveValue('## Черновик ru {url}');

  await area.fill('## Своя память\nАдрес: {url}\nРешение: {solution}. {rules}');
  await expect(tpl.getByTestId('tpl-preview')).toContainText(`<!-- zeroaimemory:begin v${v0 + 1} -->`);
  await expect(tpl.getByTestId('tpl-preview')).toContainText('Решение: «Пример»');
  await tpl.getByRole('button', { name: 'Сохранить' }).click();
  await expect(tpl.getByRole('status')).toHaveText(`Шаблон сохранён, версия блока — v${v0 + 1}.`);
  await expect(tpl.getByTestId('tpl-version')).toHaveText(`v${v0 + 1}`);
  await expect(tpl).toContainText('свой шаблон');

  // «Подключение агента» отдаёт свой текст с новой версией
  await page.getByRole('link', { name: 'Подключение агента' }).click();
  const block = frag(page, 'Текст для CLAUDE.md');
  await expect(block).toContainText(`<!-- zeroaimemory:begin v${v0 + 1} -->`);
  await expect(block).toContainText('## Своя память');

  // сброс к умолчанию — ещё одна версия
  await page.goto(`${base()}#/admin/settings`);
  await tpl.getByRole('button', { name: 'Сбросить к умолчанию' }).click();
  await expect(tpl.getByRole('status')).toHaveText(`Шаблон сброшен к умолчанию, версия блока — v${v0 + 2}.`);
  await expect(tpl).toContainText('шаблон по умолчанию');
  await expect(area).toHaveValue(/ZeroAIMemoryUrl=\{url\}/);
  await page.screenshot({ path: `${screens}settings-template.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
});
