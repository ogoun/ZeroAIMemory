// Экспорт пространства из WebUI (M6 §12б.5, задача 18b1): пункт «Экспорт (.zip)» в меню
// пространства скачивает архив с manifest.json и заметками пространства.
import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import JSZip from 'jszip';
import { ADMIN_STATE } from './session';

const base = () => {
  const v = process.env.E2E_BASE_URL;
  if (!v) throw new Error('E2E_BASE_URL не задан (global-setup)');
  return v;
};

test.use({ storageState: ADMIN_STATE, acceptDownloads: true });

test('«Экспорт (.zip)» скачивает архив пространства с manifest.json', async ({ page }) => {
  await page.goto(base());
  await expect(page.getByRole('button', { name: 'Выйти' })).toBeVisible();
  const t = page.locator('nav[aria-label="Дерево заметок"]');
  await t.getByRole('button', { name: 'Действия: Knowledge' }).click();
  const [dl] = await Promise.all([
    page.waitForEvent('download'),
    page.getByRole('menuitem', { name: 'Экспорт (.zip)' }).click(),
  ]);
  expect(dl.suggestedFilename()).toMatch(/^zam-knowledge-\d{4}-\d{2}-\d{2}\.zip$/);
  const path = await dl.path();
  const zip = await JSZip.loadAsync(readFileSync(path));
  const manifest = JSON.parse(await zip.file('manifest.json')!.async('string')) as {
    format: number;
    space_kind: string;
    files: number;
  };
  expect(manifest.format).toBe(1);
  expect(manifest.space_kind).toBe('knowledge');
  expect(manifest.files).toBe(Object.keys(zip.files).filter((n) => !zip.files[n].dir).length - 1);
  // заметка из seed.ts — с исходным путём во frontmatter
  const note = await zip.file('E2E/Вторая заметка.md')!.async('string');
  expect(note).toContain('path: "E2E/Вторая заметка"');
});
