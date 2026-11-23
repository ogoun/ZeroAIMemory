// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Правка записей журналов (M7.2, спецификация §5.9 п. 1): `PATCH` только из изменённых полей формы
// и перенос черновика на перечитанную запись после 409.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Entry } from '../src/lib/api.ts';
import { fieldsOf, isDirty, patchOf, rebase, tagList } from '../src/lib/entryEdit.ts';

/** Запись вида `kind` с полями по умолчанию. */
function entry(kind: Entry['kind'], over: Partial<Entry> = {}): Entry {
  return {
    id: '01M7GAQQKQEK1RC1QQ0CM01N7B',
    space_id: 's',
    kind,
    title: 'Заголовок',
    body: 'Текст.',
    data: {},
    tags: ['web'],
    area: 'ui',
    status: null,
    certainty: null,
    supersedes_id: null,
    superseded_by_id: null,
    retract_reason: null,
    rev: 3,
    author: { kind: 'agent', user_id: null, token_id: null, session_id: null, machine: null },
    index_state: 'indexed',
    created_at: '2026-11-23T10:00:00Z',
    updated_at: '2026-11-23T10:00:00Z',
    deleted_at: null,
    ...over,
  };
}

test('entry_patch_only_changed_fields', () => {
  // ничего не меняли — правки нет; пустой заголовок — ошибка
  const cl = entry('changelog', { data: { kind: 'fix', refs: { commits: ['abc123'] } } });
  assert.deepEqual(patchOf(cl, fieldsOf(cl)), { none: true });
  assert.deepEqual(patchOf(cl, { ...fieldsOf(cl), title: '   ' }), { error: 'Укажите заголовок.' });
  // заголовок обрезается; текст — как есть; теги без пустых и повторов; пустая область — null
  const f = { ...fieldsOf(cl), title: '  Новый  ', body: 'Иначе.\n', tags: 'web, , api, web', area: ' ' };
  assert.deepEqual(patchOf(cl, f), { patch: { title: 'Новый', body: 'Иначе.\n', tags: ['web', 'api'], area: null } });
  // вид changelog — `data` целиком: `refs` сохраняются (сервер заменяет data changelog)
  assert.deepEqual(patchOf(cl, { ...fieldsOf(cl), kind: 'feat' }), {
    patch: { data: { kind: 'feat', refs: { commits: ['abc123'] } } },
  });
  // release: версия и коммиты не трогаются — `data` в правке нет
  const rel = entry('release', { data: { version: '1.0.0', commits: [{ sha: 'a' }] } });
  assert.deepEqual(patchOf(rel, { ...fieldsOf(rel), title: 'Релиз 1.0.0' }), { patch: { title: 'Релиз 1.0.0' } });

  // задача: только изменённые поля `data` (сервер сливает), пустое — `null` снимает
  const t = entry('task', {
    data: { next_step: 'шаг', priority: 'high', owner: 'agent', estimate_min: 90, links: ['https://example.com/a'] },
  });
  assert.equal(fieldsOf(t).estimate, '90');
  assert.equal(fieldsOf(t).links, 'https://example.com/a');
  assert.deepEqual(patchOf(t, fieldsOf(t)), { none: true });
  assert.deepEqual(patchOf(t, { ...fieldsOf(t), priority: 'low', estimate: '120' }), {
    patch: { data: { priority: 'low', estimate_min: 120 } },
  });
  assert.deepEqual(patchOf(t, { ...fieldsOf(t), priority: '', owner: '', estimate: '', links: '' }), {
    patch: { data: { priority: null, owner: null, estimate_min: null, links: [] } },
  });
  assert.deepEqual(patchOf(t, { ...fieldsOf(t), links: ' https://example.com/a \n\nhttps://example.com/b\n' }), {
    patch: { data: { links: ['https://example.com/a', 'https://example.com/b'] } },
  });
  for (const bad of ['0', '1.5', '525601', 'час']) {
    assert.deepEqual(patchOf(t, { ...fieldsOf(t), estimate: bad }), { error: 'Оценка — целое число минут от 1 до 525600.' }, bad);
  }
  // у задачи `data.kind` нет — поле вида правку не даёт
  assert.deepEqual(patchOf(t, { ...fieldsOf(t), kind: 'feat' }), { none: true });
  assert.deepEqual(tagList(' a,b , a ,, c'), ['a', 'b', 'c']);
});

test('entry_edit_rebase_after_stale', () => {
  const base = fieldsOf(entry('task', { data: { priority: 'high' } }));
  // пользователь правил заголовок, в другом месте правили текст и приоритет
  const draft = { ...base, title: 'Мой заголовок' };
  const fresh = { ...base, body: 'Чужой текст.', priority: 'low' };
  assert.equal(isDirty(draft, base), true);
  assert.equal(isDirty(base, { ...base }), false);
  const r = rebase(draft, base, fresh);
  // нетронутые поля — из перечитанной записи (правка их не откатит), тронутое — пользователя
  assert.deepEqual(r.fields, { ...fresh, title: 'Мой заголовок' });
  assert.deepEqual(r.clash, []);
  // одно поле правили и он, и в другом месте по-разному — конфликт; одинаково — нет
  const both = rebase({ ...base, title: 'Мой' }, base, { ...base, title: 'Чужой' });
  assert.deepEqual(both.fields.title, 'Мой');
  assert.deepEqual(both.clash, ['title']);
  assert.deepEqual(rebase({ ...base, title: 'Тот же' }, base, { ...base, title: 'Тот же' }).clash, []);
});
