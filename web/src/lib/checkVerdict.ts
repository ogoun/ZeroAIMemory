// Ночная проверка памяти в WebUI (M7.3, спецификация §6.5 п. 5, §6.9 п. 2): тексты вердикта
// прогона, метрики к медиане базы, провалы проб и правила кнопки «Принять как норму» — одно место
// для правил страницы «Проверка памяти» и точки у «Проверка» в шапке. Вердикт — JSON задачи 5
// (`crates/zam-server/src/check/verdict.rs`, `Verdict::to_json`): `{kind, flags, after_update,
// after_update_version?, few, episode?, base_n, base_of, changed?, compared_with?}`.
// Модуль без DOM и без runtime-импортов: его читает `node --test` (`web/test/checkVerdict.test.ts`).

/** Вид вердикта прогона. */
export type VerdictKind = 'baseline' | 'ok' | 'regression' | 'changed' | 'degraded';

/** Флаг регрессии: значение и медиана базы (доли — 0..1, `p95` — мс), разница в п. п. у долей,
 * потеря устойчивых — сколько из скольких. */
export interface CheckFlag {
  name: 'hit_search' | 'hit_prompt' | 'negatives_shown' | 'p95' | 'lost_steady' | 'degraded';
  value?: number;
  median?: number;
  delta_pp?: number;
  lost?: number;
  steady?: number;
}

/** Медианы базы на общих пробах `test` (у `changed` — базы прежнего отпечатка). */
export interface CheckMedians {
  hit_search: number | null;
  hit_prompt: number | null;
  negatives_shown: number | null;
  p95: number | null;
  common: number;
}

/** Вердикт прогона (`last.verdict` у `GET /check/status`). */
export interface CheckVerdict {
  kind: VerdictKind;
  flags: CheckFlag[];
  after_update: boolean;
  after_update_version?: string;
  few: boolean;
  episode?: string;
  base_n: number;
  base_of: number;
  /** Что сменилось в отпечатке: `model` | `device` | `reranker` | `qdrant` | `check_version`. */
  changed?: { what: string; from: unknown; to: unknown };
  compared_with?: CheckMedians;
}

/** Метрики части прогона (`dev`, `test`, `all`). */
export interface PartMetrics {
  n: number;
  hit_search: number | null;
  mrr10: number | null;
  none: number | null;
  beyond: number | null;
  hit_prompt: number | null;
  prompt_n: number;
  hard: number | null;
  hard_n: number;
}

/** Метрики прогона (§6.5 п. 3). */
export interface CheckMetrics {
  dev?: PartMetrics;
  test?: PartMetrics;
  all?: PartMetrics;
  negatives_shown: number | null;
  negatives_n?: number;
  search_p50_ms: number | null;
  search_p95_ms: number | null;
  prompt_p50_ms?: number | null;
  prompt_p95_ms?: number | null;
  degraded?: boolean;
  prompt_on?: boolean;
}

export type RunStatus = 'running' | 'done' | 'skipped' | 'timeout' | 'failed';

/** Прогон (`last` у `GET /check/status`, строки `GET /check/runs`). */
export interface CheckRun {
  run: string;
  trigger: 'nightly' | 'manual';
  night: string | null;
  status: RunStatus;
  started_at: string;
  finished_at: string | null;
  metrics: CheckMetrics | null;
  verdict: CheckVerdict | null;
  flags: string[];
  episode: string | null;
  accepted_at: string | null;
  accepted_by: string | null;
  error: string | null;
  compared_with?: CheckMedians | null;
}

/** Строка тренда (до 30 прогонов, новые первыми). */
export interface TrendRow {
  run: string;
  date: string;
  trigger: 'nightly' | 'manual';
  status: RunStatus;
  verdict: VerdictKind | null;
  hit_search: number | null;
  hit_prompt: number | null;
  negatives_shown: number | null;
  p95: number | null;
}

/** Проваленная проба прогона (до 20). */
export interface FailingProbe {
  probe: string;
  question: string;
  kind: string;
  split: string;
  /** Ожидаемая цель (`i:<id>`, `n:<id>`, `t:<id>`); у посторонних вопросов нет. */
  target?: string | null;
  /** Место в глубоком поиске (`null` — не нашлась и там). */
  rank?: number | null;
  /** Посторонний вопрос: показано подсказок. */
  shown?: number;
  advice: string;
  advice_text?: string;
  /** Соседняя запись выше цели (совет «соседняя запись»). */
  neighbor?: unknown;
}

