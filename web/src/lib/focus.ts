// Фокус после перерисовки (M7.2, спецификация §5.9): закрытая форма, удалённая запись — элемент,
// где был фокус, ушёл, и браузер оставляет его на `body`; клавиатура и программа чтения с экрана
// теряют место. Журнал переводит фокус на осмысленный элемент: «Изменить» записи, заголовок
// карточки, итог действия (`Notice.svelte`).
import { tick } from 'svelte';

/** Фокус на `get()` после ближайшей перерисовки; нечего фокусировать — ничего. Не фокусируемому
 * элементу (заголовок) — `tabindex="-1"`. Прокрутка — только если элемент вне окна. */
export async function focusSoon(get: () => Element | null | undefined): Promise<void> {
  await tick();
  focusNow(get());
}

/** Фокус на `el` сразу (для `Prompt` — при закрытии окна, когда журнал уже перерисован). */
export function focusNow(el: Element | null | undefined): boolean {
  if (!(el instanceof HTMLElement)) return false;
  if (!el.matches('a[href], button, input, select, textarea, [tabindex]')) el.tabIndex = -1;
  el.focus({ preventScroll: true });
  const r = el.getBoundingClientRect();
  if (r.bottom <= 0 || r.top >= window.innerHeight) el.scrollIntoView({ block: 'nearest' });
  return true;
}

/** Начало элемента — на виду (правка карточки на месте, §6.14 п. 3): у прокручиваемого предка
 * (панель доски, карточка рядом со списком) начало элемента, ушедшее выше его края, возвращается;
 * в документе — если начало под шапкой или у нижнего края окна (поправка — `scroll-margin-top`). */
export function revealTop(el: Element | null | undefined): void {
  if (!(el instanceof HTMLElement)) return;
  for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if ((o === 'auto' || o === 'scroll') && p.scrollHeight > p.clientHeight) {
      const above = p.getBoundingClientRect().top - el.getBoundingClientRect().top;
      if (above > 0) p.scrollTop -= above;
      break;
    }
  }
  const r = el.getBoundingClientRect();
  const margin = parseFloat(getComputedStyle(el).scrollMarginTop) || 0;
  if (r.top < margin || r.top > window.innerHeight - 80) el.scrollIntoView({ block: 'start' });
}
