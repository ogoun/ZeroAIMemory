// Копии сервера в WebUI (M7.2, спецификация M7 §5.6): тексты страницы «Администрирование →
// Копии», блока «Копии» на «Индексе» и плашки `no_recent_backup` над страницами администратора —
// одно место для правил. Давность — `ageText` (`time.ts`, общее правило WebUI, пре-флайт N17).
// Модуль без runtime-импортов, кроме `.ts`: его читает `node --test` (`web/test/backups.test.ts`).
import { ageText, localTime } from './time.ts';
import type { BackupBy, BackupReport, BackupStatus } from './api';

/** Опрос страницы «Копии», пока идёт копия (§5.6 п. 2). */
export const BACKUP_POLL_MS = 3000;

/** Давность момента `iso` к моменту `now` (мс): «N дн. назад». */
export const sinceText = (iso: string, now: number): string =>
  ageText((now - new Date(iso).getTime()) / 1000);

/**
 * Плашка `no_recent_backup` (§5.6 п. 3): «Копий нет» или «Последняя копия N дн. назад»;
 * свежая копия (без предупреждения) — `null`, плашки нет.
 */
export function bannerText(b: BackupStatus | null | undefined, now: number): string | null {
  if (!b || b.warning !== 'no_recent_backup') return null;
  return b.last_at ? `Последняя копия ${sinceText(b.last_at, now)}` : 'Копий нет';
}

/** Время по-местному, часы и минуты: «идёт с 10:15». `timeZone` — для тестов. */
export const hhmm = (iso: string, timeZone?: string): string =>
  localTime(iso, { hour: '2-digit', minute: '2-digit', ...(timeZone ? { timeZone } : {}) });

/** Дата и время по-местному: строка списка копий, последняя копия. */
export const dateTime = (iso: string, timeZone?: string): string =>
  localTime(iso, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    ...(timeZone ? { timeZone } : {}),
  });

/**
 * Расписание (§5.6 п. 2): «ежедневно в 03:30 UTC (06:30 по вашему времени)» или «выключено».
 * Местное время — того же срока сегодня (`now`, мс): смена летнего времени сдвигает его вместе
 * с поясом браузера.
 */
export function scheduleText(schedule: BackupStatus['schedule'], at: string, now: number, timeZone?: string): string {
  if (schedule !== 'daily') return 'выключено';
  const m = /^(\d{1,2}):(\d{2})$/.exec(at);
  if (!m) return `ежедневно в ${at} UTC`;
  const d = new Date(now);
  const slot = Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), Number(m[1]), Number(m[2]));
  const utc = `${m[1].padStart(2, '0')}:${m[2]}`;
  return `ежедневно в ${utc} UTC (${hhmm(new Date(slot).toISOString(), timeZone)} по вашему времени)`;
}

/** Кто снимает копию — для «идёт с ЧЧ:ММ (…)». */
export const byText = (by: BackupBy | undefined): string => (by === 'schedule' ? 'по расписанию' : 'из WebUI');

const BYTES = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });

/** Размер: «800 Б», «12,5 КБ», «1,2 МБ», «3,4 ГБ» (основание 1024). */
export function bytesText(n: number): string {
  const units = ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${BYTES.format(i === 0 ? Math.round(v) : v)} ${units[i]}`;
}

/** Длительность копии: «850 мс», «3,4 с», «2 мин 5 с». */
export function msText(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} мс`;
  const s = ms / 1000;
  if (s < 60) return `${BYTES.format(s)} с`;
  const whole = Math.round(s);
  return `${Math.floor(whole / 60)} мин ${whole % 60} с`;
}

/** Итог копии одной строкой (§5.6 п. 2), как строка отчёта `zam-server backup`, без пути. */
export function reportLine(r: BackupReport): string {
  const s =
    `копия ${r.name}: баз ${r.dbs}, блобов ${r.blobs} (связано ${r.linked}, скопировано ${r.copied}), ` +
    `${bytesText(r.bytes)}, ${msText(r.ms)}`;
  return r.drift.length ? `${s}; расхождения снимков: ${r.drift.join('; ')}` : s;
}
