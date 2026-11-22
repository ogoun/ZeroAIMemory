// Класс ширины окна и сенсорный ввод на рунах (M7.2, спецификация §5.1): подписка на
// `matchMedia` с медиазапросами `MEDIA` из `layout.ts` — те же границы, что в CSS. Плюс действия
// Svelte раскладки: ширина области (`ResizeObserver`) и выбранный пункт полосы в видимой части.
import type { Action } from 'svelte/action';
import { MediaQuery } from 'svelte/reactivity';
import { MEDIA, type WidthClass } from './layout';

const phone = new MediaQuery(MEDIA.phone);
const drawer = new MediaQuery(MEDIA.drawer);
const coarse = new MediaQuery(MEDIA.coarse);

/**
 * Окно: `current` — класс ширины (телефон < 720, планшет < 1024, иначе настольный), `coarse` —
 * основной указатель сенсорный. Чтение в эффекте или шаблоне подписывает на смену.
 */
export const viewport: { readonly current: WidthClass; readonly coarse: boolean } = {
  get current(): WidthClass {
    if (phone.current) return 'phone';
    return drawer.current ? 'tablet' : 'desktop';
  },
  get coarse(): boolean {
    return coarse.current;
  },
};

/**
 * Ширина содержимого элемента (CSS px) — в `cb` при каждом изменении: раскладка по ширине области,
 * а не окна (`JournalSplit`, доска задач). Первое значение приходит до первой отрисовки.
 */
export const observeWidth: Action<HTMLElement, (w: number) => void> = (node, cb) => {
  let f = cb;
  const ro = new ResizeObserver((es) => {
    for (const e of es) f(e.contentRect.width);
  });
  ro.observe(node);
  return {
    update(next: (w: number) => void) {
      f = next;
    },
    destroy() {
      ro.disconnect();
    },
  };
};

/**
 * Полоса (`.strip`, прокрутка вбок): выбранный пункт (`aria-current="page"` или
 * `aria-selected="true"`) — в видимой части полосы. `key` — что выбрано: смена прокручивает
 * полосу снова; изменение ширины полосы (поворот, окно) — тоже. Страница при этом не
 * прокручивается.
 */
export const revealSelected: Action<HTMLElement, unknown> = (node) => {
  let frame = 0;
  const reveal = () => {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      const el = node.querySelector<HTMLElement>('[aria-current="page"], [aria-selected="true"]');
      if (!el || node.scrollWidth <= node.clientWidth) return;
      // координаты пункта внутри прокручиваемого содержимого полосы
      const box = node.getBoundingClientRect();
      const r = el.getBoundingClientRect();
      const left = r.left - box.left - node.clientLeft + node.scrollLeft;
      const right = left + r.width;
      // не целиком видим — к началу пункта: там же точка привязки прокрутки (`scroll-snap-align`)
      if (left < node.scrollLeft || right > node.scrollLeft + node.clientWidth) node.scrollLeft = left;
    });
  };
  // первое наблюдение приходит сразу — оно же и первый показ
  const ro = new ResizeObserver(reveal);
  ro.observe(node);
  return {
    update: reveal,
    destroy() {
      ro.disconnect();
      cancelAnimationFrame(frame);
    },
  };
};
