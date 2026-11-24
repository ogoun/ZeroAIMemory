// «Проверка памяти» (M7.3, спецификация §6.9 п. 2, §6.4–§6.5; решение 66): настоящий ручной прогон
// на пробе, которую агент записал итогом внутренней работы (`POST /works/run` человеком в режиме
// «агент» → `POST /recall {event: "work"}` и `POST /works/{id}/result` по токену в сессии), —
// вердикт, проваленная проба с вопросом, целью-ссылкой, «где нашлась» и советом, число проб по
// видам; «Принять как норму» — на подставленных ответах `GET /check/status` и `GET /check/runs`
// (эпизод регрессии на release-сборке не подготовить: база — 5 ночных прогонов), с точкой у
// «Проверка», окном-подтверждением, телом `{run}` — последним ночным прогоном эпизода, а не
// ручным, — и «накопление базы 1/5» после ответа; серверную часть держит
// `check_accept_new_norm_human_only` задачи 5.
import { expect, test, type Page } from '@playwright/test';
import { pageSnapshots } from './page-snaps';
import { RUN_M1, RUN_N1, agent, checkFixture, human, resetWorks } from './seed-internal';
import { ADMIN_STATE } from './session';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');

test.use({ storageState: ADMIN_STATE });
test.describe.configure({ mode: 'serial' });

pageSnapshots('check', () => '#/check', {
  // «после» — с эпизодом регрессии (подстановка), чтобы на снимке были вердикт, метрики и провалы
  prepare: async (p) => {
    const f = checkFixture();
    await p.route(/\/api\/v1\/check\/status$/, (r) => r.fulfill({ json: f.status }));
    await p.route(/\/api\/v1\/check\/runs(\?.*)?$/, (r) => r.fulfill({ json: f.runs }));
  },
});

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

/** Вопрос, которого поиск не найдёт: слов из записей e2e в нём нет (совет «нет слов спрашивающего»). */
const question = (n: number) =>
  `Как зовут фиолетового кенгуру номер ${n}, который сторожит квантовый маяк заброшенной орбитальной станции`;

/** Проба по итогу внутренней работы агента: человек ставит работу (режим «агент»), агент берёт
 * пакет и отвечает пробными вопросами. Возвращает вопросы и цели принятых проб. */
async function agentProbes(page: Page): Promise<{ question: string; target: string }[]> {
  await resetWorks(page);
  await human(page, 'PATCH', 'auth/settings', { works: { kinds: { probes: 'agent' } } });
  const run = await human<{ mode: string; queued: string[] }>(page, 'POST', 'works/run', { kind: 'probes' });
  expect(run.mode).toBe('agent');
  expect(run.queued.length).toBeGreaterThan(0);
  const session = { 'X-Zam-Session': `e2e-check-${Date.now().toString(36)}` };
  const pkg = await agent<{ text: string; work: string | null }>('POST', 'recall', { event: 'work', kind: 'probes' }, session);
  expect(pkg.work, pkg.text).toBeTruthy();
  const items = [...pkg.text.matchAll(/<record n="(\d+)" ref="([a-z]:[0-9A-Z]+)"/g)].map((m, i) => ({
    n: Number(m[1]),
    target: m[2],
    question: question(i + 1),
  }));
  expect(items.length).toBeGreaterThan(0);
  const r = await agent<{ accepted: number[]; rejected: unknown[] }>(
    'POST',
    `works/${pkg.work}/result`,
    { items: items.map((i) => ({ target: i.target, question: i.question })) },
    session,
  );
  expect(r.accepted.length, JSON.stringify(r)).toBeGreaterThan(0);
  await resetWorks(page);
  return items.filter((i) => r.accepted.includes(i.n));
}

