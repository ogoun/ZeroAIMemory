// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Название токена по умолчанию (M7.6 §9.1, решение координатора 141).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { TOKEN_NAME_MAX, defaultTokenName, localDate } from '../src/lib/tokenName.ts';

test('default_token_name', () => {
  const d = new Date(2026, 10, 28, 23, 59);
  assert.equal(localDate(d), '2026-11-28');
  assert.equal(defaultTokenName('Claude Code', 'AIIndicators', d), 'Claude Code · AIIndicators · 2026-11-28');
  assert.equal(defaultTokenName('Codex', '  новое решение ', new Date(2026, 0, 5)), 'Codex · новое решение · 2026-01-05');
  // длинное имя решения обрезается с «…», итог — ровно предел
  const long = defaultTokenName('AGENTS.md', 'я'.repeat(128), d);
  assert.equal([...long].length, TOKEN_NAME_MAX);
  assert.ok(long.startsWith('AGENTS.md · яя'), long);
  assert.ok(long.endsWith('… · 2026-11-28'), long);
});
