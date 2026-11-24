// Подписи журналов решения для WebUI (§7.2–7.6): виды записей, статусы, надёжность.
import { IDEA_STATUS_TEXT } from './ideaStatus';
import { localTime } from './time';
import type { Certainty, EntryKind, EntryStatus, TriggerType } from './api';
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

/** Заголовок формы правки записи (`EntryEdit` — в карточке и в окне `EditPanel`). */
export const EDIT_TITLE: Partial<Record<EntryKind, string>> = {
  changelog: 'Правка записи',
  release: 'Правка релиза',
  task: 'Правка задачи',
  question: 'Правка вопроса',
};

/** Журналы пространства: у решения — все, у Knowledge — без Changelog, Releases, Отчётов
 * (отчёты пишутся только в решение, M5.5) и Кода (карта кода — у решения, M5.6), иначе — нет.
 * «Идеи» (M5.7) — после «Задач» у обоих. Те же списки считает сервер (`GET /spaces/{space}/counts`,
 * `crates/zam-store/src/counts.rs`; модульный тест `journals_match_webui_source` читает их отсюда). */
export function journalsOf(kind: string): Journal[] {
  if (kind === 'solution')
    return ['changelog', 'releases', 'reports', 'code', 'insights', 'glossary', 'tasks', 'ideas', 'questions', 'vault'];
  if (kind === 'knowledge') return ['insights', 'glossary', 'tasks', 'ideas', 'questions', 'vault'];
  return [];
}

/** Вид записи → журнал, буква ссылки и `entryRef` — в `entryRefs.ts` (без зависимостей: его
 * импортирует и `ideaButtons.ts`, проверяемый `node --test`). */
export { JOURNAL_OF_KIND, entryRef } from './entryRefs';

/** Колонки доски задач (M4.5 §3) и вкладка по умолчанию в узкой области (M7.2, §5.2 п. 1) — в
 * `taskBoard.ts` (без зависимостей: его проверяет `node --test`). */
export { TASK_COLUMNS, boardTab } from './taskBoard';

/** Подпись статуса с заглавной буквы: вкладки доски («В работе»). */
export const capitalized = (s: string): string => (s ? s[0].toUpperCase() + s.slice(1) : s);

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
  // статусы идеи (M5.7, M6.5 §4) — строчными, как прочие; подписи журнала «Идеи» и Ctrl+K —
  // IDEA_STATUS_TEXT (entryStatusText)
  new: 'непросмотрено',
  proposed: 'предложена',
  accepted: 'в плане',
  deferred: 'отложена',
  declined: 'отклонена',
  returned: 'на доработку',
};

/** Статусы идеи и подписи (M5.7 §5, M6.5 §4): порядок чипов и разделов журнала «Идеи». */
export { IDEA_STATUSES, IDEA_STATUS_TEXT, type IdeaStatus } from './ideaStatus';

export type IdeaPriority = 'low' | 'normal' | 'high';
export const PRIORITIES: IdeaPriority[] = ['high', 'normal', 'low'];
export const PRIORITY_TEXT: Record<IdeaPriority, string> = { low: 'низкий', normal: 'обычный', high: 'высокий' };

/** Подпись статуса записи вида `kind`: у идеи — IDEA_STATUS_TEXT, у прочих — STATUS_TEXT. */
export const entryStatusText = (kind: string | null | undefined, s: EntryStatus): string =>
  (kind === 'idea' ? (IDEA_STATUS_TEXT as Record<string, string>)[s] : undefined) ?? STATUS_TEXT[s] ?? s;

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
