// Карта кода решения (M5.6, спецификация §9, §11): типы ответов REST `/code/*`, запросы
// страницы «Код» и исходник Mermaid «Обзора». Символ адресуется постоянным `путь#qual`
// (`symbol.ref`): id символа меняется при перезагрузке файла.
import { call } from './api';

export interface CodeCentral {
  id: number;
  path: string;
  qual: string;
  kind: string;
  sig: string | null;
  line: number;
  rank: number;
}

/** `GET /code/status`; `exists: false` — снимка нет (200, а не 404). */
export interface CodeStatus {
  solution: string;
  exists: boolean;
  files: number;
  symbols: number;
  by_lang: { lang: string; files: number; symbols: number }[];
  skipped: { path: string; reason: string }[];
  skipped_total: number;
  commit?: string | null;
  branch?: string | null;
  machine?: string | null;
  updated_at?: string;
  age_s?: number;
  other_share?: number;
  rank_state: 'ok' | 'pending' | null;
  /** Версия снимка (меняется с каждой синхронизацией и очисткой); у пустой карты нет. */
  etag?: string;
  central: CodeCentral[];
  digest: string | null;
  warnings: string[];
}

export interface CodeSymbolHit {
  id: number;
  file_id: number;
  name: string;
  qual: string;
  kind: string;
  path: string;
  line: number;
  end_line: number;
  sig: string | null;
  rank: number;
}

export interface CodeKnowledge {
  path: string;
  /** Ссылка записи (`n:<id>`, `i:<id>`, …). */
  id: string;
  kind: string;
  title: string;
  state: string;
  /** `qual` символа — у ссылок на символ; без него — знание о файле. */
  symbol?: string;
}

/** `GET /code/symbols/{путь#qual}`. */
export interface CodeSymbolCard {
  symbol: CodeSymbolHit & { body_hash: string; exported: boolean | null; ref: string };
  callers: { file_id: number; path: string; count: number }[];
  uses: { name: string; kind: string; count: number; first_line: number }[];
  knowledge: CodeKnowledge[];
  knowledge_left: number;
  tokens: number;
}

export interface CodeFileRow {
  id: number;
  path: string;
  lang: string;
  rank: number;
  symbols: number;
}

export interface CodeFilesPage {
  files: CodeFileRow[];
  total: number;
  offset: number;
  limit: number;
  warnings: string[];
}

export interface CodeEdge {
  src: number;
  dst: number;
  weight: number;
}

/** `GET /code/map?format=json` (поля, нужные «Обзору»). */
export interface CodeMapJson {
  files: { id: number; path: string; lang: string; rank: number }[];
  edges: CodeEdge[];
  files_shown: number;
  files_left: number;
  rank_state: 'ok' | 'pending' | null;
  warnings: string[];
}

/** Страница таблицы файлов (сервер — до 200). */
export const FILES_PAGE = 50;
/** «Обзор»: файлов и рёбер в диаграмме (спецификация §11). */
export const OVERVIEW_FILES = 40;
export const OVERVIEW_EDGES = 120;
/** Бюджет карты для «Обзора» — предел сервера: чем больше файлов, тем полнее выбор первых 40. */
const OVERVIEW_BUDGET = 20000;

const q = encodeURIComponent;

/** Запрос к `/code/*` общим `call` (CSRF, problem+json); администратор — от имени `user`. */
const codeCall = <T>(method: 'GET' | 'DELETE', path: string, user: string) =>
  call<T>(method, path, undefined, user);

export const codeStatus = (user: string, space: string) =>
  codeCall<CodeStatus>('GET', `code/status?solution=${q(space)}`, user);

export function codeFiles(
  user: string,
  space: string,
  p: { lang?: string; q?: string; offset?: number; limit?: number },
) {
  const s = new URLSearchParams({ solution: space });
  if (p.lang) s.set('lang', p.lang);
  if (p.q) s.set('q', p.q);
  if (p.offset) s.set('offset', String(p.offset));
  s.set('limit', String(p.limit ?? FILES_PAGE));
  return codeCall<CodeFilesPage>('GET', `code/files?${s}`, user);
}

export const codeOverview = (user: string, space: string) =>
  codeCall<CodeMapJson>('GET', `code/map?solution=${q(space)}&format=json&budget_tokens=${OVERVIEW_BUDGET}`, user);

/** Версия карты для кэша «Обзора»: etag снимка и состояние рангов (как `ETag` `/code/map`). */
export const overviewVersion = (s: Pick<CodeStatus, 'etag' | 'rank_state'>): string | null =>
  s.etag ? `${s.etag}|${s.rank_state ?? ''}` : null;

/** Последний ответ «Обзора»: одна запись — `(пользователь, решение, версия карты)`. */
let overviewCache: { key: string; data: Promise<CodeMapJson> } | null = null;

/**
 * «Обзор» с кэшем: повторное открытие вкладки (и возврат с карточки) при той же версии карты не
 * запрашивает сервер. Другое решение или новая версия (sync, очистка, пересчёт рангов) — новый
 * запрос; ошибка в кэше не остаётся. `version: null` (сервер без etag) — всегда запрос.
 */
