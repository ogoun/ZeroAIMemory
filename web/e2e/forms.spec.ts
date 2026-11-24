// Карточки и формы ввода WebUI (M7.3, спецификация §6.14; задача владельца k:01M85AS1C0EZVV95FQZ2ZEQCET:
// «при создании/выборе задачи ее карточка открывается в самом низу… Нужно видеть тут же»). Каждая
// форма перечня `form-cases.ts` — в трёх ширинах (`phone` с касанием, `tablet`, `desktop`), открытие
// с фокусом — ещё и в `tabletLand` с касанием: окно формы, ловушка фокуса, Esc, вопрос о
// несохранённом внутри окна, ошибка сервера в окне, карточка созданного на виду, «Назад» на телефоне.
// По местам — карточка на виду при открытии (`card_in_view_on_open`), ряд статусов задачи под шапкой,
// стек проверок ухода, набранное при 409, «Новый insight» с дублем, смена статуса задачи без окна.
//
// Режим съёмки `E2E_FORMS_SNAP=before|after` — только снимки каждой формы перечня (и мест задачи
// 10b) в четырёх ширинах в `.tmp/e2e/screens/forms/<режим>/<форма>-<ширина>.png` и строка «форма
// на виду или ниже видимой части» в `list.tsv` рядом, без проверок нового поведения: «до» — на
// сборке 0.0.15 (шаг 1 задачи 10a), «после» — в конце задачи. Данные — REST по токену
// администратора (агент) в своём решении `seedForms`.
import { expect, test, type Locator, type Page } from '@playwright/test';
import { appendFileSync, mkdirSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { FORM_CASES, SNAP_ONLY, dialogOf, type FormCase } from './form-cases';
import { env, pageCtx, type PageCtx } from './mobile-pages';
import { reviewSummary } from './seed';
import { ADMIN_STATE } from './session';
import { VIEWPORTS, touch, type ViewportName } from './viewports';

const base = () => env('E2E_BASE_URL');
const SNAP = process.env.E2E_FORMS_SNAP ?? '';
const screens = fileURLToPath(new URL(`../../.tmp/e2e/screens/forms/${SNAP || 'run'}/`, import.meta.url));
/** Вопрос о несохранённом (`LEAVE_EDIT`, `web/src/lib/guards.ts`). */
const LEAVE_EDIT = 'В форме есть несохранённый текст. Закрыть её без сохранения?';

test.use({ storageState: ADMIN_STATE });

// ---- данные ----

/** REST по токену администратора (агент); `extra` — заголовки. */
async function api<T = Record<string, unknown>>(method: string, path: string, body?: unknown, extra: Record<string, string> = {}): Promise<T> {
  const r = await fetch(`${base()}api/v1/${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${env('E2E_ADMIN_TOKEN')}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...extra,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (r.status === 204 ? {} : await r.json()) as T;
}

type Created = { id: string; rev: number };
const entryOf = (guid: string, body: Record<string, unknown>) => api<Created>('POST', `spaces/${guid}/entries`, body);
const patch = (e: Created, body: Record<string, unknown>) => api<Created>('PATCH', `entries/${e.id}`, body, { 'If-Match': `"${e.rev}"` });
const getEntry = (id: string) => api<{ id: string; rev: number; title: string; body: string; status: string; deleted_at: string | null; certainty: string | null }>('GET', `entries/${id}`);

/** Новое решение администратора; возвращает его GUID. */
async function newSolution(name: string): Promise<string> {
  const guid = randomUUID();
  await api('POST', 'solutions', { agent_solution_id: guid, name });
  return guid;
}

const longBody = (what: string) => Array.from({ length: 60 }, (_, i) => `${what}: строка ${i + 1} длинного текста карточки.`).join('\n');

/** Insight-решение с полями ADR (все окна карточки: замена, ADR, отзыв, надёжность, триггеры). */
const decision = (guid: string, title: string) =>
  entryOf(guid, {
    kind: 'insight',
    title,
    body: 'Окна форм вместо форм внизу карточки.',
    data: { kind: 'decision', decision: { context: 'Формы внизу карточки', options: ['оставить'], chosen: 'окна', rationale: 'видно сразу', decided_by: 'human' } },
    force: true,
  });

/** Данные строк `FORM_CASES` и снимков: своё решение, по записи на окно (`E2E_FORMS_*`). */
async function seedForms(): Promise<void> {
  if (process.env.E2E_FORMS_SOLUTION) return;
  const guid = await newSolution(`Формы E2E ${Date.now().toString(36)}`);
  const open = await entryOf(guid, { kind: 'task', title: 'Задача форм: открытая', body: 'Задача для окон смены статуса.', data: { next_step: 'проверить окна' } });
  const done = await entryOf(guid, { kind: 'task', title: 'Задача форм: выполненная', body: '' });
  await patch(done, { status: 'done' });
  const insight = await decision(guid, 'Решение форм E2E: окна');
  const sup = await decision(guid, 'Решение форм E2E: замена');
  const ideaNew = await entryOf(guid, { kind: 'idea', title: 'Идея форм: новая', body: 'Замысел для окон решений.', data: { origin: 'human' } });
  const declined = await entryOf(guid, { kind: 'idea', title: 'Идея форм: отклонённая', body: 'Замысел.', data: { origin: 'human' } });
  await api('POST', `entries/${declined.id}/review`, { outcome: 'decline', summary: reviewSummary('то, что уже отклоняли') });
  // места задачи 10b — для снимков «до»
  const term = await entryOf(guid, { kind: 'term', title: 'Окно формы', data: { definition: 'Общий каркас форм WebUI.' } });
  const question = await entryOf(guid, { kind: 'question', title: 'Вопрос форм E2E: окна или карточка?', body: 'Где править?', data: { options: [{ key: 'a', text: 'окна' }, { key: 'b', text: 'карточка' }] } });
  const changelog = await entryOf(guid, { kind: 'changelog', title: 'Запись форм E2E', body: 'Окна форм.', data: { kind: 'feat' } });
  const release = await entryOf(guid, { kind: 'release', title: 'Релиз форм E2E', body: 'Окна.', data: { version: '0.0.16' } });
  Object.assign(process.env, {
    E2E_FORMS_SOLUTION: guid,
    E2E_FORMS_TASK_OPEN: open.id,
    E2E_FORMS_TASK_DONE: done.id,
    E2E_FORMS_INSIGHT: insight.id,
    E2E_FORMS_INSIGHT_SUP: sup.id,
    E2E_FORMS_IDEA_NEW: ideaNew.id,
    E2E_FORMS_IDEA_DECLINED: declined.id,
    E2E_FORMS_TERM: term.id,
    E2E_FORMS_QUESTION: question.id,
    E2E_FORMS_CHANGELOG: changelog.id,
    E2E_FORMS_RELEASE: release.id,
  });
}

test.beforeAll(async () => {
  await seedForms();
});

// ---- помощники ----

/** Ошибки JS и CSP страницы; `allow` — ожидаемые ответы HTTP (подменённый 422, 409). */
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

/** Системные окна браузера (`window.confirm`): считаются, по умолчанию — «Отмена». */
function nativeDialogs(page: Page, accept = false): string[] {
  const seen: string[] = [];
  page.on('dialog', (d) => {
    seen.push(d.message());
    void (accept ? d.accept() : d.dismiss());
  });
  return seen;
}

let ctxCache: PageCtx | null = null;
async function ctxOf(page: Page): Promise<PageCtx> {
  ctxCache ??= await pageCtx(page);
  return ctxCache;
}

/** Адрес приложения, оболочка готова. */
async function goto(page: Page, hash: string): Promise<void> {
  await page.goto(`${base()}${hash}`);
  await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
}

/** Страница строки открыта, кнопка, открывшая окно, запоминается (`__opener`). */
async function openRow(page: Page, row: FormCase): Promise<Locator> {
  await goto(page, row.page(await ctxOf(page)));
  await page.evaluate(() => {
    const w = window as unknown as { __opener: Element | null };
    w.__opener = null;
    document.addEventListener(
      'click',
      (e) => {
        const b = e.target instanceof Element ? e.target.closest('button') : null;
        // кнопка страницы или панели карточки (она тоже `role="dialog"`), не самого окна формы
        if (b && !b.closest('[data-form-dialog]')) w.__opener = b;
      },
      true,
    );
  });
  await row.open(page);
  const d = dialogOf(page, row.dialog ?? row.name);
  await expect(d).toBeVisible();
  return d;
}

const focusInDialog = (page: Page) => page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]'));
const openerFocused = (page: Page) =>
  page.evaluate(() => {
    const o = (window as unknown as { __opener: Element | null }).__opener;
    return !!o && document.activeElement === o;
  });
/** Основная кнопка окна (на телефоне — в шапке). */
const submitOf = (d: Locator) => d.locator('button[type="submit"]:visible');
/** Значения полей окна. */
const values = (d: Locator) => d.locator('input:not([type="radio"]), textarea, select').evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value));

/** Высота шапки приложения (CSS px). */
const headerBottom = (page: Page) => page.evaluate(() => document.querySelector('header.topbar')?.getBoundingClientRect().bottom ?? 0);

/** Заголовок в фокусе и в видимой части окна (между шапкой и нижним краем). */
async function headingInView(page: Page, scope?: Locator): Promise<void> {
  const h = (scope ?? page).locator('h1:focus, h2:focus, h3:focus');
  await expect(h).toHaveCount(1);
  const box = await h.boundingBox();
  const top = await headerBottom(page);
  const vh = page.viewportSize()!.height;
  expect(box, 'заголовок отрисован').not.toBeNull();
  expect(box!.y, `заголовок ниже шапки (${top})`).toBeGreaterThanOrEqual(top - 1);
  expect(box!.y, 'заголовок выше нижнего края окна').toBeLessThan(vh - 20);
}

/** Браузер без Navigation API (Safari < 26.2, Firefox < 147): роутер считает записи по
 * `history.state.zamIdx` — запасной путь (образец — `tasks.spec.ts`, `hideNavApi`). */
const hideNavApi = (page: Page) =>
  page.addInitScript(() => Object.defineProperty(window, 'navigation', { value: undefined, configurable: true }));
/** Место в истории: индекс Navigation API, без него — номер записи роутера. */
const placeOf = (page: Page) =>
  page.evaluate(() => {
    const nav = (globalThis as unknown as { navigation?: { currentEntry?: { index: number } } }).navigation;
    return nav?.currentEntry?.index ?? (history.state as { zamIdx?: number } | null)?.zamIdx ?? null;
  });

/**
 * Поля окна идут плотно сверху вниз (§6.14 п. 1; ревью r1 задачи 10a, I1): от низа поля (или набора
 * полей) до верха следующей подписи — не больше обычного отступа (`gap` тела 10 px плюс поле на
 * пометку). Растянутые на высоту тела строки сетки дали бы здесь сотни пикселей.
 */
async function fieldsDense(scope: Locator, max = 24): Promise<void> {
  const gaps = await scope.locator('.fd-body').evaluate((b) => {
    const els = [...b.querySelectorAll<HTMLElement>('label:not(.inline), fieldset')].filter((e) => e.getClientRects().length > 0);
    const out: { gap: number; at: string }[] = [];
    for (let i = 1; i < els.length; i++) {
      const prev = els[i - 1];
      const next = els[i];
      if (prev.contains(next)) continue;
      const ctl = prev.matches('fieldset') ? prev : (prev.querySelector('input, textarea, select') ?? prev);
      const gap = next.getBoundingClientRect().top - ctl.getBoundingClientRect().bottom;
      if (gap > 0) out.push({ gap: Math.round(gap), at: `${(prev.textContent ?? '').trim().slice(0, 20)} → ${(next.textContent ?? '').trim().slice(0, 20)}` });
    }
    // и сверху вниз от шапки: шапка → пояснение → первое поле → … (у окна с одним полем — шапка или
    // пояснение → поле); низ пояснения и рядов — по их содержимому (растянутая строка сетки выше
    // своего текста), низ подписи — по её полю (ревью r2, N3)
    const contentBottom = (e: Element): number => {
      if (e.matches('label:not(.inline)')) return (e.querySelector('input, textarea, select') ?? e).getBoundingClientRect().bottom;
      if (e.matches('header, fieldset')) return e.getBoundingClientRect().bottom;
      const r = document.createRange();
      r.selectNodeContents(e);
      return r.getBoundingClientRect().bottom;
    };
    const head = b.closest('[data-form-dialog]')?.querySelector('header.fd-head');
    const seq = [head, ...[...b.children].filter((e) => e.getClientRects().length > 0)].filter((e): e is Element => !!e);
    for (let i = 1; i < seq.length; i++) {
      const gap = seq[i].getBoundingClientRect().top - contentBottom(seq[i - 1]);
      if (gap > 0) out.push({ gap: Math.round(gap), at: `${seq[i - 1].tagName.toLowerCase()} «${(seq[i - 1].textContent ?? '').trim().slice(0, 20)}» → ${seq[i].tagName.toLowerCase()}` });
    }
    return out;
  });
  for (const g of gaps) expect(g.gap, `зазор между полями окна: ${g.at}`).toBeLessThanOrEqual(max);
}

interface Width {
  name: ViewportName;
  touch: boolean;
}
const W3: Width[] = [
  { name: 'phone', touch: true },
  { name: 'tablet', touch: false },
  { name: 'desktop', touch: false },
];
const W4: Width[] = [...W3.slice(0, 2), { name: 'tabletLand', touch: true }, W3[2]];
/** Строки, у которых «Назад» на телефоне проверяется и без Navigation API (ревью r1, M1 (б)). */
const NO_NAV_ROWS = new Set(['task-create', 'task-reopen', 'insight-new', 'idea-create', 'idea-defer']);

/** Второго системного вопроса нет: роутер спрашивает синхронно в `hashchange`, а откат и его
 * `hashchange` приходят до двух кадров отрисовки — после них вопросу уже неоткуда взяться. */
const settled = (page: Page) =>
  page.evaluate(() => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(r, 0)))));

// ---- режим съёмки (шаг 1 и шаг 5 задачи 10a) ----

if (SNAP) {
  mkdirSync(screens, { recursive: true });
  const rows: { id: string; name: string; page: FormCase['page']; open: FormCase['open']; legacy?: string; dialog?: string; added?: boolean }[] = [
    ...FORM_CASES,
    ...SNAP_ONLY,
  ];
  for (const w of W4) {
    test.describe(`снимки ${SNAP} · ${w.name}`, () => {
      test.use({ viewport: VIEWPORTS[w.name], ...(w.touch ? touch : {}) });
      for (const row of rows) {
        test(`forms_snapshot ${row.id} ${w.name}`, async ({ page }) => {
          nativeDialogs(page);
          // съёмка без проверок: место, которое не открылось, — строкой в списке, а не ожиданием
          page.setDefaultTimeout(8_000);
          await goto(page, row.page(await ctxOf(page)));
          await page.waitForTimeout(400);
          let state = '';
          try {
            await row.open(page);
            await page.waitForTimeout(500);
          } catch (e) {
            state = `не открылась: ${(e as Error).message.split('\n')[0]}`;
          }
          const vh = VIEWPORTS[w.name].height;
          if (!state) {
            const dlg = page.getByRole('dialog').last();
            const legacy = row.legacy ? page.locator(`[aria-label="${row.legacy}"]`).last() : null;
            const target = (await dlg.count()) && (await dlg.isVisible()) ? dlg : legacy && (await legacy.count()) ? legacy : null;
            const box = target ? await target.boundingBox() : null;
            if (!box) state = row.added ? 'формы нет в этой сборке' : 'форма не найдена';
            else {
              const kind = target === dlg ? 'окно' : 'форма в карточке';
              state =
                box.y >= vh
                  ? `${kind} ниже видимой части (верх ${Math.round(box.y)} при высоте ${vh})`
                  : box.y + box.height > vh
                    ? `${kind} на виду частично (верх ${Math.round(box.y)}, низ ${Math.round(box.y + box.height)} при высоте ${vh})`
                    : `${kind} на виду целиком`;
            }
          }
          await page.screenshot({ path: `${screens}${row.id}-${w.name}.png` });
          appendFileSync(`${screens}list.tsv`, `${row.id}\t${w.name}\t${row.name}\t${state}\n`);
        });
      }
    });
  }
}

// ---- окна форм перечня (FORM_CASES) ----

if (!SNAP) {
  for (const w of W4) {
    test.describe(`формы · ${w.name}`, () => {
      test.use({ viewport: VIEWPORTS[w.name], ...(w.touch ? touch : {}) });
      const three = w.name !== 'tabletLand';
      const phone = w.name === 'phone';

      for (const row of FORM_CASES) {
        if (row.inline) {
          const inl = row.inline;
          // форма на месте (правило А/В): видна при открытии, фокус — на первом поле (у поля на виду
          // фокус не переводится), ошибка сервера — у формы, набранное цело, уход по адресу — один вопрос
          test(`forms_inline_open_in_view ${row.id} ${w.name}`, async ({ page }) => {
            const problems = watch(page);
            await goto(page, row.page(await ctxOf(page)));
            await row.open(page);
            const sc = inl.scope(page);
            const field = sc.getByLabel(row.first!).first();
            await expect(field).toBeVisible();
            if (inl.focus) await expect(field).toBeFocused();
            const b = (await field.boundingBox())!;
            expect(b.y, 'поле ниже шапки').toBeGreaterThanOrEqual(await headerBottom(page));
            expect(b.y + b.height, 'поле выше нижнего края').toBeLessThanOrEqual(page.viewportSize()!.height);
            expect(problems, problems.join('\n')).toEqual([]);
          });
          if (!three) continue;
          test(`forms_server_error_keeps_input ${row.id} ${w.name}`, async ({ page }) => {
            const problems = watch(page, (_p, st) => st === 422);
            const r = row.route!;
            await page.route(r.url, (route) =>
              route.request().method() === r.method
                ? route.fulfill({
                    status: 422,
                    contentType: 'application/problem+json',
                    body: JSON.stringify({ type: '/problems/validation', title: 'Unprocessable Entity', status: 422, detail: 'Отказ сервера E2E' }),
                  })
                : route.fallback(),
            );
            await goto(page, row.page(await ctxOf(page)));
            await row.open(page);
            await row.fill!(page);
            const sc = inl.scope(page);
            const typed = await values(sc);
            await sc.getByRole('button', { name: inl.submit, exact: true }).click();
            await expect(sc.getByRole('alert')).toContainText('Отказ сервера E2E');
            expect(await values(sc)).toEqual(typed);
            expect(problems, problems.join('\n')).toEqual([]);
          });
          test(`forms_inline_leave_asks ${row.id} ${w.name}`, async ({ page }) => {
            const native = nativeDialogs(page);
            const c = await ctxOf(page);
            await goto(page, row.page(c));
            await row.open(page);
            await row.fill!(page);
            const sc = inl.scope(page);
            const typed = await values(sc);
            const here = page.url();
            await page.evaluate((h) => (location.hash = h), `#/u/${c.user}/s/${env('E2E_FORMS_SOLUTION')}/j/glossary`);
            await expect.poll(() => native.length).toBe(1);
            expect(native[0]).toBe(LEAVE_EDIT);
            await expect(page).toHaveURL(here);
            expect(await values(sc)).toEqual(typed);
          });
          continue;
        }

        test(`forms_open_focus_trap_esc ${row.id} ${w.name}`, async ({ page }) => {
          const problems = watch(page);
          const native = nativeDialogs(page);
          const d = await openRow(page, row);
          // фокус: поле `first`; подтверждение — «Отмена» у `danger`, иначе основная кнопка
          if (row.first) await expect(d.getByLabel(row.first).first()).toBeFocused();
          else if (row.danger) await expect(d.getByRole('button', { name: 'Отмена', exact: true })).toBeFocused();
          else await expect(submitOf(d)).toBeFocused();
          // Tab по кругу внутри окна
          for (let i = 0; i < 20; i++) {
            await page.keyboard.press('Tab');
            expect(await focusInDialog(page), `Tab ${i + 1}: фокус в окне`).toBe(true);
          }
          // Esc без набранного закрывает, фокус — на кнопке, открывшей окно
          await page.keyboard.press('Escape');
          await expect(d).toBeHidden();
          await expect.poll(() => openerFocused(page)).toBe(true);
          expect(native).toEqual([]);
          expect(problems, problems.join('\n')).toEqual([]);
        });

        if (!three) continue;

        if (row.fill) {
          test(`forms_dirty_close_asks_inside ${row.id} ${w.name}`, async ({ page }) => {
            const problems = watch(page);
            const native = nativeDialogs(page);
            const d = await openRow(page, row);
            await row.fill!(page);
            const typed = await values(d);
            await page.keyboard.press('Escape');
            const ask = d.getByTestId('fd-ask');
            await expect(ask).toBeVisible();
            await expect(ask).toContainText(LEAVE_EDIT);
            // «Продолжить правку» — текст цел, окно открыто
            await ask.getByRole('button', { name: 'Продолжить правку' }).click();
            await expect(ask).toBeHidden();
            await expect(d).toBeVisible();
            expect(await values(d)).toEqual(typed);
            // «✕» с набранным — тот же вопрос; «Закрыть без сохранения» закрывает
            await d.getByRole('button', { name: 'Закрыть', exact: true }).click();
            await expect(ask).toBeVisible();
            await ask.getByRole('button', { name: 'Закрыть без сохранения' }).click();
            await expect(d).toBeHidden();
            // вопрос — один раз, в окне (без `window.confirm` формы)
            expect(native).toEqual([]);
            expect(problems, problems.join('\n')).toEqual([]);
          });

          test(`forms_dirty_question_announced ${row.id} ${w.name}`, async ({ page }) => {
            const d = await openRow(page, row);
            await row.fill!(page);
            await page.keyboard.press('Escape');
            const keep = d.getByRole('button', { name: 'Продолжить правку' });
            await expect(keep).toBeFocused();
            const described = await keep.evaluate((b) => {
              const ids = (b.getAttribute('aria-describedby') ?? '').split(/\s+/).filter(Boolean);
              return ids.map((id) => document.getElementById(id)?.textContent?.trim() ?? '').join(' ');
            });
            expect(described).toBe(LEAVE_EDIT);
            // живой области с тем же текстом нет: вопрос объявляется один раз — описанием кнопки
            const live = await page.evaluate(
              (q) =>
                [...document.querySelectorAll('[aria-live], [role="status"], [role="alert"], [role="log"]')].filter((e) => (e.textContent ?? '').includes(q)).length,
              LEAVE_EDIT,
            );
            expect(live).toBe(0);
            // Esc при вопросе — продолжить правку
            await page.keyboard.press('Escape');
            await expect(d.getByTestId('fd-ask')).toBeHidden();
            await expect(d).toBeVisible();
          });
        }

        if (row.route) {
          test(`forms_server_error_keeps_input ${row.id} ${w.name}`, async ({ page }) => {
            const problems = watch(page, (_p, s) => s === 422);
            const r = row.route!;
            await page.route(r.url, (route) =>
              route.request().method() === r.method
                ? route.fulfill({
                    status: 422,
                    contentType: 'application/problem+json',
                    body: JSON.stringify({ type: '/problems/validation', title: 'Unprocessable Entity', status: 422, detail: 'Отказ сервера E2E' }),
                  })
                : route.fallback(),
            );
            const d = await openRow(page, row);
            if (row.fill) await row.fill(page);
            const typed = await values(d);
            await submitOf(d).click();
            await expect(d.getByRole('alert')).toContainText('Отказ сервера E2E');
            await expect(d).toBeVisible();
            expect(await values(d)).toEqual(typed);
            expect(problems, problems.join('\n')).toEqual([]);
          });
        }

        if (phone && row.confirm) {
          test(`forms_phone_header_actions ${row.id} ${w.name}`, async ({ page }) => {
            // окно-подтверждение без полей на телефоне — прежний `.dialog` во всю ширину, не во весь
            // экран; кнопки — внизу окна, «✕» нет
            const d = await openRow(page, row);
            const vp = page.viewportSize()!;
            const box = (await d.boundingBox())!;
            expect(box.width).toBeGreaterThanOrEqual(vp.width - 1);
            expect(box.y, 'не от верха экрана').toBeGreaterThan(40);
            expect(box.height, 'не во всю высоту').toBeLessThan(vp.height * 0.7);
            await expect(d.getByRole('button', { name: 'Отмена', exact: true })).toBeVisible();
            await expect(d.getByRole('button', { name: 'Закрыть', exact: true })).toHaveCount(0);
          });
        }

        if (phone && !row.confirm) {
          test(`forms_phone_header_actions ${row.id} ${w.name}`, async ({ page }) => {
            const d = await openRow(page, row);
            const vp = page.viewportSize()!;
            const box = (await d.boundingBox())!;
            // во весь экран
            expect(box.x).toBeLessThanOrEqual(1);
            expect(box.y).toBeLessThanOrEqual(1);
            expect(box.width).toBeGreaterThanOrEqual(vp.width - 1);
            expect(box.height).toBeGreaterThanOrEqual(vp.height - 1);
            // «✕» и основное действие — в видимой шапке, нижнего ряда «Отмена» нет
            const head = d.locator('header.fd-head');
            const hb = (await head.boundingBox())!;
            expect(hb.y).toBeLessThan(60);
            await expect(head.getByRole('button', { name: 'Закрыть', exact: true })).toBeVisible();
            await expect(head.locator('button[type="submit"]')).toBeVisible();
            await expect(d.getByRole('button', { name: 'Отмена', exact: true })).toBeHidden();
            // тело прокручивается само (шапка закреплена)
            const ov = await d.locator('.fd-body').evaluate((e) => getComputedStyle(e).overflowY);
            expect(ov).toBe('auto');
            // поля — плотно сверху вниз, свободное место — внизу (ревью r1, I1)
            await fieldsDense(d);
            // «✕» — в шапке слева, у заголовка (лист iOS/Material)
            const x = (await head.getByRole('button', { name: 'Закрыть', exact: true }).boundingBox())!;
            expect(x.x).toBeLessThan(vp.width / 3);
          });

          for (const noNav of NO_NAV_ROWS.has(row.id) ? [false, true] : [false]) {
          test(`forms_phone_back_closes ${row.id} ${w.name}${noNav ? ' no_nav_api' : ''}`, async ({ page }) => {
            if (noNav) await hideNavApi(page);
            const native = nativeDialogs(page);
            let d = await openRow(page, row);
            const url = page.url();
            const at = () => placeOf(page);
            // «Назад» закрывает окно, адрес прежний, страница под окном на месте
            await page.goBack({ waitUntil: 'commit' });
            await expect(d).toBeHidden();
            expect(page.url()).toBe(url);
            await expect(page.getByRole('button', { name: 'Поиск', exact: true })).toBeVisible();
            const index0 = await at();
            // с набранным — вопрос в окне; «Продолжить правку» — окно и текст на месте; снова
            // «Назад» — снова вопрос
            await row.open(page);
            d = dialogOf(page, row.dialog ?? row.name);
            await expect(d).toBeVisible();
            await row.fill?.(page);
            const typed = await values(d);
            await page.goBack({ waitUntil: 'commit' });
            const ask = d.getByTestId('fd-ask');
            if (row.fill) {
              await expect(ask).toBeVisible();
              expect(page.url()).toBe(url);
              await ask.getByRole('button', { name: 'Продолжить правку' }).click();
              await expect(d).toBeVisible();
              expect(await values(d)).toEqual(typed);
              await page.goBack({ waitUntil: 'commit' });
              await expect(ask).toBeVisible();
              await ask.getByRole('button', { name: 'Закрыть без сохранения' }).click();
            }
            await expect(d).toBeHidden();
            expect(page.url()).toBe(url);
            // закрытие «✕» лишней записи истории не оставляет
            await row.open(page);
            await expect(d).toBeVisible();
            await d.getByRole('button', { name: 'Закрыть', exact: true }).click();
            await expect(d).toBeHidden();
            await expect.poll(at).toBe(index0);
            expect(await page.evaluate(() => (history.state as { zamLayer?: unknown } | null)?.zamLayer ?? null)).toBeNull();
            expect(native).toEqual([]);
          });
          }
        }

        if (row.confirm && row.danger) {
          test(`forms_confirm_danger_focus_cancel ${row.id} ${w.name}`, async ({ page }) => {
            const d = await openRow(page, row);
            const cancel = d.getByRole('button', { name: 'Отмена', exact: true });
            await expect(cancel).toBeFocused();
            await page.keyboard.press('Enter');
            await expect(d).toBeHidden();
            // ничего не удалено и не снято: запись адреса — на месте, с прежним статусом
            const id = /\/j\/[a-z]+\/([^/?#]+)/.exec(page.url())?.[1];
            expect(id, 'запись в адресе').toBeTruthy();
            const e = await getEntry(decodeURIComponent(id!));
            expect(e.deleted_at).toBeNull();
            expect(e.status).not.toBe('dropped');
          });
        }

        // последним: успешная отправка (замена меняет данные строки — она переходит на новую запись)
        if (row.after === 'card' && row.fill) {
          test(`forms_create_opens_created_card ${row.id} ${w.name}`, async ({ page }) => {
            const problems = watch(page);
            // замена меняет запись строки — на время теста своя свежая запись; общая запись строки
            // остаётся живой для прочих тестов (и при падении этого)
            const keep = process.env.E2E_FORMS_INSIGHT_SUP;
            if (row.id === 'insight-supersede') {
              process.env.E2E_FORMS_INSIGHT_SUP = (await decision(env('E2E_FORMS_SOLUTION'), `Решение форм E2E: замена ${Date.now().toString(36)}`)).id;
            }
            try {
              const d = await openRow(page, row);
              const before = page.url();
              await row.fill!(page);
              await submitOf(d).click();
              await expect(d).toBeHidden();
              await expect(page).not.toHaveURL(before);
              await expect(page).toHaveURL(/\/j\/(tasks|insights|ideas)\/[0-9A-Z]{26}$/);
              await headingInView(page);
              expect(problems, problems.join('\n')).toEqual([]);
            } finally {
              process.env.E2E_FORMS_INSIGHT_SUP = keep;
            }
          });
        }

        if (phone && (row.id === 'task-create' || row.id === 'idea-create')) {
          for (const noNav of [false, true]) {
          test(`forms_phone_create_then_back ${row.id} ${w.name}${noNav ? ' no_nav_api' : ''}`, async ({ page }) => {
            if (noNav) await hideNavApi(page);
            await goto(page, row.page(await ctxOf(page)));
            const board = page.url();
            // длина истории до окна: окно кладёт запись слоя, «Добавить» заменяет её записью
            // карточки — прибавилась одна запись, а не две
            const len0 = await page.evaluate(() => history.length);
            await row.open(page);
            const d = dialogOf(page, row.dialog ?? row.name);
            await expect(d).toBeVisible();
            await row.fill!(page);
            await submitOf(d).click();
            await expect(d).toBeHidden();
            await expect(page).toHaveURL(/\/j\/(tasks|ideas)\/[0-9A-Z]{26}$/);
            await headingInView(page);
            expect(await page.evaluate(() => history.length)).toBe(len0 + 1);
            await page.goBack({ waitUntil: 'commit' });
            await expect(page).toHaveURL(board);
            await expect(page.getByRole('button', { name: row.id === 'task-create' ? 'Новая задача' : 'Новая идея', exact: true })).toBeVisible();
          });
          }
        }
      }
    });
  }
}

