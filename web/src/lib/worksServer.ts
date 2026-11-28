// Настройки сервера внутренних работ в WebUI администратора (M7.6, спецификация §9.9, решения
// 171–174): группы и тексты полей формы «Настройки сервера» (у каждого — одна строка «что это и чем
// грозит», без ключей конфигурации), значения перечислений по-русски, черновик формы и правка для
// `PATCH /admin/works/settings`. Диапазоны и допустимые значения — из ответа сервера (`min`, `max`,
// `options`): те же, что у конфигурации, здесь не повторяются. Модуль без DOM и без runtime-
// импортов, кроме `.ts`: его читает `node --test` (`web/test/worksServer.test.ts`).
import { num } from './worksText.ts';

/** Поле формы: ключ `[works]`, подпись, строка «что это и чем грозит», вид элемента. */
export interface ServerFieldText {
  key: string;
  title: string;
  note: string;
  kind: 'bool' | 'enum' | 'number' | 'time' | 'text';
}

/** Группы формы по порядку. */
export const SERVER_GROUPS: { title: string; fields: ServerFieldText[] }[] = [
  {
    title: 'Общее',
    fields: [
      {
        key: 'enabled',
        title: 'Внутренние работы на сервере',
        note: 'Выключено — ни ночных работ, ни ночной проверки, ни обращений к модели у всех пользователей; ручные запуски получают отказ.',
        kind: 'bool',
      },
      {
        key: 'default_mode',
        title: 'Исполнитель по умолчанию',
        note: 'Для пользователя, который не выбрал свой: «модель» отправляет данные памяти подключённой модели без его выбора.',
        kind: 'enum',
      },
    ],
  },
  {
    title: 'Куда уходят данные',
    fields: [
      {
        key: 'model_access',
        title: 'Куда можно отправлять данные памяти',
        note: 'Куда можно отправлять данные памяти модели: куда угодно / только в локальную сеть / никуда. Подключения пользователей не удаляются — запрещённый адрес просто перестаёт получать данные.',
        kind: 'enum',
      },
      {
        key: 'allow_loopback',
        title: 'Разрешить адреса машины сервера',
        note: 'Модель на самом сервере (localhost, 127.0.0.1) и служебные адреса его сегмента; включайте, только если модель запущена здесь же — иначе так можно достучаться до служб сервера.',
        kind: 'bool',
      },
    ],
  },
  {
    title: 'Токены и нагрузка',
    fields: [
      {
        key: 'max_daily_tokens',
        title: 'Потолок токенов в сутки',
        note: 'На каждого пользователя, сутки UTC, вход и выход модели; 0 — обращений нет. Больше — дороже у платного поставщика.',
        kind: 'number',
      },
      {
        key: 'max_monthly_tokens',
        title: 'Потолок токенов в месяц',
        note: 'На каждого пользователя, месяц UTC; личный предел пользователя не бывает выше потолка.',
        kind: 'number',
      },
      {
        key: 'max_parallel',
        title: 'Одновременных обращений к модели',
        note: 'На весь сервер: больше — ночь короче, но выше нагрузка на модель и риск отказов «слишком часто».',
        kind: 'number',
      },
      {
        key: 'call_timeout_s',
        title: 'Срок одного обращения, секунд',
        note: 'Дольше ответа не ждём — обращение неудачно; слишком мало — медленная модель не успеет ответить.',
        kind: 'number',
      },
      {
        key: 'batch_records',
        title: 'Записей в одном пакете',
        note: 'Сколько записей уходит модели за раз: больше — меньше обращений, но длиннее запрос и выше риск обрыва.',
        kind: 'number',
      },
    ],
  },
  {
    title: 'Ночь',
    fields: [
      { key: 'at', title: 'Начало ночи', note: 'Время начала ночных работ и проверки — в часовом поясе ниже.', kind: 'time' },
      {
        key: 'tz',
        title: 'Часовой пояс ночи',
        note: 'Имя пояса (Europe/Moscow), UTC или смещение (+03:00); ошибка в поясе сдвинет ночь на дневные часы.',
        kind: 'text',
      },
      {
        key: 'window_min',
        title: 'Окно ночи, минут',
        note: 'Сколько длится ночь: что не успели — переходит на следующую ночь.',
        kind: 'number',
      },
      {
        key: 'check',
        title: 'Ночная проверка памяти',
        note: 'Каждую ночь проверять, находит ли поиск нужные записи; выкл — регрессии поиска видны только после ручного прогона.',
        kind: 'enum',
      },
      {
        key: 'check_max_min',
        title: 'Предел одного прогона проверки, минут',
        note: 'Дольше — прогон обрывается; слишком мало — большая память не успеет пройти проверку.',
        kind: 'number',
      },
    ],
  },
];

