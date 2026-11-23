// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Числа разделов (M7.2, спецификация §5.3): описания для чтения с экрана и формат числа, правило
// устаревания после изменяющих запросов (`changesCounts`, `api.ts`).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  changesCounts,
  countDescription,
  countLabel,
  formatCount,
  notesLabel,
  openLabel,
} from '../src/lib/counts.ts';

test('count_label_ru', () => {
  const ideas = [1, 2, 5, 11, 21, 122].map((n) => countLabel('ideas', n));
  assert.deepEqual(ideas, ['1 запись', '2 записи', '5 записей', '11 записей', '21 запись', '122 записи']);
  // разделитель разрядов ru-RU — неразрывный пробел
  assert.equal(formatCount(1234), '1 234');
  assert.equal(countLabel('tasks', 1234), '1 234 записи');
  assert.equal(countLabel('vault', 0), '0 записей');
  // «Код» — файлы карты кода, пространство — заметки
  assert.deepEqual([1, 3, 5].map((n) => countLabel('code', n)), ['1 файл', '3 файла', '5 файлов']);
  assert.deepEqual([1, 4, 11].map(notesLabel), ['1 заметка', '4 заметки', '11 заметок']);
  // «Вопросы» с открытыми — «открыто N» в описании; без открытых — только число
  assert.equal(openLabel(2), 'открыто 2');
  assert.equal(countDescription('questions', 3, 1), '3 записи, открыто 1');
  assert.equal(countDescription('questions', 3, 0), '3 записи');
  assert.equal(countDescription('ideas', 122), '122 записи');
});

test('changes_counts_rule', () => {
  const yes: [string, string][] = [
    ['POST', 'spaces/knowledge/entries'],
    ['POST', 'spaces/0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e12/vault'],
    ['PATCH', 'entries/01ABC'],
    ['DELETE', 'entries/01ABC'],
    ['POST', 'entries/01ABC/restore'],
    ['POST', 'entries/01ABC/supersede'],
    ['POST', 'entries/01ABC/retract'],
    ['POST', 'entries/01ABC/answer'],
    ['POST', 'entries/01ABC/decide'],
    ['POST', 'entries/01ABC/adopt'],
    ['POST', 'entries/01ABC/review'],
    ['POST', 'nodes'],
    ['DELETE', 'nodes/01ABC'],
    ['POST', 'nodes/01ABC/restore'],
    ['DELETE', 'vault/01ABC'],
    ['POST', 'vault/01ABC/move'],
    ['POST', 'cache/01ABC/promote'],
  ];
  for (const [m, p] of yes) assert.equal(changesCounts(m, p), true, `${m} ${p}`);
  const no: [string, string][] = [
    ['GET', 'spaces/knowledge/entries?kind=idea'],
    ['GET', 'spaces/knowledge/counts'],
    ['PUT', 'nodes/01ABC'],
    ['PATCH', 'nodes/01ABC'],
    ['POST', 'nodes/01ABC/move'],
    ['DELETE', 'nodes/01ABC/attachments/ab/x.png'],
    ['POST', 'entries/01ABC/feedback'],
    ['POST', 'entries/01ABC/release'],
    ['PATCH', 'vault/01ABC'],
    ['POST', 'blobs'],
    ['POST', 'search'],
    ['POST', 'render'],
  ];
  for (const [m, p] of no) assert.equal(changesCounts(m, p), false, `${m} ${p}`);
});
