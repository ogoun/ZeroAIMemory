// E2E WebUI: установленный Microsoft Edge (браузеры Playwright не скачиваются),
// сервер — release zam-server на 0.0.0.0, страница открывается по LAN-IP (не secure context).
import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';

const tmp = fileURLToPath(new URL('../../.tmp/e2e/', import.meta.url));

export default defineConfig({
  testDir: '.',
  outputDir: `${tmp}test-results`,
  globalSetup: './global-setup.ts',
  reporter: 'list',
  workers: 1,
  timeout: 60_000,
  use: {
    channel: 'msedge',
    headless: true,
    trace: 'retain-on-failure',
  },
});
