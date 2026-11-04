# ZeroAIMemory: agent memory protocol

The short rules live in the ZeroAIMemory block of CLAUDE.md / AGENTS.md. This is the detail.

## Connection

- Server address — the `ZeroAIMemoryUrl=` line of the block (or `ZAM_URL`). Token — `ZAM_TOKEN` or `~/.zeroaimemory/config.toml`; it is never stored in CLAUDE.md, AGENTS.md or `.mcp.json`.
- Solution — `.zeroaimemory/solution.toml` (`agent_solution_id`, `name`), created by `zam init`. All clones with this GUID share one memory.
- MCP — `<url>/mcp` (streamable HTTP, `Authorization: Bearer`, `X-Zam-Solution` header). Without HTTP transport — `zam mcp` (stdio proxy). Without MCP — CLI `zam`; without CLI — REST `/api/v1`.
- Hooks (`zam hook …`) are installed by `zam init` for Claude Code and Codex. Other agents (Cursor, Gemini CLI, Copilot, Windsurf, Cline, …) use MCP and this protocol: everything the hooks do is available as MCP calls (section "After compaction and for subagents").

## Search: four token-saving steps

Go to the next step only when the previous one was not enough. Before a task — `memory_search`; before research — over past experiments and failures (`insight_kinds=[experiment,failure]`) so you do not repeat them.

0. **Cache.** `memory_search` first looks for a ready summary. `[cache] "…" — summary …, all current` — use it and stop. `[cache, stale: …]` — reread only the changed sources and update the summary.
1. **Cards** (`mode=brief`): one line per candidate — id, path › section, score, `rev`, date, age ("N дн. назад", N days ago), `~N tok`, snippet. Id prefixes: `n:` note, `i:` insight, `c:` changelog, `r:` release, `t:` term, `k:` task, `q:` question, `v:` Vault record. `(seen)` — already given in this session, do not reread. "возможно устарело: изменился <path>" (possibly outdated: <path> changed) — the file the knowledge is about changed after it was recorded: check against the code. Rank the cards yourself by your task.
2. **Targeted read** — `memory_read` several ids in one call (up to 20). Per id: `tier=L0|L1|L2`, `section` (heading path `A > B`), `max_tokens`, `if_rev` (unchanged revision — `unchanged` without text), `since_rev` (you know revision N — get only the diff since it). L1 or a section first, full text only when needed. Default response budget — 1500 tokens.
3. **Summary** — `memory_cache_put {query, summary, sources}`: Markdown up to 600 tokens with links; `sources` — ids (and sections) the summary is really based on. The server pins source revisions and invalidates the summary when they change. Long-lived value — `promote` it into a note or an insight.

Every response ends with a `~N tok` line — your spend. Check the cache before a new search on the same topic. Search filters: `kinds` (tasks — only with an explicit `kinds: [task]`), `status`, `certainty`, `insight_kinds`, `decided_by`, `file`.

## Writing

- **Notes** — `memory_write {path, content | ops, base_rev?, message?, l0?}`; path `[<space>:/]<path>`. Without `base_rev` — create, with it — replace or patch. Always give notes an `l0` — a one-line summary. **Verified commands** (build, test, deploy commands that really worked) — notes with `verified_at: YYYY-MM-DD` (and `verified_at_commit: <sha>`) in the frontmatter: the session digest lists them as "Verified commands", and their age counts from the check date. Rechecked — update `verified_at`.
- **Journals** — `memory_log {kind, …}`:
  - `changelog` — a project change (what and why);
  - `insight` — a finding: `gotcha`, `failure`, `experiment` (with its result), `discovery`, `decision` (see below); `scope=global` when it is not about this project; `triggers` (`file_glob:**/*.sh`, `command_regex:…`, `error_regex:…`) — when to show the hint. After fixing an error, record a `gotcha`/`failure` with `error_regex` — the hook reminds you when it sees an "error → fix" pair;
  - `release` — a release;
  - `term`, `task`, `question` — glossary, tasks, questions for the human (sections below).
