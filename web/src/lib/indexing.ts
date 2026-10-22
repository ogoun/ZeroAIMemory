// Тексты состояния векторного индекса для WebUI.
//
// Причина деградации и действие администратора приходят с сервера: единый словарь —
// `DegradedReason::text` и `DegradedReason::hint` (crates/zam-server/src/index_state.rs),
// в ответах — `degraded_text` (/search), `vector.text` и `vector.hint` (/index/status).
// Здесь — только разметка подсказки, предупреждения поиска и состояние индексации заметки.

/** Текст причины из ответа; старый сервер без текста — код причины. */
export function reasonText(code: string | null | undefined, text: string | null | undefined): string {
  return text || code || 'причина не указана';
}

/** Переходное состояние (само пройдёт) — предупреждение, а не ошибка. */
export const transient = (reason: string): boolean => reason === 'qdrant_down';

/** Подсказка сервера → фрагменты; `code` — команда или ключ (в обратных кавычках). */
export function hintParts(hint: string): { t: string; code: boolean }[] {
  return hint
    .split('`')
    .map((t, i) => ({ t, code: i % 2 === 1 }))
    .filter((p) => p.t !== '');
}

/** Предупреждения ответа /search (`warnings`) — по-русски. */
const WARNING: Record<string, string> = {
  'reranker not installed':
    'реранкер не установлен (zam-server models fetch --reranker; для CPU-реранкера jina — ещё --accept-nc-license) — порядок результатов без реранка',
  'index rebuilding': 'индекс пересобирается — векторная выдача может быть неполной',
  'reranker failed': 'реранкер не сработал — порядок результатов без реранка (подробности в журнале)',
  'solution access pending': 'решение исключено из поиска: запрос доступа ждёт подтверждения владельца',
  'solution access rejected': 'решение исключено из поиска: запрос доступа отклонён владельцем',
  'solution access revoked': 'решение исключено из поиска: доступ отозван владельцем',
  'solution unavailable': 'решение исключено из поиска: база владельца недоступна',
};

export const warningText = (w: string): string => WARNING[w] ?? w;

/** Состояние индексации заметки (`index_state`): подсказка значка. */
const STATE_TEXT: Record<string, string> = {
  pending: 'Индексация: в очереди',
  indexed: 'Индексация: готово',
  failed: 'Индексация не удалась — подробности на странице «Индекс»',
  excluded: 'Не индексируется (index: false)',
};

/** Короткая подпись состояния рядом с ревизией в шапке документа. */
const STATE_SHORT: Record<string, string> = {
  pending: 'в очереди на индексацию',
  indexed: 'проиндексирована',
  failed: 'индексация не удалась',
  excluded: 'не индексируется',
};

export const stateText = (s: string): string => STATE_TEXT[s] ?? `Индексация: ${s}`;
export const stateShort = (s: string): string => STATE_SHORT[s] ?? s;
