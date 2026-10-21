---
tags: [nginx, proxy, http]
---
# nginx как обратный прокси: заголовки X-Forwarded-*, реальный IP клиента и доверенные прокси

Сводная заметка после двух историй подряд: в одной приложение за nginx генерировало ссылки
с `http://` вместо `https://` и уходило в бесконечный редирект, в другой ограничение частоты
запросов срабатывало на всех пользователей сразу, потому что все они выглядели как
`127.0.0.1`. Обе проблемы про одно: что бэкенд знает об исходном запросе, если перед ним
стоит прокси.

## Что теряется при проксировании

Когда nginx делает `proxy_pass`, для бэкенда клиентом становится сам nginx. Без
дополнительных заголовков приложение не знает:

| Что | Чем передаём | Переменная nginx |
|-----|--------------|------------------|
| IP клиента | `X-Forwarded-For`, `X-Real-IP` | `$proxy_add_x_forwarded_for`, `$remote_addr` |
| Схема (http/https) | `X-Forwarded-Proto` | `$scheme` |
| Исходный хост | `Host`, `X-Forwarded-Host` | `$host` |
| Порт | `X-Forwarded-Port` | `$server_port` |

Есть и стандартный заголовок `Forwarded` (RFC 7239), который объединяет всё в одном
(`Forwarded: for=203.0.113.7;proto=https;host=example.com`), но на практике большинство
фреймворков всё ещё смотрят на `X-Forwarded-*`.

## Базовый конфиг

```nginx
upstream app {
    server 127.0.0.1:8080;
    keepalive 32;
}

server {
    listen 443 ssl;
    server_name example.com;

    location / {
        proxy_pass http://app;
        proxy_http_version 1.1;
        proxy_set_header Connection "";

        proxy_set_header Host              $host;
        proxy_set_header X-Real-IP         $remote_addr;
        proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host  $host;

        proxy_connect_timeout 5s;
        proxy_read_timeout    60s;
    }
}
```

Пояснения:

- `$proxy_add_x_forwarded_for` дописывает `$remote_addr` к пришедшему заголовку через
  запятую. Если nginx — самый внешний узел, правильнее перезаписать заголовок целиком
  (`proxy_set_header X-Forwarded-For $remote_addr;`), иначе клиент может подсунуть свою
  «историю» и бэкенд её примет.
- `proxy_set_header` наследуется из внешнего блока только если в текущем `location` нет ни
  одной своей директивы `proxy_set_header`. Добавили в location один заголовок — все
  остальные из `server` молча пропали. Это грабли номер один.
- `proxy_http_version 1.1` и пустой `Connection` нужны, чтобы работал `keepalive` к
  upstream; по умолчанию nginx ходит к бэкенду по HTTP/1.0 и закрывает соединение.

## История с редиректом и X-Forwarded-Proto

TLS терминируется на nginx, к приложению запрос идёт по обычному HTTP. Приложение видит
`http`, считает запрос небезопасным и редиректит на `https://` — nginx снова проксирует
как `http`, и так по кругу. Плюс cookie с флагом `Secure` не ставились, а абсолютные ссылки
в письмах вели на `http://`.

Решение в две части:

1. nginx передаёт `X-Forwarded-Proto $scheme`.
2. Приложению явно говорим, что этому заголовку можно верить. В Django это
   `SECURE_PROXY_SSL_HEADER = ("HTTP_X_FORWARDED_PROTO", "https")`, в Express —
   `app.set("trust proxy", "loopback")`, в ASP.NET Core — middleware `ForwardedHeaders` с
   заполненным списком `KnownProxies`.

Если перед nginx есть ещё балансировщик облака, который сам терминирует TLS, то `$scheme`
у nginx будет `http`, и передавать надо пришедший заголовок, а не свою схему. Делается
через `map`:

```nginx
map $http_x_forwarded_proto $fwd_proto {
    default $http_x_forwarded_proto;
    ""      $scheme;
}
```

## Доверенные прокси и подмена IP

Заголовок `X-Forwarded-For` может прислать кто угодно. Верить ему можно только если запрос
пришёл от известного нам прокси. Модуль `ngx_http_realip_module` делает ровно это:

```nginx
set_real_ip_from 10.0.0.0/8;        # адреса нашего балансировщика
real_ip_header   X-Forwarded-For;
real_ip_recursive on;
```

