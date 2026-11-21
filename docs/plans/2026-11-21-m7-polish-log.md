# M7. Полировка — журнал этапа

Спецификация — `docs/specs/2026-11-21-m7-polish.md`. Общая задача в памяти —
`k:01M7ZA6HTREBE925JAGBDHM8Q9`. Указание владельца 2026-11-21: «Делай весь этап».

Журнал ведётся по волнам: ход задач, решения исполнителей и координатора, прогоны, итог волны.
Вопросы владельцу копятся в разделе «Уточнить у владельца» и отдаются списком по завершении
волны (решение владельца 2026-10-20 «не тормози… отмечай что уточнить по завершении»).

## Состав (2026-11-21)

Исходные данные: 45 открытых задач в памяти (из них 2 — владельца: «UI на мобильных
устройствах», «число записей в разделах»), строка M7 ТЗ §19, решения владельца 2026-11-21,
предложения ZeroAgent CR-21…26, открытое замечание обкатки 37. Волны и состав — спецификация §2.

Снято при составлении: пароль тестового стенда через переменную окружения
(`k:01M7GAQQMHEQPSE6RH4S07Z4XF`, владелец 2026-11-14 «оставь как есть»).

## Волна M7.1 «Опора и безопасность»

Ветка `m7.1-foundation` от `main` `7af69d7`.

### M7.1 · Задача 1 — ONNX Runtime из NuGet

Спецификация §4.1; проверка источника — `i:01M7ZB9N2QER1R4NA0XA2ZBK6E` (материалы исследования —
вне git). Ветка задачи `m7.1-t1`.

- **Манифесты.** `deploy/onnxruntime.toml` и `deploy/onnxruntime-gpu.toml` — пакеты NuGet 1.31.0
  `Microsoft.ML.OnnxRuntime` (CPU, один пакет на обе платформы: записи ссылаются на него, кэш по
  sha256 общий), `.Gpu.Windows`, `.Gpu.Linux`; адреса `api.nuget.org/v3-flatcontainer/…/….nupkg`,
  sha256 и `size` пакета, пути `runtimes/<rid>/native/…`, без `rename`. sha256 и размеры
  исполнитель пересчитал по скачанным nupkg — совпали с исследованием: CPU `7e35fdb4…` / 159 626 085,
  GPU Windows `6ef71832…` / 160 447 640, GPU Linux `7cd256a2…` / 244 928 869. В шапках —
  «файлы побайтово равны архивам GitHub v1.31.0, проверено 2026-11-21», без ссылок на `.tmp/`.
- **`zam-fetch`.** `format_of`: `.nupkg` (любой регистр) → zip. `download(url, dest, size)` —
  предел длины тела и точная проверка размера в одной функции: прежние `download_limited` и
  `check_size` после неё слиты, `fetch_file` и `install_archive` зовут её же (поведение то же).
- **`xtask fetch-deps` проверяет `size`.** При загрузке — `download_part(…, entry.size)`: длиннее —
  обрыв, короче — ошибка, `*.part` удаляется, в кэш ничего не попадает. У архива из `deps-cache` —
  размер до хэша и распаковки: «<имя>: файл кэша <путь> — N байт, а size в манифесте — M;
  исправьте size в манифесте или удалите файл кэша и повторите».
- **`xtask fetch-deps --prune-cache`.** `fetch::manifest_hashes(deploy/)` — sha256 всех
  `deploy/*.toml`; `fetch::prune_cache(target/deps-cache, keep)` удаляет файлы с другим именем
  (прежние версии и `*.part`), подкаталоги не трогает, нет каталога — `(0, 0)`. Печать —
  «<кэш>: удалено файлов — N (X МБ)».
- **Лицензия.** `deploy/licenses/ONNXRUNTIME-THIRD-PARTY-NOTICES.txt` — `ThirdPartyNotices.txt`
  пакета: во всех трёх nupkg один и тот же файл (344 457 байт, CRLF, sha256 `c53a7650…`), он же — в
  прежнем архиве GitHub `onnxruntime-win-x64-1.31.0.zip`. В репозитории — дословно с LF: 338 088
  байт, sha256 `143764b9…`. Каталог `deploy/licenses/` копируется в `LICENSES/` комплекта целиком,
  поэтому кода упаковки правка не требует.
