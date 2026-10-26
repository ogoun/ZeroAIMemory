// Подписи журналов решения для WebUI (§7.2–7.6): виды записей, статусы, надёжность.
import type { Certainty, EntryStatus, TriggerType } from './api';
import type { Journal } from './router';

export const JOURNAL_TITLE: Record<Journal, string> = {
  changelog: 'Changelog',
  releases: 'Releases',
  insights: 'Insights',
  vault: 'Vault',
};

/** Журналы пространства: у решения — все, у Knowledge — Insights и Vault, иначе — нет. */
export function journalsOf(kind: string): Journal[] {
  if (kind === 'solution') return ['changelog', 'releases', 'insights', 'vault'];
  if (kind === 'knowledge') return ['insights', 'vault'];
  return [];
}

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

export const STATUSES: EntryStatus[] = ['active', 'revisit', 'superseded', 'retracted'];

export const STATUS_TEXT: Record<EntryStatus, string> = {
  active: 'действует',
  revisit: 'на пересмотре',
  superseded: 'заменён',
  retracted: 'отозван',
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
