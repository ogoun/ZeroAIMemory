// Склонение существительного при числе (русский язык): «1 задача, 2 задачи, 5 задач». Один источник
// правила для WebUI (M7.2, спецификация §5.2 п. 6 — имя вкладки «Выполнен, 60 задач»; пре-флайт
// N17): `code.ts` реэкспортирует `plural`, подписи чисел журналов (задача 4) берут формы отсюда.
// Модуль без runtime-импортов: его читает `node --test` (`web/test/plural.test.ts`).

/** Формы существительного: при 1 (21, 101…), при 2–4 (22–24…), при 0 и 5–20 (11–14, 25…). */
export type Forms = readonly [one: string, few: string, many: string];

/** «задача» — вкладки и колонки доски задач. */
export const TASK_FORMS: Forms = ['задача', 'задачи', 'задач'];

/** «запись» — число записей журнала. */
export const RECORD_FORMS: Forms = ['запись', 'записи', 'записей'];

/** Форма при числе `n`: 1 файл, 2 файла, 5 файлов, 11 файлов, 21 файл. Дробная часть и знак не
 * учитываются. */
export function plural(n: number, one: string, few: string, many: string): string {
  const a = Math.trunc(Math.abs(n));
  const d = a % 10;
  const h = a % 100;
  if (d === 1 && h !== 11) return one;
  if (d >= 2 && d <= 4 && (h < 12 || h > 14)) return few;
  return many;
}

/** Число и существительное в нужной форме: «60 задач». */
export const countText = (n: number, f: Forms): string => `${n} ${plural(n, f[0], f[1], f[2])}`;
