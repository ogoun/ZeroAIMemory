// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Настройки сервера внутренних работ в WebUI администратора и личный предел токенов (M7.6,
// спецификация §9.9, решения 171–174): у каждого ключа `[works]` — подпись и строка пояснения,
// правка — только изменённые поля с числами, ввод вне диапазона сервера — ошибка с подписью поля;
// окно предела — пустое поле — потолок, оба пустые — без предела, выше потолка — ошибка.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { SERVER_GROUPS, changedValues, draftOf, errorKey, fieldText, valueText } from '../src/lib/worksServer.ts';
import { limitsText, parseLimits } from '../src/lib/worksText.ts';

/** Ключи секции `[works]` (как `KEYS` сервера, `works/server_settings.rs`). */
const KEYS = [
  'enabled',
  'default_mode',
  'model_access',
  'allow_loopback',
  'max_daily_tokens',
  'max_monthly_tokens',
  'max_parallel',
  'call_timeout_s',
  'batch_records',
  'at',
  'tz',
  'window_min',
  'check',
  'check_max_min',
];

test('у каждого ключа [works] — подпись и одна строка пояснения', () => {
  const keys = SERVER_GROUPS.flatMap((g) => g.fields.map((f) => f.key));
  assert.deepEqual([...keys].sort(), [...KEYS].sort());
  for (const k of KEYS) {
    const f = fieldText(k)!;
    assert.ok(f.title && f.note && !f.note.includes('\n'), k);
    // без ключей конфигурации в тексте для человека
    assert.ok(!/[a-z]+_[a-z]+/.test(f.note), `${k}: ${f.note}`);
  }
  assert.match(fieldText('model_access')!.note, /куда угодно \/ только в локальную сеть \/ никуда/);
});

test('значения для человека', () => {
  assert.equal(valueText('enabled', true), 'да');
  assert.equal(valueText('model_access', 'lan'), 'только в локальную сеть');
  assert.equal(valueText('max_daily_tokens', 2_000_000), '2 000 000');
  assert.equal(valueText('tz', 'UTC'), 'UTC');
});

test('правка — только изменённые поля, числа числами, вне диапазона — ошибка', () => {
  const fields = {
    enabled: { value: true },
    model_access: { value: 'any' },
    max_parallel: { value: 1, min: 1, max: 4 },
    at: { value: '04:30' },
  };
  const d = draftOf(fields);
  assert.deepEqual(d, { enabled: true, model_access: 'any', max_parallel: '1', at: '04:30' });
  assert.deepEqual(changedValues(d, fields), { values: {} });
  assert.deepEqual(changedValues({ ...d, enabled: false, model_access: 'lan', max_parallel: '3', at: ' 03:15 ' }, fields), {
    values: { enabled: false, model_access: 'lan', max_parallel: 3, at: '03:15' },
  });
  const bad = changedValues({ ...d, max_parallel: '5' }, fields);
  assert.ok('error' in bad && bad.key === 'max_parallel' && bad.error.includes('от 1 до 4'), JSON.stringify(bad));
  assert.ok('error' in changedValues({ ...d, max_parallel: '1.5' }, fields));
  assert.ok('error' in changedValues({ ...d, max_parallel: '' }, fields));
  assert.equal(errorKey('works.window_min: от 10 до 720'), 'window_min');
  assert.equal(errorKey('нужно поле'), null);
});

test('окно предела: пустое — потолок, оба пустые — без предела, выше потолка — ошибка', () => {
  const ceiling = { daily: 1000, monthly: 5000 };
  assert.deepEqual(parseLimits('', '', ceiling), { value: null });
  assert.deepEqual(parseLimits('400', '', ceiling), { value: { daily_tokens: 400, monthly_tokens: 5000 } });
  assert.deepEqual(parseLimits('', '0', ceiling), { value: { daily_tokens: 1000, monthly_tokens: 0 } });
  assert.deepEqual(parseLimits('1 000', '4 000', ceiling), { value: { daily_tokens: 1000, monthly_tokens: 4000 } });
  const over = parseLimits('1001', '', ceiling);
  assert.ok('error' in over && over.error.includes('1 000'), JSON.stringify(over));
  assert.ok('error' in parseLimits('-1', '', ceiling));
  assert.ok('error' in parseLimits('', '2.5', ceiling));
  assert.equal(limitsText(null), 'нет (потолок сервера)');
  assert.equal(limitsText({ daily_tokens: 400_000, monthly_tokens: 900_000 }), '400 000 в сутки · 900 000 в месяц');
});
