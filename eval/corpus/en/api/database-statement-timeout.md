# Statement timeouts for service database queries

A query that runs for minutes holds a connection from the pool, takes locks and keeps the
request thread busy long after the client has given up. Every service therefore limits how long
a single statement may run.

## Where the limit is set

The limit is set per database role: `ALTER ROLE orders_app SET statement_timeout = '5s'`.
Reporting jobs connect with a separate role that allows 10 minutes. Setting the value per role
keeps it in one place and survives application restarts.

## What the application sees

A statement that hits the limit fails with SQLSTATE `57014` (query canceled). The service maps
it to HTTP 503 with a short message and logs the query name, never the parameters.

## Lock timeouts

`lock_timeout = '2s'` is set together with the statement timeout. Migrations that need an
exclusive lock set their own short `lock_timeout` and retry, so that a migration waiting for a
lock does not block all readers behind it.

## Choosing the value

The limit is about ten times the p99 latency of the slowest normal query of the service.