/** `GET /check/status`. */
export interface CheckStatus {
  last: CheckRun | null;
  running: { run: string } | null;
  trend: TrendRow[];
  failing: FailingProbe[];
  advice: { code: string; text: string }[];
  probes: { active: number; by_kind: Record<string, number>; by_split: Record<string, number> };
  check: { server: 'nightly' | 'off'; user: 'on' | 'off' };
}

const MINUS = '−';

/** Доля → «64 %». */
export const pct = (v: number | null | undefined): string => (v == null ? '—' : `${Math.round(v * 100)} %`);

/** Разница в п. п. со знаком: «−12 п. п.», «+11 п. п.». */
export function ppText(d: number): string {
  const r = Math.round(d);
  if (r === 0) return '0 п. п.';
  return `${r < 0 ? MINUS : '+'}${Math.abs(r)} п. п.`;
}

const MS = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 });

/** Миллисекунды → «1 800 мс». */
export const msText = (v: number | null | undefined): string => (v == null ? '—' : `${MS.format(v)} мс`);

/** Подписи флагов регрессии. */
const FLAG_TITLE: Record<CheckFlag['name'], string> = {
  hit_search: 'поиск',
  hit_prompt: 'подсказки к запросу',
  negatives_shown: 'подсказки к посторонним вопросам',
  p95: 'задержка p95 поиска',
  lost_steady: 'потеря устойчивых',
  degraded: 'поиск без векторов',
};

/** Флаг регрессии одной строкой: «поиск −12 п. п. (64 % → 52 %)», «потеря устойчивых: 4 из 30». */
export function flagText(f: CheckFlag): string {
  const title = FLAG_TITLE[f.name] ?? f.name;
  switch (f.name) {
    case 'hit_search':
    case 'hit_prompt':
    case 'negatives_shown': {
      const d = f.delta_pp ?? (f.value != null && f.median != null ? (f.value - f.median) * 100 : null);
      const span = f.value != null && f.median != null ? ` (${pct(f.median)} → ${pct(f.value)})` : '';
      return d == null ? title : `${title} ${ppText(d)}${span}`;
    }
    case 'p95':
      return f.value != null && f.median != null ? `${title} ${msText(f.value)} (медиана ${msText(f.median)})` : title;
    case 'lost_steady':
      return f.lost != null && f.steady != null ? `${title}: ${f.lost} из ${f.steady}` : title;
    default:
      return title;
  }
}

/** Что сменилось в отпечатке — по-русски. */
const CHANGED_TITLE: Record<string, string> = {
  model: 'другая модель',
  device: 'другое устройство',
  reranker: 'другой реранкер',
  qdrant: 'другой режим Qdrant',
  check_version: 'другие правила проверки',
};

const valueText = (v: unknown): string =>
  v == null || v === '' ? 'нет' : typeof v === 'string' ? v : typeof v === 'number' || typeof v === 'boolean' ? String(v) : JSON.stringify(v);

/** Пометка `few`: в `test` меньше 20 общих проб — сравниваются только устойчивые и переход в
 * `degraded`. */
export const FEW_NOTE = 'в test меньше 20 проб: сравниваются только устойчивые';

/**
 * Вердикт прогона одной строкой (§6.9 п. 2): «в норме», «ухудшение: поиск −12 п. п. (64 % →
 * 52 %)», «ухудшение после обновления до 0.0.17: …», «накопление базы 3/5», «изменилось: другая
 * модель bge-m3 → e5-large», «поиск без векторов».
 */
export function verdictText(v: CheckVerdict): string {
  const few = v.few ? ` (${FEW_NOTE})` : '';
  switch (v.kind) {
    case 'ok':
      return `в норме${few}`;
    case 'baseline':
      return `накопление базы ${v.base_n}/${v.base_of}`;
    case 'regression': {
      const head = v.after_update ? `ухудшение после обновления до ${v.after_update_version ?? '?'}` : 'ухудшение';
      const what = v.flags.map(flagText).join('; ');
      return `${head}${what ? `: ${what}` : ''}${few}`;
    }
    case 'changed': {
      const c = v.changed;
      if (!c) return 'изменилось: другой отпечаток поиска';
      return `изменилось: ${CHANGED_TITLE[c.what] ?? c.what} ${valueText(c.from)} → ${valueText(c.to)}`;
    }
    case 'degraded':
      return 'поиск без векторов';
    default:
      return String((v as { kind: unknown }).kind);
  }
}

