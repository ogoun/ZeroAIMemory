---
index: false
---
# Reading GET /index/status and the Index page

`GET /api/v1/index/status` is the first place to look when semantic search "doesn't work".
Any token may call it but sees only its own user; the admin sees the user from `X-Zam-User`
or the sum over everyone.

## Fields

Abridged sketch of the response (exact nesting may differ):

```json
{"vector": {"available": false, "reason": "no_model", "text": "...", "hint": "zam-server models fetch"},
 "device": {"kind": "cpu", "reason": "..."},
 "model":  {"id": "...", "dim": 1024, "collection_model": "..."},
 "qdrant": {"state": "...", "version": "1.19.2", "rss_bytes": 0, "points": 0, "restarts": 0},
 "queue":  {"queued": 0, "running": 0, "failed": 0, "oldest": null},
 "failed_items": [],
 "reindex": {"last": "..."},
 "rebuild_skipped": [], "unavailable_dbs": []}
```

- `vector` — whether the vector channel is on; if not, a reason code plus a human-readable
  explanation and the command to fix it.
- `device` — `cpu` or `cuda`, with the reason CUDA was not used.
- `model` — current embedding model id and the one stored in the Qdrant collection; a mismatch
  means the vector channel stays off until a reindex.
- `qdrant` — supervisor state, version, resident memory of the Qdrant process (`rss_bytes`,
  read from the OS by pid: `GetProcessMemoryInfo` on Windows, `/proc/<pid>/statm` on Linux),
  number of points, restart count. This is the `zam_qdrant_rss_bytes` metric.
- `queue` and `failed_items` — indexing backlog and up to 20 failed jobs with error and attempts.
- `reindex.last`, `rebuild_skipped`, `unavailable_dbs` — last rebuild, databases excluded from
  rebuilding and databases that failed to open.

## WebUI

The same data lives on the "Index" page (`http://<host>:7700/#/admin/index`). It refreshes every
5 seconds, explains in Russian why vector search is off together with the command, and offers
two buttons — rebuild from cached embeddings or full reindex — each behind a confirmation.
Search results carry `vector`/`fts` channel labels, and tree nodes show "queued" and
"indexing error" badges.
