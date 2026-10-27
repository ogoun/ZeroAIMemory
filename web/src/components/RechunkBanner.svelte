<script lang="ts">
  // Баннер перенарезки своей базы для пользователя (не администратора, R9): полной страницы
  // «Индекс» у него нет, а `GET /index/status` по его сессии отдаёт в `dbs` только его базу.
  // Пока перенарезка идёт — опрос раз в 10 с, иначе раз в минуту; скрытая вкладка не
  // опрашивает сервер. Ошибки опроса не показываются (баннер — подсказка, не раздел).
  import { onMount } from 'svelte';
  import { ApiError, indexStatus, type Rechunk } from '../lib/api';

  let { userId, onexpired }: { userId: string; onexpired: () => void } = $props();

  const ACTIVE_MS = 10_000;
  const IDLE_MS = 60_000;

  let rechunk = $state<Rechunk | null>(null);

  async function load() {
    try {
      const st = await indexStatus();
      rechunk = (st.dbs ?? []).find((d) => d.user_id === userId)?.rechunk ?? null;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onexpired();
    }
  }

  onMount(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      clearTimeout(timer);
      timer = undefined;
      await load();
      if (alive && !document.hidden && timer === undefined) {
        timer = setTimeout(tick, rechunk ? ACTIVE_MS : IDLE_MS);
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
</script>

{#if rechunk}
  <div class="rechunk" role="status">
    Идёт перенарезка: {num(rechunk.done)}/{num(rechunk.total)}. Поиск и запись работают, векторы
    пересчитываются в фоне.
  </div>
{/if}

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
</style>
