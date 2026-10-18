// Полное «оживление» отрендеренного сервером HTML — общее для просмотра документа
// (DocView) и предпросмотра редактора (Editor): разметка якорей и кода, ссылки SPA,
// затем ленивые модули формул (KaTeX) и диаграмм (Mermaid).

import { hydrateDom, rewriteLinks } from './dom';

export interface HydrateOpts {
  /** Адрес SPA для вики-ссылки сервера (см. `rewriteLinks`). */
  wiki: (node: string, anchor: string | null) => string | null;
  /** Адрес SPA для якоря этого же документа. */
  self: (anchor: string) => string;
  /** `false` — результат устарел (документ сменился), ленивые модули не применяются. */
  alive: () => boolean;
}

/**
 * Синхронная часть выполняется сразу; промис завершается после формул и диаграмм.
 * Результат: `false` — не загрузился модуль формул или диаграмм.
 */
export async function hydrateRendered(el: HTMLElement, o: HydrateOpts): Promise<boolean> {
  hydrateDom(el);
  rewriteLinks(el, o.wiki, o.self);
  const jobs: Promise<void>[] = [];
  if (el.querySelector('.math')) {
    jobs.push(import('./katex').then((m) => (o.alive() ? m.renderMath(el) : undefined)));
  }
  if (el.querySelector('pre.mermaid-src')) {
    jobs.push(import('./mermaid').then((m) => (o.alive() ? m.renderMermaid(el) : undefined)));
  }
  if (jobs.length === 0) return true;
  const res = await Promise.allSettled(jobs);
  return !res.some((x) => x.status === 'rejected');
}
