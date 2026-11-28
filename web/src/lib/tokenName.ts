// Название токена по умолчанию на «Подключении агента» (M7.6 §9.1, решение координатора 141):
// «<агент> · <решение> · <ГГГГ-ММ-ДД>», не длиннее предела сервера; длинное имя решения
// обрезается с «…». Модуль без runtime-импортов — модульный тест (`test/tokenName.test.ts`).

/** Предел названия токена, символов (как `maxlength` поля и проверка сервера). */
export const TOKEN_NAME_MAX = 64;

/** Местная дата `ГГГГ-ММ-ДД`. */
export function localDate(d: Date): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** «<агент> · <решение> · <дата>» в пределе [`TOKEN_NAME_MAX`] символов. */
export function defaultTokenName(agent: string, solution: string, date: Date): string {
  const head = `${agent} · `;
  const tail = ` · ${localDate(date)}`;
  const room = TOKEN_NAME_MAX - [...head].length - [...tail].length;
  const sol = [...solution.trim()];
  const shown = sol.length > room ? `${sol.slice(0, Math.max(room - 1, 0)).join('')}…` : sol.join('');
  return `${head}${shown}${tail}`;
}
