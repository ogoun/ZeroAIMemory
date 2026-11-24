// Точки внимания шапки (M7.3, спецификация §6.9 пп. 1–2, §6.2 п. 5): «Проверка» — с точкой при
// открытой регрессии ночной проверки (последний законченный ночной прогон — `regression`, норма не
// принята: правило строки дайджеста сервера, `check::digest_line`);
// «Внутренние работы» (меню аккаунта) — с точкой, пока исполнитель на паузе. Одно состояние на
// страницу: оболочка читает его в разметке, страницы «Проверка памяти» и «Внутренние работы»
// обновляют его своими свежими данными (принятая норма, подтверждённый адрес снимают точку сразу).
// Загрузка — при входе (`refresh`) и при возврате на вкладку не чаще раза в 30 с; опроса по
// таймеру нет. Отказ запроса — без точки: это подсказка, а не страница.
import { call, type UserSettings } from './api';

/** Сведения для точек; `null` — ещё не известно. */
export const attention = $state<{ regression: boolean | null; pause: string | null }>({ regression: null, pause: null });

const REFRESH_MS = 30_000;
/** Прогонов в чтении: ночной среди ручных (`GET /check/runs`, новые первыми). */
export const RUNS = 60;
let at = 0;
let inflight = false;

/** Перечитать: статус проверки и паузу исполнителя своего пользователя. */
export async function refreshAttention(): Promise<void> {
  if (inflight) return;
  inflight = true;
  at = Date.now();
  try {
    type Run = { trigger: string; status: string; verdict: { kind: string } | null; accepted_at: string | null };
    const [runs, set] = await Promise.allSettled([
      call<{ items: Run[] }>('GET', `check/runs?limit=${RUNS}`),
      call<UserSettings>('GET', 'auth/settings'),
    ]);
    if (runs.status === 'fulfilled') {
      // то же правило, что `latestNightly` и `regressionOpen` (`checkVerdict.ts`): модуль страницы в
      // стартовую загрузку не тянется
      const n = runs.value.items.find((r) => r.trigger === 'nightly' && r.status === 'done');
      attention.regression = !!n && n.verdict?.kind === 'regression' && !n.accepted_at;
    }
    if (set.status === 'fulfilled') attention.pause = set.value.works?.pause?.class ?? null;
  } finally {
    inflight = false;
  }
}

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && at && Date.now() - at >= REFRESH_MS) void refreshAttention();
  });
}
