// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Разбор отказа API (M7.2, задача 8, ревью r1 m4): `Retry-After` — секунды и HTTP-дата (RFC 9110
// §10.2.3), тело problem+json (RFC 9457), не-JSON и пустое тело — общий помощник `request` и
// `importArchive` (`web/src/lib/apiError.ts`).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ApiError, apiError, retryAfterSecs } from '../src/lib/apiError.ts';

const NOW = Date.parse('2026-11-23T12:00:00Z');

test('retry_after_seconds_and_http_date', () => {
  assert.equal(retryAfterSecs('30', NOW), 30);
  assert.equal(retryAfterSecs(' 0 ', NOW), 0);
  // HTTP-дата (IMF-fixdate): секунды до неё, дробная — вверх
  assert.equal(retryAfterSecs('Mon, 23 Nov 2026 12:00:45 GMT', NOW), 45);
  assert.equal(retryAfterSecs(new Date(NOW + 1500).toUTCString(), NOW - 500), 2);
  // прошедшая дата — повторять сразу
  assert.equal(retryAfterSecs('Mon, 23 Nov 2026 11:59:00 GMT', NOW), 0);
  // нет заголовка или не разобрать — null
  assert.equal(retryAfterSecs(null, NOW), null);
  assert.equal(retryAfterSecs('', NOW), null);
  assert.equal(retryAfterSecs('скоро', NOW), null);
  assert.equal(retryAfterSecs('-5', NOW), null);
  assert.equal(retryAfterSecs('12.5', NOW), null);
  // только цифры — секунды, а не год
  assert.equal(retryAfterSecs('2026', NOW), 2026);
  // RFC 850 и asctime (без зоны — GMT) — тоже HTTP-даты
  assert.equal(retryAfterSecs('Monday, 23-Nov-26 12:00:10 GMT', NOW), 10);
  assert.equal(retryAfterSecs('Mon Nov 23 12:00:20 2026', NOW), 20);
});

test('api_error_problem_json', () => {
  const body = JSON.stringify({ type: '/problems/busy', title: 'Service Unavailable', status: 503, detail: 'занято' });
  const e = apiError(503, 'Service Unavailable', body, '30', NOW);
  assert.ok(e instanceof ApiError);
  assert.ok(e instanceof Error);
  assert.equal(e.status, 503);
  assert.equal(e.message, 'занято');
  assert.equal(e.problem?.type, '/problems/busy');
  assert.equal(e.retryAfter, 30);
  // HTTP-дата — тем же разбором
  assert.equal(apiError(503, '', body, 'Mon, 23 Nov 2026 12:01:00 GMT', NOW).retryAfter, 60);
});

test('api_error_without_detail_or_json', () => {
  // problem без detail — текст статуса, тело сохраняется
  const p = apiError(403, 'Forbidden', JSON.stringify({ title: 'Forbidden', status: 403 }), null, NOW);
  assert.equal(p.message, 'Forbidden');
  assert.deepEqual(p.problem, { title: 'Forbidden', status: 403 });
  assert.equal(p.retryAfter, null);
  // detail не строка — текст статуса
  assert.equal(apiError(422, 'Unprocessable', JSON.stringify({ detail: 42 }), null, NOW).message, 'Unprocessable');
  // не JSON (страница прокси), пустое тело, JSON не объект — без problem
  for (const body of ['<html>502 Bad Gateway</html>', '', 'null', '"текст"', '17']) {
    const e = apiError(502, 'Bad Gateway', body, null, NOW);
    assert.equal(e.message, 'Bad Gateway', body);
    assert.equal(e.problem, null, body);
  }
});
