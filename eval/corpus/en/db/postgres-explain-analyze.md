---
tags: [postgresql, performance, indexes]
---
# Reading EXPLAIN ANALYZE in PostgreSQL and picking the right index

Working notes from tuning the order history endpoint (from 1.8 s to 12 ms).

## Run it properly

```sql
EXPLAIN (ANALYZE, BUFFERS, SETTINGS)
SELECT id, total, created_at
FROM orders
WHERE customer_id = 4211 AND status = 'paid'
ORDER BY created_at DESC
LIMIT 20;
```

- `ANALYZE` actually executes the query. For `UPDATE`/`DELETE`, wrap it in
  `BEGIN; ... ROLLBACK;`.
- `BUFFERS` shows `shared hit` (from cache) vs `read` (from disk). A plan that looks fine but
  reads 400k buffers is not fine.
- Run it twice; the first run often measures a cold cache.

## What to look at

1. **Estimated vs actual rows** on each node: `rows=12` estimated against `rows=48210` actual
   means the planner is working from bad statistics. Run `ANALYZE orders;`, consider
   `ALTER TABLE ... ALTER COLUMN ... SET STATISTICS 1000` or extended statistics
   (`CREATE STATISTICS ... (dependencies)`) for correlated columns.
2. **loops**: the times shown are per loop. A nested loop inner node with `loops=50000` and
   0.05 ms each is 2.5 s in total.
3. **Rows Removed by Filter**: a large number means the scan read many rows only to discard
   them; an index matching the filter would avoid that.
4. **Sort Method: external merge Disk**: sort spilled to disk; either `work_mem` is too small
   for this query or an index could deliver rows already ordered.

## Our case

Before:

```
Limit (actual time=1795.2..1795.3 rows=20 loops=1)
  -> Sort (Sort Method: top-N heapsort)
     -> Seq Scan on orders (rows=48210) Filter: (customer_id = 4211 AND status = 'paid')
        Rows Removed by Filter: 9351790
```

A composite index matching both the equality filters and the sort order:

```sql
CREATE INDEX CONCURRENTLY idx_orders_cust_status_created
    ON orders (customer_id, status, created_at DESC);
```

After: `Index Scan using idx_orders_cust_status_created`, 20 rows, no sort node, 12 ms.

## Index rules of thumb

- Column order: equality columns first, then the range or sort column.
- A B-tree on `(a, b)` helps queries on `a` and on `a, b`, rarely on `b` alone.
- Partial index when queries always filter on one value: `... WHERE status = 'paid'`.
- `INCLUDE (total)` turns it into a covering index and enables an Index Only Scan,
  provided the visibility map is up to date (vacuum).
- Functions on columns defeat indexes: `WHERE lower(email) = ...` needs an expression index
  on `lower(email)`.
- Every index costs writes and disk. Check `pg_stat_user_indexes.idx_scan` for indexes that
  are never used and drop them.

`pg_stat_statements` is where to find which queries deserve this attention in the first place:
sort by `total_exec_time`, not by mean.
