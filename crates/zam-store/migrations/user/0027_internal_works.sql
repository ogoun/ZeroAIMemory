-- user DB, задача 1 M7.3 (спецификация M7 §6 «Общее», §6.2–§6.7): вся схема волны «Внутренние
-- работы» одной миграцией (один `bak-v26`, один шаг отката). Зачем: пробы ночной проверки, её
-- прогоны и результаты, очередь работ исполнителя, журнал отправок модели, кандидаты в Rules и
-- открытия подсказок агентом должны переживать перезапуск сервера, а §3 разрешает миграцию
-- только под хранилище проб — остальное ложится в ту же. Схема полна с первой задачи: второй
-- миграции в волне нет (колонки пре-флайта — `works.result_key`, `check_results.space_id` в
-- ключе, `check_runs.trigger_kind`; решения координатора 69, 70, 75).
--
-- Только новые таблицы, без пересборки (`rebuild: false`): прежние таблицы, их индексы и
-- триггеры не меняются — `usage` и её индекс `usage_target` (0013) остаются как были (§6.6 п. 1).
-- Внешних ключей на `entries` и `sessions` нет: ссылки на записи — строкой `<буква>:<id>` в JSON
-- (`targets`, `items`, `basis`, `result_ref`), как `data.review` идей; открытия (`usage_opened`)
-- переживают уборку сессий (`SESSION_CHILDREN` их не трогает) — это данные «Шумных» и M7.5.
-- Время — TEXT, формат zam_store::time. У `works.kind` и `proposals.type` CHECK нет: допустимые
-- значения проверяет код (`zam_store::works::WorkKind`, `ProposalType`), строка неизвестного
-- значения при чтении пропускается с WARN — виды уборки M7.5 добавятся без миграции (как
-- `session_snapshots.trigger_kind` в 0025).

-- ---------- пробы ночной проверки (§6.4 п. 1) ----------
-- Проба `x:<id>` — вопрос о записи пространства; вне поиска, дайджеста, подсказок, выжимок и
-- экспорта. `targets` — JSON `[{ref, rev}]` (у `negative` — `[]`); `space_id` у `negative` — ''
-- (общие для пользователя); `overlap` — доля слов вопроса среди слов заголовка цели. Цель
-- удалена, замещена или отозвана — `retired` с `retired_reason = 'target_gone'`.
CREATE TABLE probes (
  id TEXT PRIMARY KEY,
  space_id TEXT NOT NULL,
  question TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('probe','cache','negative','manual')),
  targets TEXT NOT NULL DEFAULT '[]',
  split TEXT NOT NULL CHECK (split IN ('dev','test')),
  overlap REAL,
  author_kind TEXT NOT NULL CHECK (author_kind IN ('agent','model','human','system')),
  author TEXT,                        -- model:<имя модели> | сессия агента | пользователь
  work_id TEXT,                       -- работа `j:`, давшая пробу
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','retired')),
  retired_reason TEXT,
  created_at TEXT NOT NULL,
  retired_at TEXT);
CREATE INDEX probes_active ON probes(space_id) WHERE status = 'active';

-- ---------- прогоны ночной проверки (§6.5 пп. 1, 4, 5, 7) ----------
-- Повод — `trigger_kind`, а не `trigger`: TRIGGER — ключевое слово SQL (как в 0025). `night` —
-- дата ночи (слот в поясе `[works] tz`, `yyyy-MM-dd`) у ночного прогона, у ручного — `NULL`:
-- ночной прогон ночи один (кроме прерванных), ищется по `(trigger_kind, night)`.
-- `fingerprint`, `context`, `metrics`, `verdict` — JSON; `flags` — JSON-массив
-- (`backup_overlap`, …); `episode` — id первого прогона эпизода регрессии. Хранение — 365 суток
-- (воркер обслуживания), результаты уходят каскадом.
CREATE TABLE check_runs (
  id TEXT PRIMARY KEY,
  trigger_kind TEXT NOT NULL CHECK (trigger_kind IN ('nightly','manual')),
  night TEXT,
  status TEXT NOT NULL CHECK (status IN ('running','done','skipped','timeout','failed')),
  started_at TEXT NOT NULL,
  finished_at TEXT,
  fingerprint TEXT,
  fp_hash TEXT,
  context TEXT,
  metrics TEXT,
  verdict TEXT,
  flags TEXT NOT NULL DEFAULT '[]',
  episode TEXT,
  accepted_at TEXT,
  accepted_by TEXT,
  error TEXT);
CREATE INDEX check_runs_night ON check_runs(trigger_kind, night);

-- Результат пробы в прогоне. Посторонний вопрос (`negative`) идёт в области каждого решения
-- пользователя (§6.5 п. 2), поэтому `space_id` — в ключе: у `negative` — решение запроса или ''
-- (без решения), у прочих — пространство пробы (решение координатора 69). Ранги — номер карточки
-- цели (NULL — не нашлась), `search_cut` — карточек в бюджете выдачи; время — мс.
CREATE TABLE check_results (
  run_id TEXT NOT NULL REFERENCES check_runs(id) ON DELETE CASCADE,
  probe_id TEXT NOT NULL,
  space_id TEXT NOT NULL DEFAULT '',
  search_rank INTEGER,
  search_cut INTEGER,
  deep_rank INTEGER,
  prompt_rank INTEGER,
  search_ms INTEGER,
  prompt_ms INTEGER,
  PRIMARY KEY (run_id, probe_id, space_id)) WITHOUT ROWID;

