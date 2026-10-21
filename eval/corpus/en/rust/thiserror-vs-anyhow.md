---
tags: [rust, errors]
---
# thiserror vs anyhow: how we structure error handling

Team convention, written down after a long review thread. Short version: **libraries expose
typed errors built with `thiserror`; binaries and top-level glue use `anyhow`**. The two are
by the same author and complement each other — it is not either/or.

## thiserror: typed errors for a library API

`thiserror` is a derive macro. It generates `Display`, `std::error::Error` and `From`
implementations and adds no runtime type of its own; the result is an ordinary enum that
callers can `match` on.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("note not found: {0}")]
    NotFound(String),

    #[error("invalid frontmatter in {path}")]
    Frontmatter { path: PathBuf, #[source] source: serde_yaml::Error },

    #[error("i/o error")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}
```

Attributes worth remembering:

- `#[error("...")]` — the `Display` text; fields by name `{path}` or position `{0}`.
- `#[from]` — generates `From<Inner>` so `?` converts automatically; also marks the source.
- `#[source]` — marks the cause without generating `From` (use when the same inner type
  appears in two variants, since two `From` impls for one type would conflict).
- `#[error(transparent)]` — forwards `Display` and `source()` to the inner error; good for an
  "other" variant wrapping a lower layer.

Rule for messages: do **not** repeat the source text in the message (`"i/o error: {0}"`
together with `#[from]`). Reporters that walk the `source()` chain will print it twice.

When to choose it: the caller needs to react differently to different failures (retry on
timeout, 404 on `NotFound`), or the crate is published and the error is part of the API.

## anyhow: one opaque error for applications

`anyhow::Error` is a boxed `dyn Error + Send + Sync + 'static` with a backtrace and a chain
of context messages. `anyhow::Result<T>` is `Result<T, anyhow::Error>`.

```rust
use anyhow::{bail, Context, Result};

fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).context("parsing config")?;
    if cfg.workers == 0 {
        bail!("workers must be > 0");
    }
    Ok(cfg)
}
```

- `.context(..)` / `.with_context(|| ..)` add a layer to the chain; works on `Option` too.
- `bail!` returns early, `ensure!(cond, ..)` is an assert that returns an error.
- `{:#}` prints the chain on one line (`reading config x: No such file`), `{:?}` prints it
  multi-line with the backtrace if `RUST_BACKTRACE=1` or `RUST_LIB_BACKTRACE=1` is set.
- `main` can return `anyhow::Result<()>`; the error is printed with `{:?}` on exit.
- `err.downcast_ref::<StoreError>()` recovers the typed error when needed.

When to choose it: the error will only be logged or shown to a user, there is no caller
that branches on the kind. CLI tools, `main`, background job runners, tests.

## Side-by-side

| | thiserror | anyhow |
|---|---|---|
| What it is | derive macro, zero runtime cost | boxed error type |
| Caller can `match` | yes | only via `downcast` |
| Context chain | manual (variants with fields) | `.context()` |
| Backtrace | if you add a field | built in |
| Fits | libraries, domain layers | applications, glue |
| Public API stability | explicit | opaque |

Mixing them is normal: a library returns `StoreError`, the binary calls it with `?` inside a
function returning `anyhow::Result`, and the conversion happens automatically because
`StoreError: std::error::Error + Send + Sync + 'static`.

## Converting at layer boundaries

Each layer owns its enum and maps the lower one explicitly when the meaning changes:

```rust
impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(id) => ApiError::NotFound(id),
            other => ApiError::Internal(other.into()),   // keep the chain for logs
        }
    }
}
```

Where the meaning does not change, `#[from]` is enough. Where it does, a hand-written
`From` or `.map_err(..)` is clearer than piling more variants into one enum.

## HTTP handlers

In axum we return `Result<Json<T>, ApiError>` and implement `IntoResponse` for `ApiError`:
`NotFound` becomes 404 with a short message, `Internal` becomes 500 with a generic body,
and the full chain (`{:#}` of the inner `anyhow::Error`) goes to the log only. Never send
the source chain to the client: it leaks paths, SQL and hostnames.

## Printing errors correctly

`Display` of a typed error prints only the top message. To show the whole cause chain
without anyhow, walk `source()`:

```rust
let mut cur: Option<&dyn std::error::Error> = Some(&err);
while let Some(e) = cur {
    eprintln!("  caused by: {e}");
    cur = e.source();
}
```

Logging libraries differ: `tracing` with `%err` prints `Display` only, `?err` prints
`Debug`. With anyhow, `{:#}` is the compact form we use in logs.

## Common mistakes

1. `anyhow::Error` in a public library signature. Callers cannot distinguish failures and
   get a dependency on anyhow forced on them.
2. A giant single error enum for the whole workspace. Every module ends up depending on every
   other one. Keep enums per layer and convert at boundaries.
3. `Box<dyn Error>` without `+ Send + Sync`. It breaks as soon as the error crosses a thread
   or an await in a spawned task.
4. `unwrap()` in library code paths that are reachable from user input.

## Collecting an iterator of Result into Result<Vec<_>>

Not specific to either crate, but it comes up in every review. `Result` implements
`FromIterator`, so a sequence of fallible items can be collected into one result that stops
at the first error:

```rust
let ids: Result<Vec<u64>, std::num::ParseIntError> =
    lines.iter().map(|s| s.parse::<u64>()).collect();
let ids = ids?;
// or in one go:
let ids = lines.iter().map(|s| s.parse::<u64>()).collect::<Result<Vec<_>, _>>()?;
```

The iteration short-circuits: items after the first `Err` are not evaluated. If you need
*all* errors, use `partition`:

```rust
let (ok, bad): (Vec<_>, Vec<_>) = items.into_iter().map(check).partition(|r| r.is_ok());
let errors: Vec<_> = bad.into_iter().map(Result::unwrap_err).collect();
```

The same `FromIterator` trick works for `Option<Vec<_>>` and for `HashMap` targets.
Inside the closure, `?` does not work for the outer function — that is exactly why the
`collect::<Result<..>>()` form exists. With anyhow, map each item with `.with_context(..)`
before collecting to keep the failing input in the message.
