// Подписи журналов решения для WebUI (§7.2–7.6): виды записей, статусы, надёжность.
import { localTime } from './time';
import type { Certainty, EntryStatus, TriggerType } from './api';
import type { Journal } from './router';

export const JOURNAL_TITLE: Record<Journal, string> = {
  changelog: 'Changelog',
  releases: 'Releases',
  reports: 'Отчёты',
  code: 'Код',
  insights: 'Insights',
  glossary: 'Словарь',
  tasks: 'Задачи',
  ideas: 'Идеи',
  questions: 'Вопросы',
  vault: 'Vault',
};

/** Журналы пространства: у решения — все, у Knowledge — без Changelog, Releases, Отчётов
 * (отчёты пишутся только в решение, M5.5) и Кода (карта кода — у решения, M5.6), иначе — нет.
 * «Идеи» (M5.7) — после «Задач» у обоих. */
export function journalsOf(kind: string): Journal[] {
  if (kind === 'solution')
    return ['changelog', 'releases', 'reports', 'code', 'insights', 'glossary', 'tasks', 'ideas', 'questions', 'vault'];
  if (kind === 'knowledge') return ['insights', 'glossary', 'tasks', 'ideas', 'questions', 'vault'];
  return [];
}

/** Вид записи журнала → журнал (переход из поиска). */
export const JOURNAL_OF_KIND: Record<string, Journal> = {
  insight: 'insights',
  changelog: 'changelog',
  release: 'releases',
  report: 'reports',
  term: 'glossary',
  task: 'tasks',
  idea: 'ideas',
  question: 'questions',
};

/** Буква ссылки записи по виду (`i:`, `t:`, … — как в `/read` и индексе ссылок на файлы). */
const REF_LETTER: Record<string, string> = {
  insight: 'i',
  changelog: 'c',
  release: 'r',
  report: 'p',
  term: 't',
  task: 'k',
  idea: 'w',
  question: 'q',
};
export const entryRef = (kind: string, id: string): string => `${REF_LETTER[kind] ?? 'e'}:${id}`;

/** Колонки доски задач (M4.5 §3) и подписи. */
export const TASK_COLUMNS: EntryStatus[] = ['open', 'in_progress', 'blocked', 'done', 'dropped'];

/** `data.decision` (ADR, M4.5 §5.1) insight `decision`: поля формы. */
export interface Adr {
  context: string;
  options: string[];
  chosen: string;
  rationale: string;
  consequences: string;
  decided_by: '' | 'human' | 'agent';
  revisit_if: string;
}

export const ADR_TEXT: Record<'context' | 'options' | 'chosen' | 'rationale' | 'consequences' | 'decided_by' | 'revisit_if', string> = {
  context: 'Контекст',
  options: 'Варианты',
  chosen: 'Выбрано',
  rationale: 'Обоснование',
  consequences: 'Последствия',
  decided_by: 'Кто решил',
  revisit_if: 'Пересмотреть, если',
};

export const DECIDED_BY_TEXT: Record<string, string> = { human: 'человек', agent: 'агент' };

/** ADR из `data.decision` записи (пустые поля — пустые строки). */
export function adrOf(data: Record<string, unknown>): Adr {
  const d = data.decision && typeof data.decision === 'object' ? (data.decision as Record<string, unknown>) : {};
  const s = (k: string) => (typeof d[k] === 'string' ? (d[k] as string) : '');
  const by = s('decided_by');
  return {
    context: s('context'),
    options: Array.isArray(d.options) ? d.options.filter((x): x is string => typeof x === 'string') : [],
    chosen: s('chosen'),
    rationale: s('rationale'),
    consequences: s('consequences'),
    decided_by: by === 'human' || by === 'agent' ? by : '',
    revisit_if: s('revisit_if'),
  };
}

/** `data.decision` для запроса: пустые поля не отправляются. */
export function adrToData(a: Adr): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const k of ['context', 'chosen', 'rationale', 'consequences', 'revisit_if'] as const) {
    const v = a[k].trim();
    if (v) out[k] = v;
  }
  const opts = a.options.map((x) => x.trim()).filter(Boolean);
  if (opts.length) out.options = opts;
  if (a.decided_by) out.decided_by = a.decided_by;
  return out;
}

export const adrIsEmpty = (a: Adr): boolean => Object.keys(adrToData(a)).length === 0;

/** `data.kind` записи changelog (crates/zam-server/src/api/entries.rs, CHANGELOG_KINDS). */
export const CHANGELOG_KINDS = ['feat', 'fix', 'refactor', 'docs', 'research', 'ops', 'other'];

/** `data.kind` insight (crates/zam-server/src/api/insights.rs, INSIGHT_KINDS). */
export const INSIGHT_KINDS = ['gotcha', 'failure', 'discovery', 'decision', 'experiment', 'pattern'];

export const CERTAINTIES: Certainty[] = ['axiom', 'theorem', 'hypothesis'];

