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

const NAMED: Record<string, string> = { quot: '"', amp: '&', lt: '<', gt: '>', apos: "'", nbsp: ' ' };
const ENTITY = /&(?:#(\d{1,7})|#x([0-9a-f]{1,6})|(quot|amp|lt|gt|apos|nbsp));/gi;

function entityChar(m: string, dec?: string, hex?: string, name?: string): string {
  if (name) return NAMED[name.toLowerCase()] ?? m;
  const cp = dec ? Number(dec) : parseInt(hex!, 16);
  return cp > 0 && cp <= 0x10ffff && (cp < 0xd800 || cp > 0xdfff) ? String.fromCodePoint(cp) : m;
}

/**
 * Диаграммы (`aria-roledescription` SVG), у которых Mermaid 12 при `htmlLabels: false` оставляет
 * сущности подписей буквально. Остальные (sequence, pie, journey, timeline, quadrantChart,
 * gitGraph, xychart, packet, radar) расшифровывают их сами — повторная расшифровка исказила бы
 * `#38;amp#59;` («&amp;» → «&»). Список — по пробе `mermaid.render` всех типов (журнал этапа M5.6,
 * задача 9B); новый тип вне списка не трогается.
 */
const LITERAL_ENTITIES = new Set([
  'flowchart-v2', 'stateDiagram', 'classDiagram', 'er', 'mindmap', 'block', 'kanban', 'requirement', 'c4',
  'architecture',
]);

/**
 * Сущности подписей (`#35;`, `#quot;` исходника Mermaid превращает в `&#35;`, `&quot;`) у типов
 * {@link LITERAL_ENTITIES} попадают в `<text>` SVG буквально («a&#58;b» вместо «a:b»), а перенос
 * строк режет их и между `<tspan>` (`&#9` | `1;`). Расшифровка — один проход по всему тексту
 * каждого `<text>`: символ — в узел, где сущность начинается, её остаток из следующих узлов
 * убирается. Меняются только текстовые узлы (`data`): разметка не появляется.
 */
function decodeLabels(root: Element): void {
  const role = root.querySelector('svg')?.getAttribute('aria-roledescription') ?? '';
  if (!LITERAL_ENTITIES.has(role)) return;
  for (const el of root.querySelectorAll('text')) {
    const nodes: Text[] = [];
    const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    for (let n = walk.nextNode(); n; n = walk.nextNode()) nodes.push(n as Text);
    const all = nodes.map((n) => n.data).join('');
    if (!all.includes('&')) continue;
    // новое содержимое узлов: начало каждого — по смещению в общем тексте
    const out = nodes.map(() => '');
    const starts: number[] = [];
    let acc = 0;
    for (const n of nodes) {
      starts.push(acc);
      acc += n.data.length;
    }
    const nodeAt = (pos: number) => {
      let i = starts.length - 1;
      while (i > 0 && starts[i] > pos) i--;
      return i;
    };
    let pos = 0;
    const copy = (to: number) => {
      // текст без сущностей — по своим узлам, границы узлов сохраняются
      while (pos < to) {
        const i = nodeAt(pos);
        const end = Math.min(to, i + 1 < starts.length ? starts[i + 1] : all.length);
        out[i] += all.slice(pos, end);
        pos = end;
      }
    };
    for (const m of all.matchAll(ENTITY)) {
      copy(m.index);
      out[nodeAt(m.index)] += entityChar(m[0], m[1], m[2], m[3]);
      pos = m.index + m[0].length;
    }
    copy(all.length);
    nodes.forEach((n, i) => {
      if (n.data !== out[i]) n.data = out[i];
    });
  }
}

export interface RenderOpts {
  theme?: Theme;
  /**
   * Готовые SVG по `тема + исходник` (кэш вызывающего, «Обзор» карты кода): найденный —
   * вставляется копией (`cloneNode`) без `mermaid.render` и разбора разметки, нарисованный —
   * кладётся. Элементы — уже очищенный DOMPurify результат этого модуля, вне документа.
   */
  cache?: Map<string, SVGElement>;
}

/**
 * Заменяет каждый `pre.mermaid-src` внутри `root` диаграммой; при ошибке — исходник с пометкой.
 * `theme` — явная тема (печать — `default`); без неё — тема страницы. Вызовы идут по очереди:
 * каждый ставит свою тему, поэтому после печатного вызова экран снова рисуется в теме страницы.
 */
export function renderMermaid(root: HTMLElement, o: RenderOpts = {}): Promise<void> {
  const run = queue.then(() => render(root, o.theme ?? pageTheme(), o.cache));
  queue = run.catch(() => undefined);
  return run;
}

async function render(root: HTMLElement, want: Theme, cache?: Map<string, SVGElement>): Promise<void> {
  for (const pre of root.querySelectorAll<HTMLElement>('pre.mermaid-src')) {
    // повторный вызов для того же документа (смена bind:this) не рендерит блок дважды
    if (!pre.isConnected || pre.dataset.zamMermaid) continue;
    pre.dataset.zamMermaid = '1';
    const src = pre.textContent ?? '';
    const box = document.createElement('div');
    box.className = 'mermaid-diagram';
    const key = `${want}
${src}`;
    try {
      const ready = cache?.get(key);
      if (ready) box.append(ready.cloneNode(true));
      else {
        init(want);
        const { svg } = await mermaid.render(`zam-mermaid-${++seq}`, src);
        box.innerHTML = sanitize(svg);
        if (!box.querySelector('svg')) throw new Error('пустой результат');
        decodeLabels(box);
        cache?.set(key, box.querySelector('svg')!.cloneNode(true) as SVGElement);
      }
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
