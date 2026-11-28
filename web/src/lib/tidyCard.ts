// Карточка предложения уборки памяти (M7.5, спецификация §8.8 п. 1; сервер — §8.7,
// `GET /proposals?type=tidy`, признак `tidy`): тексты типа кандидата, сигналов и операции, дифф
// `merge` для `DiffView` (`keep` против `text` или `drop`), доступность «Применить» (`stale` —
// кнопка недоступна с объяснением), поля и тело окна «Применить», текст отказа 409. Решает только
// человек: страница ничего не применяет сама, «Применить» — его действие на текущей ревизии.
// Модуль без DOM и без runtime-импортов, кроме `.ts`: его читает `node --test`
// (`web/test/tidyCard.test.ts`).
import { plural } from './plural.ts';

/** Тип кандидата сборщика (`zam_store::tidy::CandidateType`). */
export type TidyCandidate = 'contradiction' | 'probe_fail' | 'stale' | 'near_duplicate' | 'noisy';
/** Операция кандидата (`zam_store::tidy::Op`); удаления нет. */
export type TidyOpName = 'merge' | 'retract' | 'revisit' | 'triggers' | 'term' | 'append' | 'keep';

/** Запись кандидата (`payload.records`): ссылка и ревизия на момент предложения, L1. */
export interface TidyRecord {
  ref: string;
  rev: number;
  kind: string;
  insight_kind?: string | null;
  space?: string;
  title: string;
  l1: string;
  status?: string | null;
  certainty?: string | null;
  /** `certainty = axiom` — «решение человека». */
  human_decision?: boolean;
}

export interface TidyTrigger {
  type: string;
  pattern: string;
  tools?: string[] | null;
}

/** Операция итога исполнителя (`payload.op`). */
export interface TidyOp {
  op: TidyOpName | string;
  keep?: string;
  drop?: string;
  target?: string;
  text?: string;
  triggers?: TidyTrigger[];
  title?: string;
  definition?: string;
  aliases?: string[];
}

export interface TidySignal {
  name: string;
  value: string;
}

/** Поля карточки уборки сверх общих полей предложения. */
export interface TidyFields {
  status: string;
  candidate: TidyCandidate | string;
  signals: TidySignal[] | null;
  records: TidyRecord[] | null;
  ops?: string[] | null;
  op: TidyOp | null;
  /** Запись изменилась после предложения (ревизия не та): «Применить» — 409. */
  stale: boolean;
}

// ---- тип кандидата ----

export const CANDIDATE_TITLE: Record<string, string> = {
  contradiction: 'Противоречие',
  probe_fail: 'Проба не находит запись',
  stale: 'Возможно устарело',
  near_duplicate: 'Почти-дубли',
  noisy: 'Шумная подсказка',
};

/** Что значит тип кандидата — строка под заголовком карточки. */
export const CANDIDATE_NOTE: Record<string, string> = {
  contradiction: 'Записи противоречат друг другу (связь contradicts) дольше трёх суток, и обе действуют.',
  probe_fail: 'Пробный вопрос ночной проверки не находит запись ни в бюджете, ни в глубоком поиске три ночи подряд.',
  stale: 'Изменились файлы, на которые ссылается запись, или она давно на пересмотре.',
  near_duplicate: 'Записи почти совпадают по смыслу или по имени термина.',
  noisy: 'Подсказку часто показывают, но агент её не открывает и полезной не отмечает.',
};

export const candidateTitle = (c: string): string => CANDIDATE_TITLE[c] ?? c;

/** Заголовок карточки: «Почти-дубли: «A» и «B»», «Возможно устарело: «A»». */
export function tidyTitle(f: Pick<TidyFields, 'candidate' | 'records'>): string {
  const titles = (f.records ?? []).map((r) => `«${r.title}»`);
  const head = candidateTitle(f.candidate);
  if (!titles.length) return head;
  return `${head}: ${titles.length > 2 ? `${titles.slice(0, -1).join(', ')} и ${titles.at(-1)}` : titles.join(' и ')}`;
}

// ---- сигналы ----

const DAYS: [string, string, string] = ['сутки', 'суток', 'суток'];
const n = (v: string): number => Number(v);
const days = (v: string): string => `${v} ${plural(n(v), ...DAYS)}`;

