// «Кандидаты в Rules» (M7.3, спецификация §6.9 п. 3, §6.7 пп. 4–5): кандидат агента (`POST
// /proposals` по токену) — в списке и карточке (`JournalSplit`): путь, текст, довод, автор;
// «Принять» — окно формы с правкой пути и текста, суммой Rules в дайджесте и приростом, после — строка
// со ссылкой на созданную заметку Rules и «принят»; «Отклонить» — окно с причиной; метка дерева
// «кандидаты: N» у Rules и «Кандидаты N» в шапке — одно число, после решения из WebUI — новое.
import { expect, test, type Page } from '@playwright/test';
import { pageSnapshots } from './page-snaps';
import { agent } from './seed-internal';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const stamp = () => Date.now().toString(36);

test.use({ storageState: ADMIN_STATE });
test.describe.configure({ mode: 'serial' });

pageSnapshots('proposals', () => `#/proposals/${process.env.E2E_PROPOSAL_FORMS}`);

function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected = (r.status() === 401 && path.endsWith('/api/v1/auth/me')) || (r.status() === 404 && path.endsWith('/favicon.ico'));
    if (r.status() >= 400 && !expected) problems.push(`HTTP ${r.status()} ${r.request().method()} ${path}`);
  });
  return problems;
}

/** Кандидат агента: путь Rules, текст правила, довод. */
async function candidate(path: string, content: string): Promise<string> {
  const r = await agent<{ proposal: string }>('POST', 'proposals', {
    type: 'rule',
    path,
    content,
    reason: 'Повторялось в нескольких проектах подряд — это поведение агента, а не факт одного проекта.',
  });
  return r.proposal;
}

const card = (page: Page) => page.getByRole('article', { name: 'Карточка кандидата' });

test('proposals_rule_accept_creates_note', async ({ page }) => {
  const problems = watch(page);
  const path = `e2e/итог-${stamp()}`;
  const content = 'Заканчивая задачу, пиши итог одной строкой: что сделано и что осталось.';
  const id = await candidate(path, content);
  await page.goto(`${base()}#/proposals`);
  await expect(page.getByRole('heading', { name: 'Кандидаты в Rules', exact: true })).toBeVisible();
  // список: путь, начало текста, автор — агент
  const row = page.locator(`li[data-proposal="${id}"]`);
  await expect(row).toContainText(`rules:/${path}`);
  await expect(row).toContainText('агент');
  await row.getByRole('button', { name: `rules:/${path}` }).click();
  // карточка на виду: заголовок в фокусе, ряд решения — под шапкой
  await expect(card(page)).toBeVisible();
  await expect(card(page).getByRole('heading', { name: `rules:/${path}` })).toBeFocused();
  await expect(card(page).getByTestId('proposal-status')).toHaveText('ждёт решения');
  await expect(card(page).getByTestId('proposal-content')).toHaveText(content);
  await expect(card(page)).toContainText('Повторялось в нескольких проектах подряд');

  // «Принять»: окно с правкой пути и текста, сумма Rules и прирост
  await card(page).getByRole('button', { name: 'Принять', exact: true }).click();
  const d = page.getByRole('dialog', { name: 'Принять кандидата', exact: true });
  await expect(d).toBeVisible();
  await expect(d.getByLabel('Путь в Rules')).toBeFocused();
  await expect(d.getByLabel('Путь в Rules')).toHaveValue(path);
  await expect(d.getByLabel('Текст правила')).toHaveValue(content);
  const tokens = await d.getByTestId('proposal-tokens').innerText();
  const m = /^Rules в каждой сессии: ([\d ]+) → ([\d ]+) токенов \(\+([\d ]+)\)/.exec(tokens);
  expect(m, tokens).not.toBeNull();
  const n = (s: string) => Number(s.replace(/ /g, ''));
  expect(n(m![2])).toBeGreaterThan(n(m![1]));
  expect(n(m![2]) - n(m![1])).toBe(n(m![3]));
  // правка пути — человек вправе
  const finalPath = `${path}-принято`;
  await d.getByLabel('Путь в Rules').fill(finalPath);
  await d.getByRole('button', { name: 'Принять', exact: true }).click();
  await expect(d).toBeHidden();
  // строка со ссылкой на созданную заметку, карточка — «принят»
  await expect(card(page).getByTestId('proposal-status')).toHaveText('принят');
  const decided = card(page).getByTestId('proposal-decided');
  await expect(decided).toContainText('Принято: заметка Rules создана');
  const link = decided.getByRole('link', { name: `rules:/${finalPath}` });
  await expect(link).toBeVisible();
  // заметка Rules — по правленому пути, от человека, с происхождением в истории
  const note = await agent<{ node: { id: string; path: string }; content: string }>(
    'GET',
    `nodes/by-path?space=rules&path=${encodeURIComponent(finalPath)}&format=json`,
  );
  expect(note.content).toContain('Заканчивая задачу, пиши итог одной строкой');
  const p = await agent<{ status: string; result_ref: string }>('GET', `proposals/${id}`);
  expect(p.status).toBe('accepted');
  expect(p.result_ref).toBe(`n:${note.node.id}`);
  // ссылка ведёт на заметку
  await link.click();
  await expect(page).toHaveURL(new RegExp(`/s/rules/n/${note.node.id}$`));
  expect(problems, problems.join('\n')).toEqual([]);
});

