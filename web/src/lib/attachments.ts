// Вложения заметки (M5): вид просмотрщика по имени и типу файла, подписи состояния извлечения,
// разбор `locator` (место в документе из выдачи поиска и `/read`). Форма `locator` — как у
// `zam_docs::Locator` (`crates/zam-docs/src/locator.rs`): `page:N`, `slide:N`, `para:N`,
// `sheet:<имя>!A1:F40` (имя — в кавычках, если начинается с `'` или имеет вид `#N`),
// `sheet:#N!A1:F40` — лист по номеру.
import type { Attachment, ExtractState } from './api';

/** Просмотрщик вложения. */
export type ViewerKind = 'pdf' | 'docx' | 'sheet' | 'slides' | 'image' | 'text' | 'none';

const EXT_KIND: Record<string, ViewerKind> = {
  pdf: 'pdf',
  docx: 'docx',
  xlsx: 'sheet',
  xlsm: 'sheet',
  xls: 'sheet',
  ods: 'sheet',
  csv: 'sheet',
  pptx: 'slides',
  png: 'image',
  jpg: 'image',
  jpeg: 'image',
  gif: 'image',
  webp: 'image',
  svg: 'image',
  avif: 'image',
  bmp: 'image',
};

/** Текстовые расширения, которые сервер подсвечивает (`kind=text`, syntect). */
const TEXT_EXT = new Set(
  (
    'txt md markdown json jsonl yaml yml toml ini cfg conf log xml html htm css scss js mjs cjs ts tsx jsx ' +
    'svelte vue rs go py rb java kt kts c h cc cpp hpp cs php sh bash ps1 bat sql proto graphql lua r swift'
  ).split(' '),
);

export const extOf = (name: string): string => {
  const i = name.lastIndexOf('.');
  return i > 0 ? name.slice(i + 1).toLowerCase() : '';
};

/** Вид просмотрщика: по расширению, затем по MIME (`text/*` — текст). */
export function viewerKind(filename: string, mime = ''): ViewerKind {
  const ext = extOf(filename);
  const k = EXT_KIND[ext];
  if (k) return k;
  if (TEXT_EXT.has(ext) || mime.startsWith('text/')) return 'text';
  if (mime.startsWith('image/')) return 'image';
  return 'none';
}

/** Размер файла для человека: `512 Б`, `12,4 КиБ`, `3,1 МиБ`. */
export function sizeText(n: number): string {
  if (n < 1024) return `${n} Б`;
  const units = ['КиБ', 'МиБ', 'ГиБ'];
  let v = n / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v.toLocaleString('ru-RU', { maximumFractionDigits: v < 10 ? 1 : 0 })} ${units[u]}`;
}

const STATE_TEXT: Record<ExtractState, string> = {
  pending: 'ждёт извлечения текста',
  done: 'текст извлечён',
  failed: 'текст не извлечён',
  unsupported: 'без извлечения текста',
};

/** Причины неудачи извлечения по коду (часть `extract_error` до `:`). */
const ERROR_TEXT: Record<string, string> = {
  parse_error: 'файл повреждён или не соответствует формату',
  parse_timeout: 'разбор не уложился в срок',
  parse_crash: 'сбой разборщика',
  parse_oom: 'не хватило памяти разборщику',
  pdfium_unavailable: 'на сервере нет библиотеки PDF (pdfium)',
  too_large: 'файл больше предела разбора',
  blob_missing: 'файл отсутствует в хранилище',
  index_failed: 'ошибка индексации',
};

/** Код причины (`parse_error: xml (…)` → `parse_error`). */
export const errorCode = (e: string): string => e.split(':')[0].trim();

/** Состояние извлечения вложения по-русски (с причиной неудачи и «нужно распознавание»). */
export function stateText(a: Pick<Attachment, 'extract_state' | 'extract_error' | 'ocr_needed'>): string {
  let s = STATE_TEXT[a.extract_state] ?? a.extract_state;
  if (a.extract_state === 'failed' && a.extract_error) {
    const code = errorCode(a.extract_error);
    s += `: ${ERROR_TEXT[code] ?? code}`;
  }
  if (a.ocr_needed) s += ' · нужно распознавание';
  return s;
}

/** Место в документе. */
export type Locator =
  | { kind: 'page'; n: number }
  | { kind: 'slide'; n: number }
  | { kind: 'para'; n: number }
  | { kind: 'sheet'; name: string | null; no: number | null; range: CellRange };

/** Диапазон ячеек, строки с 1, столбцы с 0, границы включительно. */
export interface CellRange {
  text: string;
  row1: number;
  col1: number;
  row2: number;
  col2: number;
}

const CELL = /^([A-Z]{1,3})([1-9][0-9]{0,6})$/;

/** Номер столбца с 0 по буквам (`A` → 0, `AA` → 26). */
export function colIndex(letters: string): number {
  let n = 0;
  for (const ch of letters) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n - 1;
}

/** Буквы столбца по номеру с 0. */
export function colName(i: number): string {
  let s = '';
  let n = i + 1;
  while (n > 0) {
    const r = (n - 1) % 26;
    s = String.fromCharCode(65 + r) + s;
    n = Math.floor((n - 1) / 26);
  }
  return s;
}

function cell(s: string): { row: number; col: number } | null {
  const m = CELL.exec(s);
  return m ? { row: Number(m[2]), col: colIndex(m[1]) } : null;
}

/** Разбор диапазона `A1:F40` или ячейки `A1`. */
export function parseRange(s: string): CellRange | null {
  const t = s.toUpperCase();
  const [a, b] = t.includes(':') ? t.split(':', 2) : [t, t];
  const x = cell(a);
  const y = cell(b);
  if (!x || !y) return null;
  return {
    text: t,
    row1: Math.min(x.row, y.row),
    row2: Math.max(x.row, y.row),
    col1: Math.min(x.col, y.col),
    col2: Math.max(x.col, y.col),
  };
}

const NUM = /^[1-9][0-9]*$/;

/** Разбор `locator`; неверная форма — `null` (просмотрщик открывается с начала). */
export function parseLocator(s: string | null | undefined): Locator | null {
  if (!s) return null;
  const i = s.indexOf(':');
  if (i < 0) return null;
  const kind = s.slice(0, i);
  const rest = s.slice(i + 1);
  if (kind === 'page' || kind === 'slide' || kind === 'para') {
    return NUM.test(rest) ? { kind, n: Number(rest) } : null;
  }
  if (kind !== 'sheet') return null;
  const bang = rest.lastIndexOf('!');
  if (bang < 0) return null;
  const rawName = rest.slice(0, bang);
  const range = parseRange(rest.slice(bang + 1));
  if (!range || !rawName) return null;
  const quoted = rawName.length >= 2 && rawName.startsWith("'") && rawName.endsWith("'");
  if (!quoted && /^#[0-9]+$/.test(rawName)) {
    const no = Number(rawName.slice(1));
    return no >= 1 ? { kind: 'sheet', name: null, no, range } : null;
  }
  const name = quoted ? rawName.slice(1, -1).replaceAll("''", "'") : rawName;
  return name ? { kind: 'sheet', name, no: null, range } : null;
}

/** Подпись места для человека — как `Locator::label` сервера. */
export function locatorLabel(l: Locator): string {
  switch (l.kind) {
    case 'page':
      return `стр. ${l.n}`;
    case 'slide':
      return `слайд ${l.n}`;
    case 'para':
      return `абз. ${l.n}`;
    case 'sheet':
      return l.no !== null ? `лист №${l.no} ${l.range.text}` : `лист «${l.name}» ${l.range.text}`;
  }
}
