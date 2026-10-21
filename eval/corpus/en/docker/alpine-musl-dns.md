---
tags: [docker, alpine, dns]
---
# Alpine images: musl resolver surprises

Moved a Python service from `python:3.12-slim` to `python:3.12-alpine` to save 80 MB.
Rolled back within a day. Notes on why.

- **DNS search domains.** musl's resolver handles `search` and `ndots` in
  `/etc/resolv.conf` differently from glibc, and older musl versions (before 1.2.4) did
  not fall back to TCP for large answers. In Kubernetes, with `ndots:5`, short names like
  `redis` resolved on Debian images but intermittently failed on Alpine. Using fully
  qualified names with a trailing dot (`redis.default.svc.cluster.local.`) avoided it.
- **Wheels.** Many Python packages ship manylinux (glibc) wheels but no musllinux ones,
  so `pip install` compiled numpy and cryptography from source — slower builds and a
  bigger image than slim, the opposite of the goal.
- **Performance.** musl's malloc is noticeably slower under multi-threaded allocation.
- **Debugging.** `nslookup` in busybox does not behave like the bind-utils one; install
  `bind-tools` for `dig`.

Alpine is still fine for static Go binaries or simple shell tooling. For anything with
native extensions we stay on Debian slim or distroless.
