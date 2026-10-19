---
tags: [agents, tokens, cache, protocol]
---
# Agent-side summarisation: the brief → read → cache_put protocol

Design notes on how an agent should consume memory without burning its context window. The
server deliberately runs no LLM: it searches and stores, and the agent — who knows its own task
best — decides what matters and writes the summary. The protocol is scheduled for M4 (MCP,
answer cache) and tuned in M6.

## Why not summarise on the server

The agent reads the result anyway. It is cheaper to read the right pieces once and keep the
summary than to have a second model generate a digest on every request. A shared cache then
makes repeat questions nearly free for every session and machine of the same project.

## The four steps

Each step happens only if the previous one did not give enough.

```
step 0  memory_search ─▶ fresh cached summary?  ── yes ──▶ done (~100–400 tokens)
                         │ no
step 1  candidate cards (mode=brief)                 (~30–50 tokens per card)
                         │ agent picks what it needs
step 2  memory_read [id…] ─▶ only needed sections / tiers
                         │ agent writes the summary
step 3  memory_cache_put ─▶ summary + sources with revisions ─▶ reusable later
        (optional) promote ─▶ summary becomes a note or an insight
```

### Step 0 — cache lookup

`memory_search` first looks for a valid summary of a similar question. A hit is either an exact
match on `(scope_hash, query_norm)` or a semantic one: dense cosine to the stored `query_vec`
of at least 0.92 (configurable). At most two summaries are returned, and the first line looks
like:

```
[cache] "How we deploy the parser service" — summary 2026-10-12, 3 sources, all current (id: c_…)
```

### Step 1 — cards

One line per candidate: kind and id, path and section, score, revision, date, full size in
tokens and one or two lines of snippet. If this session already received the same item at the
same revision, the snippet is replaced by `(seen)` (`session_seen`, `skip_seen=true`). Ten cards
should cost no more than 500 tokens. The agent ranks the cards itself; server reranking is not
needed for this.

### Step 2 — targeted reads

`memory_read` takes several ids at once, each with optional `tier=L0|L1|L2`, `section` (heading
path), `locator` (page or sheet), `max_tokens` (truncation with an "…N more tokens" marker) and
`if_rev` — if the revision has not changed, the answer is just `unchanged`, without text.

### Step 3 — saving the summary

`memory_cache_put` takes `query`, `summary` (Markdown, up to 600 tokens by default) and
`sources` — ids with revisions or sections the summary is actually based on. The server checks
that every source exists and is accessible, records current revisions and hashes and builds
dependency rows for invalidation. `promote: {as: note|insight, …}` turns a valuable summary into
a real note or insight with `derived_from` links to its sources.

## Worked example

An agent is asked to add a health check to the parser service.

1. It calls `memory_search` with "how is the parser service deployed". No cached summary, so it
   gets eight cards. Two look relevant: a note `Solutions/parser/deploy.md` (section "Docker",
   ~340 tokens) and a gotcha insight about LF line endings in shell scripts (~120 tokens). The
   rest are about unrelated services.
2. It calls `memory_read` once with both ids: `tier=L1` for the deploy note to see the outline,
   the insight in full. Then a second read with `section="Docker > Healthcheck"`. Total read cost
   is a few hundred tokens instead of the full multi-thousand-token note.
3. It writes a 150-token summary and calls `memory_cache_put` with the query, the summary and
   both sources pinned to their current revisions.
4. Next day another session on a different machine asks "parser deployment steps". The
   semantic match on `query_vec` clears 0.92, the sources are unchanged, and the cached summary
   comes back as the first line — the whole answer for roughly 200 tokens.
5. A week later someone edits the "Docker" section. The next lookup marks the entry `stale`,
   lists the deploy note as changed, and the agent rereads only that section.

## Cache validity

- **On read.** Revisions and hashes of all sources are compared in one SQL query. Any mismatch
  marks the entry `stale` with the list of changed sources, so the agent can refresh only that
  part.
- **On write.** Editing, deleting, superseding or retracting a source invalidates dependent
  entries through the reverse index `cache_deps`. A new relevant source also counts: when new
  chunks are indexed, their dense vectors are compared with `query_vec` of live entries in the
  same scope, and at cosine ≥ 0.80 the entry becomes `stale: new_sources`.
- **Lifetime.** TTL 30 days without hits, LRU cap of 2000 entries per user. Promoted entries are
  never evicted.

## Cross-session reuse

Lookup goes through the current session first, then summaries from other sessions of the same
project (`cache.cross_session`, on by default). A foreign summary is labelled with author
session, machine and date, and the agent decides whether to trust it. The reasoning: a summary
cost someone tokens, and freshness is guaranteed by the source check, not by who wrote it.

## Token-saving rules baked into the instructions

- MCP answers in compact Markdown by default, not JSON.
- Default budgets: 800 tokens for search, 1500 for reads.
- L0/L1 and the outline first; full text only when unavoidable.
- Read several ids per call.
- Never resend what the agent already has (`seen`, `if_rev`).
- Check the cache before searching the same topic again.
- Every answer ends with a `~N tok` line so the agent sees its spend.

## Pitfalls to keep in mind

- A summary is only as good as its declared sources. If the agent lists notes it never read,
  invalidation tracks the wrong things; the instructions insist on listing only what the
  summary is really based on.
- Semantic hits at 0.92 can still be a slightly different question. The cached line shows the
  original query, so the agent can reject a near miss and fall through to step 1.
- Do not treat the cache as long-term knowledge. Anything worth keeping beyond the TTL should be
  promoted into a note or an insight, where it gets revisions and links.

## Measuring it

M4 adds a test "source changed → entry becomes stale" and measures token spend on eval
scenarios. The M6 acceptance criterion is at least 30% fewer tokens for a typical scenario
compared with M4, with the same answer completeness. Answer-cache lookup with the freshness
check targets ≤ 30 ms plus query embedding.
