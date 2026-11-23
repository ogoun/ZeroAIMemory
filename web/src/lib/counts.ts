// Числа разделов (M7.2, спецификация §5.3): подписи и правило устаревания. Видимое число —
// `Intl.NumberFormat('ru-RU')` («1 234»), для чтения с экрана — описание «122 записи»: имя ссылки
// журнала и кнопки пространства остаётся прежним («Идеи»), число — отдельный элемент вне имени
// (пре-флайт N3). Формы — `plural.ts` (N17). Кэш чисел — `counts.svelte.ts`.
// Модуль без runtime-импортов, кроме `.ts`: его читает `node --test` (`web/test/counts.test.ts`).
import { plural, RECORD_FORMS, type Forms } from './plural.ts';
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

/** Описание журнала целиком: «3 записи», у «Вопросов» с открытыми — «3 записи, открыто 1». */
export const countDescription = (journal: Journal, n: number, open = 0): string =>
  open > 0 ? `${countLabel(journal, n)}, ${openLabel(open)}` : countLabel(journal, n);

/** Описание числа заметок пространства: «4 заметки». */
export const notesLabel = (n: number): string => label(n, NOTE_FORMS);

/** Подсказка (`title`) у числа журнала. */
export const countHint = (journal: Journal): string =>
  journal === 'code' ? 'файлов в карте кода' : 'всего записей, без корзины';

/** Подсказка у числа заметок пространства. */
export const NOTES_HINT = 'заметок, без папок и корзины';

/** Изменяющий запрос `method path` (путь — от `api/v1/`) меняет числа разделов (§5.3 п. 3):
 * записи журналов (создание, правка, удаление, восстановление, замещение, отзыв, ответ на
 * вопрос, решения по идее), заметки и папки (создание, удаление, восстановление), Vault
 * (создание, удаление, перенос), перенос выжимки кэша в память. Правка текста заметки, вложения,
 * захват задачи и оценка записи числа не меняют. Применяет `request` (`api.ts`). */
export function changesCounts(method: string, path: string): boolean {
  if (method === 'GET') return false;
  const p = path.split('?')[0];
  if (/^spaces\/[^/]+\/(entries|vault)$/.test(p)) return method === 'POST';
  if (/^entries\/[^/]+$/.test(p)) return method === 'PATCH' || method === 'DELETE';
  if (/^entries\/[^/]+\/(restore|supersede|retract|answer|decide|adopt|review|propose)$/.test(p)) return true;
  if (p === 'nodes') return method === 'POST';
  if (/^nodes\/[^/]+$/.test(p)) return method === 'DELETE';
  if (/^nodes\/[^/]+\/restore$/.test(p)) return true;
  if (/^vault\/[^/]+$/.test(p)) return method === 'DELETE';
  return /^vault\/[^/]+\/move$/.test(p) || /^cache\/[^/]+\/promote$/.test(p);
}
