# Набор областей: как составлен (независимый агент, задача 11)

> **Примечание исполнителя задачи 11** (перенесено из `.tmp/m6/task11/independent/README-set.md`
> без правок текста агента). Итоговые метки `kind` в `eval/area-queries.toml` отличаются от
> описанных ниже у `aq-05` и `aq-24`: их цели на двух языках, `cross-lang` им не подходит —
> исполнитель вернул исходные метки агента (`paraphrase`, `identifier`); `aq-28` — `cross-lang`.
> Раскладка `kind` набора: paraphrase 14, cross-lang 14, identifier 4, exact 2, broad 2. `expect`,
> `area`, `area_kind` не менялись (журнал M6, задача 11).


Файл набора: `area-queries.toml` (36 запросов: same 12, other 12, none 12; по 4 на каждую
область backend/frontend/infra в каждом виде; ru 18, en 18; paraphrase 13, cross-lang 16,
identifier 3, exact 2, broad 2). Исправлено по штатной проверке набора: у aq-05 (цель
en/web/fetch-retry-abort), aq-24 (ru/db/postgres-zero-downtime-migrations) и aq-28
(en/long/redis-cluster-migration) язык запроса отличается от языка цели, поэтому kind
изменён на cross-lang (было paraphrase, identifier, identifier); expect, area, area_kind не менялись. Проверено скриптом: области всех целей соответствуют
`area_kind`, все пути и ключи существуют, у `cross-lang` язык запроса не совпадает ни с одной
целью, идентификаторы `identifier` дословно есть во всех целях, ни одна цель не встречается
больше двух раз, тексты до 200 символов.

## Прочитанные файлы

- `eval/README.md` — строки 1–224 (до раздела «Запуск»).
- `eval/queries.toml` — начало (образец формата).
- `eval/corpus/areas.toml` — целиком.
- `eval/insights.toml` — через разбор TOML: список ключей/областей/заголовков всех записей;
  тела k01-t, k02-t, k03-t, k04-t, k06-n, k07-t, k08-t, k09-t, k09-n, k10-t, k10-n, k11-t,
  k12-t, k13-t, k13-n, k14-t, k15-t, k16-t, k16-n, k17-t, k17-n, k18-t, k18-n, k19-t, k20-t,
  k20-n, k21-t, k21-n, k22-t, k23-t, k23-n, k24-t, c01-b, c05-c, c07-b, c08-b, c09-c, c13-b,
  c14-c, r02-fix, r05-fix.
- Заметки целиком: все `ru|en/api/*`, `ru|en/web/*`, `ru|en/infra/*`; `ru/ops/timeouts`,
  `ru/ops/retries-backoff-jitter`, `en/ops/http-cache-headers`, `ru/ops/tls-incomplete-chain`,
  `ru/ops/tls-expired-certificate`, `en/linux/journalctl-filters`, `en/linux/oom-killer-dmesg`,
  `en/docker/alpine-musl-dns`, `en/docker/exit-code-137-container-oom`,
  `en/ops/dns-ttl-migration`, `en/devtools/node-version-pinning`,
  `en/devtools/npm-ci-clean-install`, `en/devtools/npm-install-and-lockfile`,
  `en/devtools/gha-cache-keys`, `en/devtools/eslint-prettier-conflict`,
  `ru/devtools/pre-commit-hooks`, `ru/db/sqlite-wal`, `ru/db/sqlite-busy-timeout`.
- Частично (разделы/grep): `en/long/feature-flag-platform` (Architecture … Incidents),
  `en/long/redis-cluster-migration` (Multi-key operations), `en/long/observability-stack-design`
  (заголовки), `ru/db/sqlite-vacuum`, `ru/db/postgres-zero-downtime-migrations` (lock_timeout),
  `ru/long/checkout-load-testing-report` (кэш тарифов доставки),
  `ru/long/incident-disk-full-logs` (502), `ru/long/tls-certificate-rotation` (цепочки),
  `ru/long/dev-environment-onboarding` (версия Node), `ru/git/hooks-commit-msg` (--no-verify);
  grep по корпусу для идентификаторов (57014, lock_timeout, IntersectionObserver, rootMargin,
  CROSSSLOT, http.Client и др.).

