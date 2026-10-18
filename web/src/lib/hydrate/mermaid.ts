// Ленивый модуль диаграмм: Mermaid в режиме `securityLevel: 'strict'`, итоговый SVG
// проходит DOMPurify (профили svg и svgFilters) — это единственное место, где клиент
// сам вставляет разметку.
// Сервер отдаёт исходник как `<pre class="mermaid-src"><code>…</code></pre>`.
import DOMPurify from 'dompurify';
import mermaid from 'mermaid';

let seq = 0;
let theme: 'default' | 'dark' | null = null;

function init(): void {
  const dark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const want = dark ? 'dark' : 'default';
  if (theme === want) return;
  theme = want;
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    theme: want,
    // Mermaid 12 по умолчанию раскладывает через ELK, а elkjs (EPL-2.0) в бандл не входит
    // (см. elk-stub.ts) — раскладка dagre
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

/** Заменяет каждый `pre.mermaid-src` внутри `root` диаграммой; при ошибке — исходник с пометкой. */
export async function renderMermaid(root: HTMLElement): Promise<void> {
  init();
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
