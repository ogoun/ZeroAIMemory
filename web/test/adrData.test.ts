// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// `data` при замещении insight в WebUI (ревью задачи 13a M6, m1): у решения ключи ADR верхнего
// уровня прежней записи не копируются (сервер ответил бы 422), прочие поля — сохраняются.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DECISION_TOP_LEVEL, supersedeData } from '../src/lib/adrData.ts';

test('замещение gotcha с rationale/options на decision — ключи ADR сверху не копируются', () => {
  const old = { kind: 'gotcha', rationale: 'старое', options: ['x'], evidence: { checked: 'c' } };
  const d = supersedeData(old, 'decision', { chosen: 'A' });
  assert.deepEqual(d, { kind: 'decision', evidence: { checked: 'c' }, decision: { chosen: 'A' } });
  for (const k of DECISION_TOP_LEVEL) assert.ok(!(k in d), k);
  // прежняя запись не меняется
  assert.equal(old.rationale, 'старое');
});

test('старое решение с ключами сверху и пустой формой ADR — без ключей и без decision', () => {
  const old = { kind: 'decision', chosen: 'A', consequences: 'c', revisit_if: 'r', decision: { chosen: 'B' } };
  assert.deepEqual(supersedeData(old, 'decision', null), { kind: 'decision' });
});

test('не решение — данные прежней записи как есть, decision снимается', () => {
  const old = { kind: 'decision', rationale: 'x', decision: { chosen: 'A' } };
  assert.deepEqual(supersedeData(old, 'gotcha', null), { kind: 'gotcha', rationale: 'x' });
});
