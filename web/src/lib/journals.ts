// Подписи журналов решения для WebUI (§7.2–7.6): виды записей, статусы, надёжность.
import type { Certainty, EntryStatus, TriggerType } from './api';
import type { Journal } from './router';

export const JOURNAL_TITLE: Record<Journal, string> = {
  changelog: 'Changelog',
  releases: 'Releases',
  insights: 'Insights',
  glossary: 'Словарь',
  tasks: 'Задачи',
  questions: 'Вопросы',
  vault: 'Vault',
};

/** Журналы пространства: у решения — все, у Knowledge — без Changelog и Releases, иначе — нет. */
export function journalsOf(kind: string): Journal[] {
  if (kind === 'solution') return ['changelog', 'releases', 'insights', 'glossary', 'tasks', 'questions', 'vault'];
  if (kind === 'knowledge') return ['insights', 'glossary', 'tasks', 'questions', 'vault'];
  return [];
}

/** Вид записи журнала → журнал (переход из поиска). */
export const JOURNAL_OF_KIND: Record<string, Journal> = {
  insight: 'insights',
  changelog: 'changelog',
  release: 'releases',
  term: 'glossary',
  task: 'tasks',
  question: 'questions',
};

/** Буква ссылки записи по виду (`i:`, `t:`, … — как в `/read` и индексе ссылок на файлы). */
const REF_LETTER: Record<string, string> = {
  insight: 'i',
  changelog: 'c',
  release: 'r',
  term: 't',
  task: 'k',
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
};

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

export const fmtTime = (ts: string | null | undefined): string => {
  if (!ts) return '';
  const d = new Date(ts);
  return Number.isNaN(d.getTime())
    ? ts
    : d.toLocaleString('ru-RU', { dateStyle: 'short', timeStyle: 'short' });
};
