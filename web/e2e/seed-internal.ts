// Наполнение e2e страниц M7.3 (спецификация §6.9): записи личного Vault администратора для
// подключения модели — подставная модель в LAN-петле (`127.0.0.1`, global-setup разрешает её
// `ZAM_WORKS__ALLOW_LOOPBACK=true`), адрес «вне LAN» из TEST-NET-3 (`https://203.0.113.10/v1`, без
// DNS и соединения; решение 65) и своя запись для сценария смены адреса по токену; кандидат в Rules
// для строк `FORM_CASES` (окна «Принять» и «Отклонить» без отправки). REST — по токену
// администратора (агент): человек кандидатов не предлагает.

import type { Page } from '@playwright/test';

// ---- REST сценариев M7.3: агент (токен администратора) и человек (сессия страницы) ----

const envOf = (k: string): string => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} не задан (global-setup)`);
  return v;
};

/** REST по токену администратора — агент (`extra` — заголовки, например `X-Zam-Session`). */
export async function agent<T = Record<string, unknown>>(method: string, path: string, body?: unknown, extra: Record<string, string> = {}): Promise<T> {
  const r = await fetch(`${envOf('E2E_BASE_URL')}api/v1/${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${envOf('E2E_ADMIN_TOKEN')}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...extra,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (r.status === 204 ? {} : await r.json()) as T;
}

/** REST от имени человека — сессия страницы (cookie) и её CSRF (`GET /auth/me`): настройки
 * исполнителя и проверку подключения агент (токен) не меняет — 403. */
export async function human<T = Record<string, unknown>>(page: Page, method: string, path: string, body?: unknown): Promise<T> {
  const base = envOf('E2E_BASE_URL');
  const me = await page.request.get(`${base}api/v1/auth/me`);
  const { csrf } = (await me.json()) as { csrf: string };
  const r = await page.request.fetch(`${base}api/v1/${path}`, { method, headers: { 'X-CSRF-Token': csrf }, data: body });
  if (!r.ok()) throw new Error(`${method} ${path}: HTTP ${r.status()} ${await r.text()}`);
  return (r.status() === 204 ? {} : await r.json()) as T;
}

/** Исполнитель своего пользователя — в исходное состояние: режим «выкл», без видов и подключения. */
export async function resetWorks(page: Page): Promise<void> {
  const s = await human<{ works: { mode: string; kinds: Record<string, string>; model: unknown; check: string } }>(page, 'GET', 'auth/settings');
  const w: Record<string, unknown> = {};
  if (s.works.mode !== 'off') w.mode = 'off';
  const kinds = Object.fromEntries(Object.keys(s.works.kinds).map((k) => [k, null]));
  if (Object.keys(kinds).length) w.kinds = kinds;
  if (s.works.model) w.model = null;
  if (s.works.check !== 'on') w.check = 'on';
  if (Object.keys(w).length) await human(page, 'PATCH', 'auth/settings', { works: w });
}

/** Ключ подставной модели — склейкой (push protection: фиктивные ключи в коде не целиком). */
export const MOCK_KEY = ['e2e', 'mock', 'model', 'key'].join('-');

export interface InternalSeed {
  /** id записи Vault «модель в LAN» (подставная модель, `/v1`). */
  vaultLan: string;
  /** id записи Vault «вне LAN» (`https://203.0.113.10/v1`). */
  vaultExt: string;
  /** id записи Vault для сценария «адрес изменён по токену». */
  vaultMove: string;
  /** Кандидат в Rules строк форм. */
  proposalForms: string;
}