/** Сигнал сборщика строкой для человека. */
export function signalText(s: TidySignal): string {
  const v = s.value;
  switch (s.name) {
    case 'cosine':
      return `Сходство по смыслу (косинус): ${v.replace('.', ',')}`;
    case 'same_name':
      return `Совпадает имя термина: «${v}»`;
    case 'contradicts_days':
      return `Противоречие не разрешено ${days(v)}`;
    case 'stale_refs':
      return `Изменились связанные файлы: ${v}`;
    case 'revisit_days':
      return `На пересмотре ${days(v)}`;
    case 'probe_fails':
      return `Не находится ${v} ${plural(n(v), 'ночной прогон', 'ночных прогона', 'ночных прогонов')} подряд`;
    case 'question':
      return `Вопрос пробы: «${v}»`;
    case 'shown':
      return `Показов подсказкой: ${v}`;
    case 'opened':
      return `Открыто агентом: ${v}`;
    case 'useful':
      return `Отмечено полезным: ${v}`;
    default:
      return `${s.name}: ${v}`;
  }
}

// ---- операция ----

export const OP_TITLE: Record<string, string> = {
  merge: 'Объединить',
  retract: 'Отозвать',
  revisit: 'На пересмотр',
  triggers: 'Заменить триггеры',
  term: 'Термин',
  append: 'Дописать',
  keep: 'Оставить как есть',
};

/** Запись кандидата по ссылке; ссылки вне кандидата — `null`. */
export const recordOf = (f: Pick<TidyFields, 'records'>, ref: string | undefined): TidyRecord | null =>
  (ref && (f.records ?? []).find((r) => r.ref === ref)) || null;

/** Подпись записи: «Заголовок» (i:…), запись вне кандидата — только ссылка. */
export function recordLabel(f: Pick<TidyFields, 'records'>, ref: string | undefined): string {
  const r = recordOf(f, ref);
  return r ? `«${r.title}» (${r.ref})` : (ref ?? '?');
}

const triggerText = (t: TidyTrigger): string => `${t.type} «${t.pattern}»${t.tools?.length ? ` (${t.tools.join(', ')})` : ''}`;

/** Операция строкой: что произойдёт с записями, если человек её применит. */
export function opText(f: Pick<TidyFields, 'op' | 'records'>): string {
  const op = f.op;
  if (!op) return 'Операции нет.';
  const label = (r: string | undefined) => recordLabel(f, r);
  switch (op.op) {
    case 'merge':
      return op.text
        ? `Объединить: ${label(op.keep)} замещается новой записью с объединённым текстом, ${label(op.drop)} отзывается как дубль.`
        : `Объединить: ${label(op.drop)} отзывается как дубль ${label(op.keep)}; текст ${op.keep ?? ''} не меняется.`;
    case 'retract':
      return `Отозвать ${label(op.target)}: запись остаётся в истории со статусом «отозвана».`;
    case 'revisit':
      return `Отправить ${label(op.target)} на пересмотр (статус «на пересмотре»).`;
    case 'triggers': {
      const list = op.triggers ?? [];
      return `Заменить триггеры ${label(op.target)}: ${list.length ? list.map(triggerText).join('; ') : 'без триггеров'}.`;
    }
    case 'term':
      return op.target
        ? `Псевдонимы к термину ${label(op.target)}: ${(op.aliases ?? []).join(', ')}.`
        : `Новый термин «${op.title ?? ''}»: ${op.definition ?? ''}${op.aliases?.length ? ` Псевдонимы: ${op.aliases.join(', ')}.` : ''}`;
    case 'append':
      return `Дописать к ${label(op.target)} текст ниже.`;
    case 'keep':
      return 'Оставить как есть.';
    default:
      return `Операция ${op.op}.`;
  }
}

/** Краткая операция для строки списка: «Объединить», «Отозвать «A»». */
export function opShort(f: Pick<TidyFields, 'op' | 'records'>): string {
  const op = f.op;
  if (!op) return '';
  const title = OP_TITLE[op.op] ?? op.op;
  const target = op.op === 'merge' ? op.drop : op.target;
  const r = recordOf(f, target);
  return r ? `${title}: «${r.title}»` : title;
}

/** Записи «решение человека» (`axiom`) — предупреждение в карточке и окне «Применить». */
export function humanDecisionText(f: Pick<TidyFields, 'records'>): string {
  const h = (f.records ?? []).filter((r) => r.human_decision);
  if (!h.length) return '';
  const list = h.map((r) => `«${r.title}» (${r.ref})`).join(', ');
  return h.length === 1
    ? `${list} — решение человека (axiom): меняйте её, только если она явно ошибочна.`
    : `${list} — решения человека (axiom): меняйте их, только если они явно ошибочны.`;
}

// ---- дифф `merge` (`DiffView`) ----

/** Строк в диффе, при которых считается построчный дифф (больше — целиком «было/стало»). */
export const DIFF_LINES_MAX = 2000;

