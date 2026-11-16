// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Кнопки и формы решения по идее (M5.7 §10; M6, задача 16): общий модуль журнала «Идеи» и
// вопроса-предложения в «Вопросах»; чипы статусов, разделы и разбор агента (M6.5 §9, задача 8).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Entry, EntryStatus } from '../src/lib/api.ts';
import {
  IDEA_ACTIVE,
  IDEA_CHIPS,
  IDEA_FORMS,
  IDEA_TABS,
  REVIEW_OUTCOME_TEXT,
  chipCount,
  clip,
  decisionNotice,
  decisionOf,
  editNotice,
  groupByStatus,
  ideaButtons,
  olderReviews,
  originOf,
  priorityOf,
  refTarget,
  releasedOf,
  reviewCurrent,
  reviewLine,
  reviewNote,
  tagsOf,
} from '../src/lib/ideaButtons.ts';
import { IDEA_STATUSES, IDEA_STATUS_TEXT } from '../src/lib/ideaStatus.ts';
import { JOURNAL_OF_KIND, JOURNAL_OF_LETTER, REF_LETTER, entryRef } from '../src/lib/entryRefs.ts';
import { DEFER_FORM, START_SUBMIT, deferDefault, deferError, deferMin, sharedWarning, utcDate } from '../src/lib/ideaForms.ts';

const acts = (status: EntryStatus | null, agent = false, released = false) =>
  ideaButtons(status, agent, released).map((b) => b.act);

const entry = (over: Record<string, unknown>): Entry =>
  ({ id: 'x', kind: 'idea', status: 'new', data: {}, tags: [], ...over }) as unknown as Entry;

test('ideaButtons: состав и порядок кнопок по состоянию идеи', () => {
  // идея человека: активные — начать, отложить, отказаться; править — new, deferred и returned
  assert.deepEqual(acts('new'), ['start', 'defer', 'decline', 'edit']);
  assert.deepEqual(acts('proposed'), ['start', 'defer', 'decline']);
  assert.deepEqual(acts('deferred'), ['start', 'defer', 'decline', 'reopen', 'edit']);
  // возвращённая (M6.5 §4, §6): «Изменить» первой и главной; человек может и решить сам —
  // «Начать», «Отложить», «Отказаться»; «Вернуть» нет (на разбор её возвращает правка)
  assert.deepEqual(acts('returned'), ['edit', 'start', 'defer', 'decline']);
  assert.equal(ideaButtons('returned', false, false)[0].cls, 'primary');
  assert.deepEqual(ideaButtons('returned', false, false)[1], { act: 'start', title: 'Начать', cls: '' });
  // отклонённая: вернуть на разбор или удалить в корзину
  assert.deepEqual(acts('declined'), ['reopen', 'delete']);
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
  assert.deepEqual(IDEA_ACTIVE, ['new', 'returned', 'proposed', 'deferred']);
  const all: (EntryStatus | null)[] = ['new', 'returned', 'proposed', 'deferred', 'accepted', 'declined', 'done', 'open', null];
  for (const s of all) {
    const active = s !== null && IDEA_ACTIVE.includes(s);
    assert.equal(acts(s).includes('start'), active, `start у ${s}`);
    assert.equal(acts(s).includes('defer'), active, `defer у ${s}`);
  }
  // у returned есть «Изменить» и «Начать», нет «Вернуть» (решение координатора по задаче 8)
  assert.ok(acts('returned').includes('edit'));
  assert.ok(acts('returned').includes('start'));
  assert.ok(!acts('returned').includes('reopen'));
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
  assert.deepEqual(by.reopen, { act: 'reopen', title: 'Вернуть на разбор', cls: '' });
  assert.deepEqual(by.edit, { act: 'edit', title: 'Изменить', cls: '' });
  assert.deepEqual(ideaButtons('new', true, false)[0], { act: 'adopt', title: 'Принять как свою', cls: 'primary' });
  assert.deepEqual(ideaButtons('declined', false, false)[1], { act: 'delete', title: 'Удалить', cls: 'danger' });
});

