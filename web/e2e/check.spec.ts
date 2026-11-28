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
import { sectionItem } from './nav';
import { RUN_M1, RUN_N1, agent, checkFixture, human, resetWorks, runsReply } from './seed-internal';
// M7.4 (спецификация §7.7 п. 1): `check_trend_svg` — тренд своим SVG на подставленном `trend`
// (пропуск и ухудшение на release-сборке не подготовить); `check_probe_add_retire` — ручная проба
// и её снятие на настоящем сервере: окна формы, ошибки под полем, 409 повтора, фильтр «снятые»,
// аудит `check.probe.retire`.
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
    await p.route(/\/api\/v1\/check\/runs(\?.*)?$/, (r) => r.fulfill({ json: runsReply(r.request().url(), f.runs) }));
  },
});

/** Ошибки JS и CSP страницы и неожиданные HTTP-ответы; `allow` — ожидаемые (повтор пробы — 409). */
function watch(page: Page, allow: (path: string, status: number) => boolean = () => false): string[] {
  const problems: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(e.message));
  page.on('response', (r) => {
    const path = new URL(r.url()).pathname;
    const expected =
      (r.status() === 401 && path.endsWith('/api/v1/auth/me')) || (r.status() === 404 && path.endsWith('/favicon.ico')) || allow(path, r.status());
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
  // какие из проб агента провалятся, зависит от записей прогона e2e (поиск без векторов находит
  // часть по общим словам), поэтому берётся первая проваленная из своих, а не первая своя
  const failing = page.getByRole('table', { name: 'Проваленные пробы' });
  await expect(failing.locator('tbody tr').first()).toBeVisible();
  const texts = await failing.locator('tbody tr').allTextContents();
  const p = probes.find((x) => texts.some((t) => t.includes(x.question)));
  expect(p, 'хотя бы одна проба агента — среди проваленных').toBeTruthy();
  const row = failing.locator('tr').filter({ hasText: p!.question });
  await expect(row).toBeVisible();
  await expect(row).toContainText('не нашлась и в глубоком поиске');
  await expect(row).toContainText('нет слов спрашивающего');
  // цель — переход к её карточке
  await row.getByRole('button', { name: `Открыть ${p!.target}`, exact: true }).click();
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
  await page.route(/\/api\/v1\/check\/runs(\?.*)?$/, (r) => r.fulfill({ json: runsReply(r.request().url(), checkFixture(accepted).runs) }));
  await page.route(/\/api\/v1\/check\/accept$/, async (r) => {
    bodies.push(r.request().postDataJSON());
    accepted = true;
    const n1 = checkFixture(true).runs.items.find((x) => x.run === RUN_N1);
    return r.fulfill({ json: n1 });
  });
  await page.goto(`${base()}#/check`);
  // точка у «Проверка» — с первого экрана (оболочка читает прогоны сама); раздел может быть в «Ещё»
  await expect((await sectionItem(page, 'Проверка')).getByTestId('check-dot')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('check-verdict')).toContainText('Вердикт: ухудшение: поиск −12 п. п. (62 % → 50 %)');
  await expect(page.getByTestId('check-verdict')).toContainText('ручной прогон');
  // последний прогон — ручной: у кнопки — строка о ночном прогоне (ревью r1, Minor 6); вердикт тот
  // же — без его повтора (ревью r2, N3)
  await expect(page.getByTestId('check-open-regression')).toHaveText('«Принять как норму» относится к ночному прогону 2026-11-23.');
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

// ---- M7.4 (спецификация §7.7 п. 1): тренд и набор проб ----

/** Строка тренда той же формы, что у сервера (`GET /check/status`, `trend`). */
const trendRow = (n: number, x: Record<string, unknown>) => ({
  run: `01KQ000000000000000000TR${String(n).padStart(2, '0')}`,
  date: `2026-11-${String(10 + n).padStart(2, '0')}`,
  trigger: 'nightly',
  status: 'done',
  verdict: 'ok',
  hit_search: 0.6,
  hit_prompt: 0.3,
  negatives_shown: 0.1,
  p95: 120,
  ...x,
});

test('check_trend_svg', async ({ page }) => {
  const problems = watch(page);
  // пять прогонов (новые первыми, как у сервера): пропуск посередине, ухудшение у предпоследнего
  const trend = [
    trendRow(5, { trigger: 'manual', hit_search: 0.62, hit_prompt: 0.34, p95: 110 }),
    trendRow(4, { verdict: 'regression', hit_search: 0.48, hit_prompt: 0.2, p95: 260 }),
    trendRow(3, { hit_search: 0.6, hit_prompt: 0.31, p95: 130 }),
    trendRow(2, { status: 'skipped', verdict: null, hit_search: null, hit_prompt: null, negatives_shown: null, p95: null }),
    trendRow(1, { hit_search: 0.58, hit_prompt: 0.3, p95: 120 }),
  ];
  const f = checkFixture();
  await page.route(/\/api\/v1\/check\/status$/, (r) => r.fulfill({ json: { ...f.status, trend } }));
  await page.route(/\/api\/v1\/check\/runs(\?.*)?$/, (r) => r.fulfill({ json: runsReply(r.request().url(), f.runs) }));
  await page.goto(`${base()}#/check`);
  const sec = page.getByTestId('check-trend');
  await expect(sec.getByRole('heading', { name: 'Тренд', exact: true })).toBeVisible();
  const share = sec.getByRole('img', { name: 'Найдено поиском и среди подсказок к запросу (test), %' });
  const ms = sec.getByRole('img', { name: 'Задержка поиска p95, мс' });
  await expect(share).toBeVisible();
  await expect(ms).toBeVisible();
  // пропуск — разрыв: у каждого ряда два отрезка (1 и 3 точки), точек — 4 из 5 прогонов
  await expect(share.locator('path.line.l-hit_search')).toHaveCount(2);
  await expect(share.locator('path.line.l-hit_prompt')).toHaveCount(2);
  await expect(ms.locator('path.line.l-p95')).toHaveCount(2);
  // ухудшение — знаком ▼ (у всех рядов прогона), прочие точки — кружками
  await expect(share.locator('path.worse')).toHaveCount(2);
  await expect(share.locator('circle.dot')).toHaveCount(6);
  await expect(ms.locator('path.worse')).toHaveCount(1);
  await expect(ms.locator('circle.dot')).toHaveCount(3);
  // оси: доли — 0–100 %, задержка — от нуля до «круглого» верха (260 → 500 мс); даты краёв
  await expect(share.locator('text.tick')).toHaveText(['0 %', '50 %', '100 %', '2026-11-11', '2026-11-15']);
  await expect(ms.locator('text.tick').filter({ hasText: '500 мс' })).toHaveCount(1);
  // подпись для чтения с экрана — описание графиков, те же числа — в скрытой таблице
  const described = await share.evaluate((s) => document.getElementById(s.getAttribute('aria-describedby') ?? '')?.textContent ?? '');
  expect(described).toBe(
    'Тренд за 5 прогонов (с 2026-11-11 по 2026-11-15): найдено поиском (test) — последний 62 %, от 48 % до 62 %; ' +
      'среди подсказок к запросу (test) — последний 34 %, от 20 % до 34 %; задержка поиска p95 — последний 110 мс, ' +
      'от 110 мс до 260 мс; прогонов с ухудшением — 1, пропусков — 1.',
  );
  const table = sec.getByRole('table', { name: 'Тренд по прогонам' });
  await expect(table.locator('tbody tr')).toHaveCount(5);
  await expect(table.locator('tbody tr').nth(1)).toContainText('пропущен');
  await expect(table.locator('tbody tr').nth(3)).toContainText('ухудшение');
  // графики — по ширине области, страница вбок не прокручивается
  for (const g of [share, ms]) {
    const fig = (await g.locator('xpath=..').boundingBox())!;
    const box = (await g.boundingBox())!;
    expect(box.width).toBeLessThanOrEqual(fig.width + 0.5);
  }
  expect(await wideOf(page), 'горизонтальная прокрутка страницы').toEqual([]);
  // телефон: графики друг под другом во всю ширину; скрытая таблица страницу не раздвигает (таблица
  // не бывает уже своего содержимого — раньше она расширяла раскладку на телефоне по кругу)
  await page.setViewportSize({ width: 390, height: 844 });
  await expect.poll(async () => Math.round((await share.boundingBox())!.width)).toBeLessThan(390);
  const [a, b] = [(await share.boundingBox())!, (await ms.boundingBox())!];
  expect(b.y).toBeGreaterThan(a.y + a.height - 1);
  expect(await wideOf(page), 'горизонтальная прокрутка страницы на телефоне').toEqual([]);
  expect(problems, problems.join('\n')).toEqual([]);
});

/** Страница шире окна — самые глубокие элементы за правым краем (вне прокручиваемого предка). */
function wideOf(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const vw = document.documentElement.clientWidth;
    if (document.documentElement.scrollWidth <= vw + 0.5) return [];
    const clips = (e: Element) => ['auto', 'scroll', 'hidden', 'clip'].includes(getComputedStyle(e).overflowX);
    const wide = [...document.body.querySelectorAll('*')]
      .filter((e) => {
        if (e.getBoundingClientRect().right <= vw + 0.5) return false;
        for (let p = e.parentElement; p && p !== document.body; p = p.parentElement) if (clips(p)) return false;
        return ![...e.children].some((ch) => ch.getBoundingClientRect().right > vw + 0.5);
      })
      .slice(0, 5)
      .map((e) => `${e.tagName.toLowerCase()}.${(e.getAttribute('class') ?? '').trim().split(/\s+/).join('.')} → ${Math.round(e.getBoundingClientRect().right)}`);
    // раздвигает то, что само себя обрезает (скрытая таблица), — хотя бы ширина страницы
    return wide.length ? wide : [`страница ${document.documentElement.scrollWidth} > ${vw}`];
  });
}