Код, docs, сетки, kind-queries, результаты — не открывались.

## Как выбирались цели и соседи

Для `same`/`other` брались пары тем групп `k` из разных областей (повторы, кэш, тайм-ауты,
токены, время, загрузки, лимиты, пробы, сборка, пагинация); формулировка — о детали, которая
есть только в цели (бюджет 10 %, 57014, skipWaiting/ожидание вкладок, докачка частей и т. п.),
при этом соседняя тема другой области лексически близка. Insight той же группы включался в
`expect`, если он сам отвечает на вопрос. Для `none` — заметки и insight без области, у которых
есть близкий по теме сосед с областью; если вторая цель без области отвечает так же — она в
`expect`.

| id | цель | сосед, который НЕ отвечает |
|---|---|---|
| aq-01 | ru/api/http-client-retries, ru/ops/retries-backoff-jitter | en/web/fetch-retry-abort, ru/web/fetch-retry-ui (frontend) |
| aq-02 | en/api/product-cache-invalidation | en/web/browser-http-cache (frontend), en/ops/http-cache-headers (infra); k03-t — только TTL |
| aq-03 | ru/api/jwt-validation | en/web/token-refresh-tabs, k11-t (frontend) |
| aq-04 | en/api/database-statement-timeout | ru/infra/proxy-timeouts-long-requests (infra); ru/api/service-request-timeouts (тоже 503, без 57014) |
| aq-05 | ru/web/fetch-retry-ui, en/web/fetch-retry-abort | ru/api/http-client-retries, ru/ops/retries-backoff-jitter (backend) |
| aq-06 | ru/web/date-formatting-intl, k16-n | en/api/timestamps-utc, k15-t (backend) |
| aq-07 | en/web/large-file-upload | ru/infra/upload-size-limits (infra, части 64 МБ); k17-n — размер частей, не докачка |
| aq-08 | ru/web/service-worker-cache | ru/api/redis-cache-ttl (backend, релиз/формат); en/ops/http-cache-headers (infra) |
| aq-09 | ru/infra/proxy-timeouts-long-requests, k08-t | ru/api/service-request-timeouts, en/api/database-statement-timeout (backend) |
| aq-10 | ru/infra/readiness-liveness-probes, k23-t | en/api/health-endpoint-design, k24-t (backend) |
| aq-11 | en/infra/nginx-limit-req, k20-t, k20-n | ru/api/rate-limiting-token-bucket, k19-t (backend) |
| aq-12 | ru/infra/upload-size-limits | en/web/large-file-upload (frontend, «до 2 ГБ») |
| aq-13 | ru/api/http-client-retries | текущая frontend: en/web/fetch-retry-abort |
| aq-14 | k04-t, ru/web/service-worker-cache | текущая backend: ru/api/redis-cache-ttl |
| aq-15 | ru/api/service-request-timeouts | текущая infra: ru/infra/proxy-timeouts-long-requests (упоминает 30 с) |
| aq-16 | ru/api/redis-cache-ttl | текущая frontend: ru/web/service-worker-cache |
| aq-17 | en/web/client-error-reporting | текущая backend: ru/api/structured-logging |
| aq-18 | en/api/health-endpoint-design | текущая infra: en/infra/load-balancer-health-checks |
| aq-19 | en/api/timestamps-utc | текущая frontend: ru/web/date-formatting-intl, k16-t |
| aq-20 | ru/infra/readiness-liveness-probes, k23-n | текущая backend: en/api/health-endpoint-design |
| aq-21 | ru/web/vite-env-variables, k21-n | текущая infra: en/infra/ci-docker-layer-cache (слабый сосед) |
| aq-22 | en/web/infinite-scroll | текущая backend: ru/api/keyset-pagination, k13-t |
| aq-23 | k18-t, ru/infra/upload-size-limits | текущая frontend: en/web/large-file-upload, k17-t |
| aq-24 | en/api/database-statement-timeout, ru/db/postgres-zero-downtime-migrations | текущая infra: ru/infra/proxy-timeouts-long-requests |
| aq-25 | ru/ops/timeouts | backend: ru/api/service-request-timeouts |
| aq-26 | c01-b, ru/long/checkout-load-testing-report | backend: ru/api/redis-cache-ttl, k03-t (давка кэша на распродаже) |
| aq-27 | ru/db/sqlite-wal | backend: ru/db/sqlite-busy-timeout, p16-t |
| aq-28 | en/long/redis-cluster-migration | backend: ru/api/redis-cache-ttl, ru/api/rate-limiting-token-bucket (Lua в Redis) |
| aq-29 | en/long/feature-flag-platform | frontend: ru/web/feature-flags-first-paint; backend: en/api/feature-flag-evaluation |
| aq-30 | c05-c | frontend: en/devtools/node-version-pinning (пример 20.18.0) |
| aq-31 | ru/devtools/pre-commit-hooks | frontend: en/devtools/eslint-prettier-conflict |
| aq-32 | en/devtools/npm-install-and-lockfile | frontend: en/devtools/npm-ci-clean-install |
| aq-33 | r05-fix, ru/long/incident-disk-full-logs | infra: ru/infra/upload-size-limits, k18-n |
| aq-34 | c08-b, en/devtools/gha-cache-keys | infra: en/infra/ci-docker-layer-cache (кэш по веткам BuildKit) |
| aq-35 | ru/ops/tls-incomplete-chain, ru/long/tls-certificate-rotation | infra: ru/ops/tls-expired-certificate |
| aq-36 | en/linux/oom-killer-dmesg | infra: en/docker/exit-code-137-container-oom, p17-t |

