// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Копии сервера в WebUI (M7.2, спецификация M7 §5.6): плашка `no_recent_backup`, расписание с
// местным временем, строка итога копии, давность — общее правило `ageText`.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { bannerText, bytesText, msText, reportLine, scheduleText, sinceText } from '../src/lib/backups.ts';
import { ageText } from '../src/lib/time.ts';

const DAY = 86_400_000;

test('backup_banner_text', () => {
  const now = Date.parse('2026-11-23T12:00:00Z');
  // копии нет — «Копий нет»; свежая (без предупреждения) — плашки нет
  assert.equal(bannerText({ last_at: null, schedule: 'off', warning: 'no_recent_backup' }, now), 'Копий нет');
  assert.equal(bannerText({ last_at: '2026-11-23T03:30:00Z', schedule: 'daily', warning: null }, now), null);
  assert.equal(bannerText(null, now), null);
  // старая — «Последняя копия N дн. назад»
  const old = new Date(now - 9 * DAY - 3600_000).toISOString();
  assert.equal(bannerText({ last_at: old, schedule: 'daily', warning: 'no_recent_backup' }, now), 'Последняя копия 9 дн. назад');
  assert.equal(sinceText(new Date(now - 90_000).toISOString(), now), '1 мин назад');
});

test('backup_schedule_local_time', () => {
  const now = Date.parse('2026-11-23T12:00:00Z');
  assert.equal(scheduleText('off', '03:30', now), 'выключено');
  assert.equal(
    scheduleText('daily', '03:30', now, 'Europe/Moscow'),
    'ежедневно в 03:30 UTC (06:30 по вашему времени)',
  );
  assert.equal(scheduleText('daily', '3:05', now, 'UTC'), 'ежедневно в 03:05 UTC (03:05 по вашему времени)');
  // пояс со сменой даты: 23:30 UTC — 08:30 следующего дня в Токио
  assert.equal(scheduleText('daily', '23:30', now, 'Asia/Tokyo'), 'ежедневно в 23:30 UTC (08:30 по вашему времени)');
});

test('backup_report_line', () => {
  const r = { name: '2026-11-23_101500', dbs: 2, blobs: 10, linked: 8, copied: 2, bytes: 1_258_291, ms: 3400, drift: [] };
  assert.equal(reportLine(r), 'копия 2026-11-23_101500: баз 2, блобов 10 (связано 8, скопировано 2), 1,2 МБ, 3,4 с');
  assert.equal(
    reportLine({ ...r, drift: ['пользователь X: базы ещё нет — пропуск'] }),
    'копия 2026-11-23_101500: баз 2, блобов 10 (связано 8, скопировано 2), 1,2 МБ, 3,4 с; ' +
      'расхождения снимков: пользователь X: базы ещё нет — пропуск',
  );
  assert.deepEqual([800, 12_800, 3 * 1024 ** 3].map(bytesText), ['800 Б', '12,5 КБ', '3 ГБ']);
  assert.deepEqual([850, 125_000].map(msText), ['850 мс', '2 мин 5 с']);
});

test('age_text_shared', () => {
  // одно правило давности: снимок карты кода (`code.ts` реэкспортирует) и последняя копия
  assert.deepEqual(
    [30, 120, 7200, 172_799, 172_800, 9 * 86_400].map(ageText),
    ['только что', '2 мин назад', '2 ч назад', '47 ч назад', '2 дн. назад', '9 дн. назад'],
  );
});