export function codeOverviewCached(user: string, space: string, version: string | null): Promise<CodeMapJson> {
  if (version === null) return codeOverview(user, space);
  const key = JSON.stringify([user, space, version]);
  if (overviewCache?.key === key) return overviewCache.data;
  const data = codeOverview(user, space);
  const entry = { key, data };
  overviewCache = entry;
  data.catch(() => {
    if (overviewCache === entry) overviewCache = null;
  });
  return data;
}

/** Сброс кэша «Обзора» (очистка карты). */
export function dropOverviewCache(): void {
  overviewCache = null;
}

export function codeSymbols(user: string, space: string, text: string, kind?: string) {
  const s = new URLSearchParams({ solution: space, q: text, limit: '50' });
  if (kind) s.set('kind', kind);
  return codeCall<{ symbols: CodeSymbolHit[]; warnings: string[] }>('GET', `code/symbols?${s}`, user);
}

/** Карточка по постоянному адресу `путь#qual` (percent-encoding: `/` и `#` — в одном сегменте). */
export const codeSymbol = (user: string, space: string, ref: string) =>
  codeCall<CodeSymbolCard>('GET', `code/symbols/${q(ref)}?solution=${q(space)}`, user);

/** `DELETE /code` — только владелец решения в своей сессии WebUI. */
export const clearCode = (user: string, space: string) =>
  codeCall<{ removed: number }>('DELETE', `code?solution=${q(space)}`, user);

/** Причины пропуска файла (`skip_reason`, спецификация §6, §12). */
export const SKIP_TEXT: Record<string, string> = {
  too_large: 'слишком большой',
  minified: 'минифицирован',
  binary: 'двоичный',
  timeout: 'разбор не уложился в срок',
  parser_crash: 'сбой разбора',
  limit: 'обрезан пределом',
  lang: 'язык выключен',
};

/** Возраст снимка: «только что», «N мин назад», «N ч назад», «N дн. назад». */
export function ageText(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return 'только что';
  if (s < 3600) return `${Math.floor(s / 60)} мин назад`;
  if (s < 172_800) return `${Math.floor(s / 3600)} ч назад`;
  return `${Math.floor(s / 86_400)} дн. назад`;
}

/** Склонение: 1 файл, 2 файла, 5 файлов. */
export function plural(n: number, one: string, few: string, many: string): string {
  const d = n % 10;
  const h = n % 100;
  if (d === 1 && h !== 11) return one;
  if (d >= 2 && d <= 4 && (h < 12 || h > 14)) return few;
  return many;
}

export const filesText = (n: number) => `${n} ${plural(n, 'файл', 'файла', 'файлов')}`;
export const symbolsText = (n: number) => `${n} ${plural(n, 'символ', 'символа', 'символов')}`;

/** `ветка@коммит7` — что известно. */
export function snapshotText(s: Pick<CodeStatus, 'branch' | 'commit'>): string {
  const c = s.commit ? s.commit.slice(0, 7) : '';
  if (s.branch && c) return `${s.branch}@${c}`;
  return s.branch || c;
}

/**
 * Подпись узла Mermaid: в кавычках; всё, кроме букв, цифр, пробела и `_ . - /`, — сущностью
 * `#<код>;` (Mermaid превращает её в `&#<код>;`, `hydrate/mermaid.ts` — обратно в символ текстом
 * узла SVG, не разметкой).
 * Кроме `"`, `#`, скобок, `<>`, `&`, `` ` ``, `|`, `;` и управляющих экранируются и `:`, `=`, `@`:
 * предобработка Mermaid (`encodeEntities`) срезает последний `;` строки, где после `style` или
 * `classDef` идут `:` и `#…;` (путь `src/style:x#1` терял бы сущность), а `=`/`@` — синтаксис
 * рёбер и форм узла (`==>`, `e1@-->`, `f0@{…}`): подпись не зависит от правил лексера.
 */
export function mermaidLabel(s: string): string {
  const safe = Array.from(s, (ch) => (/[\p{L}\p{N} _.\-/]/u.test(ch) ? ch : `#${ch.codePointAt(0)};`));
  return `"${safe.join('')}"`;
}

/**
 * Исходник «Обзора»: первые {@link OVERVIEW_FILES} файлов по рангу и рёбра между ними по
 * убыванию веса (до {@link OVERVIEW_EDGES}), раскладка ELK. Узлы — `f<номер>`: пути в
 * идентификаторы не попадают.
 */
export function overviewMermaid(
  files: { id: number; path: string; rank?: number }[],
  edges: CodeEdge[],
): string {
  const top = files
    .map((f, i) => ({ f, i }))
    .sort((a, b) => (b.f.rank ?? 0) - (a.f.rank ?? 0) || a.i - b.i)
    .slice(0, OVERVIEW_FILES)
    .map((x) => x.f);
  const node = new Map(top.map((f, i) => [f.id, `f${i}`]));
  const lines = ['%%{init: {"layout": "elk"}}%%', 'flowchart LR'];
  top.forEach((f, i) => lines.push(`  f${i}[${mermaidLabel(f.path)}]`));
  edges
    .filter((e) => e.src !== e.dst && node.has(e.src) && node.has(e.dst))
    .sort((a, b) => b.weight - a.weight)
    .slice(0, OVERVIEW_EDGES)
    .forEach((e) => lines.push(`  ${node.get(e.src)} --> ${node.get(e.dst)}`));
  return lines.join('\n');
}