// ---- по местам ----

/** Наполнение журналов для `card_in_view_on_open`: своё решение, по 30 записей с длинным текстом. */
let viewGuid = '';
const viewIds: Record<string, string[]> = {};
async function seedCardsInView(): Promise<string> {
  if (viewGuid) return viewGuid;
  const guid = await newSolution(`Карточки E2E ${Date.now().toString(36)}`);
  const make = async (kind: string, extra: (i: number) => Record<string, unknown>) => {
    const ids: string[] = [];
    for (let i = 1; i <= 30; i++) ids.push((await entryOf(guid, { kind, ...extra(i) })).id);
    viewIds[kind] = ids;
  };
  await make('task', (i) => ({ title: `Задача списка ${String(i).padStart(2, '0')}`, body: longBody('задача') }));
  await make('insight', (i) => ({ title: `Insight списка ${String(i).padStart(2, '0')}`, body: longBody('insight'), data: { kind: 'discovery' }, force: true }));
  await make('idea', (i) => ({ title: `Идея списка ${String(i).padStart(2, '0')}`, body: longBody('идея'), data: { origin: 'human' } }));
  await make('term', (i) => ({ title: `Термин${String(i).padStart(2, '0')}`, data: { definition: `Определение ${i}: ${'длинное определение термина. '.repeat(9)}` }, body: longBody('термин') }));
  const sessions: string[] = [];
  for (let i = 1; i <= 30; i++) {
    const id = `e2e-forms-view-${String(i).padStart(2, '0')}-${Date.now().toString(36)}`;
    await api('POST', 'sessions', { id, agent: 'claude', tokens: { pre_tool: { calls: 1, tokens: 10 } } });
    sessions.push(id);
  }
  viewIds.session = sessions;
  viewGuid = guid;
  return guid;
}

