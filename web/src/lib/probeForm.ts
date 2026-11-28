// Набор проб на странице «Проверка памяти» (M7.4, спецификация §7.7 п. 1; REST — §7.6 п. 3):
// проверка полей окон «Добавить пробу» (цели-ссылки формы `<буква>:<id>`) и «Снять» (причина),
// подписи автора. Пределы — как у сервера (`crates/zam-server/src/api/check.rs`): что цели — живые
// записи одного пространства, проверяет он (422). Отдельный модуль ленивой страницы, а не
// `checkVerdict.ts`: тот грузится при старте (точка «Проверка» в шапке).
// Модуль без DOM и без runtime-импортов: его читает `node --test` (`web/test/probeForm.test.ts`).

/** Автор пробы — по-русски. */
export const PROBE_AUTHOR_TEXT: Record<string, string> = { agent: 'агент', model: 'модель', human: 'человек', system: 'сервер' };

/** Целей у ручной пробы — не больше (сервер: `MANUAL_TARGETS_MAX`, как у пробы из кэша). */
export const PROBE_TARGETS_MAX = 5;
/** Вопрос ручной пробы, символов (сервер: `MANUAL_QUESTION_MAX`, как запрос поиска). */
export const PROBE_QUESTION_MAX = 512;
/** Причина снятия пробы, символов (сервер: `RETIRE_REASON_CHARS`). */
export const RETIRE_REASON_MIN = 3;
export const RETIRE_REASON_MAX = 500;

/** Ссылка на запись формы `<буква>:<id>` (id — 26 знаков ULID: `i:01KQ…`, `n:01KQ…`). */
const REF_FORM = /^[a-z]:[0-9A-Za-z]{26}$/;

/**
 * Цели ручной пробы из поля: ссылки через пробел, запятую, точку с запятой или с новой строки,
 * повторы — один раз. Не ссылка `<буква>:<id>`, ни одной или больше пяти — `error` с текстом для
 * поля.
 */
export function parseProbeTargets(text: string): { refs: string[] } | { error: string } {
  const refs = [...new Set(text.split(/[\s,;]+/).filter(Boolean))];
  const bad = refs.filter((r) => !REF_FORM.test(r));
  if (bad.length) {
    const what = bad.length > 1 ? 'не ссылки' : 'не ссылка';
    return { error: `${bad.map((b) => `«${b}»`).join(', ')} — ${what} вида <буква>:<id> (i:01KQ… — insight, n:01KQ… — заметка, k:01KQ… — задача).` };
  }
  if (!refs.length) return { error: 'Нужна хотя бы одна цель — запись, которую вопрос должен находить.' };
  if (refs.length > PROBE_TARGETS_MAX) return { error: `Целей — не больше ${PROBE_TARGETS_MAX}: оставьте главные записи.` };
  return { refs };
}

/** Вопрос ручной пробы: пусто или длиннее предела — текст ошибки для поля, иначе `''`. */
export function questionError(q: string): string {
  const n = [...q.trim()].length;
  if (!n) return 'Введите вопрос — так, как его задал бы агент.';
  if (n > PROBE_QUESTION_MAX) return `Вопрос — не длиннее ${PROBE_QUESTION_MAX} символов (сейчас ${n}).`;
  return '';
}

/** Причина снятия: короче или длиннее пределов — текст ошибки для поля, иначе `''`. */
export function reasonError(r: string): string {
  const n = [...r.trim()].length;
  if (n < RETIRE_REASON_MIN) return `Причина — от ${RETIRE_REASON_MIN} символов: что в пробе неверно.`;
  if (n > RETIRE_REASON_MAX) return `Причина — не длиннее ${RETIRE_REASON_MAX} символов (сейчас ${n}).`;
  return '';
}
