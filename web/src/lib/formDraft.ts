// Черновик формы с ревизией (M7.3, спецификация §6.14 п. 5; задача k:01M83WA2A6EH0AP6FR7NPG9CJH).
//
// Форма запоминает поля записи на открытии (`base`) и правит черновик (`draft`). 409 — запись
// изменили в другом месте: форма перечитывает её (`fresh`) и переносит черновик правилом `rebase` —
// поле, которое человек не трогал (`draft` = `base`), берётся из свежей записи (иначе повтор
// откатил бы чужую правку), тронутое остаётся его; поле, изменённое и здесь, и там по-разному, —
// в `clash` (его надо сверить). Окно или форма не закрывается (решение 83). Правило прежнего
// `entryEdit.ts:117` (M7.2), теперь общее: правка задачи и записей журналов (`EntryEdit`), формы
// Insights, правка и решения идеи.
//
// Поля — простые значения или структуры (списки триггеров, варианты ADR): сравниваются по
// содержимому. Модуль без runtime-импортов: проверяется `node --test` (`test/formDraft.test.ts`).

/** Значения равны: простые — `===`, структуры — по содержимому (JSON). */
function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  return JSON.stringify(a) === JSON.stringify(b);
}

/** В черновике `draft` есть изменения против `base` (полей записи на открытии формы). */
export function isDirty<T extends Record<string, unknown>>(draft: T, base: T): boolean {
  return (Object.keys(base) as (keyof T)[]).some((k) => !same(draft[k], base[k]));
}

/**
 * Черновик поверх перечитанной записи (409): нетронутое поле — из `fresh`, тронутое — из `draft`;
 * `clash` — поля, изменённые и в черновике, и в `fresh` (по-разному).
 */
export function rebase<T extends Record<string, unknown>>(
  draft: T,
  base: T,
  fresh: T,
): { fields: T; clash: (keyof T)[] } {
  const fields = { ...draft };
  const clash: (keyof T)[] = [];
  for (const k of Object.keys(base) as (keyof T)[]) {
    if (same(draft[k], base[k])) fields[k] = fresh[k];
    else if (!same(fresh[k], base[k]) && !same(fresh[k], draft[k])) clash.push(k);
  }
  return { fields, clash };
}

/** Текст 409 формы (§6.14 п. 5), один у всех форм с ревизией: черновик перенесён на свежую запись;
 * `clash` — подписи полей, изменённых и здесь, и в другом месте (они помечены в форме), `rev` —
 * ревизия свежей записи. */
export function rebaseText(clash: string[], rev?: number): string {
  const now = rev === undefined ? '' : ` (сейчас ревизия ${rev})`;
  const both = clash.length ? ` Изменено и здесь, и в другом месте: ${clash.join(', ')} — сверьте перед сохранением.` : '';
  return `Запись изменилась, пока вы правили${now}: проверьте и сохраните снова.${both}`;
}
