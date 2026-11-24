// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Внутренние работы и кандидаты в Rules в WebUI (M7.3, спецификация §6.1–§6.3, §6.7, §6.9): класс
// адреса записи Vault (те же диапазоны, что у сервера, §6.2 п. 6), `body_extra` и рецепты, время
// ночи в поясе сервера и местное, журнал отправок, итог работы, тексты кандидатов.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  addressView,
  externalHost,
  externalOf,
  pinnedHost,
  pinnedUrl,
  nextSlot,
  nightText,
  outcomeText,
  parseBodyExtra,
  proposalAuthorText,
  reachOfHost,
  rulesTokensText,
  tzOffsetMin,
  usageText,
  vaultAddress,
  windowText,
  withRecipe,
  workResultText,
} from '../src/lib/worksText.ts';

test('works_reach_ranges', () => {
  // петля, «0.0.0.0/8», link-local (адрес метаданных облака), LAN и CGNAT, вне LAN
  for (const h of ['127.0.0.1', '127.8.9.10', '0.0.0.0', 'localhost', 'api.localhost', '[::1]', '[::ffff:127.0.0.1]', '[::ffff:7f00:1]']) {
    assert.equal(reachOfHost(h), 'loopback', h);
  }
  for (const h of ['169.254.169.254', '[fe80::1]']) assert.equal(reachOfHost(h), 'link_local', h);
  for (const h of ['10.0.0.5', '172.16.0.1', '172.31.255.255', '192.168.1.20', '100.64.0.1', '100.127.255.255', '[fd12::1]', '[::ffff:192.168.0.2]']) {
    assert.equal(reachOfHost(h), 'lan', h);
  }
  for (const h of ['203.0.113.10', '8.8.8.8', '172.32.0.1', '100.128.0.1', '[2001:db8::1]']) {
    assert.equal(reachOfHost(h), 'external', h);
  }
  // имя: что за ним — узнаёт сервер
  assert.equal(reachOfHost('api.openai.com'), 'name');
  assert.equal(reachOfHost('qwen.lan'), 'name');
});

test('works_address_view', () => {
  assert.deepEqual(addressView('https://203.0.113.10/v1'), { host: '203.0.113.10', reach: 'external', problem: '' });
  assert.deepEqual(addressView('http://127.0.0.1:8123/v1'), { host: '127.0.0.1:8123', reach: 'loopback', problem: '' });
  // порт схемы по умолчанию — не показывается (как хост в предупреждении сервера)
  assert.equal(addressView('https://api.openai.com:443/v1')?.host, 'api.openai.com');
  assert.match(addressView('http://203.0.113.10/v1')!.problem, /только в LAN/);
  assert.match(addressView('https://user:pw@example.com/v1')!.problem, /user@/);
  assert.match(addressView('https://example.com/v1?x=1')!.problem, /«\?…»/);
  assert.match(addressView('ftp://example.com')!.problem, /http или https/);
  assert.match(addressView('просто текст')!.problem, /не адрес/);
  assert.equal(addressView('  '), null);
  assert.equal(vaultAddress({ url: ' https://a.example.com/v1 ' }), 'https://a.example.com/v1');
  assert.equal(vaultAddress({ address: 'http://10.0.0.2:11434/v1' }), 'http://10.0.0.2:11434/v1');
  assert.equal(vaultAddress({}), '');
  assert.equal(externalHost(['works_model_external: 203.0.113.10']), '203.0.113.10');
  assert.equal(externalHost([]), null);
  assert.equal(externalHost(undefined), null);
});

test('works_pinned_external', () => {
  const ext = { scheme: 'https', host: '203.0.113.10', port: 443, path: '/v1' };
  assert.equal(pinnedHost(ext), '203.0.113.10');
  assert.equal(pinnedUrl(ext), 'https://203.0.113.10/v1');
  assert.equal(pinnedUrl({ scheme: 'http', host: '127.0.0.1', port: 8123, path: '/v1' }), 'http://127.0.0.1:8123/v1');
  assert.equal(pinnedHost({ scheme: 'http', host: '::1', port: 80, path: '' }), '[::1]');
  // литерал вне LAN — плашка без журнала; LAN и петля — нет
  assert.equal(externalOf(ext, []), '203.0.113.10');
  assert.equal(externalOf({ ...ext, host: '192.168.1.5', scheme: 'http', port: 8080 }, []), null);
  // имя — по последней строке журнала к этому хосту
  const name = { scheme: 'https', host: 'api.openai.com', port: 443, path: '/v1' };
  assert.equal(externalOf(name, []), null);
  assert.equal(externalOf(name, [{ host: 'api.openai.com', external: true }, { host: 'api.openai.com', external: false }]), 'api.openai.com');
  assert.equal(externalOf(name, [{ host: 'other.example.com', external: true }]), null);
  assert.equal(externalOf(null, []), null);
});