const JOURNALS: { name: string; kind: string; path: (c: PageCtx, g: string, id?: string) => string; row: (p: Page, id: string) => Locator }[] = [
  {
    name: 'Задачи',
    kind: 'task',
    path: (c, g, id) => `#/u/${c.user}/s/${g}/j/tasks${id ? `/${id}` : ''}`,
    row: (p, id) => p.locator(`[data-task="${id}"] button.title`),
  },
  {
    name: 'Insights',
    kind: 'insight',
    path: (c, g, id) => `#/u/${c.user}/s/${g}/j/insights${id ? `/${id}` : ''}`,
    row: (p, id) => p.locator(`tr[data-entry="${id}"] button`).first(),
  },
  {
    name: 'Идеи',
    kind: 'idea',
    path: (c, g, id) => `#/u/${c.user}/s/${g}/j/ideas${id ? `/${id}` : ''}`,
    row: (p, id) => p.locator(`[data-idea="${id}"] button.title`),
  },
  {
    name: 'Словарь',
    kind: 'term',
    path: (c, g, id) => `#/u/${c.user}/s/${g}/j/glossary${id ? `/${id}` : ''}`,
    row: (p, id) => p.locator(`[data-term="${id}"]`).getByRole('button').first(),
  },
  {
    name: 'Сессии',
    kind: 'session',
    path: (_c, _g, id) => `#/sessions${id ? `/${encodeURIComponent(id)}` : ''}`,
    row: (p, id) => p.locator(`tr[data-session="${id}"] button`).first(),
  },
];

