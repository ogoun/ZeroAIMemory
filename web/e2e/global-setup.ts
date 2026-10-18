// Запуск release zam-server для E2E: bind 0.0.0.0:<порт>, каталог данных в .tmp/e2e,
// адрес страницы — LAN-IP машины. Возвращает функцию остановки (teardown).
import { spawn } from 'node:child_process';
import { createWriteStream, existsSync, mkdirSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';
import { fileURLToPath } from 'node:url';
import { request } from '@playwright/test';
import { seed } from './seed';
import { ADMIN_STATE } from './session';

const repo = fileURLToPath(new URL('../../', import.meta.url));
const tmp = `${repo}.tmp/e2e/`;
export const ADMIN_PASSWORD = 'e2e-admin-pass';

// Первый не-loopback IPv4 (частные сети в приоритете); E2E_HOST переопределяет.
function lanIp(): string {
  if (process.env.E2E_HOST) return process.env.E2E_HOST;
  const all = Object.values(networkInterfaces())
    .flat()
    .filter((a) => a && a.family === 'IPv4' && !a.internal)
    .map((a) => a!.address);
  const priv = all.find((ip) => /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(ip));
  const ip = priv ?? all[0];
  if (!ip) throw new Error('нет LAN-адреса IPv4; задайте E2E_HOST');
  return ip;
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = createServer();
    s.once('error', reject);
    s.listen(0, '0.0.0.0', () => {
      const port = (s.address() as { port: number }).port;
      s.close(() => resolve(port));
    });
  });
}

async function waitHealth(url: string, alive: () => boolean): Promise<void> {
  const t0 = Date.now();
  while (Date.now() - t0 < 15_000) {
    if (!alive()) throw new Error('zam-server завершился при старте, см. .tmp/e2e/server.log');
    try {
      const r = await fetch(`${url}api/v1/health`);
      if (r.ok) return;
    } catch {
      // ещё не слушает
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${url}api/v1/health не ответил за 15 с`);
}

export default async function globalSetup(): Promise<() => Promise<void>> {
  const exe = process.env.E2E_SERVER ??
    `${repo}target/release/zam-server${process.platform === 'win32' ? '.exe' : ''}`;
  if (!existsSync(exe)) {
    throw new Error(`нет ${exe}: соберите npm run build и cargo build --release -p zam-server`);
  }
  const data = `${tmp}data`;
  rmSync(data, { recursive: true, force: true });
  mkdirSync(tmp, { recursive: true });

  const port = Number(process.env.E2E_PORT) || (await freePort());
  // Окружение без ZAM_* родителя: сборочные и клиентские переменные не влияют на сервер.
  const env: NodeJS.ProcessEnv = Object.fromEntries(
    Object.entries(process.env).filter(([k]) => !k.toUpperCase().startsWith('ZAM_')),
  );
  env.ZAM_ADMIN_PASSWORD = ADMIN_PASSWORD;
  const base = `http://${lanIp()}:${port}/`;
  const child = spawn(exe, ['serve', '--bind', `0.0.0.0:${port}`, '--data-dir', data], {
    env,
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  const log = createWriteStream(`${tmp}server.log`);
  try {
    // Журнал сервера без строки с первым токеном; сам токен — для наполнения данными.
    let rest = '';
    let token = '';
    child.stderr!.on('data', (chunk: Buffer) => {
      const lines = (rest + chunk.toString('utf8')).split('\n');
      rest = lines.pop() ?? '';
      for (const l of lines) {
        if (l.startsWith('ZeroAIMemoryToken=')) token = l.slice('ZeroAIMemoryToken='.length).trim();
        else log.write(`${l}\n`);
      }
    });

    await waitHealth(base, () => child.exitCode === null);
    if (!token) throw new Error('сервер не напечатал первый токен администратора');
    const ids = await seed(base, token);
    // один вход на весь прогон (см. session.ts)
    const rc = await request.newContext({ baseURL: base });
    const r = await rc.post('api/v1/auth/login', { data: { login: 'admin', password: ADMIN_PASSWORD } });
    if (!r.ok()) throw new Error(`вход администратора: HTTP ${r.status()}`);
    await rc.storageState({ path: ADMIN_STATE });
    await rc.dispose();
    process.env.E2E_BASE_URL = base;
    process.env.E2E_ADMIN_PASSWORD = ADMIN_PASSWORD;
    process.env.E2E_NOTE_MEDIA = ids.media;
    process.env.E2E_NOTE_SECOND = ids.second;
    // токен администратора — для правок «из другого места» в сценариях конфликта
    process.env.E2E_ADMIN_TOKEN = token;
  } catch (e) {
    child.kill();
    log.end();
    throw e;
  }

  return async () => {
    if (child.exitCode === null) {
      child.kill();
      await new Promise((r) => child.once('exit', r));
    }
    log.end();
  };
}
