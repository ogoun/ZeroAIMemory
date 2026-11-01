// Сообщения между WebUI (PdfViewer.svelte) и документом просмотрщика `/viewer/pdf`.
// Документ просмотрщика — с непрозрачным источником (CSP `sandbox` без allow-same-origin):
// - просмотрщик принимает сообщения только от `parent` с источником сервера
//   (`new URL(document.baseURI).origin`) и отвечает на этот источник;
// - WebUI принимает только от `contentWindow` своего фрейма с источником `'null'` и шлёт
//   `'*'` только в `contentWindow` своего фрейма (непрозрачному источнику адресат не задать).
// Байты PDF передаются (transfer) — документу просмотрщика не нужны ни API, ни cookie, ни выбор
// пользователя администратора.

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

/** Сообщение протокола от окна `from` с источником `origin`; иначе `null`. */
export function accept<T extends { type: string }>(
  e: MessageEvent,
  from: Window | null | undefined,
  origin: string,
): T | null {
  if (!from || e.source !== from || e.origin !== origin) return null;
  const d = e.data as { type?: unknown } | null;
  return d && typeof d.type === 'string' && TYPES.has(d.type) ? (d as T) : null;
}

/** Источник непрозрачного документа в `MessageEvent.origin`. */
export const OPAQUE_ORIGIN = 'null';
