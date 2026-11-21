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

## Уточнить у владельца

1. **Коммит `959b1ed` «Docs» (2026-11-21 18:11) на ветке `m7.1-foundation`** — ранняя версия
   трёх документов M7 (спецификация, план, журнал), закоммичена и отправлена в
   `origin/m7.1-foundation` не координатором (автор — тот же профиль git). Если это ваш
   инструмент (IDE, автосохранение с коммитом) — его стоит выключить на время работы агента:
   параллельные коммиты в рабочем каталоге могут захватить чужие правки. История не
   переписывалась, работа идёт поверх.
