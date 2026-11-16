// Карточка и лента журнала «Идеи» (M5.7 §10; M6 §16, задача 16; M6.5 §9): состав кнопок по
// состоянию идеи, разбор формы решения, текст итога, вкладки, чипы статусов и разделы
// «Группировать по статусу», разбор агента (строка решения ленты, ссылки «Опирался на»), поля идеи.
// Импортирует только `IdeasView` — модуль живёт в его ленивом чанке; общее с «Вопросами» — в
// `ideaForms.ts`. Вёрстка — в компоненте. Зависимости времени выполнения — `ideaForms.ts` и
// `ideaStatus.ts` (с расширением: модуль проверяется `node --test`), остальное — `import type`.
import type { Entry, EntryStatus, IdeaDecision, IdeaReview, ReviewOutcome } from './api';
import { DEFER_FORM, START_SUBMIT, type FormText, type IdeaForm } from './ideaForms.ts';
import { IDEA_STATUSES, IDEA_STATUS_TEXT, type IdeaStatus } from './ideaStatus.ts';
import type { IdeaPriority } from './journals';
import type { Journal } from './router';

/** Действие кнопки карточки идеи; `delete` — в корзину (у отклонённой, M6.5 §6). */
export type IdeaAction = 'start' | 'adopt' | 'defer' | 'decline' | 'reopen' | 'edit' | 'delete';

export interface IdeaButton {
  act: IdeaAction;
  title: string;
  /** Класс вида кнопки (`primary`, `danger`) или пусто. */
  cls: '' | 'primary' | 'danger';
}

/** «Активные» идеи — фильтр журнала по умолчанию (§10; M6.5 §4 — и «На доработку»); им же
 * доступны «начать», «отложить» и «отказаться». */
export const IDEA_ACTIVE: EntryStatus[] = ['new', 'returned', 'proposed', 'deferred'];

/** Чип статусов ленты (M6.5 §9): «Активные» (по умолчанию), статусы идеи по порядку, «Все». */
export type IdeaChip = 'active' | IdeaStatus | 'all';
export const IDEA_CHIPS: { id: IdeaChip; statuses: EntryStatus[] }[] = [
  { id: 'active', statuses: IDEA_ACTIVE },
  ...IDEA_STATUSES.map((s) => ({ id: s, statuses: [s] as EntryStatus[] })),
  { id: 'all', statuses: [] },
];

/** Счётчик чипа по ответу `entries/counts`: сумма его статусов («Все» — всех статусов идеи). */
export function chipCount(chip: IdeaChip, counts: Partial<Record<EntryStatus, number>>): number {
  const of = (ss: readonly EntryStatus[]) => ss.reduce((n, s) => n + (counts[s] ?? 0), 0);
  if (chip === 'all') return of(IDEA_STATUSES);
  return of(IDEA_CHIPS.find((c) => c.id === chip)?.statuses ?? []);
}

/** Разделы «Группировать по статусу» — в порядке чипов, пустых нет; неизвестный статус — в конце. */
export function groupByStatus<T extends { status?: EntryStatus | null }>(items: T[]): { status: string; items: T[] }[] {
  const order: string[] = IDEA_STATUSES;
  const groups = new Map<string, T[]>();
  for (const it of items) {
    const k = it.status ?? '';
    const g = groups.get(k);
    if (g) g.push(it);
    else groups.set(k, [it]);
  }
  const rank = (k: string) => (order.includes(k) ? order.indexOf(k) : order.length);
  return [...groups.entries()].sort(([a], [b]) => rank(a) - rank(b)).map(([status, items]) => ({ status, items }));
}

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
 * Возвращённую (M6.5 §6) человек дописывает — «Изменить» главное действие — или решает сам
 * («Начать», «Отложить», «Отказаться», §4, §6; «Вернуть» из неё нет — на разбор её возвращает
 * правка); отклонённую можно вернуть на разбор или удалить в корзину.
 */
