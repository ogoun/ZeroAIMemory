// Доска задач (M4.5 §3; M7.2, спецификация §5.2 п. 1): колонки статусов и вкладка по умолчанию в
// узкой области. Модуль без runtime-импортов: его читает `node --test` (`web/test/taskBoard.test.ts`);
// `journals.ts` реэкспортирует.
import type { EntryStatus } from './api';

/** Колонки доски задач (M4.5 §3) — порядок колонок и вкладок. */
export const TASK_COLUMNS: EntryStatus[] = ['open', 'in_progress', 'blocked', 'done', 'dropped'];

/**
 * Вкладка доски задач в узкой области (решение координатора 18): статус задачи из адреса, иначе
 * «в работе», а если в работе задач нет — «открыт». Числа — `entries/counts` с фильтрами доски; не
 * пришли (`null`) — «в работе».
 */
export function boardTab(counts: Partial<Record<EntryStatus, number>> | null, status: EntryStatus | null = null): EntryStatus {
  if (status && TASK_COLUMNS.includes(status)) return status;
  return counts && !counts.in_progress ? 'open' : 'in_progress';
}
