// Отказ REST API в WebUI: `ApiError` и один разбор ответа-отказа (problem+json RFC 9457 и
// `Retry-After` RFC 9110 §10.2.3) у `request` (fetch) и `importArchive` (XMLHttpRequest) —
// `api.ts`, пре-флайт N33 M7.2. Модуль без runtime-импортов: его читает `node --test`
// (`web/test/apiError.test.ts`); поля класса — явные (режим strip-types не знает свойств-параметров).

export class ApiError extends Error {
  status: number;
  /** Тело problem+json (RFC 9457) с дополнительными полями, если пришло. */
  problem: Record<string, unknown> | null;
  /** `Retry-After` ответа в секундах (503 занятого сервера, 429), если пришёл. */
  retryAfter: number | null;

  constructor(
    status: number,
    message: string,
    problem: Record<string, unknown> | null = null,
    retryAfter: number | null = null,
  ) {
    super(message);
    this.status = status;
    this.problem = problem;
    this.retryAfter = retryAfter;
  }
}

/** `Retry-After`: секунды или HTTP-дата → секунды от `now` (мс; прошедшая дата — 0); нет или не
 * разобрать — `null`. */
export function retryAfterSecs(v: string | null, now: number = Date.now()): number | null {
  if (!v) return null;
  const s = v.trim();
  if (/^\d+$/.test(s)) return Number(s);
  // HTTP-дата всех трёх форм (IMF-fixdate, RFC 850, asctime) — с именем месяца; без букв
  // `Date.parse` принял бы за дату и «-5», и «12.5»
  if (!/[A-Za-z]{3}/.test(s)) return null;
  // asctime пишется без зоны, но по RFC это GMT (`Date.parse` взял бы местное время)
  const at = Date.parse(/ (GMT|UTC)$/.test(s) ? s : `${s} GMT`);
  return Number.isNaN(at) ? null : Math.max(0, Math.ceil((at - now) / 1000));
}

/** Отказ API → `ApiError`: текст — `detail` problem+json (иначе `statusText`), само тело-объект
 * problem и `Retry-After` в секундах. */
export function apiError(
  status: number,
  statusText: string,
  body: string,
  retryAfter: string | null,
  now: number = Date.now(),
): ApiError {
  let detail = statusText;
  let problem: Record<string, unknown> | null = null;
  try {
    const p: unknown = JSON.parse(body);
    if (p && typeof p === 'object') {
      problem = p as Record<string, unknown>;
      if (typeof problem.detail === 'string') detail = problem.detail;
    }
  } catch {
    /* тело не JSON */
  }
  return new ApiError(status, detail, problem, retryAfterSecs(retryAfter, now));
}
