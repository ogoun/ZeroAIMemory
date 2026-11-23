// Подзадачи задач в WebUI (M7.2, спецификация §5.5; поля сервера — §5.4 п. 2): прогресс общей
// задачи «8/13» по правилу `done/(total − dropped)` — снятые в знаменатель не входят (решение
// координатора 21), та же функция — для сводки родителя у подзадачи (`parent.subtasks`, «(8/13)»);
// текст плашки «давно без обновлений» (`stale` сервера). Модуль без runtime-импортов: его читает
// `node --test` (`web/test/taskProgress.test.ts`).
import type { SubtaskSummary } from './api';

/** Поля сводки, из которых считается прогресс: `subtasks` общей или `parent.subtasks` подзадачи. */
export type ProgressOf = Pick<SubtaskSummary, 'total' | 'done' | 'dropped'>;

/** Прогресс для полосы (`role="progressbar"`): выполнено `now` из `max` — подзадач без снятых.
 * Подзадач нет или все сняты — `{now: 0, max: 0}` (полосы и счётчика нет). */
export function progressValue(s: ProgressOf | null | undefined): { now: number; max: number } {
  if (!s) return { now: 0, max: 0 };
  const max = Math.max(0, s.total - s.dropped);
  return { now: Math.min(Math.max(0, s.done), max), max };
}

/** Счётчик «8/13»; подзадач нет или все сняты — пустая строка (счётчик не показывается). */
export function progressText(s: ProgressOf | null | undefined): string {
  const { now, max } = progressValue(s);
  return max > 0 ? `${now}/${max}` : '';
}

const HOUR_MS = 3_600_000;

/** Сколько без обновлений: целые часы вниз — «14 ч», от суток — «1 сут. 1 ч», «2 сут.»; меньше
 * часа (и отрицательное — часы браузера отстают от сервера) — «меньше 1 ч». */
export function idleText(ms: number): string {
  const h = Math.floor(ms / HOUR_MS);
  if (!(h >= 1)) return 'меньше 1 ч';
  if (h < 24) return `${h} ч`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} сут. ${h % 24} ч` : `${d} сут.`;
}

/** Плашка «давно без обновлений»: «в работе, без обновлений 14 ч» — время от `last_activity_at`
 * (сервер: наибольшее `updated_at` задачи и её прямых подзадач) до `nowMs` (часы браузера). Не
 * момент — без числа. */
export function staleText(lastActivityIso: string, nowMs: number): string {
  const t = Date.parse(lastActivityIso);
  return Number.isNaN(t) ? 'в работе, без обновлений' : `в работе, без обновлений ${idleText(nowMs - t)}`;
}