test('works_body_extra', () => {
  assert.deepEqual(parseBodyExtra(''), { value: {} });
  assert.deepEqual(parseBodyExtra('{"enable_thinking": false}'), { value: { enable_thinking: false } });
  assert.match((parseBodyExtra('[1]') as { error: string }).error, /JSON-объект/);
  assert.match((parseBodyExtra('{oops') as { error: string }).error, /^не JSON/);
  assert.match((parseBodyExtra('{"model": "x", "stream": true}') as { error: string }).error, /model, stream/);
  assert.match((parseBodyExtra(JSON.stringify({ k: 'я'.repeat(2100) })) as { error: string }).error, /больше 4 КиБ/);
  // рецепт — поверх набранного; не объект — поле не трогается
  assert.equal(
    withRecipe('{"temperature_hint": 1}', { chat_template_kwargs: { enable_thinking: false } }),
    JSON.stringify({ temperature_hint: 1, chat_template_kwargs: { enable_thinking: false } }, null, 2),
  );
  assert.equal(withRecipe('', { enable_thinking: false }), JSON.stringify({ enable_thinking: false }, null, 2));
  assert.equal(withRecipe('{oops', { a: 1 }), null);
});

test('works_night_time_zone', () => {
  const now = Date.parse('2026-11-24T10:00:00Z');
  // 04:30 по Москве — 01:30 UTC; ближайшая — завтра
  assert.equal(tzOffsetMin(now, 'Europe/Moscow'), 180);
  assert.equal(new Date(nextSlot('04:30', 'Europe/Moscow', now)!).toISOString(), '2026-11-25T01:30:00.000Z');
  assert.equal(new Date(nextSlot('23:00', 'UTC', now)!).toISOString(), '2026-11-24T23:00:00.000Z');
  assert.equal(new Date(nextSlot('04:30', '+03:00', now)!).toISOString(), '2026-11-25T01:30:00.000Z');
  // летнее время: Нью-Йорк в июле — UTC−4
  assert.equal(new Date(nextSlot('04:30', 'America/New_York', Date.parse('2026-07-01T12:00:00Z'))!).toISOString(), '2026-07-02T08:30:00.000Z');
  assert.equal(nextSlot('4.30', 'UTC', now), null);
  assert.equal(windowText(120), '2 ч');
  assert.equal(windowText(90), '1 ч 30 мин');
  assert.equal(windowText(45), '45 мин');
  assert.equal(
    nightText({ at: '04:30', tz: 'Europe/Moscow', window_min: 120 }, now, 'Europe/Moscow'),
    '04:30 Europe/Moscow (у вас 04:30), окно 2 ч',
  );
  assert.equal(nightText({ at: '04:30', tz: 'Europe/Moscow', window_min: 120 }, now, 'UTC'), '04:30 Europe/Moscow (у вас 01:30), окно 2 ч');
  assert.equal(nightText({ at: '4:05', tz: 'UTC', window_min: 30 }, now, 'Asia/Tokyo'), '04:05 UTC (у вас 13:05), окно 30 мин');
});

test('works_journal_and_result_texts', () => {
  assert.equal(outcomeText('ok', 200), 'ok');
  assert.equal(outcomeText('auth', 401), 'ключ не принят (auth, HTTP 401)');
  assert.equal(outcomeText('limit', null), 'предел токенов (limit)');
  assert.equal(outcomeText('future_class', null), 'future_class');
  assert.equal(usageText(12345, 2_000_000), '12 345 из 2 000 000 (0,6 %)');
  assert.equal(usageText(0, 0), '0 — обращений нет: предел 0');
  assert.equal(
    workResultText({ accepted: [{ n: 1 }, { n: 2 }], rejected: [{ n: 3 }], skip: [], skipped: [{ n: 4 }], skip_reason: null }),
    'принято 2 · отклонено 1 · без итога 1',
  );
  assert.equal(workResultText({ accepted: [], skip_reason: 'нечего спросить' }), 'принято 0 · отказ от работы: нечего спросить');
  assert.equal(workResultText(null), '');
});

test('works_proposal_texts', () => {
  assert.equal(proposalAuthorText('model', 'model:qwen3-30b'), 'модель qwen3-30b');
  assert.equal(proposalAuthorText('agent', 's-42', 's-42'), 'агент, сессия s-42');
  assert.equal(proposalAuthorText('agent', null, null), 'агент');
  assert.equal(rulesTokensText({ now: 1840, after: 1990 }), 'Rules в каждой сессии: 1 840 → 1 990 токенов (+150)');
  assert.equal(rulesTokensText(null), '');
});