- **Editing a record** — `memory_update_entry {id, rev, action}` (409 — reread): `supersede`, `retract` (with a reason), `revisit`, `certify`, `append`, `feedback`; for tasks — `status`, `claim`, `release`; for questions — `answer`; for ideas — `propose`, `decide` (section "Ideas"). `rev` is required (except `feedback`, `claim`, `release`, `answer`): without it — 428, after someone else's edit — 409 with the current revision. On 409/428 reread the record and retry.
- **Links** — `memory_link {src, type, dst}`: `links_to`, `relates_to`, `depends_on`, `part_of`, `caused_by`, `fixes`, `derived_from`, `mentions`, `contradicts`.
- **Secrets** — only `memory_vault` (`get | set | list`). Values never reach search, summaries or logs; do not copy them into notes.
- A write response may carry `warnings` (`term_alias`, `decision_near_axiom`, …) — read them and fix the record when needed.
- **A report for the user** — `zam report` (section "Reports").

## Reports

A report is an outcome of work **for the user**: research, an experiment, model training, a comparison of options (or on the user's request). The user reads it in WebUI (the solution's "Reports" journal) with formulas, Mermaid diagrams, tables, code and images, and exports it to PDF.

- **Write** — `zam report <file.md> [--title T] [--tag X]… [--area A] [--ref commit:<sha>|file:<path>|note:<id>|task:k:<id>]… [--key K]`. Without `--title` — the first `#` heading of the file. The answer is `Отчёт: <url>`: pass the link to the user.
- **Images and files** — relative links from the file's directory: `![loss curve](figs/loss.png)`, `[data](data/run.csv)`; a path with a space — `<figs/my plot.png>` or `%20`. The CLI uploads them in one request with the body. Images — PNG, JPEG, WebP, GIF, SVG; other files are offered for download. Not allowed: `..`, an absolute path, `\`, an external or `data:` image, `![[…]]`, a link to a directory (`[code](src/)`); a file without a link from the body is rejected.
- **Limits** — up to 64 files, 200 MiB in total; a single file — up to the server's blob limit.
- **MCP** — `memory_log {kind: "report", title, body, images: [{name, data}]}`: `name` — the path from the body link, `data` — base64; body with files — up to 16 MiB, larger — `zam report`.
- **Immutable** — a report cannot be fixed or extended: a repeat is a new report. `--key K` (`idempotency_key`) protects against a duplicate when the same command is retried.
- **Agents do not read reports** — they are absent from search, `memory_read`, the digest and hints. Conclusions you need in later sessions go separately into an `insight` (`experiment`, `failure`, `decision`).

## Tasks and handing work over

A task (`task`) is work state that survives context compaction and a change of session or agent.

- **Create** — `memory_log {kind: "task", title, body: plan, data: {next_step?, owner?: agent|human, estimate_min?, links?}}` → `k:<id>`. CLI: `zam task new`.
- **Take it** — claim before work: `memory_update_entry {id: "k:<id>", action: "claim", ttl_min?}` (120 min by default, up to 1440). A live claim by someone else — 409 `task-claimed` with `claimed_by`: do not duplicate the work, pick another task or ask the human. The claim is advisory, it does not forbid edits. Done or dropped — `release`.
- **Progress** — `memory_update_entry {id, rev, action: "status", status: in_progress|blocked|done|dropped, next_step?, blocked_by?}`. Update `next_step` after every notable step: it is the first thing the next session sees. `blocked` — with `blocked_by`. From `done`/`dropped` — only reopening to `open` with a `reason`.
- **The session digest** has a "Tasks" section: `in_progress` and `blocked`, the ones claimed by this session first. Continue from `next_step`.
- **Estimates** — from the actual durations of similar tasks: closed tasks carry `duration_min` (`zam task list --status done`, REST `GET /spaces/{space}/entries?kind=task&status=done`); find similar ones with `memory_search {filters: {kinds: [task], status: [done]}}`.
- Before stopping after edits, the hook reminds you to update `next_step` of the claimed task.

## Questions for the human

A question (`question`) is something only the human decides. The answer becomes a decision in memory.

