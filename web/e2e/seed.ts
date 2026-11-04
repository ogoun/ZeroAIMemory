// Тестовые заметки E2E: создаются через REST API по первому токену администратора.
// Пространство Knowledge своего пользователя администратора, папка «E2E».
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';

/** Код rust из заметки — то, что должна положить в буфер кнопка «Копировать». */
export const RUST_CODE = 'fn main() {\n    println!("привет, {}", 2 + 2);\n}';

const MEDIA_NOTE = (img: string) => `# Медиа-заметка

Ссылка на [[Вторая заметка#Раздел два]] и битая ссылка [[Нет такой заметки]].

## Код

\`\`\`rust
${RUST_CODE}
\`\`\`

## Формулы

Строчная формула $x^2 + y^2 = z^2$ и блочная:

$$
\\int_0^1 x\\,dx = \\frac{1}{2}
$$

> [!NOTE]
> Это callout с **важным** текстом.

## Диаграмма

\`\`\`mermaid
graph TD
  A[Старт] --> B{Проверка}
  B -->|да| C[Готово]
  B -->|нет| A
\`\`\`

## Картинка

![Градиент](${img})

Сноска в конце[^1].

[^1]: Текст сноски.
`;

const ELK_NOTE = `# Диаграмма ELK

\`\`\`mermaid
---
config:
  layout: elk
---
graph TD
  A[Старт ELK] --> B{Проверка}
  B -->|да| C[Готово]
  B -->|нет| A
\`\`\`
`;

const SECOND_NOTE = `# Вторая заметка

${'Абзац для прокрутки. '.repeat(40)}

${Array.from({ length: 12 }, (_, i) => `Строка ${i + 1}.`).join('\n\n')}

## Раздел два

Цель вики-ссылки с якорем.
`;

// ---- PNG без зависимостей: RGB-градиент w×h ----

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

