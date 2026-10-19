# Incident: database connection pool exhausted

Symptoms: API latency jumped from 150 ms to the 30 s request timeout, logs full of
`timeout waiting for connection from pool` (HikariCP says `Connection is not available,
request timed out after 30000ms`). The database itself was at 15 % CPU and looked healthy.

## What happened

A new feature called a third-party fraud-check API **inside** a database transaction. The
external call took 2–8 s under load. Each request held a pooled connection for the whole time,
the pool of 20 connections per instance drained, and every other request queued behind it.

Meanwhile, `pg_stat_activity` showed dozens of sessions in `idle in transaction`: connected,
transaction open, doing nothing.

```sql
SELECT state, count(*), max(now() - xact_start) AS oldest
FROM pg_stat_activity
WHERE datname = 'shop'
GROUP BY state;
```

## Fixes

- Moved the external call outside the transaction: read, call, then open a short transaction
  to write the result.
- `idle_in_transaction_session_timeout = '30s'` on the database role, so a leaked transaction
  gets killed instead of holding locks and a connection forever.
- A pool acquire timeout of 2 s instead of 30 s. Failing fast frees the request thread and
  surfaces the problem in metrics within seconds.
- Metrics on pool usage: active, idle, pending threads, acquire time. Alert when pending > 0
  for several minutes.

## Sizing the pool

Bigger is not better. PostgreSQL forks a process per connection, and throughput peaks at a
small number of active connections, roughly a few per CPU core. With 30 app instances × 20
connections we had 600 potential connections against `max_connections = 500`; a deploy that
doubled instances briefly caused `FATAL: sorry, too many clients already`.

Better: smaller per-instance pools and PgBouncer in transaction mode in front of the database,
which multiplexes thousands of client connections onto a few dozen server connections. Note
that transaction mode breaks session state: session-level `SET`, advisory locks and, in older
PgBouncer versions, named prepared statements.
