<script lang="ts">
  // Insights (§7.5): таблица с фильтрами (вид, статус, надёжность, область, теги) и карточка
  // записи — замещение, отзыв, пересмотр, смена статуса надёжности (причина и доказательство
  // theorem человеку необязательны), триггеры, цепочка замещения, связи. Сессия WebUI — человек
  // (author_kind = human). Правка идёт с ревизией открытой записи (If-Match, §11.3): запись
  // изменилась — 409, карточка перечитывается. Столбец «показано / открыто / полезно / бесполезно»
  // и кнопки оценки (`POST /entries/{id}/feedback`, источник `webui`, §10.5); «открыто» — сессий
  // агента, открывших запись после её подсказки (M7.3 §6.6). Фильтр «Шумные» (M7.3 §6.6 п. 3,
  // решение 68) — на сервере, лента `noisy=1`: показано ≥ 10 и ни одного открытия за окно
  // `max(now − 30 сут, opened_since)`, «полезно» нет за всё время; первые 30 суток учёта открытий —
  // строка «данные об открытии копятся с <дата>».
  // Узкие экраны (M7.2, спецификация §5.2 п. 3): таблица и карточка — `JournalSplit` (рядом или
  // карточка вместо таблицы); в области журнала уже 720 px таблица остаётся таблицей (те же строки
  // и ячейки), столбцы «Вид», «Область», «Показы», «Изменено» скрыты, их значения — строкой под
  // заголовком. Условие — ширина области журнала, а не колонки списка рядом с карточкой.
  // Корзина (M7.2, спецификация §5.9 пп. 2–3): «Удалить» карточки — в корзину, с подтверждением
  // (отзыв оставляет запись в истории, удаление убирает её из журнала и поиска); «Корзина» — вместо
  // таблицы удалённые записи с «Восстановить» (`JournalTrash`, ленивый чанк).
  // Карточка и формы (M7.3, спецификация §6.14, решения 80–87): ряд действий — сразу под шапкой
  // карточки, до текста, полей ADR, файлов, цепочки и связей (правило Г); «Заменить», «Отозвать»,
  // «Сменить статус надёжности», «Триггеры» и «Поля ADR» — окна формы (`FormDialog`, правило Б):
  // несохранённое спрашивает окно (поля против их значений при открытии), уход по адресу — один
  // вопрос стека проверок ухода; 409 — запись перечитана, набранное остаётся, нетронутое поле — из
  // свежей записи (`rebase`, `formDraft.ts`), расхождения помечены, окно открыто. «Новый insight» —
  // окно `InsightNew` (ленивый чанк): кандидаты-дубли сервера — ссылками, «Записать всё равно»;
  // после записи — карточка созданной. «Полезно» / «Бесполезно» и «Пересмотреть» — сразу (Д).
  import { onMount, tick, untrack } from 'svelte';
  import CertaintyBadge from './CertaintyBadge.svelte';
  import FileRefs from './FileRefs.svelte';
  import FormDialog from './FormDialog.svelte';
  import JournalSplit from './JournalSplit.svelte';
  import Links from './Links.svelte';
  import Notice from './Notice.svelte';
  import Prompt, { type Ask } from './Prompt.svelte';
  import TrashPanel from './TrashPanel.svelte';
  import { trashAsk } from '../lib/entryTrash';
  import {
    ApiError,
    entryFeedback,
    errorText,
    getEntry,
    isStaleEntry,
    listEntries,
    patchEntry,
    retractEntry,
    supersedeEntry,
    type Certainty,
    type Entry,
    type EntryStatus,
    type NewTrigger,
    type TriggerType,
    type UsageCounts,
  } from '../lib/api';
  import { loadRefs, staleOf, staleText, type RefsByTarget } from '../lib/fileRefs';
  import {
    ADR_TEXT,
    AUTHOR_TEXT,
    CERTAINTIES,
    CERTAINTY_TEXT,
    DECIDED_BY_TEXT,
    INSIGHT_KINDS,
    JOURNAL_TITLE,
    STATUSES,
    STATUS_TEXT,
    TRIGGER_TEXT,
    TRIGGER_TYPES,
    adrIsEmpty,
    adrOf,
    adrToData,
    dataStr,
    entryRef,
    fmtTime,
    type Adr,
  } from '../lib/journals';
  import { isDirty, rebase, rebaseText } from '../lib/formDraft';
  import { BP_TABLET } from '../lib/layout';
  import { go, href } from '../lib/router';
  import { observeWidth } from '../lib/viewport.svelte';
  import { localTime } from '../lib/time';
  import { supersedeData } from '../lib/adrData';

  interface Props {
    user: string;
    space: string;
    /** Открытая запись. */
    entry: string | null;
    onexpired: () => void;
  }

  let { user, space, entry, onexpired }: Props = $props();

  const PAGE = 100;
  /** Предел шагов цепочки замещения в каждую сторону. */
  const CHAIN_MAX = 20;

  // ---- таблица ----
  let items = $state<Entry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(false);
  let error = $state('');
  let fKind = $state('');
  let fStatus = $state<'' | EntryStatus>('');
  let fCertainty = $state<'' | Certainty>('');
  let fArea = $state('');
  let fTag = $state('');
  /** Только решения человека (`decided_by=human`, M4.5 §5). */
  let fHuman = $state(false);
  /** «Шумные» (`noisy=1`, M7.3 §6.6 п. 3). */
  let fNoisy = $state(false);
  /** Начало учёта открытий (`opened_since` ленты `noisy=1`); `null` — не известно. */
  let openedSince = $state<string | null>(null);
  let refs = $state<RefsByTarget>(new Map());
  let seq = 0;
  /** Ширина области журнала (CSS px); уже 720 px — компактная таблица (§5.2 п. 3). */
  let width = $state(0);
  let compact = $derived(width > 0 && width < BP_TABLET);

  // ---- карточка ----
  let cur = $state<Entry | null>(null);
  let curError = $state('');
  let chain = $state<Entry[]>([]);
  /** Цепочка оборвалась (запись недоступна или предел шагов): до и после. */
  let chainCut = $state({ before: false, after: false });
  type Mode = 'none' | 'supersede' | 'retract' | 'certainty' | 'triggers' | 'adr';
  let mode = $state<Mode>('none');
  let busy = $state(false);
  let actionError = $state('');
  let notice = $state('');
  let eseq = 0;
  /** «Корзина»: вместо таблицы — удалённые записи. */
  let trash = $state(false);
  /** Итог действия, закрывшего карточку (удаление), — над таблицей. */
  let listNotice = $state('');
  let ask = $state<Ask | null>(null);
  /** Окно «Новый insight» (ленивый чанк `InsightNew`). */
  let newOpen = $state(false);
  /** Ошибка открытого окна формы (сервер, проверка, 409). */
  let dlgError = $state('');
  /** Поля, изменённые и в окне, и в другом месте (409): помечены в окне. */
  let clash = $state<string[]>([]);

  // формы
  let sTitle = $state('');
  let sBody = $state('');
  let sKind = $state('discovery');
  // новая запись человека по умолчанию без статуса: аксиому человек ставит явно (§7.5)
  let sCertainty = $state<Certainty | ''>('');
  let rReason = $state('');
  /** Поля ADR формы (`data.decision`); варианты — по одному в строке. */
  let adr = $state<Adr>(emptyAdr());
  let adrOptions = $state('');
  let cTo = $state<'' | Certainty>('');
  let cReason = $state('');
  let evChecked = $state('');
  let evMethod = $state('');
  let evResult = $state('');
  let tDraft = $state<NewTrigger[]>([]);
  let tType = $state<TriggerType>('keyword');
  let tPattern = $state('');
  let tTools = $state('');

  /** Поля формы окна — простые значения и структуры (сравнение `formDraft.ts`). */
  type Fields = Record<string, unknown>;

  /** Поля открытой формы `m` (без выбора типа нового триггера — он не уходит при сохранении). */
  function fieldsNow(m: Mode): Fields {
    switch (m) {
      case 'supersede':
        return { title: sTitle, body: sBody, kind: sKind, certainty: sCertainty, adr: { ...adr }, options: adrOptions };
      case 'adr':
        return { adr: { ...adr }, options: adrOptions };
      case 'retract':
        return { reason: rReason };
      case 'certainty':
        return { to: cTo, reason: cReason, checked: evChecked, method: evMethod, result: evResult };
      case 'triggers':
        return { triggers: tDraft.map((t) => ({ ...t })), pattern: tPattern, tools: tTools };
      default:
        return {};
    }
  }

  /** ADR формы из `data.decision` записи; у нового решения «кто решил» — человек. */
  function adrForm(data: Record<string, unknown>): Adr {
    const a = adrOf(data);
    return { ...a, decided_by: a.decided_by || 'human' };
  }

  /** Поля формы `m` из записи `e`: при открытии окна и из свежей записи при 409. Новая запись
   * замены — без статуса надёжности (аксиому человек ставит явно, §7.5); причины — пустые. */
  function fieldsOf(m: Mode, e: Entry): Fields {
    const ev = evidenceOf(e);
    const evStr = (k: string) => (typeof ev?.[k] === 'string' ? (ev[k] as string) : '');
    switch (m) {
      case 'supersede':
        return {
          title: e.title,
          body: e.body,
          kind: dataStr(e.data, 'kind') || 'discovery',
          certainty: '',
          adr: adrForm(e.data),
          options: adrOf(e.data).options.join('\n'),
        };
      case 'adr':
        return { adr: adrForm(e.data), options: adrOf(e.data).options.join('\n') };
      case 'retract':
        return { reason: '' };
      case 'certainty':
        return { to: e.certainty ?? '', reason: '', checked: evStr('checked'), method: evStr('method'), result: evStr('result') };
      case 'triggers':
        return { triggers: (e.triggers ?? []).map((t) => ({ type: t.type, pattern: t.pattern, tools: t.tools })), pattern: '', tools: '' };
      default:
        return {};
    }
  }

  /** Поля `f` — в форму `m`. */
  function setFields(m: Mode, f: Fields) {
    const str = (k: string) => (typeof f[k] === 'string' ? (f[k] as string) : '');
    if (m === 'supersede' || m === 'adr') {
      adr = { ...(f.adr as Adr) };
      adrOptions = str('options');
    }
    if (m === 'supersede') {
      sTitle = str('title');
      sBody = str('body');
      sKind = str('kind');
      sCertainty = str('certainty') as Certainty | '';
    } else if (m === 'retract') rReason = str('reason');
    else if (m === 'certainty') {
      cTo = str('to') as Certainty | '';
      cReason = str('reason');
      evChecked = str('checked');
      evMethod = str('method');
      evResult = str('result');
    } else if (m === 'triggers') {
      tDraft = (f.triggers as NewTrigger[]).map((t) => ({ ...t }));
      tPattern = str('pattern');
      tTools = str('tools');
    }
  }

  /** Поля формы при открытии (или из свежей записи после 409) — с ними сравнивается набранное. */
  let base: Fields = {};

  /** Подписи полей в тексте 409 (`clash`). */
  const FIELD_LABEL: Record<string, string> = {
    title: 'заголовок',
    body: 'текст',
    kind: 'вид',
    certainty: 'надёжность',
    adr: 'поля ADR',
    options: 'варианты',
    reason: 'причина',
    to: 'статус надёжности',
    checked: 'проверено',
    method: 'метод',
    result: 'результат',
    triggers: 'триггеры',
    pattern: 'шаблон',
    tools: 'инструменты',
  };

  /** Открыть окно формы `m`: поля — из записи карточки. */
  function openForm(m: Exclude<Mode, 'none'>) {
    const c = cur;
    if (!c) return;
    base = fieldsOf(m, c);
    setFields(m, base);
    if (m === 'triggers') tType = 'keyword';
    dlgError = '';
    clash = [];
    actionError = '';
    mode = m;
  }

  /** В открытом окне есть несохранённое (вопрос окна и проверка ухода). */
  const unsaved = (): boolean => mode !== 'none' && cur !== null && isDirty(fieldsNow(mode), base);

  /** Окно формы закрыли (кнопкой, Esc, фоном; несохранённое окно уже спросило). */
  function closeForm() {
    mode = 'none';
    dlgError = '';
    clash = [];
  }

  /** «← Insights»: окно формы закрывается вместе с карточкой. */
  function closeDetail() {
    closeForm();
    actionError = '';
  }

  function emptyAdr(): Adr {
    return { context: '', options: [], chosen: '', rationale: '', consequences: '', decided_by: 'human', revisit_if: '' };
  }

  /** `data.decision` формы; пустой ADR (одно лишь «кто решил») не отправляется. */
  function adrPayload(): Record<string, unknown> | null {
    const a = { ...adr, options: adrOptions.split('\n') };
    return adrIsEmpty({ ...a, decided_by: '' }) ? null : adrToData(a);
  }

  function fail(e: unknown, set: (m: string) => void) {
    if (e instanceof ApiError && e.status === 401) return onexpired();
    set(errorText(e));
  }

  // Таблица — заново только при смене пользователя или пространства: свойства приходят из объекта
  // маршрута, и эффект перезапускается на каждый переход (открытие записи) — фильтры, загруженные
  // страницы и прокрутка при этом целы (§5.1 п. 5); неудавшаяся загрузка повторяется при следующем
  // переходе с прежними фильтрами.
  let listOf = '';
  /** Первая страница таблицы не загрузилась: следующий переход повторит загрузку. */
  let failed = false;
  $effect(() => {
    const u = user;
    const s = space;
    untrack(() => {
      if (listOf === `${u}|${s}`) {
        if (failed) load(u, s, true);
        return;
      }
      listOf = `${u}|${s}`;
      fKind = '';
      fStatus = '';
      fCertainty = '';
      fArea = '';
      fTag = '';
      fHuman = false;
      fNoisy = false;
      openedSince = null;
      trash = false;
      listNotice = '';
      load(u, s, true);
      reloadRefs();
    });
  });

  /** Ссылки на файлы записей («возможно устарело»): при входе в журнал и после правки (новая
   * запись замены приходит со своими ссылками). */
  function reloadRefs() {
    const u = user;
    const s = space;
    loadRefs(u, s).then((r) => {
      if (u === user && s === space) refs = r;
    });
  }

  $effect(() => {
    const id = entry;
    const u = user;
    untrack(() => open(u, id));
  });

  async function load(u: string, s: string, reset: boolean) {
    const my = ++seq;
    loading = true;
    error = '';
    if (reset) failed = false;
    try {
      const r = await listEntries(u, s, {
        kind: 'insight',
        status: fStatus ? [fStatus] : undefined,
        certainty: fCertainty ? [fCertainty] : undefined,
        area: fArea.trim() || undefined,
        tag: fTag.trim() || undefined,
        dataKind: fKind || (fHuman ? 'decision' : undefined),
        decidedBy: fHuman ? 'human' : undefined,
        noisy: fNoisy || undefined,
        cursor: reset ? null : next,
        limit: PAGE,
      });
      if (my !== seq) return;
      if (fNoisy) openedSince = r.opened_since ?? null;
      items = reset ? r.items : [...items, ...r.items];
      next = r.next_cursor;
    } catch (e) {
      if (my !== seq) return;
      if (reset) {
        items = [];
        failed = true;
      }
      fail(e, (m) => (error = m));
    } finally {
      if (my === seq) loading = false;
    }
  }

  const reloadList = () => load(user, space, true);

  function applyText(e: SubmitEvent) {
    e.preventDefault();
    reloadList();
  }


  async function open(u: string, id: string | null) {
    const my = ++eseq;
    mode = 'none';
    // другая запись (переход по адресу уже спросил о набранном): окно «Новый insight» закрывается
    if (id !== (cur?.id ?? null)) newOpen = false;
    actionError = '';
    curError = '';
    chain = [];
    chainCut = { before: false, after: false };
    if (!id) {
      cur = null;
      return;
    }
    try {
      const e = await getEntry(u, id);
      if (my !== eseq) return;
      if (e.kind !== 'insight') {
        cur = null;
        curError = 'Это не insight.';
        return;
      }
      cur = e;
      const c = await chainOf(u, e);
      if (my !== eseq) return;
      chain = c.items;
      chainCut = c.cut;
    } catch (e) {
      if (my !== eseq) return;
      cur = null;
      fail(e, (m) => (curError = m));
    }
  }

  /** Цепочка замещения: от самой ранней записи к самой поздней (сама запись — внутри). */
  async function chainOf(
    u: string,
    e: Entry,
  ): Promise<{ items: Entry[]; cut: { before: boolean; after: boolean } }> {
    if (!e.supersedes_id && !e.superseded_by_id) return { items: [], cut: { before: false, after: false } };
    /** Шаги в одну сторону; `true` вторым значением — дальше есть, но недоступно. */
    async function walk(start: string | null, next: (x: Entry) => string | null): Promise<[Entry[], boolean]> {
      const out: Entry[] = [];
      let id = start;
      for (let i = 0; id; i++) {
        if (i >= CHAIN_MAX) return [out, true];
        try {
          const x = await getEntry(u, id);
          out.push(x);
          id = next(x);
        } catch {
          return [out, true];
        }
      }
      return [out, false];
    }
    const [before, cb] = await walk(e.supersedes_id, (x) => x.supersedes_id);
    const [after, ca] = await walk(e.superseded_by_id, (x) => x.superseded_by_id);
    return { items: [...before.reverse(), e, ...after], cut: { before: cb, after: ca } };
  }

  const usageText = (u: UsageCounts | undefined) =>
    u ? `${u.shown} / ${u.opened ?? 0} / ${u.useful} / ${u.useless}` : '—';

  /** Учёт открытий моложе 30 суток: фильтр «Шумные» неполон (§6.6 п. 3). */
  const OPENED_YOUNG_MS = 30 * 86_400_000;
  let openedYoung = $derived(!!openedSince && Date.now() - new Date(openedSince).getTime() < OPENED_YOUNG_MS);

  /** Оценка полезности: событие `usage` в базе записи, счётчики — перечитать. */
  async function rate(value: 'useful' | 'useless') {
    const c = cur;
    if (!c || busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      await entryFeedback(user, c.id, value);
      const e = await getEntry(user, c.id);
      if (cur?.id === c.id) cur = e;
      items = items.map((x) => (x.id === e.id ? { ...x, usage: e.usage } : x));
      notice = value === 'useful' ? 'Отмечено: полезно.' : 'Отмечено: бесполезно.';
    } catch (e) {
      fail(e, (m) => (actionError = m));
    } finally {
      busy = false;
    }
  }

  /** «Показать ещё»: на время загрузки кнопка остаётся (`aria-disabled`), фокус на ней цел;
   * страниц больше нет и кнопка ушла — фокус на первую догруженную запись. */
  async function more(e: MouseEvent) {
    if (loading) return;
    const btn = e.currentTarget as HTMLElement;
    const n = items.length;
    await load(user, space, false);
    await tick();
    const a = document.activeElement;
    const first = items[n]?.id;
    if (btn.isConnected || (a && a !== document.body) || !first) return;
    document.querySelector<HTMLElement>(`tr[data-entry="${CSS.escape(first)}"] button`)?.focus();
  }

  /** Открыть запись: новая запись истории — «назад» браузера возвращает к таблице (§5.1 п. 5). */
  function select(id: string) {
    notice = '';
    listNotice = '';
    go({ name: 'journal', user, space, journal: 'insights', entry: id });
  }

  /** Карточка показана (рядом с таблицей или вместо неё). */
  let detailOpen = $derived(cur !== null || curError !== '');

  /** Действие над карточкой: ошибка — понятным текстом, успех — перечитать. */
  async function act(fn: () => Promise<string | void>, done: string) {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const target = (await fn()) ?? cur?.id ?? null;
      await reloadList();
      reloadRefs();
      if (target && target !== entry) {
        select(target);
      } else {
        await open(user, target);
      }
      // после select: смена записи сбрасывает уведомление, а это — о только что сделанном
      notice = done;
    } catch (e) {
      if (isStaleEntry(e) && cur) {
        // запись изменилась в другой сессии: перечитать карточку (новая ревизия), объяснить
        const msg = errorText(e);
        await open(user, cur.id);
        actionError = msg;
      } else {
        fail(e, (m) => (actionError = m));
      }
    } finally {
      busy = false;
    }
  }

  /** Отправка окна формы: успех — окно закрыто, таблица перечитана, карточка — та же или новая
   * запись (замена), итог строкой; ошибка — в окне; 409 — набранное перенесено на свежую запись
   * (`staleForm`), окно открыто. */
  async function submitForm(fn: () => Promise<string | void>, done: string) {
    dlgError = '';
    let target: string | null;
    try {
      target = (await fn()) ?? cur?.id ?? null;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return onexpired();
      if (isStaleEntry(e) && cur) return staleForm(cur.id);
      dlgError = errorText(e);
      return;
    }
    closeForm();
    const other = !!target && target !== entry;
    // к новой записи — сразу: на телефоне запись слоя окна становится её записью
    if (other) select(target!);
    notice = done;
    busy = true;
    try {
      await reloadList();
      reloadRefs();
      if (!other) await open(user, target);
    } finally {
      busy = false;
    }
  }

  /** 409 окна: запись перечитана, нетронутые поля — из неё, набранное остаётся, расхождения
   * помечены (`rebase`); окно открыто, карточка под ним — свежая ревизия. */
  async function staleForm(id: string) {
    const m = mode;
    let fresh: Entry;
    try {
      fresh = await getEntry(user, id);
    } catch (e) {
      dlgError = errorText(e);
      return;
    }
    if (m === 'none' || mode !== m) return;
    const next = fieldsOf(m, fresh);
    const r = rebase(fieldsNow(m), base, next);
    setFields(m, r.fields);
    base = next;
    cur = fresh;
    clash = r.clash.map(String);
    dlgError = rebaseText(clash.map((k) => FIELD_LABEL[k] ?? k), fresh.rev);
  }

  /** Поле — расхождение 409: помечено. */
  const clashed = (k: string) => clash.includes(k) || undefined;

  function submitAdr() {
    const c = cur;
    if (!c) return;
    const decision = adrPayload();
    // `data` insight заменяется целиком: прочие поля сохраняются, ADR — как в форме
    const data: Record<string, unknown> = { ...c.data };
    if (decision) data.decision = decision;
    else delete data.decision;
    return submitForm(async () => {
      await patchEntry(user, c.id, c.rev, { data });
    }, 'Поля ADR сохранены.');
  }

  function submitSupersede() {
    const c = cur;
    if (!c) return;
    return submitForm(async () => {
      // у решения ключи ADR верхнего уровня прежней записи не копируются (422 сервера)
      const data = supersedeData(c.data, sKind, sKind === 'decision' ? adrPayload() : null);
      const r = await supersedeEntry(user, c.id, {
        title: sTitle.trim(),
        body: sBody,
        data,
        tags: c.tags,
        area: c.area,
        ...(sCertainty ? { certainty: sCertainty } : {}),
      }, c.rev);
      return r.entry.id;
    }, 'Запись заменена: прежняя помечена superseded.');
  }

  function submitRetract() {
    const c = cur;
    if (!c) return;
    const reason = rReason.trim();
    if (!reason) {
      dlgError = 'Укажите причину отзыва.';
      return;
    }
    return submitForm(async () => {
      await retractEntry(user, c.id, reason, c.rev);
    }, 'Запись отозвана.');
  }

  function toggleRevisit() {
    const c = cur;
    if (!c) return;
    const to = c.status === 'revisit' ? 'active' : 'revisit';
    act(
      async () => {
        await patchEntry(user, c.id, c.rev, { status: to });
      },
      to === 'revisit' ? 'Запись отправлена на пересмотр.' : 'Запись снова действует.',
    );
  }

  function submitCertainty() {
    const c = cur;
    if (!c) return;
    // человеку причина необязательна (§7.5)
    const reason = cReason.trim();
    const to = cTo === '' ? null : cTo;
    // человеку доказательство для theorem необязательно (§7.5, R8): заполнено — уходит в
    // data.evidence, пусто — не шлётся
    let data: Record<string, unknown> | undefined;
    if (to === 'theorem') {
      const ev = { checked: evChecked.trim(), method: evMethod.trim(), result: evResult.trim() };
      if (ev.checked || ev.method || ev.result) data = { ...c.data, evidence: ev };
    }
    return submitForm(async () => {
      await patchEntry(user, c.id, c.rev, {
        certainty: to,
        ...(reason ? { reason } : {}),
        ...(data ? { data } : {}),
      });
    }, 'Статус надёжности изменён.');
  }

  /** «Добавить» триггер в черновик окна (кнопка и Enter в поле шаблона — не отправка окна). */
  function addTrigger() {
    const pattern = tPattern.trim();
    if (!pattern) return;
    const tools = tTools
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    tDraft = [...tDraft, { type: tType, pattern, tools: tools.length ? tools : null }];
    tPattern = '';
    tTools = '';
  }

  function saveTriggers() {
    const c = cur;
    if (!c) return;
    const triggers = tDraft;
    return submitForm(async () => {
      await patchEntry(user, c.id, c.rev, { triggers });
    }, 'Триггеры сохранены.');
  }

  /** Окна форм карточки: заголовок, пояснение, кнопка, ширина и отправка. */
  const FORMS: Record<Exclude<Mode, 'none'>, { title: string; hint?: string; submit: string; wide?: boolean; danger?: boolean; send: () => unknown }> = {
    supersede: {
      title: 'Заменить запись',
      hint: 'Новая запись займёт место этой; прежняя останется в истории как superseded.',
      submit: 'Заменить',
      wide: true,
      send: () => submitSupersede(),
    },
    adr: {
      title: 'Поля ADR',
      hint: 'Контекст, варианты, выбор, обоснование, последствия: запись правится на месте, новой ревизией.',
      submit: 'Сохранить',
      wide: true,
      send: () => submitAdr(),
    },
    retract: {
      title: 'Отозвать запись',
      hint: 'Отзыв ничего не удаляет: запись остаётся в истории со статусом retracted.',
      submit: 'Отозвать',
      danger: true,
      send: () => submitRetract(),
    },
    certainty: {
      title: 'Статус надёжности',
      hint: 'Вы действуете как человек: доступны все статусы, в том числе axiom; причина необязательна.',
      submit: 'Сохранить',
      send: () => submitCertainty(),
    },
    triggers: { title: 'Триггеры', submit: 'Сохранить триггеры', wide: true, send: () => saveTriggers() },
  };
  let form = $derived(mode === 'none' ? null : FORMS[mode]);

  /** Итог действия в карточке (`Notice`): фокус после закрытия окна. */
  const cardNotice = () => document.querySelector<HTMLElement>('article[aria-label="Карточка insight"] .card-notice [role="status"]');

  // чанк окна «Новый insight» — заранее, когда журнал открыт: первое нажатие открывает окно сразу
  onMount(() => {
    const t = setTimeout(() => void import('./InsightNew.svelte').catch(() => {}), 1500);
    return () => clearTimeout(t);
  });

  /** «Новый insight» записан: окно закрыто, карточка созданной — на виду и в фокусе (§6.14 п. 6). */
  async function created(e: Entry) {
    newOpen = false;
    listNotice = '';
    select(e.id);
    notice = 'Insight записан.';
    await reloadList();
    reloadRefs();
  }

  /** «Удалить» карточки: в корзину; карточка закрывается, итог — над таблицей. */
  function askDelete(c: Entry) {
    actionError = '';
    notice = '';
    ask = trashAsk(user, c, {
      deleted: async (msg) => {
        // карточка закрывается сразу: иначе её «Связи» перечитались бы для удалённой записи (404),
        // пока смена адреса и перечитывание оболочки ещё идут; форма карточки — вместе с ней
        mode = 'none';
        cur = null;
        go({ name: 'journal', user, space, journal: 'insights', entry: null }, { replace: true });
        await reloadList();
        listNotice = msg;
      },
      stale: async (msg) => {
        await open(user, c.id);
        actionError = msg;
      },
      expired: onexpired,
      focus: () => listEl?.querySelector('[role="status"]'),
    });
  }

  function toggleTrash() {
    trash = !trash;
    listNotice = '';
    newOpen = false;
    // назад к таблице: восстановленные из корзины — в ней
    if (!trash) reloadList();
  }

  /** Итог над таблицей (`Notice`): фокус после удаления — сюда, кнопки карточки уже нет. */
  let listEl = $state<HTMLElement | null>(null);

  const live = (e: Entry) => e.status === 'active' || e.status === 'revisit';
  const evidenceOf = (e: Entry): Record<string, unknown> | null => {
    const ev = e.data.evidence;
    return ev && typeof ev === 'object' ? (ev as Record<string, unknown>) : null;
  };
