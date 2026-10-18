// Ленивый модуль формул: KaTeX и его стили (шрифты — файлы из бандла, 'self').
// Сервер размечает формулы как `<span class="math" data-math-style="inline|display">`
// и ```` ```math ```` как `<div class="math" data-math-style="display">`; текст — TeX.
import katex from 'katex';
import 'katex/dist/katex.min.css';

/** Рендерит все `.math` внутри `root`; ошибки разбора KaTeX показывает сам. */
export function renderMath(root: HTMLElement): void {
  for (const el of root.querySelectorAll<HTMLElement>('.math')) {
    if (el.dataset.zamMath) continue;
    const tex = el.textContent ?? '';
    try {
      katex.render(tex, el, {
        displayMode: el.dataset.mathStyle === 'display',
        throwOnError: false,
        strict: 'ignore',
      });
    } catch {
      // ошибка не синтаксиса формулы (например, предел размера) — оставляем исходник
      el.textContent = tex;
      el.classList.add('math-error');
      el.title = 'Не удалось отрисовать формулу';
    }
    el.dataset.zamMath = '1';
  }
}
