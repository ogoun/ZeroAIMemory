// Документ просмотрщика PDF `/viewer/pdf` (M5, задача 9): pdf.js 6 (PDFViewer, текстовый
// слой, поиск PDFFindController) в отдельном документе со своей CSP (web.rs: главная политика
// + 'wasm-unsafe-eval' и worker-src). Безопасность (Review Focus 5):
// - JavaScript PDF не исполняется: песочница скриптов pdf.js (PDFScriptingManager, quickjs)
//   не подключается и не входит в сборку, XFA выключен, `'unsafe-eval'` в CSP нет;
// - `isEvalSupported: false` — опция pdf.js до 5.x (в 6.x путь компиляции шрифтов через eval
//   удалён); передаётся явно на случай отката версии;
// - формы не отрисовываются (AnnotationMode.ENABLE — только ссылки), редактор аннотаций выключен;
// - внешние ссылки PDF — в новой вкладке с rel=noopener noreferrer nofollow;
// - байты файла приходят сообщением от WebUI (protocol.ts): API и выбор пользователя не нужны;
// - источник документа непрозрачный (CSP `sandbox` без allow-same-origin, web.rs): ни DOM
//   WebUI, ни cookie, ни API; статика сервера читается по CORS (`/assets/`, `/pdfjs/` —
//   `Access-Control-Allow-Origin: *`), воркер — классический из blob (см. `startWorker`).
import 'pdfjs-dist/web/pdf_viewer.css';
import './viewer.css';
import * as pdfjsLib from 'pdfjs-dist';
import { PDFJS_DIR, WORKER_FILE } from './assets';
import { accept, type FromViewer, type ToViewer } from './protocol';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const container = $<HTMLDivElement>('viewerContainer');
const pageNum = $<HTMLInputElement>('page-num');
const pageCount = $<HTMLSpanElement>('page-count');
const zoom = $<HTMLSpanElement>('zoom');
const find = $<HTMLInputElement>('find');
const findStatus = $<HTMLSpanElement>('find-status');
const errorBox = $<HTMLParagraphElement>('error');

/** Адрес данных pdf.js от `<base href>` документа (источник сервера, не `self.origin`). */
const dataUrl = (p: string) => new URL(`${PDFJS_DIR}/${p}`, document.baseURI).href;
/** Источник сервера и WebUI: у самого документа источник непрозрачный (`'null'`). */
const serverOrigin = new URL(document.baseURI).origin;

function post(m: FromViewer) {
  window.parent.postMessage(m, serverOrigin);
}

/**
 * Воркер pdf.js. Модульный воркер с адресом сервера из непрозрачного документа не создать
 * (другой источник), модульный из blob — не стартует; классический воркер из blob грузит модуль
 * `import()` по CORS, а ранние сообщения pdf.js копит до его загрузки.
 */
function startWorker(): Worker {
  const url = JSON.stringify(dataUrl(WORKER_FILE));
  const wrap =
    'const q=[];const h=(e)=>q.push(e.data);self.addEventListener("message",h);' +
    `import(${url}).then(()=>{self.removeEventListener("message",h);` +
    'for(const d of q)self.dispatchEvent(new MessageEvent("message",{data:d}));});';
  return new Worker(URL.createObjectURL(new Blob([wrap], { type: 'text/javascript' })));
}

function fail(message: string) {
  errorBox.textContent = `Не удалось открыть PDF: ${message}`;
  errorBox.hidden = false;
  post({ type: 'zam-pdf-error', message });
}

