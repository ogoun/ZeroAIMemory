// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Кнопки и формы решения по идее (M5.7 §10; M6, задача 16): общий модуль журнала «Идеи» и
// вопроса-предложения в «Вопросах».
import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Entry, EntryStatus } from '../src/lib/api.ts';
import {
  IDEA_ACTIVE,
  IDEA_FORMS,
  IDEA_TABS,
  decisionNotice,
  decisionOf,
  ideaButtons,
  originOf,
  priorityOf,
  releasedOf,
  tagsOf,
} from '../src/lib/ideaButtons.ts';
import { DEFER_FORM, START_SUBMIT, deferDefault, deferError, deferMin, sharedWarning, utcDate } from '../src/lib/ideaForms.ts';

const acts = (status: EntryStatus | null, agent = false, released = false) =>
  ideaButtons(status, agent, released).map((b) => b.act);

const entry = (over: Record<string, unknown>): Entry =>
  ({ id: 'x', kind: 'idea', status: 'new', data: {}, tags: [], ...over }) as unknown as Entry;

test('ideaButtons: состав и порядок кнопок по состоянию идеи', () => {
  // идея человека: активные — начать, отложить, отказаться; править — только new и deferred
  assert.deepEqual(acts('new'), ['start', 'defer', 'decline', 'edit']);
  assert.deepEqual(acts('proposed'), ['start', 'defer', 'decline']);
  assert.deepEqual(acts('deferred'), ['start', 'defer', 'decline', 'reopen', 'edit']);
  assert.deepEqual(acts('declined'), ['reopen']);
  assert.deepEqual(acts('done'), []);
  assert.deepEqual(acts(null), []);
  // принятая: с живой задачей кнопок нет, со снятой — отказаться и вернуть
  assert.deepEqual(acts('accepted'), []);
  assert.deepEqual(acts('accepted', false, true), ['decline', 'reopen']);
  // «от агента»: «Начать» нет до «Принять как свою»
  assert.deepEqual(acts('new', true), ['adopt', 'defer', 'decline', 'edit']);
  assert.deepEqual(acts('deferred', true), ['adopt', 'defer', 'decline', 'reopen', 'edit']);
  assert.deepEqual(acts('proposed', true), ['defer', 'decline']);
});

test('ideaButtons: «начать» и «отложить» — ровно у активных статусов (IDEA_ACTIVE)', () => {
  assert.deepEqual(IDEA_ACTIVE, ['new', 'proposed', 'deferred']);
  const all: (EntryStatus | null)[] = ['new', 'proposed', 'deferred', 'accepted', 'declined', 'done', 'open', null];
  for (const s of all) {
    const active = s !== null && IDEA_ACTIVE.includes(s);
    assert.equal(acts(s).includes('start'), active, `start у ${s}`);
    assert.equal(acts(s).includes('defer'), active, `defer у ${s}`);
  }
  assert.deepEqual(
    IDEA_TABS.map((t) => t.id),
    ['human', 'agent', 'all'],
  );
});

test('ideaButtons: подписи и вид кнопок', () => {
  const by = Object.fromEntries(ideaButtons('deferred', false, false).map((b) => [b.act, b]));
  assert.deepEqual(by.start, { act: 'start', title: 'Начать', cls: 'primary' });
  assert.deepEqual(by.defer, { act: 'defer', title: 'Отложить', cls: '' });
  assert.deepEqual(by.decline, { act: 'decline', title: 'Отказаться', cls: 'danger' });
  assert.deepEqual(by.reopen, { act: 'reopen', title: 'Вернуть', cls: '' });
  assert.deepEqual(by.edit, { act: 'edit', title: 'Изменить', cls: '' });
  assert.deepEqual(ideaButtons('new', true, false)[0], { act: 'adopt', title: 'Принять как свою', cls: 'primary' });
});

test('формы решения: подписи для вёрстки и e2e', () => {
  assert.deepEqual(IDEA_FORMS.start, { aria: 'Начать идею', title: 'Начать: где поставить задачу', submit: 'Начать' });
  assert.deepEqual(IDEA_FORMS.defer, { aria: 'Отложить идею', title: 'Отложить', submit: 'Отложить' });
  assert.deepEqual(IDEA_FORMS.decline, { aria: 'Отказаться от идеи', title: 'Отказаться', submit: 'Отказаться' });
  assert.deepEqual(IDEA_FORMS.reopen, { aria: 'Вернуть идею', title: 'Вернуть в новые', submit: 'Вернуть' });
  assert.deepEqual(IDEA_FORMS.edit, { aria: 'Правка идеи', title: 'Изменить идею', submit: 'Сохранить' });
  assert.deepEqual(IDEA_FORMS.create, { aria: 'Новая идея', title: 'Новая идея', submit: 'Записать' });
  // общие с «Вопросами» подписи — одни и те же объекты, не копии
  assert.equal(IDEA_FORMS.defer, DEFER_FORM);
  assert.equal(IDEA_FORMS.start.submit, START_SUBMIT);
});

