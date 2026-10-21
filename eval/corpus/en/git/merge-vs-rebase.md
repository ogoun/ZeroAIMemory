# Merge vs rebase: what we use where

This comes up in every new team, so here is the reasoning written down once.

## What each one does

**Merge** joins two lines of history with a new commit that has two parents. Existing commits
are untouched, their hashes stay the same. The history shows exactly what happened, including
when a branch was started and when it was brought in.

**Rebase** takes the commits of your branch, and replays them one by one on top of another base.
Each replayed commit is a *new* commit with a new hash, even if the diff is identical. The
result is a straight line, as if you had started your work from the latest upstream.

```
merge:                       rebase:
A---B---C---M  main          A---B---C  main
     \     /                           \
      D---E  feature                    D'---E'  feature
```

## Trade-offs

| Aspect                    | merge                         | rebase                          |
|---------------------------|-------------------------------|---------------------------------|
| Rewrites commits          | no                            | yes, new hashes                 |
| Safe on shared branches   | yes                           | no, others must recover         |
| History shape             | graph with merge commits      | linear                          |
| Conflict resolution       | once, in the merge commit     | per replayed commit, maybe many |
| `git bisect` friendliness | fine, but merges can confuse  | very good if each commit builds |
| Context "when was this integrated" | preserved            | lost                            |

The "golden rule": do not rebase commits that exist outside your repository and that others
may have built on. Rebasing your *local, unpublished* work is always fine.

## Our policy

1. **Updating a feature branch with main** — rebase, while the branch is yours alone:
   `git pull --rebase` or `git fetch && git rebase origin/main`. Set
   `git config --global pull.rebase true` so a pull never creates the useless
   "Merge branch 'main' of ..." commits.
2. **Shared feature branch** (two or more people push to it) — merge main into it, do not
   rebase.
3. **Integrating into main** — merge commit via the PR button (`--no-ff`), after the author has
   cleaned up the branch with interactive rebase. Squash-merge only for single-purpose small
   PRs where the intermediate commits carry no value.
4. **Release branches** — fixes go to main first and are cherry-picked to the release.

## Fast-forward

If main has not moved since you branched, `git merge feature` just moves the pointer: no merge
commit. `--ff-only` refuses anything else, which is a good setting for pulls on machines where
you never commit (`git config pull.ff only`). `--no-ff` forces a merge commit even when a
fast-forward is possible, which keeps the feature visible as a unit.

## Conflict experience

With a long branch of 30 commits a rebase may stop several times for the same file; merge asks
once. If you hit the same conflict again and again, either squash first or enable `rerere`.
Another option is `git rebase --update-refs` for stacked branches: it moves the dependent
branch pointers along so you do not have to rebase each one separately.

## Undo

A merge that has not been pushed: `git reset --hard ORIG_HEAD`. A rebase gone wrong: find the
old tip in `git reflog` and reset to it. A pushed merge on main: `git revert -m 1 <merge>`,
keeping in mind that re-merging the same branch later requires reverting the revert.
