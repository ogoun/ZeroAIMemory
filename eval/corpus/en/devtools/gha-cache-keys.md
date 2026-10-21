# Caching dependencies in GitHub Actions: keys, restore-keys and scope

These are my working notes after spending a week making our CI caching predictable. The
short version: the cache is a key-value store of tarballs, the key is everything, and most
"the cache doesn't work" reports are really "the key never matches" or "the cache is
written from the wrong branch".

## How actions/cache works

`actions/cache` does two things in one step:

1. At the step itself it tries to **restore**: look up an entry whose key equals `key`; if
   none, try each prefix in `restore-keys` in order and take the most recent entry that
   starts with that prefix.
2. In a post-job hook it **saves** the `path` under `key`, but only if there was no exact hit
   and the job succeeded.

Entries are immutable. If `key` hit exactly, nothing is saved at the end, even if you added
new packages during the job. That is why the key must contain a hash of whatever determines
the content.

```yaml
- uses: actions/cache@v4
  id: deps
  with:
    path: |
      ~/.cache/pip
      ~/.npm
    key: deps-${{ runner.os }}-${{ hashFiles('**/requirements*.txt', '**/package-lock.json') }}
    restore-keys: |
      deps-${{ runner.os }}-
- if: steps.deps.outputs.cache-hit != 'true'
  run: echo "cold or partial cache"
```

`cache-hit` is `'true'` only for an exact key match. A restore via `restore-keys` reports
`false`, which is what you want for deciding whether to run an install.

## Designing the key

| Part | Why |
|------|-----|
| A constant prefix (`deps-`) | Lets you invalidate everything by bumping it to `deps-v2-` |
| `runner.os` (and arch) | Native wheels and binaries are not portable between OSes |
| Tool version | A cache built by Node 18 may contain artifacts Node 22 cannot use |
| `hashFiles(lockfile)` | Changes exactly when dependencies change |

Things that do **not** belong in the key: `github.sha` (every commit misses, the store fills
with near-identical entries), timestamps, the run number. If you want a rolling cache, use a
dated key (`deps-2024-W17`) plus a broad `restore-keys` prefix.

`hashFiles` returns an empty string when no file matches the pattern. A typo in the glob
gives a key like `deps-Linux-` that matches forever and never refreshes. I now print the key
in a debug step when changing patterns.

## What to cache, and what not

Cache the **download cache** of the package manager, not the installed tree:

- pip: `~/.cache/pip` (or let `actions/setup-python` with `cache: pip` do it);
- npm: `~/.npm`, not `node_modules`. `npm ci` deletes `node_modules` anyway, and caching
  `node_modules` across Node versions gives broken native addons;
- Gradle and Maven: their own caches under the home directory; the setup actions have a
  `cache:` input that builds a sane key for you.

Caching the installed tree is faster to restore but couples the entry to the exact runner
image. When it goes wrong, failures look like random import errors, which is the worst kind
of failure to debug.

## Scope: why my PR does not see the cache

Cache entries are scoped by branch. A run can restore entries created:

- on its own branch (for PRs: the `refs/pull/N/merge` ref);
- on the base branch of the PR;
- on the default branch.

It can **not** restore entries from sibling feature branches. Consequences:

- The first run of every new PR is cold unless `main` has a warm entry. We added a scheduled
  workflow on `main` that only installs dependencies and saves the cache, so PRs always find
  a base entry.
- Entries created in a PR are useless for everyone else and just consume quota.

## Limits and eviction

- Total size per repository: 10 GB by default. When exceeded, the oldest entries are evicted.
- Entries not accessed for 7 days are removed.
- With many matrix cells each saving its own entry, the quota fills quickly. Share one key
  across cells where content is identical (e.g. the pip download cache does not depend on
  the test shard).

List and delete entries with the CLI:

```bash
gh cache list --limit 50 --sort size_in_bytes
gh cache delete deps-Linux-abc123
gh cache delete --all   # nuclear option after a poisoned entry
```

## Restore and save separately

`actions/cache/restore` and `actions/cache/save` split the two halves. Useful when:

- you want to save even if tests fail (dependencies were installed fine);
- you want only `main` to write entries and PRs to be read-only:

```yaml
- uses: actions/cache/restore@v4
  with: { path: ~/.npm, key: npm-${{ hashFiles('package-lock.json') }} }
- run: npm ci
- if: github.ref == 'refs/heads/main'
  uses: actions/cache/save@v4
  with: { path: ~/.npm, key: npm-${{ hashFiles('package-lock.json') }} }
```

## Security note

Caches are writable by any workflow that runs on a branch, including `pull_request` from
forks within their PR scope. Never restore a cache in a privileged workflow and then execute
something from it without verification: cache poisoning is a known escalation path.

## Checklist when the cache "does not work"

1. Print the computed key in both runs and compare.
2. Check that `hashFiles` matched at least one file.
3. Check the branch: is there an entry on `main` or on the base branch?
4. Look at `gh cache list`: maybe the entry was evicted by size.
5. Remember an exact hit never re-saves; bump the prefix if the content went stale.
6. Make sure `path` is identical between save and restore; the path list is part of the
   cache version, so a different list never matches.