- **Ask** — `memory_log {kind: "question", title: question, body: context, data: {options: [{key, text}] up to 8, recommendation?: key}}` → `q:<id>`. The same question is already open or answered — 409 `possible-duplicate` with candidates: read the answer, do not ask again. Open questions are listed in the digest.
- **Answer in WebUI** (the "Questions" page) is given by the human; answers of the last 14 days reach the digest as "Human answers".
- **Answer in chat.** The human answered you in the conversation — record it: `memory_update_entry {id: "q:<id>", action: "answer", option? | text?, by: "human", channel: "chat"}` (no question yet — ask it first). `by` is required: without it — 400 `by-required`. With `by: human` the decision gets `certainty: axiom`. CLI: `zam question answer q:<id> --option … --by human|agent`.
- **`by: human` only if the human really answered.** You decided yourself (no answer, the human asked you to choose, the answer is your conclusion) — `by: agent`: the decision gets `hypothesis`. Never attribute your own choice to the human.
- In one operation the server sets the question to `answered` and creates an insight `decision` ("question → answer") linked by `derived_from`; `decision_id` is in the response.
- **No longer relevant** — `memory_update_entry {id, rev, action: "status", status: "obsolete", reason}`.

## Ideas

An idea (`idea`, reference `w:`) is something the human wants to do but has not started yet. Unlike a task, an idea has no plan, `next_step` or claim: the work on an idea happens in the task that appears after the human answers “start”.

- **Record** — `memory_log {kind: "idea", title, body: the intent, data: {origin, priority?, horizon?, links?}}` → `w:<id>`; CLI `zam idea new`. `origin` is required: dictation “write down an idea …” → `origin: human`; your own thought for later (for example, a deferred review remark) → `origin: agent` — such an idea is never proposed, the human adopts it in WebUI. `priority` — `low|normal|high`, `horizon` — a “not before” date (`YYYY-MM-DD`, UTC). Do not rewrite a human's idea (only `append`) and do not delete it.
- **Propose.** Ideas arrive as the digest section `### Идеи` (up to 3, only at the first start of a session) and as a “an idea fits the task” hint line for a prompt. Propose one to the human: `memory_update_entry {id: "w:<id>", action: "propose", text?: why now}` (action=propose) — the server asks a question `q:` with the options “start / postpone / decline”. One idea — one showing per session; declined ideas and ideas postponed until `horizon` do not arrive.
- **Answer in chat** — `memory_update_entry {id: "q:<id>", action: "answer", option, by: "human"}`: `option` — `accept` (start), `defer` (postpone), `decline`; “postpone until …” — plus `horizon`. A decision without a question — `action: "decide", decision: accept|defer|decline|reopen`. The human decides: `by: human` — only their words; `by: agent` — only `decline` of your own idea.
- **Without the answer “start”, do not start.** “Start” creates, in one operation, a task `k:` linked to the idea with `derived_from` — then follow the task protocol; the task is `done` — the idea is `done` too.
- **Shared Knowledge ideas** — do not propose them yourself: only when the human asks (`memory_search {filters: {kinds: [idea]}}`, CLI `zam idea list --knowledge`) or when they are explicitly linked to the current work — then the server sends the hint.
- **Switch** — `ideas.proactive` (WebUI “Agent connection → Hook hints”): off — no section and no idea hints.

## Glossary

A term (`term`) is the canonical name of a project or domain concept.

- **New concept** — `memory_log {kind: "term", title: name, data: {definition (up to 300 characters, required), aliases?, not_to_confuse?, code?: [identifiers]}}` → `t:<id>`. The name or an alias is taken by another active term — 409 `term-conflict`: refine it or replace the old one (`supersede`).
- **Write with canonical terms.** An alias instead of the name in a record gives a `term_alias: "alias" → "name"` warning.
- Search expands the query with aliases by itself (`expanded` in the response); `[[Name]]` in a note with no note of that name links to the term. The glossary — `memory_search {filters: {kinds: [term]}}`, CLI `zam glossary [<term>]`; a term met in a prompt arrives as a hint with its definition.

## Decisions and contradictions

