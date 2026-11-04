# Load balancer health checks

The cloud load balancer sends traffic only to instances that pass its health check. The check
is configured per target group in the infrastructure repository.

## Check settings

The balancer calls `GET /health` on every instance every 5 seconds with a 2-second timeout. Two
failed checks mark an instance unhealthy, three successful ones bring it back. Each balancer node
checks every instance on its own, so the real request rate to `/health` is several times higher
than the interval suggests.

## Status codes

Only 200 counts as healthy. A service that is starting answers 503 until it has loaded its
configuration and warmed its caches.

## Draining

When an instance becomes unhealthy or is deregistered, the balancer stops new requests to it and
lets running requests finish for up to 30 seconds.

## Alerts

An alert fires when fewer than half of the instances of a target group are healthy for two
minutes. A flapping instance — healthy and unhealthy several times an hour — is investigated even
without an alert.
