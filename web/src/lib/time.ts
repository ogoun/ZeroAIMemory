// Время в WebUI (M6 §4.4): JSON API отдаёт моменты в RFC 3339 UTC (`…T…Z`), в локальное время
// их переводит только браузер — здесь. Агенту сервер и `zam` пишут UTC `yyyy-MM-dd HH:mm:ss`.

/** Стиль по умолчанию — короткие дата и время. */
const DEFAULT_STYLE: Intl.DateTimeFormatOptions = { dateStyle: 'short', timeStyle: 'short' };

/**
 * Момент RFC 3339 → локальное время браузера (`ru-RU`). `opts` — стиль и при необходимости пояс
 * (`timeZone`); без `dateStyle`/`timeStyle` — короткие дата и время. Не время — строка как есть.
 */
export function localTime(iso: string, opts: Intl.DateTimeFormatOptions = {}): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const styled = opts.dateStyle !== undefined || opts.timeStyle !== undefined;
  return d.toLocaleString('ru-RU', styled ? opts : { ...DEFAULT_STYLE, ...opts });
}
