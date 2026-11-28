# Task: move the project into ZeroAIMemory memory

Move everything accumulated for the project into memory: the agent's memory, session history, the owner's instructions and decisions, gotchas, tasks, terms, documentation, the code map — so that a new session of any agent continues from memory. Solution: {solution}. Server: `{url}`. The work is long and token-heavy: read purposefully, write in batches; collect questions for the owner for the report, do not stop.

## 1. Connection

- First — `zam guide`: the protocol (record kinds, fields, errors); this task relies on it.
- `zam status` from the project root shows the solution and the block — the project is connected, go to step 2. Otherwise — `{init}`. `zam` takes the address and token from `~/.zeroaimemory/config.toml`; this task has no token: if none is found, ask the owner to run `zam init` from the "Agent setup" page or to set `ZAM_TOKEN`.
{?claude}- An existing `CLAUDE.md` is kept: `zam init` adds only the block. The server address must not get into git — `--url-to local` (the address goes to `CLAUDE.local.md`, which `zam init` excludes from git itself).
{?codex,agents}- An existing `AGENTS.md` is kept: `zam init` adds only the block. The server address must not get into git — `--url-to home` (the address goes to `~/.zeroaimemory/config.toml`).
{?claude}- Into `.git/info/exclude` — the untracked `/.mcp.json`, `/.claude/settings.json`, `/.zeroaimemory/`. Do not commit a tracked file changed by `zam init` (`CLAUDE.md`, `.claude/settings.json`) — a question for the owner.
{?codex}- Into `.git/info/exclude` — the untracked `/.codex/hooks.json`, `/.zeroaimemory/`. Do not commit a tracked file changed by `zam init` (`AGENTS.md`, `.codex/hooks.json`) — a question for the owner. Codex MCP lives in `~/.codex/config.toml`, `zam init` prints a sample.
{?agents}- Into `.git/info/exclude` — the untracked `/.zeroaimemory/`. Do not commit a tracked `AGENTS.md` changed by `zam init` — a question for the owner. Connect your agent's MCP per `zam guide` ("Connection").
- MCP tools `memory_*` appear when a session starts. If there are none — the `zam` CLI (`zam <command> --help`) or a batch through stdio `zam mcp` from the root: JSON-RPC lines `initialize` (`protocolVersion` `2025-03-26`), `notifications/initialized`, then `tools/call` `{name, arguments}`, one line per call; the answer is a line with the same `id`. Calls of a batch run in parallel: dependent ones (links to new records) go in the next batch. Give every record an `idempotency_key` (CLI `--key`) like `migrate-<kind>-<label>`: a retry after a failure creates no duplicates.

## 2. Inventory of sources

First an inventory without writing: source, number of files, size, period.
{?claude}- **Claude Code memory** — `~/.claude/projects/<folder>/memory/` (index `MEMORY.md`); `<folder>` is the root path with everything except Latin letters and digits replaced by `-` (`C:\Work\Proj` → `C--Work-Proj`); folders with the same prefix (worktrees) — too. In the repository — `CLAUDE.md`, `CLAUDE.local.md`, `.claude/`.
{?claude}- **Session logs** — `*.jsonl` in the same folder, one file per session.
{?codex}- **Codex memory** — the project's `AGENTS.md` and nested ones, `~/.codex/AGENTS.md` (the owner's general instructions — Rules candidates), notes and plans in the repository.
{?codex}- **Session logs** — `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` whose `session_meta.payload.cwd` (the first line) is inside the project.
{?agents}- **Agent memory** — `AGENTS.md` (and nested ones), the agent's rules and notes in the repository (`.cursor/rules/`, `.github/copilot-instructions.md`, `GEMINI.md`, `.clinerules` etc.), plans.
{?agents}- **Session logs** — if the agent keeps them as files; `zam sessions snapshot` reads only Claude Code and Codex, others — by script (step 4).
- **Documentation** — README, `docs/`, ADRs, CHANGELOG, plans, research and audit reports; **git** — `git log` by milestones (tags, merges).
- **Already in memory** — `zam tree`, `memory_search` on key topics: if present — do not duplicate, extend.

## 3. Mapping onto memory kinds

