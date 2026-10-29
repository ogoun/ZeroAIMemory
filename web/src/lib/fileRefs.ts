// Ссылки знаний на файлы решения (M4.5 §6): индекс `GET /file-refs` → ссылки по записям.
// Индекс несёт до 5 записей на путь (новые первыми), поэтому у давней записи, вытесненной
// новыми ссылками на тот же файл, пометки может не быть. У Knowledge своего решения нет.
import { fileRefIndex } from './api';

export interface RefState {
  path: string;
  /** `ok` | `stale`. */
  state: string;
}

/** Ссылки по ссылке записи (`i:<id>`, `n:<id>`, …). */
export type RefsByTarget = Map<string, RefState[]>;

const GUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Индекс решения `space` (GUID); у прочих пространств и при сбое — пустой. */
export async function loadRefs(user: string, space: string): Promise<RefsByTarget> {
  const out: RefsByTarget = new Map();
  if (!GUID.test(space)) return out;
  try {
    for (const p of (await fileRefIndex(user, space)).refs) {
      for (const it of p.items) {
        const list = out.get(it.id) ?? [];
        list.push({ path: p.path, state: it.state });
        out.set(it.id, list);
      }
    }
  } catch {
    // пометка устаревших ссылок — подсказка: без индекса страница работает
  }
  return out;
}

export const staleOf = (refs: RefState[] | undefined): string[] =>
  (refs ?? []).filter((r) => r.state === 'stale').map((r) => r.path);

/** Подпись пометки: «возможно устарело: изменился src/a.rs» (пути через запятую). */
export const staleText = (paths: string[]): string => `возможно устарело: изменился ${paths.join(', ')}`;