const lines = (s: string): string[] => (s === '' ? [] : s.replace(/\r\n?/g, '\n').replace(/\n$/, '').split('\n'));

/**
 * Построчный дифф `a` → `b` в формате, который понимает `DiffView`: шапка `--- from` / `+++ to`,
 * один кусок `@@ -1,N +1,M @@` и строки «␠», «-», «+» (наибольшая общая подпоследовательность
 * строк; тексты больше `DIFF_LINES_MAX` строк — целиком «-» и «+»). Одинаковые тексты — пустая
 * строка (`DiffView` пишет «Различий нет.»).
 */
export function lineDiff(a: string, b: string, from: string, to: string): string {
  const x = lines(a);
  const y = lines(b);
  if (x.length === y.length && x.every((l, i) => l === y[i])) return '';
  const out: string[] = [];
  if (x.length > DIFF_LINES_MAX || y.length > DIFF_LINES_MAX) {
    out.push(...x.map((l) => `-${l}`), ...y.map((l) => `+${l}`));
  } else {
    // длины общих подпоследовательностей суффиксов: lcs[i][j] — для x[i..], y[j..]
    const lcs: number[][] = Array.from({ length: x.length + 1 }, () => new Array<number>(y.length + 1).fill(0));
    for (let i = x.length - 1; i >= 0; i--) {
      for (let j = y.length - 1; j >= 0; j--) {
        lcs[i][j] = x[i] === y[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < x.length && j < y.length) {
      if (x[i] === y[j]) {
        out.push(` ${x[i]}`);
        i++;
        j++;
      } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
        out.push(`-${x[i++]}`);
      } else {
        out.push(`+${y[j++]}`);
      }
    }
    while (i < x.length) out.push(`-${x[i++]}`);
    while (j < y.length) out.push(`+${y[j++]}`);
  }
  return [`--- ${from}`, `+++ ${to}`, `@@ -1,${x.length} +1,${y.length} @@`, ...out].join('\n') + '\n';
}

/** Что сравнивает дифф `merge`. */
export interface MergeDiff {
  /** `text` — `keep` против объединённого текста; `drop` — `keep` против `drop` (текста нет). */
  against: 'text' | 'drop';
  /** Подпись диффа. */
  caption: string;
  diff: string;
}

/**
 * Дифф карточки `merge` (§8.8 п. 1): `keep` против `text`, без текста — против `drop`. Тексты
 * записей — полные тела (`bodies` по ссылке; не загрузились — L1 из предложения, с пометкой);
 * правка человеком в окне (`edited`) заменяет `text`. Не `merge` — `null`.
 */
export function mergeDiff(
  f: Pick<TidyFields, 'op' | 'records'>,
  bodies: Record<string, string | undefined> = {},
  edited?: string,
): MergeDiff | null {
  const op = f.op;
  if (op?.op !== 'merge') return null;
  const keep = recordOf(f, op.keep);
  const drop = recordOf(f, op.drop);
  const text = (r: TidyRecord | null, ref: string | undefined) => (ref && bodies[ref] !== undefined ? bodies[ref]! : (r?.l1 ?? ''));
  const l1 = (ref: string | undefined) => (ref && bodies[ref] === undefined ? ' (L1)' : '');
  const keepText = text(keep, op.keep);
  // правка в окне — пустая: без текста (у `merge` с предложенным текстом окно её не отправит)
  const merged = edited !== undefined ? (edited.trim() ? edited : undefined) : op.text || undefined;
  if (merged !== undefined) {
    return {
      against: 'text',
      caption: `Что станет с ${recordLabel(f, op.keep)}: было${l1(op.keep)} — стало (объединённый текст)`,
      diff: lineDiff(keepText, merged, `${op.keep ?? ''}${l1(op.keep)}`, 'объединённый текст'),
    };
  }
  return {
    against: 'drop',
    caption: `Чем различаются ${recordLabel(f, op.keep)} и ${recordLabel(f, op.drop)}${l1(op.keep) || l1(op.drop) ? ' (L1)' : ''}`,
    diff: lineDiff(keepText, text(drop, op.drop), `${op.keep ?? ''}${l1(op.keep)}`, `${op.drop ?? ''}${l1(op.drop)}`),
  };
}

// ---- «Применить» ----

/** Объяснение недоступной кнопки «Применить» и отказа 409 `code: stale`. */
export const STALE_TEXT =
  'Запись изменилась после предложения — применить нельзя: ревизия не та. Кандидат вернётся следующей ночью с новыми ревизиями; предложение можно отклонить.';

/** Решение по карточке: `show` — ряд решения есть (предложение открыто), `apply` — «Применить»
 * доступно; иначе `why` — объяснение у кнопки. */
export function decideState(f: Pick<TidyFields, 'status' | 'stale'>): { show: boolean; apply: boolean; why: string } {
  if (f.status !== 'proposed') return { show: false, apply: false, why: f.status === 'stale' ? STALE_TEXT : '' };
  if (f.stale) return { show: true, apply: false, why: STALE_TEXT };
  return { show: true, apply: true, why: '' };
}

/** Окно «Применить»: поле текста (`merge`, `append`) или подтверждение без полей. */
export interface ApplyForm {
  /** `text` — с полем текста; `confirm` — окно-подтверждение. */
  kind: 'text' | 'confirm';
  /** Подпись поля. */
  label: string;
  /** Текст поля при открытии (предложенный исполнителем). */
  initial: string;
  /** Пустое поле не отправить (у `append` и у `merge` с предложенным текстом). */
  required: boolean;
  /** Подсказка под полем. */
  hint: string;
  /** Необратимо по смыслу (отзыв): красная кнопка, фокус — на «Отмена». */
  danger: boolean;
}

export function applyForm(f: Pick<TidyFields, 'op' | 'records'>): ApplyForm {
  const op = f.op;
  const confirm = (danger: boolean): ApplyForm => ({ kind: 'confirm', label: '', initial: '', required: false, hint: '', danger });
  switch (op?.op) {
    case 'merge':
      return op.text
        ? {
            kind: 'text',
            label: 'Текст объединённой записи',
            initial: op.text,
            required: true,
            hint: `Текст заменит ${op.keep ?? ''}; ${op.drop ?? ''} отзывается как дубль. Достоверность новой записи — меньшая из двух; без правки текста — не выше theorem.`,
            danger: false,
          }
        : {
            kind: 'text',
            label: 'Текст объединённой записи (необязательно)',
            initial: '',
            required: false,
            hint: `Пусто — ${op.drop ?? ''} отзывается как дубль ${op.keep ?? ''}, текст ${op.keep ?? ''} не меняется. С текстом — ${op.keep ?? ''} замещается новой записью.`,
            danger: false,
          };
    case 'append':
      return { kind: 'text', label: 'Дописать', initial: op.text ?? '', required: true, hint: `Текст допишется в конец ${op.target ?? ''} новой ревизией.`, danger: false };
    case 'retract':
      return confirm(true);
    default:
      return confirm(false);
  }
}

/**
 * Тело `POST /proposals/{id}/accept` по набранному `typed` (§8.7 п. 3: `text` — правка человеком
 * текста `merge` или `append`): текст отправляется, только если человек его изменил (тот же текст —
 * как без правки: достоверность `merge` — не выше `theorem`); пустое необязательное поле `merge` —
 * без текста. Пустое обязательное — ошибка (`Error`).
 */
export function applyBody(form: ApplyForm, typed: string): { text?: string } {
  if (form.kind !== 'text') return {};
  const t = typed.trim();
  if (!t) {
    if (form.required) throw new Error(`${form.label}: пустой текст.`);
    return {};
  }
  return t === form.initial.trim() ? {} : { text: t };
}

/** Текст отказа «Применить» по ответу сервера 409 (`code`): `stale` — запись изменилась,
 * `not_open` — предложение уже решено (другим окном или ночью). Иное — `null`. */
export function conflictText(code: unknown, status?: unknown): string | null {
  if (code === 'stale') return STALE_TEXT;
  if (code === 'not_open') return `Предложение уже решено${typeof status === 'string' && status ? ` (${status})` : ''} — обновите карточку.`;
  return null;
}

/** Строка итога после «Применить» (перед ссылкой на результат — `i:<новая>`, цель или
 * `t:<термин>`): что сделано; `withText` — у `merge` ушёл текст (предложенный или набранный);
 * предупреждения сервера (не отозван `drop`) — дописываются. */
export function appliedText(op: TidyOp | null, withText: boolean, warnings: string[] = []): string {
  const what =
    op?.op === 'merge'
      ? op.text || withText
        ? 'записи объединены'
        : 'дубль отозван'
      : op?.op === 'retract'
        ? 'запись отозвана'
        : op?.op === 'revisit'
          ? 'запись на пересмотре'
          : op?.op === 'triggers'
            ? 'триггеры заменены'
            : op?.op === 'term'
              ? op.target
                ? 'псевдонимы добавлены'
                : 'термин создан'
              : op?.op === 'append'
                ? 'текст дописан'
                : 'готово';
  const warn = warnings.length ? ` Внимание: ${warnings.join('; ')}.` : '';
  return `Применено: ${what} от вашего имени, в истории записи — «из предложения».${warn} Результат — `;
}
