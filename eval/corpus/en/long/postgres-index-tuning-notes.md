---
tags: [postgresql, indexes, performance, tuning]
---
# Index tuning on the orders database: notes from a month of query work

The orders database had accumulated indexes the way old houses accumulate furniture: each one was added for a reason, nobody remembered most of the reasons, and getting rid of any of them felt risky. At the start of this work the main orders table had twenty-three indexes, the table itself was 410 GB and its indexes together were 690 GB, and write latency for order updates had crept up by about forty percent over a year. At the same time, a handful of read queries were still slow despite all those indexes. These notes describe how we found which indexes were useful, which ones were missing, what we dropped and added, and the traps we ran into along the way. They are specific to PostgreSQL, but most of the reasoning applies elsewhere.

## Finding out what is used

### Index usage statistics

PostgreSQL counts how many times each index has been scanned since statistics were last reset. That counter is the starting point, but on its own it is misleading in two ways. First, statistics are per server, so an index that is never used on the primary may be used heavily on a replica that serves reporting queries; we had to collect the counters from every replica as well. Second, the counter does not tell you why an index was used, and an index that is scanned a million times a day might be used by a query that would run just as well with a different index. Run this on each server to see indexes on a table with their scan counts and sizes:

```sql
SELECT indexrelname, idx_scan, pg_size_pretty(pg_relation_size(indexrelid)) AS size
FROM pg_stat_user_indexes
WHERE relname = 'orders'
ORDER BY idx_scan ASC, pg_relation_size(indexrelid) DESC;
```

We reset nothing and collected the counters every day for four weeks from the primary and all four replicas, so that monthly jobs and end-of-month reporting would be included. Seven of the twenty-three indexes had zero scans on every server over the whole period. Four more had fewer than a hundred scans per day in total, all from a single internal admin page.

#### Query statistics

To understand which queries used which indexes, we used the statement statistics extension, which aggregates execution counts and timings per normalized query. For the top two hundred queries by total execution time, we ran an explain of a representative example and recorded which indexes the plan used. This was tedious, but it produced the map we needed: for every index, the list of queries that depended on it, with their frequency and cost. An index used by one query that runs once a day and takes ten milliseconds either way is not the same as an index used by the checkout path.

## Dropping indexes

### Duplicate and redundant indexes

Before looking at usage at all, we looked for indexes made redundant by others. An index on `(customer_id)` is redundant if there is also an index on `(customer_id, created_at)`, because the second can serve every query the first can, for equality on the leading column, at a small extra cost. We had three such pairs, plus one exact duplicate: two identical indexes on `(status)` with different names, one created by a migration in the application and one created by hand during an incident two years earlier and never removed. The duplicate alone was 18 GB.

#### Dropping safely

Dropping an index is instant and cannot be undone quickly: recreating a large index takes hours. So we did not drop unused indexes right away. Instead we first made them invisible to the planner, but PostgreSQL has no built-in invisible index feature, so we used a different trick for the first round. For each candidate, we ran the queries from our map that might plausibly use it, on a replica, inside a transaction that dropped the index first and rolled back at the end. Because the drop is transactional, the index is invisible to the planner inside that transaction and comes back intact on rollback. Example:

```sql
BEGIN;
DROP INDEX orders_status_created_at_idx;
EXPLAIN (ANALYZE, BUFFERS) SELECT ... ;
ROLLBACK;
```

This is safe on a replica only in the sense that it is not possible: you cannot run DDL on a physical standby. We actually ran these tests on a restored copy of the production database that was otherwise idle, with production query parameters taken from the statement statistics. On the real primary, a transactional drop takes an exclusive lock on the table until rollback, which would block all writes, so it must never be done there. Eleven indexes passed this check, with no plan in our map changing for the worse, and we dropped them on the primary one at a time over two weeks, using the concurrent form of drop to avoid blocking writes, and watching query latency between each drop.

## Adding indexes

### The slow queries

With the unused indexes gone, we turned to the queries that were still slow. Three accounted for most of the problem. The first was the customer order history page, which fetched the most recent orders for a customer, sorted by creation time, excluding cancelled orders. The second was a warehouse query that found orders in a given status for a given warehouse, older than a cutoff, to detect stuck orders. The third was a reporting query that summed order totals per day for a date range and a sales channel.

#### Partial indexes

