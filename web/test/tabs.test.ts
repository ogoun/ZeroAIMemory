// Запуск: npm run test:build (node --test, встроенная поддержка TypeScript).
// Клавиатура вкладок ARIA — одна для всех вкладок WebUI (M7.2, ревью задачи 3, m6).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { onTabKey, tabKey } from '../src/lib/tabs.ts';

test('tab_key_arrows_home_end', () => {
  // ←/→ по кругу
  assert.equal(tabKey('ArrowRight', 0, 3), 1);
  assert.equal(tabKey('ArrowRight', 2, 3), 0);
  assert.equal(tabKey('ArrowLeft', 0, 3), 2);
  assert.equal(tabKey('ArrowLeft', 2, 3), 1);
  // Home, End
  assert.equal(tabKey('Home', 2, 3), 0);
  assert.equal(tabKey('End', 0, 3), 2);
  // две вкладки (редактор): стрелка в любую сторону — другая
  assert.equal(tabKey('ArrowRight', 1, 2), 0);
  assert.equal(tabKey('ArrowLeft', 0, 2), 1);
  // прочие клавиши и пустой список — не вкладок
  for (const k of ['Enter', ' ', 'Tab', 'ArrowDown', 'ArrowUp', 'a']) assert.equal(tabKey(k, 1, 3), -1, k);
  assert.equal(tabKey('ArrowRight', 0, 0), -1);
});

test('on_tab_key_selects_and_focuses', () => {
  const focused: number[] = [];
  const tabs = [0, 1, 2].map((n) => ({ focus: () => focused.push(n) }) as unknown as HTMLElement);
  const selected: number[] = [];
  let prevented = 0;
  const ev = (key: string) => ({ key, preventDefault: () => prevented++ }) as unknown as KeyboardEvent;
  // клавиша вкладок: выбор, фокус, отмена действия по умолчанию (прокрутка страницы)
  assert.equal(onTabKey(ev('End'), 0, 3, tabs, (j) => selected.push(j)), true);
  assert.deepEqual([selected, focused, prevented], [[2], [2], 1]);
  // не своя клавиша — ничего
  assert.equal(onTabKey(ev('Enter'), 0, 3, tabs, (j) => selected.push(j)), false);
  assert.deepEqual([selected, focused, prevented], [[2], [2], 1]);
  // кнопка ещё не привязана (`bind:this`) — выбор есть, фокуса нет
  assert.equal(onTabKey(ev('ArrowRight'), 0, 3, [tabs[0]], (j) => selected.push(j)), true);
  assert.deepEqual([selected, focused], [[2, 1], [2]]);
});
