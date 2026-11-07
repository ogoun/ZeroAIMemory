// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Время в WebUI (M6 §4.4): API отдаёт RFC 3339 UTC, в локальное время переводит только браузер.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { localTime } from '../src/lib/time.ts';

test('localTime переводит UTC в часовой пояс браузера', () => {
  const iso = '2026-10-10T14:03:22.123456Z';
  const want = new Date(iso).toLocaleString('ru-RU', { dateStyle: 'short', timeStyle: 'short' });
  assert.equal(localTime(iso), want);
  // явный пояс: тот же момент в UTC и в Москве (+03:00)
  assert.equal(localTime(iso, { timeZone: 'UTC', timeStyle: 'medium' }), '14:03:22');
  assert.equal(localTime(iso, { timeZone: 'Europe/Moscow', timeStyle: 'medium' }), '17:03:22');
  // смещение во входе учитывается
  assert.equal(localTime('2026-10-10T17:03:22+03:00', { timeZone: 'UTC', timeStyle: 'medium' }), '14:03:22');
});

test('localTime: стиль по умолчанию и свой', () => {
  const iso = '2026-10-10T14:03:22Z';
  assert.equal(
    localTime(iso, { timeZone: 'UTC' }),
    new Date(iso).toLocaleString('ru-RU', { timeZone: 'UTC', dateStyle: 'short', timeStyle: 'short' }),
  );
  assert.equal(localTime(iso, { timeZone: 'UTC', dateStyle: 'medium' }).includes('2026'), true);
});

test('localTime: поля даты и времени — без стиля по умолчанию (страница «Индекс»)', () => {
  const iso = '2026-10-10T14:03:22Z';
  const o: Intl.DateTimeFormatOptions = {
    timeZone: 'UTC', year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric',
  };
  assert.equal(localTime(iso, o), new Date(iso).toLocaleString('ru-RU', o));
});

test('localTime: не время — как есть', () => {
  assert.equal(localTime('мусор'), 'мусор');
  assert.equal(localTime(''), '');
});