/** Значения перечислений по-русски. */
export const OPTION_TITLE: Record<string, Record<string, string>> = {
  default_mode: { off: 'выкл', agent: 'агент', model: 'модель' },
  model_access: { any: 'куда угодно', lan: 'только в локальную сеть', off: 'никуда' },
  check: { nightly: 'каждую ночь', off: 'выкл' },
};

/** Откуда значение. */
export const SOURCE_TITLE: Record<string, string> = { webui: 'задано в WebUI', config: 'файл конфигурации' };

/** Когда действует новое значение (перезапуск не нужен). */
export const APPLIES_TITLE: Record<string, string> = {
  next_call: 'со следующего обращения',
  next_night: 'со следующего планирования ночи',
};

/** Поле по ключу. */
export const fieldText = (key: string): ServerFieldText | undefined =>
  SERVER_GROUPS.flatMap((g) => g.fields).find((f) => f.key === key);

/** Значение для человека: «да/нет», перечисление по-русски, число с разрядами. */
export function valueText(key: string, v: unknown): string {
  if (typeof v === 'boolean') return v ? 'да' : 'нет';
  if (typeof v === 'number') return num(v);
  const s = String(v ?? '');
  return OPTION_TITLE[key]?.[s] ?? s;
}

/** Поле ответа сервера (часть `ServerField`, без зависимости от `worksApi.ts`). */
export interface FieldState {
  value: unknown;
  min?: number;
  max?: number;
}

/** Черновик формы: значение поля как в элементе (флажок — `boolean`, прочее — строка). */
export type ServerDraft = Record<string, string | boolean>;

/** Черновик из действующих значений. */
export function draftOf(fields: Record<string, FieldState>): ServerDraft {
  const d: ServerDraft = {};
  for (const [k, f] of Object.entries(fields)) d[k] = typeof f.value === 'boolean' ? f.value : String(f.value);
  return d;
}

/** Правка из черновика: только изменённые поля, числа — числами; ошибка ввода — текст с
 * подписью поля (сервер проверит то же и ответит 400). */
export function changedValues(
  draft: ServerDraft,
  fields: Record<string, FieldState>,
): { values: Record<string, unknown> } | { error: string; key: string } {
  const base = draftOf(fields);
  const values: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(draft)) {
    if (v === base[k] || !(k in fields)) continue;
    const f = fields[k];
    if (typeof f.value === 'number') {
      const n = Number(String(v).replace(/\s/g, ''));
      const title = fieldText(k)?.title ?? k;
      if (String(v).trim() === '' || !Number.isInteger(n)) return { key: k, error: `${title}: целое число.` };
      if ((f.min !== undefined && n < f.min) || (f.max !== undefined && n > f.max)) {
        return { key: k, error: `${title}: от ${num(f.min ?? 0)} до ${num(f.max ?? 0)}.` };
      }
      if (n !== f.value) values[k] = n;
    } else if (typeof v === 'string') values[k] = v.trim();
    else values[k] = v;
  }
  return { values };
}

/** Ключ поля из ошибки сервера (`works.<ключ>: …`) — подсветить поле. */
export function errorKey(message: string): string | null {
  const m = /works\.([a-z_]+)\s*:/.exec(message);
  return m && fieldText(m[1]) ? m[1] : null;
}
