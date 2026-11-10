// Кнопки и формы решения по идее (M5.7 §10; M6 §16, задача 16) — общие для журнала «Идеи» и
// вопроса-предложения в «Вопросах»: состав кнопок карточки по состоянию идеи, подписи форм,
// проверка формы, текст итога, дата «отложить» (UTC). Вёрстка — в компонентах. Модуль без
// зависимостей времени выполнения (типы — только `import type`): проверяется `node --test`.
import type { Entry, EntryStatus, IdeaDecision } from './api';
import type { IdeaPriority } from './journals';

/** Действие кнопки карточки идеи. */
export type IdeaAction = 'start' | 'adopt' | 'defer' | 'decline' | 'reopen' | 'edit';
/** Форма карточки: создание, правка и решения с полями. */
export type IdeaForm = 'create' | 'edit' | 'start' | 'defer' | 'decline' | 'reopen';

export interface IdeaButton {
  act: IdeaAction;
  title: string;
  /** Класс вида кнопки (`primary`, `danger`) или пусто. */
  cls: '' | 'primary' | 'danger';
}

const str = (d: Record<string, unknown>, k: string): string => (typeof d[k] === 'string' ? (d[k] as string) : '');

/** Происхождение идеи (`data.origin`): всё, кроме `agent`, — человек. */
export const originOf = (e: Entry): 'agent' | 'human' => (str(e.data, 'origin') === 'agent' ? 'agent' : 'human');

/** Приоритет идеи (`data.priority`), неизвестный — обычный. */
export const priorityOf = (e: Entry): IdeaPriority => {
  const p = str(e.data, 'priority');
  return p === 'low' || p === 'high' ? p : 'normal';
};

/** Задача «начать» снята или удалена: идея `accepted` без живой задачи (§5); нет `task` — как
 * удалена, по правилу сервера `idea_task_state` (ревью задачи 7 M5.7, M5). */
export const releasedOf = (e: Entry): '' | 'снята' | 'удалена' =>
  e.status !== 'accepted' ? '' : !e.task || e.task.deleted ? 'удалена' : e.task.status === 'dropped' ? 'снята' : '';

/** Теги из строки через запятую. */
export const tagsOf = (s: string): string[] =>
  s
    .split(',')
    .map((t) => t.trim())
    .filter(Boolean);

const ACTIVE: ReadonlySet<EntryStatus | null> = new Set<EntryStatus>(['new', 'proposed', 'deferred']);
const EDITABLE = (s: EntryStatus | null) => s === 'new' || s === 'deferred';

/**
 * Кнопки карточки идеи в порядке показа: `status` идеи, `agent` — идея «от агента» («Начать» нет до
 * «Принять как свою»), `released` — задача принятой идеи снята или удалена ({@link releasedOf}).
 */
export function ideaButtons(status: EntryStatus | null, agent: boolean, released: boolean): IdeaButton[] {
  const active = ACTIVE.has(status);
  const all: [boolean, IdeaAction, string, IdeaButton['cls']][] = [
    [active && !agent, 'start', 'Начать', 'primary'],
    [agent && EDITABLE(status), 'adopt', 'Принять как свою', 'primary'],
    [active, 'defer', 'Отложить', ''],
    [active || released, 'decline', 'Отказаться', 'danger'],
    [status === 'declined' || status === 'deferred' || released, 'reopen', 'Вернуть', ''],
    [EDITABLE(status), 'edit', 'Изменить', ''],
  ];
  return all.filter((b) => b[0]).map(([, act, title, cls]) => ({ act, title, cls }));
}

/** Подписи форм карточки: имя формы (ARIA), заголовок и кнопка отправки. */
export const IDEA_FORMS: Record<IdeaForm, { aria: string; title: string; submit: string }> = {
  create: { aria: 'Новая идея', title: 'Новая идея', submit: 'Записать' },
  edit: { aria: 'Правка идеи', title: 'Изменить идею', submit: 'Сохранить' },
  start: { aria: 'Начать идею', title: 'Начать: где поставить задачу', submit: 'Начать' },
  defer: { aria: 'Отложить идею', title: 'Отложить', submit: 'Отложить' },
  decline: { aria: 'Отказаться от идеи', title: 'Отказаться', submit: 'Отказаться' },
  reopen: { aria: 'Вернуть идею', title: 'Вернуть в новые', submit: 'Вернуть' },
};

/** Дата `YYYY-MM-DD` по UTC через `days` дней (как `utcDate` журналов). */
const utcDay = (days: number, now: number): string => new Date(now + days * 86_400_000).toISOString().slice(0, 10);

/** «Отложить до» по умолчанию — через 30 дней (UTC). */
export const deferDefault = (now = Date.now()): string => utcDay(30, now);
/** Наименьшая дата «отложить до» — завтра по UTC (сервер сравнивает `horizon` с сегодняшней). */
export const deferMin = (now = Date.now()): string => utcDay(1, now);
/** Ошибка даты «отложить до» или пустая строка. */
export const deferError = (horizon: string, now = Date.now()): string =>
  !horizon || horizon < deferMin(now) ? `Дата — не раньше ${deferMin(now)} (UTC).` : '';

/** Предупреждение «начать» идеи Knowledge с задачей в общем решении (§5.2, п. 1). */
export const sharedWarning = (name: string): string => `Задача появится в решении «${name}», её увидят участники.`;

export interface DecisionFields {
  horizon: string;
  reason: string;
  space: string;
}
export type DecisionOpts = { horizon?: string; reason?: string; space?: string };

/** Решение и поля запроса `decide` по форме `mode` — или ошибка заполнения. */
export function decisionOf(
  mode: 'start' | 'defer' | 'decline' | 'reopen',
  f: DecisionFields,
  now = Date.now(),
): { error: string } | { decision: IdeaDecision; opts: DecisionOpts } {
  const reason = f.reason.trim();
  if (mode === 'start') return { decision: 'accept', opts: { space: f.space } };
  if (mode === 'defer') {
    const error = deferError(f.horizon, now);
    return error ? { error } : { decision: 'defer', opts: { horizon: f.horizon } };
  }
  if (mode === 'reopen' && !reason) return { error: 'Укажите причину.' };
  return { decision: mode, opts: reason ? { reason } : {} };
}

/** Текст итога решения: `taskRef` — ссылка `k:<id>` созданной задачи, `horizon` — дата «отложить». */
export const decisionNotice = (decision: IdeaDecision, taskRef: string, horizon?: string): string =>
  decision === 'accept'
    ? `Идея начата: задача ${taskRef}.`
    : decision === 'defer'
      ? `Идея отложена до ${horizon} (UTC).`
      : decision === 'decline'
        ? 'Отказ записан.'
        : 'Идея возвращена в новые.';
