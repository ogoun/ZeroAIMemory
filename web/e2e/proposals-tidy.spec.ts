// «Предложения» → «Уборка» (M7.5, спецификация §8.8 п. 1; сервер — §8.7): вкладки «Правила» и
// «Уборка» с числами открытых, «Предложения N» в шапке — сумма; карточка уборки — тип кандидата,
// сигналы, записи (L1), операция и довод, «решение человека» у `axiom`, у `merge` — дифф (`keep`
// против `text`); «Применить» — окно (поле текста или подтверждение), «Отклонить» — окно с причиной.
// Ничего не применяется само: «Применить» — действие человека на текущей ревизии записи.
//
// На настоящем сервере — кандидаты «возможно устарело» (ссылка insight на файл изменилась, как
// `stale_ref_badge` working-memory.spec): человек включает вид `tidy` в режиме «агент» и запускает
// его, агент берёт пакет (`POST /recall {event: "work", kind: "tidy"}`) и отвечает операциями;
// «Применить» `revisit`, «Отклонить», 409 у `stale` (запись правят между открытием окна и
// отправкой: объяснение в окне, набранное цело, отправить снова нельзя) и карточка, чья запись
// изменилась до открытия (кнопка недоступна с объяснением). Кандидат `merge` на release-сборке не
// подготовить (векторы — у эмбеддера, противоречия — старше трёх суток): «Применить» `merge` с
// правленым текстом и узкий экран — на подставленных ответах (`routeTidy`, seed-internal.ts).
import { randomUUID } from 'node:crypto';
import { expect, test, type Browser, type Page } from '@playwright/test';
import { pageSnapshots } from './page-snaps';
import {
  TIDY_DROP_TITLE,
  TIDY_KEEP,
  TIDY_KEEP_TITLE,
  TIDY_MERGE,
  TIDY_MERGE_TEXT,
  TIDY_NEW,
  agent,
  human,
  resetWorks,
  routeTidy,
} from './seed-internal';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, noHorizontalScroll, touch } from './viewports';

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};
const base = () => env('E2E_BASE_URL');
const stamp = () => Date.now().toString(36);

test.use({ storageState: ADMIN_STATE });
test.describe.configure({ mode: 'serial' });

pageSnapshots('proposals-tidy', () => `#/proposals/${TIDY_MERGE}`, { prepare: (p) => routeTidy(p).then(() => undefined) });

/** Ошибки JS и CSP страницы и неожиданные HTTP-ответы; `allow` — ожидаемые (409 у `stale`). */
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

const card = (page: Page) => page.getByRole('article', { name: 'Карточка предложения уборки' });
const dialog = (page: Page, name: string) => page.getByRole('dialog', { name, exact: true });

type Entry = { id: string; rev: number; status: string; body: string };
const getEntry = (ref: string) => agent<Entry>('GET', `entries/${ref.replace(/^i:/, '')}`);

/** Кандидаты сценариев настоящего сервера: предложение и запись-цель. */
type Seeded = Record<'apply' | 'decline' | 'conflict' | 'changed', { proposal: string; target: string; title: string }>;
let seeded: Seeded | null = null;

const REASON = 'Ссылка записи на файл изменилась: запись стоит проверить, прежде чем ей верить.';
const APPEND = 'Проверено после правки файла: поведение прежнее.';

/**
 * Кандидаты «возможно устарело» в своём решении (по записи на сценарий) и итог работы `tidy`
 * агентом: человек включает вид в режиме «агент» и запускает его, агент берёт пакеты, пока они
 * есть, и отвечает своим записям операциями, прочим — `keep` (отсеяны, человеку не видны).
 */