С `real_ip_recursive on` nginx идёт по списку адресов справа налево и пропускает все,
что входят в `set_real_ip_from`; первый недоверенный адрес становится `$remote_addr`. Без
этой опции берётся просто последний адрес из списка, что при цепочке из двух прокси даёт
адрес внутреннего балансировщика.

Наша ошибка с ограничением частоты: в приложении была включена опция «доверять всем
прокси», и оно брало самый левый адрес из `X-Forwarded-For`. Его клиент контролирует
полностью, так что обойти лимит можно было, просто подставляя случайный IP. А когда
заголовок не передавался вовсе, все запросы шли от `127.0.0.1` и делили один лимит.

Правило: список доверенных прокси — это конкретные адреса или подсети, а не «все».
Идти по `X-Forwarded-For` справа налево и остановиться на первом адресе, которого нет в
списке. Это и есть IP клиента.

## Чек-лист при подключении нового бэкенда за nginx

- [ ] `Host`, `X-Forwarded-For`, `X-Forwarded-Proto` выставляются в каждом `location`,
      где есть свои `proxy_set_header`.
- [ ] Приложению задан конкретный список доверенных прокси.
- [ ] Проверить подмену: `curl -H "X-Forwarded-For: 1.2.3.4" https://example.com/whoami`
      не должен показывать `1.2.3.4` как адрес клиента.
- [ ] Абсолютные URL и редиректы строятся с `https`.
- [ ] Таймауты `proxy_read_timeout` согласованы с таймаутами приложения: если бэкенд
      отвечает дольше, клиент получит 504 от nginx, а работа на бэкенде продолжится.
- [ ] В access-логе nginx пишется и `$remote_addr`, и `$http_x_forwarded_for` — иначе при
      разборе инцидента не восстановить, откуда шёл запрос.

## Таймауты, буферизация и WebSocket

Помимо заголовков, у проксирования есть ещё несколько настроек, которые регулярно всплывают
в разборах.

- `proxy_connect_timeout` — время на установку соединения с бэкендом. По умолчанию 60 с,
  что слишком много: если бэкенд не принимает соединения, лучше быстро вернуть 502 и
  попробовать следующий сервер из `upstream`.
- `proxy_read_timeout` — сколько ждать между двумя чтениями от бэкенда (не общее время
  ответа). Долгие отчёты, которые долго ничего не пишут, упираются именно в него и дают 504.
- `proxy_next_upstream` — при каких ошибках nginx отправит запрос на другой сервер. По
  умолчанию это `error timeout`, и неидемпотентные запросы (POST) повторно не отправляются,
  если запрос уже ушёл на бэкенд. Добавлять `non_idempotent` стоит только понимая
  последствия: заказ может создаться дважды.
- `proxy_buffering on` (по умолчанию) — nginx читает ответ бэкенда в буферы и отдаёт
  медленному клиенту сам, освобождая бэкенд. Для потоковых ответов (Server-Sent Events,
  длинный опрос) буферизацию выключают: `proxy_buffering off;` или заголовком
  `X-Accel-Buffering: no` из приложения.
- `client_max_body_size` — по умолчанию 1 МБ; загрузка файлов больше получает `413 Request
  Entity Too Large` ещё до бэкенда.

WebSocket требует явной передачи заголовков апгрейда, иначе соединение не переключится:

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ""      close;
}

location /ws/ {
    proxy_pass http://app;
    proxy_http_version 1.1;
    proxy_set_header Upgrade    $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
    proxy_set_header Host       $host;
    proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_read_timeout 1h;
}
```

Обратите внимание: в этом `location` свои `proxy_set_header`, поэтому заголовки
`X-Forwarded-*` повторены — из `server` они не унаследуются. И `proxy_read_timeout` здесь
большой: без сообщений в канале соединение иначе закроется через минуту. Правильнее, чтобы
приложение слало ping чаще таймаута.

## Полезное для отладки

Временный location, который возвращает то, что видит nginx:

```nginx
location = /debug-headers {
    default_type text/plain;
    return 200 "remote_addr=$remote_addr\nxff=$http_x_forwarded_for\nscheme=$scheme\nhost=$host\n";
}
```

На стороне приложения — аналогичный эндпоинт, печатающий вычисленный IP клиента и схему.
Сравнение двух ответов быстро показывает, на каком звене теряется информация. После
отладки оба эндпоинта убрать: они раскрывают внутреннюю топологию.
