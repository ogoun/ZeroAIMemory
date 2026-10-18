# M0 — журнал выполнения (решения и отложенные замечания)

Перенесено из рабочего журнала процесса SDD (.superpowers/, не в git). План: [2026-10-18-m0-skeleton.md](2026-10-18-m0-skeleton.md).


Spec: docs/specs/2026-10-17-zeroaimemory-tz.md (v1.0). Branch m0-skeleton, base commit 4913ebc.

## Pre-flight scan
| Пара / задача | Производит → потребляет | Находка |
|---|---|---|
| T0 ↔ все | workspace, тулчейн | Rust portable в .toolchain, C: полон; нужно окружение для всех последующих команд → R1. git core.autocrlf=true на машине → R2 |
| T1 → T5, T6 | Id (+ToSql feature sqlite), generate_token, hash_token, mask_token | согласовано |
| T2 → T7 | ServerConfig, load, admin_secret(Option<&str>) | T2 создаёт только main.rs, T7 — lib.rs → R3 |
| T3 → T4..T6 | Db::open, write, read, Migration | согласовано |
| T4 → T5..T7 | DataDir (держит лок, не Clone) | T6 описывает `UserDbs { dir: DataDir-paths }` → R4 |
| T5 → T7..T9 | SystemStore API | согласовано |
| T7 → T8 | AppState, build, Router | ConnectInfo нужен T8 (client_ip), в oneshot-тестах его нет → R5 |
| T8 тест логов | глобальный tracing subscriber в тестовом бинаре | → R6 |
| T8 → T11 | GET /spaces | T11 печатает пользователя «по /spaces», но форма ответа не задана → R7 |
| T7 ↔ T10 | build.rs | T7 создаёт, T10 изменяет — согласовано после правки плана |
| T7 → T13 | первый токен печатается в stderr | smoke читает его из stderr — согласовано |
| T12 → T13 | target/deps/<platform>/<name>/ | согласовано |
| T14 | пуш и тег | внешнее действие → стоп-точка, спросить владельца |
| Каждая задача: собственная согласованность | тесты ↔ код ↔ файлы | T2 тест-хелпер test_util.rs не в списке Files — допустимо (внутри crate); остальное согласовано |
| Правило temp | тесты | unit-тесты не имеют CARGO_TARGET_TMPDIR → R8 |

## Rulings
- R1: Ruling: тулчейн ставится в `.toolchain/` (RUSTUP_HOME, CARGO_HOME); Task 0 добавляет `scripts/dev-env.ps1` и `scripts/dev-env.sh`, которые выставляют RUSTUP_HOME, CARGO_HOME, PATH, а также TMP/TEMP=`<repo>/.tmp/systmp` и npm_config_cache=`<repo>/.tmp/npm-cache`; все последующие команды выполняются после этого скрипта — правило «временные только в проекте» и диск C полон — если неверно: лишний скрипт.
- R2: Ruling: Task 0 добавляет `.gitattributes` (`* text=auto eol=lf`; `*.ps1`, `*.cmd`, `*.bat` → `eol=crlf`; бинарные форматы → binary) — именно сценарий CRLF/LF из ТЗ §1.1; shell-скрипты и YAML должны быть LF — если неверно: перенормализация файлов.
- R3: Ruling: Task 2 сразу создаёт `crates/zam-server/src/lib.rs` (`pub mod config;`) и тонкий `main.rs`; Task 7 расширяет lib.rs — избегает переноса модулей позже — цена ошибки: нулевая.
- R4: Ruling: `UserDbs` хранит `PathBuf` каталога `users/` (из `DataDir::users_dir()`), а `DataDir` (владелец лока) живёт в `AppState` — DataDir не клонируемый — цена: добавить метод users_dir().
- R5: Ruling: сервер запускается через `into_make_service_with_connect_info::<SocketAddr>()`; извлекатели берут `Option<ConnectInfo<SocketAddr>>`, при отсутствии peer считается недоверенным `0.0.0.0` — тесты через oneshot вставляют ConnectInfo явно, когда нужно — цена: мелкая.
- R6: Ruling: тест «токен не в логах» вынести в отдельный интеграционный бинарь `tests/auth_logs.rs` с одним тестом, глобальный subscriber с записью в общий буфер — исключает гонки subscriber'ов — цена: лишний тестовый бинарь.
- R7: Ruling: `GET /api/v1/spaces` возвращает `{ "user": {"id","name"}, "access": "rw|ro|admin", "spaces": [...] }` — ТЗ форму не задаёт, `zam status` нужен пользователь — цена: изменить форму позже (M1).
- R8: Ruling: unit-тесты пишут во `<workspace>/target/test-tmp/<crate>/<test>` (путь от `CARGO_MANIFEST_DIR`), интеграционные — в `CARGO_TARGET_TMPDIR`; `std::env::temp_dir()` запрещён — правило владельца — цена: нулевая.