async function seedTidy(browser: Browser): Promise<Seeded> {
  if (seeded) return seeded;
  const ctx = await browser.newContext({ storageState: ADMIN_STATE });
  const page = await ctx.newPage();
  try {
    const run = stamp();
    const guid = randomUUID();
    await agent('POST', 'solutions', { agent_solution_id: guid, name: `Уборка E2E ${run}` });
    const hex = (c: string) => c.repeat(64);
    const made: Partial<Seeded> = {};
    for (const key of ['apply', 'decline', 'conflict', 'changed'] as const) {
      const path = `src/tidy_${key}_${run}.rs`;
      const title = `Уборка E2E ${key} ${run}`;
      const e = await agent<{ id: string }>('POST', `spaces/${guid}/entries`, {
        kind: 'insight',
        title,
        body: `Поведение файла ${path}: проверено на сборке ${run}.`,
        data: { kind: 'discovery' },
        refs: { files: [{ path, hash: hex('a') }] },
        force: true,
      });
      // файл изменился: ссылка с хэшем — `stale`, запись — кандидат «возможно устарело»
      await agent('POST', 'files/changed', { solution: guid, files: [{ path, hash: hex('b') }] });
      made[key] = { proposal: '', target: `i:${e.id}`, title };
    }
    const mine = made as Seeded;
    const opOf = (target: string, n: number) => {
      if (target === mine.apply.target) return { n, op: 'revisit', target, reason: REASON };
      if (target === mine.conflict.target) return { n, op: 'append', target, text: APPEND, reason: REASON };
      if (target === mine.decline.target || target === mine.changed.target) return { n, op: 'retract', target, reason: REASON };
      return { n, op: 'keep', reason: 'Кандидат не из сценария e2e уборки — оставить как есть.' };
    };
    await resetWorks(page);
    await human(page, 'PATCH', 'auth/settings', { works: { kinds: { tidy: 'agent' } } });
    const r = await human<{ mode: string; queued: string[] }>(page, 'POST', 'works/run', { kind: 'tidy' });
    expect(r.mode).toBe('agent');
    expect(r.queued.length).toBeGreaterThan(0);
    // агент работает в своём решении: пакеты — работы пространств сессии (решение и Knowledge)
    const session = { 'X-Zam-Session': `e2e-tidy-${run}`, 'X-Zam-Solution': guid };
    /** Ход работы — в сообщение отказа: какие пакеты пришли и что принято. */
    const trace: unknown[] = [{ queued: r.queued }];
    for (let i = 0; i < 10; i++) {
      const pkg = await agent<{ text: string; work: string | null }>('POST', 'recall', { event: 'work', kind: 'tidy' }, session);
      if (!pkg.work) {
        trace.push({ end: pkg.text.slice(0, 300) });
        break;
      }
      const items = [...pkg.text.matchAll(/<record n="(\d+)" ref="([a-z]:[0-9A-Z]+)"/g)].map((m) => opOf(m[2], Number(m[1])));
      const res = await agent<{ accepted: number[]; rejected: unknown[] }>('POST', `works/${pkg.work}/result`, { items }, session);
      trace.push({ work: pkg.work, items, res, head: pkg.text.slice(0, 200) });
      expect(res.rejected, JSON.stringify(res)).toEqual([]);
    }
    await resetWorks(page);
    const list = await agent<{ proposals: { id: string; records: { ref: string }[] }[] }>('GET', 'proposals?type=tidy&status=proposed&limit=200');
    for (const k of Object.keys(mine) as (keyof Seeded)[]) {
      const p = list.proposals.find((x) => x.records?.[0]?.ref === mine[k].target);
      expect(p, `предложение уборки для ${k}: ${JSON.stringify(list)}; работа: ${JSON.stringify(trace)}`).toBeTruthy();
      mine[k].proposal = p!.id;
    }
    seeded = mine;
    return mine;
  } finally {
    await ctx.close();
  }
}

