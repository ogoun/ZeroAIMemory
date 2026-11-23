// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Числа разделов (M7.2, спецификация §5.3): описания для чтения с экрана и формат числа, правило
// устаревания после изменяющих запросов (`changesCounts`, `api.ts`).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  CountsCache,
  VISIBLE_REFRESH_MS,
  changesCounts,
  countDescription,
  countLabel,
  formatCount,
  notesLabel,
  openLabel,
  type CountsSlot,
} from '../src/lib/counts.ts';

test('count_label_ru', () => {
  const ideas = [1, 2, 5, 11, 21, 122].map((n) => countLabel('ideas', n));
  assert.deepEqual(ideas, ['1 запись', '2 записи', '5 записей', '11 записей', '21 запись', '122 записи']);
  // разделитель разрядов ru-RU — неразрывный пробел
  assert.equal(formatCount(1234), '1 234');
  assert.equal(countLabel('tasks', 1234), '1 234 записи');
  assert.equal(countLabel('vault', 0), '0 записей');
  // «Код» — файлы карты кода, пространство — заметки
  assert.deepEqual([1, 3, 5].map((n) => countLabel('code', n)), ['1 файл', '3 файла', '5 файлов']);
  assert.deepEqual([1, 4, 11].map(notesLabel), ['1 заметка', '4 заметки', '11 заметок']);
  // «Вопросы» с открытыми — «открыто N» в описании; без открытых — только число
  assert.equal(openLabel(2), 'открыто 2');
  assert.equal(countDescription('questions', 3, 1), '3 записи, открыто 1');
  assert.equal(countDescription('questions', 3, 0), '3 записи');
  assert.equal(countDescription('ideas', 122), '122 записи');
});

test('changes_counts_rule', () => {
  const yes: [string, string][] = [
    ['POST', 'spaces/knowledge/entries'],
    ['POST', 'spaces/0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12/vault'],
    ['PATCH', 'entries/01ABC'],
    ['DELETE', 'entries/01ABC'],
    ['POST', 'entries/01ABC/restore'],
    ['POST', 'entries/01ABC/supersede'],
    ['POST', 'entries/01ABC/retract'],
    ['POST', 'entries/01ABC/answer'],
    ['POST', 'entries/01ABC/decide'],
    ['POST', 'entries/01ABC/adopt'],
    ['POST', 'entries/01ABC/review'],
    ['POST', 'nodes'],
    ['DELETE', 'nodes/01ABC'],
    ['POST', 'nodes/01ABC/restore'],
    ['DELETE', 'vault/01ABC'],
    ['POST', 'vault/01ABC/move'],
    ['POST', 'cache/01ABC/promote'],
    // очистка карты кода — журнал «Код» пустеет (ревью r1, M1)
    ['DELETE', 'code?solution=0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12'],
  ];
  for (const [m, p] of yes) assert.equal(changesCounts(m, p), true, `${m} ${p}`);
  const no: [string, string][] = [
    ['GET', 'spaces/knowledge/entries?kind=idea'],
    ['GET', 'spaces/knowledge/counts'],
    ['GET', 'code/status?solution=0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12'],
    ['PUT', 'nodes/01ABC'],
    ['PATCH', 'nodes/01ABC'],
    ['POST', 'nodes/01ABC/move'],
    ['DELETE', 'nodes/01ABC/attachments/ab/x.png'],
    ['POST', 'entries/01ABC/feedback'],
    ['POST', 'entries/01ABC/release'],
    ['PATCH', 'vault/01ABC'],
    ['POST', 'blobs'],
    ['POST', 'search'],
    ['POST', 'render'],
  ];
  for (const [m, p] of no) assert.equal(changesCounts(m, p), false, `${m} ${p}`);
});

// ---- кэш `CountsCache` (ревью r1, M4): подставные хранилище, запрос, часы и отложенный вызов ----

/** Стенд кэша: `Map` слотов, запросы ждут `answer`, часы `clock.t`, отложенные вызовы — `flush`. */
function bench() {
  const store = new Map<string, CountsSlot<number>>();
  const calls: { key: string; resolve: (v: number) => void; reject: (e: Error) => void }[] = [];
  const deferred: (() => void)[] = [];
  const clock = { t: 1_000_000 };
  const cache = new CountsCache<number>({
    store,
    fetch: (user, space) =>
      new Promise<number>((resolve, reject) => calls.push({ key: `${user}|${space}`, resolve, reject })),
    now: () => clock.t,
    defer: (f) => deferred.push(f),
  });
  /** Выполнить отложенные загрузки. */
  const flush = () => deferred.splice(0).forEach((f) => f());
  /** Ответить на первый ждущий запрос ключа и дождаться записи слота. */
  const answer = async (key: string, v: number | Error) => {
    const i = calls.findIndex((c) => c.key === key);
    assert.ok(i >= 0, `нет запроса ${key}`);
    const [c] = calls.splice(i, 1);
    if (v instanceof Error) c.reject(v);
    else c.resolve(v);
    await new Promise((r) => setImmediate(r));
  };
  return { store, calls, cache, clock, flush, answer };
}

