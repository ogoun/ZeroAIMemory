// Сверка строк стартовой загрузки `.size-limit.json` с тем, что `dist/index.html` грузит при
// старте (M7.3, ревью r2 задачи 10a, N1). Строка `start` (старт без `api`) и строка `api` вместе
// перечисляют все чанки старта — скрипт `<script type="module" src>` и `<link rel="modulepreload">`;
// `start+api` — их объединение (весь статический старт одним числом). Сборщик выделяет общие чанки
// сам (`FormDialog`, `router`…): новый общий чанк старта, не попавший ни в одну строку, или шаблон,
// который ничего не находит или находит ленивый чанк, — ошибка сборки (плагин в `vite.config.ts`),
// а не молчаливый недосчёт. Модуль без runtime-импортов: проверяется `node --test`
// (`build/start-budget.test.ts`).

/** Строка `.size-limit.json` (нужные поля). */
export interface LimitLine {
  name: string;
  path: string | string[];
}

/** Ключ строки — первое слово имени: `start`, `api`, `start+api`. */
const keyOf = (l: LimitLine) => l.name.split(' ')[0];
const pathsOf = (l: LimitLine | undefined): string[] => (!l ? [] : Array.isArray(l.path) ? l.path : [l.path]);

/** Чанки старта из `index.html`: `dist/assets/…js` скрипта модуля и `modulepreload`. */
export function staticAssets(html: string): string[] {
  const out = new Set<string>();
  for (const m of html.matchAll(/<script\b[^>]*\btype="module"[^>]*\bsrc="\.\/([^"]+\.js)"/g)) out.add(`dist/${m[1]}`);
  for (const m of html.matchAll(/<link\b[^>]*\brel="modulepreload"[^>]*\bhref="\.\/([^"]+\.js)"/g)) out.add(`dist/${m[1]}`);
  return [...out].sort();
}

/** Шаблон size-limit (`*` — любая часть имени без `/`) как регулярное выражение. */
export function globRe(pattern: string): RegExp {
  const esc = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*');
  return new RegExp(`^${esc}$`);
}

/**
 * Ошибки сверки (пусто — всё сходится). `files` — все файлы `dist/assets` в виде `dist/assets/…`.
 * - каждый чанк старта — ровно в одной из строк `start` и `api`;
 * - каждый шаблон этих строк находит хотя бы один файл и только чанки старта;
 * - шаблоны `start+api` — ровно шаблоны `start` и `api`.
 */
export function checkStart(html: string, lines: LimitLine[], files: string[]): string[] {
  const errors: string[] = [];
  const line = (k: string) => lines.find((l) => keyOf(l) === k);
  const start = line('start');
  const api = line('api');
  const all = line('start+api');
  for (const [k, l] of [['start', start], ['api', api], ['start+api', all]] as const) {
    if (!l) errors.push(`в .size-limit.json нет строки «${k} …»`);
  }
  if (errors.length) return errors;
  const assets = staticAssets(html);
  if (!assets.length) return ['в dist/index.html нет чанков старта (script type="module", modulepreload)'];
  const set = new Set(assets);
  const own = (l: LimitLine) => pathsOf(l).map((p) => ({ p, re: globRe(p) }));
  const startP = own(start!);
  const apiP = own(api!);
  for (const a of assets) {
    const inStart = startP.some((x) => x.re.test(a));
    const inApi = apiP.some((x) => x.re.test(a));
    if (!inStart && !inApi) errors.push(`чанк старта ${a} — ни в строке «start», ни в «api»: добавьте его шаблон в «start»`);
    if (inStart && inApi) errors.push(`чанк старта ${a} — и в «start», и в «api»`);
  }
  for (const { p, re } of [...startP, ...apiP]) {
    const hit = files.filter((f) => re.test(f));
    if (!hit.length) errors.push(`шаблон ${p} не находит файлов в dist/assets — уберите его из строки`);
    for (const f of hit) if (!set.has(f)) errors.push(`шаблон ${p} находит ${f}, а он не грузится при старте`);
  }
  const want = [...pathsOf(start), ...pathsOf(api)].sort();
  const got = [...pathsOf(all)].sort();
  if (want.join('\n') !== got.join('\n')) errors.push('шаблоны строки «start+api» должны быть ровно шаблонами «start» и «api»');
  return errors;
}
