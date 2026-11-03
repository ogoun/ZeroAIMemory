// Печатная копия отчёта в `body` (M5.5): класс и удаление — без загрузки модуля печати
// (`lib/print.ts` грузится по клику «Скачать PDF»).

/** Класс печатного контейнера; `ready` — копия догружена и участвует в печати (print.css). */
export const PRINT_ROOT_CLASS = 'print-root';
export const PRINT_READY_CLASS = 'ready';

/** Убирает печатную копию: смена отчёта, уход со страницы, повторная печать. */
export function clearPrint(): void {
  for (const el of document.querySelectorAll(`body > .${PRINT_ROOT_CLASS}`)) el.remove();
}

/**
 * Готовая печатная копия отчёта `id` или `null`. Повторное «Скачать PDF» печатает её сразу, без
 * пересборки и ожидания: `window.print()` вызывается в обработчике клика, пока действует
 * пользовательская активация (Safari после долгого ожидания её теряет).
 */
export function readyPrint(id: string): HTMLElement | null {
  for (const el of document.querySelectorAll<HTMLElement>(`body > .${PRINT_ROOT_CLASS}.${PRINT_READY_CLASS}`)) {
    if (el.dataset.report === id) return el;
  }
  return null;
}
