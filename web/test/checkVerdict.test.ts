// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Ночная проверка памяти в WebUI (M7.3, спецификация §6.5 п. 5, §6.9 п. 2): тексты вердикта всех
// видов, принятая норма, кнопка «Принять как норму» и точка у «Проверка», метрики к медиане.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  acceptRun,
  flagText,
  foundText,
  metricRows,
  ppText,
  regressionOpen,
  runStatusText,
  shownVerdict,
  verdictText,
  type CheckRun,
  type CheckVerdict,
} from '../src/lib/checkVerdict.ts';

const base: CheckVerdict = { kind: 'ok', flags: [], after_update: false, few: false, base_n: 5, base_of: 5 };
const v = (x: Partial<CheckVerdict>): CheckVerdict => ({ ...base, ...x });

test('verdict_texts', () => {
  assert.equal(verdictText(v({ kind: 'ok' })), 'в норме');
  assert.equal(
    verdictText(v({ kind: 'ok', few: true })),
    'в норме (в test меньше 20 проб: сравниваются только устойчивые)',
  );
  assert.equal(verdictText(v({ kind: 'baseline', base_n: 3 })), 'накопление базы 3/5');
  assert.equal(verdictText(v({ kind: 'baseline', base_n: 0 })), 'накопление базы 0/5');
  assert.equal(
    verdictText(v({ kind: 'regression', flags: [{ name: 'hit_search', value: 0.52, median: 0.64, delta_pp: -12 }] })),
    'ухудшение: поиск −12 п. п. (64 % → 52 %)',
  );
  assert.equal(
    verdictText(
      v({
        kind: 'regression',
        after_update: true,
        after_update_version: '0.0.17-m7.4',
        flags: [{ name: 'lost_steady', lost: 4, steady: 30 }],
      }),
    ),
    'ухудшение после обновления до 0.0.17-m7.4: потеря устойчивых: 4 из 30',
  );
  // несколько флагов — через «;», пометка `few` — в конце
  assert.equal(
    verdictText(
      v({
        kind: 'regression',
        few: true,
        flags: [
          { name: 'negatives_shown', value: 0.3, median: 0.19, delta_pp: 11 },
          { name: 'degraded' },
        ],
      }),
    ),
    'ухудшение: подсказки к посторонним вопросам +11 п. п. (19 % → 30 %); поиск без векторов (в test меньше 20 проб: сравниваются только устойчивые)',
  );
  assert.equal(
    verdictText(v({ kind: 'changed', changed: { what: 'model', from: 'bge-m3', to: 'e5-large' } })),
    'изменилось: другая модель bge-m3 → e5-large',
  );
  assert.equal(
    verdictText(v({ kind: 'changed', changed: { what: 'reranker', from: null, to: 'bge-reranker' } })),
    'изменилось: другой реранкер нет → bge-reranker',
  );
  assert.equal(verdictText(v({ kind: 'changed' })), 'изменилось: другой отпечаток поиска');
  assert.equal(verdictText(v({ kind: 'degraded' })), 'поиск без векторов');
});

test('verdict_flag_texts', () => {
  assert.equal(ppText(-12.4), '−12 п. п.');
  assert.equal(ppText(10.6), '+11 п. п.');
  assert.equal(ppText(0.2), '0 п. п.');
  assert.equal(flagText({ name: 'hit_prompt', value: 0.5, median: 0.6, delta_pp: -10 }), 'подсказки к запросу −10 п. п. (60 % → 50 %)');
  // разница без `delta_pp` — по значению и медиане
  assert.equal(flagText({ name: 'hit_search', value: 0.5, median: 0.75 }), 'поиск −25 п. п. (75 % → 50 %)');
  assert.equal(flagText({ name: 'p95', value: 1800, median: 900 }), 'задержка p95 поиска 1\u00a0800 мс (медиана 900 мс)');
  assert.equal(flagText({ name: 'p95' }), 'задержка p95 поиска');
});