test('отложить: дата UTC по умолчанию, наименьшая и проверка', () => {
  const now = Date.UTC(2026, 10, 10, 23, 30);
  assert.equal(deferDefault(now), '2026-12-10');
  assert.equal(deferMin(now), '2026-11-11');
  // одна функция даты на «отложить» и на формы: `deferMin` — это `utcDate(1)`
  assert.equal(utcDate(0, now), '2026-11-10');
  assert.equal(deferMin(now), utcDate(1, now));
  assert.equal(deferDefault(now), utcDate(30, now));
  // без `now` — от текущего момента
  const day = (n: number) => new Date(Date.now() + n * 86_400_000).toISOString().slice(0, 10);
  assert.ok([day(30), day(31)].includes(deferDefault()));
  assert.ok([day(1), day(2)].includes(deferMin()));
  assert.equal(deferError('2026-11-11', now), '');
  assert.equal(deferError('2027-01-01', now), '');
  assert.equal(deferError('2026-11-10', now), 'Дата — не раньше 2026-11-11 (UTC).');
  assert.equal(deferError('', now), 'Дата — не раньше 2026-11-11 (UTC).');
});

test('decisionOf: решение и поля запроса по форме', () => {
  const f = { horizon: '2026-12-01', reason: '  уже не нужно ', space: 'knowledge' };
  assert.deepEqual(decisionOf('start', f), { decision: 'accept', opts: { space: 'knowledge' } });
  // дату «отложить» проверяет вызывающий (`deferError`), здесь — только поля запроса
  assert.deepEqual(decisionOf('defer', f), { decision: 'defer', opts: { horizon: '2026-12-01' } });
  // причина отказа необязательна, возврата — обязательна; пробелы по краям срезаются
  assert.deepEqual(decisionOf('decline', f), { decision: 'decline', opts: { reason: 'уже не нужно' } });
  assert.deepEqual(decisionOf('decline', { ...f, reason: '  ' }), { decision: 'decline', opts: {} });
  assert.deepEqual(decisionOf('reopen', f), { decision: 'reopen', opts: { reason: 'уже не нужно' } });
  assert.deepEqual(decisionOf('reopen', { ...f, reason: ' ' }), { error: 'Укажите причину.' });
});

test('decisionNotice: текст итога решения', () => {
  assert.equal(decisionNotice('accept', 'k:abc'), 'Идея начата: задача k:abc.');
  assert.equal(decisionNotice('defer', '', '2026-12-01'), 'Идея отложена до 2026-12-01 (UTC).');
  assert.equal(decisionNotice('decline', ''), 'Отказ записан.');
  assert.equal(decisionNotice('reopen', ''), 'Идея возвращена в новые.');
});

test('поля идеи: происхождение, приоритет, снятая задача, теги', () => {
  assert.equal(originOf(entry({ data: { origin: 'agent' } })), 'agent');
  assert.equal(originOf(entry({ data: { origin: 'human' } })), 'human');
  assert.equal(originOf(entry({ data: {} })), 'human');
  assert.equal(priorityOf(entry({ data: { priority: 'high' } })), 'high');
  assert.equal(priorityOf(entry({ data: { priority: 'low' } })), 'low');
  assert.equal(priorityOf(entry({ data: { priority: 'странный' } })), 'normal');
  assert.equal(priorityOf(entry({ data: {} })), 'normal');
  // задача снята или удалена — только у принятой идеи; нет `task` — как удалена
  const task = (over: Record<string, unknown>) => ({ id: 't', status: 'open', deleted: false, ...over });
  assert.equal(releasedOf(entry({ status: 'accepted', task: task({}) })), '');
  assert.equal(releasedOf(entry({ status: 'accepted', task: task({ status: 'dropped' }) })), 'снята');
  assert.equal(releasedOf(entry({ status: 'accepted', task: task({ deleted: true }) })), 'удалена');
  assert.equal(releasedOf(entry({ status: 'accepted' })), 'удалена');
  assert.equal(releasedOf(entry({ status: 'new' })), '');
  assert.equal(releasedOf(entry({ status: 'done', task: task({ status: 'dropped' }) })), '');
  assert.deepEqual(tagsOf(' ui, , кэш ,api'), ['ui', 'кэш', 'api']);
  assert.deepEqual(tagsOf(''), []);
});

test('sharedWarning: предупреждение об общем решении', () => {
  assert.equal(sharedWarning('Магазин'), 'Задача появится в решении «Магазин», её увидят участники.');
});
