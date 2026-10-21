---
tags: [rust, cross-compilation, linux]
---
# Static Linux binaries with the musl target

Goal: one binary that runs on any x86_64 or aarch64 Linux regardless of the glibc version on
the host. Building on a new distro and deploying to an old one gives
`version `GLIBC_2.34' not found` — the musl target avoids that.

## Basic build (x86_64 on x86_64 Linux)

```
rustup target add x86_64-unknown-linux-musl
sudo apt install musl-tools        # provides musl-gcc, needed by crates with C code
cargo build --release --target x86_64-unknown-linux-musl
file target/x86_64-unknown-linux-musl/release/app   # "static-pie linked" or "statically linked"
```

The musl targets link statically by default (`crt-static` is on). Pure-Rust projects need
nothing else.

## Cross to aarch64

Needs a linker for the target. In `.cargo/config.toml`:

```toml
[target.aarch64-unknown-linux-musl]
linker = "aarch64-linux-musl-gcc"
```

Getting a musl cross toolchain is the annoying part. Options:

- **cargo-zigbuild**: uses `zig cc` as the linker and C compiler for any target.
  `cargo zigbuild --release --target aarch64-unknown-linux-musl`. No containers, works from
  macOS and Windows too. My default.
- **cross**: runs the build inside a prepared container image per target. Convenient if a
  container runtime is already available.
- A musl.cc style prebuilt toolchain unpacked somewhere and referenced via `linker`.

Crates with C code built through the `cc` crate look for `CC_aarch64_unknown_linux_musl`
and `AR_aarch64_unknown_linux_musl` environment variables (underscores, not dashes).

## Problems seen in practice

**OpenSSL.** `openssl-sys` wants a musl-built libssl. Easiest: switch `reqwest`/`tokio` TLS
to `rustls` (`default-features = false, features = ["rustls-tls"]`). If OpenSSL is required,
the `vendored` feature of the `openssl` crate builds it from source (needs perl and make).

**Allocator performance.** musl's malloc is slow under multi-threaded load; we measured a
3x regression on a JSON-heavy service. Swap the global allocator:

```rust
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

**DNS.** musl's resolver does not support everything in `resolv.conf` the same way
(for example, it queries nameservers in parallel and has had limited TCP fallback for large
responses in older versions). Rare, but it shows up in Kubernetes with long search lists.

**Proc-macros and build scripts** are compiled for the host, not for musl, so they are
unaffected.

**`ldd` says "not a dynamic executable"** — that is the expected result, not an error.

## Reading linker errors

- `linker 'cc' not found` or `error: linking with 'cc' failed` with a long list of
  unresolved symbols for an aarch64 target: no target linker is configured, cargo fell back
  to the host compiler. Set `linker` in `.cargo/config.toml` or use zigbuild.
- `musl-gcc: not found` from a `*-sys` crate build script: install `musl-tools` (x86_64) or
  point `CC_<triple>` at a cross compiler.
- `undefined reference to '__memcpy_chk'` or other `_chk` symbols: some object file was
  compiled against glibc headers (often a prebuilt static library shipped by a `*-sys`
  crate). It has to be rebuilt for musl; look for a `vendored` or `bundled` feature.
- `cannot find -lstdc++` for crates with C++ code (RocksDB, some compression libraries):
  the musl toolchain needs a static libstdc++; zig provides libc++ automatically, which is
  another reason we default to zigbuild.

## Building from Windows or macOS

The musl target can be added with rustup on any host, but linking needs a Linux-capable
linker. On Windows the practical options are WSL (build inside a Linux distribution as
usual) or `cargo-zigbuild`, which ships its own linker logic through zig and does not care
about the host OS. `rust-lld` alone is not enough once any crate needs C code, because the
C parts still need a compiler that targets musl.

## Binary size

Static binaries are bigger than dynamic ones, but most of the size is our own code and
dependencies, not libc. What helped:

```toml
[profile.release]
strip = true          # remove symbols
lto = "thin"
codegen-units = 1
panic = "abort"       # only if nothing relies on catching panics
```

Typical result for a mid-sized web service: 28 MB unstripped, 9 MB stripped. `cargo bloat
--release --target x86_64-unknown-linux-musl --crates` shows which dependencies dominate.

## glibc static instead of musl?

`RUSTFLAGS="-C target-feature=+crt-static"` with the `x86_64-unknown-linux-gnu` target also
produces a static binary. It avoids musl's allocator and resolver differences, but glibc
does not support static linking well: anything that uses NSS (`getpwnam`, some DNS
configurations) still tries to load shared libraries at runtime and warns at link time.
For network services that only resolve hostnames, it usually works; we still prefer musl
because its behaviour is predictable.

## Testing the result

- Run the binary in a minimal environment: an empty chroot, or a scratch container image if
  you have a container runtime, with only the binary copied in. If it starts, it is truly
  self-contained.
- `readelf -d app` should list no `NEEDED` entries.
- For aarch64 builds on an x86_64 machine, `qemu-aarch64` user-mode emulation runs the
  static binary directly, which is enough for smoke tests in CI.

## CI matrix we use

| Target | Tool | Runner |
|---|---|---|
| `x86_64-unknown-linux-musl` | plain cargo + musl-tools | Linux x64 |
| `aarch64-unknown-linux-musl` | cargo-zigbuild | Linux x64 |
| `x86_64-pc-windows-msvc` | plain cargo | Windows |

## Checklist for a release pipeline

1. `rustup target add <triple>`.
2. Pin the toolchain in `rust-toolchain.toml` (including `targets = [...]`).
3. Choose rustls over OpenSSL where possible.
4. Replace the allocator for multi-threaded services.
5. Smoke-test the binary on the oldest distro you support.