- **A decision (ADR)** — `memory_log {kind: "insight", data: {kind: "decision", decision: {context?, options?, chosen?, rationale?, consequences?, decided_by?: human|agent, revisit_if?}}}`. The human's decisions — `filters.decided_by: human`.
- **Certainty** `certainty`: `axiom` — accepted truth, `theorem` — proven or verified, `hypothesis` — an assumption. Changed with a `reason`.
- Set, change or remove `axiom` **only on the user's direct instruction**. If your own conclusion differs, record a `hypothesis` or `theorem` with the objection and evidence, link it to the axiom with `contradicts` (the record's `links` or `memory_link {src: <your record>, type: "contradicts", dst: <axiom>}`) and tell the user. Object to a `theorem` the same way: a `hypothesis` with `contradicts` to it. Results never rank a record above an active, more reliable record (`theorem`/`axiom`) it contradicts.
- `warnings: ["decision_near_axiom: i:<id> …"]` on a decision write — an axiom on the same topic exists: check that you do not contradict it.

## File references

- Tie code knowledge to files: `refs: {files: [{path}]}` in `memory_log` and `memory_update_entry` (`supersede`, `append`); for notes — `refs:` (a list of paths) in the frontmatter. The path is relative to the solution root, with `/`. CLI `zam log … --ref <path>` and `zam write --ref <path>` add the content hash and the commit themselves.
- The file changed after the record — the reference becomes `stale`, the card says "возможно устарело: изменился <path>". Checked and still true — `zam refs verify <path>` (or rewrite the record with the same paths); wrong — `supersede`.
- "What memory knows about a file" — `memory_search {filters: {file: "<path>"}}`, CLI `zam links file:<path>`. With hooks the file hint arrives by itself on read and before an edit. `zam refs check [--since <commit>]` checks all references of the solution against the working copy.
- Knowledge about a single function — a symbol reference `path#Type::method` (section "Code map"): an edit of nearby code does not make it outdated.

## Code map

The solution's code map — files and symbols (functions, methods, types) with signatures, ordered by their links (PageRank over references between files). The `zam` client builds it (tree-sitter: Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, Java, C#), the server stores it. Memory knowledge is still found with `memory_search`: symbols are absent from search, hints and summaries.

- **When to call it** — before working in an unfamiliar part of the code, before an edit with unclear consequences, when you need to find where a name is defined. Do not read whole files just to get oriented — the map first.
- **Map** — `memory_code {action: "map", focus?, budget_tokens?, path?}`, CLI `zam code map [--focus <path|name>]… [--task <text>] [--tokens N]`: the key files and symbols with signatures within a budget (1000 tokens by default; `~N tok` at the end). `focus` — `{paths: [task files], idents: [names], text: "task text"}` (`text` up to 2000 characters): the map lifts these files and what they reference. `path` — a subtree only. `warning: rank pending` — ranks are being recomputed, the map uses the previous ones.
- **Symbol** — `memory_code {action: "symbol", query: "name" | "path#qual", path?}`, CLI `zam code sym <name|qual>`: a single exact match — a card (definition, signature, who references it, what its file references, attached knowledge), several — a list of `path#qual` addresses; narrow with `path` or an address. `refs`, CLI `zam code refs` — only who references it (files and reference counts by name).
- **Impact of an edit** — `memory_code {action: "impact", path | files: [{path, symbols?: [{qual, body_hash}]}]}`; CLI `zam code impact [--since <commit>]` computes the working copy hashes itself. The answer — changed and removed symbols, consumer files one step away and knowledge worth rechecking; a path without symbol hashes — all knowledge about the path.
- **Symbol reference** — `path#Type::method` (`qual` from the map or the card, the container joined with `::` in every language): `refs: {files: [{path: "src/a.rs#Foo::bar"}]}` in `memory_log`, `refs: ["src/a.rs#Foo::bar"]` in a note's frontmatter, `zam log … --ref src/a.rs#Foo::bar`. Such a reference becomes outdated only when the body of this symbol changes — `stale` with reason `symbol_changed`; the symbol removed or renamed — `symbol_removed`; an edit of a neighbouring function does not touch it (a file reference becomes outdated on any edit of the file).
- **Sync** — `zam code sync` (`--full` — from scratch): file fingerprints, only changed files go to the server. With Claude Code and Codex hooks the map syncs by itself in the background from `session-start` and `checkpoint` — at most every `code.auto_interval_min` (30 min) or when `HEAD` changes; the session digest shows a "карта кода: N файлов, M символов…" (code map: N files, M symbols) line. `zam code status` — the snapshot, skipped files with reasons.
- **Switches** — the `code_map` hint type (client `[hooks] code_map`, "Hook hints" in WebUI): turns off the digest line, the `### Код` section of the subagent package and the background sync; explicit `memory_code` and `zam code` keep working. `code.auto = false` — no background sync, `code.enabled = false` — no map.
- **What reaches the server** — path, language, file hash; for symbols — name, `qual`, kind, lines, body hash; references aggregated by name. Code bodies never leave the machine. The signature (the first line of the definition, up to 160 characters; the server masks secrets) — only with `code.send_signatures = true` (the default); `false` — a map of names and kinds.

