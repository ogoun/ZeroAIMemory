// «Удалить» записи журнала (M7.2, спецификация §5.9 п. 2): в корзину с подтверждением — окно
// `Prompt`, `DELETE` с ревизией карточки (`If-Match`); вернуть запись можно из «Корзины» журнала
// (`JournalTrash.svelte`). Один текст и один разбор 409 для всех журналов.
import type { Ask } from '../components/Prompt.svelte';
import { ApiError, deleteEntry, errorText, isStaleEntry, type Entry, type EntryKind } from './api';

/** Вид записи: в вопросе («Удалить задачу?») и в итоге («Задача «…» перемещена в корзину.»). */
const NOUN: Partial<Record<EntryKind, [string, string]>> = {
  changelog: ['запись', 'Запись «%» перемещена'],
  release: ['релиз', 'Релиз «%» перемещён'],
  task: ['задачу', 'Задача «%» перемещена'],
  question: ['вопрос', 'Вопрос «%» перемещён'],
  insight: ['запись', 'Запись «%» перемещена'],
  term: ['термин', 'Термин «%» перемещён'],
  idea: ['идею', 'Идея «%» перемещена'],
};

/** Что делает журнал после ответа сервера. */
export interface TrashHandlers {
  /** Запись в корзине: перечитать журнал, закрыть карточку, показать `notice`. */
  deleted: (notice: string) => void | Promise<void>;
  /** 409 — запись изменилась, пока открыта карточка: перечитать её и показать `message`. */
  stale: (message: string) => void | Promise<void>;
  /** 401 — сессия кончилась. */
  expired: () => void;
  /** Куда перевести фокус, когда запись ушла (кнопка «Удалить» ушла вместе с ней): обычно — итог
   * журнала (`Notice.svelte`). */
  focus?: () => Element | null | undefined;
}

/** Окно подтверждения «Удалить» записи `e` пользователя `user`: прочие ошибки — в окне. */
export function trashAsk(user: string, e: Entry, h: TrashHandlers): Ask {
  const [noun, done] = NOUN[e.kind] ?? ['запись', 'Запись «%» перемещена'];
  /** Запись удалена (а не 409): фокус — по `h.focus`, иначе — на кнопку, открывшую окно. */
  let gone = false;
  return {
    title: `Удалить ${noun}?`,
    message: `«${e.title}» — в корзину журнала; вернуть можно из «Корзины».`,
    confirm: 'Удалить',
    danger: true,
    refocus: () => (gone ? h.focus?.() : null),
    run: async () => {
      try {
        await deleteEntry(user, e.id, e.rev);
      } catch (err) {
        if (err instanceof ApiError && err.status === 401) return h.expired();
        // окно закрывается: ошибка — в перечитанной карточке, рядом с новыми значениями
        if (isStaleEntry(err)) return h.stale(errorText(err));
        throw new Error(errorText(err));
      }
      gone = true;
      // функцией: `$&` и прочие шаблоны замены в заголовке — текст, а не подстановка
      await h.deleted(`${done.replace('%', () => e.title)} в корзину.`);
    },
  };
}
