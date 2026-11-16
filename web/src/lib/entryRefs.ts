// Ссылки записей журналов (`i:`, `k:`, `w:`, …): вид записи → журнал и буква ссылки, буква → журнал.
// Один источник для `journals.ts` (переход из поиска, `entryRef`) и модулей, которые проверяет
// `node --test` (`ideaButtons.ts`: ссылки «Опирался на» разбора идеи). Зависимостей времени
// выполнения нет.
import type { Journal } from './router';

/** Вид записи журнала → журнал (переход из поиска, ссылки разбора). */
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

/** Буква ссылки записи по виду (`i:`, `t:`, … — как в `/read` и индексе ссылок на файлы;
 * `zam_store::EntryKind::ref_letter`). */
export const REF_LETTER: Record<string, string> = {
  insight: 'i',
  changelog: 'c',
  release: 'r',
  report: 'p',
  term: 't',
  task: 'k',
  idea: 'w',
  question: 'q',
};

/** Буква ссылки → журнал записи (обратное `REF_LETTER` через `JOURNAL_OF_KIND`). */
export const JOURNAL_OF_LETTER: Record<string, Journal> = Object.fromEntries(
  Object.entries(REF_LETTER).map(([kind, letter]) => [letter, JOURNAL_OF_KIND[kind]]),
);

/** Ссылка записи `<буква>:<id>`; вид без буквы — общая `e:`. */
export const entryRef = (kind: string, id: string): string => `${REF_LETTER[kind] ?? 'e'}:${id}`;
