---
tags: [rust, cargo, workspace]
---
# Cargo workspace: shared dependency versions and per-package profiles

## workspace.dependencies

Declare versions once in the root `Cargo.toml`:

```toml
[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
tokio = { version = "1.40", default-features = false }
internal-util = { path = "crates/util" }
```

Members opt in:

```toml
[dependencies]
serde = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
```

Rules that surprised people:

- A member cannot override `version` or `path`; only `features` and `optional` can be added
  locally. Features are **added** to the workspace ones, not replaced.
- `default-features = false` must be in the workspace entry; setting it in the member while
  the workspace entry keeps defaults on does not turn them off (cargo warns about this).
- `[workspace.package]` does the same for `version`, `edition`, `license`,
  `rust-version`: `edition.workspace = true`.

## Profiles live only in the root

`[profile.*]` sections in member manifests are ignored with a warning. Put them in the
workspace root.

## Optimising dependencies in debug builds

Debug builds of heavy crates (image decoding, compression, crypto, ONNX bindings) can be
10–50x slower than release. Our own code stays at `opt-level = 0` for fast compiles and good
debugging, dependencies get optimised:

```toml
[profile.dev.package."*"]
opt-level = 2          # every crate except workspace members

[profile.dev.package.image]
opt-level = 3          # one specific crate

[profile.dev.build-override]
opt-level = 0          # build scripts and proc-macros: compile fast
```

The `"*"` override applies to non-member packages only. Dependencies are compiled once and
cached, so the extra cost is paid only on a clean build.

Release-side settings we use:

```toml
[profile.release]
lto = "thin"
codegen-units = 1
debug = "line-tables-only"   # usable backtraces, small binary
```

A custom profile (`[profile.ci] inherits = "release"`, `lto = false`) keeps CI builds faster
than full release while still optimised; select it with `cargo build --profile ci`.
