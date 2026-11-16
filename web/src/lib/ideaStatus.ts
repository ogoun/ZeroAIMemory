// Статусы идеи и их подписи (M5.7 §5, M6.5 §4): один источник для журнала «Идеи» (чипы, разделы
// «Группировать по статусу», строки ленты), Ctrl+K (`journals.ts` реэкспортирует) и модулей,
// которые проверяет `node --test` (`ideaButtons.ts`). Зависимостей времени выполнения нет.

/** Статус идеи. */
export type IdeaStatus = 'new' | 'returned' | 'accepted' | 'declined' | 'done' | 'deferred' | 'proposed';

/** Статусы идеи в порядке чипов и разделов ленты (подписи — Global Constraints плана M6.5). */
export const IDEA_STATUSES: IdeaStatus[] = ['new', 'returned', 'accepted', 'declined', 'done', 'deferred', 'proposed'];

/** Подписи статусов идеи для человека. */
export const IDEA_STATUS_TEXT: Record<IdeaStatus, string> = {
  new: 'Непросмотрено',
  returned: 'На доработку',
  accepted: 'В плане',
  declined: 'Отклонена',
  done: 'Сделано',
  deferred: 'Отложена',
  proposed: 'Предложена',
};
