---
tags: [rust, async, tokio, send]
---
# "future cannot be sent between threads safely": MutexGuard held across .await

A write-up after we hit this three times in one quarter. The error looks scary but it is
almost always the same root cause: some value that is not `Send` is still alive at an
`.await` point inside a future that is passed to `tokio::spawn`.

## The error

```
error: future cannot be sent between threads safely
   --> src/worker.rs:41:18
    |
41  |     tokio::spawn(handle(state.clone()));
    |                  ^^^^^^^^^^^^^^^^^^^^ future returned by `handle` is not `Send`
    |
    = help: within `impl Future<Output = ()>`, the trait `Send` is not implemented for
            `std::sync::MutexGuard<'_, Vec<Job>>`
note: future is not `Send` as this value is used across an await
    |
    |     let jobs = state.jobs.lock().unwrap();
    |         ---- has type `std::sync::MutexGuard<'_, Vec<Job>>` which is not `Send`
    |     send_report(&jobs).await;
    |                       ^^^^^^ await occurs here, with `jobs` maybe used later
```

`tokio::spawn` requires `F: Future + Send + 'static`, because the multi-threaded runtime may
move the task to another worker thread at any `.await`. An `async fn` compiles into a state
machine; every local that is alive across an `.await` becomes a field of that state machine.
If one of those fields is `!Send`, the whole future is `!Send`.

## Why MutexGuard is not Send

`std::sync::MutexGuard` is `!Send` on purpose: on some platforms a mutex must be unlocked by
the same OS thread that locked it. If the task resumed on another thread and dropped the
guard there, that would be undefined behaviour for the underlying lock. The same applies to
`parking_lot::MutexGuard` (unless its `send_guard` feature is on) and to `Rc`, `RefCell`
borrows, and raw pointers.

Even if it were `Send`, holding a blocking lock across an await is a bug by itself: while
the task is suspended, any other task that calls `lock()` blocks a runtime worker thread.
With enough of them the runtime stalls.

## Fix 1: drop the guard before awaiting

Most of the time the critical section is small. Copy out what you need and let the guard go.

```rust
async fn handle(state: Arc<State>) {
    let snapshot: Vec<Job> = {
        let jobs = state.jobs.lock().unwrap();
        jobs.iter().filter(|j| j.done).cloned().collect()
    }; // guard dropped here
    send_report(&snapshot).await;
}
```

A scoped block is more reliable than `drop(jobs)`. Older compilers computed "alive across
await" from scopes rather than real liveness, so an explicit `drop(guard)` before `.await`
did not always help; with a block there is no doubt. Recent rustc versions are smarter, but
the block also documents intent.

Watch out for temporaries in expressions:

```rust
// guard lives until the end of the whole statement, including the await
send(state.jobs.lock().unwrap().len()).await;
```

Bind the value first: `let n = state.jobs.lock().unwrap().len();` then `send(n).await`.

## Fix 2: use an async-aware mutex

If the lock really must be held during I/O (for example, a single connection that must not
be used concurrently), use `tokio::sync::Mutex`. Its guard is `Send` (when `T: Send`) and
`lock().await` yields instead of blocking the thread.

```rust
let conn = state.conn.lock().await;
conn.write_all(&buf).await?;
```

Costs: it is slower than `std::sync::Mutex` for short sections, and holding it across long
awaits serialises the tasks. The tokio docs themselves recommend the std mutex for plain
data guarded for a few instructions.

| Situation | Choice |
|---|---|
| Short update of in-memory data, no await inside | `std::sync::Mutex` (or `parking_lot`) |
| Guard must live across `.await` | `tokio::sync::Mutex` |
| Many readers, shared config | `tokio::sync::RwLock` or `arc-swap` |
| Exclusive resource with a queue of requests | an actor task plus `mpsc` channel |

## Fix 3: restructure as an actor

For shared resources with complex state, a dedicated task owning the data and receiving
commands over `tokio::sync::mpsc` removes the lock entirely. Callers send a message with a
`oneshot::Sender` for the reply. No guard exists, so nothing can be held across an await.

## Other values that cause the same error

- `Rc<T>` alive across an await: switch to `Arc<T>`.
- `RefCell` borrow (`Ref`, `RefMut`) across an await.
- `Box<dyn Error>` in a local or a `Result` kept across an await: it is not `Send`; use
  `Box<dyn Error + Send + Sync>` or `anyhow::Error`.
- A `!Send` type from a C library wrapper.
- `ThreadRng` from `rand::thread_rng()` held in a local across an await.

## Finding the culprit fast

- Read the `note:` lines, not the top: they name the type and the exact await.
- To check a function in isolation, add a compile-time assertion:

```rust
fn assert_send<T: Send>(_: T) {}
#[allow(dead_code)]
fn check() { assert_send(handle(Arc::new(State::default()))); }
```

- If the future really cannot be `Send` (it uses a `!Send` library), run it with
  `tokio::task::spawn_local` inside a `LocalSet`, or on a current-thread runtime.

## Why spawn needs Send but block_on does not

`Runtime::block_on(fut)` and `#[tokio::main]`'s body run the future on the calling thread
and never move it, so a `!Send` future is accepted there. That is why code works when called
directly from `main` and breaks the moment someone wraps it in `tokio::spawn` for
concurrency. The same function signature is fine in one place and an error in another —
the requirement comes from the spawner, not from the `async fn` itself.

The `'static` half of the bound is a separate problem: a spawned task may outlive the
caller, so it cannot borrow the caller's locals. That error mentions "borrowed value does
not live long enough" or "argument requires that ... is borrowed for 'static", not `Send`.
Fix with `Arc` clones moved into the task.

## Error appearing far from the cause

The worst variant: the `!Send` value is deep in a call chain. `handler` awaits `service`,
which awaits `repo`, which holds a guard across an await. The error points at the
`tokio::spawn` (or at axum's `Handler` trait bound, with a much less readable message:
"the trait `Handler<_, _>` is not implemented for fn item"). For axum, add
`#[axum::debug_handler]` to the handler — it turns the bound failure into a readable
diagnostic that names the `!Send` type. Then bisect with the `assert_send` helper above,
moving it down the chain until the innermost function that fails.

## Incident timeline (for the record)

1. A refactor moved metrics collection into the request path; the metrics struct sat
   behind `std::sync::Mutex`.
2. Someone added an `.await` for an HTTP push inside the locked block. The handler was
   called directly in tests, so they passed.
3. The production router spawned the handler; compilation failed in CI only for the
   server crate. Fixed by building the payload under the lock and sending it after.
4. Follow-up: `clippy -D warnings` in CI, and a rule in review notes — no `.await` inside a
   block that holds a std guard.

## Clippy helps

`clippy::await_holding_lock` (warn by default in the `suspicious` group) flags std and
parking_lot guards held across await points. `clippy::await_holding_refcell_ref` does the
same for `RefCell`. Keeping clippy in CI would have caught two of our three incidents before
review.
