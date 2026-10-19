---
tags: [docker, compose]
---
# Compose: depends_on does not wait for the database to be ready

Symptom: `docker compose up` on a fresh machine, the API container exits immediately with
"connection refused" to Postgres, then works on the second `up`. Classic race: the
database container was *started*, but Postgres inside it was still initializing.

Plain `depends_on: [db]` only orders container creation and start. It knows nothing about
readiness. The fix is a healthcheck on the dependency plus the long form of `depends_on`.

```yaml
services:
  db:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: example
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres -d postgres"]
      interval: 5s
      timeout: 3s
      retries: 10
      start_period: 20s
    volumes:
      - pgdata:/var/lib/postgresql/data

  migrate:
    build: .
    command: ["./api", "migrate"]
    depends_on:
      db:
        condition: service_healthy

  api:
    build: .
    depends_on:
      db:
        condition: service_healthy
      migrate:
        condition: service_completed_successfully
    restart: unless-stopped

volumes:
  pgdata:
```

Conditions available: `service_started` (the default), `service_healthy`,
`service_completed_successfully` (for one-shot jobs like migrations).

## Healthcheck details

- `start_period` is a grace window: failures during it do not count against `retries`,
  but a success marks the container healthy immediately.
- `CMD` runs the binary directly, `CMD-SHELL` goes through `/bin/sh -c`. Images without a
  shell (distroless, scratch) need `CMD` with a binary that exists in the image — `curl`
  usually does not.
- Status is visible in `docker ps` (`healthy`, `unhealthy`, `starting`) and in
  `docker inspect --format '{{json .State.Health}}' <container>`, which includes the last
  few probe outputs. That output is the first place to look when a container sticks in
  `starting`.
- Outside of compose's `depends_on`, plain Docker does **not** restart an unhealthy
  container. The status is informational unless an orchestrator acts on it.

## Still retry in the app

Healthchecks fix startup ordering, not runtime outages. The database can restart later;
the API should retry connections with backoff instead of exiting on the first failure.
