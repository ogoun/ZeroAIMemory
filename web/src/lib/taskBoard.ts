// Доска задач (M4.5 §3; M7.2, спецификация §5.2 п. 1): колонки статусов и вкладка по умолчанию в
// узкой области. Числа доски и шаги планов (M7.3, спецификация §6.14 п. 10, задача 10c): строка над
// доской, «+K шагов» у колонок и вкладок. Модуль без runtime-импортов, кроме `.ts`: его читает
// `node --test` (`web/test/taskBoard.test.ts`); `journals.ts` реэкспортирует.
import type { EntryStatus } from './api';
import { PLAN_STEP_FORMS, STEP_FORMS, TASK_FORMS, countText } from './plural.ts';

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

/** Числа по статусам (`entries/counts`). */
export type StatusCounts = Partial<Record<EntryStatus, number>>;

/** Задачи доски и шаги планов внутри них (подзадачи `part_of`). */
export interface StepCounts {
  /** Задачи верхнего уровня (`top_level=1`) — без шагов планов. */
  tasks: number;
  /** Шаги планов: все записи без задач верхнего уровня. */
  steps: number;
  /** Шаги планов по статусам колонок. */
  byStatus: Record<EntryStatus, number>;
}

/**
 * Шаги планов по числам доски: `top` — верхний уровень (`top_level=1`), `all` — все записи с теми
 * же фильтрами; шаги статуса — разность (не меньше 0: два запроса — два снимка базы). Одного из
 * чисел нет — `null`.
 */
export function stepCounts(top: StatusCounts | null, all: StatusCounts | null): StepCounts | null {
  if (!top || !all) return null;
  const byStatus = Object.fromEntries(
    TASK_COLUMNS.map((s) => [s, Math.max(0, (all[s] ?? 0) - (top[s] ?? 0))]),
  ) as Record<EntryStatus, number>;
  const sum = (c: Partial<Record<EntryStatus, number>>) => TASK_COLUMNS.reduce((n, s) => n + (c[s] ?? 0), 0);
  return { tasks: sum(top), steps: sum(byStatus), byStatus };
}

/** Что показывает доска: шаги свёрнуты под общими (по умолчанию), шаги на доске («Подзадачи на
 * доске») или «Из идей» (задачи из идей, и шаги тоже). */
export type BoardMode = 'folded' | 'shown' | 'from_idea';

/** Строка над доской: текст и подпись кнопки-переключателя (пусто — кнопки нет). */
export interface BoardSummary {
  lead: string;
  action: string;
}

/**
 * Строка над доской (задача владельца 2026-11-24: «на вкладке Задачи число 109, а в колонках в
 * сумме 37 — где остальные?»): сумма колонок и число вкладки «Задачи» не расходятся без объяснения.
 * Свёрнуты — «37 задач · 72 шага планов внутри них — показать на доске»; на доске — «37 задач ·
 * показаны и 72 шага планов — свернуть»; «Из идей» — «Из идей — 7 задач и 5 шагов планов» (без
 * кнопки: переключатель с «Из идей» неактивен, что шаги на доске, говорит строка у него). Шагов нет
 * — строки нет (числа и так сходятся), кроме «Из идей»: там строка поясняет фильтр.
 */
export function boardSummary(c: StepCounts | null, mode: BoardMode): BoardSummary | null {
  if (!c) return null;
  const tasks = countText(c.tasks, TASK_FORMS);
  const steps = countText(c.steps, PLAN_STEP_FORMS);
  if (mode === 'from_idea') {
    return { lead: c.steps ? `Из идей — ${tasks} и ${steps}` : `Из идей — ${tasks}`, action: '' };
  }
  if (!c.steps) return null;
  if (mode === 'shown') return { lead: `${tasks} · показаны и ${steps} —`, action: 'свернуть' };
  return { lead: `${tasks} · ${steps} внутри ${c.tasks === 1 ? 'неё' : 'них'} —`, action: 'показать на доске' };
}

/** «+18 шагов» у числа колонки или вкладки, пока шаги свёрнуты; шагов нет — пусто. */
export const stepsTag = (k: number): string => (k > 0 ? `+${countText(k, STEP_FORMS)}` : '');

/** Подсказка к «+K шагов» колонки статуса `status` (подпись статуса — `STATUS_TEXT`). */
export const stepsHint = (k: number, status: string): string =>
  `Ещё ${countText(k, PLAN_STEP_FORMS)} со статусом «${status}» — внутри общих задач, на доске свёрнуты; «Подзадачи на доске» — показать`;

/** Имя вкладки статуса для чтения с экрана — продолжение «Открыт, 33 задачи»: « и 18 шагов
 * планов»; шагов нет — пусто. */
export const stepsName = (k: number): string => (k > 0 ? ` и ${countText(k, PLAN_STEP_FORMS)}` : '');
