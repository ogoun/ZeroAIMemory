import type { Plugin } from 'vite';

/** Плагин сборки: сверка строк стартовой загрузки size-limit с dist/index.html (см. start-budget-plugin.mjs). */
export function startBudget(): Plugin;