</script>

<section class="journal insights" use:observeWidth={(w) => (width = w)}>
  <!-- таблица и карточка — рядом в широкой области, карточка вместо таблицы в узкой (§5.2 п. 3) -->
  <JournalSplit
    open={detailOpen}
    back={{ href: href({ name: 'journal', user, space, journal: 'insights', entry: null }), label: JOURNAL_TITLE.insights }}
    onclose={closeDetail}
    {entry}
  >
    {#snippet list()}
      {@render table()}
    {/snippet}
    {#snippet detail()}
      {@render insightDetail()}
    {/snippet}
  </JournalSplit>
</section>

<Prompt bind:ask />

{#if newOpen}
  <!-- «Новый insight» — окно (§6.14 п. 7), ленивый чанк (загружен заранее, `onMount`); пока чанк
       идёт и если не пришёл — окно с тем же заголовком: «Загрузка…» или ошибка и «Обновить страницу» -->
  {#await import('./InsightNew.svelte')}
    <!-- заглушка — того же вида, что окно (на телефоне сразу во весь экран, без скачка) -->
    <FormDialog open title="Новый insight" submit="Записать" busy onsubmit={() => {}} onclose={() => (newOpen = false)}>
      <p class="muted" aria-busy="true">Загрузка…</p>
    </FormDialog>
  {:then { default: InsightNew }}
    <InsightNew {user} {space} oncreated={created} onclose={() => (newOpen = false)} {onexpired} />
  {:catch e}
    <FormDialog
      open
      confirm
      title="Новый insight"
      submit="Обновить страницу"
      onsubmit={() => location.reload()}
      onclose={() => (newOpen = false)}
    >
      <p class="error" role="alert">Окно не загрузилось: {errorText(e)} — обновите страницу</p>
    </FormDialog>
  {/await}
{/if}

{#snippet table()}
  <!-- «Новый insight» и «Корзина» — одной полосой, как у Задач и Идей; «Корзина» — одна кнопка в
       обоих видах (фокус переживает переключение); фильтры — у живой таблицы -->
  <div class="row bar">
    {#if !trash}<button type="button" class="primary" onclick={() => (newOpen = true)}>Новый insight</button>{/if}
    <button type="button" class="toggle" aria-pressed={trash} onclick={toggleTrash}>Корзина</button>
  </div>
  {#if !trash}
    <form class="filters row" onsubmit={applyText} aria-label="Фильтры insights">
      {@render filters()}
    </form>
  {/if}
  {#if trash}
    <TrashPanel {user} {space} kind="insight" {onexpired} />
  {:else}
    {@render liveTable()}
  {/if}
{/snippet}

{#snippet filters()}
  <label class="row">
    Вид
    <select bind:value={fKind} aria-label="Вид insight" onchange={reloadList}>
      <option value="">все</option>
      {#each INSIGHT_KINDS as k (k)}<option value={k}>{k}</option>{/each}
    </select>
  </label>
  <label class="row">
    Статус
    <select bind:value={fStatus} aria-label="Статус" onchange={reloadList}>
      <option value="">все</option>
      {#each STATUSES as s (s)}<option value={s}>{STATUS_TEXT[s]}</option>{/each}
    </select>
  </label>
  <label class="row">
    Надёжность
    <select bind:value={fCertainty} aria-label="Надёжность" onchange={reloadList}>
      <option value="">любая</option>
      {#each CERTAINTIES as c (c)}<option value={c}>{c}</option>{/each}
    </select>
  </label>
  <input bind:value={fArea} placeholder="область" aria-label="Область" />
  <input bind:value={fTag} placeholder="тег" aria-label="Тег" />
  <label class="row">
    <input type="checkbox" bind:checked={fHuman} onchange={reloadList} aria-label="Решения человека" /> Решения человека
  </label>
  <label class="row" title="Показано подсказками не меньше 10 раз за 30 суток, ни одного открытия агентом и ни одной оценки «полезно»: кандидат отключить триггер или переписать запись">
    <input type="checkbox" bind:checked={fNoisy} onchange={reloadList} aria-label="Шумные" /> Шумные
  </label>
  <button type="submit">Применить</button>
{/snippet}

{#snippet liveTable()}
  <div bind:this={listEl}><Notice text={listNotice} /></div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if fNoisy}
    <p class="muted small noisy-note" data-testid="noisy-note">
      Шумные — показано подсказками не меньше 10 раз и ни разу не открыто агентом за 30 суток, «полезно» не отмечено ни
      разу.{#if openedYoung && openedSince}<span class="warn-inline">
          Данные об открытии копятся с {localTime(openedSince, { dateStyle: 'long' })}: первые 30 суток фильтр неполон.</span
        >{/if}
    </p>
  {/if}

  <div class="table-wrap">
    <table aria-label="Insights">
      <thead>
        <tr>
          <th>Надёжность</th><th>Заголовок</th>{#if !compact}<th>Вид</th>{/if}<th>Статус</th>
          {#if !compact}
            <th>Область</th>
            <th title="Показано подсказками / открыто агентом / полезно / бесполезно">Показано / открыто / полезно / бесполезно</th><th>Изменено</th>
          {/if}
        </tr>
      </thead>
      <tbody>
        {#each items as e (e.id)}
          <tr class:selected={e.id === entry} class="st-{e.status}" data-entry={e.id}>
            <td><CertaintyBadge value={e.certainty} /></td>
            <td>
              <button type="button" class="link" onclick={() => select(e.id)}>{e.title}</button>
              {#each e.tags as t (t)}<span class="chip">#{t}</span>{/each}
              {#if staleOf(refs.get(entryRef('insight', e.id))).length}
                <span class="chip stale" title={staleText(staleOf(refs.get(entryRef('insight', e.id))))}>возможно устарело</span>
              {/if}
              {#if compact}
                <!-- скрытые столбцы — строкой под заголовком -->
                <div class="muted small row-meta" data-testid="row-meta">
                  {[dataStr(e.data, 'kind'), e.area ?? '', `показано ${usageText(e.usage)}`, fmtTime(e.updated_at)].filter(Boolean).join(' · ')}
                </div>
              {/if}
            </td>
            {#if !compact}<td>{dataStr(e.data, 'kind')}</td>{/if}
            <td>{e.status ? STATUS_TEXT[e.status] : ''}</td>
            {#if !compact}
              <td>{e.area ?? ''}</td>
              <td class="usage">{usageText(e.usage)}</td>
              <td class="muted">{fmtTime(e.updated_at)}</td>
            {/if}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if !loading && items.length === 0 && !error}<p class="muted">{fNoisy ? 'Шумных записей нет.' : 'Записей нет.'}</p>{/if}
  {#if loading}<p class="muted">Загрузка…</p>{/if}
  {#if next}
    <button type="button" class="more" aria-disabled={loading} aria-busy={loading} onclick={more}>Показать ещё</button>
  {/if}
{/snippet}

{#snippet insightDetail()}
  {#if curError}<p class="error" role="alert">{curError}</p>{/if}
  {#if cur}
    {@const c = cur}
    <article class="card panel" aria-label="Карточка insight">
      <header>
        <h2>{c.title}</h2>
        <div class="row meta">
          <CertaintyBadge value={c.certainty} />
          {#if !c.certainty}<span class="muted">без статуса надёжности</span>{/if}
          <span class="chip status">{c.status ? STATUS_TEXT[c.status] : ''}</span>
          <span class="muted">{dataStr(c.data, 'kind')}</span>
          {#if c.area}<span class="chip">{c.area}</span>{/if}
          <span class="muted small">
            {AUTHOR_TEXT[c.author.kind] ?? c.author.kind} · ревизия {c.rev} · {fmtTime(c.updated_at)}
          </span>
        </div>
        {#if c.retract_reason}<p class="warn">Отозвано: {c.retract_reason}</p>{/if}
      </header>

      <!-- ряд действий и оценка — сразу под шапкой, до длинных разделов (правило Г) -->
      <div class="row actions" role="group" aria-label="Действия с записью">
        {#if live(c)}
          <button type="button" onclick={() => openForm('supersede')} disabled={busy}>Заменить</button>
          <button type="button" onclick={toggleRevisit} disabled={busy}>
            {c.status === 'revisit' ? 'Вернуть в действующие' : 'Пересмотреть'}
          </button>
          <button type="button" onclick={() => openForm('certainty')} disabled={busy}>Сменить статус надёжности</button>
          <button type="button" onclick={() => openForm('triggers')} disabled={busy}>Триггеры</button>
          {#if dataStr(c.data, 'kind') === 'decision'}
            <button type="button" onclick={() => openForm('adr')} disabled={busy}>Править ADR</button>
          {/if}
        {/if}
        {#if c.status !== 'retracted'}
          <button type="button" class="danger" onclick={() => openForm('retract')} disabled={busy}>Отозвать</button>
        {/if}
        <button type="button" class="danger" onclick={() => askDelete(c)} disabled={busy}>Удалить</button>
      </div>
      <div class="row rate" role="group" aria-label="Оценка полезности">
        <span class="muted small" data-testid="usage">
          показано {c.usage?.shown ?? 0} · открыто {c.usage?.opened ?? 0} · полезно {c.usage?.useful ?? 0} · бесполезно {c.usage?.useless ?? 0}
        </span>
        <button type="button" onclick={() => rate('useful')} disabled={busy}>Полезно</button>
        <button type="button" onclick={() => rate('useless')} disabled={busy}>Бесполезно</button>
      </div>
      <div class="card-notice"><Notice text={notice} /></div>
      {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

      {#if c.body}<p class="body">{c.body}</p>{/if}
      {#if evidenceOf(c)}
        {@const ev = evidenceOf(c)!}
        {#if c.certainty !== 'theorem'}<p class="muted small ev-title">Прежнее доказательство</p>{/if}
        <dl class="evidence" class:old={c.certainty !== 'theorem'}>
          <dt>Проверено</dt><dd>{String(ev.checked ?? '')}</dd>
          <dt>Метод</dt><dd>{String(ev.method ?? '')}</dd>
          <dt>Результат</dt><dd>{String(ev.result ?? '')}</dd>
        </dl>
      {/if}
      {#if dataStr(c.data, 'kind') === 'decision' && !adrIsEmpty({ ...adrOf(c.data), decided_by: '' })}
        {@const a = adrOf(c.data)}
        <dl class="adr" aria-label="Поля ADR">
          {#if a.context}<dt>{ADR_TEXT.context}</dt><dd>{a.context}</dd>{/if}
          {#if a.options.length}
            <dt>{ADR_TEXT.options}</dt>
            <dd><ul>{#each a.options as o (o)}<li>{o}</li>{/each}</ul></dd>
          {/if}
          {#if a.chosen}<dt>{ADR_TEXT.chosen}</dt><dd>{a.chosen}</dd>{/if}
          {#if a.rationale}<dt>{ADR_TEXT.rationale}</dt><dd>{a.rationale}</dd>{/if}
          {#if a.consequences}<dt>{ADR_TEXT.consequences}</dt><dd>{a.consequences}</dd>{/if}
          {#if a.decided_by}<dt>{ADR_TEXT.decided_by}</dt><dd>{DECIDED_BY_TEXT[a.decided_by]}</dd>{/if}
          {#if a.revisit_if}<dt>{ADR_TEXT.revisit_if}</dt><dd>{a.revisit_if}</dd>{/if}
        </dl>
      {/if}
      {#if staleOf(refs.get(entryRef('insight', c.id))).length}
        <p class="warn" data-testid="stale-note">{staleText(staleOf(refs.get(entryRef('insight', c.id))))}</p>
      {/if}
      <FileRefs refs={refs.get(entryRef('insight', c.id)) ?? []} />

      {#if chain.length > 1}
        <div class="chain">
          <h3>Цепочка замещения</h3>
          <ol>
            {#if chainCut.before}<li class="muted cut">…дальше недоступно</li>{/if}
            {#each chain as x (x.id)}
              <li class:here={x.id === c.id}>
                {#if x.id === c.id}
                  <strong>{x.title}</strong>
                {:else}
                  <button type="button" class="link" onclick={() => select(x.id)}>{x.title}</button>
                {/if}
                <span class="muted small">{x.status ? STATUS_TEXT[x.status] : ''}</span>
              </li>
            {/each}
            {#if chainCut.after}<li class="muted cut">…дальше недоступно</li>{/if}
          </ol>
        </div>
      {/if}

      <div class="links">
        <h3>Связи</h3>
        <Links {user} kind="entry" id={c.id} {onexpired} />
      </div>
    </article>
  {/if}
{/snippet}

<!-- формы карточки — окна (§6.14, правило Б): замена, поля ADR, отзыв, надёжность, триггеры -->
<FormDialog
  open={form !== null && cur !== null}
  title={form?.title ?? ''}
  subject={cur?.title}
  description={form?.hint}
  submit={form?.submit ?? ''}
  wide={form?.wide}
  danger={form?.danger}
  error={dlgError}
  dirty={unsaved}
  refocus={cardNotice}
  onsubmit={async () => {
    await form?.send();
  }}
  onclose={closeForm}
>
  {#if mode === 'supersede'}
    <label class:clash={clashed('title')}>Заголовок {@render clashNote('title')}<input bind:value={sTitle} required data-autofocus /></label>
    <label class:clash={clashed('body')}>Текст {@render clashNote('body')}<textarea bind:value={sBody} rows="6"></textarea></label>
    <div class="row fd-pair">
      <label class:clash={clashed('kind')}>Вид {@render clashNote('kind')}
        <select bind:value={sKind}>{#each INSIGHT_KINDS as k (k)}<option value={k}>{k}</option>{/each}</select>
      </label>
      <label>Надёжность
        <select bind:value={sCertainty} aria-label="Надёжность новой записи">
          <option value="">без статуса</option>
          {#each CERTAINTIES as x (x)}<option value={x}>{x}</option>{/each}
        </select>
      </label>
    </div>
    {#if sKind === 'decision'}{@render adrFields()}{/if}
  {:else if mode === 'adr'}
    {@render adrFields()}
  {:else if mode === 'retract'}
    <label>Причина <textarea bind:value={rReason} rows="3" data-autofocus></textarea></label>
  {:else if mode === 'certainty'}
    <!-- главный выбор окна — статус: фокус при открытии — на выбранном -->
    <fieldset class="row opts" class:clash={clashed('to')}>
      <legend class="sr">Новый статус{clashed('to') ? ' — изменено и здесь, и в другом месте' : ''}</legend>
      {#each CERTAINTIES as x (x)}
        <label class="inline opt" title={CERTAINTY_TEXT[x]}
          ><input type="radio" bind:group={cTo} value={x} data-autofocus={cTo === x ? '' : undefined} /> {x}</label
        >
      {/each}
      <label class="inline opt"><input type="radio" bind:group={cTo} value="" data-autofocus={cTo === '' ? '' : undefined} /> без статуса</label>
    </fieldset>
    {#if clashed('to')}<p class="clash-note" aria-hidden="true">статус изменён и здесь, и в другом месте</p>{/if}
    {#if cTo === 'theorem'}
      <div class="ev">
        <p class="muted small">Доказательство (data.evidence) <span class="muted small">(необязательно)</span>: что проверено, как и с каким результатом.</p>
        <label class:clash={clashed('checked')}>Проверено {@render clashNote('checked')}<input bind:value={evChecked} /></label>
        <label class:clash={clashed('method')}>Метод {@render clashNote('method')}<input bind:value={evMethod} /></label>
        <label class:clash={clashed('result')}>Результат {@render clashNote('result')}<input bind:value={evResult} /></label>
      </div>
    {/if}
    <label>Причина <span class="muted small">(необязательно)</span> <textarea bind:value={cReason} rows="2"></textarea></label>
  {:else if mode === 'triggers'}
    {#if tDraft.length === 0}<p class="muted">Триггеров нет.</p>{/if}
    {#if clashed('triggers')}<p class="clash-note">триггеры изменены и здесь, и в другом месте</p>{/if}
    {#if tDraft.length}
    <ul class="triggers" class:clash={clashed('triggers')}>
      {#each tDraft as t, i (i)}
        <li>
          <span class="chip">{TRIGGER_TEXT[t.type]}</span> <code>{t.pattern}</code>
          {#if t.tools?.length}<span class="muted small">({t.tools.join(', ')})</span>{/if}
          <button type="button" class="danger small-btn" aria-label="Удалить триггер {t.pattern}"
            onclick={() => (tDraft = tDraft.filter((_, j) => j !== i))}>×</button>
        </li>
      {/each}
    </ul>
    {/if}
    <div class="row add-trigger" role="group" aria-label="Новый триггер">
      <select bind:value={tType} aria-label="Тип триггера">
        {#each TRIGGER_TYPES as x (x)}<option value={x}>{TRIGGER_TEXT[x]}</option>{/each}
      </select>
      <input
        bind:value={tPattern}
        placeholder="шаблон"
        aria-label="Шаблон триггера"
        data-autofocus
        onkeydown={(e) => {
          // Enter в шаблоне — добавить триггер в черновик, а не сохранить окно
          if (e.key === 'Enter' && !e.ctrlKey && !e.metaKey) {
            e.preventDefault();
            addTrigger();
          }
        }}
      />
      <input bind:value={tTools} placeholder="инструменты: Write, Bash" aria-label="Инструменты" />
      <button type="button" onclick={addTrigger}>Добавить</button>
    </div>
  {/if}
</FormDialog>

<!-- пометка поля-расхождения 409 в подписи (её читает и программа чтения с экрана) -->
{#snippet clashNote(k: string)}
  {#if clashed(k)}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}
{/snippet}

{#snippet adrFields()}
  <fieldset class="adr-form" class:clash={clashed('adr') || clashed('options')}>
    <legend class="muted small">Решение (ADR){#if clashed('adr') || clashed('options')}<span class="clash-note">изменено и здесь, и в другом месте</span>{/if}</legend>
    <label>{ADR_TEXT.context} <textarea bind:value={adr.context} rows="2"></textarea></label>
    <label>{ADR_TEXT.options} <span class="muted small">(по одному в строке)</span> <textarea bind:value={adrOptions} rows="3"></textarea></label>
    <label>{ADR_TEXT.chosen} <input bind:value={adr.chosen} /></label>
    <label>{ADR_TEXT.rationale} <textarea bind:value={adr.rationale} rows="2"></textarea></label>
    <label>{ADR_TEXT.consequences} <textarea bind:value={adr.consequences} rows="2"></textarea></label>
    <label>{ADR_TEXT.decided_by}
      <select bind:value={adr.decided_by}>
        <option value="human">человек</option>
        <option value="agent">агент</option>
      </select>
    </label>
    <label>{ADR_TEXT.revisit_if} <input bind:value={adr.revisit_if} /></label>
  </fieldset>
{/snippet}


<style>
  .more[aria-disabled='true'] {
    opacity: 0.5;
    cursor: default;
  }
  .bar {
    margin-bottom: 8px;
  }
  .filters {
    margin-bottom: 12px;
  }
  .filters label {
    gap: 6px;
  }
  .filters input:not([type='checkbox']) {
    width: 130px;
  }
  .row-meta {
    margin-top: 2px;
    overflow-wrap: anywhere;
  }
  .usage {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .rate {
    margin: 8px 0;
  }
  .rate button {
    padding: 2px 10px;
  }
  tr.selected {
    background: var(--selected);
  }
  tr.st-superseded td,
  tr.st-retracted td {
    color: var(--muted);
  }
  .link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    text-align: left;
  }
  .chip {
    display: inline-block;
    margin-left: 4px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--chip);
    font-size: 12px;
  }
  .card {
    margin-top: 4px;
  }
  .card h2 {
    margin: 0 0 4px;
  }
  .meta {
    gap: 6px;
  }
  .body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .evidence {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 12px;
    margin: 8px 0;
    font-size: 14px;
  }
  .adr {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 12px;
    margin: 8px 0;
    font-size: 14px;
  }
  .adr dt {
    color: var(--muted);
  }
  .adr dd {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .adr ul {
    margin: 0;
    padding-left: 18px;
  }
  .adr-form {
    display: grid;
    gap: 8px;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .adr-form label {
    display: grid;
    gap: 4px;
  }
  .adr-form textarea {
    font: inherit;
    padding: 6px 10px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .chip.stale {
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .evidence.old {
    opacity: 0.75;
  }
  .ev-title {
    margin: 8px 0 0;
  }
  .cut {
    list-style: none;
  }
  .evidence dt {
    color: var(--muted);
  }
  .evidence dd {
    margin: 0;
  }
  .chain ol {
    margin: 0;
    padding-left: 20px;
  }
  .chain li.here {
    font-weight: 500;
  }
  h3 {
    margin: 14px 0 6px;
    font-size: 15px;
  }
  .actions {
    margin-top: 12px;
  }
  header .warn {
    margin: 8px 0 0;
  }
  .card-notice :global(.notice:not(:empty)) {
    margin: 8px 0 0;
  }
  /* окна форм: вид и надёжность — рядом, варианты статуса — в ряд */
  .fd-pair {
    align-items: start;
  }
  .fd-pair > label {
    flex: 1 1 180px;
  }
  .opt {
    display: inline-flex;
    gap: 6px;
    align-items: center;
  }
  .add-trigger input {
    flex: 1 1 160px;
    width: auto !important;
    min-width: 0;
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .ev {
    display: grid;
    gap: 6px;
    padding: 8px;
    border-left: 3px solid var(--callout-tip);
  }
  .triggers {
    margin: 0;
    padding-left: 18px;
  }
  .small {
    font-size: 12px;
  }
  /* строка фильтра «Шумные» и предупреждение «данные об открытии копятся с …» (M7.3 §6.6 п. 3) */
  .noisy-note {
    margin: 6px 0;
  }
  .warn-inline {
    margin-left: 4px;
    padding: 0 4px;
    border-radius: 4px;
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  /* «×» триггера — прежний кегль: общее `button.small-btn` (app.css) задаёт 12 px */
  .small-btn {
    padding: 0 6px;
    font-size: inherit;
  }
</style>
