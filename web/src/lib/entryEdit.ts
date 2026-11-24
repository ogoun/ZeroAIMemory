// Правка записи журнала (M7.2, спецификация §5.9 п. 1, форма `EntryEdit.svelte`): поля формы из
// записи, `PATCH` только из изменённых полей и перенос черновика на перечитанную запись после 409.
// Без runtime-импортов, кроме `.ts`: модуль проверяет `node --test` (`test/entryEdit.test.ts`).
import type { Entry, EntryPatch } from './api';

/** Поля формы — строки, как в элементах ввода. */
export type EditFields = {
  title: string;
  /** Простым текстом, как у задач и идей. */
  body: string;
  /** Через запятую. */
  tags: string;
  area: string;
  /** changelog: `data.kind`. */
  kind: string;
  /** Задача: `data.priority`; `''` — без приоритета. */
  priority: string;
  /** Задача: `data.owner` (`agent` | `human`); `''` — не задан. */
  owner: string;
  /** Задача: `data.estimate_min`, минут; `''` — без оценки. */
  estimate: string;
  /** Задача: `data.links`, по одной в строке. */
  links: string;
};

/** Предел оценки задачи, минут (`ESTIMATE_MAX_MIN`, `crates/zam-store/src/tasks.rs`). */
export const ESTIMATE_MAX = 525_600;

/** Имена полей формы в тексте ошибки и подсказке «изменено и у вас, и в другом месте». */
export const FIELD_TEXT: Record<keyof EditFields, string> = {
  title: 'заголовок',
  body: 'текст',
  tags: 'теги',
  area: 'область',
  kind: 'вид',
  priority: 'приоритет',
  owner: 'кто ведёт',
  estimate: 'оценка',
  links: 'ссылки',
};

const str = (d: Record<string, unknown>, k: string): string => (typeof d[k] === 'string' ? (d[k] as string) : '');

/** Непустые строки списка `s` по разделителю `sep`, без повторов, в прежнем порядке. */
const listOf = (s: string, sep: string): string[] => [...new Set(s.split(sep).map((x) => x.trim()).filter(Boolean))];

/** Теги из строки через запятую. */
export const tagList = (s: string): string[] => listOf(s, ',');

/** Поля формы записи `e`. */
export function fieldsOf(e: Entry): EditFields {
  const d = e.data ?? {};
  const links = Array.isArray(d.links) ? d.links.filter((x): x is string => typeof x === 'string') : [];
  return {
    title: e.title,
    body: e.body,
    tags: e.tags.join(', '),
    area: e.area ?? '',
    kind: str(d, 'kind'),
    priority: str(d, 'priority'),
    owner: str(d, 'owner'),
    estimate: typeof d.estimate_min === 'number' ? String(d.estimate_min) : '',
    links: links.join('\n'),
  };
}

/** Итог формы: правка, ошибка заполнения или «изменений нет». */
export type EditResult = { patch: EntryPatch } | { error: string } | { none: true };

/**
 * `PATCH` записи `e` из полей `f` — только изменённое (запись могли править в другом месте, и
 * нетронутое поле не должно её откатить): заголовок (обрезанный, пустой — ошибка), текст как есть,
 * теги, область (пустая — `null`); у changelog — `data` целиком с новым видом (сервер заменяет
 * `data` changelog, `refs` и прочие поля сохраняются); у задачи — `data` только с изменёнными
 * полями (сервер сливает их с прежней `data`, `null` снимает поле).
 */
export function patchOf(e: Entry, f: EditFields): EditResult {
  const was = fieldsOf(e);
  const p: EntryPatch = {};
  const title = f.title.trim();
  if (!title) return { error: 'Укажите заголовок.' };
  if (title !== e.title.trim()) p.title = title;
  if (f.body !== e.body) p.body = f.body;
  const tags = tagList(f.tags);
  if (tags.join('\n') !== e.tags.join('\n')) p.tags = tags;
  const area = f.area.trim();
  if (area !== (e.area ?? '')) p.area = area || null;
  if (e.kind === 'changelog' && f.kind && f.kind !== was.kind) p.data = { ...e.data, kind: f.kind };
  if (e.kind === 'task') {
    const d: Record<string, unknown> = {};
    if (f.priority !== was.priority) d.priority = f.priority || null;
    if (f.owner !== was.owner) d.owner = f.owner || null;
    const est = f.estimate.trim();
    if (est !== was.estimate) {
      const n = Number(est);
      if (est && !(Number.isInteger(n) && n >= 1 && n <= ESTIMATE_MAX))
        return { error: `Оценка — целое число минут от 1 до ${ESTIMATE_MAX}.` };
      d.estimate_min = est ? n : null;
    }
    const links = listOf(f.links, '\n');
    if (links.join('\n') !== listOf(was.links, '\n').join('\n')) d.links = links;
    if (Object.keys(d).length) p.data = d;
  }
  return Object.keys(p).length ? { patch: p } : { none: true };
}

// `isDirty` и `rebase` (409: черновик поверх перечитанной записи) — общее правило форм с ревизией
// (M7.3, §6.14 п. 5): `formDraft.ts`; здесь — реэкспорт для формы правки записей.
export { isDirty, rebase } from './formDraft.ts';