## Сомнительные места разметки

- aq-09: `ru/ops/timeouts` (без области) общо объясняет, что `proxy_read_timeout` меньше
  тайм-аута приложения даёт 504; про 60 с по умолчанию и маршрут не говорит — в `expect` не
  включена.
- aq-02: `insight:k03-t` упоминает TTL 15 минут карточек, но не механизм удаления — не включён.
- aq-12: `insight:k18-t` говорит, что предел задаётся явно из общего шаблона стенда, но не
  текущие значения — не включён; при строгой оценке может считаться частично релевантным.
- aq-23: заметка `ru/infra/upload-size-limits` не называет 413 и значение по умолчанию 1 МБ,
  включена как ответ на «где предел»; главный ответ — `k18-t`.
- aq-06: `k16-n` отвечает только на вторую половину (скорость таблиц); включён.
- aq-10/aq-20: `en/api/health-endpoint-design`/`k24-t` (backend) о проверке зависимостей в
  `/health`, не о пробах Kubernetes — считаю неотвечающими.
- aq-30: `ru/long/dev-environment-onboarding` (без области) содержит `nodejs 20.11.1` в списке
  версий — устаревшее/иное значение; не включена. Также существуют замещённые `c05-a`,
  `c05-b` (без `forbid`, вид набора не `superseded`).
- aq-26: `c01-a` (замещённый, 10 минут) — не в `expect`; отчёт о нагрузке отвечает только про
  прогрев.
- aq-21: в текущей области infra соседа той же темы почти нет (кэш слоёв CI — слабая связь).
- aq-08, aq-14: `en/web/browser-http-cache` (frontend) и `en/ops/http-cache-headers` (infra)
  описывают `no-cache` для оболочки — частично близко к aq-14 (для other допустимо, но в
  `expect` не включены, т. к. вопрос именно про service worker).
- aq-28 (ru → en), aq-24 и aq-05 (часть целей на другом языке): исправлено по штатной
  проверке — kind = `cross-lang` (правило проверки: язык запроса отличается от языка цели).
- aq-14 содержит слова «service worker», aq-17 — «в браузере»: для `other` это допустимо и
  естественно, но облегчает запрос.