- **CI.** `package.yml`: ключ кэша `deps-v2-<платформа>-<хэш манифестов>` и `restore-keys`
  `deps-v2-<платформа>-` (первый прогон — без кэша, §4.1 п. 6); шаг «чистка deps-cache»
  (`cargo xtask fetch-deps --prune-cache`) сразу после `fetch-deps` — кэш сохраняет post-шаг
  `actions/cache` в конце задачи. Прогон CI — координатор.
- **Документы.** `deploy/cuda.toml` — строка об источнике ORT GPU (NuGet, те же байты, набор и
  проверка импортов в силе); README — «Лицензия» и таблица команд xtask; CHANGELOG
  `[Unreleased]`.
- **[решение исполнителя, задача 1]** `DepManifest::validate` отвергает адрес с неизвестным
  форматом архива. Ловушка исследования («неизвестный формат архива» у `.nupkg») иначе
  обнаруживалась только после скачивания 160–245 МБ; сейчас — до загрузки, и
  `real_manifests_are_valid` ловит её без сети. Тест — `validation_rejects_bad_hash_and_url`
  (`.exe` — ошибка, `.nupkg` — ок).
- **[решение исполнителя, задача 1]** `manifest_hashes` собирает значения ключей `sha256` на любой
  глубине всех `deploy/*.toml` (зависимости, `cuda.toml`, `models.toml`; `config.example.toml`
  разбирается, но `sha256` в нём нет), а не разбирает три схемы по отдельности. Так выполняется
  интерфейс «ни в одном манифесте `deploy/*.toml`», и новый манифест не требует правки чистки.
  Нечитаемый TOML — ошибка: по неполному списку чистка удалила бы нужный архив.
- **[решение исполнителя, задача 1]** `--prune-cache` — отдельный режим без загрузки: clap
  запрещает сочетание с `--platform`, `--print-hashes`, `--flavor`. Чистка идёт после проверки
  манифестов — при неверном манифесте кэш не трогается.
- **[решение исполнителя, задача 1]** `--print-hashes` печатает и размер: под комментарием
  `# <имя>.toml [platform.<плат>]` строки `sha256 = "<hex>"` и `size = N` — TOML, вставляется как
  есть (круг правок 1). Без этого обновление версии давало бы sha256 без `size`, и
  `fetch-deps` падал бы на прежнем `size`. Размер при `--print-hashes` не проверяется: в
  манифесте он может быть от прежней версии.
- **[решение исполнителя, задача 1]** размер файла кэша проверяется до хэша, как в `fetch_file`
  («чужой файл не хэшируется»). Поэтому сообщение называет обе причины — неверный `size` в
  манифесте или испорченный файл кэша.
- **[решение исполнителя, задача 1]** `ci.yml` и `release.yml` не менялись: они кэшируют
  `target/deps` (распакованное, заменяется по штампу `<имя>.sha256`), а не `deps-cache`, и прежние
  версии там не копятся. Первый прогон после смены манифестов перекачает ORT с NuGet по
  несовпавшему штампу — это тоже проверка источника.
- **[решение исполнителя, задача 1]** smoke (`docs_layout`) лицензии ORT не проверяет, как и до
  задачи: состав `LICENSES/` закреплён тестом упаковки `licenses_include_ort_third_party`
  (настоящий `deploy/`). Если нужна проверка и на готовом архиве CI — одна строка в `smoke.rs`.
- **Для задачи 10 (ТЗ v1.27):** §16.3, пункты «Откуда берутся зависимости» (ORT из NuGet,
  проверка `size`, `--prune-cache`) и «Лицензии» (в `LICENSES/` комплекта сервера добавлен
  `ONNXRUNTIME-THIRD-PARTY-NOTICES.txt`; было 13 файлов в dev).

#### TDD

RED (`CARGO_TARGET_DIR=target\m71-t1`, `-j 2`):

- `cargo test -p zam-fetch --lib -- format_of_nupkg_is_zip extract_from_nupkg
  validation_rejects_bad_hash_and_url` — 3 FAILED: `assertion failed: matches!(format_of(u),
  Some(Format::Zip))`; `Invalid("неизвестный формат архива: u.nupkg")`; `unwrap_err()` на `Ok`
  (адрес `.exe` проходит проверку).
- `cargo test -p xtask --bin xtask -- fetch_rejects_size_mismatch
  fetch_download_rejects_size_mismatch licenses_include_ort_third_party real_manifests_are_valid`
  — 4 FAILED: архив из кэша с чужим `size` распакован (`unwrap_err()` на `Ok(true)`); загрузка
  длиннее `size` докачана, легла в кэш и упала только на распаковке — «неизвестный формат архива:
  http://127.0.0.1:…/a.nupkg» (та самая ловушка); в архиве комплекта нет
  `LICENSES/ONNXRUNTIME-THIRD-PARTY-NOTICES.txt`; `onnxruntime [linux-x64]:
  https://github.com/microsoft/onnxruntime/…`.
- `cargo test -p xtask --bin xtask -- prune_cache_keeps_manifest_hashes` — `E0425: cannot find
  function manifest_hashes` / `prune_cache`.

GREEN — те же команды: 3 и 5 ok.

#### Прогоны

| Проверка | Итог |
|---|---|
| `cargo test -j 2 -p zam-fetch -p xtask` | xtask 240 ok (было 235), `tests/ab.rs` 3 ok, `tests/eval_cli.rs` 5 ok; zam-fetch 32 ok, 1 ignored (было 30) |
| `cargo fmt -p zam-fetch -p xtask --check` | чисто |
| `cargo clippy -p zam-fetch -p xtask --all-targets -- -D warnings` | чисто |
| `cargo xtask fetch-deps --platform windows-x64` и `--platform linux-x64` в worktree: `target/deps-cache` создан пустым, в него скопированы три nupkg под именами sha256 (вместо загрузки с NuGet) и архивы Qdrant/pdfium Linux из основного кэша; прокси `HTTP(S)_PROXY=http://127.0.0.1:9` — сети нет | обе платформы «из кэша», sha256 и размеры всех 8 извлечённых файлов (`onnxruntime{,-gpu}` × 2 платформы) равны файлам основного `target/deps/<платформа>/onnxruntime*` из архивов GitHub |
| `cargo xtask fetch-deps --prune-cache` (в кэш добавлены «прежний» архив 2 МБ и `4242.part`) | «удалено файлов — 2 (2.5 МБ)», 5 архивов манифестов на месте; повтор — 0; с `--platform` — отказ clap «cannot be used with» |

Данные проверки (кэш ~600 МБ, Linux и GPU в `target/deps` worktree) после прогона удалены;
`target/deps/windows-x64/onnxruntime` worktree распакован заново из nupkg (те же байты, штамп —
новый sha256).

#### Круг правок 1 (ревью задачи 1: Approved, четыре Minor)

1. Загрузка длиннее `size` обрывается после `size + 1` байта, и текст «получено N» вводил в
   заблуждение: N всегда было `size + 1`. Теперь это отдельный вариант
   `FetchError::SizeExceeded { what, max }` с текстом «ожидалось N байт, получено больше N байт
   (загрузка оборвана)». `SizeMismatch` остался для точного размера после загрузки и для файла на
   месте. RED: тело 5 байт при `size = 3` давало «получено 4», тело 10 байт при `size = 4` —
   «получено 5». Тесты — `fetch_file_checks_size_and_removes_stale_parts` (вариант и текст) и
   `fetch_download_rejects_size_mismatch` («больше 4 байт»).
2. Список форматов в ошибке `validate` больше не захардкожен: `download::ARCHIVE_SUFFIXES`
   (окончание → формат) — один источник и для `format_of`, и для текста «допустимы: .zip, .nupkg,
   …». `validation_rejects_bad_hash_and_url` проверяет, что в тексте есть каждое окончание.
3. `--print-hashes` печатал `sha256 = "…" size = N` одной строкой — это не TOML. Теперь
   `print_hashes_lines` выводит комментарий `# <имя>.toml [platform.<плат>]` и две строки TOML.
   Тест `print_hashes_lines_are_toml` разбирает вывод как TOML (RED — `E0425`).
4. `real_manifests_are_valid` проверяет ORT на уже загруженных манифестах, без повторной загрузки.

Прогон: `cargo test -j 2 -p zam-fetch -p xtask` — xtask 241 ok (+1), `tests/ab.rs` 3 ok,
`tests/eval_cli.rs` 5 ok, zam-fetch 32 ok, 1 ignored; `cargo fmt --check` и `cargo clippy
--all-targets -- -D warnings` обоих крейтов — чисто.

### M7.1 · Задача 2 — Ключ доступа по адресу сервера

Спецификация §4.4, задача памяти `k:01M7Z4T2T4E4ERXZVBY8VBYTPT` (решение владельца
`i:01M7Z4SBS0EJA816CCYPKPA4TA`). Ветка `m7.1-t2`.

Сделано: `locate::normalize_url`; `HomeConfig.servers` (`[servers."<адрес>"] token`);
`Source::HomeServer(путь, адрес)`; шаг 4 `locate::resolve` берёт токен только для найденного
адреса, `Resolved.other_server_keys` — в конфиге есть токены других адресов; `plan_home` пишет
`[servers."<адрес>"]`, переводит прежний формат; `cmd::missing_token` — общий отказ с подсказкой
`zam init --url <адрес> --token <токен>` для `connect`, `zam status`, `zam init` и журнала хуков.

Решения:
- **[решение исполнителя, задача 2]** Код отказа. Токена для адреса нет, а в домашнем конфиге есть
  токены других адресов (или прежний `token` без `url`) — код 1 (настройка) до обращения к сети:
  так требует тест задания `missing_token_for_url_hint`. Токена нет вовсе — код 3, как раньше
  (`missing_token_exit_3_prints_server`, `notes.rs` «нет токена → 3» не меняются), но с той же
  подсказкой. `zam status` в первом случае отказывает до `/health`.
- ~~[решение исполнителя, задача 2] `token` верхнего уровня без `url` — ничей, не отправляется~~ —
  заменено решением координатора (ниже).
- ~~[решение исполнителя, задача 2] `url` верхнего уровня пишет только `--url-to home`~~ —
  заменено решением координатора (ниже).
- **[решение координатора, задача 2]** §4.4 п. 1 «прежние url/token читаются как раньше» —
  поведение 0.0.13-m6.6 для существующих конфигов не ломается. Ключ без адреса (`token` без `url`,
  его писал `--url-to local`) уходит на любой адрес, пока записей `[servers]` с токеном нет (пустая
  таблица `[servers]` не в счёт); с ними — не используется. `zam status` в обоих случаях пишет
  примечание «ключ без адреса (<маска>) … — привяжите: zam init --url <адрес> --token …»
  (`Resolved.unbound_token`, `cmd::unbound_token_note`). `zam init --token T`: ключ без адреса,
  равный `T`, привязывается к адресу и уходит с верхнего уровня; другой остаётся как есть, вывод
  предупреждает, что он больше не используется, и печатает команду привязки
  (`HomePlan::Write.unbound_left`). Режим `block` пишет `url` верхнего уровня, как раньше, если
  его нет; другой адрес не заменяется (`locate::TopUrl`: local — `Keep`, block — `IfAbsent`,
  home — `Set`).
- **[решение исполнителя, задача 2]** Рядом с оставшимся ключом без адреса `block` `url` не пишет:
  `token` + `url` верхнего уровня — прежняя пара «ключ этого адреса», и ключ без адреса молча стал
  бы ключом адреса проекта (а при следующей записи ушёл бы в `[servers."<url>"]` или потерялся бы
  за своей записью этого адреса). ~~У `--url-to home` ключ без адреса — ключ этого адреса,
  другой `--token` — замена~~ — заменено решением координатора, круг правок 1 (ниже).
- **[решение исполнителя, задача 2]** Тот же адрес, другой токен с `--token` — замена с прежним
  предупреждением («был другой токен — заменён»): §4.4 п. 3 убирает предупреждение о замене ключа
  *другого сервера*, а смена токена того же сервера — событие, о котором стоит сказать.
- ~~[решение исполнителя, задача 2] Значение адреса в `Resolved.url` не нормализуется дальше
  прежнего~~ — круг правок 1 (Minor 4, DRY): адрес поиска настроек, `url` верхнего уровня и ключи
  `[servers]` приводятся одним `normalize_url`. `plan_home_url` (`--url-to home`) сравнивает
  адреса после нормализации, как и ключи.
- Тесты `init_second_server_keeps_first`, `init_migrates_legacy_home_config`,
  `status_names_server_token_source` ходят на тестовые серверы `127.0.0.1:<порт>` (`zam init` и
  `zam status` обращаются к серверу), адреса RFC 5737 — в `missing_token_for_url_hint` и юнит-тестах
  `plan_home_keys_by_server`. В задании `cargo test -p zam-cli --lib locate`, но у `zam-cli` нет
  цели lib — юнит-тесты гоняются `--bin zam locate`.
- Изменены и прежние тесты формата записи (`init_creates_solution_block_hooks_mcp`,
  `init_bom_files_and_v1_token_without_flag`, `init_url_local_writes_claude_local_and_exclude`,
  `home_url_plan_keeps_token_and_refuses_other_url`): они проверяли `token`/`url` верхнего уровня
  после `zam init`. Тесты порядка поиска (`flag_beats_env_beats_file_beats_home` и др.) не менялись.

Прогоны (Windows, `CARGO_TARGET_DIR=target\m71-t2`, `-j 2`):
- RED: `cargo test -p zam-cli --bin zam locate` — 5 из 19 падают (`home_config_legacy_and_two_servers`:
  ключ `t1` вместо `t2`; `home_token_not_sent_to_other_server`: `t1` ушёл на `http://b:2`;
  `plan_home_keys_by_server`: `Conflict` для второго адреса; нормализация; `plan_home_url` —
  `Conflict` на `HTTP://…/`); `--test status` — 2 из 2 новых (код 2 «недоступен» вместо 1; код 3
  «токен не задан» при ключе в `[servers]`); `--test init` — 5 из 5 (нет `[servers]`;
  «был другой токен — заменён» у второго сервера).
- GREEN: `--bin zam locate` 19/19; `--test init` 30/30; `--test status` 12/12; `--test hooks`
  113/113; `--test mcp_bind` 4/4; `cargo clippy -p zam-cli --all-targets -- -D warnings` и
  `cargo fmt -p zam-cli -- --check` — чисто.
- Решения координатора (второй коммит). Новые тесты: `unbound_token_used_until_server_keys`
  (`locate.rs`), `status_uses_unbound_token_with_note` (`status.rs`),
  `init_second_server_leaves_unbound_token` (`init.rs`); `plan_home_keys_by_server` — режимы
  `TopUrl` и ключ без адреса; `init_creates_solution_block_hooks_mcp`,
  `init_bom_files_and_v1_token_without_flag`, `init_second_server_keeps_first` — снова `url` у
  block. Код написан до тестов, поэтому RED снят обратной мутацией (поведение первого коммита:
  ключ без адреса не берётся, block → `TopUrl::Keep`): `unbound_token_used_until_server_keys`
  (`None` вместо токена), `status_uses_unbound_token_with_note` (код 1 вместо 0), три теста init
  (нет `url`) падают; мутация убрана.

Круг правок 1 (ревью «Needs fixes», третий коммит):
- **[решение координатора, задача 2, Important 1]** `--url-to home` при ключе без адреса T1 (оба
  пути — с записью токена и без неё: `ZAM_TOKEN`, перенос из блока v1): токен, который пишется
  или найден для адреса, равен T1 — T1 переносится в `[servers."<адрес>"]`, пишется `url`,
  `token` верхнего уровня удаляется; иначе (или у адреса уже свой ключ) — отказ (код 1) до любой
  записи: «в домашнем конфиге есть ключ без адреса (<маска>) — привяжите его к его серверу: zam
  init --url <адрес> --token … — или удалите строку token, затем повторите». `plan_home(TopUrl::Set)`
  и `plan_home_url(home, url, token)`; `init` передаёт токен адреса (`--token`, перенос из блока или
  найденный).
- Minor: `zam status --json` при отказе «токены только других серверов» — JSON-отчёт с `error`;
  предупреждение о неиспользуемом ключе без адреса — и у `zam init` без `--token`, и при повторе без
  записи (`locate::unused_unbound_token`); псевдонимы одного сервера — разные адреса (CHANGELOG,
  руководство); `norm_url`, `home_url` и ручные обрезки заменены `normalize_url` (общий разбор
  файла — `home_doc`); тест строки `missing_token` в `hooks.log` (`missing_token_for_url_logged`);
  комментарий `TopUrl::IfAbsent` (0.0.13-m6.6 перезаписывал `url`).

### M7.1 · Задача 4 — Строка очереди идей и мелочи подсказок

Спецификация §4.5 (решение владельца `i:01M7Z53BM6EGK9Z9W5G18NYGAM`). Ветка `m7.1-t4`.

Сделано:
- **Строка очереди — одна за эпоху.** `ideas::review_queue_line` (бывш. `review_queue_head` +
  `QueueHead::line`) читает до поиска `zam_store::review_queue_head` (длина очереди, первая идея,
  время последней ревизии идей очереди — одним чтением) и `zam_store::review_queue_shown` (показ
  очереди сессии в эпоху); текст — `REVIEW_QUEUE_LINE` / `review_queue_summary(first, total)`:
  «ZeroAIMemory: идей ждут разбора: N, первая «…» — список: `zam idea queue`», не длиннее 60
  токенов (`REVIEW_QUEUE_TOKENS`; длинный заголовок укорачивается по токенам с «…»).
- **Отметка показа** — `session_seen` с частью `REVIEW_QUEUE_PART` (`review_queue`): цель —
  первая идея очереди, ревизия — число идей в очереди при показе (`ideas::queue_mark`). Ставит её
  учёт ответа (`recall::seen_items`) только когда строка вошла в текст: снятая пределом вывода
  канала, повтором ссылки, контрольной группой, `record: false` — без отметки. Раздел
  `### Идеи на разбор` дайджеста ставит ту же отметку (длина очереди его раздела) — показ в эпохе.
  Подсказка-идея по близости к запросу не меняется: строка очереди отметку подсказки
  (`RECALL_PART`) своей первой идее не ставит.
- Слова-реплики: +6 форм (`PROMPT_REPLY_WORDS` 71). `broad_alias`: аббревиатура (`is_acronym`) не
  `Short`. `memory_vault delete` по имени — `ByName::Exact` (`get`, `rename`, `move` —
  `ByName::Loose`, как раньше).
- `zam-store`: `review_queue_unseen` и `seen_entry_revs` (строка на идею M6.6) удалены — других
  потребителей нет; их тесты заменены тестами новых функций. Миграций нет.
- Флаг `/health.features` `idea_queue_line`, `docs/api-compat.md` (таблица флагов, «Подсказки к
  запросу», «Разбор идей», `term_alias_broad`); протокол `zam guide` ru/en (строка очереди,
  `term_alias_broad`, `delete` по точному имени); CHANGELOG.

Решения:
- **[решение исполнителя, задача 4]** «Рост очереди» (§4.5 п. 1, «пришла идея, которой не было
  при показе») — строка повторяется, если очередь длиннее показанной **или** у идеи очереди
  ревизия новее показа (`changed_at > shown.at`). Одного числа мало: агент разбирает очередь
  (3 → 0), человек пишет идею (0 → 1 < 3) — по числу она ждала бы сжатия, вопреки решению
  координатора; тест `queue_line_new_idea_after_reviews`. По ревизии строка возвращается и для
  идеи, вернувшейся на разбор (`returned` → `new`, снятый вопрос — как в M6.6, тест
  `prompt_review_queue_new_revision`), и после правки идеи очереди человеком (редко). Сравнение
  числа остаётся: идея, записанная между чтением очереди и учётом ответа, ревизией старше показа
  не будет, а очередь — длиннее. В отметке — наибольшее число за эпоху (как у любой отметки
  `session_seen`, upsert `max(rev)`), время — последнего показа.
- **[решение исполнителя, задача 4]** Строка — «первая» по порядку очереди (`high`, затем самые
  старые), без пропуска идей, которые ответ несёт подсказкой-идеей (M6.6 так делал для строки на
  идею). Если первая идея уже идёт подсказкой «к задаче подходит идея на разбор» (та зовёт
  разобрать), строку снимает обычный фильтр повтора ссылки, отметки нет — строка приходит со
  следующим запросом. Так в `items` нет двух одинаковых ссылок.
- **[решение исполнителя, задача 4]** «ru/en» (§4.5 п. 1) — текст строки русский, как все строки
  `/recall` (языка у подсказок нет); английский перевод — в `guide.en.md`, как у соседних строк.
  Тест `agent_setup` сверяет обе версии протокола с `REVIEW_QUEUE_LINE`.
- **[решение исполнителя, задача 4]** Флаг `idea_queue_line` — по общему ограничению этапа
  («изменение выдачи агенту — признак в `/health.features`»), хотя M6.6 такую строку вводил без
  флага: клиенту, который разбирал «новая идея на разбор … w:<id>», нужен способ узнать формат.
- **[решение исполнителя, задача 4]** `term_alias_broad`: аббревиатура не «короткая», но проверка
  служебного слова для неё остаётся (§4.5 п. 3 говорит только о коротких). Следствие: `OK` теперь
  без предупреждения (две заглавные), тест `glossary::term_alias_broad_warning_on_term_write`
  проверяет `Ok`.
- **[решение исполнителя, задача 4]** 404 у `delete` с именем в другом регистре — список
  `- v:<id> «<точное имя>» · <область>` (без значений) и «удаление — только по точному имени
  (регистр важен) или v:<id>».
- Случаи eval: `p-54`–`p-57` (одна-две реплики) молчали и в базе — короткий запрос M6.6 без
  лексической находки; поэтому добавлены `p-58`, `p-59` (три реплики — у базы «длинный» запрос с
  поиском). Происхождение `m71` в `xtask` (`SOURCES`), в решающие части сеток не входит.

**Eval `cargo xtask eval --hints`** (CPU, `bge-m3-int8@a206e10e`, `--set recall.timeout_ms=60000`
в обоих плечах). База — бинарники дерева `9074fe98` (`git archive HEAD`, сборка
`target\m71-t4-base`; поведение = `m7.1-foundation` до задачи, по подсказкам = `main`), ветка —
release-сборка рабочего дерева задачи (`target\m71-t4-rel`); набор, корпус и `xtask` — одни
(189 случаев, отпечатки набора `662239d774c22cf6`, корпуса `34aafe1af1e9c1ca`). Отчёты —
`.tmp/m71/t4-logs/eval/` (вне git), сравнение по случаям — `.tmp/m71/t4-logs/old_cases.py`.

| | показано | нужных | лишних | hint_precision | hint_recall | silence_rate |
|---|---|---|---|---|---|---|
| прежние 183 — база / ветка | 160 / 160 | 93 / 93 | 67 / 67 | 0,581 / 0,581 | 0,838 / 0,838 | 0,806 / 0,806 (75/93) |
| `prompt`, прежние 53 — база / ветка | 58 / 58 | 31 / 31 | 27 / 27 | 0,534 / 0,534 | 0,816 / 0,816 | 0,750 / 0,750 |
| `post_tool` 112, `pre_tool` 18 | без изменений | | | | | |
| новые `m71` (6) — база | 3 | 0 | 3 | 0,000 | — | 0,833 (5/6) |
| новые `m71` (6) — ветка | 0 | 0 | 0 | — | — | **1,000 (6/6)** |
| все 189 — база / ветка | 163 / 160 | 93 / 93 | 70 / 67 | 0,571 / 0,581 | 0,838 / 0,838 | 0,808 / 0,818 |

На прежних случаях показанное совпало по каждому случаю (нужных не меньше, лишних не больше),
кроме порядка у `p-34` (те же две подсказки, как в M6.6). Новые: у базы `p-58` «ладно,
продолжаем, приступай» — три лишних (`p18-t`, `p19-h`, `rule:commits`), у ветки — тишина;
`p-54`–`p-57`, `p-59` молчат в обоих. Модель калибровки `prompt` совпала с `/recall`: база —
100 %, ветка — 98 % (1 из 59 — `p-58`: `xtask` обоих прогонов собран до правки реплик; `xtask`
ветки с новым `PROMPT_REPLY_WORDS` совпадёт полностью). Задержки в отчётах несравнимы (машина
под сборками и тестами).

Прогоны (Windows, `CARGO_TARGET_DIR=target\m71-t4`, `-j 2`):
- RED: `zam-search --lib reply_words_forms_silent` — «ладно» с предметным словом;
  `zam-store --lib broad_alias_acronym_not_short` — `UI` → `Some(Short)`; `zam-store --test
  sessions --test ideas_review` — нет `review_queue_shown`, `REVIEW_QUEUE_PART`,
  `review_queue_head` (E0425/E0432); `zam-server --test ideas_review_recall -- queue_line_` — 3
  новых падают (строка «новая идея на разбор … (ещё 19)» вместо одной строки очереди; «ровно в
  одном ответе» — `[]` вместо `[0]`: строки «идей ждут разбора» нет); `--test mcp --
  vault_delete_exact_name_only` (после правки очереди, до правки `tools.rs`) — `delete
  name=deploy-key` удалил `Deploy-Key`.
- Обратные мутации (тесты, дописанные к реализации): без сравнения ревизии с показом падают
  `queue_line_new_idea_after_reviews` и `prompt_review_queue_new_revision`; без предела токенов —
  `queue_line_long_title_within_tokens`. Мутации убраны.
- GREEN: `zam-search --lib` 45 ok; `zam-store --lib` 116 ok, 1 ignored; `zam-store --test sessions`
  18 ok, `--test ideas_review` 13 ok; `zam-server --test ideas_review_recall` 31 ok (+6: три теста
  задания, `queue_line_new_idea_after_reviews`, `queue_line_long_title_within_tokens`, — и прежние
  с одной строкой вместо строки на идею), `--test mcp` 77 ok, `--test agent_setup` 18,
  `--test boot` 15, `--test glossary` 12, `--test m65_acceptance` 7, `--test ideas_recall` 30
  (2 ignored), `--test ideas_review` 9, `--test recall` 52 (1 ignored), `--test recall_frozen` 4,
  `--test sessions` 17, `--test m6_acceptance` 1 (2 ignored), `zam-server --lib` 307 (1 ignored);
  `xtask -- hints` 22 ok (`hint_set_is_valid` — 189 случаев). `cargo fmt --all --check`, `cargo
  clippy -p zam-search -p zam-store -p zam-server -p xtask --all-targets -- -D warnings`, `cargo
  clippy -p zam-server -- -D warnings` (без `test-hooks`) — чисто; `cargo doc --no-deps` —
  zam-server 60, zam-store 55 предупреждений (базовые линии).
- Сбой процесса: откат второй мутации `Copy-Item` сохранил время файла, и первый повтор набора
  собрал старый артефакт (ложное падение `queue_line_long_title_within_tokens`) — ловушка
  `i:01M7GPFT7AEP49CA87PS5R44P0`; файл «тронут», набор повторён целиком, числа выше — из повтора.

## Уточнить у владельца

1. **Коммит `959b1ed` «Docs» (2026-11-21 18:11) на ветке `m7.1-foundation`** — ранняя версия
   трёх документов M7 (спецификация, план, журнал), закоммичена и отправлена в
   `origin/m7.1-foundation` не координатором (автор — тот же профиль git). Если это ваш
   инструмент (IDE, автосохранение с коммитом) — его стоит выключить на время работы агента:
   параллельные коммиты в рабочем каталоге могут захватить чужие правки. История не
   переписывалась, работа идёт поверх.
