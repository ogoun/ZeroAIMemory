// Карточка идеи журнала «Идеи» (M5.7 §10; M6 §16, задача 16): состав кнопок по состоянию идеи,
// разбор формы решения, текст итога, вкладки, поля идеи. Импортирует только `IdeasView` — модуль
// живёт в его ленивом чанке; общее с «Вопросами» — в `ideaForms.ts`. Вёрстка — в компоненте.
// Зависимость времени выполнения одна — `ideaForms.ts` (с расширением: модуль проверяется
// `node --test`), остальное — `import type`.
import type { Entry, EntryStatus, IdeaDecision } from './api';
import { DEFER_FORM, START_SUBMIT, type FormText, type IdeaForm } from './ideaForms.ts';
import type { IdeaPriority } from './journals';

/** Действие кнопки карточки идеи. */
export type IdeaAction = 'start' | 'adopt' | 'defer' | 'decline' | 'reopen' | 'edit';

export interface IdeaButton {
  act: IdeaAction;
  title: string;
  /** Класс вида кнопки (`primary`, `danger`) или пусто. */
  cls: '' | 'primary' | 'danger';
}

/** «Активные» идеи — фильтр журнала по умолчанию (§10); им же доступны «начать» и «отложить». */
export const IDEA_ACTIVE: EntryStatus[] = ['new', 'proposed', 'deferred'];

/** Вкладки журнала по происхождению (§10). */
export type IdeaTab = 'human' | 'agent' | 'all';
export const IDEA_TABS: { id: IdeaTab; title: string }[] = [
  { id: 'human', title: 'Мои' },
  { id: 'agent', title: 'От агента' },
  { id: 'all', title: 'Все' },
];

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

/**
 * Кнопки карточки идеи в порядке показа: `status` идеи, `agent` — идея «от агента» («Начать» нет до
 * «Принять как свою»), `released` — задача принятой идеи снята или удалена ({@link releasedOf}).
 */
export function ideaButtons(status: EntryStatus | null, agent: boolean, released: boolean): IdeaButton[] {
  const active = !!status && IDEA_ACTIVE.includes(status);
  const editable = status === 'new' || status === 'deferred';
  const all: [boolean, IdeaAction, string, IdeaButton['cls']][] = [
    [active && !agent, 'start', 'Начать', 'primary'],
    [agent && editable, 'adopt', 'Принять как свою', 'primary'],
    [active, 'defer', 'Отложить', ''],
    [active || released, 'decline', 'Отказаться', 'danger'],
    [status === 'declined' || status === 'deferred' || released, 'reopen', 'Вернуть', ''],
    [editable, 'edit', 'Изменить', ''],
  ];
  return all.filter((b) => b[0]).map(([, act, title, cls]) => ({ act, title, cls }));
}

/** Подписи форм карточки (`IdeaForm.svelte`). */
export const IDEA_FORMS: Record<IdeaForm, FormText> = {
  create: { aria: 'Новая идея', title: 'Новая идея', submit: 'Записать' },
  edit: { aria: 'Правка идеи', title: 'Изменить идею', submit: 'Сохранить' },
  start: { aria: 'Начать идею', title: 'Начать: где поставить задачу', submit: START_SUBMIT },
  defer: DEFER_FORM,
  decline: { aria: 'Отказаться от идеи', title: 'Отказаться', submit: 'Отказаться' },
  reopen: { aria: 'Вернуть идею', title: 'Вернуть в новые', submit: 'Вернуть' },
};

export interface DecisionFields {
  horizon: string;
  reason: string;
  space: string;
}
export type DecisionOpts = { horizon?: string; reason?: string; space?: string };

/** Заполненная форма карточки (`IdeaForm.svelte`): тело новой идеи, правка или решение. */
export type IdeaSubmit =
  | { create: { title: string; body: string; tags: string[]; data: Record<string, unknown> } }
  | { patch: { title: string; body: string; tags: string[]; data?: Record<string, unknown> } }
  | { decision: IdeaDecision; opts: DecisionOpts };

/** Решение и поля запроса `decide` по форме `mode` — или ошибка заполнения. Дату «отложить»
 * проверяет вызывающий (`deferError` из `ideaForms.ts`). */
export function decisionOf(
  mode: 'start' | 'defer' | 'decline' | 'reopen',
  f: DecisionFields,
): { error: string } | { decision: IdeaDecision; opts: DecisionOpts } {
  const reason = f.reason.trim();
  if (mode === 'start') return { decision: 'accept', opts: { space: f.space } };
  if (mode === 'defer') return { decision: 'defer', opts: { horizon: f.horizon } };
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
