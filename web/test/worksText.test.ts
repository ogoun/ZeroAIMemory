// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Внутренние работы и кандидаты в Rules в WebUI (M7.3, спецификация §6.1–§6.3, §6.7, §6.9): класс
// адреса записи Vault (те же диапазоны, что у сервера, §6.2 п. 6), `body_extra` и рецепты, время
// ночи в поясе сервера и местное, журнал отправок, итог работы, тексты кандидатов. M7.6 (§9.2,
// решения 166–169): протокол Anthropic, выбор записи с адресом API, путь адреса, итог проверки.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  CALL_NO_MS_HINT,
  addressView,
  adminCheckText,
  callDurationText,
  callMsText,
  checkOffText,
  modesSummary,
  num,
  pinnedHost,
  pinnedUrl,
  isModelUrl,
  nextSlot,
  nightText,
  pathWarning,
  recipesOf,
  requestUrlOf,
  testFailText,
  vaultChoiceLabel,
  outcomeText,
  parseBodyExtra,
  proposalAuthorText,
  queueText,
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
  // вложенный IPv4 (как у сервера): петля и link-local внутри — их класс, частный внутри — вне LAN
  assert.equal(reachOfHost('[::127.0.0.1]'), 'loopback');
  assert.equal(reachOfHost('[64:ff9b::7f00:1]'), 'loopback');
  assert.equal(reachOfHost('[2002:7f00:1::]'), 'loopback');
  assert.equal(reachOfHost('[64:ff9b::a9fe:a9fe]'), 'link_local');
  assert.equal(reachOfHost('[::10.0.0.5]'), 'external');
  assert.equal(reachOfHost('[2002:c0a8:1::]'), 'external');
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
});

test('works_pinned_address', () => {
  // класс сохранённого подключения — поле `external` ответа настроек (сервер, без DNS); здесь —
  // только запись адреса
  const ext = { scheme: 'https', host: '203.0.113.10', port: 443, path: '/v1' };
  assert.equal(pinnedHost(ext), '203.0.113.10');
  assert.equal(pinnedUrl(ext), 'https://203.0.113.10/v1');
  assert.equal(pinnedUrl({ scheme: 'http', host: '127.0.0.1', port: 8123, path: '/v1' }), 'http://127.0.0.1:8123/v1');
  assert.equal(pinnedHost({ scheme: 'http', host: '::1', port: 80, path: '' }), '[::1]');
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
  // принятое, которое закрытие работы не записало (предел, повтор), — видно (финальное ревью
  // M7.3, Minor 1): «принято» без пометки было бы неправдой
  assert.equal(
    workResultText({ accepted: [{ n: 1 }, { n: 2 }], rejected: [], warnings: ['i:1: проба не записана — предел'] }),
    'принято 2 · не записано 1',
  );
});

test('works_journal_duration_none', () => {
  // финальное ревью M7.3, Minor 2: строка без длительности (`aborted`, `internal`, `limit`, …) —
  // «—» с подсказкой, а не «0 мс»
  assert.equal(callMsText(null), '—');
  assert.equal(callMsText(undefined), '—');
  assert.equal(callMsText(0), '0');
  assert.equal(callMsText(1234), num(1234));
  assert.equal(callDurationText(null), `— (${CALL_NO_MS_HINT})`);
  assert.equal(callDurationText(85), `${num(85)} мс`);
  assert.match(CALL_NO_MS_HINT, /ответа не было/);
});

test('works_check_server_off_texts', () => {
  // финальное ревью M7.3, Minor 12: выключатель «Ночная проверка моей памяти» при проверке,
  // выключенной на сервере, — с причиной
  assert.match(checkOffText('check_off'), /^На сервере ночная проверка выключена администратором/);
  assert.match(checkOffText('check_off'), /«Проверка памяти»/);
  assert.match(checkOffText('disabled'), /внутренние работы выключены/);
  assert.doesNotMatch(checkOffText('disabled'), /ручной прогон/);
  assert.match(checkOffText(null), /ночная проверка выключена/);
});

test('works_proposal_texts', () => {
  assert.equal(proposalAuthorText('model', 'model:qwen3-30b'), 'модель qwen3-30b');
  assert.equal(proposalAuthorText('agent', 's-42', 's-42'), 'агент, сессия s-42');
  assert.equal(proposalAuthorText('agent', null, null), 'агент');
  assert.equal(rulesTokensText({ now: 1840, after: 1990 }), 'Rules в каждой сессии: 1 840 → 1 990 токенов (+150)');
  // форма слова — по второму числу
  assert.equal(rulesTokensText({ now: 0, after: 43 }), 'Rules в каждой сессии: 0 → 43 токена (+43)');
  assert.equal(rulesTokensText({ now: 20, after: 21 }), 'Rules в каждой сессии: 20 → 21 токен (+1)');
  assert.equal(rulesTokensText(null), '');
});

test('works_admin_summary_texts', () => {
  // сводка администратора (M7.4 §7.7 п. 2): режимы видов, очередь, последний прогон проверки; вид
  // уборки (M7.5 §8.7) — третьим, без строки сервера — «выкл»
  assert.equal(modesSummary({ probes: { mode: 'agent' }, rules: { mode: 'off' } }), 'пробы: агент · кандидаты в Rules: выкл · уборка: выкл');
  assert.equal(modesSummary({ probes: { mode: 'off' }, rules: { mode: 'off' }, tidy: { mode: 'model' } }), 'пробы: выкл · кандидаты в Rules: выкл · уборка: модель');
  assert.equal(modesSummary(undefined), 'пробы: выкл · кандидаты в Rules: выкл · уборка: выкл');
  assert.equal(queueText(2, 1), '2 в очереди · 1 идёт');
  assert.equal(queueText(0, 0), 'пусто');
  assert.equal(adminCheckText({ status: 'done', verdict: 'regression' }), 'ухудшение');
  assert.equal(adminCheckText({ status: 'skipped', verdict: null }), 'пропущен');
  assert.equal(adminCheckText({ status: null, verdict: null }), 'не было');
});

