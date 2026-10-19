// Тексты состояния векторного индекса для WebUI: причины деградации (`degraded_reason`
// ответа /search и `vector.reason` /index/status — `DegradedReason` в
// crates/zam-server/src/index_state.rs), что делать администратору, предупреждения поиска
// и состояние индексации заметки (`index_state`).

/** Причина деградации — коротко, для строки над результатами поиска и заголовка алерта. */
const REASON: Record<string, string> = {
  no_runtime: 'модели не загружены (не найдена библиотека ONNX Runtime)',
  no_model: 'модели не загружены',
  qdrant_down: 'векторная база Qdrant запускается или недоступна',
  qdrant_locked: 'каталог векторной базы занят другим экземпляром сервера',
  broken: 'векторная база Qdrant выключена после сбоев',
  model_mismatch: 'индекс построен другой моделью, нужна переиндексация',
  embed_failed: 'не удалось вычислить эмбеддинг запроса',
  search_failed: 'запрос к векторной базе не удался',
};

export function reasonText(reason: string | null | undefined): string {
  if (!reason) return 'причина не указана';
  return REASON[reason] ?? `причина «${reason}»`;
}

/** Что сделать администратору; `cmd` — команда для запуска на сервере (моноширинным). */
export interface Action {
  text: string;
  cmd?: string;
  /** Помогает переиндексация кнопкой на странице индекса. */
  reindex?: boolean;
}

const ACTION: Record<string, Action> = {
  no_runtime: {
    text:
      'Положите библиотеку ONNX Runtime рядом с zam-server (входит в комплект поставки) или укажите путь в ml.ort_dylib, затем скачайте модели и перезапустите сервер:',
    cmd: 'zam-server models fetch',
  },
  no_model: { text: 'Скачайте модели и перезапустите сервер:', cmd: 'zam-server models fetch' },
  qdrant_down: {
    text:
      'Подождите: Qdrant запускается или перезапускается. Задания индексации копятся и будут обработаны автоматически. Если состояние не меняется, проверьте:',
    cmd: 'zam-server qdrant status',
  },
  qdrant_locked: {
    text:
      'Каталог данных Qdrant занят другим экземпляром zam-server. Остановите его и перезапустите этот сервер.',
  },
  broken: {
    text:
      'Qdrant выключен супервизором или не запущен. Проверьте журнал сервера и состояние Qdrant, устраните причину, перезапустите сервер и запустите переиндексацию.',
    cmd: 'zam-server qdrant status',
    reindex: true,
  },
  model_mismatch: {
    text:
      'Векторы в индексе построены другой моделью (смена модели или устройства). Векторный поиск выключен до переиндексации — запустите её кнопкой ниже.',
    reindex: true,
  },
  embed_failed: { text: 'Эмбеддинг запроса не удался. Подробности — в журнале сервера.' },
  search_failed: {
    text: 'Запрос к Qdrant не удался. Подробности — в журнале сервера и в выводе команды:',
    cmd: 'zam-server qdrant status',
  },
};

/** Действие по причине; неизвестная причина — подсказка сервера (`hint`). */
export function actionFor(reason: string, hint: string | null | undefined): Action {
  return ACTION[reason] ?? { text: hint || 'Подробности — в журнале сервера.' };
}

/** Переходное состояние (само пройдёт) — предупреждение, а не ошибка. */
export const transient = (reason: string): boolean => reason === 'qdrant_down';

/** Предупреждения ответа /search (`warnings`) — по-русски. */
const WARNING: Record<string, string> = {
  'reranker not installed': 'реранкер не установлен — порядок результатов без реранка',
  'reranker failed': 'реранкер не сработал — порядок результатов без реранка (подробности в журнале)',
};

export const warningText = (w: string): string => WARNING[w] ?? w;

/** Состояние индексации заметки (`index_state`) — подсказка значка. */
export const STATE_TEXT: Record<string, string> = {
  pending: 'Индексация: в очереди',
  indexed: 'Индексация: готово',
  failed: 'Индексация не удалась — подробности на странице «Индекс»',
  excluded: 'Не индексируется (index: false)',
};

export const stateText = (s: string): string => STATE_TEXT[s] ?? `Индексация: ${s}`;
