// Тестовые заметки E2E: создаются через REST API по первому токену администратора.
// Пространство Knowledge своего пользователя администратора, папка «E2E».
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
export async function seed(base: string, token: string): Promise<{ media: string; second: string }> {
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
  return { media: media.id, second: second.id };
}

// ---- журналы решения, Vault и совместный доступ (M3) ----

/** GUID решения E2E (каталог данных пересоздаётся на каждый прогон). */
export const SOLUTION_GUID = '0e2e0000-0000-4000-8000-000000000013';
export const SOLUTION_NAME = 'E2E решение';
/** Значение секрета Vault: должно показываться только по «Показать». */
export const VAULT_SECRET = 'e2e-vault-value-7Qm2';
export const MEMBER_NAME = 'e2e-member';

/** Запрос от имени администратора (cookie-сессия + CSRF): админ-API токеном недоступно. */
export type AdminCall = <T>(method: string, path: string, body?: unknown) => Promise<T>;

export interface JournalSeed {
  /** Insight для замещения. */
  oldInsight: string;
  /** Гипотеза для повышения до theorem. */
  hypothesis: string;
  memberId: string;
  memberToken: string;
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

  // второй пользователь запрашивает доступ к решению: 202 pending
  const member = await admin<{ user: { id: string }; token: { plaintext: string } }>('POST', 'admin/users', {
    name: MEMBER_NAME,
  });
  const r = await fetch(`${base}api/v1/solutions`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${member.token.plaintext}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ agent_solution_id: SOLUTION_GUID, name: SOLUTION_NAME }),
  });
  if (r.status !== 202) throw new Error(`запрос доступа участника: HTTP ${r.status} ${await r.text()}`);

  return {
    oldInsight: old.id,
    hypothesis: hyp.id,
    memberId: member.user.id,
    memberToken: member.token.plaintext,
  };
}
