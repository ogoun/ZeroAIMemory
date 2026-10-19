# Логи контейнеров съели диск: ротация json-file

По умолчанию драйвер логов Docker — `json-file`, и он **не ограничивает** размер. Всё,
что контейнер пишет в stdout/stderr, копится в
`/var/lib/docker/containers/<id>/<id>-json.log`. У нас один болтливый воркер за месяц
набрал 60 ГБ.

Найти виновника:

```sh
sudo du -h /var/lib/docker/containers/*/*-json.log | sort -h | tail
```

Глобальная настройка в `/etc/docker/daemon.json`:

```json
{
  "log-driver": "local",
  "log-opts": { "max-size": "20m", "max-file": "5" }
}
```

Либо `json-file` с теми же `max-size`/`max-file`. Драйвер `local` сжимает файлы и по
умолчанию уже ротирует (100 МБ на контейнер), `docker logs` с ним работает.

После правки — `systemctl restart docker`. Подвох: настройки применяются только к
**новым** контейнерам; существующие надо пересоздать (`docker compose up -d
--force-recreate`). Для отдельного сервиса в compose — ключ `logging:` с `driver` и
`options`.

Срочно освободить место без перезапуска: `truncate -s 0` на файл лога. Удалять его через
`rm` бесполезно — dockerd держит файл открытым, и место не вернётся.
