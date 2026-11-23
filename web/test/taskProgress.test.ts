// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Подзадачи в WebUI (M7.2, спецификация §5.5 пп. 1–3): прогресс общей задачи «8/13» по правилу
// `done/(total − dropped)` (§5.4 п. 2, решение 21) и текст плашки «давно без обновлений».
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { idleText, progressText, progressValue, staleText } from '../src/lib/taskProgress.ts';

const H = 3_600_000;

test('progress_and_stale_text', () => {
  // 13 подзадач, 8 выполнено, снятых нет — «8/13»
  assert.equal(progressText({ total: 13, done: 8, dropped: 0 }), '8/13');
  assert.deepEqual(progressValue({ total: 13, done: 8, dropped: 0 }), { now: 8, max: 13 });
  // снятые в знаменатель не входят: 13 подзадач, 8 выполнено, 1 снята — «8/12»
  assert.equal(progressText({ total: 13, done: 8, dropped: 1 }), '8/12');
  assert.deepEqual(progressValue({ total: 13, done: 8, dropped: 1 }), { now: 8, max: 12 });
  // сводка подзадачи целиком (`subtasks` сервера) — те же поля
  assert.equal(progressText({ total: 6, done: 3, dropped: 0, open: 1, in_progress: 2, blocked: 0 }), '3/6');
  // все выполнены
  assert.equal(progressText({ total: 5, done: 5, dropped: 0 }), '5/5');
  assert.equal(progressText({ total: 5, done: 0, dropped: 0 }), '0/5');
  // подзадач нет — пусто
  assert.equal(progressText({ total: 0, done: 0, dropped: 0 }), '');
  assert.equal(progressText(undefined), '');
  assert.equal(progressText(null), '');
  assert.deepEqual(progressValue(undefined), { now: 0, max: 0 });
  // все сняты — счётчик не показывается
  assert.equal(progressText({ total: 3, done: 0, dropped: 3 }), '');
  assert.deepEqual(progressValue({ total: 3, done: 0, dropped: 3 }), { now: 0, max: 0 });

  // «без обновлений»: часы от `last_activity_at` — целые, вниз; от суток — «N сут. M ч»
  const now = Date.parse('2026-11-23T12:00:00Z');
  const ago = (ms: number) => new Date(now - ms).toISOString();
  assert.equal(staleText(ago(14 * H), now), 'в работе, без обновлений 14 ч');
  assert.equal(staleText(ago(25 * H), now), 'в работе, без обновлений 1 сут. 1 ч');
  assert.equal(staleText(ago(14 * H + 59 * 60_000), now), 'в работе, без обновлений 14 ч');
  assert.equal(staleText(ago(48 * H), now), 'в работе, без обновлений 2 сут.');
  assert.equal(staleText(ago(24 * H * 10 + 23 * H), now), 'в работе, без обновлений 10 сут. 23 ч');
  // момент сервера без долей секунды — тот же разбор
  assert.equal(staleText('2026-11-22T22:00:00Z', now), 'в работе, без обновлений 14 ч');
  assert.equal(idleText(14 * H), '14 ч');
  assert.equal(idleText(25 * H), '1 сут. 1 ч');
  // меньше часа (часы браузера отстают от сервера) — «меньше 1 ч»; не время — без числа
  assert.equal(staleText(ago(10 * 60_000), now), 'в работе, без обновлений меньше 1 ч');
  assert.equal(staleText(new Date(now + H).toISOString(), now), 'в работе, без обновлений меньше 1 ч');
  assert.equal(staleText('не время', now), 'в работе, без обновлений');
});
