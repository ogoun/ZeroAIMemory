<script lang="ts">
  // Плашка `no_recent_backup` над страницами администратора (M7.2, спецификация M7 §5.6 п. 3, M6
  // §12б.4): «Копий нет» или «Последняя копия N дн. назад» со ссылкой «Копии». Статус — лёгкий
  // `GET /index/status?only=backup` (без Qdrant, очередей и баз пользователей); перечитывается при
  // каждом переходе между страницами администратора (`key`) — копия, снятая на «Копиях», снимает
  // плашку на следующей странице. Ошибки опроса не показываются (плашка — подсказка, не раздел).
  // Пользователю (не администратору) её нет: оболочка показывает её только в разделах
  // администрирования.
  import { ApiError, backupStatus, type BackupStatus } from '../lib/api';
  import { bannerText } from '../lib/backups';
  import { href } from '../lib/router';

  let { key, onexpired }: { key: unknown; onexpired?: () => void } = $props();

  let st = $state<BackupStatus | null>(null);
  /** Поколение запроса: ответ прежнего перехода не затирает свежий. */
  let seq = 0;

  $effect(() => {
    void key;
    const my = ++seq;
    backupStatus()
      .then((r) => {
        if (my === seq) st = r.backup;
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) onexpired?.();
      });
  });

  let text = $derived(bannerText(st, Date.now()));
</script>

{#if text}
  <div class="backup-banner" role="note" aria-label="Копии сервера" data-testid="backup-banner">
    <b>{text}.</b>
    <span>Копия нужна, чтобы восстановить память после сбоя диска или переехать на другую машину.</span>
    <a href={href({ name: 'backups' })}>Копии</a>
  </div>
{/if}

<style>
  .backup-banner {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
    margin: 0 0 16px;
    padding: 10px 12px;
    border: 1px solid var(--callout-warning);
    border-left-width: 4px;
    border-radius: 6px;
    background: var(--warn-bg);
    color: var(--warn-text);
    overflow-wrap: anywhere;
  }
  /* ссылка — цветом текста плашки с подчёркиванием: акцентный синий на `--warn-bg` светлой темы
     ниже 4,5 : 1 */
  .backup-banner a {
    margin-left: auto;
    font-weight: 600;
    color: inherit;
    text-decoration: underline;
  }
  @media (pointer: coarse) {
    .backup-banner a {
      display: inline-flex;
      align-items: center;
      min-height: 44px;
    }
  }
</style>
