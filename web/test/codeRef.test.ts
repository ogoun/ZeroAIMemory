// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Адрес карточки символа (M6 §12а): у `qual` вида якоря строки (`L10`) — ссылка по `id`.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isLineAnchor, symbolEntry } from '../src/lib/codeRef.ts';

test('isLineAnchor — как zam_code::is_line_anchor', () => {
  for (const q of ['L10', 'L1-L2', 'L10-20', 'L0']) assert.equal(isLineAnchor(q), true, q);
  for (const q of ['L', 'L10#2', 'Lx', 'L10-', 'L10-Lx', 'l10', 'Store::L10', 'L1-L2-L3', 'open'])
    assert.equal(isLineAnchor(q), false, q);
});

test('line_anchor_symbol_link_by_id: L10 — по id, прочие — путь#qual', () => {
  assert.equal(symbolEntry({ id: 42, path: 'src/a.rs', qual: 'L10' }), '42');
  assert.equal(symbolEntry({ id: 43, path: 'src/a.rs', qual: 'L1-L2' }), '43');
  assert.equal(symbolEntry({ id: 44, path: 'src/a.rs', qual: 'Store::open' }), 'src/a.rs#Store::open');
});