## After compaction and for subagents (without hooks)

With Claude Code and Codex hooks this happens by itself: `SessionStart` with `source: compact | resume` brings the recovery package with the session journal summary, `SubagentStart` — the package for the subagent's task. Without hooks (or when the package is not in your context) call it yourself:

- **At the start** — `memory_recall {event: "session_start"}` (digest).
- **After context compaction or resume** — `memory_recall {event: "recovery"}`: tasks with `next_step`, human answers, decisions of the last 7 days, then the digest. Continue from `next_step` of the claimed task.
- **A task for a subagent** — call `memory_recall {event: "brief", task: "<task>"}` and put the package into the task; add the line "first call — `memory_recall {event: "brief", task: …}`" to the task itself. The package: rules, `axiom`/`theorem` decisions, terms, gotchas and file knowledge for the task.
- **Before a new file type and after an error** — `memory_recall` (`prompt`, or `post_tool` with the error text).
- Hook hint switches (`hooks.*`) do not affect explicit MCP calls.

## Environment in Vault

- Hosts, stands, environments — Vault records of types `host` and `environment` with open fields `purpose`, `address`, `limits`, `notes` (a secret value is optional). The session digest lists them in the "Environment" section (without secrets).
- Find — `memory_vault {action: "list", name: "<word>"}`; open fields and the value (if any) — `memory_vault {action: "get", id: "v:<id>"}` — the reveal is audited. Create or extend — `memory_vault {action: "set", name, type: "host", purpose?, address?, limits?, notes?, value?}` (the value is optional; an update changes only the fields you pass), CLI `zam vault add <name> --type host --purpose … --address … --limits …` (a value only with `--secret-stdin`) or WebUI.
- Learned something important about the environment (an address, stand limits) — record it in Vault, not in a note; passwords and keys — only as the record value.

## Proactive recall

- `ZeroAIMemory: …` lines in your context are hints from memory: rules, gotchas, past failures, tasks, human answers, file knowledge. Take them into account; read by id when needed.
- With hooks (`zam hook …`, installed by `zam init`) hints arrive by themselves: at session start (digest), on a prompt, before a file edit or a command, on a file read, after an error, at subagent start. The same error for the third time — a hint to check memory. Before stopping, the hook reminds you to record the outcome if there were edits but no memory writes.

## Spaces

- Solution (GUID from `solution.toml`) — project memory; Knowledge — the user's general knowledge; Rules — rules read at the start of every session (agent writes only if the administrator allowed them); Human — the person's notes, not available to agents.
- Someone else's solution GUID — an access request to its owner; until approved, writes get 403.

## Instruction block

The block in CLAUDE.md / AGENTS.md is marked `<!-- zeroaimemory:begin vN -->`. If the server says the block is outdated, update it with `zam init` (or WebUI "Connect an agent"). `zam init` never touches text outside the block; a repeated `zam init` replaces the previous ZeroAIMemory hooks with the new ones.