test('check_probe_add_retire', async ({ page }) => {
  // ожидаемый отказ сервера — повтор вопроса (409 `probe-duplicate`)
  const problems = watch(page, (path, status) => status === 409 && path.endsWith('/api/v1/check/probes'));
  const stamp = Date.now().toString(36);
  const target = await agent<{ id: string }>('POST', 'spaces/knowledge/entries', {
    kind: 'insight',
    title: `Цель ручной пробы ${stamp}`,
    body: 'Сборщик мусора блобов удаляет файл через каталог .gc/ после фиксации строки.',
    data: { kind: 'discovery' },
  });
  const ref = `i:${target.id}`;
  const q = `Как сборщик мусора удаляет блоб без ссылок ${stamp}`;
  await page.goto(`${base()}#/check`);
  await expect(page.getByRole('heading', { name: 'Пробы', exact: true })).toBeVisible();

  // «Добавить пробу» — окно формы: фокус на вопросе; цель не той формы — ошибка под полем, окно открыто
  await page.getByRole('button', { name: 'Добавить пробу', exact: true }).click();
  const add = page.getByRole('dialog', { name: 'Добавить пробу', exact: true });
  await expect(add.getByLabel('Вопрос')).toBeFocused();
  await add.getByLabel('Вопрос').fill(q);
  await add.getByLabel('Цели').fill(`${target.id} ${ref}`);
  await add.getByRole('button', { name: 'Добавить', exact: true }).click();
  await expect(add.getByRole('alert')).toContainText(`«${target.id}» — не ссылка вида <буква>:<id>`);
  await expect(add.getByLabel('Цели')).toBeFocused();
  await expect(add.getByLabel('Цели')).toHaveAttribute('aria-invalid', 'true');
  await expect(add.getByLabel('Вопрос')).toHaveValue(q);
  // ссылка верна — проба добавлена: окно закрыто, итог в фокусе, строка в наборе
  const created = page.waitForResponse((r) => r.request().method() === 'POST' && r.url().endsWith('/api/v1/check/probes'));
  await add.getByLabel('Цели').fill(ref);
  await add.getByRole('button', { name: 'Добавить', exact: true }).click();
  const id = ((await (await created).json()) as { id: string }).id;
  expect(id).toMatch(/^x:[0-9A-Z]{26}$/);
  await expect(add).toBeHidden();
  const notice = page.getByRole('status').filter({ hasText: `Проба ${id} добавлена` });
  await expect(notice).toBeVisible();
  await expect(notice).toBeFocused();
  const row = page.locator(`[data-probe-set="${id}"]`);
  await expect(row).toContainText(q);
  await expect(row).toContainText('ручные');
  await expect(row).toContainText('человек');
  await expect(row.getByRole('button', { name: `Открыть ${ref}`, exact: true })).toBeVisible();

  // повтор вопроса — 409: ошибка в окне, набранное цело; закрытие с набранным — вопрос в окне
  await page.getByRole('button', { name: 'Добавить пробу', exact: true }).click();
  await add.getByLabel('Вопрос').fill(q);
  await add.getByLabel('Цели').fill(ref);
  await add.getByRole('button', { name: 'Добавить', exact: true }).click();
  await expect(add.getByRole('alert')).toContainText('Такой вопрос уже есть у активной пробы');
  await expect(add.getByLabel('Вопрос')).toHaveValue(q);
  await page.keyboard.press('Escape');
  await add.getByTestId('fd-ask').getByRole('button', { name: 'Закрыть без сохранения' }).click();
  await expect(add).toBeHidden();

  // «Снять» — опасное подтверждение с причиной: короткая — ошибка под полем; верная — проба снята
  await row.getByRole('button', { name: /^Снять пробу «/ }).click();
  const retire = page.getByRole('dialog', { name: 'Снять пробу?', exact: true });
  await expect(retire).toContainText(q);
  await expect(retire.getByLabel('Причина снятия')).toBeFocused();
  await retire.getByLabel('Причина снятия').fill('аб');
  await retire.getByRole('button', { name: 'Снять', exact: true }).click();
  await expect(retire.getByRole('alert')).toHaveText('Причина — от 3 символов: что в пробе неверно.');
  const retired = page.waitForResponse((r) => r.request().method() === 'DELETE' && r.url().includes('/api/v1/check/probes/'));
  await retire.getByLabel('Причина снятия').fill('цель замещена новой записью');
  await retire.getByRole('button', { name: 'Снять', exact: true }).click();
  expect((await retired).status()).toBe(204);
  await expect(retire).toBeHidden();
  await expect(page.getByRole('status').filter({ hasText: `Проба ${id} снята` })).toBeFocused();
  await expect(row).toHaveCount(0);
  // фильтр сервера «снятые» — проба с причиной, без «Снять»
  const listed = page.waitForRequest((r) => r.url().includes('/api/v1/check/probes?') && r.url().includes('status=retired'));
  await page.getByLabel('Состояние').selectOption('retired');
  await listed;
  await expect(row).toContainText('цель замещена новой записью');
  await expect(row.getByRole('button', { name: /^Снять пробу/ })).toHaveCount(0);
  // снятие — в аудите, с причиной
  const audit = (await (await page.request.get(`${base()}api/v1/admin/audit?action=check.probe.retire&limit=5`)).json()) as {
    items: { target: string; details: { reason: string } }[];
  };
  expect(audit.items.find((a) => a.target === id)?.details.reason).toBe('цель замещена новой записью');
  expect(problems, problems.join('\n')).toEqual([]);
});
