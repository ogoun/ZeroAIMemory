# build.rs and rerun-if-changed

## Default behaviour

Without any `rerun-if` instructions, Cargo re-runs the build script whenever **any file in
the package** changes. For a crate with a build script that compiles C code or generates
bindings, that means every edit to `src/main.rs` triggers the slow step.

As soon as the script prints at least one `rerun-if-changed` or `rerun-if-env-changed`, the
default is switched off and only the listed inputs are watched.

```rust
// build.rs
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=proto/");          // directory: any file inside
    println!("cargo::rerun-if-changed=native/wrapper.c");
    println!("cargo::rerun-if-env-changed=LIBFOO_DIR");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out.join("generated.rs"), generate()).unwrap();
}
```

`cargo::` with two colons is the syntax since Rust 1.77; the old `cargo:` form still works
and is needed if the crate supports older toolchains.

Printing only `rerun-if-changed=build.rs` is a common idiom: "run once, re-run only when the
script itself changes".

## Gotchas

- **Paths are relative to the package root**, not to the current directory of the shell.
- A path that does not exist makes Cargo re-run the script on every build — a typo in the
  file name silently turns the cache off.
- **Never write into `src/`.** Generated files go to `OUT_DIR` and are pulled in with
  `include!(concat!(env!("OUT_DIR"), "/generated.rs"));`. Writing into the source tree
  changes a watched file and can cause endless rebuilds; it also breaks `cargo publish`
  verification.
- Environment variables read with `std::env::var` are not tracked automatically; list them
  with `rerun-if-env-changed`. Variables Cargo sets itself (`TARGET`, `PROFILE`) are fine.
- `cfg!(target_os = ..)` inside build.rs describes the **host**. For the target use
  `CARGO_CFG_TARGET_OS`.

## Seeing what the script printed

Output goes to `target/<profile>/build/<pkg>-<hash>/output`; stderr is in `stderr` next to
it. `cargo build -vv` shows the lines live. Warnings for the user are emitted with
`cargo::warning=...`.

Other useful instructions: `cargo::rustc-link-lib=foo`, `cargo::rustc-link-search=native=..`,
`cargo::rustc-cfg=has_feature_x` (declare it with `cargo::rustc-check-cfg=cfg(has_feature_x)`
to avoid the `unexpected_cfgs` lint), `cargo::rustc-env=GIT_HASH=...`.