const run = (x: Partial<CheckRun>): CheckRun => ({
  run: 'R1',
  trigger: 'nightly',
  night: '2026-11-23',
  status: 'done',
  started_at: '2026-11-23T01:30:00.000000Z',
  finished_at: '2026-11-23T01:40:00.000000Z',
  metrics: null,
  verdict: v({ kind: 'regression', episode: 'R0', flags: [{ name: 'hit_search', value: 0.5, median: 0.62, delta_pp: -12 }] }),
  flags: [],
  episode: 'R0',
  accepted_at: null,
  accepted_by: null,
  error: null,
  ...x,
});
test('verdict_accepted_norm_starts_base', () => {
  // принятая норма: база начинается с этого прогона — «накопление базы 1/5», кнопки и точки нет
  const accepted = run({ accepted_at: '2026-11-23T08:00:00.000000Z', accepted_by: 'admin' });
  assert.equal(verdictText(shownVerdict(accepted)!), 'накопление базы 1/5');
  assert.equal(regressionOpen(accepted), false);
  assert.equal(acceptRun(accepted), null);
  // ручной прогон того же эпизода, снятый до принятия ночного, — тоже «накопление базы 1/5»
  const manual = run({ run: 'M1', trigger: 'manual', night: null });
  assert.equal(verdictText(shownVerdict(manual, accepted)!), 'накопление базы 1/5');
  // эпизод другой — прежний вердикт
  assert.equal(verdictText(shownVerdict({ ...manual, episode: 'R9' }, accepted)!), 'ухудшение: поиск −12 п. п. (62 % → 50 %)');
  // не принятая — прежний вердикт
  assert.equal(verdictText(shownVerdict(run({}))!), 'ухудшение: поиск −12 п. п. (62 % → 50 %)');
  assert.equal(shownVerdict(run({ verdict: null, status: 'timeout' })), null);
});

test('verdict_accept_target_latest_nightly', () => {
  // правило сервера (`check::digest_line`): последний законченный ночной прогон (его отдаёт фильтр
  // сервера `?trigger=nightly&status=done&limit=1`) — регрессия без принятой нормы
  assert.equal(regressionOpen(run({ run: 'R1' })), true);
  assert.equal(acceptRun(run({ run: 'R1' })), 'R1');
  // последний ночной — в норме (эпизод снят): ни кнопки, ни точки
  assert.equal(regressionOpen(run({ run: 'R2', verdict: v({ kind: 'ok' }) })), false);
  assert.equal(acceptRun(run({ run: 'R2', verdict: v({ kind: 'ok' }) })), null);
  // норма принята — ни кнопки, ни точки
  assert.equal(acceptRun(run({ accepted_at: '2026-11-23T08:00:00.000000Z' })), null);
  // ночных прогонов нет
  assert.equal(regressionOpen(null), false);
  assert.equal(acceptRun(null), null);
});

test('verdict_run_status_and_failing', () => {
  assert.equal(runStatusText({ status: 'failed', error: 'interrupted' }), 'прерван остановкой сервера');
  assert.equal(runStatusText({ status: 'failed', error: 'база недоступна' }), 'сбой: база недоступна');
  assert.match(runStatusText({ status: 'skipped', error: null }), /переиндексация/);
  assert.match(runStatusText({ status: 'timeout', error: null }), /предел времени прогона \(настройка администратора сервера\)/);
  const f = { probe: 'x:1', question: 'q', kind: 'probe', split: 'test', target: 'i:1', advice: 'term' };
  assert.equal(foundText({ ...f, rank: null }), 'не нашлась и в глубоком поиске');
  assert.equal(foundText({ ...f, rank: 14 }), 'в глубоком поиске на месте 14 — за границей бюджета основного');
  assert.equal(foundText({ ...f, kind: 'negative', target: null, shown: 2 }), 'посторонний вопрос: подсказок 2');
});

test('verdict_metric_rows', () => {
  const m = {
    test: { n: 30, hit_search: 0.8, mrr10: 0.6, none: 0.1, beyond: 0.1, hit_prompt: null, prompt_n: 0, hard: null, hard_n: 0 },
    negatives_shown: 0.1,
    search_p50_ms: 120,
    search_p95_ms: 1234.4,
  };
  const rows = metricRows(m, { hit_search: 0.9, hit_prompt: null, negatives_shown: 0.05, p95: 900, common: 30 });
  assert.deepEqual(
    rows.map((r) => [r.value, r.median]),
    [
      ['80 %', '90 %'],
      ['—', '—'],
      ['10 %', '5 %'],
      ['1\u00a0234 мс', '900 мс'],
    ],
  );
  // без базы — медианы «—»
  assert.deepEqual(metricRows(m, null).map((r) => r.median), ['—', '—', '—', '—']);
  assert.deepEqual(metricRows(null, null), []);
});
