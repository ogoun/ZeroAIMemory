// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Вкладка доски задач по умолчанию (M7.2, спецификация §5.2 п. 1, решение 18).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { TASK_COLUMNS, boardTab } from '../src/lib/taskBoard.ts';

test('board_tab_default', () => {
  // по умолчанию — «в работе»; нет задач в работе — «открыт» (даже если и открытых нет)
  assert.equal(boardTab({ open: 3, in_progress: 2, blocked: 0, done: 60, dropped: 0 }), 'in_progress');
  assert.equal(boardTab({ open: 3, in_progress: 0, blocked: 1, done: 60, dropped: 4 }), 'open');
  assert.equal(boardTab({ open: 0, in_progress: 0, blocked: 0, done: 0, dropped: 0 }), 'open');
  assert.equal(boardTab({ open: 3 }), 'open');
  // числа не пришли — «в работе»
  assert.equal(boardTab(null), 'in_progress');
  // задача из адреса — вкладка её статуса, что бы ни показывали числа
  assert.equal(boardTab({ in_progress: 2 }, 'done'), 'done');
  assert.equal(boardTab(null, 'blocked'), 'blocked');
  // не статус задачи (идея по ошибочному адресу) — правило по числам
  assert.equal(boardTab({ in_progress: 0 }, 'accepted'), 'open');
  assert.deepEqual(TASK_COLUMNS, ['open', 'in_progress', 'blocked', 'done', 'dropped']);
});
