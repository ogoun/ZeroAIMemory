// Общее для ленты и страницы отчётов (M5.5): автор, размер файла, ссылки `data.refs`.
import type { AuthorView } from './api';
import { AUTHOR_TEXT } from './journals';

/** Автор отчёта: вид, сессия агента и машина (если известны). */
export function reportAuthor(a: AuthorView): string {
  const parts = [AUTHOR_TEXT[a.kind] ?? a.kind];
  if (a.session_id) parts.push(`сессия ${a.session_id}`);
  if (a.machine) parts.push(a.machine);
  return parts.join(', ');
}

/** Размер файла по-русски (Б, КБ, МБ). */
export function fmtSize(n: number): string {
  if (n < 1024) return `${n} Б`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} КБ`;
  return `${(n / 1024 / 1024).toFixed(1)} МБ`;
}

/** Ключи `data.refs` отчёта (crates/zam-server/src/api/reports.rs, REFS_KEYS) и подписи. */
export const REF_TITLE: Record<string, string> = {
  commits: 'Коммиты',
  files: 'Файлы',
  notes: 'Заметки',
  tasks: 'Задачи',
};

/** Непустые списки `data.refs` в порядке `REF_TITLE`. */
export function refsOf(data: Record<string, unknown>): [string, string[]][] {
  const r = data.refs;
  if (!r || typeof r !== 'object') return [];
  const refs = r as Record<string, unknown>;
  return Object.keys(REF_TITLE)
    .map((k): [string, string[]] => [
      k,
      Array.isArray(refs[k]) ? (refs[k] as unknown[]).filter((s): s is string => typeof s === 'string') : [],
    ])
    .filter(([, v]) => v.length > 0);
}
