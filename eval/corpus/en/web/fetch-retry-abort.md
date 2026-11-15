# Retrying fetch requests in the storefront

The storefront single-page app talks to the API with a thin wrapper around `fetch`. The wrapper
owns timeouts, retries and cancellation, so components never call `fetch` directly.

## Timeouts and cancellation

Every request gets an `AbortController`. A timer aborts the request after 10 seconds, and the
component that started it aborts it on unmount, so a page the user has left does not keep
updating state.

## Which requests are retried

Only `GET` requests are retried automatically, and only on network errors (`TypeError` from
`fetch`) and on 502, 503 and 504. Mutations such as "add to cart" are never retried by the
wrapper; the UI shows an error with a retry button instead.

## Backoff

The wrapper waits 300 ms, then 1 s, then gives up. The delay is randomized by up to half of its
value so that thousands of browsers do not retry in lockstep after an outage.

## Offline

When `navigator.onLine` is false the wrapper does not spend attempts at all: it waits for the
`online` event and then sends the request once.
