# ZeroAIMemory: agent memory protocol

The short rules live in the ZeroAIMemory block of CLAUDE.md / AGENTS.md. This is the detail.

## Connection

- Server address — the `ZeroAIMemoryUrl=` line of the block (or `ZAM_URL`). Token — `ZAM_TOKEN` or `~/.zeroaimemory/config.toml`; it is never stored in CLAUDE.md, AGENTS.md or `.mcp.json`.
- Solution — `.zeroaimemory/solution.toml` (`agent_solution_id`, `name`), created by `zam init`. All clones with this GUID share one memory.
- MCP — `<url>/mcp` (streamable HTTP, `Authorization: Bearer`, `X-Zam-Solution` header). Without HTTP transport — `zam mcp` (stdio proxy). Without MCP — CLI `zam`; without CLI — REST `/api/v1`.

## Search: four token-saving steps

Go to the next step only when the previous one was not enough.

0. **Cache.** `memory_search` first looks for a ready summary. `[cache] "…" — summary …, all current` — use it and stop. `[cache, stale: …]` — reread only the changed sources and update the summary.
1. **Cards** (`mode=brief`): one line per candidate — id, path › section, score, `rev`, date, `~N tok`, snippet. Id prefixes: `n:` note, `i:` insight, `c:` changelog, `r:` release. `(seen)` — already given in this session, do not reread. Rank the cards yourself by your task.
2. **Targeted read** — `memory_read` several ids in one call (up to 20). Per id: `tier=L0|L1|L2`, `section` (heading path `A > B`), `max_tokens`, `if_rev` (unchanged revision — `unchanged` without text). L1 or a section first, full text only when needed. Default response budget — 1500 tokens.
3. **Summary** — `memory_cache_put {query, summary, sources}`: Markdown up to 600 tokens with links; `sources` — ids (and sections) the summary is really based on. The server pins source revisions and invalidates the summary when they change. Long-lived value — `promote` it into a note or an insight.

Every response ends with a `~N tok` line — your spend. Check the cache before a new search on the same topic.

## Writing

- **Notes** — `memory_write {path, content | ops, base_rev?, message?, l0?}`; path `[<space>:/]<path>`. Without `base_rev` — create, with it — replace or patch. Give a note an `l0` — a one-line summary.
- **Solution journals** — `memory_log`:
  - `changelog` — a project change (what and why);
  - `insight` — a finding: `gotcha`, `failure`, `experiment` (with its result), `discovery`; `scope=global` when it is not about this project; `triggers` (`file_glob:**/*.sh`, `command_regex:…`, `error_regex:…`) — when to show the hint;
  - `release` — a release.
- **Editing a record** — `memory_update_entry {id, rev, action}`: `supersede`, `retract` (with a reason), `revisit`, `certify`, `append`, `feedback`. `rev` is required: without it — 428, after someone else's edit — 409 with the current revision. On 409/428 reread the record and retry.
- **Links** — `memory_link {src, type, dst}`: `links_to`, `relates_to`, `depends_on`, `part_of`, `caused_by`, `fixes`, `derived_from`, `mentions`.
- **Secrets** — only `memory_vault` (`get | set | list`). Values never reach search, summaries or logs; do not copy them into notes.

## Certainty

- `certainty`: `axiom` — accepted truth, `theorem` — proven or verified, `hypothesis` — an assumption. Changed with a `reason`.
- Set, change or remove `axiom` **only on the user's direct instruction**. If your own conclusion differs, record a `hypothesis` or `theorem` alongside (with the objection and evidence) and tell the user.

## Proactive recall

- `ZeroAIMemory: …` lines in your context are hints from memory: rules, gotchas, past failures. Take them into account; read by id when needed.
- With hooks (`zam hook …`, installed by `zam init`) hints arrive by themselves: at session start (digest), on a prompt, before a file edit or a command, after an error. Before stopping, the hook reminds you to record the outcome if there were edits but no memory writes.
- Without hooks call `memory_recall` yourself: at the start (`session_start`), before a new file type and after an error (`post_tool` with the error text).

## Spaces

- Solution (GUID from `solution.toml`) — project memory; Knowledge — the user's general knowledge; Rules — rules read at the start of every session (agent writes only if the administrator allowed them); Human — the person's notes, not available to agents.
- Someone else's solution GUID — an access request to its owner; until approved, writes get 403.

## Instruction block

The block in CLAUDE.md / AGENTS.md is marked `<!-- zeroaimemory:begin vN -->`. If the server says the block is outdated, update it with `zam init` (or WebUI "Connect an agent"). `zam init` never touches text outside the block.
