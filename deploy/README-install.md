# ZeroAIMemory: установка

Комплект содержит сервер `zam-server`, клиент `zam`, библиотеки ONNX Runtime и pdfium,
Qdrant (`qdrant/<версия>/`), пример конфигурации `config.example.toml` и лицензии (`LICENSES/`).
Поддерживаются Windows x64 и Linux x64.

## Порты и данные

| Что | Значение по умолчанию |
|---|---|
| Сервер (WebUI и API) | `0.0.0.0:7700` (`[server] bind`) |
| Qdrant HTTP / gRPC (запускается сервером, начиная с этапа M2) | `6333` / `6334` (`[qdrant]`) |
| Каталог данных | `./data` (`data_dir`), относительный путь считается от текущего каталога |

В каталоге данных лежат базы, индексы и файл блокировки; второй экземпляр сервера на тот же
каталог не запустится. Делайте резервные копии каталога при остановленном сервере.

## Первый запуск

Администратор задаётся не в базе данных, а в конфигурации. Два варианта:

1. Хэш пароля в `config.toml` (рекомендуется). Запустите команду без аргументов и введите
   пароль в ответ на приглашение (ввод не отображается и не попадает в историю оболочки):

   ```
   zam-server hash-password
   ```

   Результат (`$argon2id$...`) вставьте в `[admin] password_hash`. Не передавайте пароль
   через `echo ... |`: в Windows PowerShell 5.1 конвейер меняет кодировку не-ASCII символов,
   `cmd` добавляет к паролю пробел перед `|`, а сама команда остаётся в истории.
   Для скриптов в Linux — чтение без отображения:

   ```
   read -rs -p 'Пароль: ' PW && echo >&2 && printf '%s\n' "$PW" | zam-server hash-password; unset PW
   ```
2. Переменная окружения `ZAM_ADMIN_PASSWORD` (пароль хэшируется в памяти; удобно для проверки).

Скопируйте `config.example.toml` в `config.toml`, при необходимости измените значения и запустите:

```
zam-server serve --config config.toml
```

Параметры `--bind` и `--data-dir` переопределяют конфигурацию; переменные окружения
`ZAM_<РАЗДЕЛ>__<КЛЮЧ>` (например, `ZAM_SERVER__BIND`) тоже.

### Первый токен администратора

При самом первом старте сервер печатает в stderr (не в журнал) рамку:

```
==== ZeroAIMemory: первый токен администратора (показывается один раз) ====
ZeroAIMemoryToken=<токен>
==========================================================================
```

Сохраните токен: повторно он не показывается. Новые токены выпускаются в WebUI
(`http://<хост>:7700/`, вход администратора) или через админ-API.

Проверка подключения клиентом:

```
zam status --url http://127.0.0.1:7700 --token <токен>
```

Коды выхода `zam status`: 0 — всё в порядке, 1 — ошибка конфигурации,
2 — сервер недоступен, 3 — токен отклонён.

Без флагов `zam` ищет адрес и токен по порядку: `--url`/`--token`, переменные `ZAM_URL`/`ZAM_TOKEN`,
блок в файлах проекта от текущего каталога вверх (ближайший каталог важнее; внутри каталога —
`CLAUDE.local.md`, затем `CLAUDE.md`, затем `AGENTS.md`), затем `~/.zeroaimemory/config.toml`.
Адрес и токен ищутся независимо. Блок в `CLAUDE.md`:

```
<!-- zeroaimemory:begin v1 -->
ZeroAIMemoryUrl=http://<хост>:7700
ZeroAIMemoryToken=<токен>
<!-- zeroaimemory:end -->
```

Личный токен удобнее держать в `CLAUDE.local.md` (обычно не коммитится).

## Работа с заметками из CLI

Адрес и токен `zam` находит так же, как `zam status`; у каждой команды есть флаги `--url`,
`--token` и `--json` (вывод в JSON). Токен никогда не печатается.

**Цель** — `[пространство:]путь`: пространство `knowledge`, `rules`, `human`, `solution`
или GUID решения. Без префикса — решение из ближайшего вверх `.zeroaimemory/solution.toml`
(поля `id` и `name`), а если его нет — `knowledge`. В `read`, `history`, `write` и `attach`
вместо пути можно передать id узла (26 символов, как печатает `zam`). Пути с пробелами
и кириллицей берите в кавычки.

```
# дерево с краткими сводками (L0); --depth от 1 до 5
zam tree knowledge:
zam tree "knowledge:Инструменты" --depth 2

# запись: из stdin или --file; печатает <id>@<ревизия>
echo "# Bash скрипты" | zam write "knowledge:Инструменты/Bash скрипты" -m "первая версия"
zam write "knowledge:Инструменты/Bash скрипты" --file note.md --base-rev 3 --l0 "Полезные скрипты"

# чтение: Markdown как сохранён (байты, переводы строк не меняются)
zam read "knowledge:Инструменты/Bash скрипты"
zam read "knowledge:Инструменты/Bash скрипты" --section "Установка > Linux"
zam read <id> --tier L1                          # L0 — сводка, L1 — оглавление
zam read knowledge:a knowledge:b                    # перед каждой — строка «--- цель ---»

# история: ревизии (rev, время, автор, машина, сообщение), дифф, откат
zam history "knowledge:Инструменты/Bash скрипты"
zam history "knowledge:Инструменты/Bash скрипты" --diff 1..3
zam history "knowledge:Инструменты/Bash скрипты" --revert 1

# поиск: Markdown, в области — текущее решение, Knowledge и Rules; --all — все решения
zam search "bash скрипты"
zam search docker --all --mode notes --top-k 5

# вложение: печатает ссылку _attachments/… для вставки в заметку
zam attach "knowledge:Инструменты/Bash скрипты" ./схема.png
```

