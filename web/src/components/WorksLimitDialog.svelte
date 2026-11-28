<script lang="ts">
  // Окно «Предел токенов» (M7.6, спецификация §9.9, решение 172; правило Б §6.14 — форма по
  // кнопке): личный предел токенов модели пользователя на сутки и месяц UTC, не больше потолков
  // сервера. Пустое поле — потолок сервера, оба пустые — личного предела нет. Своя страница
  // «Внутренние работы» — `PATCH /auth/settings`, администратор (страница пользователя и таблица
  // «Администрирование → Внутренние работы») — `PATCH /admin/users/{id}/settings` (аудит — от имени
  // администратора). Ошибка (выше потолка — 422 с потолком) — в окне, поля целы.
  import { untrack } from 'svelte';
  import FormDialog from './FormDialog.svelte';
  import { ApiError, errorText } from '../lib/api';
  import { patchWorks, type WorksLimits } from '../lib/worksApi';
  import { num, parseLimits } from '../lib/worksText';

  let {
    open,
    user = null,
    who = '',
    limits,
    ceiling,
    onsaved,
    onclose,
    onexpired,
  }: {
    open: boolean;
    /** Чужой пользователь (администратор); `null` — свой. */
    user?: string | null;
    /** Чей предел — подзаголовок окна (логин). */
    who?: string;
    limits: WorksLimits | null;
    ceiling: { daily: number; monthly: number };
    onsaved: (limits: WorksLimits | null) => void | Promise<void>;
    onclose: () => void;
    onexpired: () => void;
  } = $props();

  let daily = $state('');
  let monthly = $state('');
  let base = $state({ daily: '', monthly: '' });
  let error = $state('');

  // поля — заново при каждом открытии (набранное в зависимости эффекта не входит)
  $effect(() => {
    if (!open) return;
    const l = limits;
    untrack(() => {
      daily = l ? String(l.daily_tokens) : '';
      monthly = l ? String(l.monthly_tokens) : '';
      base = { daily, monthly };
      error = '';
    });
  });

  const dirty = () => open && (daily !== base.daily || monthly !== base.monthly);

  async function submit() {
    error = '';
    const p = parseLimits(daily, monthly, ceiling);
    if ('error' in p) throw new Error(p.error);
    try {
      await patchWorks({ limits: p.value }, user);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        onexpired();
        return;
      }
      throw new Error(errorText(e));
    }
    base = { daily, monthly };
    await onsaved(p.value);
  }
</script>

<FormDialog
  {open}
  title="Предел токенов"
  subject={who || undefined}
  description={`Сколько токенов модели можно израсходовать — не больше потолка сервера (${num(ceiling.daily)} в сутки и ${num(ceiling.monthly)} в месяц): дошли до предела — обращения ждут следующих суток или месяца. Пустое поле — потолок сервера, оба пустые — личного предела нет.`}
  submit="Сохранить"
  {error}
  {dirty}
  onsubmit={submit}
  {onclose}
>
  <label>
    В сутки (UTC), токенов
    <input
      type="text"
      inputmode="numeric"
      autocomplete="off"
      bind:value={daily}
      placeholder={`потолок: ${num(ceiling.daily)}`}
      data-autofocus
    />
  </label>
  <label>
    В месяц (UTC), токенов
    <input type="text" inputmode="numeric" autocomplete="off" bind:value={monthly} placeholder={`потолок: ${num(ceiling.monthly)}`} />
  </label>
</FormDialog>
