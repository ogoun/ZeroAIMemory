// Уход из формы с несохранённым (M7.2, ревью задачи 10, круг 2; финальное ревью, I1): один вопрос
// для формы правки записи (`EntryEdit` — переход по адресу и действия журнала, которые закрыли бы
// форму), формы статуса задачи и форм карточек Insights, «Идей» и «Словаря». Свой модуль: его
// импортирует основной чанк (`TasksView`, журналы), а `entryEdit.ts` — только ленивый чанк формы.
import { setGuard } from './router';

/** Вопрос `window.confirm`, как у редактора заметок. */
export const LEAVE_EDIT = 'В форме есть несохранённый текст. Закрыть её без сохранения?';

/**
 * Защита несохранённого на время жизни формы или журнала: переход по адресу (запись из Ctrl+K,
 * ссылка, пункт журнала, «назад») при `dirty()` сначала спрашивает `LEAVE_EDIT` — проверка ухода
 * роутера, как у редактора заметок; закрытие и перезагрузка вкладки — `beforeunload`. `dirty` —
 * признак владельца формы (сравнение полей с записью), читается в момент перехода: форма, которую
 * журнал уже закрыл (или решил закрыть), отвечает `false`. Возвращает снятие защиты (для `onMount`).
 * Проверка ухода у роутера одна: защита ставится там, где других нет (журнал без `EntryEdit`).
 */
export function guardLeave(dirty: () => boolean): () => void {
  const off = setGuard(() => !dirty() || window.confirm(LEAVE_EDIT));
  const onbeforeunload = (e: BeforeUnloadEvent) => {
    if (dirty()) e.preventDefault();
  };
  window.addEventListener('beforeunload', onbeforeunload);
  return () => {
    off();
    window.removeEventListener('beforeunload', onbeforeunload);
  };
}