/** Пояснение к вердикту (строка под ним). */
export function verdictNote(v: CheckVerdict): string {
  switch (v.kind) {
    case 'baseline':
      return 'База — пять последних ночных прогонов с тем же отпечатком поиска; ручные прогоны в неё не входят.';
    case 'regression':
      return 'Эпизод держится, пока метрики не вернутся: следующие ночи сравниваются с прежней базой.';
    case 'changed':
      return 'Сменился отпечаток поиска: с прежней базой не сравнить, база копится заново.';
    case 'degraded':
      return 'Эмбеддер не загружен: поиск идёт без векторов, такие прогоны в базу не входят.';
    default:
      return '';
  }
}

/**
 * Вердикт, который показывает страница. Принятая норма начинает базу заново — «накопление базы
 * 1/5», хотя сохранённый вердикт прогона прежний: у самого принятого прогона (`accepted_at`) и у
 * ручного прогона того же эпизода (`nightly` — последний ночной, его норма принята).
 */
export function shownVerdict(r: CheckRun | null, nightly: CheckRun | null = null): CheckVerdict | null {
  if (!r?.verdict) return null;
  const accepted =
    !!r.accepted_at ||
    (r.verdict.kind === 'regression' && !!nightly?.accepted_at && !!r.episode && r.episode === nightly.episode);
  if (accepted) return { ...r.verdict, kind: 'baseline', flags: [], after_update: false, few: false, base_n: 1 };
  return r.verdict;
}

/** Прогон без вердикта (пропущен, предел времени, сбой, идёт) — строкой. */
export function runStatusText(r: Pick<CheckRun, 'status' | 'error'>): string {
  switch (r.status) {
    case 'running':
      return 'идёт…';
    case 'skipped':
      return 'пропущен: шла переиндексация — выдача неполна, сравнение бессмысленно';
    case 'timeout':
      return 'превышен предел времени прогона (настройка администратора сервера): метрики по пройденным пробам, без вердикта';
    case 'failed':
      return r.error === 'interrupted' ? 'прерван остановкой сервера' : `сбой: ${r.error ?? 'причина не известна'}`;
    default:
      return 'готово';
  }
}

/** Ночной или ручной. */
export const triggerText = (t: CheckRun['trigger']): string => (t === 'nightly' ? 'ночной' : 'ручной');

/** Регрессия открыта (точка у «Проверка», строка дайджеста агента): последний законченный ночной
 * прогон (`GET /check/runs?trigger=nightly&status=done&limit=1`) — `regression`, норма не принята
 * (правило сервера `check::digest_line`). */
export const regressionOpen = (nightly: CheckRun | null | undefined): boolean =>
  !!nightly && nightly.verdict?.kind === 'regression' && !nightly.accepted_at;

/**
 * Прогон для «Принять как норму» (§6.5 п. 5): последний законченный **ночной** прогон, если его
 * регрессия открыта (принять можно только его, иначе сервер — 409
 * `/problems/check-accept-not-latest`); `null` — кнопки нет.
 */
export const acceptRun = (nightly: CheckRun | null | undefined): string | null =>
  regressionOpen(nightly) ? nightly!.run : null;

/** Строка метрики к медиане базы. */
export interface MetricRow {
  label: string;
  value: string;
  median: string;
}

/** Метрики последнего прогона к медиане базы (§6.9 п. 2): по части `test`, посторонние, p95. */
export function metricRows(m: CheckMetrics | null, med: CheckMedians | null | undefined): MetricRow[] {
  if (!m) return [];
  const md = med ?? null;
  return [
    { label: 'Найдено поиском (test)', value: pct(m.test?.hit_search), median: pct(md?.hit_search) },
    { label: 'Среди подсказок к запросу (test)', value: pct(m.test?.hit_prompt), median: pct(md?.hit_prompt) },
    { label: 'Посторонние вопросы с подсказками', value: pct(m.negatives_shown), median: pct(md?.negatives_shown) },
    { label: 'Задержка поиска p95', value: msText(m.search_p95_ms), median: msText(md?.p95) },
  ];
}

/** Где нашлась проваленная проба. */
export function foundText(f: FailingProbe): string {
  if (f.kind === 'negative') return `посторонний вопрос: подсказок ${f.shown ?? 0}`;
  if (f.rank == null) return 'не нашлась и в глубоком поиске';
  return `в глубоком поиске на месте ${f.rank} — за границей бюджета основного`;
}

/** Виды проб — по-русски. */
export const PROBE_KIND_TEXT: Record<string, string> = {
  probe: 'вопросы исполнителя',
  cache: 'из кэша выжимок',
  negative: 'посторонние',
  manual: 'ручные',
};
