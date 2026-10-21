---
tags: [git, recovery]
---
# Recovering lost commits and deleted branches with reflog

Incident from last sprint: a developer ran `git reset --hard` to "clean up" and lost two days
of commits that were never pushed. They were recovered in five minutes. Writing down how.

## What the reflog is

Every time a ref moves (commit, checkout, reset, rebase, merge, pull, amend) git appends a line
to a local log: `.git/logs/HEAD` for HEAD and `.git/logs/refs/heads/<name>` per branch. The
commits themselves stay in the object database until garbage collection removes them, and
gc does not touch anything still referenced from the reflog. Defaults: 90 days for entries
reachable from the current tip, 30 days for unreachable ones (`gc.reflogExpire`,
`gc.reflogExpireUnreachable`).

The reflog is **local only**. It is not pushed, not cloned; a fresh clone has an empty one.

## Reading it

```bash
git reflog                       # same as git reflog show HEAD
git reflog show feature/login    # moves of one branch
git log -g --date=iso            # reflog with full log format and timestamps
git reflog --date=relative
```

Typical output:

```
4e1d2c7 HEAD@{0}: reset: moving to HEAD~3
9ab03f1 HEAD@{1}: commit: add token refresh
...
```

`HEAD@{1}` is "where HEAD was one move ago". You can also use time: `main@{yesterday}`,
`HEAD@{2.hours.ago}`.

## Recipes

**Undo a hard reset**

```bash
git reflog                  # find the line before "reset: moving to"
git reset --hard HEAD@{1}
```

**Restore a deleted branch.** `git branch -D` prints the hash it deleted
(`Deleted branch x (was 9ab03f1)`). If that scrolled away, the branch's own reflog is gone with
it, but HEAD's reflog still has the checkouts:

```bash
git reflog | grep "checkout: moving from feature/login"
git branch feature/login 9ab03f1
```

**Undo a rebase.** Right after it, `ORIG_HEAD` points to the old tip:
`git reset --hard ORIG_HEAD`. Later, look for the `rebase (start)` line and take the entry
just before it.

**Commits made on a detached HEAD** and then abandoned by switching away: git warns with the
hash at that moment; otherwise they are in the HEAD reflog as `commit:` lines.

## When the reflog does not help

Stashes that were dropped, or objects created by `git add` but never committed, are not in any
reflog. They can still be found as dangling objects:

```bash
git fsck --lost-found --no-reflogs
git fsck --unreachable | grep commit
git show <hash>
```

A dropped stash is a dangling commit with a message like `WIP on main: ...`. Staged-but-never-
committed content appears as dangling blobs; you get file contents back but not file names.

## Prevention

- Use `git reset --keep` or `git switch` instead of `--hard` when you only want to move.
- `git stash` before experiments.
- Push work in progress to a personal remote branch daily.
- Never run `git gc --prune=now` or `git reflog expire --expire=now --all` while hunting for
  lost work; that is the one thing that makes recovery impossible.
