// Плагин сборки: строки стартовой загрузки `.size-limit.json` (`start`, `api`, `start+api`)
// сверяются с тем, что `dist/index.html` грузит при старте (`build/start-budget.ts`, M7.3, ревью r2
// задачи 10a, N1): новый общий чанк старта вне строк или шаблон без файлов — ошибка сборки, а не
// молчаливый недосчёт size-limit. Модуль на JavaScript с объявлением типов рядом
// (start-budget-plugin.d.mts): vite.config.ts проверяется без типов Node.
import { readdirSync, readFileSync } from 'node:fs';
import { checkStart } from './start-budget.ts';

/** @returns {import('vite').Plugin} */
export function startBudget() {
  return {
    name: 'zam-start-budget',
    apply: 'build',
    closeBundle() {
      const files = readdirSync('dist/assets').map((f) => `dist/assets/${f}`);
      const lines = JSON.parse(readFileSync('.size-limit.json', 'utf8'));
      const errors = checkStart(readFileSync('dist/index.html', 'utf8'), lines, files);
      if (errors.length) {
        throw new Error(`стартовая загрузка: строки .size-limit.json не сходятся с dist/index.html:\n${errors.join('\n')}`);
      }
    },
  };
}