test('tidy_tabs_and_merge_card_apply_with_text', async ({ page }) => {
  const problems = watch(page);
  const mock = await routeTidy(page, { counts: { rules: 0, tidy: 2 }, accept: true });
  await page.goto(`${base()}#/proposals`);
  await expect(page.getByRole('heading', { name: 'Предложения', exact: true })).toBeVisible();
  // в «Правилах» открытых нет, в «Уборке» есть — вкладка по умолчанию «Уборка»; числа открытых —
  // у вкладок, сумма — у раздела в шапке
  const tabs = page.getByRole('tablist', { name: 'Тип предложений' });
  await expect(tabs.getByRole('tab', { name: 'Уборка, 2 открытых' })).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.getByRole('tab', { name: 'Правила, 0 открытых' })).toHaveAttribute('aria-selected', 'false');
  await expect(page.getByRole('navigation', { name: 'Разделы' }).getByTestId('candidates-count')).toHaveText('2');
  const row = page.locator(`li[data-proposal="${TIDY_MERGE}"]`);
  await expect(row).toContainText(`Почти-дубли: «${TIDY_KEEP_TITLE}» и «${TIDY_DROP_TITLE}»`);
  await expect(row).toContainText('решение человека');
  await row.getByRole('button').first().click();
  await expect(page).toHaveURL(new RegExp(`#/proposals/(o:|o%3A)${TIDY_MERGE.slice(2)}$`));
  // карточка на виду: заголовок в фокусе, ряд решения — под шапкой
  const c = card(page);
  await expect(c.getByRole('heading', { level: 2 })).toBeFocused();
  await expect(c.getByTestId('proposal-status')).toHaveText('ждёт решения');
  await expect(c.getByTestId('tidy-signals')).toContainText('Сходство по смыслу (косинус): 0,86');
  await expect(c.getByTestId('tidy-op')).toContainText(`«${TIDY_KEEP_TITLE}» (${TIDY_KEEP}) замещается новой записью с объединённым текстом`);
  await expect(c.getByTestId('tidy-human')).toContainText(`«${TIDY_DROP_TITLE}»`);
  await expect(c.getByTestId('tidy-human')).toContainText('решение человека (axiom)');
  await expect(c.locator(`li[data-record="${TIDY_KEEP}"]`)).toContainText('ревизия 3');
  // дифф: полный текст `keep` против объединённого
  const diff = c.getByTestId('tidy-diff');
  await expect(diff.locator('.ln.del').filter({ hasText: 'Каркас — FormDialog.' })).toHaveCount(1);
  await expect(diff.locator('.ln.add').filter({ hasText: TIDY_MERGE_TEXT.split('\n')[1] })).toHaveCount(1);
  const actions = c.getByRole('group', { name: 'Решение по предложению' });
  const op = c.getByTestId('tidy-op');
  expect((await actions.boundingBox())!.y).toBeLessThan((await op.boundingBox())!.y);

  // «Применить»: окно с объединённым текстом, правка человеком — новый дифф в окне, тело `{text}`
  await actions.getByRole('button', { name: 'Применить', exact: true }).click();
  const d = dialog(page, 'Применить предложение');
  const field = d.getByLabel('Текст объединённой записи');
  await expect(field).toBeFocused();
  await expect(field).toHaveValue(TIDY_MERGE_TEXT);
  const edited = `${TIDY_MERGE_TEXT}\nКаркас — FormDialog, проверка ухода — в стеке.`;
  await field.fill(edited);
  await expect(d.locator('.ln.add').filter({ hasText: 'проверка ухода — в стеке' })).toHaveCount(1);
  await d.getByRole('button', { name: 'Применить', exact: true }).click();
  await expect(d).toBeHidden();
  expect(mock.accepts).toEqual([{ text: edited }]);
  await expect(c.getByTestId('proposal-status')).toHaveText('принят');
  const decided = c.getByTestId('proposal-decided');
  await expect(decided).toContainText('Применено: записи объединены от вашего имени');
  await expect(decided.getByRole('button', { name: TIDY_NEW })).toBeVisible();
  await expect(decided).toBeFocused();
  await expect(c.getByRole('button', { name: 'Применить', exact: true })).toHaveCount(0);
  // вкладка «Правила» — кандидаты в Rules, карточка уборки закрывается
  await tabs.getByRole('tab', { name: /^Правила/ }).click();
  await expect(page).toHaveURL(/#\/proposals$/);
  await expect(page.getByLabel('Статус кандидатов')).toBeVisible();
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tidy_apply_revisit_real', async ({ page, browser }) => {
  test.setTimeout(120_000);
  const s = (await seedTidy(browser)).apply;
  const problems = watch(page);
  await page.goto(`${base()}#/proposals/${s.proposal}`);
  const c = card(page);
  await expect(c.getByRole('heading', { level: 2 })).toHaveText(`Возможно устарело: «${s.title}»`);
  await expect(c.getByTestId('tidy-signals')).toContainText('Изменились связанные файлы: 1');
  await expect(c.getByTestId('tidy-op')).toHaveText(`Отправить «${s.title}» (${s.target}) на пересмотр (статус «на пересмотре»).`);
  await expect(c).toContainText(REASON);
  await expect(page.getByRole('tab', { name: /^Уборка/ })).toHaveAttribute('aria-selected', 'true');
  // подтверждение без полей, не разрушительное: фокус — на «Применить»
  await c.getByRole('button', { name: 'Применить', exact: true }).click();
  const d = dialog(page, 'Применить предложение');
  const go = d.getByRole('button', { name: 'Применить', exact: true });
  await expect(go).toBeFocused();
  await go.click();
  await expect(d).toBeHidden();
  await expect(c.getByTestId('proposal-status')).toHaveText('принят');
  await expect(c.getByTestId('proposal-decided')).toContainText('Применено: запись на пересмотре');
  expect((await getEntry(s.target)).status).toBe('revisit');
  const p = await agent<{ status: string; result_ref: string }>('GET', `proposals/${s.proposal}`);
  expect(p).toMatchObject({ status: 'accepted', result_ref: s.target });
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tidy_decline_real', async ({ page, browser }) => {
  test.setTimeout(120_000);
  const s = (await seedTidy(browser)).decline;
  const problems = watch(page);
  const before = await getEntry(s.target);
  await page.goto(`${base()}#/proposals/${s.proposal}`);
  const c = card(page);
  await expect(c.getByTestId('tidy-op')).toContainText(`Отозвать «${s.title}»`);
  await c.getByRole('button', { name: 'Отклонить', exact: true }).click();
  const d = dialog(page, 'Отклонить предложение');
  await expect(d.getByLabel('Причина')).toBeFocused();
  await d.getByLabel('Причина').fill('запись верна: файл переименован без изменений');
  await d.getByRole('button', { name: 'Отклонить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(c.getByTestId('proposal-status')).toHaveText('отклонён');
  await expect(c.getByTestId('proposal-decided')).toContainText('Предложение отклонено');
  await expect(c).toContainText('запись верна: файл переименован без изменений');
  const p = await agent<{ status: string; decision_note: string | null }>('GET', `proposals/${s.proposal}`);
  expect(p).toMatchObject({ status: 'declined', decision_note: 'запись верна: файл переименован без изменений' });
  // запись не тронута
  const after = await getEntry(s.target);
  expect([after.rev, after.status]).toEqual([before.rev, before.status]);
  // в «ждут решения» его нет, в «отклонённые» — есть
  await expect(page.locator(`li[data-proposal="${s.proposal}"]`)).toHaveCount(0);
  await page.getByLabel('Статус предложений уборки').selectOption('declined');
  await expect(page.locator(`li[data-proposal="${s.proposal}"]`)).toContainText('отклонён');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tidy_stale_409_keeps_typed', async ({ page, browser }) => {
  test.setTimeout(120_000);
  const s = (await seedTidy(browser)).conflict;
  const problems = watch(page, (path, st) => st === 409 && path.endsWith('/accept'));
  await page.goto(`${base()}#/proposals/${s.proposal}`);
  const c = card(page);
  await expect(c.getByTestId('tidy-op-text')).toHaveText(APPEND);
  await c.getByRole('button', { name: 'Применить', exact: true }).click();
  const d = dialog(page, 'Применить предложение');
  const field = d.getByLabel('Дописать');
  await expect(field).toHaveValue(APPEND);
  const typed = `${APPEND} Человек уточнил: проверено вручную.`;
  await field.fill(typed);
  // запись правят из другого места между открытием окна и отправкой
  const e = await getEntry(s.target);
  await agent('PATCH', `entries/${e.id}`, { append_body: 'Правка из другого места.' }, { 'If-Match': `"${e.rev}"` });
  await d.getByRole('button', { name: 'Применить', exact: true }).click();
  // 409: объяснение в окне, набранное цело, отправить снова нельзя
  await expect(d.getByRole('alert')).toContainText('Запись изменилась после предложения — применить нельзя');
  await expect(d).toBeVisible();
  await expect(field).toHaveValue(typed);
  await expect(d.getByRole('button', { name: 'Применить', exact: true })).toBeDisabled();
  // закрыть — с вопросом о набранном; карточка — «устарел», ряда решения нет, объяснение на виду
  await page.keyboard.press('Escape');
  await d.getByRole('button', { name: 'Закрыть без сохранения' }).click();
  await expect(d).toBeHidden();
  await expect(c.getByTestId('proposal-status')).toHaveText('устарел');
  await expect(c.getByTestId('tidy-stale')).toContainText('Кандидат вернётся следующей ночью');
  await expect(c.getByTestId('tidy-stale')).toBeFocused();
  await expect(c.getByRole('button', { name: 'Применить', exact: true })).toHaveCount(0);
  const p = await agent<{ status: string }>('GET', `proposals/${s.proposal}`);
  expect(p.status).toBe('stale');
  // набранное не легло: в записи — только правка из другого места
  const now = await getEntry(s.target);
  expect(now.body).toContain('Правка из другого места.');
  expect(now.body).not.toContain('Человек уточнил');
  expect(problems, problems.join('\n')).toEqual([]);
});

test('tidy_changed_before_open_apply_disabled', async ({ page, browser }) => {
  test.setTimeout(120_000);
  const s = (await seedTidy(browser)).changed;
  const problems = watch(page);
  const e = await getEntry(s.target);
  await agent('PATCH', `entries/${e.id}`, { append_body: 'Уточнение после предложения.' }, { 'If-Match': `"${e.rev}"` });
  await page.goto(`${base()}#/proposals`);
  await page.getByRole('tab', { name: /^Уборка/ }).click();
  const row = page.locator(`li[data-proposal="${s.proposal}"]`);
  await expect(row).toContainText('запись изменилась');
  await row.getByRole('button').first().click();
  const c = card(page);
  await expect(c.getByTestId('proposal-status')).toHaveText('ждёт решения');
  // «Применить» недоступно с объяснением (оно же — описание кнопки); «Отклонить» — можно
  const apply = c.getByRole('button', { name: 'Применить', exact: true });
  await expect(apply).toBeDisabled();
  await expect(c.getByTestId('tidy-stale')).toContainText('Запись изменилась после предложения — применить нельзя');
  await expect(apply).toHaveAttribute('aria-describedby', 'tidy-stale-why');
  await c.getByRole('button', { name: 'Отклонить', exact: true }).click();
  const d = dialog(page, 'Отклонить предложение');
  await d.getByRole('button', { name: 'Отклонить', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(c.getByTestId('proposal-status')).toHaveText('отклонён');
  expect(problems, problems.join('\n')).toEqual([]);
});

test.describe('узкий экран', () => {
  test.use({ viewport: VIEWPORTS.phone, ...touch });

  test('tidy_phone_card_and_apply', async ({ page }) => {
    const problems = watch(page);
    await routeTidy(page);
    await page.goto(`${base()}#/proposals/${TIDY_MERGE}`);
    const c = card(page);
    await expect(c).toBeVisible();
    // карточка вместо списка, «←» к списку; страница вбок не прокручивается, дифф переносится
    await expect(page.locator(`li[data-proposal="${TIDY_MERGE}"]`)).toBeHidden();
    await expect(c.getByTestId('tidy-diff')).toBeVisible();
    expect(await noHorizontalScroll(page)).toBe(true);
    // ряд решения — без прокрутки
    await expect(c.getByRole('button', { name: 'Применить', exact: true })).toBeInViewport();
    await c.getByRole('button', { name: 'Применить', exact: true }).click();
    const d = dialog(page, 'Применить предложение');
    await expect(d).toBeVisible();
    // окно формы — во весь экран, основное действие — в закреплённой шапке
    const vp = page.viewportSize()!;
    const box = (await d.boundingBox())!;
    expect(box.width).toBeGreaterThanOrEqual(vp.width - 1);
    expect(box.height).toBeGreaterThanOrEqual(vp.height - 1);
    await expect(d.locator('header.fd-head button[type="submit"]')).toBeVisible();
    expect(await noHorizontalScroll(page)).toBe(true);
    await page.keyboard.press('Escape');
    await expect(d).toBeHidden();
    expect(problems, problems.join('\n')).toEqual([]);
  });
});
