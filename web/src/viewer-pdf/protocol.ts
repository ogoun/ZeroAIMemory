// Сообщения между WebUI (PdfViewer.svelte) и документом просмотрщика `/viewer/pdf`.
// Оба документа — одного источника: получатель проверяет `event.source` (родитель или свой
// iframe) и `event.origin`. Байты PDF передаются (transfer) — документу просмотрщика не нужны
// ни API, ни cookie, ни выбор пользователя администратора.

/** WebUI → просмотрщик. */
export type ToViewer =
  | { type: 'zam-pdf-open'; data: ArrayBuffer; page: number | null }
  | { type: 'zam-pdf-page'; page: number };

/** Просмотрщик → WebUI. */
export type FromViewer =
  | { type: 'zam-pdf-ready' }
  | { type: 'zam-pdf-state'; page: number; pages: number }
  | { type: 'zam-pdf-error'; message: string };

const TYPES = new Set(['zam-pdf-open', 'zam-pdf-page', 'zam-pdf-ready', 'zam-pdf-state', 'zam-pdf-error']);

/** Сообщение протокола от ожидаемого окна того же источника; иначе `null`. */
export function accept<T extends { type: string }>(e: MessageEvent, from: Window | null | undefined): T | null {
  if (!from || e.source !== from || e.origin !== location.origin) return null;
  const d = e.data as { type?: unknown } | null;
  return d && typeof d.type === 'string' && TYPES.has(d.type) ? (d as T) : null;
}