## Tasks
Task 0: dispatched (base 4913ebc, implementer sonnet)
Task 0: Ruling: license MIT (как в существующем LICENSE) вместо Apache-2.0 из dispatch — LICENSE уже MIT, ошибка в моём брифе — цена: сменить при желании владельца
Task 0: review 1 — approved with 1 Important (dev-env.ps1 без BOM); .gitignore проверен контроллером (git check-ignore .toolchain .tmp .superpowers — ok)
Task 0: minor (deferred): rust-toolchain.toml channel=stable плавает (локально 1.99.1); targets в rust-toolchain.toml сверх брифа; [term] color=auto лишний
Task 0: fix round 1/5 (1 addressed, 0 open; commits 02e9341..f3ad0df)
Task 0: complete (commits 4913ebc..f3ad0df, review clean)
Task 1: dispatched (base f3ad0df, implementer sonnet)
Task 1: implementer DONE (3b25990); review dispatched
Task 1: minor (deferred): нет теста mask_token на не-ASCII/короткие строки; нет теста монотонности Id в одну мс; Display Id игнорирует ширину (f.pad); rusqlite bundled в workspace deps
Task 1: complete (commits f3ad0df..3b25990, review clean)
Task 2: dispatched (base 3b25990, implementer sonnet)
Task 2: Ruling: трейлер Co-Authored-By — модели, реально писавшей коммит (Sonnet у исполнителей) — честная атрибуция — цена: нет
Task 2: implementer DONE (39a1b54); review dispatched
Task 2: review 1 — Needs fixes: Important(plan-mandated) env-значение как TOML-скаляр ломает строковые поля (ZAM_ADMIN__LOGIN=123)
Task 2: Ruling: исправить — при провале пробы с разобранным значением повторить пробу как строку; формулировка плана «при неудаче берётся строкой» трактуется как «при неудаче типа» — ТЗ требует рабочей настройки через env — цена: нет
Task 2: Ruling: в тот же раунд включить minor 2 (пустая неизвестная таблица в TOML молча принимается) и minor 3 (ошибка синтаксиса цитирует строку файла с хэшем) — дёшево, касается правила «нет секретов в логах» — цена: чуть больше работы
Task 2: minor (deferred): base_path не схлопывает внутренние //; Debug печатает хэш пароля; лишняя зависимость zam-core; trusted_proxies из env только TOML-массивом; нет теста различия солей
Task 2: fix round 1/5 (3 addressed, 0 open; commits 39a1b54..e49c813)
Task 2: minor (deferred): столбец в ошибке синтаксиса считается в байтах
Task 2: complete (commits 3b25990..e49c813, review clean)
Task 3: dispatched (base e49c813, implementer sonnet)
Task 3: implementer DONE_WITH_CONCERNS (cc3919b) — RED не сохранён (TDD-нарушение, на ревью); review dispatched (opus: конкурентность)
Task 3: minor (deferred): (1) поток-писатель не join-ится при drop — на Windows файл держится; (2) после catch_unwind/в PooledConn::drop не проверяется is_autocommit; (3) семантика отмены write() не задокументирована; (4) паника неотличима от Closed в логах; (5) результат journal_mode=WAL не проверяется; (6) версии миграций не валидируются (дубликаты/пропуски); (7) to_string_lossy для пути бэкапа; (8) тесты: PRAGMA писателя не проверены, concurrent-тест не доказывает единственность писателя; (9) RED не сохранён
Task 3: complete (commits e49c813..cc3919b, review clean)
Task 3: Ruling: minor 1, 2, 5, 6 из Task 3 включить в объём Task 4 (тот же крейт zam-store, до задач, где БД переоткрываются и удаляются) — дёшево и снимает недетерминизм на Windows — цена: Task 4 чуть больше
Task 4: dispatched (base cc3919b, implementer sonnet)
Task 4: implementer DONE_WITH_CONCERNS (d032469, f0d9e21); review dispatched
Task 4: review 1 — Needs fixes: Important1 Inner::drop закрывает писателя раньше читателей → WAL без checkpoint, rename-тест вводит в заблуждение; Important2 пул сжался до 0 → read() висит вечно
Task 4: minor (deferred): savepoint-тест не различает исправление; ошибки записи pid проглатываются и pid нечитаем на Windows под локом (.zam.pid?); нет doc о блокирующем Drop; rename-тест различает только на Windows; подкаталоги создаются до взятия лока; нет теста на journal_mode≠wal
Task 4: fix round 1/5 (2 addressed, 0 open; commits f0d9e21..be5679b)
Task 4: minor (deferred): при отмене future read() во время drop последнего Db -wal может остаться; нет сквозного теста Closed при пустом пуле
Task 4: complete (commits cc3919b..be5679b, review clean)
Task 5: dispatched (base be5679b, implementer sonnet)
Task 5: implementer DONE (21642cd); review dispatched (opus: безопасность токенов)
Task 5: minor (deferred): (1) формат времени TEXT переменной точности; (2) просроченные сессии не удаляются без обращения; (3) ошибка обновления last_used проглатывается без лога; (4) стадо UPDATE last_used; (5) Unsupported для ошибок JSON; (6) ts_opt unwrap_or(None); (7) expect на OsRng; (8) admin_user ORDER BY id; (9) csrf открытым текстом (допустимо)
Task 5: complete (commits be5679b..21642cd, review clean)
Task 5: Ruling: minor 1, 2, 4(WHERE-условие), 5 из Task 5 включить в объём Task 6 (тот же крейт; формат времени — общий для всех таблиц, лучше закрепить до появления users/*.db) — цена: Task 6 чуть больше
Task 6: dispatched (base 21642cd, implementer sonnet)
Task 6: implementer DONE (50045c6 + ea211df); review dispatched
Task 6: minor (deferred): Db::open синхронно под глобальным tokio Mutex (spawn_blocking); concurrent_get тест не проверяет идентичность Db; .bak-v0 проверяется только на существование; нет частичного UNIQUE(kind) для фиксированных пространств; Corrupt для переполнения времени; условие порога last_used без теста гонки
Task 6: complete (commits 21642cd..ea211df, review clean)
Task 7: dispatched (base ea211df, implementer sonnet)
Task 7: implementer DONE_WITH_CONCERNS (4806e25) — RED не сохранён; добавлен SystemStore::ping(); лог-файл с суффиксом даты; review dispatched
Task 7: review 1 — Needs fixes: 6 Important (побочные эффекты run() до проверки пароля/лока; потеря первого токена — нетранзакционный bootstrap и печать до bind; login-конфликт с не-админом; prompt rpassword в stdout; build.rs git-хэш устаревает/пересборка; нет тестов маски аудита/Debug/503)
Task 7: Ruling: первый токен печатается после успешного bind; при старте, если у админа нет ни одного неотозванного токена, выпускается новый «initial» (самовосстановление вместо транзакции на 3 шага) — цена: лишний токен в редком случае
Task 7: Ruling: существующий пользователь с именем login без is_admin → отказ старта с понятным сообщением, без автоповышения — безопасность — цена: ручное вмешательство
Task 7: minor (deferred): ctrl_c Err → немедленная остановка; нет таймаута graceful shutdown; try_init молчит; RUST_LOG не учитывается; имя лог-файла с датой (документировать); нет теста SystemStore::ping
Task 7: fix round 1/5 (5 addressed, 1 open — 2b: сбой bind после выпуска токена теряет токен; commits 4806e25..115fb3e)
Task 7: Ruling: 503 /ready покрыт только через ApiError::Unavailable — принять для M0, seam для сбоя system.db — техдолг — цена: ветка ready() без интеграционного теста
Task 7: minor (deferred): ensure_admin_user не учитывает срок токена (expiry в M0 нет); нет теста «build не ставит subscriber»
Task 7: fix round 2/5 (1 addressed, 0 open; commits 115fb3e..0101575)
Task 7: minor (deferred): run_bind тест проверяет только is_err, а не AddrInUse
Task 7: complete (commits ea211df..0101575, review clean)
Task 8: dispatched (base 0101575, implementer opus — безопасность)
Task 8: implementer DONE_WITH_CONCERNS (eb2eb82); target_user async → User; RateLimiter::peek; review dispatched (opus)
Task 8: review 1 — Needs fixes: Important1 peek блокирует верные токены со всего IP (DoS за Traefik без trusted_proxies); Important2 аудит admin_login_failed пишет введённый логин (риск пароля)
Task 8: Ruling: при превышении bad_token всё равно проверять токен; верный проходит, неверный → 429 без повторного warn; однократный warn, если пришёл X-Forwarded-For, а peer не в trusted_proxies — секрет 238 бит не перебирается, лимит — от шума — цена: нет
Task 8: Ruling: аудит admin_login_failed пишет {"login_matches": bool}, без введённого значения — исключает утечку пароля — цена: меньше данных для разбора инцидентов
Task 8: Ruling: в тот же раунд включить minor 1 (фиктивный хэш вычислять в finish(), ошибка — отказ старта), minor 2 (ручной Debug Principal/AdminOnly с маской), minor 8 ([profile.dev.package.argon2] opt-level=3) — дёшево, безопасность/скорость тестов — цена: нет
Task 8: minor (deferred): XFF с портом/скобками не разбирается; trusted_proxies ::ffff:/… не канонизируются; WWW-Authenticate Bearer на /auth/login; HashMap лимитера без верхнего предела; нет интеграционного теста истёкшей сессии
Task 8: fix round 1/5 (5 addressed, 0 open; commits eb2eb82..5666474)
Task 8: minor (deferred): заголовок Forwarded (RFC 7239) не поддерживается; неверные токены в формате при превышении всё равно ходят в БД
Task 8: complete (commits 0101575..5666474, review clean)
Task 9: dispatched (base 5666474, implementer sonnet)
Task 9: implementer DONE (ed55df3); review dispatched
Task 9: minor (deferred): create_user неатомарен (user→db→audit→token); patch_user две записи; нет теста CSRF на DELETE/PATCH, переименования админа, лимита 500; limit=0 → 400 документировать; N+1 в list_users
Task 9: complete (commits 5666474..ed55df3, review clean)
Task 10: dispatched (base ed55df3, implementer sonnet)
Task 10: implementer DONE_WITH_CONCERNS (056420a); review dispatched
Task 10: review 1 — Needs fixes: Important1 F5 сбрасывает вход (CSRF только в памяти) → /auth/me отдаёт csrf; Important2 build.rs без web/dist перезапускается каждый раз
Task 10: Ruling: /auth/me возвращает csrf (GET, SameSite=Strict, без CORS — безопасно); в тот же раунд — 404 problem+json для отсутствующих путей с расширением файла под /assets — дёшево — цена: нет
Task 10: minor (deferred): нет тестов ассетов/HEAD/не-GET на SPA; общее состояние tokenName между пользователями; ошибка копирования не видна в диалоге; Cache-Control no-store на admin GET; engines в package.json
Task 10: fix round 1/5 (3 addressed, 0 open; commits 056420a..d3b33e7)
Task 10: minor (deferred): ZAM_WEB_REBUILD срабатывает только при смене значения; нет теста bearer → 403 на /auth/me; /auth/me читает сессию повторно
Task 10: complete (commits ed55df3..d3b33e7, review clean)
Task 11: dispatched (base d3b33e7, implementer sonnet)
Task 11: implementer DONE_WITH_CONCERNS (9b3a1c7); cold start mean 5.8ms p95 7.7ms (Windows release); proxy env отключён; review dispatched
Task 11: review 1 — Needs fixes: Important1 .proxy(None) ломает доступ через корпоративный прокси; Important2 битый config.toml молча → «адрес не задан»; Important3 чтение CLAUDE.md без лимита размера
Task 11: Ruling: прокси из env уважается (ureq по умолчанию, NO_PROXY), для loopback-хостов агент без прокси; в тот же раунд — ручной Debug Found/Resolved с маской, timeout_global 10 с + timeout_resolve 2 с, URL без схемы → exit 1 — дёшево, путь хуков — цена: нет
Task 11: minor (deferred): семантика нескольких/незакрытых блоков не задокументирована, плейсхолдеры <...>; --json игнорируется при exit 1/2; 429 на /spaces → exit 2; 200 не-JSON на /health; нет тестов 5xx и токена с кавычками; ring под musl требует C-компилятор (проверить в CI)
Task 11: fix round 1/5 (6 addressed, 0 open; commits 9b3a1c7..66c5502)
Task 11: minor (deferred): регистр в bypass_proxy/check_url (HTTP://, LOCALHOST); http://:80 проходит check_url; config.toml-каталог молча пропускается; Debug маскирует только значения с zam_
Task 11: complete (commits d3b33e7..66c5502, review clean)
Task 12: dispatched (base 66c5502, implementer sonnet)
Task 12: implementer DONE (c05f46f); pdfium chromium/8100; rename в манифесте; review dispatched
Task 12: review 1 — Needs fixes: Important1 нет очистки/атомарности target/deps/<platform>/<name>/ перед извлечением
Task 12: Ruling: в тот же раунд — minor 1 (валидация 64 hex + https), 2 (различать FileNotFound в zip), 3 (только обычные файлы; дубликаты имён назначения — ошибка), 4 (тест недоверенного кэша + подсказка), 5 (--print-hashes проверяет платформу) — цепочка поставки, дёшево — цена: нет
Task 12: minor (deferred): zip/deflate тянет zopfli/nix/typed-path (deflate-flate2 легче); осиротевшие *.part
Task 12: fix round 1/5 (6 addressed, 0 open; commits c05f46f..a85091c)
Task 12: minor (deferred): подмена каталога не строго атомарна (out→out.old); validate проверяет все платформы; .tmp не защищён от параллельного запуска; права +x на Linux не выставляются при извлечении (→ учесть в Task 13 при упаковке tar)
Task 12: complete (commits 66c5502..a85091c, review clean)
Task 13: Ruling: локальная Linux-проверка smoke необязательна (WSL-тулчейн лёг бы на заполненный диск C:), обязательна в CI (Task 14); в tar.gz исполняемые файлы с mode 0755 — цена: Linux-комплект впервые проверяется в CI
Task 13: dispatched (base a85091c, implementer sonnet)
Task 13: implementer DONE_WITH_CONCERNS (67de361); Windows SMOKE OK; review dispatched
Task 13: minor (deferred): (1) SHA256SUMS включает старые архивы из dist; (2) README Linux без --strip-components; (3) npx license-checker без закреплённой версии, excludePackages зашит; (4) THIRD-PARTY-WEB без текстов лицензий; (5) нет тестов smoke/режимов zip; (6) qdrant --version без таймаута; (7) путь vswhere зашит
Task 13: complete (commits a85091c..67de361, review clean)
Task 14: Ruling: minor 1, 2, 3, 6 из Task 13 включить в Task 14 (они влияют на корректность релиза и CI) — цена: Task 14 больше
Task 14: Ruling: шаг 2 (push ветки и тег v0.0.1-m0) исполнитель НЕ выполняет — внешнее действие, спросить владельца в конце — цена: CI не проверен до согласия
Task 14: dispatched (base 67de361, implementer sonnet)
Task 14: implementer DONE_WITH_CONCERNS (4a20a47, 2b705c1); reusable package.yml; actionlint чисто; push не выполнялся; review dispatched
Task 14: review 1 — Approved, но Important (наследие Task 13): zam-server.exe/zam.exe динамически импортируют VCRUNTIME140 — на чистой Windows не запустятся, CI-smoke не ловит
Task 14: Ruling: [target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"] для наших бинарей; smoke на Windows проверяет отсутствие VCRUNTIME/MSVCP в импортах zam-server.exe и zam.exe (dumpbin через vswhere); vcruntime140.dll остаётся у qdrant — цена: чуть больше бинарь
Task 14: Ruling: в тот же раунд — minor 1 (литералы "\n"), 4 (версия workspace = 0.0.1-m0, release проверяет тег == версия workspace), 5 (ZAM_GIT_SHA обрезать до 12), 6 (cargo test в release до publish), 7 (concurrency для ci с cancel-in-progress на PR) — цена: нет
Task 14: minor (deferred): ref-ссылки в CHANGELOG; write_sha256sums пути вне dist; кэш rust-cache чистит target/deps-cache (порядок post-шагов) и лимит 10 ГБ; ping в тесте таймаута; повтор релиза создаёт второй draft
Task 14: fix round 1/5 (6 addressed, 0 open; commits 2b705c1..59ab614)
Task 14: minor (deferred): нумерация шагов smoke (шаг 9 выполняется первым); многострочный литерал STUB_INDEX в build.rs
Task 14: complete (commits 67de361..59ab614, review clean)
Final review: dispatched (opus, range 4913ebc..59ab614)
Final review: With fixes — Critical1 ZAM_GIT_SHA из env CI роняет zam-server (deny_unknown_fields) → smoke в CI упадёт на обеих ОС; Important: 2 нет сквозной проверки CLAUDE.md→zam status; 3 WebUI не проверен в браузере; 4 «Копировать» не работает по HTTP в LAN; 5 echo|hash-password опасно на Windows; 6 тулчейн не закреплён; 7 THIRD-PARTY-WEB без текстов лицензий
Final: Ruling: сборочные переменные (ZAM_GIT_SHA, ZAM_WEB_REBUILD, ZAM_WEB_INDEX, ZAM_VCRUNTIME_DLL) — в RESERVED_ENV; ZAM_GIT_SHA в workflow только на шагах build; smoke чистит ZAM_* дочернего процесса кроме ZAM_ADMIN_PASSWORD — двойная защита — цена: нет
Final: Ruling: браузерная проверка WebUI — Playwright с установленным Microsoft Edge (channel msedge, без скачивания браузеров на диск C:) по LAN-IP машины, локальный скрипт в web/e2e, в CI не включать в M0 — цена: e2e только локально
Final: Ruling: порядок поиска в каталоге CLAUDE.local.md → CLAUDE.md → AGENTS.md (local — место для токена) — цена: изменение поведения до релиза, допустимо
Final: Ruling: восстановление первого токена — только если у админа нет ни одного токена вообще (а не «нет неотозванных») — «отозвал — значит отозвал» — цена: при отзыве всех токенов вход только через WebUI (админ-пароль), что допустимо
Final: fix wave dispatched (opus)
Final: fix wave DONE (cec6010..cbe6135); e2e Edge по LAN-IP 1 passed; SMOKE OK с ZAM_GIT_SHA; scoped re-review dispatched
Final: re-review — все 15 пунктов ADDRESSED; новые Minor: (1) зависший zam-server при сбое global-setup e2e; (2) Playwright пишет профиль/артефакты в %TEMP% (нарушение правила владельца); (3) ZAM_E2E_SERVER не в RESERVED_ENV; (4) e2e перезаписывает буфер обмена хоста, нумерация шагов smoke
Final: Ruling: пункты 1–3 исправить отдельной маленькой правкой, несмотря на «одну волну» — п.2 нарушает явное правило владельца о временных файлах, которое приоритетнее процесса — цена: ещё один коммит
Final: Ruling: п.4 — parked (e2e — локальный ручной инструмент; буфер — ожидаемое следствие проверки копирования) — цена: перезаписанный буфер при запуске e2e
Final: post-review fix 1add650 (e2e TMP внутри проекта, kill при сбое setup, E2E_SERVER) — проверен контроллером по diff; final review clean
