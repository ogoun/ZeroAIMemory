<script lang="ts">
  // Окно записи Vault (M7.6, решение 167): создание и правка — на странице Vault и из окна
  // «Подключение модели» («Новая запись…», «Изменить запись», «Исправить адрес»). Каркас — общий
  // образец форм M7.3 (`FormDialog`: несохранённое спрашивает окно, ошибка сервера — в окне без
  // потери набранного, телефон — во весь экран).
  // - Поля: «Название», тип, «Адрес» (`data.url`), открытые поля по типу (пользователь; у хоста и
  //   окружения — назначение, адрес, ограничения, заметки), описание и «Значение» — только для
  //   записи: поле пароля (у SSH-ключа и заметки — многострочное), при правке пусто = прежнее
  //   значение (сервер `PATCH` без `secret` перешифровывает его под новую ревизию); после
  //   сохранения значение нигде не показывается.
  // - Подключение модели (`model` — протокол подключения): «Название», «Адрес API» с подсказкой
  //   протокола и «Ключ доступа»; тип — `token`, а пустой ключ при создании — запись без значения
  //   (тип `host`, сервер без ключа): владелец окна подключения ставит «без ключа».
  // - 409 правки: запись изменили в другом месте — ревизия обновляется, набранное остаётся,
  //   повторное «Сохранить» записывает поверх; 409 создания — имя занято (текст сервера).
  // Форма создаётся заново на каждое открытие: начальные значения — из свойств один раз.
  import { untrack } from 'svelte';
  import FormDialog from './FormDialog.svelte';
  import { ApiError, createVault, errorText, getVault, patchVault, type VaultData, type VaultItem } from '../lib/api';
  import { MODEL_URL_HINT, VAULT_TYPE_TEXT, VAULT_TYPES, vaultFieldsOf, type ModelProtocol, type VaultFields } from '../lib/vaultForm';

  interface Props {
    open: boolean;
    /** Чья запись (администратор — пользователь страницы). */
    user: string;
    /** `knowledge` или GUID решения (создание). */
    space: string;
    /** Правимая запись; создание — `null`. */
    item: VaultItem | null;
    /** Окно подключения модели: протокол — подсказка адреса; поля — только адрес и ключ. */
    model?: ModelProtocol | null;
    /** Адрес при открытии («Исправить адрес»: адрес записи с /v1). */
    url?: string;
    onsaved: (it: VaultItem, keyless: boolean) => void;
    onclose: () => void;
    onexpired?: () => void;
  }

  let { open, user, space, item, model = null, url, onsaved, onclose, onexpired }: Props = $props();

  const editing = untrack(() => item !== null);
  let rev = untrack(() => item?.rev ?? 0);
  const src: VaultData = untrack(() => item?.data ?? { type: 'token' });

  let title = $state(untrack(() => item?.title ?? ''));
  let type = $state(untrack(() => src.type));
  let f = $state<VaultFields>(untrack(() => ({ ...vaultFieldsOf(src), ...(url !== undefined ? { url } : {}) })));
  let secret = $state('');
  let error = $state('');

  const now = () => ({ title, type, ...f, secret });
  const base = now();
  // «Исправить адрес» открывает окно уже с исправленным адресом — это несохранённая правка
  const baseUrl = untrack(() => vaultFieldsOf(src).url);
  const dirty = () => {
    const n = now();
    return (Object.keys(n) as (keyof typeof n)[]).some((k) => (k === 'url' ? n.url !== baseUrl : n[k] !== base[k]));
  };

  /** Тип записи окружения: значение необязательно. */
  const envType = (t: string) => t === 'host' || t === 'environment';
  /** Многострочное значение (SSH-ключ, заметка). */
  let multiline = $derived(type === 'ssh_key' || type === 'note');
  let showUser = $derived(['password', 'ssh_key', 'connection_string', 'repo'].includes(type));
  let showEnv = $derived(envType(type));
  /** Пустой ключ при создании записи модели — сервер без ключа. */
  let keyless = $derived(!!model && !editing && !secret.trim());

  /** Открытые поля для сервера: пустое — без поля; при правке — поверх прежних полей записи. */
  function dataOut(t: string): VaultData {
    const keep: Record<string, unknown> = editing && model ? { ...src } : {};
    const out: Record<string, unknown> = { ...keep, type: t };
    for (const [k, v] of Object.entries(f)) {
      const s = (v ?? '').trim();
      if (s) out[k] = s;
      else delete out[k];
    }
    return out as VaultData;
  }

  function check(): string {
    if (!title.trim()) return model ? 'Укажите название — так запись будет видна в списке.' : 'Укажите название записи.';
    if (model && !f.url.trim()) return 'Укажите адрес API модели.';
    if (!editing && !model && !envType(type) && !secret.trim()) return 'Укажите значение: без него сохраняются только записи хоста и окружения.';
    return '';
  }

  async function send() {
    error = '';
    const bad = check();
    if (bad) throw new Error(bad);
    // запись модели: с ключом — токен; без ключа при создании — запись хоста без значения
    // (`keyless` — до очистки поля значения: после неё оно пустое)
    const noKey = keyless;
    const t = model ? (noKey ? 'host' : type === 'host' && secret.trim() ? 'token' : editing ? type : 'token') : type;
    const s = secret.trim() ? secret : undefined;
    let saved: VaultItem;
    try {
      saved = editing && item
        ? await patchVault(user, item.id, rev, { title: title.trim(), data: dataOut(t), ...(s ? { secret: s } : {}) })
        : await createVault(user, space, { title: title.trim(), data: dataOut(t), ...(s ? { secret: s } : {}) });
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        onexpired?.();
        throw new Error('Сессия истекла — войдите снова.');
      }
      if (editing && item && e instanceof ApiError && e.status === 409) {
        // запись изменили в другом месте: набранное остаётся, ревизия — свежая
        const fresh = await getVault(user, item.id).catch(() => null);
        if (fresh && fresh.rev !== rev) {
          rev = fresh.rev;
          throw new Error(`Запись изменили в другом месте (теперь ревизия ${fresh.rev}). Набранное сохранено: нажмите «Сохранить» ещё раз, чтобы записать поверх.`);
        }
      }
      throw new Error(errorText(e));
    }
    secret = '';
    onsaved(saved, noKey);
  }