test('counts_cache_load_once_and_generation', async () => {
  const b = bench();
  // чтение без чисел — одна отложенная загрузка; повторное чтение, пока запрос в пути, — без второй
  assert.equal(b.cache.get('u1', 'knowledge'), null);
  b.flush();
  assert.equal(b.calls.length, 1);
  assert.equal(b.cache.get('u1', 'knowledge'), null);
  b.flush();
  assert.equal(b.calls.length, 1);
  await b.answer('u1|knowledge', 5);
  assert.equal(b.cache.get('u1', 'knowledge'), 5);
  b.flush();
  assert.equal(b.calls.length, 0, 'свежее число — без запроса');
  // `invalidate` во время запроса: пришедший ответ — устаревший, следующее чтение перечитывает
  b.cache.invalidate('u1', 'knowledge');
  assert.equal(b.cache.get('u1', 'knowledge'), 5, 'устаревшее число — на экране до нового');
  b.flush();
  b.cache.invalidate('u1');
  await b.answer('u1|knowledge', 6);
  assert.deepEqual(b.store.get('u1|knowledge')?.stale, true);
  assert.equal(b.cache.get('u1', 'knowledge'), 6);
  b.flush();
  await b.answer('u1|knowledge', 7);
  assert.deepEqual({ ...b.store.get('u1|knowledge'), at: 0 }, { value: 7, stale: false, at: 0 });
  // `load = false` — только загруженное, без запроса
  assert.equal(b.cache.get('u1', 'solution-x', false), null);
  b.flush();
  assert.equal(b.calls.length, 0);
});

test('counts_cache_error_without_retry', async () => {
  const b = bench();
  b.cache.get('u1', 'knowledge');
  b.flush();
  await b.answer('u1|knowledge', new Error('сбой'));
  // сбой — без чисел и без повторов на каждом чтении
  assert.equal(b.cache.get('u1', 'knowledge'), null);
  b.flush();
  assert.equal(b.calls.length, 0);
  // повтор — после устаревания; сбой при прежних числах оставляет их
  b.cache.invalidate('u1');
  b.cache.get('u1', 'knowledge');
  b.flush();
  await b.answer('u1|knowledge', 3);
  b.cache.invalidate('u1');
  b.cache.get('u1', 'knowledge');
  b.flush();
  await b.answer('u1|knowledge', new Error('сбой'));
  assert.equal(b.cache.get('u1', 'knowledge'), 3);
});

test('counts_cache_visible_not_more_often_than_30s', async () => {
  const b = bench();
  b.cache.get('u1', 'knowledge');
  b.flush();
  await b.answer('u1|knowledge', 1);
  // возврат на вкладку через 10 с — числа свежие, запроса нет
  b.clock.t += 10_000;
  b.cache.visible();
  b.cache.get('u1', 'knowledge');
  b.flush();
  assert.equal(b.calls.length, 0);
  // через 30 с после загрузки — перечитать
  b.clock.t += VISIBLE_REFRESH_MS - 10_000;
  b.cache.visible();
  b.cache.get('u1', 'knowledge');
  b.flush();
  assert.equal(b.calls.length, 1);
  await b.answer('u1|knowledge', 2);
  // сразу после перечитывания — снова не чаще раза в 30 с
  b.clock.t += 1_000;
  b.cache.visible();
  b.cache.get('u1', 'knowledge');
  b.flush();
  assert.equal(b.calls.length, 0);
  assert.equal(b.cache.get('u1', 'knowledge'), 2);
});

test('counts_cache_invalidate_scope', async () => {
  const b = bench();
  const G = '0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12';
  const keys = [`owner|${G}`, `member|${G}`, 'owner|knowledge', 'member|knowledge', `member|01OTHERSOLUTIONID000000000`];
  for (const k of keys) b.store.set(k, { value: 1, stale: false, at: b.clock.t });
  const stale = () => keys.filter((k) => b.store.get(k)?.stale);
  // запись под владельцем: все его пространства и решения у всех (у администратора одно решение
  // видно под владельцем и участником, ревью r1 M6); личные пространства участника — нет
  b.cache.invalidate('owner');
  assert.deepEqual(stale(), [`owner|${G}`, `member|${G}`, 'owner|knowledge', `member|01OTHERSOLUTIONID000000000`]);
  for (const k of keys) b.store.set(k, { value: 1, stale: false, at: b.clock.t });
  // раскрытие решения под участником — это решение у всех пользователей
  b.cache.invalidate('member', G);
  assert.deepEqual(stale(), [`owner|${G}`, `member|${G}`]);
  for (const k of keys) b.store.set(k, { value: 1, stale: false, at: b.clock.t });
  // личное пространство — только своё
  b.cache.invalidate('member', 'knowledge');
  assert.deepEqual(stale(), ['member|knowledge']);
  for (const k of keys) b.store.set(k, { value: 1, stale: false, at: b.clock.t });
  // запрос без пользователя — всё
  b.cache.invalidate();
  assert.deepEqual(stale(), keys);
});
