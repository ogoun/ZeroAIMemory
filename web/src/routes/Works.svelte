<script lang="ts">
  // «Внутренние работы» (#/works; M7.3, спецификация §6.9 п. 1, §6.1–§6.3; ленивый чанк): режим
  // исполнителя — общий и по видам (выкл / агент / модель, с пояснением; меняется сразу, правило
  // Д §6.14); подключённая модель — окно формы `FormDialog` (правило Б): запись личного Vault по
  // имени (уходит как `v:<id>`, её адрес и класс — LAN, вне LAN, петля — видны до сохранения),
  // модель, ключ, предел ответа, `body_extra` с рецептами и «Проверить подключение» в окне; плашка
  // «данные памяти уходят за пределы локальной сети» при подключении вне LAN (поле сервера
  // `works.model.external`: по закреплённому адресу, без DNS; ревью r1, I1) и «Отключить модель»;
  // пауза — причина,
  // подсказка и у `vault_changed` «был → стал» с «Подтвердить адрес» (окно-подтверждение, шлёт
  // `confirm_url` = «стал»; 409 — новые «был → стал» в том же окне); ночь — время в поясе сервера
  // и местное, окно, итог последней ночи по видам и выключатель ночной проверки; очередь работ и
  // «Запустить сейчас» (окно-подтверждение; у модели — опрос очереди, пока работа идёт); журнал
  // отправок (метаданные, без текста); токены суток и месяца к пределам. Таблицы — `.table-wrap`,
  // на телефоне — карточки (§5.1). Свой пользователь (блок администратора — M7.4, §6.13).
  import { onMount } from 'svelte';
  import FormDialog from '../components/FormDialog.svelte';
  import Notice from '../components/Notice.svelte';
  import { ApiError, errorText, listVault, type Me, type VaultItem } from '../lib/api';
  import { attention } from '../lib/attention.svelte';
  import { isDirty } from '../lib/formDraft';
  import { localTime } from '../lib/time';
  import { viewport } from '../lib/viewport.svelte';
  import {
    connInput,
    listWorks,
    patchWorks,
    runWorks,
    testModel,
    workCalls,
    worksSettings,
    worksUsage,
    type ModelConn,
    type WorkCall,
    type WorksList,
    type WorksPatch,
    type WorksSettings,
    type WorksUsage,
  } from '../lib/worksApi';
  import {
    KIND_SHORT,
    KIND_TITLE,
    MODE_NOTE,
    MODE_TITLE,
    ORIGIN_TEXT,
    PAUSE_TITLE,
    RECIPES,
    STATE_TEXT,
    WORKS_MODES,
    WORK_KINDS,
    addressView,
    bodyExtraText,
    callTokensText,
    nightText,
    num,
    outcomeText,
    parseBodyExtra,
    pauseHint,
    pinnedUrl,
    reachText,
    reasonText,
    usageText,
    vaultAddress,
    withRecipe,
    workResultText,
    type WorkKind,
    type WorksMode,
  } from '../lib/worksText';

  let { me, onexpired }: { me: Me; onexpired: () => void } = $props();

  let settings = $state<WorksSettings | null>(null);
  let usage = $state<WorksUsage | null>(null);
  let list = $state<WorksList | null>(null);
  let calls = $state<WorkCall[]>([]);
  let days = $state(7);
  let vault = $state<VaultItem[]>([]);
  let vaultLocked = $state('');
  let error = $state('');
  let callsError = $state('');
  /** Итог действия страницы и его ошибка (строка у кнопок, правило Д). */
  let modeNotice = $state('');
  let modeError = $state('');
  let connNotice = $state('');
  /** Итог раздела подключения: после «Подтвердить адрес» кнопка уходит вместе с паузой — фокус сюда. */
  let connBox = $state<HTMLElement | null>(null);
  let connError = $state('');
  let runNotice = $state('');
  let busy = $state(false);
  /** Время ответа: «у вас 04:30» — от него. */
  let now = $state(Date.now());

  let phone = $derived(viewport.current === 'phone');

  function expired(e: unknown): boolean {
    if (e instanceof ApiError && e.status === 401) {
      onexpired();
      return true;
    }
    return false;
  }

  async function loadSettings() {
    const s = await worksSettings();
    settings = s.works;
    attention.pause = s.works.pause?.class ?? null;
  }

  async function loadQueue() {
    const [l, u] = await Promise.all([listWorks(), worksUsage()]);
    list = l;
    usage = u;
    now = Date.now();
  }

  async function loadCalls() {
    callsError = '';
    try {
      calls = (await workCalls(days)).calls;
    } catch (e) {
      if (expired(e)) return;
      callsError = errorText(e);
    }
  }

  async function loadVault() {
    try {
      const v = await listVault(me.user_id, 'knowledge');
      vault = v.items;
      vaultLocked = v.locked ? (v.reason ?? 'Vault заблокирован') : '';
    } catch (e) {
      if (expired(e)) return;
      vault = [];
    }
  }

  async function load() {
    error = '';
    try {
      await Promise.all([loadSettings(), loadQueue(), loadCalls(), loadVault()]);
    } catch (e) {
      if (expired(e)) return;
      error = errorText(e);
    }
    plan();
  }

  // ---- опрос очереди, пока работа модели идёт (ручной запуск) ----
  let timer: ReturnType<typeof setTimeout> | undefined;
  let alive = true;
  const POLL_MS = 3000;
  /** После ручного запуска модели — опрос ещё минуту: работа появляется в очереди не сразу (её
   * ставит учтённый запуск сервера после ответа 202). */
  const AFTER_RUN_MS = 60_000;
  let pollUntil = 0;
  const working = () =>
    Date.now() < pollUntil || !!list?.works.some((w) => w.state === 'running' || (w.state === 'queued' && w.mode === 'model'));
  function plan() {
    clearTimeout(timer);
    timer = undefined;
    if (alive && working() && !document.hidden) {
      timer = setTimeout(async () => {
        try {
          await Promise.all([loadQueue(), loadCalls()]);
        } catch (e) {
          if (expired(e)) return;
        }
        plan();
      }, POLL_MS);
    }
  }

  onMount(() => {
    void load();
    const onvis = () => {
      if (!document.hidden) plan();
    };
    document.addEventListener('visibilitychange', onvis);
    return () => {
      alive = false;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onvis);
    };
  });

  /** Правка настроек: ответ — все настройки (у подключения — `external` и `host` для плашки). */
  async function save(patch: WorksPatch) {
    const r = await patchWorks(patch);
    settings = r.works;
    attention.pause = r.works.pause?.class ?? null;
    return r;
  }

  // ---- режим (правило Д: сразу) ----

  async function setMode(m: WorksMode) {
    modeNotice = '';
    modeError = '';
    busy = true;
    try {
      await save({ mode: m });
      modeNotice = `Общий режим: ${MODE_TITLE[m]}.`;
      await loadQueue();
    } catch (e) {
      if (expired(e)) return;
      modeError = errorText(e);
      // выбор не принят: переключатели и списки — снова по сохранённым настройкам
      if (settings) settings = { ...settings };
    } finally {
      busy = false;
    }
  }

  async function setKind(k: WorkKind, v: string) {
    modeNotice = '';
    modeError = '';
    busy = true;
    try {
      await save({ kinds: { [k]: v === '' ? null : (v as WorksMode) } });
      modeNotice = v === '' ? `${KIND_TITLE[k]}: как общий режим.` : `${KIND_TITLE[k]}: ${MODE_TITLE[v as WorksMode]}.`;
      await loadQueue();
    } catch (e) {
      if (expired(e)) return;
      modeError = errorText(e);
      // выбор не принят: переключатели и списки — снова по сохранённым настройкам
      if (settings) settings = { ...settings };
    } finally {
      busy = false;
    }
  }

  /** Итог выключателя ночной проверки — у него же (раздел «Ночь»). */
  let nightNotice = $state('');
  let nightError = $state('');

  async function setCheck(on: boolean) {
    nightNotice = '';
    nightError = '';
    try {
      await save({ check: on ? 'on' : 'off' });
      nightNotice = on ? 'Ночная проверка вашей памяти включена.' : 'Ночная проверка вашей памяти выключена (ручной прогон — на странице «Проверка памяти»).';
    } catch (e) {
      if (expired(e)) return;
      nightError = errorText(e);
      // выбор не принят: выключатель — снова по сохранённой настройке
      if (settings) settings = { ...settings };
    }
  }

  // ---- подключение модели (окно формы, правило Б) ----

  let connOpen = $state(false);
  let fVault = $state('');
  let fModel = $state('');
  let fAuth = $state<ModelConn['auth']>('key');
  let fMax = $state(4000);
  let fField = $state<ModelConn['token_field']>('auto');
  let fExtra = $state('');
  let dlgNotice = $state('');
  let dlgError = $state('');
  type Fields = Record<string, unknown>;
  /** Поля при открытии окна (или после сохранения в окне) — с ними сравнивается набранное. */
  let base = $state.raw<Fields>({});
  const fieldsNow = (): Fields => ({ vault: fVault, model: fModel, auth: fAuth, max: fMax, field: fField, extra: fExtra });
  const connDirty = () => connOpen && isDirty(fieldsNow(), base);

  /** Записи личного Vault с адресом — выбор подключения (§6.9 п. 1: «по именам»). */
  let choices = $derived(vault.filter((v) => vaultAddress(v.data)));
  let chosen = $derived(vault.find((v) => v.id === fVault) ?? null);
  let chosenAddr = $derived(chosen ? vaultAddress(chosen.data) : '');
  let view = $derived(addressView(chosenAddr));
  let extraCheck = $derived(parseBodyExtra(fExtra));
  let extraError = $derived('error' in extraCheck ? extraCheck.error : '');

  function openConn() {
    const m = settings?.model;
    const id = m?.vault.replace(/^v:/, '') ?? '';
    fVault = id || choices[0]?.id || '';
    fModel = m?.model ?? '';
    fAuth = m?.auth ?? 'key';
    fMax = m?.max_tokens ?? 4000;
    fField = m?.token_field ?? 'auto';
    fExtra = bodyExtraText(m?.body_extra);
    base = fieldsNow();
    dlgNotice = '';
    dlgError = '';
    connOpen = true;
  }

  /** Проверка полей окна; ошибка — исключением (текст — в окне). */
  function connBody(): NonNullable<WorksPatch['model']> {
    if (!fVault) throw new Error('Выберите запись личного Vault с адресом модели.');
    const model = fModel.trim();
    if (!model || model.length > 200) throw new Error('Модель — имя у поставщика, от 1 до 200 символов.');
    const max = Number(fMax);
    if (!Number.isInteger(max) || max < 256 || max > 32000) throw new Error('Предел ответа — от 256 до 32 000 токенов.');
    const extra = parseBodyExtra(fExtra);
    if ('error' in extra) throw new Error(`body_extra: ${extra.error}`);
    // запись — по id (`v:<id>`), а не по имени: переименованная запись не подменится другой с тем же
    // именем (контракт задачи 2 после ревью r1–r2)
    return { protocol: 'openai', vault: `v:${fVault}`, model, auth: fAuth, max_tokens: max, token_field: fField, body_extra: extra.value };
  }

  /** 409 `vault_changed` при сохранении: адрес записи изменили после закрепления. */
  function changedText(e: unknown): string | null {
    if (!(e instanceof ApiError) || e.status !== 409 || e.problem?.code !== 'vault_changed') return null;
    return `Адрес записи изменён после сохранения: был ${String(e.problem.was ?? '?')} → стал ${String(e.problem.now ?? '?')}. Подтвердите новый адрес на странице («Подтвердить адрес») или верните прежний в записи Vault.`;
  }

  /** Сохранить подключение; `close` — закрыть окно после удачи. */
  async function saveConn(close: boolean): Promise<boolean> {
    dlgError = '';
    dlgNotice = '';
    const had = settings?.pause;
    try {
      await save({ model: connBody() });
    } catch (e) {
      if (expired(e)) return false;
      const changed = changedText(e);
      if (changed) void loadSettings();
      if (close) throw new Error(changed ?? errorText(e));
      dlgError = changed ?? errorText(e);
      return false;
    }
    base = fieldsNow();
    const text = `Подключение сохранено${had ? ', пауза снята' : ''}.`;
    if (close) {
      connNotice = text;
      connOpen = false;
    } else dlgNotice = text;
    return true;
  }

  function addRecipe(v: Record<string, unknown>) {
    const t = withRecipe(fExtra, v);
    if (t !== null) fExtra = t;
  }

  /** Итог проверки подключения — строкой. */
  function testText(r: Awaited<ReturnType<typeof testModel>>): string {
    if (r.ok) return `Подключение работает: ответ за ${r.ms ?? '?'} мс.`;
    const cls = r.class ? outcomeText(r.class, null) : 'не работает';
    return `Подключение не работает: ${cls}.${r.hint ? ` ${r.hint}` : ''}`;
  }

  /** «Проверить подключение»: на паузе `vault_changed` — 409 без обращения (сервер), страница
   * показывает «был → стал». */
  async function runTest(): Promise<{ text: string; ok: boolean }> {
    try {
      const r = await testModel();
      await loadSettings();
      void loadCalls();
      return { text: testText(r), ok: r.ok };
    } catch (e) {
      if (expired(e)) return { text: '', ok: false };
      if (e instanceof ApiError && e.problem?.code === 'vault_changed') {
        await loadSettings();
        return { text: 'Адрес подключения изменён — подтвердите его (плашка выше), проверка на новый адрес не идёт.', ok: false };
      }
      return { text: errorText(e), ok: false };
    }
  }

  async function testFromPage() {
    connNotice = '';
    connError = '';
    busy = true;
    const r = await runTest();
    busy = false;
    if (r.ok) connNotice = r.text;
    else connError = r.text;
  }

  let dlgTesting = $state(false);
  /** «Проверить подключение» в окне: набранное сначала сохраняется (проверяется сохранённое). */
  async function testInDialog() {
    dlgTesting = true;
    try {
      // набранное не сохранено или подключения ещё нет — сначала сохранить (сервер проверяет сохранённое)
      if ((!settings?.model || isDirty(fieldsNow(), base)) && !(await saveConn(false))) return;
      dlgError = '';
      const r = await runTest();
      if (r.ok) dlgNotice = `${dlgNotice ? `${dlgNotice} ` : ''}${r.text}`;
      else dlgError = r.text;
    } finally {
      dlgTesting = false;
    }
  }

  // ---- «Подтвердить адрес» (окно-подтверждение, `confirm_url` = «стал») ----

  let confirmOpen = $state(false);
  let confirmWas = $state('');
  let confirmNow = $state('');

  function openConfirm() {
    const p = settings?.pause;
    if (!p) return;
    confirmWas = p.from ?? '';
    confirmNow = p.to ?? '';
    confirmOpen = true;
  }

  async function confirmAddress() {
    const m = settings?.model;
    if (!m) throw new Error('Подключения нет: сохраните его заново.');
    try {
      // только поля входа: `pinned`, `host`, `external` ответа сервер в `PATCH` не принимает
      await save({ model: { ...connInput(m), confirm_url: confirmNow } });
    } catch (e) {
      if (expired(e)) return;
      if (e instanceof ApiError && e.status === 409 && e.problem?.code === 'vault_changed') {
        // адрес сменили ещё раз, пока окно было открыто: новые «был → стал», подтверждать — их
        confirmWas = String(e.problem.was ?? confirmWas);
        confirmNow = String(e.problem.now ?? confirmNow);
        void loadSettings();
        throw new Error(`Адрес снова изменился: был ${confirmWas} → стал ${confirmNow}. Проверьте новый адрес и подтвердите ещё раз.`);
      }
      throw new Error(errorText(e));
    }
    confirmOpen = false;
    connNotice = `Адрес подтверждён: ${confirmNow}. Пауза снята.`;
  }

  // ---- «Запустить сейчас» (окно-подтверждение) ----

  let runKind = $state<WorkKind | null>(null);
  let effective = $derived(settings?.effective ?? list?.modes ?? {});
  const modeOf = (k: WorkKind) => effective[k]?.mode ?? 'off';

  async function startRun() {
    const k = runKind;
    if (!k) return;
    runNotice = '';
    let r;
    try {
      r = await runWorks(k);
    } catch (e) {
      if (expired(e)) return;
      throw new Error(errorText(e));
    }
    runKind = null;
    if (r.mode === 'model') pollUntil = Date.now() + AFTER_RUN_MS;
    runNotice =
      r.mode === 'model'
        ? `${KIND_TITLE[k]}: запуск ${r.run} идёт — итог появится в очереди и журнале отправок.`
        : r.queued.length
          ? `${KIND_TITLE[k]}: в очереди агента — ${r.queued.join(', ')}. Агент возьмёт работу memory_recall event=work.`
          : `${KIND_TITLE[k]}: работы нет — кандидатов не нашлось.`;
    await Promise.all([loadQueue(), loadCalls()]);
    plan();
  }

  // ---- производное ----

  // ---- «Отключить модель» (окно-подтверждение, необратимо: подключение придётся вводить заново) ----

  let disconnectOpen = $state(false);

  async function disconnect() {
    try {
      await save({ model: null });
    } catch (e) {
      if (expired(e)) return;
      throw new Error(errorText(e));
    }
    disconnectOpen = false;
    connNotice = 'Модель отключена: данные памяти ей больше не уходят. Режим «модель» не действует, пока не подключите её снова.';
    await loadQueue();
  }

  let pause = $derived(settings?.pause ?? null);
  /** Хост подключения вне LAN — только по полю сервера (`external`, без DNS при чтении). */
  let external = $derived(settings?.model?.external ? settings.model.host || 'адрес подключения' : null);
  /** Какой-нибудь вид работает в режиме «модель» — данные уходят сейчас, иначе — уйдут при выборе. */
  let modelActive = $derived(Object.values(settings?.effective ?? {}).some((e) => e.mode === 'model'));
  let modelVault = $derived(settings?.model ? vault.find((v) => `v:${v.id}` === settings!.model!.vault) ?? null : null);
  /** Итог последней ночи по видам. */
  let lastNight = $derived.by(() => {
    const n = usage?.last_night;
    if (!n || !list) return [];
    return list.works.filter((w) => w.origin === 'nightly' && w.night === n);
  });
  const kindTitle = (k: string) => (KIND_TITLE as Record<string, string>)[k] ?? k;
