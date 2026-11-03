// Ленивый модуль диаграмм: Mermaid в режиме `securityLevel: 'strict'`, итоговый SVG
// проходит DOMPurify (профили svg и svgFilters) — это единственное место, где клиент
// сам вставляет разметку.
// Сервер отдаёт исходник как `<pre class="mermaid-src"><code>…</code></pre>`.
import DOMPurify from 'dompurify';
import mermaid from 'mermaid';

type Theme = 'default' | 'dark';

let seq = 0;
let theme: Theme | null = null;
/** Очередь вызовов: тема Mermaid глобальна, печать (`default`) не должна вклиниться в экран. */
let queue: Promise<void> = Promise.resolve();

/** Тема страницы — по теме ОС, как токены цвета WebUI (app.css). */
function pageTheme(): Theme {
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'default';
}

function init(want: Theme): void {
  if (theme === want) return;
  theme = want;
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    theme: want,
    // Mermaid 12 по умолчанию раскладывает через ELK; по умолчанию оставляем dagre, а ELK
    // (elkjs, EPL-2.0, ленивый чанк) включается самой диаграммой: `layout: elk` во frontmatter
    // или `%%{init: {"layout": "elk"}}%%`
    layout: 'dagre',
    state: { layout: 'dagre' },
    // подписи — текст SVG, без foreignObject с HTML внутри
    htmlLabels: false,
    flowchart: { htmlLabels: false },
    fontFamily: 'system-ui, -apple-system, "Segoe UI", sans-serif',
  });
}

function sanitize(svg: string): string {
  return DOMPurify.sanitize(svg, { USE_PROFILES: { svg: true, svgFilters: true } });
}

/**
 * Заменяет каждый `pre.mermaid-src` внутри `root` диаграммой; при ошибке — исходник с пометкой.
 * `theme` — явная тема (печать — `default`); без неё — тема страницы. Вызовы идут по очереди:
 * каждый ставит свою тему, поэтому после печатного вызова экран снова рисуется в теме страницы.
 */
export function renderMermaid(root: HTMLElement, o: { theme?: Theme } = {}): Promise<void> {
  const run = queue.then(() => render(root, o.theme ?? pageTheme()));
  queue = run.catch(() => undefined);
  return run;
}

async function render(root: HTMLElement, want: Theme): Promise<void> {
  init(want);
  for (const pre of root.querySelectorAll<HTMLElement>('pre.mermaid-src')) {
    // повторный вызов для того же документа (смена bind:this) не рендерит блок дважды
    if (!pre.isConnected || pre.dataset.zamMermaid) continue;
    pre.dataset.zamMermaid = '1';
    const src = pre.textContent ?? '';
    const box = document.createElement('div');
    box.className = 'mermaid-diagram';
    try {
      const { svg } = await mermaid.render(`zam-mermaid-${++seq}`, src);
      box.innerHTML = sanitize(svg);
      if (!box.querySelector('svg')) throw new Error('пустой результат');
      box.setAttribute('role', 'img');
      box.setAttribute('aria-label', 'Диаграмма');
      pre.replaceWith(box);
    } catch (e) {
      // временный узел Mermaid при ошибке может остаться в body
      document.getElementById(`dzam-mermaid-${seq}`)?.remove();
      const note = document.createElement('p');
      note.className = 'mermaid-error';
      note.textContent = `Ошибка диаграммы: ${e instanceof Error ? e.message : String(e)}`;
      pre.classList.add('mermaid-failed');
      pre.before(note);
    }
  }
}
