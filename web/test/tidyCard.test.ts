// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Карточка предложения уборки (M7.5, спецификация §8.8 п. 1): текст операций и сигналов, дифф
// `merge` (`keep` против `text` или `drop`), `stale` — «Применить» недоступно с объяснением, поля и
// тело окна «Применить», отказ 409.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  STALE_TEXT,
  appliedText,
  applyBody,
  applyForm,
  conflictText,
  decideState,
  humanDecisionText,
  lineDiff,
  mergeDiff,
  opShort,
  opText,
  signalText,
  tidyTitle,
  type TidyFields,
  type TidyRecord,
} from '../src/lib/tidyCard.ts';

const rec = (ref: string, title: string, l1: string, extra: Partial<TidyRecord> = {}): TidyRecord => ({
  ref,
  rev: 1,
  kind: 'insight',
  insight_kind: 'gotcha',
  title,
  l1,
  status: 'active',
  certainty: null,
  human_decision: false,
  ...extra,
});

const A = rec('i:A', 'Сборка падает без кэша', 'Сборка падает без кэша.\nЧистить target.');
const B = rec('i:B', 'Сборка падает без кэша моделей', 'Сборка падает без кэша моделей.\nЧистить target.', { certainty: 'axiom', human_decision: true });

const card = (op: TidyFields['op'], extra: Partial<TidyFields> = {}): TidyFields => ({
  status: 'proposed',
  candidate: 'near_duplicate',
  signals: [{ name: 'cosine', value: '0.85' }],
  records: [A, B],
  op,
  stale: false,
  ...extra,
});

test('tidy_op_texts', () => {
  const merge = card({ op: 'merge', keep: 'i:A', drop: 'i:B', text: 'Объединённый текст.' });
  assert.equal(tidyTitle(merge), 'Почти-дубли: «Сборка падает без кэша» и «Сборка падает без кэша моделей»');
  assert.equal(
    opText(merge),
    'Объединить: «Сборка падает без кэша» (i:A) замещается новой записью с объединённым текстом, «Сборка падает без кэша моделей» (i:B) отзывается как дубль.',
  );
  assert.equal(
    opText(card({ op: 'merge', keep: 'i:A', drop: 'i:B' })),
    'Объединить: «Сборка падает без кэша моделей» (i:B) отзывается как дубль «Сборка падает без кэша» (i:A); текст i:A не меняется.',
  );
  assert.equal(opShort(merge), 'Объединить: «Сборка падает без кэша моделей»');
  const one = { records: [A] };
  assert.equal(opText({ ...one, op: { op: 'retract', target: 'i:A' } }), 'Отозвать «Сборка падает без кэша» (i:A): запись остаётся в истории со статусом «отозвана».');
  assert.equal(opText({ ...one, op: { op: 'revisit', target: 'i:A' } }), 'Отправить «Сборка падает без кэша» (i:A) на пересмотр (статус «на пересмотре»).');
  assert.equal(
    opText({ ...one, op: { op: 'triggers', target: 'i:A', triggers: [{ type: 'keyword', pattern: 'кэш' }, { type: 'file_glob', pattern: 'build/**', tools: ['Edit'] }] } }),
    'Заменить триггеры «Сборка падает без кэша» (i:A): keyword «кэш»; file_glob «build/**» (Edit).',
  );
  assert.equal(opText({ ...one, op: { op: 'triggers', target: 'i:A', triggers: [] } }), 'Заменить триггеры «Сборка падает без кэша» (i:A): без триггеров.');
  assert.equal(opText({ records: [], op: { op: 'term', title: 'Кэш', definition: 'Каталог моделей.', aliases: ['cache'] } }), 'Новый термин «Кэш»: Каталог моделей. Псевдонимы: cache.');
  assert.equal(opText({ records: [rec('t:T', 'Кэш', 'Каталог.', { kind: 'term' })], op: { op: 'term', target: 't:T', aliases: ['cache', 'кеш'] } }), 'Псевдонимы к термину «Кэш» (t:T): cache, кеш.');
  assert.equal(opText({ ...one, op: { op: 'append', target: 'i:A', text: 'x' } }), 'Дописать к «Сборка падает без кэша» (i:A) текст ниже.');
  // ссылка вне кандидата — как есть
  assert.equal(opText({ ...one, op: { op: 'retract', target: 'i:Z' } }), 'Отозвать i:Z: запись остаётся в истории со статусом «отозвана».');
  assert.equal(humanDecisionText(merge), '«Сборка падает без кэша моделей» (i:B) — решение человека (axiom): меняйте её, только если она явно ошибочна.');
  assert.equal(humanDecisionText({ records: [A] }), '');
});

test('tidy_signal_texts', () => {
  assert.equal(signalText({ name: 'cosine', value: '0.85' }), 'Сходство по смыслу (косинус): 0,85');
  assert.equal(signalText({ name: 'contradicts_days', value: '1' }), 'Противоречие не разрешено 1 сутки');
  assert.equal(signalText({ name: 'contradicts_days', value: '5' }), 'Противоречие не разрешено 5 суток');
  assert.equal(signalText({ name: 'revisit_days', value: '21' }), 'На пересмотре 21 сутки');
  assert.equal(signalText({ name: 'stale_refs', value: '2' }), 'Изменились связанные файлы: 2');
  assert.equal(signalText({ name: 'probe_fails', value: '3' }), 'Не находится 3 ночных прогона подряд');
  assert.equal(signalText({ name: 'question', value: 'Где кэш?' }), 'Вопрос пробы: «Где кэш?»');
  assert.equal(signalText({ name: 'shown', value: '12' }), 'Показов подсказкой: 12');
  assert.equal(signalText({ name: 'same_name', value: 'кэш' }), 'Совпадает имя термина: «кэш»');
  assert.equal(signalText({ name: 'future', value: 'x' }), 'future: x');
});