export function makePng(w: number, h: number): Buffer {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // глубина
  ihdr[9] = 2; // RGB
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) {
    const row = y * (w * 3 + 1);
    for (let x = 0; x < w; x++) {
      const p = row + 1 + x * 3;
      raw[p] = Math.round((x / w) * 255);
      raw[p + 1] = Math.round((y / h) * 255);
      raw[p + 2] = 180;
    }
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---- API ----

async function api<T>(base: string, token: string, method: string, path: string, init: RequestInit = {}): Promise<T> {
  const r = await fetch(`${base}api/v1/${path}`, {
    ...init,
    method,
    headers: { Authorization: `Bearer ${token}`, ...(init.headers ?? {}) },
  });
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${await r.text()}`);
  return (await r.json()) as T;
}

interface Node {
  id: string;
  current_rev: number;
}

const json = (body: unknown): RequestInit => ({
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

/** Создаёт заметки; возвращает их id. */
export async function seed(base: string, token: string): Promise<{ media: string; second: string; elk: string }> {
  const second = await api<Node>(base, token, 'POST', 'nodes', json({
    space: 'knowledge',
    path: 'E2E/Вторая заметка',
    kind: 'note',
    content: SECOND_NOTE,
  }));
  const media = await api<Node>(base, token, 'POST', 'nodes', json({
    space: 'knowledge',
    path: 'E2E/Медиа-заметка',
    kind: 'note',
    content: '# Медиа-заметка\n',
  }));
  // вложение привязывается к существующей заметке, затем ссылка вставляется в текст
  const form = new FormData();
  form.append('node', media.id);
  form.append('file', new Blob([new Uint8Array(makePng(240, 120))], { type: 'image/png' }), 'градиент.png');
  const blob = await api<{ link: string }>(base, token, 'POST', 'blobs', { body: form });
  await api<Node>(base, token, 'PUT', `nodes/${media.id}`, {
    headers: { 'Content-Type': 'application/json', 'If-Match': String(media.current_rev) },
    body: JSON.stringify({ content: MEDIA_NOTE(blob.link) }),
  });
  // диаграмма с раскладкой ELK — отдельной заметкой: ленивый чанк ELK не должен грузиться в других сценариях
  const elk = await api<Node>(base, token, 'POST', 'nodes', json({
    space: 'knowledge',
    path: 'E2E/Диаграмма ELK',
    kind: 'note',
    content: ELK_NOTE,
  }));
  return { media: media.id, second: second.id, elk: elk.id };
}

// ---- журналы решения, Vault и совместный доступ (M3) ----

/** GUID решения E2E (каталог данных пересоздаётся на каждый прогон). */
export const SOLUTION_GUID = '0e2e0000-0000-4000-8000-000000000013';
export const SOLUTION_NAME = 'E2E решение';
/** Значение секрета Vault: должно показываться только по «Показать». */
export const VAULT_SECRET = 'e2e-vault-value-7Qm2';
export const MEMBER_NAME = 'e2e-member';
/** Второй запросивший доступ — для отклонения с причиной. */
export const GUEST_NAME = 'e2e-guest';

/** Запрос от имени администратора (cookie-сессия + CSRF): админ-API токеном недоступно. */
export type AdminCall = <T>(method: string, path: string, body?: unknown) => Promise<T>;

export interface JournalSeed {
  /** Insight для замещения. */
  oldInsight: string;
  /** Гипотеза для повышения до theorem. */
  hypothesis: string;
  memberId: string;
  memberToken: string;
  guestToken: string;
}

interface EntryResp {
  id: string;
}

/**
 * Решение администратора с журналами (changelog разных видов и областей, два релиза, insights
 * разной надёжности), секрет Vault и второй пользователь с запросом доступа к решению.
 * Записи — по токену администратора (агент): hypothesis по умолчанию, theorem — с evidence.
 */
export async function seedJournals(base: string, token: string, admin: AdminCall): Promise<JournalSeed> {
  const post = <T>(path: string, body: unknown) => api<T>(base, token, 'POST', path, json(body));
  const sp = `spaces/${SOLUTION_GUID}`;
  await post('solutions', { agent_solution_id: SOLUTION_GUID, name: SOLUTION_NAME });

  const changelog = (title: string, kind: string, area: string, body = '') =>
    post<EntryResp>(`${sp}/entries`, { kind: 'changelog', title, body, area, data: { kind } });
  await changelog('Добавлен экспорт в Markdown', 'feat', 'web', 'Кнопка «Экспорт» в меню заметки.');
  await post<EntryResp>(`${sp}/entries`, {
    kind: 'release',
    title: 'Релиз 0.1.0',
    body: 'Первый релиз.\nЭкспорт заметок.\n',
    data: { version: '0.1.0', date: '2026-10-01' },
  });
  await changelog('Исправлен CRLF в хэше', 'fix', 'store');
  await changelog('Описан формат журналов', 'docs', 'web');
  await post<EntryResp>(`${sp}/entries`, {
    kind: 'release',
    title: 'Релиз 0.2.0',
    body: 'Первый релиз.\nИсправления хранилища.\n',
    data: {
      version: '0.2.0',
      date: '2026-10-20',
      git_tag: 'v0.2.0',
      commits: [
        { sha: 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678', subject: 'fix(store): CRLF' },
        { sha: 'b2c3d4e5f60718293a4b5c6d7e8f901234567890', subject: 'docs: журналы' },
      ],
    },
  });

  const insight = (title: string, kind: string, extra: Record<string, unknown> = {}) =>
    post<{ id: string }>(`${sp}/entries`, {
      kind: 'insight',
      title,
      body: `${title}: подробности.`,
      area: 'index',
      ...extra,
      data: { kind, ...((extra.data as object) ?? {}) },
    });
  const hyp = await insight('Кэш эмбеддингов ускоряет переиндексацию', 'discovery', { tags: ['perf'] });
  await insight('CRLF ломает хэш содержимого', 'gotcha', {
    certainty: 'theorem',
    data: { evidence: { checked: 'хэш файла с CRLF и LF', method: 'тест store', result: 'хэши различаются' } },
  });
  const old = await insight('Старый вывод о Qdrant', 'decision');

  await api(base, token, 'POST', `${sp}/vault`, json({
    title: 'E2E токен деплоя',
    data: { type: 'token', username: 'deploy-bot', url: 'https://deploy.example.test' },
    secret: VAULT_SECRET,
  }));

  // ещё два пользователя запрашивают доступ к решению: 202 pending
  const requester = async (name: string) => {
    const u = await admin<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name });
    const r = await fetch(`${base}api/v1/solutions`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${u.token.plaintext}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ agent_solution_id: SOLUTION_GUID, name: SOLUTION_NAME }),
    });
    if (r.status !== 202) throw new Error(`запрос доступа ${name}: HTTP ${r.status} ${await r.text()}`);
    return u;
  };
  const member = await requester(MEMBER_NAME);
  const guest = await requester(GUEST_NAME);

  return {
    oldInsight: old.id,
    hypothesis: hyp.id,
    memberId: member.user.id,
    memberToken: member.token.plaintext,
    guestToken: guest.token.plaintext,
  };
}

// ---- отчёты (M5.5) ----

/** Флаг, который ставил бы скрипт SVG отчёта: картинка `<img>` скрипты не исполняет. */
export const SVG_FLAG = '__zamSvgPwned';
/** Заголовок полного отчёта (первый `#` тела совпадает — в шапке не дублируется). */
export const REPORT_TITLE = 'Отчёт E2E: обучение модели';
/** Слово только из текста отчёта «perf» — для полнотекстового поиска. */
export const REPORT_WORD = 'квазичастицами';
/** Слово из двух разделов одного отчёта: `mode=brief` даёт два элемента выдачи на отчёт. */
export const REPORT_DUP_WORD = 'гиперзарядами';
/** Число однотипных отчётов для курсора ленты (страница WebUI — 20). */
export const REPORT_BULK = 21;
export const REPORT_SESSION = 'e2e-report-sess';
export const REPORT_MACHINE = 'e2e-gpu-box';
export const REPORT_COMMIT = '0123abc4567def89';

const SVG = `<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60" onload="window.${SVG_FLAG}=1">
<script>window.${SVG_FLAG}=1</script><rect width="120" height="60" fill="#3a7"/></svg>`;

const REPORT_BODY = `# ${REPORT_TITLE}

Потери падают: $L = -\\sum_i y_i \\log p_i$, итог:

$$
\\mathrm{acc} = \\frac{TP + TN}{N}
$$

![Кривая потерь](loss.png)

![Схема](diag.svg)

| эпоха | потери |
|------:|-------:|
| 1 | 0.92 |
| 2 | 0.41 |

\`\`\`python
print("готово")
\`\`\`

\`\`\`mermaid
graph LR
  A[Данные] --> B[Модель]
\`\`\`

Сырые данные — [results.csv](results.csv).
`;

/** Отчёт для печати и PDF (задача 7): три PNG, формула, Mermaid, таблица, длинный код. */
export const PRINT_TITLE = 'Отчёт E2E: печать';
/** Подписи узлов диаграммы печатного отчёта — ищутся текстом в PDF. */
export const PRINT_NODE_A = 'Энкодер';
export const PRINT_NODE_B = 'Декодер';
/** Строка кода без пробелов длиннее ширины A4: на бумаге переносится. */
export const PRINT_LONG_CODE = 'x'.repeat(30) + '_' + 'y'.repeat(300);
export const PRINT_FILE = 'raw-data.csv';
/** SVG без собственных размеров (только `viewBox`): печать его не блокирует. */
const SVG_NOSIZE = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 40"><rect width="100" height="40" fill="#58a"/></svg>';

const PRINT_BODY = `# ${PRINT_TITLE}

Метрика $\\mathrm{F1}$ — гармоническое среднее:

$$
\\mathrm{F1} = \\frac{2 P R}{P + R}
$$

![Первая](p1.png)

![Вторая](p2.png)

![Третья](p3.png)

![Без размеров](nosize.svg)

Пояснение к метрике[^f1].

| шаг | значение |
|----:|---------:|
${Array.from({ length: 40 }, (_, i) => `| ${i + 1} | ${(i * 0.25).toFixed(2)} |`).join('\n')}

\`\`\`text
${PRINT_LONG_CODE}
\`\`\`

\`\`\`mermaid
graph LR
  A[${PRINT_NODE_A}] --> B[${PRINT_NODE_B}]
\`\`\`

Исходные замеры — [${PRINT_FILE}](${PRINT_FILE}).

[^f1]: F1 — среднее гармоническое точности и полноты.
`;

export interface ReportSeed {
  print: string;
  full: string;
  perf: string;
  trash: string;
  dup: string;
  task: string;
}

/**
 * Отчёты решения E2E по токену администратора (агент, multipart как `zam report`): полный
 * (формула, диаграмма, PNG, SVG со скриптом, таблица, код, CSV, `data.refs`), отчёт «perf» для
 * фильтров и поиска, отчёт для корзины и {@link REPORT_BULK} однотипных — для курсора.
 */
export async function seedReports(base: string, token: string, noteId: string): Promise<ReportSeed> {
  const sp = `spaces/${SOLUTION_GUID}/entries`;
  const task = await api<EntryResp>(base, token, 'POST', sp, json({ kind: 'task', title: 'Задача из отчёта E2E', body: '' }));
  const report = async (
    meta: Record<string, unknown>,
    files: [string, string, Uint8Array | string][] = [],
  ): Promise<string> => {
    const form = new FormData();
    form.append('meta', new Blob([JSON.stringify(meta)], { type: 'application/json' }));
    for (const [name, mime, data] of files) {
      form.append('file', new Blob([typeof data === 'string' ? data : new Uint8Array(data)], { type: mime }), name);
    }
    const r = await api<EntryResp>(base, token, 'POST', sp, {
      body: form,
      headers: { 'X-Zam-Session': REPORT_SESSION, 'X-Zam-Machine': REPORT_MACHINE },
    });
    return r.id;
  };
  // печатный — первым: лента (reports.spec) ждёт полный отчёт последним
  const print = await report(
    {
      title: PRINT_TITLE,
      body: PRINT_BODY,
      tags: ['print'],
      data: { refs: { commits: [REPORT_COMMIT], files: ['src/print.py'] } },
    },
    [
      ['p1.png', 'image/png', makePng(320, 160)],
      ['p2.png', 'image/png', makePng(300, 150)],
      ['p3.png', 'image/png', makePng(280, 140)],
      ['nosize.svg', 'image/svg+xml', SVG_NOSIZE],
      [PRINT_FILE, 'text/csv', 'step,value\n1,0.5\n'],
    ],
  );
  for (let i = 1; i <= REPORT_BULK; i++) {
    await report({ title: `Серия E2E №${i}`, body: `Замер ${i}.\n`, tags: ['bulk'], area: 'bulk' });
  }
  const trash = await report({ title: 'Отчёт E2E для корзины', body: 'Удаляется и восстанавливается.\n', tags: ['trash'] });
  const perf = await report({
    title: 'Отчёт E2E: производительность индекса',
    body: `Сравнение с ${REPORT_WORD}: индекс быстрее на 12 %.\n`,
    tags: ['perf'],
    area: 'index',
  });
  const filler = 'Абзац с подробностями измерений и условий опыта. '.repeat(60);
  const dup = await report({
    title: 'Отчёт E2E: два раздела',
    body: `## Первый раздел

Замер с ${REPORT_DUP_WORD} в начале.

${filler}

## Второй раздел

${filler}

Повтор с ${REPORT_DUP_WORD} в конце.
`,
    tags: ['dup'],
  });
  const full = await report(
    {
      title: REPORT_TITLE,
      body: REPORT_BODY,
      tags: ['ml'],
      area: 'train',
      data: {
        refs: {
          commits: [REPORT_COMMIT],
          files: ['src/train.py'],
          notes: [`n:${noteId}`],
          tasks: [`k:${task.id}`],
        },
      },
    },
    [
      ['loss.png', 'image/png', makePng(160, 90)],
      ['diag.svg', 'image/svg+xml', SVG],
      ['results.csv', 'text/csv', 'epoch,loss\n1,0.92\n2,0.41\n'],
    ],
  );
  return { print, full, perf, trash, dup, task: task.id };
}

/** Заголовок отчёта приёмки M5.5, записанного настоящим `zam report` (спецификация §12 п. 1). */
export const CLI_REPORT_TITLE = 'Отчёт E2E из zam report';
export const CLI_NODE_A = 'Источник';
export const CLI_NODE_B = 'Приёмник';

const CLI_REPORT_BODY = `# ${CLI_REPORT_TITLE}

Потери $L = -\\sum_i y_i \\log p_i$ падают, а площадь:

$$
\\int_0^1 x\\,dx = \\frac{1}{2}
$$

\`\`\`mermaid
graph LR
  A[${CLI_NODE_A}] --> B[${CLI_NODE_B}]
\`\`\`

![Потери](figs/loss.png)

![Точность](figs/acc.png)

![Шаг](figs/lr.png)
`;

/**
 * Отчёт приёмки M5.5 командой `zam report` (release-бинарник `target/release/zam` или
 * `E2E_ZAM`): проект в `.tmp/e2e/cli-report/w` с `solution.toml` решения E2E и тремя PNG рядом.
 * Уровнем выше — файл `.git` (как в тестах CLI): корень проекта не поднимается до репозитория.
 * Окружение — без `ZAM_*`, переменных агента и прокси; `HOME` — каталог проекта.
 */
export function seedCliReport(base: string, token: string, repo: string): string {
  const exe = process.env.E2E_ZAM ?? `${repo}target/release/zam${process.platform === 'win32' ? '.exe' : ''}`;
  if (!existsSync(exe)) throw new Error(`нет ${exe}: соберите cargo build --release -p zam-cli`);
  const top = `${repo}.tmp/e2e/cli-report/`;
  rmSync(top, { recursive: true, force: true });
  const w = `${top}w/`;
  mkdirSync(`${w}.zeroaimemory`, { recursive: true });
  mkdirSync(`${w}figs`, { recursive: true });
  writeFileSync(`${top}.git`, 'gitdir: ./zam-test-no-repo\n');
  writeFileSync(
    `${w}.zeroaimemory/solution.toml`,
    `agent_solution_id = "${SOLUTION_GUID}"\nname = "${SOLUTION_NAME}"\n`,
  );
  writeFileSync(`${w}figs/loss.png`, makePng(300, 150));
  writeFileSync(`${w}figs/acc.png`, makePng(280, 140));
  writeFileSync(`${w}figs/lr.png`, makePng(260, 130));
  writeFileSync(`${w}report.md`, CLI_REPORT_BODY);
  const drop = /^(ZAM_|CLAUDE|CODEX|HTTPS?_PROXY$|ALL_PROXY$|NO_PROXY$)/i;
  const env: NodeJS.ProcessEnv = Object.fromEntries(
    Object.entries(process.env).filter(([k]) => !drop.test(k)),
  );
  Object.assign(env, { ZAM_URL: base.replace(/\/$/, ''), ZAM_TOKEN: token, HOME: w, USERPROFILE: w });
  const out = execFileSync(exe, ['report', 'report.md', '--json'], { cwd: w, env, encoding: 'utf8' });
  const { id } = JSON.parse(out) as { id: string };
  if (!id) throw new Error(`zam report: нет id в ответе ${out}`);
  return id;
}

// ---- карта кода (M5.6) ----

/** Секрет в сигнатуре константы `API_TOKEN` (src/b.rs): сервер маскирует его до записи. */
export const CODE_SECRET = `ghp_${'A1b2C3d4E5'.repeat(4).slice(0, 36)}`;
export const CODE_COMMIT = '9f8e7d6c5b4a39281706f5e4d3c2b1a098765432';
export const CODE_BRANCH = 'm5.6-e2e';
export const CODE_MACHINE = 'e2e-code-box';
/** Символ, к которому привязана заметка `refs: [путь#qual]`. */
export const CODE_REF = 'src/a.rs#Store::open';
export const CODE_NOTE_TITLE = 'Заметка E2E о методе open';
export const CODE_TASK_TITLE = 'Задача E2E: замок файла src/a.rs';
/** Заметка со ссылками и на файл, и на его символ: в ответе карточки — две строки одного `id`. */
export const CODE_BOTH_TITLE = 'Заметка E2E о файле и методе open';

type Sym = [name: string, qual: string, kind: string, line: number, sig?: string];
type Ref = [name: string, kind: string, count: number];

/** Файлы карты E2E: 6 разобранных (40 символов) и 2 пропущенных (минифицирован, слишком большой). */
export const CODE_FILES: { path: string; lang: string; symbols: Sym[]; refs: Ref[]; skip?: string }[] = [
  {
    path: 'src/a.rs',
    lang: 'rust',
    symbols: [
      ['Store', 'Store', 'struct', 1, 'pub struct Store'],
      ['open', 'Store::open', 'method', 5, 'pub fn open(path: &Path) -> Result<Store, Error>'],
      ['close', 'Store::close', 'method', 12, 'pub fn close(self)'],
      ['get', 'Store::get', 'method', 16, 'pub fn get(&self, key: &str) -> Option<&[u8]>'],
      ['put', 'Store::put', 'method', 22, 'pub fn put(&mut self, key: &str, v: Vec<u8>)'],
      ['delete', 'Store::delete', 'method', 28, 'pub fn delete(&mut self, key: &str) -> bool'],
      ['Error', 'Error', 'enum', 34, 'pub enum Error'],
      ['DEFAULT_PATH', 'DEFAULT_PATH', 'const', 40, 'pub const DEFAULT_PATH: &str = "data.db";'],
    ],
    refs: [['Error', 'type', 2], ['normalize', 'call', 1]],
  },
  {
    path: 'src/b.rs',
    lang: 'rust',
    symbols: [
      ['Index', 'Index', 'struct', 1, 'pub struct Index'],
      ['build', 'Index::build', 'method', 4, 'pub fn build(store: &Store) -> Index'],
      ['search', 'Index::search', 'method', 10, 'pub fn search(&self, q: &str) -> Vec<Hit>'],
      ['merge', 'Index::merge', 'method', 16, 'pub fn merge(&mut self, other: Index)'],
      ['rank', 'rank', 'function', 22, 'fn rank(hits: &mut [Hit])'],
      ['normalize', 'normalize', 'function', 28, 'pub fn normalize(s: &str) -> String'],
      ['API_TOKEN', 'API_TOKEN', 'const', 34, `const API_TOKEN: &str = "${CODE_SECRET}";`],
    ],
    refs: [['open', 'call', 2], ['Store', 'type', 3], ['get', 'call', 4]],
  },
  {
    path: 'src/c.rs',
    lang: 'rust',
    symbols: [
      ['Config', 'Config', 'struct', 1, 'pub struct Config'],
      ['load', 'Config::load', 'method', 5, 'pub fn load(path: &Path) -> Config'],
      ['save', 'Config::save', 'method', 11, 'pub fn save(&self) -> io::Result<()>'],
      ['parse_args', 'parse_args', 'function', 17, 'fn parse_args() -> Args'],
      ['main', 'main', 'function', 23, 'fn main()'],
    ],
    refs: [['Store', 'type', 1], ['open', 'call', 1], ['Index', 'type', 1], ['search', 'call', 2]],
  },
  {
    path: 'web/app.ts',
    lang: 'typescript',
    symbols: [
      ['App', 'App', 'class', 1, 'export class App'],
      ['start', 'App.start', 'method', 4, 'start(): void'],
      ['render', 'render', 'function', 12, 'export function render(el: HTMLElement): void'],
      ['mount', 'mount', 'function', 18, 'export function mount(): App'],
      ['Router', 'Router', 'class', 24, 'export class Router'],
      ['go', 'Router.go', 'method', 27, 'go(to: string): void'],
      ['back', 'Router.back', 'method', 31, 'back(): void'],
      ['View', 'View', 'interface', 36, 'export interface View'],
    ],
    refs: [['fmt', 'call', 3], ['debounce', 'call', 1], ['Queue', 'type', 1]],
  },
  {
    path: 'web/util.ts',
    lang: 'typescript',
    symbols: [
      ['fmt', 'fmt', 'function', 1, 'export function fmt(n: number): string'],
      ['clamp', 'clamp', 'function', 5, 'export function clamp(x: number, a: number, b: number): number'],
      ['debounce', 'debounce', 'function', 9, 'export function debounce(f: () => void, ms: number)'],
      ['throttle', 'throttle', 'function', 15, 'export function throttle(f: () => void, ms: number)'],
      ['uid', 'uid', 'function', 21, 'export const uid = (): string =>'],
      ['Queue', 'Queue', 'class', 25, 'export class Queue<T>'],
    ],
    refs: [],
  },
  {
    path: 'tools/gen.py',
    lang: 'python',
    symbols: [
      ['Generator', 'Generator', 'class', 1, 'class Generator:'],
      ['run', 'Generator.run', 'method', 4, 'def run(self) -> None:'],
      ['emit', 'emit', 'function', 10, 'def emit(line: str) -> None:'],
      ['main', 'main', 'function', 14, 'def main() -> int:'],
      ['parse_cli', 'parse_cli', 'function', 18, 'def parse_cli(argv: list[str]) -> dict:'],
      ['write_file', 'write_file', 'function', 24, 'def write_file(path: str, text: str) -> None:'],
    ],
    refs: [['emit', 'call', 2], ['normalize', 'call', 1]],
  },
  { path: 'web/vendor.min.js', lang: 'javascript', symbols: [], refs: [], skip: 'minified' },
  { path: 'assets/big.ts', lang: 'typescript', symbols: [], refs: [], skip: 'too_large' },
];

const hexOf = (n: number, len: number) => n.toString(16).padStart(len, '0');

/**
 * Карта кода решения E2E по токену администратора — как её строит `zam code sync`, но без
 * бинарника `zam` (синтетические символы): `POST /code/sync` с манифестом, затем
 * `PUT /code/files` файлов из `need`. Повторный вызов восстанавливает карту после очистки.
 */
export async function seedCodeMap(base: string, token: string): Promise<void> {
  const files = CODE_FILES.map((f, i) => ({ ...f, hash: hexOf(0xc0de00 + i, 64) }));
  const meta = { solution: SOLUTION_GUID, commit: CODE_COMMIT, branch: CODE_BRANCH, machine: CODE_MACHINE };
  const sync = await api<{ need: string[] }>(base, token, 'POST', 'code/sync', json({
    ...meta,
    manifest: files.map((f) => ({ path: f.path, hash: f.hash })),
  }));
  const need = new Set(sync.need);
  let body = 0;
  const upload = files
    .filter((f) => need.has(f.path))
    .map((f) => ({
      path: f.path,
      lang: f.lang,
      hash: f.hash,
      size: f.skip === 'too_large' ? 4_000_000 : 1200,
      parse_state: f.skip ? 'skipped' : 'ok',
      ...(f.skip ? { skip_reason: f.skip } : {}),
      symbols: f.symbols.map(([name, qual, kind, line, sig]) => ({
        name,
        qual,
        kind,
        line,
        end_line: line + 4,
        ...(sig ? { sig } : {}),
        body_hash: hexOf(++body, 16),
        exported: true,
      })),
      refs: f.refs.map(([name, kind, count]) => ({ name, kind, count, first_line: 1 })),
    }));
  if (upload.length) await api(base, token, 'PUT', 'code/files', json({ ...meta, files: upload }));
}

/**
 * Знания о коде: заметка со ссылкой на символ {@link CODE_REF} и задача о файле src/a.rs
 * (задача, а не insight: журнал Insights в journals.spec считает свои записи).
 */
export async function seedCodeKnowledge(base: string, token: string): Promise<{ note: string; task: string; both: string }> {
  const note = await api<Node>(base, token, 'POST', 'nodes', json({
    space: SOLUTION_GUID,
    path: `E2E/${CODE_NOTE_TITLE}`,
    kind: 'note',
    content: `---\nrefs: ["${CODE_REF}"]\n---\n# ${CODE_NOTE_TITLE}\n\nОткрытие хранилища берёт замок файла.\n`,
  }));
  const task = await api<EntryResp>(base, token, 'POST', `spaces/${SOLUTION_GUID}/entries`, json({
    kind: 'task',
    title: CODE_TASK_TITLE,
    body: 'Проверить замок файла при открытии хранилища.',
    refs: { files: [{ path: 'src/a.rs' }] },
  }));
  const both = await api<Node>(base, token, 'POST', 'nodes', json({
    space: SOLUTION_GUID,
    path: `E2E/${CODE_BOTH_TITLE}`,
    kind: 'note',
    content: `---\nrefs: ["src/a.rs", "${CODE_REF}"]\n---\n# ${CODE_BOTH_TITLE}\n\nФайл и его метод open.\n`,
  }));
  return { note: note.id, task: task.id, both: both.id };
}

// ---- идеи (M5.7) ----

/** Решение журнала «Идеи»: своё, отдельно от решения E2E (сценарии идей ставят задачи и вопросы). */
export const IDEAS_GUID = '0e2e0000-0000-4000-8000-000000000057';
export const IDEAS_NAME = 'Идеи E2E';
/** Участник решения идей (`rw`, одобрен): решение «общее» — предупреждение «Начать» идеи Knowledge. */
export const IDEAS_MEMBER = 'e2e-ideas-member';
export const IDEA_HUMAN_TITLE = 'Идея E2E: звездолётная карта связей';
/** Слово только из идеи человека — для Ctrl+K. */
export const IDEA_WORD = 'звездолётная';
export const IDEA_AGENT_TITLE = 'Идея агента E2E: кэш миниатюр';
export const IDEA_PROPOSED_TITLE = 'Идея E2E: экспорт графа в SVG';
export const IDEA_KNOWLEDGE_TITLE = 'Общая идея E2E: шаблоны заметок';

export interface IdeaSeed {
  human: string;
  agent: string;
  proposed: string;
  question: string;
  knowledge: string;
}

/**
 * Решение «Идеи E2E» (общее: одобренный участник `rw`) и идеи по токену администратора (агент):
 * идея человека (по его словам, `origin: human`), своя идея агента, идея с вопросом-предложением
 * (`propose`) и общая идея Knowledge.
 */
export async function seedIdeas(base: string, token: string, admin: AdminCall): Promise<IdeaSeed> {
  const post = <T>(path: string, body: unknown) => api<T>(base, token, 'POST', path, json(body));
  await post('solutions', { agent_solution_id: IDEAS_GUID, name: IDEAS_NAME });
  const u = await admin<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', { name: IDEAS_MEMBER });
  const r = await fetch(`${base}api/v1/solutions`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${u.token.plaintext}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ agent_solution_id: IDEAS_GUID, name: IDEAS_NAME }),
  });
  if (r.status !== 202) throw new Error(`запрос доступа ${IDEAS_MEMBER}: HTTP ${r.status} ${await r.text()}`);
  await admin('POST', `solutions/${IDEAS_GUID}/access/${u.user.id}/approve`, { access: 'rw' });

  const sp = `spaces/${IDEAS_GUID}/entries`;
  const idea = (title: string, origin: 'human' | 'agent', extra: Record<string, unknown> = {}, where = sp) =>
    post<EntryResp>(where, { kind: 'idea', title, body: `${title}: замысел.`, ...extra, data: { origin, ...((extra.data as object) ?? {}) } });
  const human = await idea(IDEA_HUMAN_TITLE, 'human', {
    body: `Показывать ${IDEA_WORD} карту связей заметок.`,
    tags: ['ui'],
    data: { priority: 'high' },
  });
  const agent = await idea(IDEA_AGENT_TITLE, 'agent');
  const proposed = await idea(IDEA_PROPOSED_TITLE, 'human');
  const pr = await post<{ question: { id: string } }>(`entries/${proposed.id}/propose`, {
    text: 'Сейчас правим граф — экспорт ляжет рядом.',
  });
  const knowledge = await idea(IDEA_KNOWLEDGE_TITLE, 'human', {}, 'spaces/knowledge/entries');
  return { human: human.id, agent: agent.id, proposed: proposed.id, question: pr.question.id, knowledge: knowledge.id };
}