export const CERTAINTY_TEXT: Record<Certainty, string> = {
  axiom: 'аксиома — принято человеком как истина',
  theorem: 'теорема — проверено, есть доказательство',
  hypothesis: 'гипотеза — не проверено',
};

/** Статусы insight (фильтр списка Insights). */
export const STATUSES: EntryStatus[] = ['active', 'revisit', 'superseded', 'retracted'];

export const STATUS_TEXT: Record<EntryStatus, string> = {
  active: 'действует',
  revisit: 'на пересмотре',
  superseded: 'заменён',
  retracted: 'отозван',
  open: 'открыт',
  in_progress: 'в работе',
  blocked: 'заблокирован',
  done: 'выполнен',
  dropped: 'снят',
  answered: 'отвечен',
  obsolete: 'неактуален',
  // статусы идеи (M5.7) — для поиска Ctrl+K; в журнале «Идеи» — IDEA_STATUS_TEXT
  new: 'новая',
  proposed: 'предложена',
  accepted: 'начата',
  deferred: 'отложена',
  declined: 'отклонена',
};

/** Статусы идеи (M5.7 §5) в порядке фильтра журнала «Идеи». */
export const IDEA_STATUSES: EntryStatus[] = ['new', 'proposed', 'deferred', 'accepted', 'done', 'declined'];
/** «Активные» идеи — фильтр журнала по умолчанию (§10). */
export const IDEA_ACTIVE: EntryStatus[] = ['new', 'proposed', 'deferred'];

export type IdeaPriority = 'low' | 'normal' | 'high';
export const PRIORITIES: IdeaPriority[] = ['high', 'normal', 'low'];
export const PRIORITY_TEXT: Record<IdeaPriority, string> = { low: 'низкий', normal: 'обычный', high: 'высокий' };

/** Подписи статусов идеи (женский род: «идея»); STATUS_TEXT общий — для задач и вопросов. */
export const IDEA_STATUS_TEXT: Record<'new' | 'proposed' | 'accepted' | 'deferred' | 'declined' | 'done', string> = {
  new: 'новая',
  proposed: 'предложена',
  accepted: 'начата',
  deferred: 'отложена',
  declined: 'отклонена',
  done: 'выполнена',
};

/** Подпись статуса идеи; не статус идеи — как есть, `null` — пусто. */
export const ideaStatusText = (s: EntryStatus | null): string =>
  s && s in IDEA_STATUS_TEXT ? IDEA_STATUS_TEXT[s as keyof typeof IDEA_STATUS_TEXT] : (s ?? '');

/** Вкладки журнала «Идеи» по происхождению (§10). */
export type IdeaTab = 'human' | 'agent' | 'all';
export const IDEA_TABS: { id: IdeaTab; title: string }[] = [
  { id: 'human', title: 'Мои' },
  { id: 'agent', title: 'От агента' },
  { id: 'all', title: 'Все' },
];

/** Фильтр статуса журнала «Идеи»: подпись и статусы запроса (пусто — все). */
export type IdeaStatusSel = 'active' | 'accepted' | 'done' | 'declined' | 'all';
export const IDEA_STATUS_SEL: { id: IdeaStatusSel; title: string; statuses: EntryStatus[] }[] = [
  { id: 'active', title: 'Активные', statuses: IDEA_ACTIVE },
  { id: 'accepted', title: IDEA_STATUS_TEXT.accepted, statuses: ['accepted'] },
  { id: 'done', title: IDEA_STATUS_TEXT.done, statuses: ['done'] },
  { id: 'declined', title: IDEA_STATUS_TEXT.declined, statuses: ['declined'] },
  { id: 'all', title: 'все', statuses: [] },
];

/** Дата `YYYY-MM-DD` по UTC через `days` дней от сегодняшней (сервер сравнивает `horizon` с
 * сегодняшней датой UTC: «отложить до» — не раньше завтрашней по UTC). */
export const utcDate = (days: number, now = Date.now()): string =>
  new Date(now + days * 86_400_000).toISOString().slice(0, 10);

export const TRIGGER_TYPES: TriggerType[] = ['file_glob', 'command_regex', 'error_regex', 'keyword'];

export const TRIGGER_TEXT: Record<TriggerType, string> = {
  file_glob: 'файлы (glob)',
  command_regex: 'команда (regex)',
  error_regex: 'ошибка (regex)',
  keyword: 'ключевое слово',
};

export const AUTHOR_TEXT: Record<string, string> = {
  human: 'человек',
  agent: 'агент',
  system: 'система',
};

/** Строковое поле `data` записи или пустая строка. */
export const dataStr = (data: Record<string, unknown>, key: string): string => {
  const v = data[key];
  return typeof v === 'string' ? v : '';
};

/** Момент API (RFC 3339 UTC) в локальном времени браузера, коротко; пусто — пустая строка. */
export const fmtTime = (ts: string | null | undefined): string => (ts ? localTime(ts) : '');
