---
tags: [tls, nginx, сертификаты]
---
# Неполная цепочка сертификатов: браузер доволен, curl ругается

Классика, на которую мы наступили при переносе API на новый сервер. В Chrome всё открывается,
а мобильное приложение на Android и `curl` из CI падают с ошибкой проверки:

```
curl: (60) SSL certificate problem: unable to get local issuer certificate
```

Сертификат при этом свежий, срок не истёк, имя совпадает. Причина — сервер отдаёт только
листовой сертификат, без промежуточного (intermediate) CA.

## Почему браузер не замечает

Десктопные браузеры умеют достраивать цепочку сами: кэшируют промежуточные сертификаты с
других сайтов или скачивают их по ссылке из расширения AIA (Authority Information Access).
OpenSSL, Go, Java, старые Android так не делают — им нужна полная цепочка от сервера,
кроме корня, который лежит в доверенном хранилище клиента.

## Как проверить

```bash
openssl s_client -connect api.example.com:443 -servername api.example.com -showcerts </dev/null
```

Смотреть на блок `Certificate chain`. Должно быть минимум два элемента:

```
 0 s:CN = api.example.com
   i:C = US, O = Let's Encrypt, CN = R11
 1 s:C = US, O = Let's Encrypt, CN = R11
   i:C = US, O = Internet Security Research Group, CN = ISRG Root X1
```

Если есть только строка `0`, а внизу `Verify return code: 21 (unable to verify the first
certificate)` — цепочка неполная. `-servername` обязателен, иначе при SNI можно получить
сертификат другого виртуального хоста и долго искать не там.

## Исправление

- Для Let's Encrypt (certbot) в конфиге указывать `fullchain.pem`, а не `cert.pem`:
  `ssl_certificate /etc/letsencrypt/live/api.example.com/fullchain.pem;`
- Если сертификат куплен у коммерческого CA, склеить файл руками: сначала листовой, потом
  промежуточные, корень не добавлять: `cat server.crt intermediate.crt > bundle.crt`.
- Порядок важен: листовой всегда первым. Обратный порядок nginx примет, а клиенты — нет.
- После правки `nginx -t && systemctl reload nginx` и повторить `s_client`.

## Что добавили в чек-лист выкладки

Проверка цепочки с машины без браузера (`curl -v https://...` из CI) и внешний сервис
проверки TLS после каждой замены сертификата. Отдельно — проверка срока действия, но это
другая история (см. заметку про истёкший сертификат).
