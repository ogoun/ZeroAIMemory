// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Стек проверок ухода (M7.3, спецификация §6.14 п. 4; задача k:01M83ZDF5WED9AH9WVFF1S234C): переход
// по адресу при любой «грязной» проверке спрашивает один раз — текстом верхней грязной; согласие
// отпускает все, отказ оставляет всё как было; `beforeunload` — если грязная хоть одна.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { LEAVE_EDIT, createGuards, type Guard } from '../src/lib/guards.ts';

/** Проверка с набранным: `release` отпускает набранное (форма закроется), повторный переход не
 * спрашивает. */
function form(question?: string): Guard & { typed: boolean; released: number } {
  const g = {
    typed: true,
    released: 0,
    question,
    dirty: () => g.typed,
    release: () => {
      g.released++;
      g.typed = false;
    },
  };
  return g;
}

test('guards_one_question_top_text', () => {
  const asked: string[] = [];
  const guards = createGuards((q) => (asked.push(q), false));
  const below = form('Вопрос нижней формы');
  const top = form('Вопрос верхней формы');
  guards.push(below);
  guards.push(top);
  // две грязные — один вопрос, текстом верхней; отказ — всё как было
  assert.equal(guards.allow(), false);
  assert.deepEqual(asked, ['Вопрос верхней формы']);
  assert.equal(below.released + top.released, 0);
  assert.equal(below.typed && top.typed, true);
  // верхняя чистая — вопрос нижней грязной; без своего текста — `LEAVE_EDIT`
  top.typed = false;
  assert.equal(guards.allow(), false);
  assert.deepEqual(asked, ['Вопрос верхней формы', 'Вопрос нижней формы']);
  const plain = createGuards((q) => (asked.push(q), false));
  plain.push(form());
  plain.allow();
  assert.equal(asked.at(-1), LEAVE_EDIT);
  // чистые проверки не спрашивают
  const clean = createGuards(() => assert.fail('вопрос без набранного'));
  clean.push({ dirty: () => false });
  assert.equal(clean.allow(), true);
  assert.equal(createGuards(() => false).allow(), true);
});

test('guards_release_all_on_confirm', () => {
  let asks = 0;
  const guards = createGuards(() => (asks++, true));
  const a = form();
  const b = form();
  // чистая проверка тоже отпускается: её форма закрывается вместе с прочими
  let cleanReleased = 0;
  guards.push(a);
  guards.push({ dirty: () => false, release: () => cleanReleased++ });
  guards.push(b);
  assert.equal(guards.allow(), true);
  assert.equal(asks, 1);
  assert.deepEqual([a.released, b.released, cleanReleased], [1, 1, 1]);
  // повторный переход — без вопроса: набранное отпущено
  assert.equal(guards.allow(), true);
  assert.equal(asks, 1);
  // следующий адрес передаётся проверке: редактор заметки свободен внутри своей заметки
  const nav = createGuards<string>(() => (asks++, false));
  nav.push({ dirty: (next) => next !== 'та же заметка' });
  assert.equal(nav.allow('та же заметка'), true);
  assert.equal(nav.allow('другая'), false);
  assert.equal(asks, 2);
});

test('guards_beforeunload_any_dirty', () => {
  const guards = createGuards(() => assert.fail('beforeunload не спрашивает своим вопросом'));
  assert.equal(guards.anyDirty(), false);
  const a = form();
  const b = form();
  a.typed = false;
  guards.push(a);
  assert.equal(guards.anyDirty(), false);
  guards.push(b);
  assert.equal(guards.anyDirty(), true);
  b.typed = false;
  a.typed = true;
  assert.equal(guards.anyDirty(), true);
});

test('guards_pop_removes_only_own', () => {
  const asked: string[] = [];
  const guards = createGuards((q) => (asked.push(q), false));
  const a = form('a');
  const b = form('b');
  const c = form('c');
  const offA = guards.push(a);
  const offB = guards.push(b);
  guards.push(c);
  // снятие средней не трогает соседей; повторное снятие — ничего
  offB();
  offB();
  guards.allow();
  assert.deepEqual(asked, ['c']);
  c.typed = false;
  guards.allow();
  assert.deepEqual(asked, ['c', 'a']);
  offA();
  assert.equal(guards.anyDirty(), false);
  // одна и та же проверка дважды — снимается своя запись
  const twice = createGuards(() => false);
  const g = form();
  const off1 = twice.push(g);
  twice.push(g);
  off1();
  assert.equal(twice.anyDirty(), true);
});
