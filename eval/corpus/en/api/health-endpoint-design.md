# Health endpoints of backend services

Every backend service exposes `GET /health` for load balancers, uptime checks and the deploy
pipeline. The endpoint is part of the service contract and is documented in its API spec.

## Response

The response is JSON with `status` (`ok`, `degraded`, `down`), the service version, the commit
and a list of dependencies with their own status. HTTP status is 200 for `ok` and `degraded`
and 503 for `down`.

## Dependencies

A dependency is reported as `degraded` when the service can still answer part of the requests
without it, for example when the recommendation service is down and product pages render
without recommendations.

## Access

The endpoint needs no authentication but returns the dependency list only to requests from the
internal network. External callers see only `status`.

## Deploy checks

The deploy pipeline waits until `/health` reports the new version with `ok` on every instance
before it shifts traffic.