test('works_anthropic_body_extra_and_recipes', () => {
  // у Anthropic зарезервированы system и параметры выборки — с пояснением про версию API
  assert.match((parseBodyExtra('{"temperature": 0.2}', 'anthropic') as { error: string }).error, /temperature.*2026-10-21/);
  assert.match((parseBodyExtra('{"system": "x"}', 'anthropic') as { error: string }).error, /system задаёт сервер/);
  assert.deepEqual(parseBodyExtra('{"output_config": {"effort": "low"}}', 'anthropic'), { value: { output_config: { effort: 'low' } } });
  // у OpenAI-совместимого — как раньше: system и temperature не зарезервированы
  assert.deepEqual(parseBodyExtra('{"system": "x", "temperature": 0.2}'), { value: { system: 'x', temperature: 0.2 } });
  assert.match((parseBodyExtra('{"max_completion_tokens": 1}', 'openai') as { error: string }).error, /max_completion_tokens/);
  assert.ok(recipesOf('anthropic').some((r) => JSON.stringify(r.value).includes('effort')));
  assert.ok(recipesOf('openai').some((r) => JSON.stringify(r.value).includes('enable_thinking')));
});

test('works_model_url_choices', () => {
  // решение 166: в выборе — только http(s)-адреса; описания хостов — нет
  for (const ok of ['https://api.openai.com/v1', 'http://192.0.2.5:8080/v1', 'https://api.anthropic.com', 'HTTP://host.example.com']) {
    assert.equal(isModelUrl(ok), true, ok);
  }
  for (const bad of ['192.0.2.10 (SSH, ключ в агенте)', 'локальная машина', 'ssh://host', 'https:// пробел', '', 'https://']) {
    assert.equal(isModelUrl(bad), false, bad);
  }
  assert.equal(vaultChoiceLabel('llm', ' http://192.0.2.5:8080/v1 '), 'llm — http://192.0.2.5:8080/v1');
});

test('works_request_url_and_path_warning', () => {
  // куда уйдёт запрос: как `request_url` сервера (у Anthropic /v1 не удваивается)
  assert.equal(requestUrlOf('openai', 'http://192.0.2.5:8080/v1/'), 'http://192.0.2.5:8080/v1/chat/completions');
  assert.equal(requestUrlOf('anthropic', 'https://api.anthropic.com'), 'https://api.anthropic.com/v1/messages');
  assert.equal(requestUrlOf('anthropic', 'https://api.anthropic.com/v1'), 'https://api.anthropic.com/v1/messages');
  // решение 168: OpenAI-совместимый без пути — предупреждение и исправление «+ /v1»
  const w = pathWarning('openai', 'http://192.0.2.5:8080/');
  assert.ok(w && w.text.includes('Адрес без /v1') && w.text.includes('http://192.0.2.5:8080/chat/completions'), w?.text);
  assert.equal(w?.fixed, 'http://192.0.2.5:8080/v1');
  assert.equal(pathWarning('openai', 'http://192.0.2.5:8080/v1'), null);
  assert.equal(pathWarning('openai', 'https://dashscope.example.com/compatible-mode/v1'), null);
  // полная точка входа вместо базы — путь удвоится
  assert.equal(pathWarning('openai', 'https://api.example.com/v1/chat/completions')?.fixed, 'https://api.example.com/v1');
  // Anthropic: база без пути — нормально, точка входа /v1/messages — исправить
  assert.equal(pathWarning('anthropic', 'https://api.anthropic.com'), null);
  assert.equal(pathWarning('anthropic', 'https://api.anthropic.com/v1'), null);
  assert.equal(pathWarning('anthropic', 'https://api.anthropic.com/v1/messages')?.fixed, 'https://api.anthropic.com/v1');
  assert.equal(pathWarning('openai', 'описание хоста'), null);
});

test('works_test_fail_text', () => {
  // решение 169: понятная причина и полный адрес запроса
  const url = 'http://192.0.2.5:8080/v1/chat/completions';
  assert.equal(
    testFailText({ ok: false, ms: 5, class: 'auth', url }, 'm'),
    `Подключение не работает: ключ не подошёл — проверьте значение записи Vault. Адрес запроса: ${url}`,
  );
  assert.match(testFailText({ ok: false, ms: 5, class: 'not_found', url }, 'm'), /по адресу нет API — проверьте адрес/);
  assert.match(
    testFailText({ ok: false, ms: 5, class: 'not_found', url, models: ['a', 'b'] }, 'qwen'),
    /модели «qwen» на нём нет — есть: a, b\./,
  );
  assert.match(testFailText({ ok: false, ms: null, class: 'unavailable', url }, 'm'), /сервер не отвечает/);
  assert.equal(
    testFailText({ ok: false, ms: 5, class: 'bad_json', hint: 'модель ответила не JSON' }, 'm'),
    'Подключение не работает: ответ не JSON (bad_json). модель ответила не JSON.',
  );
});