`write` создаёт заметку, если её нет, и заменяет текст существующей с проверкой ревизии
(`--base-rev` или текущая). Если заметку успели изменить с другой машины, команда
печатает в stderr дифф от вашей базовой ревизии к текущей и завершается с кодом 4.
Перед первой записью в решение оно регистрируется на сервере по `solution.toml`.
Имя машины (`COMPUTERNAME` в Windows, `HOSTNAME` в Linux, если переменная экспортирована)
попадает в историю ревизий.

Коды выхода команд: 0 — успех, 1 — конфигурация (нет адреса, битый `solution.toml`),
2 — сервер недоступен, 3 — нет токена или он отклонён (в том числе нет прав на запись),
4 — конфликт ревизий, 5 — не найдено, 6 — неверный ввод (аргументы, путь, не UTF-8).

## Windows

1. Распакуйте архив `zam-server-<версия>-windows-x64-cpu.zip`, например в `C:\ZeroAIMemory`.
2. `zam-server.exe` и `zam.exe` собраны со статическим CRT, а рядом с `qdrant.exe` лежит app-local
   `vcruntime140.dll`: отдельная установка VC Redistributable не нужна.
3. Создайте `config.toml` и запустите `zam-server.exe serve --config config.toml`.
4. Для работы в фоне зарегистрируйте запуск в Планировщике заданий или через менеджер служб (например, NSSM).

Остановка: Ctrl+C в окне сервера.

## Linux

1. Распакуйте архив `zam-server-<версия>-linux-x64-cpu.tar.gz` в `/opt/zeroaimemory`
   (лишний верхний каталог архива отбрасывается):

   ```
   sudo install -d /opt/zeroaimemory
   sudo tar -xzf zam-server-<версия>-linux-x64-cpu.tar.gz --strip-components=1 -C /opt/zeroaimemory
   ```

   Исполняемые файлы: `/opt/zeroaimemory/zam-server`, `/opt/zeroaimemory/zam`,
   `/opt/zeroaimemory/qdrant/<версия>/qdrant`.
2. Создайте пользователя и каталоги:

   ```
   sudo useradd --system --home /var/lib/zeroaimemory --shell /usr/sbin/nologin zam
   sudo install -d -o zam -g zam /var/lib/zeroaimemory
   sudo install -d /etc/zeroaimemory
   sudo cp /opt/zeroaimemory/config.example.toml /etc/zeroaimemory/config.toml
   ```

   В `config.toml` укажите `data_dir = "/var/lib/zeroaimemory"` и `[admin] password_hash`
   (хэш: `/opt/zeroaimemory/zam-server hash-password`, пароль вводится по приглашению).
3. Установите юнит systemd и запустите службу:

   ```
   sudo cp /opt/zeroaimemory/systemd/zam-server.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now zam-server
   sudo journalctl -u zam-server -n 50      # первый токен в рамке (stderr)
   ```

   В юните `ReadWritePaths` должен совпадать с `data_dir`; остановка — `SIGTERM`
   (таймаут 20 с).
4. `zam-server` собран под `x86_64-unknown-linux-gnu` на Ubuntu 22.04 (glibc 2.35 и новее);
   `zam` — статический (musl).

## За обратным прокси (Traefik)

Сервер можно опубликовать по HTTPS под префиксом пути, например `https://example.com/memory`.

1. В `config.toml` сервера:

   ```
   [server]
   bind = "0.0.0.0:7700"
   public_url = "https://example.com/memory"
   base_path = "/memory"                 # префикс без завершающего "/"
   trusted_proxies = ["172.18.0.2"]      # адрес (или подсеть) Traefik, как его видит сервер
   ```

   `X-Forwarded-For` и `X-Forwarded-Proto` принимаются только от адресов из `trusted_proxies`:
   по ним определяются IP клиента (лимиты и аудит) и HTTPS. Если последний `X-Forwarded-Proto`
   от доверенного прокси равен `https`, cookie сессии WebUI получает атрибут `Secure`.
   Без `trusted_proxies` все клиенты за прокси выглядят как один IP (сервер один раз
   предупреждает об этом в журнале), а cookie остаётся без `Secure`.
2. Traefik передаёт путь как есть: префикс **не** срезается (`StripPrefix` не нужен), сервер
   сам обслуживает маршруты под `base_path`. Пример меток Docker:

   ```
   labels:
     - traefik.enable=true
     - traefik.http.routers.zam.rule=Host(`example.com`) && PathPrefix(`/memory`)
     - traefik.http.routers.zam.entrypoints=websecure
     - traefik.http.routers.zam.tls=true
     - traefik.http.services.zam.loadbalancer.server.port=7700
   ```

   Тот же маршрут в файловом провайдере (`dynamic.yml`):

   ```
   http:
     routers:
       zam:
         rule: "Host(`example.com`) && PathPrefix(`/memory`)"
         entryPoints: [websecure]
         tls: {}
         service: zam
     services:
       zam:
         loadBalancer:
           servers:
             - url: "http://10.0.0.5:7700"
   ```

   Traefik по умолчанию добавляет `X-Forwarded-For` и `X-Forwarded-Proto`.
3. Клиенты указывают полный адрес с префиксом: `ZeroAIMemoryUrl=https://example.com/memory`.

## Обновление

Остановите сервер, замените файлы комплекта (конфигурацию и каталог данных не трогайте),
запустите сервер: миграции баз применяются автоматически, копия файла создаётся перед миграцией.
