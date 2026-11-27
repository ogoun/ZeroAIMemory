// Числа разделов (M7.2, спецификация §5.3): подписи, правило устаревания и кэш. Видимое число —
// `Intl.NumberFormat('ru-RU')` («1 234»), для чтения с экрана — описание «122 записи»: имя ссылки
// журнала и кнопки пространства остаётся прежним («Идеи»), число — отдельный элемент вне имени
// (пре-флайт N3). Формы — `plural.ts` (N17). Логика кэша — `CountsCache` (хранилище, запрос, часы
// и отложенный вызов подставляются); экземпляр на рунах с подписками — `counts.svelte.ts`.
// Модуль без runtime-импортов, кроме `.ts`: его читает `node --test` (`web/test/counts.test.ts`).
import { plural, PLAN_STEP_FORMS, RECORD_FORMS, TASK_FORMS, type Forms } from './plural.ts';
import type { Journal } from './router';

const NUM = new Intl.NumberFormat('ru-RU');

/** Число с разделителем разрядов: `1234` → «1 234» (разделитель — неразрывный пробел). */
export const formatCount = (n: number): string => NUM.format(n);

/** «файл» — число файлов карты кода (журнал «Код»). */
export const FILE_FORMS: Forms = ['файл', 'файла', 'файлов'];

/** «заметка» — число заметок пространства. */
export const NOTE_FORMS: Forms = ['заметка', 'заметки', 'заметок'];

const label = (n: number, f: Forms): string => `${formatCount(n)} ${plural(n, f[0], f[1], f[2])}`;

/** Описание числа журнала для чтения с экрана: «122 записи»; у «Кода» — файлы карты («3 файла»). */
export const countLabel = (journal: Journal, n: number): string =>
  label(n, journal === 'code' ? FILE_FORMS : RECORD_FORMS);

/** Метка открытых вопросов: «открыто 2». */
export const openLabel = (n: number): string => `открыто ${formatCount(n)}`;

/** Число «Задач» с шагами планов (M7.3, спецификация §6.14 п. 10, задача 10c): «109 записей: 37
 * задач и 72 шага планов» — число журнала считает все записи, доска по умолчанию — задачи без
 * шагов (`task_steps` ответа `GET /spaces/{space}/counts`). Шагов нет — `null`. */
export const tasksSplit = (n: number, steps: number): string | null =>
  steps > 0 ? `${countLabel('tasks', n)}: ${label(Math.max(0, n - steps), TASK_FORMS)} и ${label(steps, PLAN_STEP_FORMS)}` : null;

/** Описание журнала целиком: «3 записи», у «Вопросов» с открытыми — «3 записи, открыто 1», у
 * «Задач» с шагами планов (`steps`) — «109 записей: 37 задач и 72 шага планов». */
export const countDescription = (journal: Journal, n: number, open = 0, steps = 0): string => {
  const split = journal === 'tasks' ? tasksSplit(n, steps) : null;
  if (split) return split;
  return open > 0 ? `${countLabel(journal, n)}, ${openLabel(open)}` : countLabel(journal, n);
};

/** Описание числа заметок пространства: «4 заметки». */
export const notesLabel = (n: number): string => label(n, NOTE_FORMS);

/** Подсказка (`title`) у числа журнала; у «Задач» с шагами планов — «109 записей: 37 задач и 72
 * шага планов» (`n` — число журнала, `steps` — шаги). */
export const countHint = (journal: Journal, n = 0, steps = 0): string =>
  (journal === 'tasks' ? tasksSplit(n, steps) : null) ??
  (journal === 'code' ? 'файлов в карте кода' : 'всего записей, без корзины');

/** Подсказка у числа заметок пространства. */
export const NOTES_HINT = 'заметок, без папок и корзины';

/** Изменяющий запрос `method path` (путь — от `api/v1/`) меняет числа разделов (§5.3 п. 3):
 * записи журналов (создание, правка, удаление, восстановление, замещение, отзыв, ответ на
 * вопрос, решения по идее), заметки и папки (создание, удаление, восстановление), Vault
 * (создание, удаление, перенос), очистка карты кода (`DELETE code?solution=`), перенос выжимки
 * кэша в память. Правка текста заметки, вложения, захват задачи и оценка записи числа не меняют.
 * Применяет `request` (`api.ts`). */
export function changesCounts(method: string, path: string): boolean {
  if (method === 'GET') return false;
  const p = path.split('?')[0];
  if (p === 'code') return method === 'DELETE';
  if (/^spaces\/[^/]+\/(entries|vault)$/.test(p)) return method === 'POST';
  if (/^entries\/[^/]+$/.test(p)) return method === 'PATCH' || method === 'DELETE';
  if (/^entries\/[^/]+\/(restore|supersede|retract|answer|decide|adopt|review|propose)$/.test(p)) return true;
  if (p === 'nodes') return method === 'POST';
  if (/^nodes\/[^/]+$/.test(p)) return method === 'DELETE';
  if (/^nodes\/[^/]+\/restore$/.test(p)) return true;
  if (/^vault\/[^/]+$/.test(p)) return method === 'DELETE';
  return /^vault\/[^/]+\/move$/.test(p) || /^cache\/[^/]+\/promote$/.test(p);
}