test('check_status_and_failing', async ({ page }) => {
  test.setTimeout(180_000);
  const problems = watch(page);
  const probes = await agentProbes(page);
  await page.goto(`${base()}#/check`);
  await expect(page.getByRole('heading', { name: 'Проверка памяти', exact: true })).toBeVisible();
  await expect(page.getByTestId('check-probes')).toContainText(/Активных проб: \d+ · вопросы исполнителя: \d+/);
  // «Проверить сейчас» — сразу (правило Д): прогон идёт в фоне, страница опрашивает статус
  await page.getByRole('button', { name: 'Проверить сейчас', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: /Прогон (начат|уже идёт)/ })).toBeVisible();
  const verdict = page.getByTestId('check-verdict');
  await expect(verdict).toContainText(/ручной прогон/, { timeout: 120_000 });
  await expect(page.getByTestId('check-running')).toHaveCount(0, { timeout: 120_000 });
  // ручной прогон в базу не входит: «накопление базы N/5»; без эмбеддера — «поиск без векторов»
  await expect(verdict).toContainText(/Вердикт: (накопление базы \d\/5|поиск без векторов|в норме)/);
  // метрики к медиане базы
  await expect(page.getByRole('table', { name: 'Метрики к медиане базы' })).toContainText('Найдено поиском (test)');
  // проваленная проба: вопрос, ожидаемая цель ссылкой, где нашлась, совет
  const failing = page.getByRole('table', { name: 'Проваленные пробы' });
  const p = probes[0];
  const row = failing.locator('tr').filter({ hasText: p.question });
  await expect(row).toBeVisible();
  await expect(row).toContainText('не нашлась и в глубоком поиске');
  await expect(row).toContainText('нет слов спрашивающего');
  // цель — переход к её карточке
  await row.getByRole('button', { name: `Открыть ${p.target}`, exact: true }).click();
  await expect(page).toHaveURL(/#\/u\/[^/]+\/s\/[^/]+\/(j\/[a-z]+|n)\/[0-9A-Z]{26}$/);
  await expect(page.locator('h1, h2').filter({ hasText: /\S/ }).first()).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('check_accept_new_norm', async ({ page }) => {
  const problems = watch(page);
  // эпизод регрессии — подстановкой (решение 66): последний законченный — ручной M1, принять
  // можно только последний ночной N1
  let accepted = false;
  const bodies: unknown[] = [];
  await page.route(/\/api\/v1\/check\/status$/, (r) => r.fulfill({ json: checkFixture(accepted).status }));
  await page.route(/\/api\/v1\/check\/runs(\?.*)?$/, (r) => r.fulfill({ json: checkFixture(accepted).runs }));
  await page.route(/\/api\/v1\/check\/accept$/, async (r) => {
    bodies.push(r.request().postDataJSON());
    accepted = true;
    const n1 = checkFixture(true).runs.items.find((x) => x.run === RUN_N1);
    return r.fulfill({ json: n1 });
  });
  await page.goto(`${base()}#/check`);
  // точка у «Проверка» — с первого экрана (оболочка читает прогоны сама)
  const nav = page.getByRole('navigation', { name: 'Разделы' });
  await expect(nav.getByRole('link', { name: 'Проверка', exact: true }).getByTestId('check-dot')).toBeVisible();
  await expect(page.getByTestId('check-verdict')).toContainText('Вердикт: ухудшение: поиск −12 п. п. (62 % → 50 %)');
  await expect(page.getByTestId('check-verdict')).toContainText('ручной прогон');
  // «Принять как норму» — окно-подтверждение (правило Б), фокус — на основной кнопке
  await page.getByRole('button', { name: 'Принять как норму', exact: true }).click();
  const d = page.getByRole('dialog', { name: 'Принять как норму?', exact: true });
  await expect(d).toBeVisible();
  await expect(d).toContainText('2026-11-23');
  await expect(d.getByRole('button', { name: 'Принять как норму', exact: true })).toBeFocused();
  await d.getByRole('button', { name: 'Принять как норму', exact: true }).click();
  await expect(d).toBeHidden();
  // тело — последний законченный ночной прогон эпизода, а не последний (ручной)
  expect(bodies).toEqual([{ run: RUN_N1 }]);
  expect(RUN_M1).not.toBe(RUN_N1);
  // после ответа: база начинается заново, кнопки и точки нет
  await expect(page.getByTestId('check-verdict')).toContainText('Вердикт: накопление базы 1/5');
  await expect(page.getByRole('status').filter({ hasText: 'Норма принята' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Принять как норму', exact: true })).toHaveCount(0);
  await expect(page.getByTestId('check-dot')).toHaveCount(0);
  // проваленные пробы подстановки — с советом
  await expect(page.getByRole('table', { name: 'Проваленные пробы' })).toContainText('нет слов спрашивающего');
  expect(problems, problems.join('\n')).toEqual([]);
});
