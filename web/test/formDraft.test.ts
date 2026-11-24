// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Черновик формы с ревизией (M7.3, спецификация §6.14 п. 5; задача k:01M83WA2A6EH0AP6FR7NPG9CJH):
// 409 — запись перечитана, нетронутое поле берётся из свежей записи, тронутое остаётся, поле,
// изменённое и здесь, и в другом месте по-разному, — в `clash`. Общее правило прежнего
// `entryEdit.ts:117` для любых полей формы (Insights, «Идеи», «Задачи», «Словарь»).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isDirty, rebase, rebaseText } from '../src/lib/formDraft.ts';
import { isDirty as entryDirty, rebase as entryRebase } from '../src/lib/entryEdit.ts';

test('rebase_untouched_from_fresh', () => {
  // форма «Статус надёжности»: человек набрал причину, в другом месте сменили статус и метод
  const base = { to: 'hypothesis', reason: '', method: 'тест' };
  const draft = { ...base, reason: 'проверено на стенде' };
  const fresh = { to: 'theorem', reason: '', method: 'замер' };
  const r = rebase(draft, base, fresh);
  assert.deepEqual(r.fields, { to: 'theorem', reason: 'проверено на стенде', method: 'замер' });
  assert.deepEqual(r.clash, []);
  // ничего не трогали — вся форма из свежей записи
  assert.deepEqual(rebase({ ...base }, base, fresh).fields, fresh);
  // исходные объекты не меняются
  assert.equal(draft.to, 'hypothesis');
});

test('rebase_clash_marked', () => {
  const base = { title: 'Было', body: 'Текст', tags: 'a' };
  // заголовок правили и здесь, и там по-разному — расхождение; одинаково — нет
  const r = rebase({ ...base, title: 'Мой' }, base, { ...base, title: 'Чужой', tags: 'b' });
  assert.deepEqual(r.fields, { title: 'Мой', body: 'Текст', tags: 'b' });
  assert.deepEqual(r.clash, ['title']);
  assert.deepEqual(rebase({ ...base, title: 'Тот же' }, base, { ...base, title: 'Тот же' }).clash, []);
  // поле тронуто здесь, а там не менялось — не расхождение
  assert.deepEqual(rebase({ ...base, body: 'Мой текст' }, base, { ...base }).clash, []);
});

test('rebase_structured_fields', () => {
  // списки (триггеры insight, варианты ADR) сравниваются по содержимому, а не по ссылке
  const base = { triggers: [{ type: 'keyword', pattern: 'x' }], pattern: '' };
  const fresh = { triggers: [{ type: 'keyword', pattern: 'y' }], pattern: '' };
  const untouched = rebase({ triggers: [{ type: 'keyword', pattern: 'x' }], pattern: 'набрано' }, base, fresh);
  assert.deepEqual(untouched.fields, { triggers: [{ type: 'keyword', pattern: 'y' }], pattern: 'набрано' });
  assert.deepEqual(untouched.clash, []);
  const both = rebase({ triggers: [], pattern: '' }, base, fresh);
  assert.deepEqual(both.fields.triggers, []);
  assert.deepEqual(both.clash, ['triggers']);
  assert.equal(isDirty({ triggers: [{ type: 'keyword', pattern: 'x' }], pattern: '' }, base), false);
  assert.equal(isDirty({ triggers: [], pattern: '' }, base), true);
});

test('form_draft_dirty', () => {
  const base = { a: '1', b: '' };
  assert.equal(isDirty({ ...base }, base), false);
  assert.equal(isDirty({ ...base, b: ' ' }, base), true);
  // `entryEdit.ts` — тот же модуль (реэкспорт): прежняя форма правки записей живёт на нём
  assert.equal(entryDirty, isDirty);
  assert.equal(entryRebase, rebase);
});

test('rebase_text_one_for_all_forms', () => {
  // один текст 409 у всех форм с ревизией; расхождения названы, ревизия — если известна
  assert.equal(rebaseText([]), 'Запись изменилась, пока вы правили: проверьте и сохраните снова.');
  assert.equal(
    rebaseText(['заголовок', 'теги'], 7),
    'Запись изменилась, пока вы правили (сейчас ревизия 7): проверьте и сохраните снова. Изменено и здесь, и в другом месте: заголовок, теги — сверьте перед сохранением.',
  );
});
