# Docker layer cache in CI

Service images are built in CI on ephemeral runners. Without a cache every build starts from
scratch, so the pipeline imports and exports the BuildKit cache explicitly.

## Dockerfile order

Steps that change rarely come first: the base image, system packages, then the dependency
manifest and the dependency install, and only then the application source. A change in the
source invalidates only the last layers.

## Cache backend

`docker buildx build --cache-from type=registry,ref=registry.local/orders:buildcache
--cache-to type=registry,ref=registry.local/orders:buildcache,mode=max` stores the cache in the
internal registry. `mode=max` also exports intermediate layers of multi-stage builds.

## Cache per branch

Pull request builds read the cache of `main` and of their own branch but write only to their own
branch reference, so experimental branches do not evict the main cache.

## Measuring

The pipeline prints the number of cached steps. A build that suddenly reports zero cached steps
usually means the base image tag moved.
