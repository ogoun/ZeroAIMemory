// Обёртка запуска Playwright: временные файлы (профиль браузера, артефакты) — внутри проекта,
// в <repo>/.tmp/e2e/tmp, а не в системном %TEMP%. Аргументы передаются в playwright как есть.
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const tmp = fileURLToPath(new URL('../../.tmp/e2e/tmp/', import.meta.url));
mkdirSync(tmp, { recursive: true });

const cli = createRequire(import.meta.url).resolve('@playwright/test/cli');
const child = spawn(
  process.execPath,
  [cli, 'test', '-c', 'e2e/playwright.config.ts', ...process.argv.slice(2)],
  { stdio: 'inherit', env: { ...process.env, TMP: tmp, TEMP: tmp, TMPDIR: tmp } },
);
child.on('exit', (code, sig) => process.exit(code ?? (sig ? 1 : 0)));
