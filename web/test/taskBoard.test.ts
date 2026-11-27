// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Вкладка доски задач по умолчанию (M7.2, спецификация §5.2 п. 1, решение 18).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { TASK_COLUMNS, boardSummary, boardTab, listStepsText, stepCounts, stepsHint, stepsName, stepsTag } from '../src/lib/taskBoard.ts';

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

// Шаги планов в числах доски (M7.3, §6.14 п. 10, задача 10c): числа рабочей памяти владельца —
// вкладка «Задачи» 109, колонки 33 + 1 + 0 + 2 + 1 = 37, шаги 18 + 2 + 0 + 52 + 0 = 72.
const TOP = { open: 33, in_progress: 1, blocked: 0, done: 2, dropped: 1 };
const ALL = { open: 51, in_progress: 3, blocked: 0, done: 54, dropped: 1 };

test('board_steps_counts', () => {
  const c = stepCounts(TOP, ALL);
  assert.ok(c);
  assert.equal(c.tasks, 37);
  assert.equal(c.steps, 72);
  assert.deepEqual(c.byStatus, { open: 18, in_progress: 2, blocked: 0, done: 52, dropped: 0 });
  // сумма колонок верхнего уровня и шагов — число вкладки «Задачи»
  assert.equal(c.tasks + c.steps, 109);
  // одного из чисел нет — нечего показывать
  assert.equal(stepCounts(null, ALL), null);
  assert.equal(stepCounts(TOP, null), null);
  // два запроса — два снимка: разность не уходит в минус
  assert.deepEqual(stepCounts({ open: 3 }, { open: 2 })?.byStatus.open, 0);
  // нет статуса в ответе — 0
  assert.deepEqual(stepCounts({}, { done: 4 })?.byStatus, { open: 0, in_progress: 0, blocked: 0, done: 4, dropped: 0 });
});

test('board_steps_summary', () => {
  const c = stepCounts(TOP, ALL);
  // тире перед кнопкой ставит разметка (скрытым от чтения с экрана): текст — описание кнопки
  assert.deepEqual(boardSummary(c, 'folded'), { lead: '37 задач · 72 шага планов внутри них', action: 'показать на доске' });
  assert.deepEqual(boardSummary(c, 'shown'), { lead: '37 задач · показаны и 72 шага планов', action: 'свернуть' });
  // одна задача — «внутри неё»; 21 задача — «внутри них»; склонение шагов
  assert.equal(boardSummary(stepCounts({ open: 1 }, { open: 2 }), 'folded')?.lead, '1 задача · 1 шаг плана внутри неё');
  assert.equal(boardSummary(stepCounts({ open: 21 }, { open: 24 }), 'folded')?.lead, '21 задача · 3 шага планов внутри них');
  // причастие — в согласии с числом шагов: 1 и 21 — «показан», 2–4 и 5+ — «показаны» (ревью r1, Minor 1)
  assert.equal(boardSummary(stepCounts({ open: 5 }, { open: 6 }), 'shown')?.lead, '5 задач · показан и 1 шаг плана');
  assert.equal(boardSummary(stepCounts({ open: 5 }, { open: 26 }), 'shown')?.lead, '5 задач · показан и 21 шаг плана');
  assert.equal(boardSummary(stepCounts({ open: 5 }, { open: 7 }), 'shown')?.lead, '5 задач · показаны и 2 шага планов');
  assert.equal(boardSummary(stepCounts({ open: 5 }, { open: 16 }), 'shown')?.lead, '5 задач · показаны и 11 шагов планов');
  // шагов нет — строки нет (числа и так сходятся)
  const none = stepCounts({ open: 5, done: 2 }, { open: 5, done: 2 });
  assert.equal(boardSummary(none, 'folded'), null);
  assert.equal(boardSummary(none, 'shown'), null);
  assert.equal(boardSummary(null, 'folded'), null);
  // «Из идей» — строка поясняет, что показано, без кнопки (переключатель неактивен)
  assert.deepEqual(boardSummary(stepCounts({ open: 7 }, { open: 9, done: 3 }), 'from_idea'), {
    lead: 'Из идей — 7 задач и 5 шагов планов',
    action: '',
  });
  assert.deepEqual(boardSummary(none, 'from_idea'), { lead: 'Из идей — 7 задач', action: '' });
});

test('board_steps_tag', () => {
  assert.deepEqual([0, 1, 2, 18, 52].map(stepsTag), ['', '+1 шаг', '+2 шага', '+18 шагов', '+52 шага']);
  assert.deepEqual([0, 1, 18].map(stepsName), ['', ' и 1 шаг плана', ' и 18 шагов планов']);
  const tail = '«Подзадачи на доске» — показать';
  assert.equal(stepsHint(18, 'открыт'), `Ещё 18 шагов планов со статусом «открыт» — внутри общих задач, на доске свёрнуты; ${tail}`);
  // 1 и 21 — «свёрнут», 3 — «свёрнуты» (ревью r1, Minor 1)
  assert.equal(stepsHint(1, 'снят'), `Ещё 1 шаг плана со статусом «снят» — внутри общих задач, на доске свёрнут; ${tail}`);
  assert.equal(stepsHint(21, 'открыт'), `Ещё 21 шаг плана со статусом «открыт» — внутри общих задач, на доске свёрнут; ${tail}`);
  assert.equal(stepsHint(3, 'выполнен'), `Ещё 3 шага планов со статусом «выполнен» — внутри общих задач, на доске свёрнуты; ${tail}`);
});

test('board_list_steps_text', () => {
  // свёрнутые шаги статуса в списке колонки или вкладки (ревью r1, Minor 2–3)
  assert.equal(listStepsText(2, true), 'Задач нет — 2 шага планов этого статуса внутри общих задач');
  assert.equal(listStepsText(1, true), 'Задач нет — 1 шаг плана этого статуса внутри общих задач');
  assert.equal(listStepsText(4, false), 'Ещё 4 шага планов этого статуса — внутри общих задач');
  assert.equal(listStepsText(0, true), '');
  assert.equal(listStepsText(0, false), '');
});