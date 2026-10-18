// «Оживление» санитизированного HTML документа без внешних библиотек:
// якоря заголовков, кнопки «Копировать» у блоков кода, подсказки битых вики-ссылок,
// признак увеличения у картинок. Обработчики кликов — делегированием в DocView.
// Формулы и диаграммы — ленивые модули katex.ts и mermaid.ts.

import { wikiTarget } from '../router';

/** Кнопка копирования кода: `data-copy` — признак для делегирования. */
const COPY_LABEL = 'Копировать';

export function hydrateDom(root: HTMLElement): void {
  // Якоря: ссылка «#» рядом с заголовком; href — сам id (обрабатывает DocView).
  for (const h of root.querySelectorAll<HTMLElement>('h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]')) {
    if (h.querySelector(':scope > a.heading-anchor')) continue;
    const a = document.createElement('a');
    a.className = 'heading-anchor';
    a.setAttribute('href', `#${encodeURIComponent(h.id)}`);
    a.dataset.anchor = h.id;
    a.setAttribute('aria-label', `Ссылка на раздел «${h.textContent ?? ''}»`);
    a.textContent = '#';
    h.append(a);
  }

  // Блоки кода (кроме исходника Mermaid): обёртка с кнопкой.
  for (const pre of root.querySelectorAll<HTMLElement>('pre:not(.mermaid-src)')) {
    if (pre.parentElement?.classList.contains('code-block')) continue;
    const wrap = document.createElement('div');
    wrap.className = 'code-block';
    pre.replaceWith(wrap);
    wrap.append(pre);
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'copy-code';
    btn.dataset.copy = '';
    btn.textContent = COPY_LABEL;
    wrap.append(btn);
  }

  for (const a of root.querySelectorAll<HTMLAnchorElement>('a.wikilink.broken')) {
    a.title = 'заметка не найдена';
    a.setAttribute('aria-disabled', 'true');
  }

  for (const img of root.querySelectorAll<HTMLImageElement>('img')) {
    img.classList.add('zoomable');
  }
}

/**
 * Внутренние ссылки документа → полные адреса SPA, чтобы работали и «открыть в новой
 * вкладке», и копирование ссылки: вики-ссылки `#/n/<id>[#якорь]` — в контексте текущего
 * пользователя и пространства, ссылки на якоря (`#fn-1`, `#h-…`) — на этот же документ.
 */
export function rewriteLinks(
  root: HTMLElement,
  wiki: (node: string, anchor: string | null) => string | null,
  self: (anchor: string) => string,
): void {
  for (const a of root.querySelectorAll<HTMLAnchorElement>('a[href^="#"]')) {
    const raw = a.getAttribute('href') ?? '';
    const w = wikiTarget(raw);
    if (w) {
      const to = wiki(w.node, w.anchor);
      if (to) a.setAttribute('href', to);
    } else if (!raw.startsWith('#/') && raw.length > 1) {
      a.setAttribute('href', self(raw.slice(1)));
    }
  }
}

/** Текст блока кода для кнопки копирования. */
export function codeOf(btn: HTMLElement): string {
  const code = btn.closest('.code-block')?.querySelector('pre code, pre');
  return code?.textContent ?? '';
}

/** Временная подпись кнопки после копирования. */
export function flashButton(btn: HTMLElement, text: string): void {
  btn.textContent = text;
  window.setTimeout(() => {
    btn.textContent = COPY_LABEL;
  }, 1500);
}
