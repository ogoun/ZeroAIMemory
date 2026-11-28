// «Подключение агента» (§15.5) и «Настройки → Шаблон инструкции» (задача 13 M4): опции меняют
// фрагменты, «Копировать» по HTTP (без Clipboard API), выпуск токена — сразу переход на страницу
// с токеном в командах установки, типы подсказок хуков пользователя на сервере (R5).
import { expect, test, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, noHorizontalScroll } from './viewports';

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
  // M6: stdio-прокси zam mcp (привязка к сессии агента)
  await expect(frag(page, 'Фрагмент .mcp.json')).toContainText('"command": "zam"');
  await expect(frag(page, 'Фрагмент .mcp.json')).not.toContainText('ZAM_TOKEN');
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
  // GUID — в блоке CLAUDE.md/AGENTS.md и solution.toml; `.mcp.json` с M6 — stdio `zam mcp` без
  // GUID (решение берётся из solution.toml; журнал M6, задача 8b → 13a)
  await expect(block).toContainText(guid!);
  await expect(frag(page, 'Фрагмент .mcp.json')).not.toContainText(guid!);
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
  await page.getByLabel('Название токена').fill('e2e-setup');
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

// M7.6 §9.1: «Подключить с переносом данных» — задание агенту на перенос проекта от сервера
test('Подключение с переносом данных: задание с сервера, смена агента и языка, копирование, три ширины', async ({ page, context }) => {
  const problems = watch(page);
  await page.goto(`${base()}#/agent-setup`);
  const opt = page.getByLabel('Подключить с переносом данных');
  await expect(opt).toBeEnabled();
  await expect(opt).not.toBeChecked();
  const task = frag(page, 'Задание агенту на перенос');
  await expect(task).toHaveCount(0);

  await page.getByLabel('Новое (GUID выдаст сервер)').check();
  await page.getByLabel('Имя решения').fill('Перенос E2E');
  await opt.check();
  const text = task.getByLabel('Текст задания на перенос');
  await expect(text).toHaveValue(/Решение: «Перенос E2E» \(`[0-9a-f-]{36}`\)/);
  await expect(text).toHaveValue(/~\/\.claude\/projects\//);
  await expect(text).toHaveAttribute('readonly', '');
  // блок — сразу под текстом для CLAUDE.md; токена в задании нет
  const order = await page.locator('section.frag').evaluateAll((els) => els.map((e) => e.getAttribute('aria-label')));
  expect(order.slice(0, 2)).toEqual(['Текст для CLAUDE.md', 'Задание агенту на перенос']);
  await expect(text).not.toHaveValue(/zam_|<токен>/);

  // агент Codex и язык — текст обновляется
  await page.getByLabel('Codex (AGENTS.md и хуки Codex)').check();
  await expect(text).toHaveValue(/~\/\.codex\/sessions\//);
  await expect(text).not.toHaveValue(/~\/\.claude\/projects\//);
  await page.getByLabel('Язык инструкции').selectOption('en');
  await expect(text).toHaveValue(/^# Task: move the project into ZeroAIMemory memory/);
  await page.getByLabel('Язык инструкции').selectOption('ru');
  await page.getByLabel('Claude Code (CLAUDE.md)').check();
  await expect(text).toHaveValue(/~\/\.claude\/projects\//);

  // «Копировать» — в буфере текст задания
  await task.getByRole('button', { name: 'Копировать' }).click();
  await expect(task.getByRole('button', { name: 'Скопировано' })).toBeVisible();
  await expect(task.getByRole('status')).toHaveText('Скопировано в буфер обмена.');
  const value = await text.inputValue();
  const port = new URL(base()).port;
  await context.grantPermissions(['clipboard-read'], { origin: `http://localhost:${port}` });
  const probe = await context.newPage();
  await probe.goto(`http://localhost:${port}/`);
  expect((await probe.evaluate(() => navigator.clipboard.readText())).replace(/\r\n/g, '\n')).toBe(value);
  await probe.close();

  // три ширины: без горизонтальной прокрутки; снимки — в .tmp/e2e/screens
  for (const w of ['phone', 'tablet', 'desktop'] as const) {
    await page.setViewportSize(VIEWPORTS[w]);
    await expect(text).toBeVisible();
    expect(await noHorizontalScroll(page), w).toBe(true);
    await page.screenshot({ path: `${screens}agent-setup-migrate-${w}.png`, fullPage: true });
    // окно с блоком задания (снимок всей страницы на телефоне слишком высок для просмотра)
    await task.evaluate((e) => e.scrollIntoView({ block: 'start' }));
    await page.screenshot({ path: `${screens}agent-setup-migrate-${w}-view.png` });
  }

  // галочка снята — блока нет
  await opt.uncheck();
  await expect(task).toHaveCount(0);
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Сервер без признака agent_setup_migrate — галочка выключена с пояснением', async ({ page }) => {
  const problems = watch(page);
  await page.route('**/api/v1/health', async (route) => {
    const r = await route.fetch();
    const j = await r.json();
    j.features = (j.features ?? []).filter((f: string) => f !== 'agent_setup_migrate');
    await route.fulfill({ response: r, json: j });
  });
  const setupUrls: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/api/v1/agent-setup')) setupUrls.push(r.url());
  });
  await page.goto(`${base()}#/agent-setup`);
  const opt = page.getByLabel('Подключить с переносом данных');
  await expect(opt).toBeDisabled();
  await expect(page.getByText('Сервер старше 0.0.19: задания на перенос нет — обновите сервер.')).toBeVisible();
  await expect(frag(page, 'Текст для CLAUDE.md')).toBeVisible();
  await expect(frag(page, 'Задание агенту на перенос')).toHaveCount(0);
  expect(setupUrls.some((u) => u.includes('migrate='))).toBe(false);
  expect(problems, problems.join('\n')).toEqual([]);
});

// M7.6 (решения координатора 141–143): «Название токена» — подпись и пояснение; пустое на
// «Аккаунте» — ошибка под полем; на «Подключении агента» — название по умолчанию, «Отозвать этот
// токен» возвращает заглушку, отозванный — в «Отозванные (N)»
test('Название токена: пустое на «Аккаунте» — ошибка под полем и фокус', async ({ page }) => {
  const problems = watch(page);
  const posts: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/api/v1/auth/tokens')) posts.push(r.url());
  });
  await page.goto(`${base()}#/account`);
  const field = page.getByLabel('Название токена');
  await expect(field).toHaveAttribute('placeholder', 'например: ноутбук, Claude Code');
  await expect(page.getByText('Это не сам токен, а подпись для себя — чтобы отличить его в списке.')).toBeVisible();
  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  const form = page.getByRole('form', { name: 'Выпуск токена' });
  await expect(form.getByRole('alert')).toHaveText('Укажите название токена');
  await expect(field).toBeFocused();
  await expect(field).toHaveAttribute('aria-invalid', 'true');
  expect(posts).toEqual([]);
  await field.fill('e2e-name');
  await expect(form.getByRole('alert')).toHaveCount(0);
  await expect(page.getByText('Отозвать — токен перестаёт работать сразу и навсегда; строка остаётся в истории.')).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Выпуск на «Подключении агента»: название по умолчанию, «Отозвать этот токен», список отозванных', async ({ page }) => {
  const problems = watch(page);
  const day = new Date();
  const p2 = (n: number) => String(n).padStart(2, '0');
  const date = `${day.getFullYear()}-${p2(day.getMonth() + 1)}-${p2(day.getDate())}`;
  await page.goto(`${base()}#/agent-setup`);
  const field = page.getByLabel('Название токена');
  await expect(field).toHaveValue(`Claude Code · новое решение · ${date}`);
  // следует за агентом и решением, пока не правили
  await page.getByLabel('Codex (AGENTS.md и хуки Codex)').check();
  await page.getByLabel('Новое (GUID выдаст сервер)').check();
  await page.getByLabel('Имя решения').fill('Токен E2E');
  const name = `Codex · Токен E2E · ${date}`;
  await expect(field).toHaveValue(name);
  await expect(page.getByRole('link', { name: '«Мои токены»' })).toHaveAttribute('href', '#/account');
  await expect(page.getByRole('link', { name: 'токены пользователей' })).toHaveAttribute('href', '#/admin');
  await expect(page.getByText('«Администрирование» → «Пользователи»')).toBeVisible();

  await page.getByRole('button', { name: 'Выпустить токен' }).click();
  const dialog = page.getByRole('dialog');
  const plain = await dialog.getByLabel('Токен').inputValue();
  await dialog.getByRole('button', { name: 'Закрыть' }).click();
  const init = frag(page, /zam init — Linux/);
  await expect(init).toContainText(`--token '${plain}'`);

  // отзыв: подтверждение, после — заглушка в командах
  await page.getByRole('button', { name: 'Отозвать этот токен' }).click();
  const ask = page.getByRole('dialog', { name: 'Отозвать токен?' });
  await expect(ask).toContainText(name);
  await ask.getByRole('button', { name: 'Отозвать', exact: true }).click();
  await expect(ask).toBeHidden();
  await expect(page.getByTestId('setup-token-notice')).toHaveText(`Токен «${name}» отозван: в командах снова заглушка <токен>.`);
  await expect(page.getByTestId('setup-token-notice')).toBeFocused();
  await expect(init).not.toContainText(plain);
  await expect(page.getByRole('button', { name: 'Отозвать этот токен' })).toHaveCount(0);
  expect((await fetch(`${base()}api/v1/spaces`, { headers: { Authorization: `Bearer ${plain}` } })).status).toBe(401);

  // «Аккаунт»: токен — в свёрнутом списке отозванных
  await page.getByRole('link', { name: '«Мои токены»' }).click();
  await expect(page).toHaveURL(/#\/account$/);
  const active = page.getByRole('table', { name: 'Мои токены' });
  await expect(active).toBeVisible();
  await expect(active.locator(`tr[data-token="${name}"]`)).toHaveCount(0);
  const summary = page.getByText(/^Отозванные \(\d+\)$/);
  await expect(page.getByRole('table', { name: 'Отозванные токены' })).toBeHidden();
  await summary.click();
  const row = page.getByRole('table', { name: 'Отозванные токены' }).locator(`tr[data-token="${name}"]`);
  await expect(row).toBeVisible();
  await expect(row).toContainText('отозван');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('Выпуск токена в «Пользователях» — сразу «Подключение агента»', async ({ page }) => {
  const problems = watch(page);
  await page.goto(`${base()}#/admin`);
  const row = page.getByRole('row').filter({ hasText: '(админ)' }).first();
  await row.getByRole('button', { name: 'Токены' }).click();
  await page.getByLabel('Название токена').fill('e2e-jump');
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

  // сброс к умолчанию — ещё одна версия; с подтверждением (M7.3, §6.14 правило Д)
  await page.goto(`${base()}#/admin/settings`);
  await tpl.getByRole('button', { name: 'Сбросить к умолчанию' }).click();
  await page.getByRole('dialog', { name: 'Сбросить шаблон к умолчанию?' }).getByRole('button', { name: 'Сбросить', exact: true }).click();
  await expect(tpl.getByRole('status')).toHaveText(`Шаблон сброшен к умолчанию, версия блока — v${v0 + 2}.`);
  await expect(tpl).toContainText('шаблон по умолчанию');
  await expect(area).toHaveValue(/ZeroAIMemoryUrl=\{url\}/);
  await page.screenshot({ path: `${screens}settings-template.png`, fullPage: true });
  expect(problems, problems.join('\n')).toEqual([]);
});