test('proposals_decline', async ({ page }) => {
  const problems = watch(page);
  const path = `e2e/отклонить-${stamp()}`;
  const id = await candidate(path, 'Перед каждым ответом перечитывай весь журнал проекта целиком, без исключений.');
  await page.goto(`${base()}#/proposals/${id}`);
  await expect(card(page)).toBeVisible();
  await card(page).getByRole('button', { name: 'Отклонить', exact: true }).click();
  const d = page.getByRole('dialog', { name: 'Отклонить кандидата', exact: true });
  await expect(d.getByLabel('Причина')).toBeFocused();
  await d.getByLabel('Причина').fill('слишком дорого для каждой сессии');
  await d.getByRole('button', { name: 'Отклонить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(card(page).getByTestId('proposal-status')).toHaveText('отклонён');
  await expect(card(page).getByTestId('proposal-decided')).toContainText('Кандидат отклонён');
  await expect(card(page)).toContainText('слишком дорого для каждой сессии');
  await expect(card(page).getByRole('button', { name: 'Принять', exact: true })).toHaveCount(0);
  const p = await agent<{ status: string; decision_note: string | null }>('GET', `proposals/${id}`);
  expect(p).toMatchObject({ status: 'declined', decision_note: 'слишком дорого для каждой сессии' });
  // в «ждут решения» его нет, в «отклонённые» — есть
  await expect(page.locator(`li[data-proposal="${id}"]`)).toHaveCount(0);
  await page.getByLabel('Статус кандидатов').selectOption('declined');
  await expect(page.locator(`li[data-proposal="${id}"]`)).toContainText('отклонён');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tree_rules_candidates_label', async ({ page }) => {
  const problems = watch(page);
  const before = (await agent<{ rules: number }>('GET', 'proposals/counts')).rules;
  const a = await candidate(`e2e/метка-a-${stamp()}`, 'Короткие ответы человеку — сначала вывод, затем подробности по запросу.');
  await candidate(`e2e/метка-b-${stamp()}`, 'Не повторяй человеку то, что он только что сказал: сразу к делу.');
  const me = (await (await page.request.get(`${base()}api/v1/auth/me`)).json()) as { user_id: string };
  await page.goto(`${base()}#/u/${me.user_id}`);
  const tree = page.locator('nav[aria-label="Дерево заметок"]');
  // у Rules своего пользователя — «кандидаты: N» (как «запросы: N» у решений), в шапке — то же число
  const label = tree.getByRole('link', { name: `кандидаты: ${before + 2}` });
  await expect(label).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Разделы' }).getByTestId('candidates-count')).toHaveText(String(before + 2));
  // решение из WebUI — число перечитывается (`changesCounts`), без перезагрузки
  await label.click();
  await expect(page).toHaveURL(/#\/proposals$/);
  await page.locator(`li[data-proposal="${a}"]`).getByRole('button').first().click();
  await card(page).getByRole('button', { name: 'Отклонить', exact: true }).click();
  await page.getByRole('dialog', { name: 'Отклонить кандидата' }).getByRole('button', { name: 'Отклонить', exact: true }).click();
  await expect(card(page).getByTestId('proposal-status')).toHaveText('отклонён');
  await expect(page.getByRole('navigation', { name: 'Разделы' }).getByTestId('candidates-count')).toHaveText(String(before + 1));
  await expect(tree.getByRole('link', { name: `кандидаты: ${before + 1}` })).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});
