// Размеры окна и помощники e2e узких экранов (M7.2, спецификация §5.1 «Проверка»).
//
// Сенсорный ввод — `touch`: в Edge 158 (Chromium) `hasTouch: true` сам включает медиазапросы
// `(pointer: coarse)` и `(hover: none)` (шаг 1 задачи 1 M7.2, утверждающий тест
// `touch_emulation_pointer_coarse` в mobile.spec.ts); включение через CDP
// `Emulation.setEmulatedMedia` не нужно. `isMobile` — мобильная эмуляция Chromium (meta viewport,
// полосы прокрутки поверх содержимого).
import { expect, type Locator, type Page } from '@playwright/test';

/** Размеры окна (CSS px): телефоны, планшет в двух ориентациях, ноутбук, настольный. */
export const VIEWPORTS = {
  phoneS: { width: 360, height: 740 },
  phone: { width: 390, height: 844 },
  tablet: { width: 820, height: 1180 },
  tabletLand: { width: 1180, height: 820 },
  laptop: { width: 1024, height: 768 },
  desktop: { width: 1440, height: 900 },
} as const;

export type ViewportName = keyof typeof VIEWPORTS;

/** Параметры контекста Playwright для сенсорного ввода (`test.use({...touch})`). */
export const touch = { hasTouch: true, isMobile: true } as const;

/**
 * Страница не прокручивается вбок: ширина документа не больше окна (`scrollWidth ≤ innerWidth`) и
 * его видимой ширины без полосы прокрутки (`clientWidth`).
 */
export async function noHorizontalScroll(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const de = document.documentElement;
    return de.scrollWidth <= Math.min(window.innerWidth, de.clientWidth);
  });
}

/**
 * Соседние цели одного ряда (общий родитель, пересечение по высоте) — не ближе `px` друг к другу
 * по горизонтали (по `getBoundingClientRect`). Возвращает число проверенных пар.
 */
export async function minGap(locator: Locator, px = 8): Promise<number> {
  const found = await locator.evaluateAll((els) => {
    const rows = new Map<Element | null, DOMRect[]>();
    for (const e of els) {
      const r = e.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      rows.set(e.parentElement, [...(rows.get(e.parentElement) ?? []), r]);
    }
    const gaps: { gap: number; at: string }[] = [];
    for (const rs of rows.values()) {
      rs.sort((a, b) => a.left - b.left);
      for (let i = 1; i < rs.length; i++) {
        const [a, b] = [rs[i - 1], rs[i]];
        if (b.top >= a.bottom || a.top >= b.bottom) continue;
        gaps.push({ gap: b.left - a.right, at: `${Math.round(a.right)}…${Math.round(b.left)} y=${Math.round(a.top)}` });
      }
    }
    return gaps;
  });
  for (const g of found) expect(g.gap, `${locator}: зазор ${g.at}`).toBeGreaterThanOrEqual(px - 0.5);
  return found.length;
}

/** Каждый элемент `locator` отрисован и не меньше `px` × `px` (по `boundingBox`). */
export async function minTarget(locator: Locator, px = 44): Promise<void> {
  const n = await locator.count();
  expect(n, `нет целей: ${locator}`).toBeGreaterThan(0);
  for (let i = 0; i < n; i++) {
    const el = locator.nth(i);
    const box = await el.boundingBox();
    expect(box, `${locator} #${i} не отрисован`).not.toBeNull();
    const what = `${locator} #${i} «${((await el.textContent()) ?? '').trim()}» ${box!.width}×${box!.height}`;
    // доли пикселя — округление вёрстки
    expect(box!.width, what).toBeGreaterThanOrEqual(px - 0.5);
    expect(box!.height, what).toBeGreaterThanOrEqual(px - 0.5);
  }
}
