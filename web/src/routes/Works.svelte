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
  // и местное, окно, итог последней ночи по видам и выключатель ночной проверки (неактивен с
  // причиной, если сервер её не проводит — `usage.check`); очередь работ и
  // «Запустить сейчас» (окно-подтверждение; у модели — опрос очереди, пока работа идёт); журнал
  // отправок (метаданные, без текста); токены суток и месяца к пределам. Таблицы — `.table-wrap`,
  // на телефоне — карточки (§5.1).
  // M7.4 (§7.7 п. 2): у администратора — блок «Все пользователи» (сводка `GET /admin/works`,
  // `WorksAdmin.svelte`); имя пользователя ведёт на его вид страницы (`#/works?user=<id>`): та же
  // страница по его настройкам (`/admin/users/{id}/settings`), очереди, токенам, журналу и
  // Vault, правки — от имени администратора (аудит). Точку паузы в шапке ставит только своя
  // страница. Смену пользователя страница не переживает — оболочка монтирует её заново (`{#key}`).
  // M7.6 (§9.2 п. 6; решения 166–170): «Протокол» — OpenAI-совместимый или Anthropic Messages (у
  // Anthropic «Поле предела» скрыто, рецепты и подсказка адреса — свои); в выборе записи — только
  // записи с адресом API (`http(s)://`), пункт «<имя> — <адрес>»; «Новая запись…», «Изменить
  // запись» и «Исправить адрес» — окно записи Vault `VaultForm` (ленивый чанк) поверх окна
  // подключения, после сохранения запись выбрана; куда уйдёт запрос — под выбором, адрес без /v1
  // — предупреждение; неудачная проверка — понятная причина и полный адрес запроса.
  import { onMount } from 'svelte';
  import FormDialog from '../components/FormDialog.svelte';
  import Notice from '../components/Notice.svelte';
  import WorksAdmin from '../components/WorksAdmin.svelte';
  import { ApiError, errorText, isAdminSession, listVault, type Me, type User, type VaultItem } from '../lib/api';
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
    CALL_NO_MS_HINT,
    KIND_SHORT,
    KIND_TITLE,
    MODE_NOTE,
    MODE_TITLE,
    ORIGIN_TEXT,
    PAUSE_TITLE,
    PROTOCOLS,
    PROTOCOL_TITLE,
    STATE_TEXT,
    WORKS_MODES,
    WORK_KINDS,
    addressView,
    bodyExtraText,
    callDurationText,
    callMsText,
    callTokensText,
    checkOffText,
    nightText,
    num,
    outcomeText,
    parseBodyExtra,
    isModelUrl,
    pathWarning,
    pauseHint,
    pinnedUrl,
    protocolOf,
    reachText,
    reasonText,
    recipesOf,
    requestUrlOf,
    testFailText,
    usageText,
    vaultAddress,
    vaultChoiceLabel,
    withRecipe,
    workResultText,
    type Protocol,
    type WorkKind,
    type WorksMode,
  } from '../lib/worksText';

  let {
    me,
    users = [],
    user = null,
    onexpired,
  }: {
    me: Me;
    /** Пользователи сервера (администратор): имя пользователя страницы. */
    users?: User[];
    /** Пользователь страницы из адреса (`?user=`); `null` — свой. */
    user?: string | null;
    onexpired: () => void;
  } = $props();

  let admin = $derived(isAdminSession(me));
  /** Чужой пользователь страницы — только у администратора (сервер чужое иначе не отдаст: 403). */
  let target = $derived<string | null>(admin && user && user !== me.user_id ? user : null);
  let targetName = $derived(target ? (users.find((u) => u.id === target)?.name ?? target) : '');

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

  /** Точка паузы в шапке — по своему исполнителю: чужая страница её не трогает. */
  function markPause(s: WorksSettings) {
    if (!target) attention.pause = s.pause?.class ?? null;
  }

  async function loadSettings() {
    const s = await worksSettings(target);
    settings = s.works;
    markPause(s.works);
  }

  async function loadQueue() {
    const [l, u] = await Promise.all([listWorks(30, target), worksUsage(target)]);
    list = l;
    usage = u;
    now = Date.now();
  }

  async function loadCalls() {
    callsError = '';
    try {
      calls = (await workCalls(days, target)).calls;
    } catch (e) {
      if (expired(e)) return;
      callsError = errorText(e);
    }
  }

  async function loadVault() {
    try {
      const v = await listVault(target ?? me.user_id, 'knowledge');
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
    const r = await patchWorks(patch, target);
    settings = r.works;
    markPause(r.works);
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
      const whose = target ? `памяти пользователя ${targetName}` : 'вашей памяти';
      nightNotice = on ? `Ночная проверка ${whose} включена.` : `Ночная проверка ${whose} выключена (ручной прогон — на странице «Проверка памяти»).`;
    } catch (e) {
      if (expired(e)) return;
      nightError = errorText(e);
      // выбор не принят: выключатель — снова по сохранённой настройке
      if (settings) settings = { ...settings };
    }
  }

  // ---- подключение модели (окно формы, правило Б) ----

  let connOpen = $state(false);
  let fProto = $state<Protocol>('openai');
  let fVault = $state('');
  let fModel = $state('');
  let fAuth = $state<ModelConn['auth']>('key');
  let fMax = $state(4000);
  let fField = $state<ModelConn['token_field']>('auto');
  let fExtra = $state('');
  /** Адрес записи подключения, который человек сам поменял в окне записи Vault: при сохранении
   * подключения он подтверждается (`confirm_url`) — иначе пауза «адрес изменён» на свою же правку. */
  let fConfirm = $state('');
  let dlgNotice = $state('');
  let dlgError = $state('');
  type Fields = Record<string, unknown>;
  /** Поля при открытии окна (или после сохранения в окне) — с ними сравнивается набранное. */
  let base = $state.raw<Fields>({});
  const fieldsNow = (): Fields => ({ proto: fProto, vault: fVault, model: fModel, auth: fAuth, max: fMax, field: fField, extra: fExtra, confirm: fConfirm });
  const connDirty = () => connOpen && isDirty(fieldsNow(), base);

  /** Записи личного Vault с адресом API (`http(s)://…`) — выбор подключения (§6.9 п. 1; решение
   * 166: описания хостов вроде «192.0.2.10 (SSH…)» не показываются). */
  let choices = $derived(vault.filter((v) => isModelUrl(vaultAddress(v.data))));
  let chosen = $derived(vault.find((v) => v.id === fVault) ?? null);
  let chosenAddr = $derived(chosen ? vaultAddress(chosen.data) : '');
  let view = $derived(addressView(chosenAddr));
  /** Куда уйдёт запрос и замечание о пути адреса (решение 168). */
  let reqUrl = $derived(isModelUrl(chosenAddr) ? requestUrlOf(fProto, chosenAddr) : '');
  let pathWarn = $derived(pathWarning(fProto, chosenAddr));
  let extraCheck = $derived(parseBodyExtra(fExtra, fProto));
  let extraError = $derived('error' in extraCheck ? extraCheck.error : '');

  function openConn() {
    const m = settings?.model;
    const id = m?.vault.replace(/^v:/, '') ?? '';
    fProto = protocolOf(m?.protocol);
    fVault = id || choices[0]?.id || '';
    fModel = m?.model ?? '';
    fAuth = m?.auth ?? 'key';
    fMax = m?.max_tokens ?? 4000;
    fField = m?.token_field ?? 'auto';
    fExtra = bodyExtraText(m?.body_extra);
    fConfirm = '';
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
    const extra = parseBodyExtra(fExtra, fProto);
    if ('error' in extra) throw new Error(`body_extra: ${extra.error}`);
    // запись — по id (`v:<id>`), а не по имени: переименованная запись не подменится другой с тем же
    // именем (контракт задачи 2 после ревью r1–r2); у Anthropic предел — всегда max_tokens
    const token_field = fProto === 'anthropic' ? 'auto' : fField;
    // свой новый адрес записи подключения — подтверждён тем, кто его ввёл (решение 167)
    const confirm = fConfirm && fConfirm === chosenAddr ? { confirm_url: fConfirm } : {};
    return { protocol: fProto, vault: `v:${fVault}`, model, auth: fAuth, max_tokens: max, token_field, body_extra: extra.value, ...confirm };
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
    fConfirm = '';
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

  /** Итог проверки подключения — строкой; неудача — понятная причина и полный адрес запроса
   * (решение 169). */
  function testText(r: Awaited<ReturnType<typeof testModel>>): string {
    if (r.ok) return `Подключение работает: ответ за ${r.ms ?? '?'} мс.`;
    return testFailText(r, settings?.model?.model ?? fModel.trim());
  }

  // ---- запись Vault из окна подключения (решение 167): поверх окна подключения ----

  /** Окно записи Vault: создание (`item: null`) или правка; `url` — исправленный адрес. */
  let vform = $state<{ item: VaultItem | null; url?: string } | null>(null);

  /** «Исправить адрес» (решение 168): окно записи с исправленным адресом, значение не трогается. */
  function fixAddress() {
    const w = pathWarn;
    if (chosen && w) vform = { item: chosen, url: w.fixed };
  }

  /** Запись сохранена: список — заново, запись выбрана; без ключа — «без ключа». */
  async function vaultSaved(it: VaultItem, keyless: boolean) {
    const was = vform?.item ? vaultAddress(vform.item.data) : null;
    vform = null;
    await loadVault();
    fVault = it.id;
    if (keyless) fAuth = 'none';
    dlgError = '';
    const now = vaultAddress(it.data);
    const own = settings?.model?.vault === `v:${it.id}`;
    // адрес записи сохранённого подключения поменял сам человек: подтвердится при сохранении
    if (own && was !== null && was !== now) fConfirm = now;
    dlgNotice = `Запись «${it.title}» сохранена и выбрана.${fConfirm ? ' Новый адрес подключения подтвердится при сохранении.' : ''}`;
  }

  /** «Проверить подключение»: на паузе `vault_changed` — 409 без обращения (сервер), страница
   * показывает «был → стал». */
  async function runTest(): Promise<{ text: string; ok: boolean }> {
    try {
      const r = await testModel(target);
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
      r = await runWorks(k, target);
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
  /** Какой-нибудь вид работает в режиме «модель» сейчас (действующий режим) — данные уходят. */
  let modelActive = $derived(Object.values(settings?.effective ?? {}).some((e) => e.mode === 'model'));
  /** Режим «модель» выбран человеком (общий или у вида) — на паузе действующий режим «выкл», но
   * данные уйдут, как только пауза снимется (ревью r2 задачи 8, N1). */
  let modelChosen = $derived(
    settings?.mode === 'model' || Object.values(settings?.kinds ?? {}).some((m) => m === 'model'),
  );
  /** Почему выбранная «модель» сейчас не действует (не пауза): причина первого вида. */
  let modelOffReason = $derived(
    Object.values(settings?.effective ?? {}).find((e) => e.mode === 'off' && e.reason && e.reason !== 'paused')?.reason ?? null,
  );
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
  <h2 id="works-h">Внутренние работы{#if target}: {targetName}{/if}</h2>
  {#if target}
    <p class="viewing" data-testid="works-viewing">
      Страница пользователя <b>{targetName}</b> глазами администратора: настройки, очередь и журнал — его; правки
      записываются в аудит от имени администратора. <a href="#/works">Свои внутренние работы</a>
    </p>
  {/if}
  <p class="muted small">
    Работы памяти над собственными записями: пробные вопросы ночной проверки, кандидаты в Rules и уборка памяти. Знания
    по их итогу не меняются — пробы идут в проверку, кандидатов в Rules и предложения уборки принимает человек
    («Предложения»).
  </p>
  {#if error}<p class="error" role="alert">Не удалось загрузить: {error}</p>{/if}

  {#if external}
    <div class="alert ext" role="note" aria-label="Данные вне локальной сети" data-testid="works-external">
      <!-- текст — по выбранному режиму и паузе: действующий режим на паузе «выкл» (ревью r2, N1) -->
      {#if modelActive}
        <p class="alert-title">Данные памяти уходят за пределы локальной сети: {external}</p>
      {:else if modelChosen && pause?.class === 'vault_changed'}
        <p class="alert-title">
          Данные памяти уйдут за пределы локальной сети на <code>{pause.to ?? external}</code> сразу после «Подтвердить адрес»
        </p>
      {:else if modelChosen && pause}
        <p class="alert-title">Данные памяти уйдут за пределы локальной сети, как только пауза снимется: {external}</p>
      {:else if modelChosen && modelOffReason}
        <p class="alert-title">Подключённая модель — вне локальной сети: {external}</p>
        <p>Сейчас данные не уходят: {reasonText(modelOffReason)}.</p>
      {:else}
        <p class="alert-title">Подключённая модель — вне локальной сети: {external}</p>
        <p>Данные памяти уйдут за пределы локальной сети, когда вид работ будет в режиме «модель».</p>
      {/if}
      <p>Модели вне локальной сети уходит маскированный пакет работ (без Human, Vault и записей участников).</p>
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

{#if admin}
  <!-- блок администратора (M7.4 §7.7 п. 2): сводка всех пользователей, имя — вид страницы -->
  <WorksAdmin current={target ?? me.user_id} self={me.user_id} {onexpired} />
{/if}

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
        <div><dt>Протокол</dt><dd>{PROTOCOL_TITLE[protocolOf(m.protocol)]}</dd></div>
        <div><dt>Запись Vault</dt><dd title={m.vault}>{modelVault?.title ?? 'запись личного Vault'}</dd></div>
        <div><dt>Адрес</dt><dd><code class="path">{m.pinned ? pinnedUrl(m.pinned) : 'не закреплён'}</code></dd></div>
        <div><dt>Модель</dt><dd>{m.model}</dd></div>
        <div><dt>Ключ</dt><dd>{m.auth === 'key' ? 'значение записи Vault' : 'без ключа'}</dd></div>
        <div><dt>Предел ответа</dt><dd>{num(m.max_tokens)} токенов{m.token_field !== 'auto' && m.protocol !== 'anthropic' ? ` (${m.token_field})` : ''}</dd></div>
        <div><dt>body_extra</dt><dd>{Object.keys(m.body_extra).length ? Object.keys(m.body_extra).join(', ') : 'нет'}</dd></div>
      </dl>
    {:else}
      <p class="muted">
        Модель не подключена. Протокол — OpenAI-совместимый Chat Completions (OpenAI, Qwen, Kimi, DeepSeek, OpenRouter,
        Ollama, llama.cpp, vLLM, LM Studio) или Anthropic Messages (Anthropic и локальные серверы с таким входом): адрес
        API и ключ доступа — запись вашего личного Vault.
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
      <!-- сервер ночную проверку не проводит (`[works] check = off` или `enabled = false`):
           выключатель неактивен с причиной — включённая галочка не обещает проверку, которой не
           будет (финальное ревью M7.3, Minor 12) -->
      {@const serverOff = u.check?.server === 'off'}
      <label class="opt">
        <input
          type="checkbox"
          checked={settings.check === 'on'}
          disabled={serverOff}
          aria-describedby={serverOff ? 'works-check-off' : undefined}
          onchange={(e) => setCheck(e.currentTarget.checked)}
        />
        {target ? `Ночная проверка памяти пользователя ${targetName}` : 'Ночная проверка моей памяти'}
      </label>
      {#if serverOff}
        <p class="muted small" id="works-check-off" data-testid="works-check-off">{checkOffText(u.check.reason)}</p>
      {/if}
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
          <div class="small">токены {callTokensText(c)} · {callDurationText(c.ms)}{c.items != null ? ` · записей ${c.items}` : ''}</div>
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
              <!-- длительности нет — ответа не было: «—» с подсказкой, а не «0» (финальное ревью M7.3, Minor 2) -->
              <td class="num" data-testid="call-ms" title={c.ms == null ? CALL_NO_MS_HINT : undefined}>{callMsText(c.ms)}</td>
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
  description="Адрес API модели и ключ доступа — запись {target
    ? `личного Vault пользователя ${targetName}`
    : 'вашего личного Vault'}; ключ в окне не показывается."
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
  <label>
    Протокол
    <select bind:value={fProto}>
      {#each PROTOCOLS as p (p)}<option value={p}>{PROTOCOL_TITLE[p]}</option>{/each}
    </select>
  </label>
  {#if choices.length}
    <!-- что выбирать — строкой над выбором (решение 166); адрес выбранной записи, его класс и куда
         уйдёт запрос — подписью (видно до сохранения; описание поля для чтения с экрана —
         `aria-describedby`, в имя поля не входит): поля окна идут плотно -->
    <label>
      Запись Vault
      <span class="field-note muted" id="works-vault-note" aria-hidden="true">
        Запись вашего Vault, где хранятся адрес API модели и ключ доступа. Выберите сервер модели или создайте запись
      </span>
      <span class="field-note" aria-hidden="true" id="works-addr" data-testid="works-address">
        адрес: <code class="path">{chosenAddr || 'нет адреса'}</code>{#if view}<span class="muted">{` — ${reachText(view.reach)}`}</span>{/if}
        {#if view?.problem}<span class="error-text">{view.problem}</span>{/if}
        {#if view?.reach === 'external' && !view.problem}<span class="warn note-ext">Данные памяти уйдут за пределы локальной сети: {view.host}</span>{/if}
        {#if reqUrl}<span class="muted" data-testid="works-request-url">запрос уйдёт на <code class="path">{reqUrl}</code></span>{/if}
      </span>
      <select bind:value={fVault} data-autofocus aria-describedby="works-vault-note works-addr">
        {#each choices as v (v.id)}<option value={v.id}>{vaultChoiceLabel(v.title, vaultAddress(v.data))}</option>{/each}
      </select>
    </label>
  {:else}
    <p class="warn small" role="note">
      {vaultLocked ? `${vaultLocked}. ` : ''}В вашем Vault пока нет записи с адресом API модели. Создайте её здесь: название, адрес
      API и ключ доступа.
    </p>
  {/if}
  <!-- действия с записью — группой под выбором: предупреждение о пути адреса и кнопки окна записи
       Vault (поля окна идут плотно — группа считается полем) -->
  <fieldset class="vault-actions" aria-label="Создать или изменить запись">
    {#if choices.length && pathWarn}
      <div class="warn small row path-warn" role="note" data-testid="works-path-warning">
        <span>{pathWarn.text}</span>
        <button type="button" class="small-btn" onclick={() => fixAddress()}>Исправить адрес</button>
      </div>
    {/if}
    <div class="row">
      <button type="button" class="small-btn" onclick={() => (vform = { item: null })} disabled={!!vaultLocked}>Новая запись…</button>
      {#if chosen}
        <button type="button" class="small-btn" onclick={() => (vform = { item: chosen })} disabled={!!vaultLocked}>Изменить запись</button>
      {/if}
    </div>
  </fieldset>
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
  <!-- у Anthropic Messages предел — всегда max_tokens: поля нет (§9.2 п. 6) -->
  {#if fProto === 'openai'}
    <label>
      Поле предела
      <!-- пояснение — только описание поля (`aria-describedby`), не часть его имени (ревью r2, N2) -->
      <span class="field-note muted" id="works-field-note" aria-hidden="true">авто — max_completion_tokens у api.openai.com, max_tokens у прочих</span>
      <select bind:value={fField} aria-describedby="works-field-note">
        <option value="auto">авто</option>
        <option value="max_tokens">max_tokens</option>
        <option value="max_completion_tokens">max_completion_tokens</option>
      </select>
    </label>
  {/if}
  <label>
    body_extra (JSON, необязательно)
    <textarea rows="4" bind:value={fExtra} aria-invalid={!!extraError} spellcheck="false"
      placeholder={fProto === 'anthropic' ? '{"output_config": {"effort": "low"}}' : '{"chat_template_kwargs": {"enable_thinking": false}}'}></textarea>
  </label>
  {#if extraError}<p class="error small" role="alert">{extraError}</p>{/if}
  <div class="row recipes" role="group" aria-label="Рецепты body_extra">
    {#each recipesOf(fProto) as r (r.name)}<button type="button" class="small-btn" onclick={() => addRecipe(r.value)}>{r.title}</button>{/each}
  </div>
  {#snippet extra()}
    <!-- без сохранённого подключения проверять нечего: сначала сохранить (сервер проверяет сохранённое) -->
    <button type="button" disabled={dlgTesting || !choices.length} onclick={testInDialog}>
      {connOpen && (!settings?.model || isDirty(fieldsNow(), base)) ? 'Сохранить и проверить' : 'Проверить подключение'}
    </button>
  {/snippet}
</FormDialog>

{#if vform}
  <!-- окно записи Vault поверх окна подключения (решение 167): создать, изменить, исправить адрес -->
  {#await import('../components/VaultForm.svelte') then m}
    <m.default
      open
      user={target ?? me.user_id}
      space="knowledge"
      item={vform.item}
      model={fProto}
      url={vform.url}
      onsaved={vaultSaved}
      onclose={() => (vform = null)}
      onexpired={() => expired(new ApiError(401, 'сессия истекла'))}
    />
  {:catch}
    <p class="error" role="alert">Форма записи не загрузилась — обновите страницу.</p>
  {/await}
{/if}

<!-- фокус по умолчанию — на «Отмена» (`cautious`, как у опасных подтверждений, APG alertdialog):
     Enter не отправляет ключ и пакеты на новый адрес без сверки «был / стал» (финальное ревью M7.3,
     Minor 10) -->
<FormDialog
  open={confirmOpen}
  confirm
  cautious
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
  /* страница чужого пользователя (администратор): чья она — заметно, ссылка на свою */
  .viewing {
    margin: 8px 0;
    padding: 8px 10px;
    border-left: 4px solid var(--accent);
    border-radius: 4px;
    background: var(--selected);
    overflow-wrap: anywhere;
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
  .vault-actions {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
</style>
