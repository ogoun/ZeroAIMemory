# Cargo feature unification and resolver 2 / 3

## How features combine

Cargo builds each dependency **once per build** with the **union** of all features any crate
in the graph requested. If crate A asks for `tokio = { features = ["rt"] }` and crate B asks
for `["net"]`, tokio is compiled with both. That is why features must be additive: enabling a
feature must never remove an API or change behaviour incompatibly. Mutually exclusive features
(`backend-a` vs `backend-b`) break as soon as two crates in the graph pick different ones.

Consequences we ran into:

- `cargo build -p server` and `cargo build` at the workspace root can produce **different**
  feature sets for a shared dependency, because the root build includes requests from every
  member. Result: the dependency is recompiled when switching between the two commands.
- A test-only feature turned on by a dev-dependency leaked into release builds of another
  crate under the old resolver.

## Resolver "2"

Default for edition 2021. It stops unifying features in three places:

1. **Build dependencies and proc-macros** are resolved separately from normal dependencies,
   so a `build.rs` that needs `serde/std` does not force it on an embedded target.
2. **Target-specific dependencies** (`[target.'cfg(windows)'.dependencies]`) do not
   contribute features when building for a different target.
3. **Dev-dependencies** add features only when building something that needs them (tests,
   examples, benches), not for `cargo build`.

Side effect: a crate shared between build-deps and normal deps may be compiled twice.

## Resolver "3"

Stabilised in Rust 1.84 and default for edition 2024. Feature handling is the same as "2".
What it adds is MSRV-aware version selection: when picking versions, cargo prefers releases
whose `rust-version` is compatible with the workspace's own `rust-version`
(`incompatible-rust-versions = "fallback"`).

## Where to set it

In a workspace the resolver is a workspace-wide setting. For a virtual manifest (no root
package) it must be set explicitly, otherwise cargo warns and falls back to "1":

```toml
[workspace]
members = ["crates/*"]
resolver = "3"
```

With a root package, the root `edition` implies the resolver.

## Debugging feature sets

```
cargo tree -e features -i tokio        # who enables which tokio features
cargo tree -e features --target x86_64-pc-windows-msvc
cargo tree -d                          # duplicate versions in the graph
```

`-i` inverts the tree so you see the path from your crates to the dependency and which edge
turned a feature on.
