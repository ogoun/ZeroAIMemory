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

1. Хэш пароля в `config.toml` (рекомендуется):

   ```
   echo "мой-пароль" | zam-server hash-password
   ```

   Результат (`$argon2id$...`) вставьте в `[admin] password_hash`.
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

## Windows

1. Распакуйте архив `zam-server-<версия>-windows-x64-cpu.zip`, например в `C:\ZeroAIMemory`.
2. Рядом с `qdrant.exe` лежит app-local `vcruntime140.dll`, отдельная установка VC Redistributable не нужна.
3. Создайте `config.toml` и запустите `zam-server.exe serve --config config.toml`.
4. Для работы в фоне зарегистрируйте запуск в Планировщике заданий или через менеджер служб (например, NSSM).

Остановка: Ctrl+C в окне сервера.

## Linux

1. Распакуйте архив `zam-server-<версия>-linux-x64-cpu.tar.gz` в `/opt/zeroaimemory`
   (исполняемые файлы: `zam-server`, `zam`, `qdrant/<версия>/qdrant`).
2. Создайте пользователя и каталоги:

   ```
   sudo useradd --system --home /var/lib/zeroaimemory --shell /usr/sbin/nologin zam
   sudo install -d -o zam -g zam /var/lib/zeroaimemory
   sudo install -d /etc/zeroaimemory
   sudo cp config.example.toml /etc/zeroaimemory/config.toml
   ```

   В `config.toml` укажите `data_dir = "/var/lib/zeroaimemory"` и `[admin] password_hash`.
3. Установите юнит systemd и запустите службу:

   ```
   sudo cp systemd/zam-server.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now zam-server
   sudo journalctl -u zam-server -n 50      # первый токен в рамке (stderr)
   ```

   В юните `ReadWritePaths` должен совпадать с `data_dir`; остановка — `SIGTERM`
   (таймаут 20 с).
4. `zam-server` собран под `x86_64-unknown-linux-gnu` на Ubuntu 22.04 (glibc 2.35 и новее);
   `zam` — статический (musl).

## Обновление

Остановите сервер, замените файлы комплекта (конфигурацию и каталог данных не трогайте),
запустите сервер: миграции баз применяются автоматически, копия файла создаётся перед миграцией.