</script>

<section class="panel works-page" aria-labelledby="works-h">
  <h2 id="works-h">Внутренние работы</h2>
  <p class="muted small">
    Работы памяти над собственными записями: пробные вопросы ночной проверки и кандидаты в Rules. Знания по их итогу не
    меняются — пробы идут в проверку, кандидатов принимает человек («Кандидаты в Rules»).
  </p>
  {#if error}<p class="error" role="alert">Не удалось загрузить: {error}</p>{/if}

  {#if external}
    <div class="alert ext" role="note" aria-label="Данные вне локальной сети" data-testid="works-external">
      {#if modelActive}
        <p class="alert-title">Данные памяти уходят за пределы локальной сети: {external}</p>
        <p>Подключённая модель — вне локальной сети: ей уходит маскированный пакет работ (без Human, Vault и записей участников).</p>
      {:else}
        <p class="alert-title">Подключённая модель — вне локальной сети: {external}</p>
        <p>Данные памяти уйдут за пределы локальной сети, когда вид работ будет в режиме «модель»: ей уходит маскированный пакет работ (без Human, Vault и записей участников).</p>
      {/if}
    </div>
  {/if}
  {#if pause}
    <div class="alert pause" role="note" aria-label="Исполнитель на паузе" data-testid="works-pause">
      <p class="alert-title">
        Исполнитель на паузе с {localTime(pause.since)}: {PAUSE_TITLE[pause.class] ?? pause.class}
      </p>
      {#if pause.class === 'vault_changed'}
        <p data-testid="works-pause-change">Был <code>{pause.from ?? '?'}</code> → стал <code>{pause.to ?? '?'}</code></p>
      {/if}
      <p>{pauseHint(pause.class)}</p>
      {#if pause.class === 'vault_changed' && settings?.model}
        <button type="button" class="primary" onclick={openConfirm}>Подтвердить адрес</button>
      {/if}
    </div>
  {/if}
</section>

{#if settings}
  {@const s = settings}
  <section class="panel block" aria-labelledby="works-mode-h">
    <h2 id="works-mode-h">Исполнитель</h2>
    <fieldset class="modes">
      <legend>Общий режим</legend>
      {#each WORKS_MODES as m (m)}
        <label class="opt">
          <input
            type="radio"
            name="works-mode"
            value={m}
            checked={s.mode === m}
            disabled={busy || (m === 'model' && !s.model)}
            onchange={() => setMode(m)}
          />
          <span><b>{MODE_TITLE[m]}</b> — <span class="muted">{MODE_NOTE[m]}</span>
            {#if m === 'model' && !s.model}<span class="muted"> Сначала подключите модель (ниже).</span>{/if}</span>
        </label>
      {/each}
    </fieldset>
    <div class="kinds">
      {#each WORK_KINDS as k (k)}
        {@const eff = s.effective[k]}
        <div class="kind">
          <label>
            {KIND_TITLE[k]}
            <select value={s.kinds[k] ?? ''} disabled={busy} onchange={(e) => setKind(k, e.currentTarget.value)}>
              <option value="">как общий ({MODE_TITLE[s.mode]})</option>
              {#each WORKS_MODES as m (m)}<option value={m} disabled={m === 'model' && !s.model}>{MODE_TITLE[m]}</option>{/each}
            </select>
          </label>
          <span class="muted small" data-testid="works-effective-{k}">
            действует: {MODE_TITLE[eff?.mode ?? 'off']}{eff?.reason ? ` — ${reasonText(eff.reason)}` : ''}
          </span>
        </div>
      {/each}
    </div>
    <Notice text={modeNotice} />
    {#if modeError}<p class="error" role="alert">{modeError}</p>{/if}
  </section>

  <section class="panel block" aria-labelledby="works-model-h">
    <h2 id="works-model-h">Подключённая модель</h2>
    {#if s.model}
      {@const m = s.model}
      <dl class="facts">
        <div><dt>Запись Vault</dt><dd title={m.vault}>{modelVault?.title ?? 'запись личного Vault'}</dd></div>
        <div><dt>Адрес</dt><dd><code class="path">{m.pinned ? pinnedUrl(m.pinned) : 'не закреплён'}</code></dd></div>
        <div><dt>Модель</dt><dd>{m.model}</dd></div>
        <div><dt>Ключ</dt><dd>{m.auth === 'key' ? 'значение записи Vault' : 'без ключа'}</dd></div>
        <div><dt>Предел ответа</dt><dd>{num(m.max_tokens)} токенов{m.token_field !== 'auto' ? ` (${m.token_field})` : ''}</dd></div>
        <div><dt>body_extra</dt><dd>{Object.keys(m.body_extra).length ? Object.keys(m.body_extra).join(', ') : 'нет'}</dd></div>
      </dl>
    {:else}
      <p class="muted">
        Модель не подключена. Подключение — OpenAI-совместимый Chat Completions (OpenAI, Anthropic через его
        OpenAI-совместимый вход, Qwen, Kimi, DeepSeek, OpenRouter, Ollama, llama.cpp, vLLM, LM Studio): адрес и ключ —
        запись вашего личного Vault.
      </p>
    {/if}
    <div class="row actions">
      <button type="button" class="primary" onclick={openConn}>{s.model ? 'Изменить подключение' : 'Подключить модель'}</button>
      {#if s.model}
        <button type="button" disabled={busy} onclick={testFromPage}>Проверить подключение</button>
        <button type="button" class="danger" disabled={busy} onclick={() => (disconnectOpen = true)}>Отключить модель</button>
      {/if}
    </div>
    <div bind:this={connBox}><Notice text={connNotice} /></div>
    {#if connError}<p class="error" role="alert">{connError}</p>{/if}
  </section>
{/if}

{#if usage}
  {@const u = usage}
  <section class="panel block" aria-labelledby="works-night-h">
    <h2 id="works-night-h">Ночь</h2>
    <p data-testid="works-night">{nightText(u.night, now)}</p>
    {#if u.last_night}
      <p class="small">Последняя ночь — {u.last_night}:</p>
      {#if lastNight.length}
        <ul class="small night-list">
          {#each lastNight as w (w.id)}
            <li>{kindTitle(w.kind)}: {STATE_TEXT[w.state] ?? w.state}{workResultText(w.result) ? ` · ${workResultText(w.result)}` : ''}{w.error_class ? ` · ${outcomeText(w.error_class, null)}` : ''}</li>
          {/each}
        </ul>
      {:else}
        <p class="muted small">Работ в ту ночь не было.</p>
      {/if}
    {:else}
      <p class="muted small">Ночных работ ещё не было.</p>
    {/if}
    {#if settings}
      <label class="opt">
        <input type="checkbox" checked={settings.check === 'on'} onchange={(e) => setCheck(e.currentTarget.checked)} />
        Ночная проверка моей памяти
      </label>
      <Notice text={nightNotice} />
      {#if nightError}<p class="error" role="alert">{nightError}</p>{/if}
    {/if}
  </section>

  <section class="panel block" aria-labelledby="works-queue-h">
    <h2 id="works-queue-h">Очередь работ</h2>
    <div class="row actions">
      {#each WORK_KINDS as k (k)}
        <button type="button" disabled={modeOf(k) === 'off'} onclick={() => (runKind = k)} aria-label="Запустить сейчас: {KIND_TITLE[k]}">
          Запустить сейчас: {KIND_SHORT[k]}
        </button>
      {/each}
    </div>
    {#each WORK_KINDS as k (k)}
      {#if modeOf(k) === 'off'}
        <p class="muted small">{KIND_TITLE[k]}: режим «выкл»{effective[k]?.reason ? ` — ${reasonText(effective[k]!.reason)}` : ''}; запуск — после выбора агента или модели.</p>
      {/if}
    {/each}
    <Notice text={runNotice} />
    {#if list && list.works.length}
      {#if phone}
        <ul class="cards" aria-label="Работы">
          {#each list.works as w (w.id)}
            <li class="card" data-work={w.id}>
              <div class="card-head"><b>{kindTitle(w.kind)}</b> <span>{STATE_TEXT[w.state] ?? w.state}</span></div>
              <div class="small">{localTime(w.created_at)} · {MODE_TITLE[w.mode]} · {ORIGIN_TEXT[w.origin] ?? w.origin}</div>
              {#if workResultText(w.result) || w.error_class}
                <div class="small">{workResultText(w.result)}{w.error_class ? ` ${outcomeText(w.error_class, null)}` : ''}</div>
              {/if}
              <div class="small muted"><code>{w.id}</code> · записей {w.items.length}</div>
            </li>
          {/each}
        </ul>
      {:else}
        <div class="table-wrap">
          <table aria-label="Работы">
            <thead><tr><th>Создана</th><th>Вид</th><th>Исполнитель</th><th>Откуда</th><th>Состояние</th><th>Итог</th></tr></thead>
            <tbody>
              {#each list.works as w (w.id)}
                <tr data-work={w.id}>
                  <td class="nowrap">{localTime(w.created_at)}<div class="muted small"><code>{w.id}</code></div></td>
                  <td>{kindTitle(w.kind)}<div class="muted small">записей {w.items.length}</div></td>
                  <td>{MODE_TITLE[w.mode]}</td>
                  <td>{ORIGIN_TEXT[w.origin] ?? w.origin}{w.night ? ` ${w.night}` : ''}</td>
                  <td>{STATE_TEXT[w.state] ?? w.state}</td>
                  <td class="result">{workResultText(w.result)}{w.error_class ? ` ${outcomeText(w.error_class, null)}` : ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    {:else if list}
      <p class="muted">Работ пока не было.</p>
    {/if}
  </section>

  <section class="panel block" aria-labelledby="works-tokens-h">
    <h2 id="works-tokens-h">Токены модели</h2>
    <dl class="facts">
      <div>
        <dt>Сутки (UTC)</dt>
        <dd data-testid="works-day">{usageText(u.day.tokens, u.day.limit)}</dd>
        {#if u.day.limit > 0}<meter min="0" max={u.day.limit} value={u.day.tokens} aria-label="Токены суток"></meter>{/if}
      </div>
      <div>
        <dt>Месяц (UTC)</dt>
        <dd data-testid="works-month">{usageText(u.month.tokens, u.month.limit)}</dd>
        {#if u.month.limit > 0}<meter min="0" max={u.month.limit} value={u.month.tokens} aria-label="Токены месяца"></meter>{/if}
      </div>
    </dl>
    <p class="muted small">
      Вход и выход модели по журналу отправок. Потолок сервера — {num(u.ceiling.daily)} в сутки и {num(u.ceiling.monthly)}
      в месяц (настройка администратора сервера); не помещается — запрос не отправляется до следующей ночи.
    </p>
  </section>
{/if}

<section class="panel block" aria-labelledby="works-calls-h">
  <h2 id="works-calls-h">Журнал отправок</h2>
  <p class="muted small">Обращения к модели — время, вид, модель, токены, итог; текста пакета и ответа в журнале нет. Хранится 180 суток.</p>
  <label class="row">
    За
    <select bind:value={days} onchange={loadCalls} aria-label="Период журнала">
      <option value={1}>сутки</option>
      <option value={7}>7 суток</option>
      <option value={30}>30 суток</option>
      <option value={180}>180 суток</option>
    </select>
  </label>
  {#if callsError}<p class="error" role="alert">{callsError}</p>{/if}
  {#if !calls.length}
    <p class="muted">Обращений не было.</p>
  {:else if phone}
    <ul class="cards" aria-label="Журнал отправок">
      {#each calls as c, i (i)}
        <li class="card" data-call={c.kind}>
          <div class="card-head"><b>{KIND_SHORT[c.kind] ?? c.kind}</b> <span>{outcomeText(c.outcome, c.http_status)}</span></div>
          <div class="small">{localTime(c.ts)} · {c.model} · {c.host}{c.external ? ' · вне LAN' : ''}</div>
          <div class="small">токены {callTokensText(c)} · {num(c.ms)} мс{c.items != null ? ` · записей ${c.items}` : ''}</div>
        </li>
      {/each}
    </ul>
  {:else}
    <div class="table-wrap">
      <table aria-label="Журнал отправок">
        <thead><tr><th>Время</th><th>Вид</th><th>Модель</th><th>Хост</th><th>Токены</th><th>мс</th><th>Итог</th></tr></thead>
        <tbody>
          {#each calls as c, i (i)}
            <tr data-call={c.kind}>
              <td class="nowrap">{localTime(c.ts)}</td>
              <td>{KIND_SHORT[c.kind] ?? c.kind}{c.items != null ? ` · ${c.items}` : ''}</td>
              <td>{c.model}</td>
              <td>{c.host}{#if c.external}<span class="tag ext-tag">вне LAN</span>{/if}</td>
              <td class="num">{callTokensText(c)}</td>
              <td class="num">{num(c.ms)}</td>
              <td>{outcomeText(c.outcome, c.http_status)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<FormDialog
  open={connOpen}
  title="Подключение модели"
  description="OpenAI-совместимый Chat Completions. Адрес (поле url записи, с /v1, как у поставщика) и ключ — запись вашего личного Vault."
  submit="Сохранить"
  wide
  busy={dlgTesting}
  error={dlgError}
  notice={dlgNotice}
  dirty={connDirty}
  onsubmit={async () => {
    await saveConn(true);
  }}
  onclose={() => (connOpen = false)}
>
  {#if choices.length}
    <!-- адрес выбранной записи и его класс — подписью над выбором (видно до сохранения; описание
         поля для чтения с экрана — `aria-describedby`, в имя поля не входит): поля окна идут плотно -->
    <label>
      Запись Vault
      <span class="field-note" aria-hidden="true" id="works-addr" data-testid="works-address">
        адрес: <code class="path">{chosenAddr || 'нет адреса'}</code>{#if view}<span class="muted">{` — ${reachText(view.reach)}`}</span>{/if}
        {#if view?.problem}<span class="error-text">{view.problem}</span>{/if}
        {#if view?.reach === 'external' && !view.problem}<span class="warn note-ext">Данные памяти уйдут за пределы локальной сети: {view.host}</span>{/if}
      </span>
      <select bind:value={fVault} data-autofocus aria-describedby="works-addr">
        {#each choices as v (v.id)}<option value={v.id}>{v.title}</option>{/each}
      </select>
    </label>
  {:else}
    <p class="warn small" role="note">
      {vaultLocked ? `${vaultLocked}. ` : ''}В личном Vault нет записей с адресом. Создайте запись: тип token, ссылка — адрес с /v1,
      значение — ключ; например <code>zam vault set my-model --type token --link https://адрес/v1 --global</code>.
    </p>
  {/if}
  <!-- «Имя модели», а не «Модель»: имена записей Vault в выборе выше часто содержат «модель» -->
  <label>
    Имя модели
    <input bind:value={fModel} required maxlength="200" placeholder="как у поставщика, например qwen3-30b-a3b" />
  </label>
  <label>
    Ключ
    <select bind:value={fAuth}>
      <option value="key">значение записи Vault</option>
      <option value="none">без ключа (локальный сервер)</option>
    </select>
  </label>
  <label>
    Предел ответа, токенов
    <input type="number" min="256" max="32000" step="1" bind:value={fMax} />
  </label>
  <label>
    Поле предела
    <span class="field-note muted" id="works-field-note">авто — max_completion_tokens у api.openai.com, max_tokens у прочих</span>
    <select bind:value={fField} aria-describedby="works-field-note">
      <option value="auto">авто</option>
      <option value="max_tokens">max_tokens</option>
      <option value="max_completion_tokens">max_completion_tokens</option>
    </select>
  </label>
  <label>
    body_extra (JSON, необязательно)
    <textarea rows="4" bind:value={fExtra} aria-invalid={!!extraError} spellcheck="false" placeholder={'{"chat_template_kwargs": {"enable_thinking": false}}'}></textarea>
  </label>
  {#if extraError}<p class="error small" role="alert">{extraError}</p>{/if}
  <div class="row recipes" role="group" aria-label="Рецепты body_extra">
    {#each RECIPES as r (r.name)}<button type="button" class="small-btn" onclick={() => addRecipe(r.value)}>{r.title}</button>{/each}
  </div>
  {#snippet extra()}
    <!-- без сохранённого подключения проверять нечего: сначала сохранить (сервер проверяет сохранённое) -->
    <button type="button" disabled={dlgTesting || !choices.length} onclick={testInDialog}>
      {connOpen && (!settings?.model || isDirty(fieldsNow(), base)) ? 'Сохранить и проверить' : 'Проверить подключение'}
    </button>
  {/snippet}
</FormDialog>

<FormDialog
  open={confirmOpen}
  confirm
  title="Подтвердить новый адрес подключения?"
  description="После подтверждения сервер будет отправлять пакеты работ и ключ на новый адрес. Если адрес меняли не вы — отмените и верните прежний адрес в записи Vault."
  submit="Подтвердить адрес"
  refocus={() => connBox?.querySelector('[role="status"]')}
  onsubmit={confirmAddress}
  onclose={() => (confirmOpen = false)}
>
  <!-- адреса — моноширинно, каждый своей строкой: сверка «был / стал» — последний рубеж против подмены -->
  <dl class="addr-change" data-testid="works-confirm-change">
    <dt>Был</dt>
    <dd><code data-testid="works-confirm-was">{confirmWas}</code></dd>
    <dt>Стал</dt>
    <dd><code data-testid="works-confirm-now">{confirmNow}</code></dd>
  </dl>
</FormDialog>

<FormDialog
  open={disconnectOpen}
  confirm
  danger
  title="Отключить модель?"
  description="Подключение удалится: модель больше не получит данных памяти, режим «модель» перестанет действовать. Запись Vault останется; подключить можно заново."
  submit="Отключить"
  refocus={() => connBox?.querySelector('[role="status"]')}
  onsubmit={disconnect}
  onclose={() => (disconnectOpen = false)}
/>

<FormDialog
  open={runKind !== null}
  confirm
  title={runKind ? `Запустить «${KIND_TITLE[runKind]}» сейчас?` : ''}
  description={runKind && modeOf(runKind) === 'model'
    ? 'Подключённая модель получит пакет сейчас, вне окна ночи; токены — из пределов суток и месяца. Ручных запусков вида — до 4 пакетов за сутки UTC.'
    : 'Работа встанет в очередь агента: он возьмёт её в своей сессии (memory_recall event=work), когда закончит текущий шаг.'}
  submit="Запустить"
  onsubmit={startRun}
  onclose={() => (runKind = null)}
/>

<style>
  .block {
    margin-top: 16px;
  }
  .small {
    font-size: 13px;
  }
  .alert {
    margin: 12px 0 0;
    padding: 10px 12px;
    border: 1px solid var(--callout-warning);
    border-left-width: 4px;
    border-radius: 6px;
    background: var(--warn-bg);
    color: var(--warn-text);
    overflow-wrap: anywhere;
  }
  .alert p {
    margin: 4px 0;
  }
  .alert-title {
    font-weight: 600;
  }
  .alert.pause {
    border-color: var(--danger);
  }
  .modes {
    display: grid;
    gap: 8px;
    min-width: 0;
    margin: 0 0 12px;
    padding: 8px 12px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .opt {
    display: flex;
    gap: 8px;
    align-items: flex-start;
  }
  .opt input {
    margin-top: 4px;
    flex: none;
  }
  .kinds {
    display: grid;
    gap: 10px;
  }
  .kind {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    align-items: center;
  }
  .kind label {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  /* телефон: подпись вида — над выбором у обоих видов (одно выравнивание, ревью r1) */
  @media (max-width: 719px) {
    .kind label {
      flex-direction: column;
      align-items: flex-start;
      gap: 4px;
    }
  }
  .addr-change {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    margin: 0;
  }
  .addr-change dt {
    color: var(--muted);
  }
  .addr-change dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .actions {
    margin: 12px 0 8px;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px 24px;
    margin: 0 0 4px;
  }
  .facts dt {
    font-size: 12px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .facts dd {
    margin: 2px 0 0;
    overflow-wrap: anywhere;
  }
  .facts meter {
    width: 100%;
  }
  .path {
    overflow-wrap: anywhere;
  }
  .night-list {
    margin: 4px 0 8px;
    padding-left: 20px;
  }
  .nowrap {
    white-space: nowrap;
  }
  .num {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .result {
    min-width: 160px;
  }
  .tag {
    margin-left: 6px;
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
  }
  .ext-tag {
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .field-note {
    display: block;
    margin-top: 2px;
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .field-note > span {
    display: block;
  }
  .field-note .muted {
    display: inline;
  }
  .error-text {
    color: var(--danger);
  }
  .note-ext {
    margin-top: 4px;
    padding: 4px 8px;
  }
  .recipes {
    gap: 6px;
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
    justify-content: space-between;
    gap: 8px;
  }
  td {
    vertical-align: top;
  }
</style>
