# Timestamps in service databases and APIs

All services store and exchange time as instants in UTC. Local time appears only at the edge,
when a user interface renders a value for a person.

## Database columns

Postgres columns for moments in time use `timestamptz`. Dates without time, such as a delivery
day, use `date`. Plain `timestamp` columns are not used for new tables.

## API format

The API sends and accepts RFC 3339 strings with an explicit offset, for example
`2026-03-29T01:30:00Z`. A value without an offset is rejected with 400 instead of being
guessed.

## Business days

Rules such as "orders placed before 14:00 ship the same day" depend on the warehouse time zone.
The warehouse record stores an IANA zone name (`Europe/Berlin`), and the rule is evaluated by
converting the instant into that zone, never by adding a fixed offset.

## Tests

Tests run with the process time zone set to `Pacific/Chatham`, whose odd offset exposes code
that silently relies on the machine's local time.
