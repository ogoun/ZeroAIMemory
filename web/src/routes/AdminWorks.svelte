<script lang="ts">
  // «Администрирование → Внутренние работы» (#/admin/works; M7.6, спецификация §9.9, решения
  // 171–174; ленивый чанк): «Настройки сервера» — все ключи секции `[works]` формой на странице
  // (правило А §6.14: поля на виду, «Сохранить» — одной правкой, набранное — под проверкой ухода):
  // у поля — подпись, одна строка «что это и чем грозит», действующее значение и его источник
  // («файл конфигурации» / «задано в WebUI»), когда действует новое (без перезапуска) и «Вернуть к
  // файлу» (сразу, правило Д). Диапазоны и допустимое — из ответа сервера (те же, что у
  // конфигурации); 409 (другой администратор успел раньше) — значения перечитаны, тронутые поля
  // остаются набранными. «Пределы токенов пользователей» — расход суток и месяца, личный предел,
  // действующий и «Изменить» (окно «Предел токенов», не больше потолка). Таблица — `.table-wrap`,
  // на телефоне — карточки (§5.1).
  import { onMount } from 'svelte';
  import Notice from '../components/Notice.svelte';
  import WorksLimitDialog from '../components/WorksLimitDialog.svelte';
  import { ApiError, errorText } from '../lib/api';
  import { guardLeave } from '../lib/leaveEdit';
  import { viewport } from '../lib/viewport.svelte';
  import {
    adminWorks,
    patchServerSettings,
    serverSettings,
    type AdminWorksRow,
    type ServerField,
    type ServerSettings,
    type WorksLimits,
  } from '../lib/worksApi';
  import {
    APPLIES_TITLE,
    OPTION_TITLE,
    SERVER_GROUPS,
    SOURCE_TITLE,
    changedValues,
    draftOf,
    errorKey,
    fieldText,
    valueText,
    type ServerDraft,
  } from '../lib/worksServer';
  import { limitsText, num } from '../lib/worksText';

  let { onexpired }: { onexpired: () => void } = $props();

  let settings = $state<ServerSettings | null>(null);
  let rows = $state<AdminWorksRow[] | null>(null);
  let ceiling = $state<{ daily: number; monthly: number } | null>(null);
  let error = $state('');
  let draft = $state<ServerDraft>({});
  /** Значения сервера, с которыми сравнивается набранное. */
  let base = $state.raw<ServerDraft>({});
  let formError = $state('');
  let badKey = $state<string | null>(null);
  let notice = $state('');
  let busy = $state(false);

  let phone = $derived(viewport.current === 'phone');
  const touched = (k: string) => draft[k] !== base[k];
  const dirty = () => !!settings && Object.keys(base).some(touched);

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && e.status === 401) {
      onexpired();
      return true;
    }
    return false;
  }

  /** Новые значения сервера; `keep` — тронутые поля остаются набранными, нетронутые — из новых. */
  function take(s: ServerSettings, keep: boolean) {
    const fresh = draftOf(s.fields);
    const next = { ...fresh };
    if (keep) for (const k of Object.keys(fresh)) if (k in draft && touched(k)) next[k] = draft[k];
    draft = next;
    base = fresh;
    settings = s;
  }

  async function loadRows() {
    const w = await adminWorks();
    rows = w.users;
    ceiling = w.ceiling ?? null;
    return w;
  }

  async function load() {
    error = '';
    try {
      const w = await loadRows();
      if (w.settings) take(w.settings, false);
      else error = 'Сервер не поддерживает настройки внутренних работ в WebUI: они — только в файле конфигурации.';
    } catch (e) {
      if (expired(e)) return;
      error = errorText(e);
    }
  }

  // набранное — в стеке проверок ухода (правило А): согласие уйти — набранное отпущено
  onMount(() => {
    void load();
    return guardLeave(dirty, () => {
      draft = { ...base };
    });
  });

  const titleOf = (k: string) => fieldText(k)?.title ?? k;

  /** 409: значения перечитаны, набранное в тронутых полях осталось. */
  async function conflict() {
    try {
      take(await serverSettings(), true);
    } catch (e) {
      if (expired(e)) return;
    }
    formError =
      'Настройки сервера только что изменил другой администратор: значения обновлены, ваши правки остались в полях — проверьте их и сохраните ещё раз.';
  }

  async function save() {
    if (!settings) return;
    formError = '';
    badKey = null;
    notice = '';
    const c = changedValues(draft, settings.fields);
    if ('error' in c) {
      formError = c.error;
      badKey = c.key;
      return;
    }
    // порядок — как в форме
    const keys = SERVER_GROUPS.flatMap((g) => g.fields.map((f) => f.key)).filter((k) => k in c.values);
    if (!keys.length) {
      draft = { ...base };
      notice = 'Изменений нет.';
      return;
    }
    busy = true;
    try {
      take(await patchServerSettings(settings.rev, c.values), false);
      notice = `Сохранено: ${keys.map(titleOf).join(', ')}. Действует без перезапуска сервера.`;
      await loadRows();
    } catch (e) {
      if (expired(e)) return;
      if (e instanceof ApiError && e.status === 409) await conflict();
      else {
        formError = errorText(e);
        badKey = errorKey(formError);
      }
    } finally {
      busy = false;
    }
  }

  /** «Вернуть к файлу» — сразу (правило Д): прочие набранные поля остаются. */
  async function reset(key: string) {
    if (!settings) return;
    formError = '';
    badKey = null;
    notice = '';
    busy = true;
    try {
      const s = await patchServerSettings(settings.rev, { [key]: null });
      take(s, true);
      draft[key] = draftOf(s.fields)[key];
      notice = `«${titleOf(key)}» — снова из файла конфигурации: ${valueText(key, s.fields[key]?.value)}.`;
      await loadRows();
    } catch (e) {
      if (expired(e)) return;
      if (e instanceof ApiError && e.status === 409) await conflict();
      else formError = errorText(e);
    } finally {
      busy = false;
    }
  }

  function cancel() {
    draft = { ...base };
    formError = '';
    badKey = null;
    notice = '';
  }

  const id = (k: string) => `srv-${k}`;
  const fieldOf = (k: string): ServerField | undefined => settings?.fields[k];

  // ---- пределы пользователей (окно «Предел токенов») ----

  let limitRow = $state<AdminWorksRow | null>(null);
  let limitsNotice = $state('');

  async function limitSaved(l: WorksLimits | null) {
    const who = limitRow?.login ?? '';
    limitRow = null;
    limitsNotice = l ? `Предел пользователя ${who}: ${limitsText(l)}.` : `У пользователя ${who} личного предела нет — действует потолок сервера.`;
    try {
      await loadRows();
    } catch (e) {
      if (expired(e)) return;
    }
  }

  const pair = (a: number | undefined, b: number | undefined) => (a === undefined || b === undefined ? '—' : `${num(a)} / ${num(b)}`);
