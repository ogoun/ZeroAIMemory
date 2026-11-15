# Request rate limits at the nginx edge

The edge nginx in front of the storefront and the API applies coarse rate limits per client IP.
They protect the backends from floods; fine-grained limits per customer live in the services.

## Zones

`limit_req_zone $binary_remote_addr zone=api:20m rate=20r/s;` defines a shared zone. A 20 MB
zone holds counters for roughly 320,000 addresses. The login endpoint has its own zone with
`rate=5r/m`.

## Applying a limit

`limit_req zone=api burst=40 nodelay;` in the API `location` lets short bursts through and
rejects the rest. The status for rejected requests is set with `limit_req_status 429;` because
the default is 503.

## Client addresses

Behind the cloud load balancer, `$binary_remote_addr` must be the real client address. The
`real_ip_header` and `set_real_ip_from` directives restore it from `X-Forwarded-For` for the
balancer's address ranges only.

## Dry run

New limits are first deployed with `limit_req_dry_run on;`. Rejections are then only logged, and
the log shows whether normal traffic would be affected.
