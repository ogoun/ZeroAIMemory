// Кэш чисел разделов на странице (M7.2, спецификация §5.3 п. 3): `CountsCache` (`counts.ts`) на
// реактивной `SvelteMap` — дерево и полоса журналов, читая `counts.get` в разметке, подписаны на
// свой ключ. Загрузка — при первом чтении (раскрытие пространства, открытие журнала); устаревание:
// после изменяющих запросов записей и заметок (подписка `onEntriesChanged`, её зовёт `api.ts` —
// одна точка), при раскрытии пространства и открытии журнала (`invalidate` страниц), при возврате на
// вкладку (`visibilitychange`) — у чисел старше 30 с. Опроса по таймеру нет.
import { SvelteMap } from 'svelte/reactivity';
import { onEntriesChanged, spaceCounts, type SpaceCounts } from './api';
import { CountsCache, type CountsSlot } from './counts';

/** Кэш чисел разделов — один на страницу. */
export const counts = new CountsCache<SpaceCounts>({
  store: new SvelteMap<string, CountsSlot<SpaceCounts>>(),
  fetch: spaceCounts,
  now: () => Date.now(),
  defer: (f) => queueMicrotask(f),
});

onEntriesChanged((user) => counts.invalidate(user));
if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') counts.visible();
  });
}
