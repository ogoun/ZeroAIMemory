<script lang="ts">
  // Администрирование → «Решения» (§11.6): реестр решений с владельцами, участники и запросы
  // доступа — одобрить (rw или ro), отклонить с причиной, отозвать. Решения о доступе
  // действуют на следующем же запросе участника (кэша нет).
  import Prompt, { type Ask } from '../components/Prompt.svelte';
  import {
    ApiError,
    approveAccess,
    errorText,
    listSolutions,
    rejectAccess,
    revokeAccess,
    solutionAccess,
    type Access,
    type AccessEntry,
    type SolutionItem,
  } from '../lib/api';
  import { fmtTime } from '../lib/journals';

  let { onexpired }: { onexpired: () => void } = $props();

  const STATE_TEXT: Record<string, string> = {
    pending: 'ждёт решения',
    approved: 'одобрен',
    rejected: 'отклонён',
    revoked: 'отозван',
  };

  let solutions = $state<SolutionItem[]>([]);
  let members = $state<Record<string, AccessEntry[]>>({});
  let open = $state<string | null>(null);
  let error = $state('');
  let loading = $state(false);
  let ask = $state<Ask | null>(null);

  function fail(e: unknown) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    error = errorText(e);
  }

  async function load() {
    loading = true;
    error = '';
    try {
      solutions = (await listSolutions()).sort((a, b) => a.name.localeCompare(b.name));
      // запросы всех решений сразу: число ожидающих — в строке реестра
      const lists = await Promise.all(
        solutions.map((s) => solutionAccess(s.agent_solution_id).then((r) => r.members)),
      );
      members = Object.fromEntries(solutions.map((s, i) => [s.agent_solution_id, lists[i]]));
    } catch (e) {
      fail(e);
    } finally {
      loading = false;
    }
  }

  async function refresh(guid: string) {
    members[guid] = (await solutionAccess(guid)).members;
  }

  async function run(guid: string, fn: () => Promise<unknown>) {
    error = '';
    try {
      await fn();
      await refresh(guid);
    } catch (e) {
      fail(e);
    }
  }

  const approve = (guid: string, m: AccessEntry, access: Access) =>
    run(guid, () => approveAccess(guid, m.user_id, access));
  const revoke = (guid: string, m: AccessEntry) => run(guid, () => revokeAccess(guid, m.user_id));

  function reject(guid: string, m: AccessEntry) {
    ask = {
      title: 'Отклонить запрос',
      message: `Запрос пользователя «${m.user_name}». Причина сохраняется в запросе и в журнале аудита.`,
      label: 'Причина',
      placeholder: 'Например, решение закрытое',
      confirm: 'Отклонить',
      danger: true,
      run: async (reason) => {
        try {
          await rejectAccess(guid, m.user_id, reason);
          await refresh(guid);
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) onexpired();
          throw new Error(errorText(e));
        }
      },
    };
  }

  const pendingOf = (guid: string) => (members[guid] ?? []).filter((m) => m.state === 'pending').length;
  const approvedOf = (guid: string) => (members[guid] ?? []).filter((m) => m.state === 'approved').length;

  load();
</script>

<section class="panel">
  <h2>Решения</h2>
  <p class="muted small">
    Реестр решений (agent_solution_id), владельцы и участники. Участник работает в базе владельца
    только с пространством решения; одобрение, отклонение и отзыв действуют сразу.
  </p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if !loading && solutions.length === 0 && !error}<p class="muted">Решений пока нет.</p>{/if}

  <table aria-label="Реестр решений">
    <thead><tr><th>Решение</th><th>Владелец</th><th>Участники</th><th>Запросы</th><th></th></tr></thead>
    <tbody>
      {#each solutions as s (s.agent_solution_id)}
        {@const guid = s.agent_solution_id}
        <tr data-solution={guid}>
          <td>
            <strong>{s.name}</strong>
            <div class="muted small"><code>{guid}</code> · {fmtTime(s.created_at)}</div>
          </td>
          <td>{s.owner.name ?? s.owner.id}</td>
          <td>{approvedOf(guid)}</td>
          <td>
            {#if pendingOf(guid)}<span class="badge">{pendingOf(guid)}</span>{:else}<span class="muted">—</span>{/if}
          </td>
          <td>
            <button type="button" onclick={() => (open = open === guid ? null : guid)} aria-expanded={open === guid}>
              {open === guid ? 'Скрыть' : 'Доступ'}
            </button>
          </td>
        </tr>
        {#if open === guid}
          <tr>
            <td colspan="5">
              {#if (members[guid] ?? []).length === 0}
                <p class="muted">Запросов доступа нет.</p>
              {:else}
                <table aria-label="Участники {s.name}">
                  <thead><tr><th>Пользователь</th><th>Состояние</th><th>Режим</th><th>Запрошен</th><th>Решение</th><th></th></tr></thead>
                  <tbody>
                    {#each members[guid] as m (m.user_id)}
                      <tr data-member={m.user_name}>
                        <td>{m.user_name}</td>
                        <td class="state-{m.state}">{STATE_TEXT[m.state] ?? m.state}</td>
                        <td>{m.state === 'approved' ? m.access : '—'}</td>
                        <td class="muted">{fmtTime(m.requested_at)}</td>
                        <td class="muted">{fmtTime(m.decided_at)}{#if m.decided_by} · {m.decided_by === 'admin' ? 'администратор' : 'владелец'}{/if}</td>
                        <td class="row acts">
                          {#if m.state === 'pending'}
                            <button type="button" class="primary" onclick={() => approve(guid, m, 'rw')}>Одобрить rw</button>
                            <button type="button" onclick={() => approve(guid, m, 'ro')}>Одобрить ro</button>
                            <button type="button" class="danger" onclick={() => reject(guid, m)}>Отклонить</button>
                          {:else if m.state === 'approved'}
                            <button type="button" class="danger" onclick={() => revoke(guid, m)}>Отозвать</button>
                          {/if}
                        </td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              {/if}
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
</section>

<Prompt bind:ask />

<style>
  .small {
    font-size: 12px;
  }
  .badge {
    display: inline-block;
    min-width: 20px;
    padding: 0 6px;
    border-radius: 10px;
    background: var(--accent);
    color: var(--accent-text);
    text-align: center;
    font-size: 12px;
  }
  .acts {
    gap: 4px;
  }
  .acts button {
    padding: 2px 10px;
  }
  .state-approved {
    color: var(--callout-tip);
  }
  .state-rejected,
  .state-revoked {
    color: var(--muted);
  }
</style>