async function call<T>(base: string, token: string, method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(`${base}api/v1/${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (await r.json()) as T;
}

// ---- эпизод регрессии для подстановки (`page.route`, решение 66) ----
//
// На release-сборке регрессию не подготовить: база — 5 ночных прогонов, ручные в неё не входят, а
// ночной прогон делает только `NightRunner` в окне. Подставляются ответы `GET /check/status` и
// `GET /check/runs` той же формы, что у сервера (`crates/zam-server/src/api/check.rs`): эпизод из
// двух ночных прогонов (N0 — его начало, N1 — последний ночной) и ручной M1 после них — последний
// законченный прогон показывает идущий эпизод; принять можно только N1.

export const RUN_N0 = '01KQ00000000000000000E2EN0';
export const RUN_N1 = '01KQ00000000000000000E2EN1';
export const RUN_M1 = '01KQ00000000000000000E2EM1';

/** Ответы статуса и списка прогонов; `accepted` — норма N1 принята (ответ после «Принять»). */
export function checkFixture(accepted = false): { status: Record<string, unknown>; runs: { items: Record<string, unknown>[] } } {
  const compared = { hit_search: 0.62, hit_prompt: null, negatives_shown: 0.1, p95: 120, common: 24 };
  const verdict = {
    kind: 'regression',
    flags: [{ name: 'hit_search', value: 0.5, median: 0.62, delta_pp: -12 }],
    after_update: false,
    few: false,
    episode: RUN_N0,
    base_n: 5,
    base_of: 5,
    compared_with: compared,
  };
  const part = { n: 24, hit_search: 0.5, mrr10: 0.4, none: 0.3, beyond: 0.2, hit_prompt: null, prompt_n: 0, hard: null, hard_n: 0 };
  const run = (id: string, trigger: 'nightly' | 'manual', night: string | null, at: string, acceptedAt: string | null) => ({
    run: id,
    trigger,
    night,
    status: 'done',
    started_at: at,
    finished_at: at,
    fingerprint: null,
    fp_hash: 'e2e',
    context: null,
    metrics: { dev: part, test: part, all: part, negatives_shown: 0.1, negatives_n: 30, search_p50_ms: 80, search_p95_ms: 130, degraded: false, prompt_on: true },
    verdict,
    flags: [],
    episode: RUN_N0,
    accepted_at: acceptedAt,
    accepted_by: acceptedAt ? 'admin' : null,
    error: null,
  });
  const n0 = run(RUN_N0, 'nightly', '2026-11-22', '2026-11-22T01:40:00.000000Z', null);
  const n1 = run(RUN_N1, 'nightly', '2026-11-23', '2026-11-23T01:40:00.000000Z', accepted ? new Date().toISOString() : null);
  const m1 = run(RUN_M1, 'manual', null, '2026-11-23T09:15:00.000000Z', null);
  const trend = (r: ReturnType<typeof run>) => ({
    run: r.run,
    date: r.night ?? '2026-11-23',
    trigger: r.trigger,
    status: r.status,
    verdict: 'regression',
    hit_search: 0.5,
    hit_prompt: null,
    negatives_shown: 0.1,
    p95: 130,
  });
  return {
    status: {
      last: { ...m1, compared_with: compared },
      running: null,
      trend: [m1, n1, n0].map(trend),
      failing: [
        {
          probe: 'x:01KQ00000000000000000E2EP1',
          question: 'Почему карточка задачи открывается под доской и как сделать её видимой сразу',
          kind: 'probe',
          split: 'test',
          target: 'i:01KQ00000000000000000E2EI1',
          rank: null,
          advice: 'term',
          advice_text: 'нет слов спрашивающего: термин или псевдоним (term), дописать запись',
        },
      ],
      advice: [{ code: 'term', text: 'нет слов спрашивающего: термин или псевдоним (term), дописать запись' }],
      probes: { active: 31, by_kind: { probe: 24, negative: 7 }, by_split: { dev: 16, test: 15 } },
      check: { server: 'off', user: 'on' },
    },
    runs: { items: [m1, n1, n0] },
  };
}

/** Ответ `GET /check/runs?…` по подставленному списку — фильтр как у сервера: `trigger`, `status`,
 * `limit` (новые первыми). Точка «Проверка» и страница берут последний законченный ночной прогон
 * фильтром `?trigger=nightly&status=done&limit=1`. */
export function runsReply(url: string, runs: { items: Record<string, unknown>[] }): { items: Record<string, unknown>[] } {
  const q = new URL(url).searchParams;
  const trigger = q.get('trigger');
  const status = q.get('status');
  const limit = Number(q.get('limit') ?? 30);
  const items = runs.items.filter((r) => (!trigger || r.trigger === trigger) && (!status || r.status === status));
  return { items: items.slice(0, limit) };
}

// ---- предложения уборки для подстановки (`page.route`, M7.5 §8.8 п. 1) ----
//
// Кандидат `merge` на release-сборке e2e не подготовить: почти-дубли insight ищутся по векторам
// эмбеддера (модели у сервера e2e нет), противоречие — старше трёх суток. Карточки той же формы,
// что у сервера (`api/proposals.rs::proposal_json`, `works/tidy.rs::record_json`), подставляются
// ответами `GET /proposals?type=tidy`, `GET /proposals/{id}` и `GET /entries/{id}` (полные тела
// записей диффа); «Применить» — по желанию сценария (`accept`): тело запроса запоминается, ответ —
// `accepted`. Настоящий сервер «Применить», «Отклонить» и 409 у `stale` проверяет на кандидатах
// «возможно устарело» (`proposals-tidy.spec.ts`) и в `crates/zam-server/tests/proposals_tidy.rs`.

export const TIDY_MERGE = 'o:01KQ00000000000000000E2ET1';
export const TIDY_RETRACT = 'o:01KQ00000000000000000E2ET2';
export const TIDY_KEEP = 'i:01KQ00000000000000000E2EK1';
export const TIDY_DROP = 'i:01KQ00000000000000000E2ED1';
export const TIDY_OLD = 'i:01KQ00000000000000000E2ER1';
/** Новая запись `merge` после «Применить». */
export const TIDY_NEW = 'i:01KQ00000000000000000E2EN1';
const TIDY_SPACE = '01KQ00000000000000000E2ES1';
export const TIDY_KEEP_TITLE = 'Окно формы — поверх страницы';
export const TIDY_DROP_TITLE = 'Форма на телефоне — во весь экран';
/** Объединённый текст, предложенный исполнителем. */
export const TIDY_MERGE_TEXT =
  'Окно формы открывается поверх страницы, карточка остаётся на виду.\nНа телефоне окно — во весь экран, основное действие — в шапке.';
/** Полное тело `keep` (L1 в предложении — его начало). */
const KEEP_BODY = 'Окно формы открывается поверх страницы.\nНа телефоне — во весь экран.\nКаркас — FormDialog.';
const DROP_BODY = 'На телефоне окно формы — во весь экран, основное действие — в шапке.';

type TidyStatus = 'proposed' | 'accepted' | 'declined' | 'stale';

function tidyBase(id: string, status: TidyStatus) {
  const done = status !== 'proposed';
  return {
    id,
    type: 'tidy',
    status,
    space: TIDY_SPACE,
    path: '',
    content: '',
    basis: [],
    author_kind: 'model',
    author: 'model:qwen3-e2e',
    work: 'j:01KQ00000000000000000E2EW1',
    session: null,
    shell: false,
    created_at: '2026-11-27T01:40:00.000000Z',
    decided_at: done ? '2026-11-28T09:15:00.000000Z' : null,
    decided_by: done ? 'admin' : null,
    decision_note: null,
    result_ref: status === 'accepted' ? (id === TIDY_MERGE ? TIDY_NEW : TIDY_OLD) : null,
    stale: status === 'stale',
    sources: [],
  };
}

const record = (ref: string, rev: number, title: string, l1: string, certainty: string | null) => ({
  ref,
  rev,
  kind: 'insight',
  insight_kind: 'pattern',
  space: TIDY_SPACE,
  title,
  l1,
  status: 'active',
  certainty,
  human_decision: certainty === 'axiom',
});

/** Карточка `merge` почти-дублей (`keep` — theorem, `drop` — решение человека, axiom). */
export function tidyMergeCard(status: TidyStatus = 'proposed') {
  return {
    ...tidyBase(TIDY_MERGE, status),
    reason: 'Обе записи — об одном окне формы; объединённый текст короче и не теряет правила телефона.',
    targets: [
      { target: TIDY_KEEP, rev: 3 },
      { target: TIDY_DROP, rev: 1 },
    ],
    candidate: 'near_duplicate',
    signals: [{ name: 'cosine', value: '0.86' }],
    records: [
      record(TIDY_KEEP, 3, TIDY_KEEP_TITLE, 'Окно формы открывается поверх страницы.\nНа телефоне — во весь экран.', 'theorem'),
      record(TIDY_DROP, 1, TIDY_DROP_TITLE, DROP_BODY, 'axiom'),
    ],
    ops: ['merge', 'retract', 'keep'],
    op: { op: 'merge', keep: TIDY_KEEP, drop: TIDY_DROP, text: TIDY_MERGE_TEXT },
  };
}

/** Карточка «возможно устарело» с операцией `retract` (окно-подтверждение `danger`). */
export function tidyRetractCard(status: TidyStatus = 'proposed') {
  return {
    ...tidyBase(TIDY_RETRACT, status),
    reason: 'Файл, на который ссылается запись, переписан: путь сборки WebUI давно другой.',
    targets: [{ target: TIDY_OLD, rev: 2 }],
    candidate: 'stale',
    signals: [{ name: 'stale_refs', value: '1' }],
    records: [record(TIDY_OLD, 2, 'Сборка WebUI — старый путь', 'Сборка WebUI лежит в web/build.', null)],
    ops: ['retract', 'revisit', 'append', 'keep'],
    op: { op: 'retract', target: TIDY_OLD },
  };
}

/**
 * Подстановка предложений уборки (см. выше): список и карточки `merge` и `retract`, тела записей
 * диффа; `counts` — ответ `GET /proposals/counts`; `accept` — «Применить» `merge` отвечает
 * `accepted` (тела запросов — в `accepts`), иначе запрос уходит дальше (`route.fallback`, у строк
 * форм его ловит подстановка 422).
 */
export async function routeTidy(page: Page, opts: { counts?: { rules: number; tidy: number }; accept?: boolean } = {}): Promise<{ accepts: unknown[] }> {
  const st = { merge: 'proposed' as TidyStatus, accepts: [] as unknown[] };
  const cards = () => [tidyMergeCard(st.merge), tidyRetractCard()];
  await page.route(/\/api\/v1\/proposals(\?.*)?$/, (r) => {
    const u = new URL(r.request().url());
    if (r.request().method() !== 'GET' || u.searchParams.get('type') !== 'tidy') return r.fallback();
    const status = u.searchParams.get('status');
    const list = cards()
      .filter((c) => !status || c.status === status)
      .map(({ sources: _s, ...c }) => c);
    return r.fulfill({ json: { proposals: list } });
  });
  await page.route(/\/api\/v1\/proposals\/(?:o%3A|o:)?01KQ00000000000000000E2ET([12])(\/accept)?$/, (r) => {
    const m = /E2ET([12])(\/accept)?$/.exec(new URL(r.request().url()).pathname)!;
    const merge = m[1] === '1';
    if (!m[2] && r.request().method() === 'GET') return r.fulfill({ json: merge ? tidyMergeCard(st.merge) : tidyRetractCard() });
    if (m[2] && merge && opts.accept && r.request().method() === 'POST') {
      st.accepts.push(r.request().postDataJSON());
      st.merge = 'accepted';
      const { sources: _s, ...p } = tidyMergeCard('accepted');
      return r.fulfill({ json: { proposal: p, result: TIDY_NEW } });
    }
    return r.fallback();
  });
  await page.route(/\/api\/v1\/entries\/01KQ00000000000000000E2E(K1|D1)$/, (r) => {
    if (r.request().method() !== 'GET') return r.fallback();
    const keep = r.request().url().endsWith('K1');
    const id = (keep ? TIDY_KEEP : TIDY_DROP).slice(2);
    return r.fulfill({
      json: {
        id,
        kind: 'insight',
        space_id: TIDY_SPACE,
        rev: keep ? 3 : 1,
        title: keep ? TIDY_KEEP_TITLE : TIDY_DROP_TITLE,
        body: keep ? KEEP_BODY : DROP_BODY,
        data: { kind: 'pattern' },
        tags: [],
        status: 'active',
        certainty: keep ? 'theorem' : 'axiom',
      },
    });
  });
  if (opts.counts) {
    const c = opts.counts;
    await page.route(/\/api\/v1\/proposals\/counts$/, (r) => (r.request().method() === 'GET' ? r.fulfill({ json: c }) : r.fallback()));
  }
  return st;
}

/** Записи Vault и кандидат; `mock` — базовый адрес подставной модели (`http://127.0.0.1:<порт>`). */
export async function seedInternal(base: string, token: string, mock: string): Promise<InternalSeed> {
  const vault = (title: string, url: string) =>
    call<{ id: string }>(base, token, 'POST', 'spaces/knowledge/vault', {
      title,
      data: { type: 'token', url, description: 'e2e: подключённая модель' },
      secret: MOCK_KEY,
    });
  const lan = await vault('E2E модель (LAN)', `${mock}/v1`);
  const ext = await vault('E2E модель (вне LAN)', 'https://203.0.113.10/v1');
  const move = await vault('E2E модель (смена адреса)', `${mock}/v1`);
  const p = await call<{ proposal: string }>(base, token, 'POST', 'proposals', {
    type: 'rule',
    path: 'e2e/кандидат-форм',
    content: 'Перед правкой окна формы проверяй его на телефоне: шапка, основное действие и «Назад».',
    reason: 'Окна форм открываются на всех устройствах — правило касается любого проекта с WebUI.',
  });
  return { vaultLan: lan.id, vaultExt: ext.id, vaultMove: move.id, proposalForms: p.proposal };
}
