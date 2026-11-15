// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Предупреждения ответа /search по-русски (`warningText`): словарь знает все строки сервера.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { warningText } from '../src/lib/indexing.ts';

test('warningText: предупреждения поиска переведены, неизвестное — как есть', () => {
  // строки сервера: `WARN_*` в crates/zam-server/src (api/search, index_state, rerank)
  const known = [
    'reranker not installed',
    'index rebuilding',
    'reranker failed',
    'solution access pending',
    'solution access rejected',
    'solution access revoked',
    'solution unavailable',
    'contradicts graph truncated',
    'contradicts graph unavailable',
  ];
  for (const w of known) {
    const t = warningText(w);
    assert.notEqual(t, w, `нет перевода: ${w}`);
    assert.match(t, /[а-яё]/i, w);
  }
  assert.equal(
    warningText('contradicts graph truncated'),
    'граф противоречий обрезан — исключения и порядок по противоречиям могли примениться не полностью',
  );
  assert.equal(
    warningText('contradicts graph unavailable'),
    'граф противоречий недоступен — порядок и исключения по противоречиям не применены',
  );
  assert.equal(warningText('что-то новое'), 'что-то новое');
});