test('tidy_line_diff', () => {
  assert.equal(lineDiff('a\nb\n', 'a\nb', 'x', 'y'), '');
  assert.equal(lineDiff('a\nb\nc', 'a\nB\nc\nd', 'keep', 'text'), '--- keep\n+++ text\n@@ -1,3 +1,4 @@\n a\n-b\n+B\n c\n+d\n');
  // строка, начинающаяся с «--», после шапки — удалённая, а не шапка
  assert.equal(lineDiff('-- x', '', 'k', 't'), '--- k\n+++ t\n@@ -1,1 +1,0 @@\n--- x\n');
});

test('tidy_merge_diff', () => {
  const withText = card({ op: 'merge', keep: 'i:A', drop: 'i:B', text: 'Сборка падает без кэша.\nЧистить target и models.' });
  // L1 предложения, тела не загружены
  let d = mergeDiff(withText)!;
  assert.equal(d.against, 'text');
  assert.match(d.diff, /^--- i:A \(L1\)\n\+\+\+ объединённый текст\n/);
  assert.match(d.diff, /\n-Чистить target\.\n\+Чистить target и models\.\n$/);
  // полное тело keep и правка человеком в окне
  d = mergeDiff(withText, { 'i:A': 'Сборка падает без кэша.\nЧистить target.\nПодробно.' }, 'Сборка падает без кэша.')!;
  assert.match(d.diff, /^--- i:A\n/);
  assert.match(d.diff, /\n-Чистить target\.\n-Подробно\.\n$/);
  assert.match(d.caption, /^Что станет с «Сборка падает без кэша» \(i:A\): было — стало/);
  // без текста — keep против drop
  d = mergeDiff(card({ op: 'merge', keep: 'i:A', drop: 'i:B' }))!;
  assert.equal(d.against, 'drop');
  assert.match(d.diff, /^--- i:A \(L1\)\n\+\+\+ i:B \(L1\)\n/);
  assert.match(d.caption, /^Чем различаются «Сборка падает без кэша» \(i:A\) и «Сборка падает без кэша моделей» \(i:B\)/);
  // набранный текст у merge без текста — против него; пустой — снова drop
  assert.equal(mergeDiff(card({ op: 'merge', keep: 'i:A', drop: 'i:B' }), {}, 'Новое.')!.against, 'text');
  assert.equal(mergeDiff(card({ op: 'merge', keep: 'i:A', drop: 'i:B' }), {}, '  ')!.against, 'drop');
  assert.equal(mergeDiff(card({ op: 'retract', target: 'i:A' })), null);
});

test('tidy_stale_apply_disabled', () => {
  assert.deepEqual(decideState({ status: 'proposed', stale: false }), { show: true, apply: true, why: '' });
  assert.deepEqual(decideState({ status: 'proposed', stale: true }), { show: true, apply: false, why: STALE_TEXT });
  assert.deepEqual(decideState({ status: 'stale', stale: true }), { show: false, apply: false, why: STALE_TEXT });
  assert.deepEqual(decideState({ status: 'accepted', stale: false }), { show: false, apply: false, why: '' });
  assert.match(STALE_TEXT, /^Запись изменилась после предложения — применить нельзя/);
  assert.equal(conflictText('stale'), STALE_TEXT);
  assert.equal(conflictText('not_open', 'declined'), 'Предложение уже решено (declined) — обновите карточку.');
  assert.equal(conflictText('path_taken'), null);
});

test('tidy_apply_form_and_body', () => {
  const m = applyForm(card({ op: 'merge', keep: 'i:A', drop: 'i:B', text: 'Объединённый.' }));
  assert.equal(m.kind, 'text');
  assert.equal(m.initial, 'Объединённый.');
  assert.equal(m.required, true);
  assert.deepEqual(applyBody(m, ' Объединённый. '), {});
  assert.deepEqual(applyBody(m, 'Поправленный.'), { text: 'Поправленный.' });
  assert.throws(() => applyBody(m, '  '), /пустой текст/);
  const bare = applyForm(card({ op: 'merge', keep: 'i:A', drop: 'i:B' }));
  assert.equal(bare.required, false);
  assert.deepEqual(applyBody(bare, ''), {});
  assert.deepEqual(applyBody(bare, 'Свой текст.'), { text: 'Свой текст.' });
  const ap = applyForm({ records: [A], op: { op: 'append', target: 'i:A', text: 'Дописка.' } });
  assert.deepEqual([ap.kind, ap.label, ap.initial, ap.required], ['text', 'Дописать', 'Дописка.', true]);
  const rt = applyForm({ records: [A], op: { op: 'retract', target: 'i:A' } });
  assert.deepEqual([rt.kind, rt.danger], ['confirm', true]);
  assert.deepEqual(applyBody(rt, 'x'), {});
  assert.deepEqual([applyForm({ records: [A], op: { op: 'revisit', target: 'i:A' } }).kind, applyForm({ records: [A], op: { op: 'revisit', target: 'i:A' } }).danger], ['confirm', false]);
  assert.match(appliedText({ op: 'merge', keep: 'i:A', drop: 'i:B' }, false), /^Применено: дубль отозван/);
  assert.match(appliedText({ op: 'merge', keep: 'i:A', drop: 'i:B' }, true), /^Применено: записи объединены/);
  assert.match(appliedText({ op: 'retract', target: 'i:A' }, false, ['i:B не отозван']), /запись отозвана.*Внимание: i:B не отозван\./);
});
