# Product cache in the catalog service

The catalog service keeps product cards in Redis in front of Postgres. A card is the product
row plus prices, stock flags and the main image URL, serialized as JSON.

## Keys and lifetime

The key is `product:{id}:{locale}`. Cards live for 15 minutes; prices change rarely and a
price change also deletes the key explicitly.

## Invalidation

The catalog writes an outbox row on every product change. A worker reads the outbox and deletes
the affected keys for all locales. Deleting is preferred to rewriting: the next reader fills
the cache with fresh data from the primary database.

## Reads

A miss reads Postgres and writes the card back with `SET ... EX`. Reads go to a replica only
for list pages; a single card is read from the primary right after an invalidation so that the
cache is not refilled with stale replica data.

## Monitoring

The dashboard shows hit ratio per locale, the size of the outbox backlog and the number of keys
deleted per minute. A backlog above 10,000 rows pages the on-call engineer.