export function ideaButtons(status: EntryStatus | null, agent: boolean, released: boolean): IdeaButton[] {
  const active = !!status && IDEA_ACTIVE.includes(status);
  const returned = status === 'returned';
  const editable = status === 'new' || status === 'deferred';
  const all: [boolean, IdeaAction, string, IdeaButton['cls']][] = [
    [returned, 'edit', 'Изменить', 'primary'],
    [active && !agent, 'start', 'Начать', returned ? '' : 'primary'],
    [agent && editable, 'adopt', 'Принять как свою', 'primary'],
    [active, 'defer', 'Отложить', ''],
    [active || released, 'decline', 'Отказаться', 'danger'],
    [status === 'declined' || status === 'deferred' || released, 'reopen', 'Вернуть на разбор', ''],
    [editable, 'edit', 'Изменить', ''],
    [status === 'declined', 'delete', 'Удалить', 'danger'],
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
  reopen: { aria: 'Вернуть идею', title: 'Вернуть на разбор', submit: 'Вернуть' },
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
        : 'Идея возвращена на разбор.';

/** Итог правки идеи: возвращённую человек дописал, и сервер вернул её в «Непросмотрено» —
 * «Отправлено на разбор» (M6.5 §6); иначе — «Идея сохранена». */
export const editNotice = (before: EntryStatus | null, after: EntryStatus | null): string =>
  before === 'returned' && after === 'new' ? 'Отправлено на разбор.' : 'Идея сохранена.';

// ---- разбор агента (M6.5 §5, §9) ----

/** Подписи исходов разбора. */
export const REVIEW_OUTCOME_TEXT: Record<ReviewOutcome, string> = {
  plan: 'В план',
  return: 'На доработку',
  decline: 'Отклонена',
  done: 'Сделано',
};

/** Статусы идеи, которые даёт исход (`plan` — и `done`: задача плана выполнена). */
const OUTCOME_STATUSES: Record<ReviewOutcome, EntryStatus[]> = {
  plan: ['accepted', 'done'],
  return: ['returned'],
  decline: ['declined'],
  done: ['done'],
};

const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);

/** Разбор из JSON (`data.review`, элемент `data.reviews`) или `null` — не разбор. */
function parseReview(v: unknown): IdeaReview | null {
  if (!v || typeof v !== 'object') return null;
  const o = v as Record<string, unknown>;
  const outcome = o.outcome;
  if (outcome !== 'plan' && outcome !== 'return' && outcome !== 'decline' && outcome !== 'done') return null;
  const s = (k: string) => (typeof o[k] === 'string' ? (o[k] as string) : undefined);
  return {
    outcome,
    summary: s('summary') ?? '',
    missing: s('missing'),
    basis: strs(o.basis),
    similar: strs(o.similar),
    task: s('task'),
    by_user: s('by_user') ?? '',
    session: s('session'),
    at: s('at') ?? '',
  };
}

/** Последний разбор агента (`data.review`) или `null`. */
export const reviewOf = (e: Entry): IdeaReview | null => parseReview(e.data.review);

/** Прежние разборы (`data.reviews` без текущего), новые первыми. */
export function olderReviews(e: Entry): IdeaReview[] {
  const raw = Array.isArray(e.data.reviews) ? (e.data.reviews as unknown[]) : [];
  const all = raw.map(parseReview).filter((r): r is IdeaReview => r !== null);
  const cur = reviewOf(e);
  return cur && all.length && all[0].at === cur.at && all[0].outcome === cur.outcome ? all.slice(1) : all;
}

/** Последний разбор определяет нынешнее состояние идеи: статус — тот, что дал исход, и после
 * разбора человек не решал иначе (`data.decided` не разбора и позже него). */
export function reviewCurrent(e: Entry): boolean {
  const r = reviewOf(e);
  if (!r || !e.status || !OUTCOME_STATUSES[r.outcome].includes(e.status)) return false;
  const d = e.data.decided;
  if (!d || typeof d !== 'object') return true;
  const o = d as Record<string, unknown>;
  const at = typeof o.at === 'string' ? Date.parse(o.at) : NaN;
  const rat = Date.parse(r.at);
  return o.channel === 'review' || Number.isNaN(at) || Number.isNaN(rat) || at <= rat;
}

/** Пояснение в карточке, когда последний разбор уже не определяет состояние идеи: статус после
 * него другой (возвращённую дописали, отклонённую вернули на разбор) или решал человек. */
export function reviewNote(e: Entry): string {
  if (!reviewOf(e) || reviewCurrent(e)) return '';
  const label = (IDEA_STATUS_TEXT as Record<string, string>)[e.status ?? ''] ?? e.status ?? '';
  const r = reviewOf(e)!;
  return e.status && OUTCOME_STATUSES[r.outcome].includes(e.status)
    ? 'После разбора решение принял человек.'
    : `После разбора статус изменился: ${label}.`;
}

/** Текст в одну строку не длиннее `n` символов (длиннее — обрезка с «…»). */
export function clip(s: string, n: number): string {
  const t = Array.from(s.replace(/\s+/g, ' ').trim());
  return t.length <= n ? t.join('') : `${t.slice(0, n).join('').trimEnd()}…`;
}

/** Ссылки на сделанное — для строки «Сделано: …» (как правило сервера у исхода `done`). */
const DONE_PREFIXES = ['c:', 'r:', 'commit:', 'k:'];

/**
 * Строка решения ленты (M6.5 §9): «В план → k:<id> «…»» (заголовок задачи, если известен) |
 * «Не хватает: <60 символов>» | «Отклонена: <60 символов разбора>» | «Сделано: <ссылка>». Пусто —
 * разбора нет или он уже не определяет состояние идеи ({@link reviewCurrent}).
 */
export function reviewLine(e: Entry): string {
  const r = reviewOf(e);
  if (!r || !reviewCurrent(e)) return '';
  switch (r.outcome) {
    case 'plan': {
      const ref = r.task ?? (typeof e.data.task_id === 'string' ? `k:${e.data.task_id}` : '');
      const title = e.task && !e.task.deleted ? (e.task.title ?? '') : '';
      return `В план → ${ref}${title ? ` «${clip(title, 60)}»` : ''}`.trimEnd();
    }
    case 'return':
      return `Не хватает: ${clip(r.missing ?? '', 60)}`;
    case 'decline':
      return `Отклонена: ${clip(r.summary, 60)}`;
    case 'done': {
      const ref = r.basis.find((b) => DONE_PREFIXES.some((p) => b.startsWith(p))) ?? r.basis[0] ?? '';
      return ref ? `Сделано: ${ref}` : 'Сделано';
    }
  }
}

/** Буква ссылки записи → журнал (буквы — как `REF_LETTER` в `journals.ts`; `n:` — заметка). */
const REF_JOURNAL: Record<string, Journal> = {
  i: 'insights',
  c: 'changelog',
  r: 'releases',
  p: 'reports',
  t: 'glossary',
  k: 'tasks',
  q: 'questions',
  w: 'ideas',
};

/** Куда ведёт ссылка разбора (`basis`, `similar`): журнал записи, заметка, текст для копирования. */
export type RefTarget = { journal: Journal; id: string } | { node: string } | { copy: string } | null;

/** Ссылка разбора → журнал записи (`k:`, `i:`, …) или заметка (`n:`); `file:` и `commit:` — текст
 * для «копировать» (путь или sha без префикса); иное — `null` (показывается как есть). */
export function refTarget(ref: string): RefTarget {
  const i = ref.indexOf(':');
  if (i < 0) return null;
  const p = ref.slice(0, i);
  const rest = ref.slice(i + 1).trim();
  if (!rest) return null;
  if (p === 'file' || p === 'commit') return { copy: rest };
  if (!/^[0-9A-Za-z]{1,64}$/.test(rest)) return null;
  if (p === 'n') return { node: rest };
  const journal = REF_JOURNAL[p];
  return journal ? { journal, id: rest } : null;
}
