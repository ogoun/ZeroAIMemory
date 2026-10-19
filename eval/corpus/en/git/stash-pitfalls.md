# git stash: the traps I keep stepping into

Stash is a quick way to park uncommitted changes. It is also the place where work quietly
disappears. Collected pitfalls and the habits that avoid them.

## Untracked files are not stashed by default

`git stash` saves tracked modifications and the index only. New files stay in the working tree,
and if you then switch branches they come along, which looks like "stash did not work".

```bash
git stash push -u -m "wip: auth refactor"   # -u / --include-untracked
git stash push -a                           # --all: also ignored files (build dirs!)
```

Avoid `-a` in projects with big `target/` or `node_modules/` directories — it stashes all of
it and is very slow.

## Anonymous stashes pile up

After a week there are twelve entries named `WIP on main: 3f2a1b0 ...` and nobody knows which
is which. Always give a message with `-m`. `git stash list` then becomes readable.

## pop vs apply

`git stash pop` applies and drops the entry. If applying causes a conflict, the entry is kept,
but people often think it was dropped and run `git stash drop` "to clean up". Prefer
`git stash apply`, check the result, then `git stash drop stash@{0}` explicitly.

## The index is not restored

By default `apply` does not restore what was staged vs unstaged; everything comes back as
unstaged modifications. Use `git stash apply --index` to get the staging split back.

## Partial stash

```bash
git stash push -p                    # choose hunks interactively
git stash push -- src/config.rs      # only these paths
git stash push --staged              # only what is in the index (Git 2.35+)
git stash push --keep-index          # stash everything, but leave the index in place
```

`--keep-index` is useful to test exactly what you are about to commit.

## Conflicts on apply

A stash records the commit it was created on. Applying it to a branch that has moved a lot
produces conflicts. A cleaner option is to turn the stash into its own branch, based on the
original commit:

```bash
git stash branch rescue-auth stash@{2}
```

## Recovering a dropped stash

A dropped or cleared stash is not in the reflog of any branch, but the commit object lives
until gc:

```bash
git fsck --unreachable | grep commit | cut -d' ' -f3 \
  | xargs git log --merges --no-walk --oneline | grep "WIP on"
git stash apply <hash>
```

## When not to stash

If the interruption is longer than a few minutes — a hotfix, a review of a colleague's branch —
stash is the wrong tool. Make a WIP commit on the branch, or open a second working directory
with `git worktree add` and leave the current one untouched.