async function main() {
  pdfjsLib.GlobalWorkerOptions.workerPort = startWorker();
  // компоненты просмотрщика берут pdf.js из globalThis.pdfjsLib — до их загрузки
  (globalThis as unknown as { pdfjsLib: typeof pdfjsLib }).pdfjsLib = pdfjsLib;
  const v = await import('pdfjs-dist/web/pdf_viewer.mjs');

  const eventBus = new v.EventBus();
  const linkService = new v.PDFLinkService({
    eventBus,
    externalLinkTarget: v.LinkTarget.BLANK,
    externalLinkRel: 'noopener noreferrer nofollow',
  });
  const findController = new v.PDFFindController({ eventBus, linkService });
  const viewer = new v.PDFViewer({
    container,
    eventBus,
    linkService,
    findController,
    textLayerMode: 1,
    annotationMode: pdfjsLib.AnnotationMode.ENABLE,
    annotationEditorMode: pdfjsLib.AnnotationEditorType.DISABLE,
    enableAutoLinking: false,
    removePageBorders: false,
  });
  linkService.setViewer(viewer);

  let initialPage: number | null = null;
  let doc: pdfjsLib.PDFDocumentProxy | null = null;
  let loading: pdfjsLib.PDFDocumentLoadingTask | null = null;

  const state = () => post({ type: 'zam-pdf-state', page: viewer.currentPageNumber, pages: viewer.pagesCount });
  const showZoom = () => (zoom.textContent = `${Math.round(viewer.currentScale * 100)}%`);

  eventBus.on('pagesinit', () => {
    viewer.currentScaleValue = 'page-width';
    if (initialPage && initialPage <= viewer.pagesCount) viewer.currentPageNumber = initialPage;
    pageCount.textContent = `из ${viewer.pagesCount}`;
    pageNum.max = String(viewer.pagesCount);
    pageNum.value = String(viewer.currentPageNumber);
    showZoom();
    state();
  });
  eventBus.on('pagechanging', (e: { pageNumber: number }) => {
    pageNum.value = String(e.pageNumber);
    state();
  });
  eventBus.on('scalechanging', showZoom);
  eventBus.on('updatefindmatchescount', (e: { matchesCount: { current: number; total: number } }) => {
    const { current, total } = e.matchesCount;
    findStatus.textContent = total ? `${current} из ${total}` : '';
  });
  eventBus.on('updatefindcontrolstate', (e: { state: number; matchesCount: { current: number; total: number } }) => {
    if (e.state === v.FindState.NOT_FOUND) findStatus.textContent = 'не найдено';
    else if (e.matchesCount?.total) findStatus.textContent = `${e.matchesCount.current} из ${e.matchesCount.total}`;
  });

  async function open(data: ArrayBuffer, page: number | null) {
    initialPage = page;
    errorBox.hidden = true;
    await loading?.destroy();
    loading = pdfjsLib.getDocument({
      data: new Uint8Array(data),
      cMapUrl: dataUrl('cmaps/'),
      cMapPacked: true,
      wasmUrl: dataUrl('wasm/'),
      iccUrl: dataUrl('iccs/'),
      enableXfa: false,
      // см. заголовок файла: в pdf.js 6 опции нет, путь eval удалён
      ...({ isEvalSupported: false } as object),
    });
    try {
      doc = await loading.promise;
    } catch (e) {
      fail(e instanceof Error ? e.message : String(e));
      return;
    }
    viewer.setDocument(doc);
    linkService.setDocument(doc, null);
  }

  function goPage(n: number) {
    if (!doc || !Number.isFinite(n)) return;
    viewer.currentPageNumber = Math.min(Math.max(1, Math.trunc(n)), viewer.pagesCount);
  }

  function search(again: boolean, previous: boolean) {
    const query = find.value;
    if (!query) {
      findStatus.textContent = '';
      return;
    }
    eventBus.dispatch('find', {
      source: null,
      type: again ? 'again' : '',
      query,
      caseSensitive: false,
      entireWord: false,
      highlightAll: true,
      findPrevious: previous,
      matchDiacritics: false,
    });
  }

  $('prev').addEventListener('click', () => viewer.previousPage());
  $('next').addEventListener('click', () => viewer.nextPage());
  pageNum.addEventListener('change', () => goPage(Number(pageNum.value)));
  $('zoom-in').addEventListener('click', () => viewer.increaseScale());
  $('zoom-out').addEventListener('click', () => viewer.decreaseScale());
  $('fit').addEventListener('click', () => (viewer.currentScaleValue = 'page-width'));
  let last = '';
  find.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    search(find.value === last, e.shiftKey);
    last = find.value;
  });
  find.addEventListener('input', () => {
    search(false, false);
    last = find.value;
  });
  $('find-next').addEventListener('click', () => search(true, false));
  $('find-prev').addEventListener('click', () => search(true, true));
  window.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      find.focus();
      find.select();
    }
  });

  window.addEventListener('message', (e) => {
    const m = accept<ToViewer>(e, window.parent, serverOrigin);
    if (!m) return;
    if (m.type === 'zam-pdf-open') void open(m.data, m.page);
    else if (m.type === 'zam-pdf-page') goPage(m.page);
  });
  post({ type: 'zam-pdf-ready' });
}

// документ открыт не во фрейме WebUI — показывать нечего
if (window.parent === window) {
  fail('просмотрщик открывается из WebUI');
} else {
  main().catch((e) => fail(e instanceof Error ? e.message : String(e)));
}
