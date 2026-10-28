<script lang="ts">
  // Баннер перенарезки баз (§8.2, §15.4): «Идёт перенарезка: изменились <ключи>, N из M».
  // Единственная точка показа: оболочка (у пользователя — его база, R9; у администратора — все
  // базы) и страница «Индекс», которая передаёт уже полученный статус (`dbs`) и сама не
  // опрашивает сервер дважды. Без `dbs` компонент опрашивает лёгкий
  // `GET /index/status?only=rechunk` (без Qdrant и очередей): пока
  // перенарезка идёт — раз в 10 с, иначе раз в минуту; скрытая вкладка не опрашивает сервер.
  // Ошибки опроса не показываются (баннер — подсказка, не раздел).
  import { onMount } from 'svelte';
  import { ApiError, rechunkStatus, type IndexStatus } from '../lib/api';

  let {
    userId = null,
    dbs = null,
    names = {},
    onexpired,
  }: {
    /** Только эта база (у пользователя); `null` — все видимые. */
    userId?: string | null;
    /** Статус баз, полученный страницей; `null` — опрашивать самому. */
    dbs?: IndexStatus['dbs'] | null;
    /** Имена пользователей баз (у администратора). */
    names?: Record<string, string>;
    onexpired?: () => void;
  } = $props();

  const ACTIVE_MS = 10_000;
  const IDLE_MS = 60_000;

  let polled = $state<IndexStatus['dbs']>([]);

  let rechunks = $derived(
    (dbs ?? polled)
      .filter((d) => !userId || d.user_id === userId)
      .flatMap((d) => (d.rechunk ? [{ user_id: d.user_id, ...d.rechunk }] : [])),
  );

  async function load() {
    try {
      polled = (await rechunkStatus()).dbs ?? [];
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onexpired?.();
    }
  }

  onMount(() => {
    if (dbs) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      clearTimeout(timer);
      timer = undefined;
      await load();
      if (alive && !document.hidden && timer === undefined) {
        timer = setTimeout(tick, rechunks.length ? ACTIVE_MS : IDLE_MS);
      }
    };
    const onvis = () => {
      if (document.hidden) {
        clearTimeout(timer);
        timer = undefined;
      } else {
        void tick();
      }
    };
    document.addEventListener('visibilitychange', onvis);
    void tick();
    return () => {
      alive = false;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onvis);
    };
  });

  const num = (n: number) => n.toLocaleString('ru-RU');
  const changedText = (keys: string[]) => (keys.length ? keys.join(', ') : 'параметры нарезки');
</script>

{#each rechunks as r (r.user_id)}
  <div class="rechunk" role="status">
    <p class="title">Идёт перенарезка: изменились {changedText(r.changed)}, {num(r.done)} из {num(r.total)}</p>
    <p>
      {#if !userId}База <b>{names[r.user_id] ?? r.user_id}</b> перенарезается в фоне после смены настроек нарезки:
      {:else}Ваша база перенарезается в фоне после смены настроек нарезки:{/if}
      поиск и запись работают, векторы пересчитываются только у изменившихся фрагментов.
    </p>
  </div>
{/each}

<style>
  .rechunk {
    margin: 0 0 16px;
    padding: 10px 12px;
    border: 1px solid var(--callout-warning);
    border-left-width: 4px;
    border-radius: 6px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .rechunk p {
    margin: 0;
  }
  .title {
    font-weight: 600;
    margin-bottom: 2px !important;
  }
</style>