/** Карточка журнала — блок рядом со списком, панель или вместо списка. */
const detailOf = (p: Page) => p.locator('.jsplit-detail');

if (!SNAP) {
  for (const w of W4) {
    test.describe(`карточки · ${w.name}`, () => {
      test.use({ viewport: VIEWPORTS[w.name], ...(w.touch ? touch : {}) });

      for (const j of JOURNALS) {
        test(`card_in_view_on_open ${j.kind} ${w.name}`, async ({ page }) => {
          test.setTimeout(120_000);
          const problems = watch(page);
          const g = await seedCardsInView();
          const c = await ctxOf(page);
          const ids = viewIds[j.kind];
          await goto(page, j.path(c, g));
          // строка далеко внизу списка (новые — сверху, у Словаря — по алфавиту): прокрутить к ней и
          // открыть
          const last = j.kind === 'term';
          const pick = last ? ids[ids.length - 1] : ids[0];
          const row = j.row(page, pick);
          await row.scrollIntoViewIfNeeded();
          await row.click();
          await expect(page).toHaveURL(new RegExp(`${encodeURIComponent(pick).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
          const d = detailOf(page);
          await expect(d.locator('h2, h3').first()).toBeVisible();
          await headingInView(page, d);
          // прежняя карточка прокручена вниз, открыта другая запись — новая карточка с начала
          await d.evaluate((el) => {
            el.scrollTop = el.scrollHeight;
            window.scrollTo(0, document.documentElement.scrollHeight);
          });
          const next = last ? ids[ids.length - 2] : ids[1];
          await page.evaluate((h) => (location.hash = h), j.path(c, g, next));
          await expect(page).toHaveURL(new RegExp(`${encodeURIComponent(next).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
          await expect.poll(() => d.evaluate((el) => el.scrollTop)).toBe(0);
          await headingInView(page, d);
          expect(problems, problems.join('\n')).toEqual([]);
        });
      }

      test(`card_actions_in_view_on_open ${w.name}`, async ({ page }) => {
        test.setTimeout(120_000);
        const g = await newSolution(`План E2E ${Date.now().toString(36)}`);
        const parent = await entryOf(g, { kind: 'task', title: 'Общая задача с 50 подзадачами', body: longBody('план') });
        for (let i = 1; i <= 50; i++) {
          const k = await entryOf(g, { kind: 'task', title: `Шаг ${String(i).padStart(2, '0')}`, body: '' });
          await api('POST', 'edges', { src: `e:${k.id}`, type: 'part_of', dst: `e:${parent.id}` });
        }
        const c = await ctxOf(page);
        await goto(page, `#/u/${c.user}/s/${g}/j/tasks/${parent.id}`);
        const card = page.getByRole('article', { name: 'Карточка задачи' });
        await expect(card.getByRole('heading', { name: 'Общая задача с 50 подзадачами' })).toBeVisible();
        const vh = page.viewportSize()!.height;
        for (const l of [card.getByRole('group', { name: 'Смена статуса' }), card.getByLabel('Следующий шаг')]) {
          const b = (await l.boundingBox())!;
          expect(b.y + b.height, `${l} — без прокрутки`).toBeLessThanOrEqual(vh);
          expect(b.y).toBeGreaterThanOrEqual(0);
        }
        // текст и подзадачи — ниже ряда действий
        const sb = (await card.getByTestId('task-subtasks').boundingBox())!;
        const ab = (await card.getByRole('group', { name: 'Смена статуса' }).boundingBox())!;
        expect(sb.y).toBeGreaterThan(ab.y);
      });
    });
  }

  test.describe('по местам · desktop', () => {
    test.use({ viewport: VIEWPORTS.desktop });

    test('task_status_no_fields_immediate', async ({ page }) => {
      const problems = watch(page);
      const g = env('E2E_FORMS_SOLUTION');
      const t = await entryOf(g, { kind: 'task', title: `Задача статуса ${Date.now().toString(36)}`, body: '' });
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}/s/${g}/j/tasks/${t.id}`);
      const card = page.getByRole('article', { name: 'Карточка задачи' });
      // «→ в работе» — сразу, без окна, итог строкой
      await card.getByRole('button', { name: '→ в работе', exact: true }).click();
      // окна формы нет (панель карточки на доске — тоже `role="dialog"`, она не в счёт)
      await expect(page.locator('[data-form-dialog]')).toHaveCount(0);
      await expect(card.getByRole('status')).toHaveText('Статус: в работе.');
      await expect(card.locator('.chip.status')).toHaveText('в работе');
      // «→ снят» — подтверждение (отмена — статус прежний)
      await card.getByRole('button', { name: '→ снят', exact: true }).click();
      const drop = dialogOf(page, 'Снять задачу?');
      await expect(drop).toBeVisible();
      await drop.getByRole('button', { name: 'Отмена', exact: true }).click();
      await expect(drop).toBeHidden();
      expect((await getEntry(t.id)).status).toBe('in_progress');
      // «→ выполнен» — подтверждение; согласие — выполнен
      await card.getByRole('button', { name: '→ выполнен', exact: true }).click();
      const done = dialogOf(page, 'Отметить задачу выполненной?');
      await expect(done).toBeVisible();
      await done.getByRole('button', { name: 'Отметить выполненной' }).click();
      await expect(done).toBeHidden();
      await expect(card.locator('.chip.status')).toHaveText('выполнен');
      await expect(card.getByRole('status')).toHaveText('Статус: выполнен.');
      // «Переоткрыть» у выполненной — окно с обязательной причиной
      await card.getByRole('button', { name: 'Переоткрыть', exact: true }).click();
      const re = dialogOf(page, 'Переоткрыть задачу');
      await expect(re).toBeVisible();
      await expect(re.getByLabel('Причина переоткрытия')).toBeFocused();
      await submitOf(re).click();
      await expect(re).toBeVisible();
      expect((await getEntry(t.id)).status).toBe('done');
      await re.getByLabel('Причина переоткрытия').fill('нашли ошибку');
      await submitOf(re).click();
      await expect(re).toBeHidden();
      await expect(card.locator('.chip.status')).toHaveText('открыт');
      expect(problems, problems.join('\n')).toEqual([]);
    });

    test('leave_guard_stack_board_forms', async ({ page }) => {
      const g = env('E2E_FORMS_SOLUTION');
      const t = await entryOf(g, { kind: 'task', title: `Задача ухода ${Date.now().toString(36)}`, body: '' });
      await patch(t, { status: 'done' });
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}/s/${g}/j/tasks/${t.id}`);
      const native = nativeDialogs(page);
      await page.getByRole('article', { name: 'Карточка задачи' }).getByRole('button', { name: 'Переоткрыть', exact: true }).click();
      const re = dialogOf(page, 'Переоткрыть задачу');
      await re.getByLabel('Причина переоткрытия').fill('набранная причина');
      // переход Ctrl+K к другой записи: один вопрос (системное окно роутера), «Отмена» — остаёмся
      await page.keyboard.press('Control+k');
      const search = page.getByRole('combobox', { name: 'Поиск по заметкам' });
      await expect(search).toBeVisible();
      await search.fill('Решение форм');
      await expect(page.locator('#search-opt-0')).toBeVisible({ timeout: 15_000 });
      await search.press('Enter');
      await expect.poll(() => native.length).toBe(1);
      expect(native[0]).toBe(LEAVE_EDIT);
      await expect(page).toHaveURL(new RegExp(`/j/tasks/${t.id}$`));
      await expect(re).toBeVisible();
      await expect(re.getByLabel('Причина переоткрытия')).toHaveValue('набранная причина');
      await settled(page);
      expect(native.length).toBe(1);
    });

    test('insights_409_keeps_typed', async ({ page }) => {
      const problems = watch(page, (p, s) => s === 409 && /\/supersede$/.test(p));
      const g = env('E2E_FORMS_SOLUTION');
      const ins = await decision(g, `Insight 409 ${Date.now().toString(36)}`);
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}/s/${g}/j/insights/${ins.id}`);
      await page.getByRole('article', { name: 'Карточка insight' }).getByRole('button', { name: 'Заменить', exact: true }).click();
      const d = dialogOf(page, 'Заменить запись');
      await d.getByLabel('Текст', { exact: true }).fill('Набранный текст замены.');
      // запись правят в другом месте, пока окно открыто
      const fresh = `Заголовок из другого места ${Date.now().toString(36)}`;
      await patch(ins, { title: fresh });
      await submitOf(d).click();
      await expect(d.getByRole('alert')).toContainText('Запись изменилась, пока вы правили');
      await expect(d).toBeVisible();
      await expect(d.getByLabel('Текст', { exact: true })).toHaveValue('Набранный текст замены.');
      // нетронутое поле — из свежей записи
      await expect(d.getByLabel('Заголовок', { exact: true })).toHaveValue(fresh);
      // повтор сохраняет
      await submitOf(d).click();
      await expect(d).toBeHidden();
      await expect(page.getByRole('article', { name: 'Карточка insight' }).getByRole('heading', { name: fresh })).toBeVisible();
      expect(problems, problems.join('\n')).toEqual([]);
    });

    test('ideas_409_keeps_typed', async ({ page }) => {
      const problems = watch(page, (p, s) => s === 409 && /\/entries\/[^/]+$/.test(p));
      const g = env('E2E_FORMS_SOLUTION');
      const idea = await entryOf(g, { kind: 'idea', title: `Идея 409 ${Date.now().toString(36)}`, body: 'Было.', data: { origin: 'human' } });
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}/s/${g}/j/ideas/${idea.id}`);
      const card = page.getByRole('article', { name: 'Карточка идеи' });
      await card.getByRole('button', { name: 'Изменить', exact: true }).click();
      const form = card.getByRole('form', { name: 'Правка идеи' });
      await expect(form.getByLabel('Заголовок')).toBeFocused();
      const typed = `Заголовок человека ${Date.now().toString(36)}`;
      await form.getByLabel('Заголовок').fill(typed);
      // агент дополнил идею, пока форма открыта (дополнение идеи человека токену разрешено)
      await api('PATCH', `entries/${idea.id}`, { append_body: 'Дополнение агента.' }, { 'If-Match': `"${idea.rev}"` });
      await form.getByRole('button', { name: 'Сохранить' }).click();
      await expect(form.getByRole('alert')).toContainText('Запись изменилась, пока вы правили');
      // набранное цело, нетронутое поле (замысел) — из свежей идеи
      await expect(form.getByLabel('Заголовок')).toHaveValue(typed);
      await expect(form.getByLabel('Замысел')).toHaveValue(/Дополнение агента\./);
      await form.getByRole('button', { name: 'Сохранить' }).click();
      await expect(form).toBeHidden();
      await expect(card.getByRole('heading', { name: typed })).toBeFocused();
      const saved = await getEntry(idea.id);
      expect(saved.title).toBe(typed);
      expect(saved.body).toContain('Дополнение агента.');
      expect(problems, problems.join('\n')).toEqual([]);
    });

    test('insight_create_duplicate_force', async ({ page }) => {
      const problems = watch(page, (p, s) => s === 409 && /\/entries$/.test(p));
      const g = env('E2E_FORMS_SOLUTION');
      const title = `Дубль форм ${Date.now().toString(36)}`;
      const old = await entryOf(g, { kind: 'insight', title, body: 'Первая запись.', data: { kind: 'gotcha' } });
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}/s/${g}/j/insights`);
      await page.getByRole('button', { name: 'Новый insight', exact: true }).click();
      const d = dialogOf(page, 'Новый insight');
      await expect(d.getByLabel('Заголовок')).toBeFocused();
      await d.getByLabel('Заголовок').fill(title);
      await d.getByLabel('Текст').fill('Вторая запись о том же.');
      await submitOf(d).click();
      // кандидат-дубль — ссылкой в окне; «Записать всё равно»
      const dups = d.getByTestId('insight-duplicates');
      await expect(dups.getByRole('link', { name: title })).toHaveAttribute('href', new RegExp(`/j/insights/${old.id}$`));
      await d.getByRole('button', { name: 'Записать всё равно' }).click();
      await expect(d).toBeHidden();
      await expect(page).toHaveURL(/\/j\/insights\/[0-9A-Z]{26}$/);
      expect(page.url().endsWith(old.id)).toBe(false);
      const card = page.getByRole('article', { name: 'Карточка insight' });
      await expect(card.getByRole('heading', { name: title })).toBeFocused();
      await expect(card.getByRole('status')).toHaveText('Insight записан.');
      expect(problems, problems.join('\n')).toEqual([]);
    });
  });

  test.describe('по местам · phone', () => {
    test.use({ viewport: VIEWPORTS.phone, ...touch });

    test('forms_phone_prompt_dense', async ({ page }) => {
      // окно `Prompt` с полем (дерево: «Новая заметка») на телефоне — во весь экран, поле сразу под
      // шапкой, свободное место — внизу (ревью r1, I1: в 0.0.15 оно было компактным окном)
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}`);
      const toggle = page.getByRole('button', { name: 'Показать дерево' });
      if (await toggle.isVisible()) await toggle.click();
      await page.locator('nav[aria-label="Дерево заметок"]').getByRole('button', { name: 'Действия: Knowledge' }).first().click();
      await page.getByRole('menuitem', { name: 'Новая заметка' }).click();
      const d = dialogOf(page, 'Новая заметка');
      await expect(d.getByLabel('Имя')).toBeFocused();
      await fieldsDense(d);
      // поле — под шапкой и пояснением, в верхней части экрана, а не посреди пустого окна
      const field = (await d.getByLabel('Имя').boundingBox())!;
      expect(field.y + field.height, 'поле — в верхней части окна').toBeLessThan(page.viewportSize()!.height / 3);
      await page.keyboard.press('Escape');
      await expect(d).toBeHidden();
    });

    test('leave_guard_stack_board_forms phone', async ({ page }) => {
      const g = env('E2E_FORMS_SOLUTION');
      const t = await entryOf(g, { kind: 'task', title: `Задача ухода телефона ${Date.now().toString(36)}`, body: '' });
      await patch(t, { status: 'done' });
      const c = await ctxOf(page);
      await goto(page, `#/u/${c.user}/s/${g}/j/tasks/${t.id}`);
      const native = nativeDialogs(page);
      await page.getByRole('article', { name: 'Карточка задачи' }).getByRole('button', { name: 'Переоткрыть', exact: true }).click();
      const re = dialogOf(page, 'Переоткрыть задачу');
      await re.getByLabel('Причина переоткрытия').fill('набранная причина');
      // уход по адресу (ссылка другой записи) — один вопрос роутера, «Отмена» — окно и текст на месте
      await page.evaluate((h) => (location.hash = h), `#/u/${c.user}/s/${g}/j/tasks/${env('E2E_FORMS_TASK_OPEN')}`);
      await expect.poll(() => native.length).toBe(1);
      await expect(re).toBeVisible();
      await expect(re.getByLabel('Причина переоткрытия')).toHaveValue('набранная причина');
      await settled(page);
      expect(native.length).toBe(1);
    });
  });
}
