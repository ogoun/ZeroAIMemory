---
tags: [docker, dockerfile, build, ci]
---
# Multi-stage Dockerfile and layer cache: from 1.4 GB and 9 minutes to 90 MB and 40 seconds

Write-up of how we rebuilt the Dockerfile for the Go API service. The original was a
single stage based on the full `golang` image, copied the whole repository first and ran
`go build` afterwards. Every commit invalidated every layer and the image shipped the
compiler, the module cache and the source tree.

## How the layer cache decides

Each instruction produces a layer keyed by the instruction text plus, for `COPY` and
`ADD`, a checksum of the copied files. The builder reuses a cached layer only if the
parent layer was reused too. Consequences:

- once one step misses the cache, **every step after it** is rebuilt;
- `COPY . .` near the top means any change to any file — a README, a test — rebuilds
  everything below;
- `RUN apt-get update` is cached by its text, not by the state of the mirror, so it can
  serve week-old package lists until something above it changes.

So order instructions from "changes rarely" to "changes on every commit".

## The rewritten Dockerfile

```dockerfile
# syntax=docker/dockerfile:1

FROM golang:1.23-bookworm AS build
WORKDIR /src

# 1. dependency manifests only — this layer survives ordinary code changes
COPY go.mod go.sum ./
RUN --mount=type=cache,target=/go/pkg/mod go mod download

# 2. now the source
COPY . .
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/api ./cmd/api

FROM gcr.io/distroless/static-debian12:nonroot AS runtime
COPY --from=build /out/api /api
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/api"]
```

What each part buys:

| Change | Effect |
|---|---|
| `COPY go.mod go.sum` before `COPY . .` | dependency download cached until manifests change |
| `--mount=type=cache` | module and build caches persist between builds without entering the image |
| separate `runtime` stage | final image has only the binary; no compiler, no sources |
| distroless `nonroot` | no shell, no package manager, non-root user by default |
| `CGO_ENABLED=0` | static binary, so the runtime base needs no libc |

The same pattern applies to other ecosystems: copy `package.json` and the lockfile, run
`npm ci`, then copy the source; copy `Cargo.toml`/`Cargo.lock` and build dependencies
first; copy `requirements.txt` and install before the code.

## Build context and .dockerignore

`COPY . .` copies whatever is in the build context, and checksums are computed over it.
Our context included `.git` (300 MB), local `bin/` artifacts and `.env` files. Any `git
fetch` changed `.git` and busted the cache. The `.dockerignore` we use:

```
.git
**/*_test.go
bin/
dist/
.env*
docker-compose*.yml
Dockerfile*
```

Excluding the Dockerfile itself is fine — the builder reads it separately.

## Targets and debugging

Stages double as build targets. `docker build --target build -t api:build .` stops after
the build stage, which gives a container with the toolchain for poking around. In CI we
also have a `test` stage based on `build` that runs `go test ./...`; it is only built
when explicitly targeted, because BuildKit skips stages the final target does not depend
on.

## Cache in CI

Local builds keep their cache in the builder. CI runners are fresh every time, so the
cache has to be exported and imported:

```sh
docker buildx build \
  --cache-from type=registry,ref=registry.example.net/api:buildcache \
  --cache-to type=registry,ref=registry.example.net/api:buildcache,mode=max \
  -t registry.example.net/api:$GIT_SHA --push .
```

`mode=max` stores layers of all stages, not just the final one; without it the build
stage is never reused. Note that `--mount=type=cache` contents are **not** exported this
way — they live only on the builder — so on ephemeral runners the dependency layer from
step 1 does the heavy lifting.

## Results

| Metric | Before | After |
|---|---|---|
| Image size | 1.4 GB | 91 MB |
| Rebuild after code change (CI, warm registry cache) | 9 min | 40 s |
| Rebuild after `go.mod` change | 9 min | 2.5 min |
| CVE scanner findings | 212 | 3 |

## Reading the build output to find the cache miss

When a build is slower than expected, the plain progress output tells you where the
cache broke:

```sh
docker buildx build --progress=plain . 2>&1 | grep -E "CACHED|^#[0-9]+ \["
```

Steps marked `CACHED` were reused. The first step without that marker is the one whose
input changed; everything after it is a consequence. In our case it was usually one of:

- a `COPY` of a directory that contained generated files (fixed in `.dockerignore`);
- a `LABEL build_date=...` or `ARG GIT_SHA` placed near the top — a value that changes on
  every build invalidates every layer below it;
- a base image tag such as `golang:1.23` that moved to a new patch release upstream, which
  is correct behaviour but surprising; pin by digest (`@sha256:...`) if builds must be
  reproducible and update the digest deliberately.

`docker history <image>` shows the size of each layer of the final image; a large layer
right after a `COPY` usually means something unwanted came along.

## Gotchas we hit

- `COPY --from=build` paths are absolute in the source stage; a relative path is resolved
  against `/`, not the stage's `WORKDIR`.
- distroless has no shell, so `HEALTHCHECK CMD curl ...` cannot work there; we added a
  `/api healthcheck` subcommand instead.
- `ARG` declared before the first `FROM` is visible only in `FROM` lines; redeclare it
  inside the stage to use it in `RUN`.
- Changing a build `ARG` value invalidates the cache from the first instruction that uses
  it, so put version args as late as possible.