test('формы решения: подписи для вёрстки и e2e', () => {
  assert.deepEqual(IDEA_FORMS.start, { aria: 'Начать идею', title: 'Начать: где поставить задачу', submit: 'Начать' });
  assert.deepEqual(IDEA_FORMS.defer, { aria: 'Отложить идею', title: 'Отложить', submit: 'Отложить' });
  assert.deepEqual(IDEA_FORMS.decline, { aria: 'Отказаться от идеи', title: 'Отказаться', submit: 'Отказаться' });
  assert.deepEqual(IDEA_FORMS.reopen, { aria: 'Вернуть идею', title: 'Вернуть на разбор', submit: 'Вернуть' });
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
  assert.equal(decisionNotice('reopen', ''), 'Идея возвращена на разбор.');
  // правка: возвращённая, ставшая «Непросмотрено», — «Отправлено на разбор» (M6.5 §6)
  assert.equal(editNotice('returned', 'new'), 'Отправлено на разбор.');
  assert.equal(editNotice('returned', 'returned'), 'Идея сохранена.');
  assert.equal(editNotice('new', 'new'), 'Идея сохранена.');
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

test('статусы идеи: подписи и порядок чипов (M6.5 §4, §9)', () => {
  assert.deepEqual(IDEA_STATUSES, ['new', 'returned', 'accepted', 'declined', 'done', 'deferred', 'proposed']);
  assert.deepEqual(
    IDEA_STATUSES.map((s) => IDEA_STATUS_TEXT[s]),
    ['Непросмотрено', 'На доработку', 'В плане', 'Отклонена', 'Сделано', 'Отложена', 'Предложена'],
  );
  // чипы: «Активные», статусы по порядку, «Все»
  assert.deepEqual(
    IDEA_CHIPS.map((c) => c.id),
    ['active', ...IDEA_STATUSES, 'all'],
  );
  assert.deepEqual(IDEA_CHIPS[0].statuses, IDEA_ACTIVE);
  assert.deepEqual(IDEA_CHIPS.at(-1)!.statuses, []);
  const counts = { new: 4, returned: 1, accepted: 2, declined: 1, done: 0, deferred: 3, proposed: 1 };
  assert.equal(chipCount('returned', counts), 1);
  assert.equal(chipCount('active', counts), 4 + 1 + 1 + 3);
  assert.equal(chipCount('all', counts), 12);
  assert.equal(chipCount('done', {}), 0);
});

test('groupByStatus: разделы в порядке чипов, пустых нет', () => {
  const it = (id: string, status: EntryStatus | null) => ({ id, status });
  const g = groupByStatus([it('a', 'declined'), it('b', 'new'), it('c', 'returned'), it('d', 'new'), it('e', 'open'), it('f', 'accepted')]);
  assert.deepEqual(
    g.map((x) => [x.status, x.items.map((i) => i.id)]),
    [
      ['new', ['b', 'd']],
      ['returned', ['c']],
      ['accepted', ['f']],
      ['declined', ['a']],
      ['open', ['e']],
    ],
  );
  assert.deepEqual(groupByStatus([]), []);
});

const review = (over: Record<string, unknown>) => ({
  outcome: 'plan',
  summary: 'Разбор: идея просит статусы в ленте, в плане такого нет.',
  basis: [],
  similar: [],
  by_user: 'u',
  at: '2026-11-16T10:00:00Z',
  ...over,
});

test('reviewLine: строка решения для четырёх исходов', () => {
  const long = 'Какие статусы показывать в ленте и в каком порядке, и нужна ли группировка по ним вообще?';
  // plan — ссылка на задачу и её заголовок
  const plan = entry({
    status: 'accepted',
    data: { review: review({ outcome: 'plan', task: 'k:01TASK' }), task_id: '01TASK' },
    task: { id: '01TASK', status: 'open', title: 'Статусы в ленте' },
  });
  assert.deepEqual(reviewLine(plan), { kind: 'plan', text: 'В план → k:01TASK «Статусы в ленте»' });
  // без заголовка задачи (удалена) — только ссылка
  assert.deepEqual(reviewLine(entry({ ...plan, task: { id: '01TASK', deleted: true } })), { kind: 'plan', text: 'В план → k:01TASK' });
  // задача выполнена — идея done, строка та же
  assert.equal(reviewLine(entry({ ...plan, status: 'done' })).text, 'В план → k:01TASK «Статусы в ленте»');
  // return — начало missing, не длиннее 60 символов
  const ret = entry({ status: 'returned', data: { review: review({ outcome: 'return', missing: long }) } });
  assert.deepEqual(reviewLine(ret), { kind: 'return', text: `Не хватает: ${clip(long, 60)}` });
  assert.ok(reviewLine(ret).text.endsWith('…'));
  assert.ok(Array.from(clip(long, 60)).length <= 61);
  // decline — начало разбора (переносы — пробелом)
  const dec = entry({ status: 'declined', data: { review: review({ outcome: 'decline', summary: `Уже обсуждали.\n\n${long}` }) } });
  assert.deepEqual(reviewLine(dec), { kind: 'decline', text: `Отклонена: ${clip(`Уже обсуждали. ${long}`, 60)}` });
  // done — ссылка на сделанное (первая c:/r:/commit:/k:)
  const done = entry({ status: 'done', data: { review: review({ outcome: 'done', basis: ['n:01NOTE', 'commit:abc1234', 'c:01CL'] }) } });
  assert.deepEqual(reviewLine(done), { kind: 'done', text: 'Сделано: commit:abc1234' });
  // нет разбора — пусто
  assert.deepEqual(reviewLine(entry({ status: 'new' })), { kind: '', text: '' });
  // короткий текст — без «…»
  assert.equal(clip('  коротко \n ', 60), 'коротко');
});

test('reviewLine и reviewCurrent: разбор, который уже не определяет состояние, строки не даёт', () => {
  // возвращённую дописали — снова «Непросмотрено»: «Не хватает» уже неактуально
  const edited = entry({ status: 'new', data: { review: review({ outcome: 'return', missing: 'что дописать' }) } });
  assert.equal(reviewCurrent(edited), false);
  assert.deepEqual(reviewLine(edited), { kind: '', text: '' });
  assert.equal(reviewNote(edited), 'После разбора статус изменился: Непросмотрено.');
  // после разбора человек решил сам — решение человека позже разбора
  const human = entry({
    status: 'declined',
    data: {
      review: review({ outcome: 'decline' }),
      decided: { decision: 'decline', by: 'human', channel: 'webui', at: '2026-11-16T12:00:00Z' },
    },
  });
  assert.equal(reviewCurrent(human), false);
  assert.deepEqual(reviewLine(human), { kind: '', text: '' });
  assert.equal(reviewNote(human), 'После разбора решение принял человек.');
  // решение самого разбора — текущее
  const agent = entry({
    status: 'declined',
    data: {
      review: review({ outcome: 'decline' }),
      decided: { decision: 'decline', by: 'agent', channel: 'review', at: '2026-11-16T10:00:00Z' },
    },
  });
  assert.equal(reviewCurrent(agent), true);
  assert.equal(reviewNote(agent), '');
  assert.equal(reviewNote(entry({ status: 'new' })), '');
  assert.equal(REVIEW_OUTCOME_TEXT.plan, 'В план');
  assert.equal(REVIEW_OUTCOME_TEXT.return, 'На доработку');
  assert.equal(REVIEW_OUTCOME_TEXT.decline, 'Отклонена');
  assert.equal(REVIEW_OUTCOME_TEXT.done, 'Сделано');
});

test('olderReviews: история без текущего, новые первыми', () => {
  const cur = review({ outcome: 'plan', at: '2026-11-16T11:00:00Z' });
  const prev = review({ outcome: 'return', missing: 'что', at: '2026-11-16T09:00:00Z' });
  assert.deepEqual(
    olderReviews(entry({ data: { review: cur, reviews: [cur, prev] } })).map((r) => r.outcome),
    ['return'],
  );
  assert.deepEqual(olderReviews(entry({ data: { review: cur, reviews: [cur] } })), []);
  assert.deepEqual(olderReviews(entry({ data: {} })), []);
  // мусор в истории пропускается
  assert.equal(olderReviews(entry({ data: { review: cur, reviews: [cur, { outcome: 'x' }, 5, prev] } })).length, 1);
});

test('refTarget: ссылки «Опирался на» и «Похожие»', () => {
  assert.deepEqual(refTarget('k:01ABC'), { journal: 'tasks', id: '01ABC' });
  assert.deepEqual(refTarget('w:01ABC'), { journal: 'ideas', id: '01ABC' });
  assert.deepEqual(refTarget('i:01ABC'), { journal: 'insights', id: '01ABC' });
  assert.deepEqual(refTarget('c:01ABC'), { journal: 'changelog', id: '01ABC' });
  assert.deepEqual(refTarget('r:01ABC'), { journal: 'releases', id: '01ABC' });
  assert.deepEqual(refTarget('t:01ABC'), { journal: 'glossary', id: '01ABC' });
  assert.deepEqual(refTarget('q:01ABC'), { journal: 'questions', id: '01ABC' });
  assert.deepEqual(refTarget('n:01ABC'), { node: '01ABC' });
  // file: и commit: — текстом с «копировать»
  assert.deepEqual(refTarget('file:docs/specs/a.md'), { copy: 'docs/specs/a.md' });
  assert.deepEqual(refTarget('commit:abc1234'), { copy: 'abc1234' });
  // мусор — как есть
  assert.equal(refTarget('без префикса'), null);
  assert.equal(refTarget('x:01ABC'), null);
  assert.equal(refTarget('k:'), null);
  assert.equal(refTarget('k:../../admin'), null);
  // свойства прототипа объекта — не буквы ссылок
  assert.equal(refTarget('constructor:01ABC'), null);
  assert.equal(refTarget('toString:01ABC'), null);
  // буквы ссылок — те же, что у entryRef (entryRefs.ts)
  assert.deepEqual(refTarget(`${entryRef('report', '01R').split(':')[0]}:01R`), { journal: 'reports', id: '01R' });
  assert.equal(entryRef('idea', '01W'), 'w:01W');
  assert.equal(entryRef('нет такого', '01E'), 'e:01E');
});

test('entryRefs: буква ссылки → журнал — обратное REF_LETTER', () => {
  for (const [kind, letter] of Object.entries(REF_LETTER)) assert.equal(JOURNAL_OF_LETTER[letter], JOURNAL_OF_KIND[kind], kind);
  assert.equal(Object.keys(JOURNAL_OF_LETTER).length, Object.keys(REF_LETTER).length);
});
