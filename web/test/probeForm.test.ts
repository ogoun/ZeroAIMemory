// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Окна набора проб «Проверки памяти» (M7.4, спецификация §7.7 п. 1): цели-ссылки формы
// `<буква>:<id>`, вопрос и причина снятия — в пределах сервера.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseProbeTargets, questionError, reasonError } from '../src/lib/probeForm.ts';

const id = (n: number) => `01KQ0000000000000000000E${String(n).padStart(2, '0')}`;

test('probe_targets_ref_form', () => {
  // пробел, запятая, перевод строки; повтор — один раз
  assert.deepEqual(parseProbeTargets(` i:${id(1)}, n:${id(2)}\nt:${id(3)} i:${id(1)} `), { refs: [`i:${id(1)}`, `n:${id(2)}`, `t:${id(3)}`] });
  // не та форма: без буквы, заглавная буква, короткий id
  const bad = parseProbeTargets(`${id(1)} I:${id(2)} i:123`);
  assert.ok('error' in bad);
  assert.match(bad.error, new RegExp(`^«${id(1)}», «I:${id(2)}», «i:123» — не ссылки вида <буква>:<id>`));
  assert.deepEqual(parseProbeTargets('  '), { error: 'Нужна хотя бы одна цель — запись, которую вопрос должен находить.' });
  const six = [1, 2, 3, 4, 5, 6].map((n) => `i:${id(n)}`).join(' ');
  assert.deepEqual(parseProbeTargets(six), { error: 'Целей — не больше 5: оставьте главные записи.' });
});

test('probe_question_and_reason_limits', () => {
  assert.equal(questionError('  '), 'Введите вопрос — так, как его задал бы агент.');
  assert.equal(questionError('Как устроен сборщик мусора блобов?'), '');
  assert.equal(questionError('я'.repeat(513)), 'Вопрос — не длиннее 512 символов (сейчас 513).');
  assert.equal(reasonError(' аб '), 'Причина — от 3 символов: что в пробе неверно.');
  assert.equal(reasonError('цель замещена'), '');
  assert.equal(reasonError('я'.repeat(501)), 'Причина — не длиннее 500 символов (сейчас 501).');
});
