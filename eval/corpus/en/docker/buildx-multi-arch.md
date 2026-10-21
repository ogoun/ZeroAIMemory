# Building arm64 and amd64 images with buildx

We added Graviton nodes, so images must exist for both `linux/amd64` and `linux/arm64`
under one tag. A multi-platform image is a manifest list: the registry returns the right
variant for the pulling machine.

## Setup

```sh
docker buildx create --name multi --driver docker-container --use
docker buildx inspect --bootstrap
# emulation for foreign architectures, once per host
docker run --privileged --rm tonistiigi/binfmt --install arm64,amd64
```

The default `docker` driver cannot build several platforms at once and cannot hold a
manifest list locally; the `docker-container` driver can.

## Build and push

```sh
docker buildx build --platform linux/amd64,linux/arm64 \
  -t registry.example.net/api:1.8.0 --push .
docker buildx imagetools inspect registry.example.net/api:1.8.0
```

`--load` into the local image store works only for a single platform (unless the
containerd image store is enabled), so multi-platform builds normally go straight to a
registry with `--push`.

## Emulation is slow — cross-compile instead

Under QEMU, our `npm ci` with native modules took 14 minutes for arm64 versus 2 for
amd64. For compiled languages, run the compiler natively and target the other
architecture using the automatic build args:

```dockerfile
FROM --platform=$BUILDPLATFORM golang:1.23 AS build
ARG TARGETOS TARGETARCH
WORKDIR /src
COPY . .
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH go build -o /out/app ./cmd/app

FROM gcr.io/distroless/static-debian12
COPY --from=build /out/app /app
ENTRYPOINT ["/app"]
```

`$BUILDPLATFORM` is the machine running the build, `TARGETARCH` is `amd64` or `arm64`
for each variant. The build stage runs natively for both; only the tiny final stage is
per-architecture.

Alternative for heavy builds: native arm64 runners attached to the same builder with
`docker buildx create --append`, so no emulation at all.

## Pitfalls

- Base images must themselves be multi-arch; check with `imagetools inspect`.
- Binaries downloaded in `RUN curl ...` are often hardcoded to x86_64 — use
  `TARGETARCH` in the URL.
- "exec format error" at container start means an image for the wrong architecture was
  pulled or built.
