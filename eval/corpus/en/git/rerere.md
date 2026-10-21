# rerere: let git remember conflict resolutions

Problem: a long-lived branch that I rebase onto main every morning hits the same conflict in
`routes.rs` every time, and I resolve it the same way every time.

`rerere` ("reuse recorded resolution") records the conflicted hunk and how you resolved it.
Next time the identical conflict appears, git applies the stored resolution automatically.

```bash
git config --global rerere.enabled true
git config --global rerere.autoUpdate true   # also stage the auto-resolved files
```

What happens:

- On conflict git prints `Recorded preimage for 'src/routes.rs'`.
- After you resolve and commit: `Recorded resolution for 'src/routes.rs'`.
- Next time: `Resolved 'src/routes.rs' using previous resolution.`

Useful commands: `git rerere status`, `git rerere diff`, and `git rerere forget src/routes.rs`
when a recorded resolution was wrong. Data lives in `.git/rr-cache`, local only; entries expire
after 60 days (resolved) or 15 days (unresolved).

Always review the result with tests: rerere matches text, not intent, and a resolution that was
right last week may be wrong after main changed nearby.
