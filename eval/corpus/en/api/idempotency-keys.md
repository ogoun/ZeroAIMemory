# Idempotency keys in the orders API

Clients of the orders API retry requests after timeouts and network errors. To make retries of
mutating requests safe, the API accepts an `Idempotency-Key` header on `POST` and `PATCH`.

## Storage

The service stores the key, a hash of the request body, the response status and the response
body in a table with a 24-hour lifetime. The key is scoped to the client, so two clients may use
the same value.

## Replays

A request with a known key and the same body hash is not executed again: the stored response is
returned with `Idempotent-Replayed: true`. The same key with a different body is rejected with
422, because the client is reusing keys by mistake.

## Concurrent requests

While the first request with a key is still running, a second one with the same key gets 409 and
`Retry-After: 1`. The row is locked with `SELECT ... FOR UPDATE SKIP LOCKED` so that only one
request runs.

## Clients

The storefront, the seller dashboard and partner integrations generate keys as UUIDs per user
action, not per HTTP attempt, and send the same key in every retry of that action.
