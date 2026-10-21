# git worktree: several branches checked out at once

I used to keep two full clones of the monorepo to review PRs while a long build ran. Worktrees
replace that: one repository, several working directories, each on its own branch, sharing a
single object database.

## Basics

```bash
# new directory next to the main one, on an existing branch
git worktree add ../app-review origin/feature/payments

# create a new branch at the same time
git worktree add -b hotfix/1.4.2 ../app-hotfix v1.4.1

# a throwaway detached checkout, e.g. to run an old version
git worktree add --detach ../app-old v1.2.0

git worktree list
git worktree remove ../app-review
git worktree prune          # forget worktrees whose directories were deleted by hand
```

## Rules that bite

- **One branch, one worktree.** Git refuses to check out a branch that is already checked out
  in another worktree (`fatal: 'main' is already used by worktree at ...`). Use `--detach` or a
  new branch name. `--force` overrides it, but then two directories move the same branch.
- **Shared refs and config.** Branches, tags, stash, and `.git/config` are shared. A stash made
  in one worktree is visible in all of them. Per-worktree config needs
  `extensions.worktreeConfig` and `git config --worktree`.
- **Hooks are shared** (they live in the common `.git/hooks`).
- **Not shared:** HEAD, the index, untracked files, build outputs. Each worktree needs its own
  `node_modules` or `target`, so the first build is a full one. For Rust I point
  `CARGO_TARGET_DIR` to a common directory when the branches are close.
- **Moving a worktree:** use `git worktree move`, not `mv`; otherwise run `git worktree repair`.
- **Submodules** in worktrees are still rough: each worktree needs its own
  `git submodule update --init`.
- **Removable drives / network shares:** if a worktree lives on a disk that is sometimes
  unmounted, `prune` may delete its metadata. `git worktree lock` prevents that.

The secondary worktree has a `.git` *file* (not a directory) containing `gitdir: ...` pointing
back into the main repository's `.git/worktrees/<name>`.

## Typical uses

1. Hotfix on a release branch while the feature work stays half-done and uncommitted.
2. Reviewing a pull request with a real build and tests, without disturbing the current tree.
3. Running a long benchmark on one commit while continuing to edit on another.
4. Comparing behaviour of two versions side by side.

## Worktree vs stash vs second clone

| Need                                    | Tool                     |
|-----------------------------------------|--------------------------|
| Interruption of a few minutes           | stash                    |
| Parallel work for hours or days         | worktree                 |
| Fully isolated config, hooks, remotes   | separate clone           |
| Clean environment to reproduce a CI bug | separate clone, fresh    |
