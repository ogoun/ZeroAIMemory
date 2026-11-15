// Решение по идее — общее для журнала «Идеи» и вопроса-предложения в «Вопросах» (M5.7 §10; M6
// §16, задача 16): подписи формы «отложить» и кнопки «начать», дата «отложить» (UTC),
// предупреждение об общем решении. Только это и лежит в основном чанке (его импортирует
// `QuestionsView`); кнопки карточки, остальные подписи форм и разбор решения — `ideaButtons.ts`
// (ленивый чанк журнала «Идеи»). Модуль без зависимостей: проверяется `node --test`.

/** Форма карточки идеи: создание, правка и решения с полями. */
export type IdeaForm = 'create' | 'edit' | 'start' | 'defer' | 'decline' | 'reopen';

/** Подписи формы: имя формы (ARIA), заголовок и кнопка отправки. */
export interface FormText {
  aria: string;
  title: string;
  submit: string;
}

/** Форма «отложить» — в карточке идеи и у вопроса-предложения. */
export const DEFER_FORM: FormText = { aria: 'Отложить идею', title: 'Отложить', submit: 'Отложить' };
/** Кнопка «начать» — в карточке идеи и в подтверждении вопроса-предложения. */
export const START_SUBMIT = 'Начать';

/** Дата `YYYY-MM-DD` по UTC через `days` дней от сегодняшней (сервер сравнивает `horizon` с
 * сегодняшней датой UTC: «отложить до» — не раньше завтрашней по UTC). */
export const utcDate = (days: number, now = Date.now()): string =>
  new Date(now + days * 86_400_000).toISOString().slice(0, 10);

/** «Отложить до» по умолчанию — через 30 дней (UTC). */
export const deferDefault = (now = Date.now()): string => utcDate(30, now);
/** Наименьшая дата «отложить до» — завтра по UTC. */
export const deferMin = (now = Date.now()): string => utcDate(1, now);
/** Ошибка даты «отложить до» или пустая строка. */
export const deferError = (horizon: string, now = Date.now()): string =>
  !horizon || horizon < deferMin(now) ? `Дата — не раньше ${deferMin(now)} (UTC).` : '';

/** Предупреждение «начать» идеи Knowledge с задачей в общем решении (§5.2, п. 1). */
export const sharedWarning = (name: string): string => `Задача появится в решении «${name}», её увидят участники.`;
