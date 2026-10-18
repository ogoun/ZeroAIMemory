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
