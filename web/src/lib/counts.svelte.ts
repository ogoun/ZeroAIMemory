// Кэш чисел разделов на странице (M7.2, спецификация §5.3 п. 3): `CountsCache` (`counts.ts`) на
// реактивной `SvelteMap` — дерево и полоса журналов, читая `counts.get` в разметке, подписаны на
// свой ключ. Загрузка — при первом чтении (раскрытие пространства, открытие журнала); устаревание:
// после изменяющих запросов записей и заметок (подписка `onEntriesChanged`, её зовёт `api.ts` —
// одна точка), при раскрытии пространства и открытии журнала (`invalidate` страниц), при возврате на
// вкладку (`visibilitychange`) — у чисел старше 30 с. Опроса по таймеру нет.
// С M7.3 (§6.7 п. 5) — второй экземпляр той же логики: открытые кандидаты в Rules
// (`GET /proposals/counts`) для метки дерева «кандидаты: N» и «Кандидаты N» шапки; ключ —
// «свой пользователь | rules» (страница кандидатов — своего пользователя), сброс — те же события.
import { SvelteMap } from 'svelte/reactivity';
import { onEntriesChanged, proposalCounts as fetchProposalCounts, spaceCounts, type ProposalCounts, type SpaceCounts } from './api';
import { CountsCache, type CountsSlot } from './counts';

/** Кэш чисел разделов — один на страницу. */
export const counts = new CountsCache<SpaceCounts>({
  store: new SvelteMap<string, CountsSlot<SpaceCounts>>(),
  fetch: spaceCounts,
  now: () => Date.now(),
  defer: (f) => queueMicrotask(f),
});

/** Кэш числа открытых кандидатов в Rules своего пользователя (ключ — `<свой id>|rules`). */
export const proposalCounts = new CountsCache<ProposalCounts>({
  store: new SvelteMap<string, CountsSlot<ProposalCounts>>(),
  fetch: () => fetchProposalCounts(),
  now: () => Date.now(),
  defer: (f) => queueMicrotask(f),
});

onEntriesChanged((user) => {
  counts.invalidate(user);
  proposalCounts.invalidate(user);
});
if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') {
      counts.visible();
      proposalCounts.visible();
    }
  });
}
