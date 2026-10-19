# tokio::task::spawn_blocking: when and how

Symptom we had: an axum service where p99 latency of *all* endpoints jumped whenever someone
uploaded a large image. The handler resized the image synchronously inside `async fn`. Tokio
worker threads are few (one per core by default) and cooperative: a task that runs 300 ms
without reaching an `.await` occupies a worker for 300 ms, and timers, other requests and
I/O on that worker wait.

## The fix

```rust
let thumb = tokio::task::spawn_blocking(move || resize(&bytes, 256))
    .await
    .map_err(|e| anyhow::anyhow!("resize task failed: {e}"))??;
```

`spawn_blocking` moves the closure to a separate pool of blocking threads and returns a
`JoinHandle<T>`. Awaiting it yields `Result<T, JoinError>`; the `JoinError` is set when the
closure panicked (or the task was cancelled at shutdown).

## What belongs there

- CPU-heavy work longer than roughly 10–100 µs between awaits: hashing passwords (argon2,
  bcrypt), compression, image processing, parsing big files.
- Blocking APIs with no async variant: `std::fs` in bulk, synchronous database drivers such
  as `rusqlite`, blocking FFI calls.

## Pitfalls

- **The pool is large but finite.** Default `max_blocking_threads` is 512. Extra calls are
  queued. It is meant for blocking I/O; for sustained CPU-bound parallel work use `rayon` and
  bridge the result back with a `tokio::sync::oneshot` channel, otherwise you oversubscribe
  the cores.
- **Cannot be cancelled.** Dropping the `JoinHandle` or aborting it does not stop a closure
  that is already running. The runtime waits for blocking tasks on shutdown unless you use
  `shutdown_timeout`.
- **Needs `'static` + `Send`.** Clone or move owned data into the closure; borrowed locals do
  not work.
- **`block_in_place`** is an alternative that converts the current worker into a blocking
  one without moving data, but it panics on the current-thread runtime. I prefer
  `spawn_blocking` for clarity.
- Calling `Handle::block_on` from inside the closure is allowed; calling it from an async
  context panics with `Cannot start a runtime from within a runtime`.

## How to spot it

`tokio-console` shows tasks with long poll times. A quick check without tooling: log the
elapsed time around the suspicious synchronous call; anything in milliseconds is a candidate.