-- ---------- очередь внутренних работ (§6.3 п. 1) ----------
-- Работа `j:<id>`: `kind` — вид работы без CHECK (`WorkKind`), `night` — дата ночи у ночной
-- постановки; `items` — JSON ссылок и ревизий кандидатов пакета; `result` — JSON итога (принято,
-- отклонено с причинами, `skip` исполнителя, пропущено); `result_key` — `Idempotency-Key`
-- принятого итога (повтор с тем же ключом — прежний ответ, другой — 409; как
-- `entries.idempotency_key` в 0008, решение 70). Незакрытой работы вида на пространство — не
-- больше одной (`works_open`, проверяет код). Закрытые работы хранятся 365 суток от
-- `finished_at` (воркер обслуживания) — столько же итоги работ читает отсев кандидатов вида
-- (`works_finished`; финальное ревью M7.3, Minor 4).
CREATE TABLE works (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  space_id TEXT NOT NULL,
  mode TEXT NOT NULL CHECK (mode IN ('agent','model')),
  origin TEXT NOT NULL CHECK (origin IN ('nightly','manual')),
  night TEXT,
  state TEXT NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued','running','done','failed','expired','cancelled')),
  items TEXT NOT NULL DEFAULT '[]',
  claimed_session TEXT,
  claimed_until TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT,
  result TEXT,
  result_key TEXT,
  error_class TEXT);
CREATE INDEX works_open ON works(space_id, kind) WHERE state IN ('queued','running');
CREATE INDEX works_night ON works(origin, night);
CREATE INDEX works_finished ON works(kind, finished_at);

-- ---------- журнал отправок модели (§6.2 п. 8) ----------
-- Метаданные обращения к подключённой модели — без текста пакета и ответа. `kind` — вид работы
-- или `test` (проверка подключения, §6.2 п. 9); `outcome` — `ok`, класс ошибки, `aborted`
-- (запрос ушёл, ответа нет — остановка сервера) или `internal` (сбой сервера без отправки);
-- `ms` — NULL, если ответа не было; `items` — элементов в пакете. Расход токенов суток и месяца — сумма по журналу
-- (`work_calls_ts`), отдельного счётчика нет. `run` — запуск обращения: `night:<дата ночи>` или
-- `manual:<id запуска>` (у проверки подключения — NULL): «стоп до следующей ночи» и аудит
-- `key_use` «раз за ночь» считаются по строкам своего запуска (`work_calls_run`; решение
-- координатора 77, ревью r1 задачи 2). Хранение — 180 суток.
CREATE TABLE work_calls (
  id INTEGER PRIMARY KEY,
  ts TEXT NOT NULL,
  work_id TEXT,
  kind TEXT NOT NULL,
  host TEXT NOT NULL,
  model TEXT NOT NULL,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  estimated INTEGER NOT NULL DEFAULT 0,
  ms INTEGER,
  outcome TEXT NOT NULL,
  http_status INTEGER,
  external INTEGER NOT NULL DEFAULT 0,
  items INTEGER,
  run TEXT);
CREATE INDEX work_calls_ts ON work_calls(ts);
CREATE INDEX work_calls_run ON work_calls(run) WHERE run IS NOT NULL;

-- ---------- предложения человеку: кандидаты в Rules (§6.7 п. 1) ----------
-- Предложение `o:<id>`: `type` без CHECK (`ProposalType`; в M7.3 — `rule`, уборка M7.5 добавит
-- свои), `payload` — JSON по типу (`rule` — `{path, content}`), `targets` и `basis` — JSON-массивы
-- ссылок, `dedup_key` — ключ отсева повторов, `result_ref` — заметка `n:<id>` принятого.
-- `skipped` — исполнитель решил «правила здесь нет»: человеку не показывается.
CREATE TABLE proposals (
  id TEXT PRIMARY KEY,
  space_id TEXT NOT NULL,
  type TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'proposed'
         CHECK (status IN ('proposed','accepted','declined','withdrawn','stale','skipped')),
  targets TEXT NOT NULL DEFAULT '[]',
  payload TEXT NOT NULL DEFAULT '{}',
  reason TEXT,
  basis TEXT NOT NULL DEFAULT '[]',
  dedup_key TEXT,
  author_kind TEXT NOT NULL CHECK (author_kind IN ('agent','model')),
  author TEXT,
  work_id TEXT,
  session_id TEXT,
  created_at TEXT NOT NULL,
  decided_at TEXT,
  decided_by TEXT,
  decision_note TEXT,
  result_ref TEXT);
CREATE INDEX proposals_open ON proposals(space_id, type, status);
CREATE INDEX proposals_dedup ON proposals(dedup_key);

-- ---------- «агент открыл подсказку» (§6.6 пп. 1–2) ----------
-- Чтение цели по токену сессией после показа этой цели подсказкой той же сессии в той же эпохе;
-- одно событие на сессию и цель держит первичный ключ (`INSERT OR IGNORE`), а не проверка.
-- `source` — событие показа (`session_start`, `prompt`, …). Отдельная таблица, а не событие
-- `usage`: `usage` не пересобирается (решение координатора 50).
CREATE TABLE usage_opened (
  ts TEXT NOT NULL,
  user_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id TEXT NOT NULL,
  source TEXT,
  PRIMARY KEY (session_id, target_kind, target_id)) WITHOUT ROWID;
CREATE INDEX usage_opened_target ON usage_opened(target_kind, target_id);

-- Момент применения миграции (§6.6 п. 3): с него копятся открытия — граница окна «Шумных».
-- Формат хранилища `fmt_ts` (шесть знаков долей), как в 0019.
INSERT INTO meta (key, value) VALUES
  ('usage_opened_since', strftime('%Y-%m-%dT%H:%M:%S', 'now') || '.000000Z');