// ---- кэш чисел (§5.3 п. 3) ----

/** Возврат на вкладку перечитывает числа, загруженные не меньше 30 с назад. */
export const VISIBLE_REFRESH_MS = 30_000;

/** Слот кэша: последние числа (`null` — первое чтение не удалось), пометка «перечитать» и время
 * загрузки. */
export interface CountsSlot<V> {
  value: V | null;
  stale: boolean;
  at: number;
}

/** Хранилище слотов: `Map` в тестах, `SvelteMap` на странице (чтение подписывает разметку). */
export interface SlotStore<V> {
  get(key: string): CountsSlot<V> | undefined;
  set(key: string, slot: CountsSlot<V>): unknown;
  keys(): Iterable<string>;
}

export interface CountsDeps<V> {
  store: SlotStore<V>;
  /** Чтение чисел пространства (`GET /spaces/{space}/counts`). */
  fetch: (user: string, space: string) => Promise<V>;
  /** Часы (мс). */
  now: () => number;
  /** Отложенный вызов: загрузка из разметки — после отрисовки (микрозадача). */
  defer: (f: () => void) => void;
}

/** Ссылки личных пространств (`spaceRef`): у каждого пользователя свои. Ссылка решения (GUID или
 * id) — одно пространство у всех, кому оно доступно. */
const PERSONAL = new Set(['rules', 'human', 'knowledge']);

/**
 * Кэш чисел разделов: ключ «пользователь | ссылка пространства», одно чтение на ключ — дерево и
 * полоса журналов показывают одно значение. Устаревшее число остаётся до прихода нового.
 */
export class CountsCache<V> {
  #d: CountsDeps<V>;
  /** Ключи с запросом в пути. */
  #inflight = new Set<string>();
  /** Поколение ключа: `invalidate` во время запроса — ответ уже устарел. */
  #gen = new Map<string, number>();

  constructor(d: CountsDeps<V>) {
    this.#d = d;
  }

  /** Числа пространства `space` пользователя `user` (`null` — ещё нет). `load` — загрузить, если
   * чисел нет или они устарели (отложенно, `defer`); `false` — только уже загруженное. */
  get(user: string, space: string, load = true): V | null {
    const k = `${user}|${space}`;
    const s = this.#d.store.get(k);
    if (load && (!s || s.stale) && !this.#inflight.has(k)) this.#d.defer(() => void this.#load(user, space));
    return s?.value ?? null;
  }

  /**
   * Числа устарели: показанные перечитаются при следующем чтении разметкой, прочие — когда их
   * покажут. Ключ подходит, если пространство — `space` (без него — любое), а пользователь —
   * `user` (без него — любой) или пространство — решение: общее решение у администратора видно
   * под владельцем и участниками, и запись под одним из них меняет числа у всех (ревью r1, M6).
   */
  invalidate(user?: string, space?: string): void {
    const hit = (k: string) => {
      const i = k.indexOf('|');
      const [u, s] = [k.slice(0, i), k.slice(i + 1)];
      return (space === undefined || s === space) && (user === undefined || u === user || !PERSONAL.has(s));
    };
    for (const k of new Set([...this.#d.store.keys(), ...this.#inflight])) {
      if (hit(k)) this.#stale(k);
    }
  }

  /** Возврат на вкладку: устаревают числа старше `VISIBLE_REFRESH_MS` — перечитывание ключа не
   * чаще раза в 30 с. */
  visible(): void {
    const now = this.#d.now();
    for (const k of [...this.#d.store.keys()]) {
      const s = this.#d.store.get(k);
      if (s && !s.stale && now - s.at >= VISIBLE_REFRESH_MS) this.#stale(k);
    }
  }

  #stale(k: string): void {
    this.#gen.set(k, (this.#gen.get(k) ?? 0) + 1);
    const s = this.#d.store.get(k);
    if (s && !s.stale) this.#d.store.set(k, { ...s, stale: true });
  }

  async #load(user: string, space: string): Promise<void> {
    const k = `${user}|${space}`;
    if (this.#inflight.has(k)) return;
    this.#inflight.add(k);
    const gen = this.#gen.get(k) ?? 0;
    let value: V | null = this.#d.store.get(k)?.value ?? null;
    try {
      value = await this.#d.fetch(user, space);
    } catch {
      // числа — подсказка: без них дерево и полоса работают, повтор — при следующем устаревании
    } finally {
      this.#inflight.delete(k);
    }
    this.#d.store.set(k, { value, stale: (this.#gen.get(k) ?? 0) !== gen, at: this.#d.now() });
  }
}
