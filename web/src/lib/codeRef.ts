// Адрес карточки символа карты кода (M6 §12а, отложено финальным ревью M5.6). Постоянный адрес —
// `путь#qual`; но `qual` вида якоря строки GitHub (`L10`, `L1-L2`, `L10-20`) сервер читает как
// путь с якорем и отвечает 400, поэтому у такого символа ссылка — по `id`
// (`GET /code/symbols/{id}`), с подсказкой «уточните path». Модуль без импортов: его проверяет
// `npm run test:build` (node --test).

/** Якорь строки в стиле GitHub: `L10`, `L10-L20`, `L10-20` (как `zam_code::is_line_anchor`). */
export function isLineAnchor(qual: string): boolean {
  const num = (s: string) => /^[0-9]+$/.test(s);
  if (!qual.startsWith('L')) return false;
  const rest = qual.slice(1);
  const dash = rest.indexOf('-');
  if (dash < 0) return num(rest);
  const b = rest.slice(dash + 1);
  return num(rest.slice(0, dash)) && num(b.startsWith('L') ? b.slice(1) : b);
}

/** Адрес карточки для маршрута «Код»: `путь#qual`, у якоря строки — `id` символа. */
export function symbolEntry(s: { id: number; path: string; qual: string }): string {
  return isLineAnchor(s.qual) ? String(s.id) : `${s.path}#${s.qual}`;
}

/** Подсказка к ссылке по `id` (у прочих символов — нет). */
export const LINE_ANCHOR_HINT =
  'имя совпадает с якорем строки (путь#L10 — строка файла): карточка открыта по id, он меняется при перезагрузке файла — уточните path в поиске';