The order history query filtered out cancelled orders, which are about eight percent of all orders, and sorted by creation time. The existing index on `(customer_id, created_at)` could serve it, but the executor then had to filter out cancelled rows, and for customers with many cancelled orders it read far more rows than it returned. A partial index that excludes cancelled orders is smaller and returns exactly the rows the query needs in the right order. Because the condition in the index must match the query's condition for the planner to use it, we also changed the query to use exactly the same expression. Example of the index:

```sql
CREATE INDEX CONCURRENTLY orders_customer_recent_active_idx
ON orders (customer_id, created_at DESC)
WHERE status <> 'cancelled';
```

The warehouse query was a better fit for a partial index still. It looks only for orders in three intermediate statuses, which at any moment are less than one percent of the table, because almost every order eventually reaches a final status. A partial index on `(warehouse_id, status, updated_at)` restricted to those three statuses was about 300 MB, compared to tens of gigabytes for a full index on the same columns, and the query went from around four seconds to under ten milliseconds. The interesting thing about this index is that it stays small forever: rows enter it when an order reaches an intermediate status and leave it when the order moves on, so its size tracks the number of orders in flight, not the size of the table.

### Covering indexes

The reporting query read a large number of rows by date and channel and summed the totals. The existing index on `(channel, created_at)` found the right rows, but for each one the executor had to visit the table to read the total, and those visits were random reads across 410 GB. Adding the total as an included column turned the plan into an index-only scan, which reads only the index and the visibility map. Index-only scans depend on the visibility map being up to date, which depends on vacuum: on pages that changed since the last vacuum, the executor still has to visit the table to check visibility. Since most orders older than a few days never change, the visibility map for old data stays clean and the query benefits fully, while for the most recent days it partly falls back to table visits. The query went from about forty seconds to two seconds for a typical month-long range.

#### Building indexes without downtime

All new indexes were built with the concurrent option, which does not block writes but takes longer and can fail, leaving an invalid index behind. One of our builds failed after six hours because of a unique violation that appeared while the index was being built, on an index we had believed was safe to make unique. The invalid index stayed in place, consuming space and slowing writes, until we dropped it. We now check for invalid indexes after every concurrent build:

```sql
SELECT indexrelid::regclass FROM pg_index WHERE NOT indisvalid;
```

## Measuring the result

We compared the four weeks before and after the work. The orders table now has fifteen indexes instead of twenty-three, and their total size dropped from 690 GB to 410 GB. Median write latency for order updates fell by about thirty percent, and the 99th percentile by more, because fewer indexes meant fewer random writes during peak periods. WAL volume dropped by about a fifth, which also reduced replication lag on the replicas during bulk updates. The three slow queries are now fast, and none of the queries in our map got slower beyond noise. Storage costs went down too, though that was never the goal: the reduction in index size freed enough space on the primary that a planned disk expansion was postponed by a year. Backup duration also shrank, since physical backups copy indexes along with tables, and the nightly full backup of the orders database now finishes about ninety minutes earlier than before. On a different note, the work also exposed a monitoring gap: we had no dashboard showing index sizes over time, so the growth from 400 GB to 690 GB over two years had gone unnoticed. Index and table sizes per database are now on the storage dashboard, with a weekly trend line and an annotation for every migration that adds or drops an index.

### Write amplification in numbers

It is worth spelling out why dropping indexes helped writes so much. Every update to an order that changes an indexed column, or that cannot be done as a heap-only tuple update because the page is full, requires an insertion into every index on the table. With twenty-three indexes, a single status change meant up to twenty-three index insertions, each a random write to a different part of the disk. Reducing the number of indexes and lowering the fill factor of the table to leave room for heap-only updates together raised the share of heap-only tuple updates from about thirty percent to over seventy percent, which is where most of the write latency improvement came from.

##### Fill factor

Lowering the fill factor of an existing table only affects pages written after the change, so to benefit from it for existing data we had to rewrite the table, which on 410 GB is not something you do casually. We used an online repacking tool that rebuilds the table in the background and swaps it in at the end with a brief lock. The rewrite took about nine hours on a weekend and needed roughly as much free disk space as the table itself, which we had to provision in advance.

## Keeping it this way

The main risk now is drift: indexes get added during incidents or for one-off reports and never removed. We added three safeguards. Every index on the large tables must be created by a migration in the repository, with a comment naming the query it supports; a weekly job reports indexes that exist in the database but not in the migrations. A monthly job lists indexes with zero scans across all servers over the past thirty days and opens a ticket for review. And the review checklist for migrations that add an index on the orders table now asks for the explain output of the target query before and after, run against a recent production-sized copy rather than a development database, because plans on small data are often completely different from plans on real data.