</script>

<FormDialog
  {open}
  title={editing ? 'Изменить запись Vault' : 'Новая запись Vault'}
  subject={editing ? item?.title : undefined}
  description={model
    ? 'Адрес API модели и ключ доступа хранятся в вашем Vault; ключ после сохранения не показывается.'
    : 'Значение хранится зашифрованным и после сохранения не показывается.'}
  submit={editing ? 'Сохранить' : 'Создать'}
  {error}
  {dirty}
  onsubmit={send}
  {onclose}
>
  <label>
    Название
    <input bind:value={title} required maxlength="200" data-autofocus={url === undefined ? '' : undefined}
      placeholder={model ? 'например, Qwen на рабочей станции' : ''} />
  </label>
  {#if !model}
    <label>
      Тип
      <select bind:value={type}>
        {#each VAULT_TYPES as t (t)}<option value={t}>{VAULT_TYPE_TEXT[t]}</option>{/each}
      </select>
    </label>
  {/if}
  <label>
    {model ? 'Адрес API' : 'Адрес (ссылка)'}
    {#if model}<span class="field-note muted" id="vf-url-note" aria-hidden="true">{MODEL_URL_HINT[model]}</span>{/if}
    <input bind:value={f.url} type="url" inputmode="url" maxlength="2000" spellcheck="false" autocomplete="off"
      required={!!model} data-autofocus={url !== undefined ? '' : undefined}
      aria-describedby={model ? 'vf-url-note' : undefined}
      placeholder={model === 'anthropic' ? 'https://api.anthropic.com' : model ? 'https://адрес/v1' : 'https://…'} />
  </label>
  {#if !model && showUser}
    <label>Пользователь <input bind:value={f.username} maxlength="200" autocomplete="off" /></label>
  {/if}
  {#if !model && showEnv}
    <label>Назначение <input bind:value={f.purpose} maxlength="500" /></label>
    <label>Адрес хоста (как подключаться) <input bind:value={f.address} maxlength="500" placeholder="например, 192.0.2.10, вход по SSH" /></label>
    <label>Ограничения <textarea bind:value={f.limits} rows="2" maxlength="1000"></textarea></label>
    <label>Заметки <textarea bind:value={f.notes} rows="2" maxlength="2000"></textarea></label>
  {/if}
  {#if !model}
    <label>Описание <input bind:value={f.description} maxlength="2000" /></label>
  {/if}
  <label>
    {model ? 'Ключ доступа' : 'Значение'}
    <span class="field-note muted" id="vf-secret-note" aria-hidden="true">
      {#if editing}пусто — прежнее значение сохранится{:else if model}пусто — сервер модели без ключа (локальный){:else if envType(type)}необязательно у хоста и окружения{:else}хранится зашифрованным{/if}
    </span>
    {#if multiline && !model}
      <textarea class="secret-area" bind:value={secret} rows="4" spellcheck="false" autocomplete="off"
        aria-describedby="vf-secret-note"></textarea>
    {:else}
      <input type="password" bind:value={secret} autocomplete="new-password" spellcheck="false"
        aria-describedby="vf-secret-note" />
    {/if}
  </label>
  {#if keyless}
    <p class="muted small" data-testid="vault-keyless">
      Без ключа запись сохранится без значения, а в подключении будет выбрано «без ключа (локальный сервер)».
    </p>
  {/if}
</FormDialog>

<style>
  .secret-area {
    font-family: ui-monospace, Consolas, monospace;
    -webkit-text-security: disc;
  }
  .small {
    font-size: 12px;
  }
</style>