</script>

<section class="panel" aria-labelledby="srv-h">
  <h2 id="srv-h">Внутренние работы сервера</h2>
  <p class="muted small">
    Настройки исполнителя внутренних работ для всего сервера: значения, заданные здесь, сильнее файла конфигурации и
    действуют без перезапуска. Свои режимы, подключение модели и очередь — на странице
    <a href="#/works">Внутренние работы</a>.
  </p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if !settings && !error}<p class="muted">Загрузка…</p>{/if}
</section>

{#if settings}
  <section class="panel block" aria-labelledby="srv-form-h">
    <h2 id="srv-form-h">Настройки сервера</h2>
    <form
      aria-label="Настройки сервера"
      onsubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      {#each SERVER_GROUPS as g (g.title)}
        <fieldset class="group">
          <legend>{g.title}</legend>
          {#each g.fields as f (f.key)}
            {@const sf = fieldOf(f.key)}
            {#if sf}
              <div class="field" class:changed={touched(f.key)} data-field={f.key}>
                {#if f.kind === 'bool'}
                  <label class="opt">
                    <input
                      type="checkbox"
                      checked={draft[f.key] === true}
                      onchange={(e) => (draft[f.key] = e.currentTarget.checked)}
                      aria-describedby="{id(f.key)}-note {id(f.key)}-meta"
                      disabled={busy}
                    />
                    <span>{f.title}</span>
                  </label>
                {:else}
                  <label for={id(f.key)}>{f.title}</label>
                  {#if f.kind === 'enum'}
                    <select
                      id={id(f.key)}
                      value={draft[f.key]}
                      onchange={(e) => (draft[f.key] = e.currentTarget.value)}
                      aria-describedby="{id(f.key)}-note {id(f.key)}-meta"
                      disabled={busy}
                    >
                      {#each sf.options ?? [] as o (o)}<option value={o}>{OPTION_TITLE[f.key]?.[o] ?? o}</option>{/each}
                    </select>
                  {:else}
                    <input
                      id={id(f.key)}
                      type={f.kind === 'time' ? 'time' : 'text'}
                      inputmode={f.kind === 'number' ? 'numeric' : undefined}
                      autocomplete="off"
                      spellcheck="false"
                      value={draft[f.key]}
                      oninput={(e) => (draft[f.key] = e.currentTarget.value)}
                      aria-describedby="{id(f.key)}-note {id(f.key)}-meta"
                      aria-invalid={badKey === f.key}
                      disabled={busy}
                    />
                  {/if}
                {/if}
                <p class="note muted small" id="{id(f.key)}-note">
                  {f.note}{#if sf.min !== undefined && sf.max !== undefined}{` Допустимо: от ${num(sf.min)} до ${num(sf.max)}.`}{/if}
                </p>
                <p class="meta small" id="{id(f.key)}-meta" data-testid="srv-meta-{f.key}">
                  <span>Действует: <b>{valueText(f.key, sf.value)}</b> · {SOURCE_TITLE[sf.source] ?? sf.source}</span>
                  {#if sf.source === 'webui'}
                    <span class="muted">в файле: {valueText(f.key, sf.file)}</span>
                  {/if}
                  <span class="muted">новое — {APPLIES_TITLE[sf.applies] ?? sf.applies}</span>
                  {#if sf.source === 'webui'}
                    <button type="button" class="small-btn" disabled={busy} onclick={() => reset(f.key)} aria-label="Вернуть к файлу: {f.title}"
                      >Вернуть к файлу</button
                    >
                  {/if}
                </p>
              </div>
            {/if}
          {/each}
        </fieldset>
      {/each}
      {#if formError}<p class="error" role="alert">{formError}</p>{/if}
      <div class="row actions">
        <button type="submit" class="primary" disabled={busy || !dirty()}>Сохранить</button>
        <button type="button" disabled={busy || !dirty()} onclick={cancel}>Отменить правку</button>
        {#if dirty()}<span class="muted small">есть несохранённые изменения</span>{/if}
      </div>
      <Notice text={notice} />
    </form>
  </section>
{/if}

{#if rows}
  <section class="panel block" aria-labelledby="srv-limits-h">
    <h2 id="srv-limits-h">Пределы токенов пользователей</h2>
    <p class="muted small">
      Расход модели за сутки и месяц UTC и личный предел пользователя — не больше потолка сервера{ceiling
        ? ` (${num(ceiling.daily)} в сутки и ${num(ceiling.monthly)} в месяц)`
        : ''}. Действует меньший из потолка и личного предела.
    </p>
    <Notice text={limitsNotice} />
    {#if !rows.length}
      <p class="muted">Пользователей нет.</p>
    {:else if phone}
      <ul class="cards" aria-label="Пределы токенов пользователей">
        {#each rows as r (r.user)}
          <li class="card" data-user={r.user}>
            <div class="card-head">
              <b>{r.login}</b>
              {#if !r.error && ceiling}
                <button type="button" onclick={() => (limitRow = r)} aria-label="Изменить предел: {r.login}">Изменить</button>
              {/if}
            </div>
            {#if r.error}
              <div class="small error">База не прочитана: {r.error}</div>
            {:else}
              <div class="small">Расход, сутки / месяц: {pair(r.tokens?.day, r.tokens?.month)}</div>
              <div class="small">Личный предел: {limitsText(r.limits)}</div>
              <div class="small">Действует, сутки / месяц: {pair(r.limit?.daily, r.limit?.monthly)}</div>
            {/if}
          </li>
        {/each}
      </ul>
    {:else}
      <div class="table-wrap">
        <table aria-label="Пределы токенов пользователей">
          <thead>
            <tr>
              <th>Пользователь</th>
              <th>Расход, сутки / месяц</th>
              <th>Личный предел</th>
              <th>Действует, сутки / месяц</th>
              <th><span class="sr-only">Действия</span></th>
            </tr>
          </thead>
          <tbody>
            {#each rows as r (r.user)}
              <tr data-user={r.user}>
                <td>{r.login}</td>
                {#if r.error}
                  <td colspan="4" class="error">База не прочитана: {r.error}</td>
                {:else}
                  <td class="num">{pair(r.tokens?.day, r.tokens?.month)}</td>
                  <td>{limitsText(r.limits)}</td>
                  <td class="num">{pair(r.limit?.daily, r.limit?.monthly)}</td>
                  <td>
                    {#if ceiling}
                      <button type="button" onclick={() => (limitRow = r)} aria-label="Изменить предел: {r.login}">Изменить</button>
                    {/if}
                  </td>
                {/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
{/if}

{#if ceiling}
  <WorksLimitDialog
    open={!!limitRow}
    user={limitRow?.user ?? null}
    who={limitRow?.login ?? ''}
    limits={limitRow?.limits ?? null}
    {ceiling}
    onsaved={limitSaved}
    onclose={() => (limitRow = null)}
    {onexpired}
  />
{/if}

<style>
  .block {
    margin-top: 16px;
  }
  .small {
    font-size: 13px;
  }
  .group {
    border: 1px solid var(--border);
    border-radius: 8px;
    margin: 0 0 12px;
    padding: 8px 12px 4px;
    min-width: 0;
  }
  .group legend {
    font-weight: 600;
    padding: 0 4px;
  }
  .field {
    display: grid;
    gap: 4px;
    padding: 8px 0;
    border-top: 1px solid var(--border);
    min-width: 0;
  }
  .field:first-of-type {
    border-top: 0;
  }
  .field.changed {
    border-left: 3px solid var(--accent, var(--border));
    padding-left: 8px;
  }
  .field label:not(.opt) {
    font-weight: 600;
  }
  .field input:not([type='checkbox']),
  .field select {
    max-width: 360px;
    width: 100%;
  }
  .opt {
    display: flex;
    gap: 10px;
    align-items: center;
    font-weight: 600;
    min-height: 44px;
  }
  .note,
  .meta {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 12px;
  }
  .actions {
    margin: 8px 0;
  }
  .num {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  td {
    vertical-align: top;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .cards {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  .card {
    display: grid;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow-wrap: anywhere;
  }
  .card-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 4px 8px;
  }
  .card-head button {
    min-height: 44px;
  }
</style>
