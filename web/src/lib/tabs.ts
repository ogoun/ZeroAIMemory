// Клавиатура вкладок ARIA (`role="tablist"`, образец ARIA APG): ←/→ — соседняя вкладка по кругу,
// Home — первая, End — последняя; выбор следует за фокусом, `tabindex` бегающий (у вкладок — сам
// компонент). Общая для всех вкладок с клавиатурой: `StatusTabs`, вкладки «Кода», «Идей», листов
// таблицы, панелей редактора (ревью задачи 3 M7.2, m6), вида «Связей» и вкладки документа
// (`DocView`, финальное ревью M7.2, M3). Модуль без runtime-импортов: его читает `node --test`
// (web/test/tabs.test.ts).

/** Индекс вкладки после клавиши `key` на вкладке `i` из `n`; `-1` — клавиша не вкладок. */
export function tabKey(key: string, i: number, n: number): number {
  if (n <= 0) return -1;
  if (key === 'ArrowRight') return (i + 1) % n;
  if (key === 'ArrowLeft') return (i - 1 + n) % n;
  if (key === 'Home') return 0;
  if (key === 'End') return n - 1;
  return -1;
}

/**
 * Обработчик `keydown` вкладки `i` из `n`: клавиша вкладок — `select(j)` и фокус на вкладку `j`
 * (кнопки — `tabs` по порядку), действие браузера по умолчанию отменяется. Результат — клавиша
 * обработана.
 */
export function onTabKey(
  e: KeyboardEvent,
  i: number,
  n: number,
  tabs: ArrayLike<HTMLElement | null | undefined>,
  select: (j: number) => void,
): boolean {
  const j = tabKey(e.key, i, n);
  if (j < 0) return false;
  e.preventDefault();
  select(j);
  tabs[j]?.focus();
  return true;
}
