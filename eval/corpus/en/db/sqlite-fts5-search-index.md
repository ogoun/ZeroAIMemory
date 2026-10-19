# Using SQLite FTS5 as a search index next to regular tables

How we added full-text search to an app that already stores everything in SQLite, without a
separate search server.

## External content table

Store the text once in the normal table and let the FTS5 table index it:

```sql
CREATE TABLE articles (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL
);

CREATE VIRTUAL TABLE articles_fts USING fts5(
    title, body,
    content = 'articles',
    content_rowid = 'id'
);
```

With `content=`, the FTS table keeps only the inverted index and reads column values from
`articles` when needed (for `snippet()` or `highlight()`). Disk usage roughly halves compared
to a regular FTS5 table that stores its own copy.

The catch: FTS5 does not watch the content table. You must keep the index in sync yourself,
usually with triggers:

```sql
CREATE TRIGGER articles_ai AFTER INSERT ON articles BEGIN
  INSERT INTO articles_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
CREATE TRIGGER articles_ad AFTER DELETE ON articles BEGIN
  INSERT INTO articles_fts(articles_fts, rowid, title, body)
  VALUES ('delete', old.id, old.title, old.body);
END;
CREATE TRIGGER articles_au AFTER UPDATE ON articles BEGIN
  INSERT INTO articles_fts(articles_fts, rowid, title, body)
  VALUES ('delete', old.id, old.title, old.body);
  INSERT INTO articles_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
```

The special `'delete'` command must receive the **old** values exactly; if they differ from
what was indexed, the index silently becomes corrupt. If in doubt, rebuild:
`INSERT INTO articles_fts(articles_fts) VALUES ('rebuild');` and check with
`INSERT INTO articles_fts(articles_fts) VALUES ('integrity-check');`.

## Querying

```sql
SELECT a.id, a.title,
       snippet(articles_fts, 1, '<b>', '</b>', '…', 12) AS excerpt
FROM articles_fts
JOIN articles a ON a.id = articles_fts.rowid
WHERE articles_fts MATCH 'title:backup AND restor*'
ORDER BY rank
LIMIT 20;
```

- `rank` is BM25 by default; `bm25(articles_fts, 5.0, 1.0)` weights title matches higher.
- User input must not go into `MATCH` raw: quotes, `-`, `:` and bare `AND`/`OR` are query
  syntax and cause `fts5: syntax error`. Wrap each term in double quotes, escaping embedded ones.

## Maintenance

FTS5 writes many small b-tree segments and merges them in the background. After bulk loads run
`INSERT INTO articles_fts(articles_fts) VALUES ('optimize');` to merge into one segment.
The `automerge` and `crisismerge` options tune how aggressively merging happens on write.