One piece of knowledge — one record: merge duplicates; on a contradiction the newer and more reliable wins, a disputed one is a question for the owner. Write source texts with `reject_secrets: true` (`memory_write`; `zam write|import --reject-secrets`): a text with a secret is not stored even as a revision — remove the value and retry. Journal records (`insight`, `task`, `term`, `changelog`) have no such flag — remove secrets from their text yourself.
- **The owner's instructions and decisions** — `insight` with `certainty: theorem` and `data: {kind: "decision", decision: {context, chosen, rationale, decided_by: "human"}, evidence: {checked: "<source>", method: "agent memory migration", result: "<gist>"}}`; not `axiom` — only the owner sets it. A rejected approach — `decision` with `options` or `failure`.
- **Rules for all projects** (behaviour, not a project fact) — as Rules candidates: `memory_write {path: "rules:/<path>", content, candidate: {reason, basis}}` (`zam rules candidate`); the human accepts them in WebUI. Do not duplicate rules from the digest.
- **Gotchas and failures** — `insight` `gotcha`/`failure` with triggers (`file_glob` — a path mask from the root, `error_regex`, `command_regex`, `keyword` — a whole word: "commission" does not match "commissions") and `refs.files`. A wide trigger (`**/*`, a common word) is noise.
- **Architecture, deployment, research, audits, reviews** — notes in topic folders (`architecture/`, `research/`…), the date in the name (`2026-10-03-contour`), `l0` (a one-line summary) in the frontmatter, links — `[[path]]` and `memory_link` (`zam link`). A folder of .md files is easy to load with `zam import <dir> --reject-secrets` (first `--dry-run`); a note and a folder with the same name are incompatible.
- **Entry notes** — `overview` (what the project is, where things are), `status` (done, in progress), `environment` (build, tests, run; verified commands — with `verified_at: YYYY-MM-DD`) — at the solution root: the digest shows up to 10 roots, notes before folders.
- **Open work** — `task` (the plan in `body`, `data.next_step`; waiting for the owner — `owner: human`), work in progress — then `status: in_progress`. Finished work — `changelog` (`data.kind`: `feat|fix|refactor|docs|research|ops|other`, the date at the start of the title: `YYYY-MM-DD: …`).
- **Terms** — `term`: `definition` up to 300 characters, `aliases` (for an abbreviation — also lowercase), `not_to_confuse` — an array.
- **Hosts, addresses, credentials** — Vault only: `zam vault set <name> --type host --purpose … --address …`, a secret — `--secret-stdin`, shared across projects — `--global`. In notes — the record name, without addresses and secrets.

## 4. Sessions

- Do not read logs whole (they reach hundreds of megabytes). For each one except this session's — `zam sessions snapshot --transcript <file>` (a long one — `--tail-kb 16384`): a snapshot without an LLM, with secrets masked, goes to the server; read it — `zam sessions show <id>`. A snapshot holds only the tail (5 requests, 20 files, 10 commands, the summary); "nothing to take" — an empty log.
- History — by script, line by line, writing to a file in the temporary directory, not into the context: dates, the owner's requests, edited files, number of commands; mask secrets and addresses right away.
{?claude}  A request is a `type: "user"` line with text in `message.content`, without `isMeta`, `isSidechain`, `tool_result` and `<command-…>`/`<system-reminder>`; time — `timestamp`; edits — `tool_use` `Edit`/`Write` with `input.file_path`.
{?codex}  A request is `event_msg` with `payload.type: "user_message"` (`payload.message`); time — `timestamp`; edits — `apply_patch`, commands — `function_call`.
- From the extract — notes `work-history/YYYY-MM` (per day: what the owner asked for, what was done, the outcome) and `work-history/overview`; whatever the agent's memory lacks — into decisions, gotchas, tasks (step 3).

## 5. Code map

- `zam code sync` from the root, then `zam code status`: files by language, symbols, skips — into the report.
- Foreign code (vendored, generated) among the "central" symbols — `exclude` masks in `[code]` of `.zeroaimemory/config.toml` and `zam code sync` again; `.gitattributes` — only with the owner's consent.
- Knowledge about one function — a `path#Type::method` reference in `refs`.

## 6. Verification

- Digest — `memory_recall {event: "session_start"}`: overview and status, tasks with `next_step`, environment, rules, gotchas.
- 3–5 searches on key topics (`zam search "<topic>"`): what is needed — in the first places, otherwise refine `l0`, a term, an alias.
- A gotcha: `zam triggers sync`, then from the root feed `zam hook pre-tool` on stdin `{"hook_event_name":"PreToolUse","session_id":"migrate-check","tool_name":"Edit","tool_input":{"file_path":"<full path of a file with a gotcha>"}}`: the answer has `ZeroAIMemory: для <mask>: …`; and `memory_recall {event: "brief", task: "<a task about that file>"}` — the gotcha is in `### Ловушки`.

## 7. Report to the owner

`zam report <file.md> --title "Project moved into memory"` — the link to the owner; for yourself — `changelog` (`data.kind: docs`). In the report:
- how much was moved: notes, decisions, gotchas (triggers), tasks, changelog, terms, Rules candidates, Vault, links, the code map;
- sessions: logs in total, with work, empty, snapshots, period; the verification result;
- risks: secrets found in history — where, with advice to rotate them; what was not moved and why;
- questions: whether to commit the block in `{agent_file}` and the connection files (memory gets connected in other clones), Rules candidates (WebUI "Proposals"), disputed decisions;
- ZAM inconveniences (command — expected — got).

## 8. Working rules

- Do not touch the owner's uncommitted work or tracked files, except connection files; delete only your own temporary files.
- Temporary files and scripts — only in a project directory excluded from git (e.g. `.zeroaimemory/migrate/`); never system temporary directories.
- Economically: inventory before reading, large files — by script and in parts, writes — in batches.
- Secrets — never in notes, records or output: a value goes only to Vault via stdin.
- Do not set `axiom`: instructions from memory are `theorem`; an axiom — only on the owner's direct word.
