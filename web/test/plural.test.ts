// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Склонение при числе (M7.2, спецификация §5.2 п. 6): имя вкладки доски «Выполнен, 60 задач».
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { RECORD_FORMS, TASK_FORMS, countText, plural } from '../src/lib/plural.ts';

test('plural_ru', () => {
  const tasks = [1, 2, 5, 11, 21, 122].map((n) => countText(n, TASK_FORMS));
  assert.deepEqual(tasks, ['1 задача', '2 задачи', '5 задач', '11 задач', '21 задача', '122 задачи']);
  const records = [1, 2, 5, 11, 21, 122].map((n) => countText(n, RECORD_FORMS));
  assert.deepEqual(records, ['1 запись', '2 записи', '5 записей', '11 записей', '21 запись', '122 записи']);
  // 0, 12–14 и 111–114 — «много»; 101, 1001 — «одна»; 104 — «несколько»
  assert.deepEqual(
    [0, 12, 13, 14, 111, 112, 114, 101, 1001, 104].map((n) => plural(n, 'один', 'несколько', 'много')),
    ['много', 'много', 'много', 'много', 'много', 'много', 'много', 'один', 'один', 'несколько'],
  );
  // знак и дробная часть не меняют форму
  assert.equal(plural(-21, 'задача', 'задачи', 'задач'), 'задача');
  assert.equal(plural(2.5, 'задача', 'задачи', 'задач'), 'задачи');
});

test('plural_matches_code_files_text', () => {
  // прежние подписи страницы «Код» (`filesText`, `symbolsText`) — то же правило
  assert.deepEqual(
    [1, 3, 8, 40].map((n) => `${n} ${plural(n, 'файл', 'файла', 'файлов')}`),
    ['1 файл', '3 файла', '8 файлов', '40 файлов'],
  );
});
