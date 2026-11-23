// Общий кэш чисел разделов на рунах (M7.2, спецификация §5.3 п. 3): ключ «пользователь |
// пространство» (ссылка пространства — `spaceRef`), одно чтение `GET /spaces/{space}/counts` на ключ —
// дерево и полоса журналов показывают одно значение. Загрузка — при первом чтении (раскрытие
// пространства, открытие журнала); устаревание — `invalidate`: после изменяющих запросов записей и
// заметок (подписка `onEntriesChanged`, её зовёт `api.ts` — одна точка), при раскрытии пространства и
// открытии журнала (запись агента видна без перезагрузки), при возврате на вкладку
// (`visibilitychange`) — у чисел старше 30 с. Опроса по таймеру нет. Устаревшее число остаётся на
// экране до прихода нового.
import { SvelteMap } from 'svelte/reactivity';
import { onEntriesChanged, spaceCounts, type SpaceCounts } from './api';

/** Возврат на вкладку перечитывает числа, загруженные не меньше 30 с назад. */
export const VISIBLE_REFRESH_MS = 30_000;

interface Slot {
  /** Последние числа; `null` — первое чтение не удалось. */
  value: SpaceCounts | null;
  /** Перечитать при следующем чтении. */
  stale: boolean;
  /** Время загрузки (`Date.now()`). */
  at: number;
}

const keyOf = (user: string, space: string) => `${user}|${space}`;

class Counts {
  #slots = new SvelteMap<string, Slot>();
  /** Ключи с запросом в пути. */
  #inflight = new Set<string>();
  /** Поколение ключа: `invalidate` во время запроса — ответ уже устарел. */
  #gen = new Map<string, number>();

  constructor() {
    onEntriesChanged((user) => this.invalidate(user));
    if (typeof document !== 'undefined') document.addEventListener('visibilitychange', () => this.#onVisible());
  }

  /**
   * Числа пространства `space` пользователя `user` (`null` — ещё нет). `load` — загрузить, если чисел
   * нет или они устарели: запрос уходит микрозадачей, после отрисовки (чтение в разметке и
   * `$derived` подписывает на значение, менять состояние в них нельзя); `false` — только то, что уже
   * загружено (свёрнутое пространство дерева).
   */
  get(user: string, space: string, load = true): SpaceCounts | null {
    const k = keyOf(user, space);
    const s = this.#slots.get(k);
    if (load && (!s || s.stale) && !this.#inflight.has(k)) queueMicrotask(() => void this.#load(user, space));
    return s?.value ?? null;
  }

  /** Числа пользователя `user` (без него — всех) и пространства `space` (без него — всех
   * пространств) устарели: показанные перечитаются сразу, прочие — при следующем чтении. */
  invalidate(user?: string, space?: string): void {
    const hit = (k: string) => {
      const i = k.indexOf('|');
      return (user === undefined || k.slice(0, i) === user) && (space === undefined || k.slice(i + 1) === space);
    };
    for (const k of new Set([...this.#slots.keys(), ...this.#inflight])) {
      if (!hit(k)) continue;
      this.#gen.set(k, (this.#gen.get(k) ?? 0) + 1);
      const s = this.#slots.get(k);
      if (s && !s.stale) this.#slots.set(k, { ...s, stale: true });
    }
  }

  async #load(user: string, space: string): Promise<void> {
    const k = keyOf(user, space);
    if (this.#inflight.has(k)) return;
    this.#inflight.add(k);
    const gen = this.#gen.get(k) ?? 0;
    let value = this.#slots.get(k)?.value ?? null;
    try {
      value = await spaceCounts(user, space);
    } catch {
      // числа — подсказка: без них дерево и полоса работают, повтор — при следующем устаревании
    } finally {
      this.#inflight.delete(k);
    }
    this.#slots.set(k, { value, stale: (this.#gen.get(k) ?? 0) !== gen, at: Date.now() });
  }

  #onVisible(): void {
    if (document.visibilityState !== 'visible') return;
    const now = Date.now();
    const old = [...this.#slots].filter(([, s]) => !s.stale && now - s.at >= VISIBLE_REFRESH_MS);
    for (const [k, s] of old) {
      this.#gen.set(k, (this.#gen.get(k) ?? 0) + 1);
      this.#slots.set(k, { ...s, stale: true });
    }
  }
}

/** Кэш чисел разделов — один на страницу. */
export const counts = new Counts();
