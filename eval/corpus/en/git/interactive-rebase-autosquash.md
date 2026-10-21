---
tags: [git, history, rebase]
---
# Interactive rebase and autosquash: cleaning up a branch before review

Notes I keep for myself and the team on how we tidy feature branches before opening a pull
request. The goal is a history where every commit builds, has a meaningful message and does
one thing. Interactive rebase is the main tool; autosquash makes it almost mechanical.

## When to do it (and when not)

- Only on commits that nobody else has based work on. Rewriting a branch that a colleague
  already pulled forces them to recover, and they usually do it badly.
- Before review, not after approval. Reviewers lose their place if the commits they commented
  on disappear. After the first review round we push fixups as separate commits and squash
  them at the very end, right before merge.
- Never on `main` or release branches. Our server rejects non-fast-forward pushes there anyway.

## The todo list

`git rebase -i <base>` opens an editor with one line per commit, oldest at the top. The base is
the commit *before* the first one you want to touch, so `git rebase -i HEAD~4` lets you edit the
last four. For a feature branch it is usually simpler to say `git rebase -i origin/main` —
git takes every commit that is on the branch but not on main.

| Command   | Short | What it does                                                    |
|-----------|-------|-----------------------------------------------------------------|
| `pick`    | `p`   | keep the commit as is                                           |
| `reword`  | `r`   | keep the changes, stop to edit the message                      |
| `edit`    | `e`   | stop after applying, so you can amend or split it               |
| `squash`  | `s`   | meld into the previous commit, concatenate both messages        |
| `fixup`   | `f`   | meld into the previous commit, drop this message                |
| `fixup -C`|       | meld, but keep *this* commit's message instead                  |
| `drop`    | `d`   | remove the commit (deleting the line does the same)             |
| `exec`    | `x`   | run a shell command; a non-zero exit stops the rebase           |
| `break`   | `b`   | pause here, continue with `git rebase --continue`               |

Reordering is just moving lines. If two commits touch the same lines, moving them past each
other will produce a conflict, so reorder carefully.

## Autosquash workflow

Instead of remembering which commit a small fix belongs to, record it at commit time:

```bash
# the fix belongs to commit a1b2c3d ("parser: handle empty input")
git add -p
git commit --fixup=a1b2c3d

# a fix that also needs a new message
git commit --fixup=amend:a1b2c3d

# only change the message of an older commit, no content
git commit --fixup=reword:a1b2c3d

# keep the message for later editing, like the squash command
git commit --squash=a1b2c3d
```

These create commits titled `fixup! parser: handle empty input` and so on. Later:

```bash
git rebase -i --autosquash origin/main
```

The todo list arrives already sorted: every `fixup!` line sits right under its target and is
marked `fixup`. Usually I just save and close. To make it the default:

```bash
git config --global rebase.autoSquash true
```

With that setting even a plain `git rebase -i` rearranges the lines. Since Git 2.44 the option
only affects interactive rebases, which is what you want.

A handy alias to find the target without copying hashes:

```bash
# fixup into the last commit that touched this file
git commit --fixup=$(git log -n1 --format=%h -- src/parser.rs)
```

There is also the third-party `git absorb` tool which guesses targets automatically from
the blame of the staged hunks. It works well on clean branches; I check its result with
`git log --oneline` before rebasing.

## Splitting a commit

Mark it `edit`. When the rebase stops:

```bash
git reset HEAD~          # undo the commit, keep changes in the working tree
git add -p               # stage the first logical part
git commit -m "first part"
git add -p
git commit -m "second part"
git rebase --continue
```

## Checking that every commit builds

`exec` lines make the rebase run a command after each pick. The shortcut flag adds one after
every commit:

```bash
git rebase -i --exec "cargo test --quiet" origin/main
```

If a test fails, the rebase stops on that commit with the working tree in place; fix it, `git
commit --amend`, then `git rebase --continue`. This catches the classic problem where the
squashed history is fine at the tip but commit 3 of 7 does not compile, which later breaks
bisecting.

## Conflicts during rebase

- Resolve, `git add` the files, then `git rebase --continue`. Do not run `git commit` yourself
  unless the rebase stopped on an `edit` line.
- `git rebase --skip` drops the current commit entirely — useful when its changes are already
  upstream, dangerous otherwise.
- `git rebase --abort` returns everything to the state before you started.
- Enable `rerere` if you rebase the same long branch repeatedly; it remembers resolutions.
- `git config --global merge.conflictStyle zdiff3` shows the base version in conflict markers,
  which makes the "theirs/ours" confusion during rebase much easier to untangle. Remember that
  during rebase "ours" is the branch you are rebasing onto and "theirs" is your own commit.

## Amend vs fixup

`git commit --amend` only works on the tip. It replaces the last commit with a new one
(new hash), adding staged changes and optionally a new message (`--no-edit` keeps it).
For anything older than the tip, use fixup plus autosquash. Both rewrite history, so the same
rule applies: only before others have the commits.

## Pushing the result

After rewriting, a plain push is rejected as non-fast-forward. Use
`git push --force-with-lease`, never bare `--force`; see the separate note on that flag.

## If it went wrong

The old branch tip is still in the reflog. `git reflog` shows something like
`HEAD@{5}: rebase (start): checkout origin/main`; the line just before it is your previous
state. `git reset --hard HEAD@{6}` (or `ORIG_HEAD` right after the rebase finished) puts the
branch back. Nothing is lost until garbage collection, which by default keeps reachable reflog
entries for 90 days and unreachable ones for 30.

## Team conventions

1. One logical change per commit; refactors separate from behaviour changes.
2. Messages in imperative mood, subject up to ~72 characters.
3. Fixup commits are fine during review, but none may reach main.
4. CI runs a check that fails if any commit title starts with `fixup!` or `squash!`.
5. We merge with a merge commit, so the cleaned commits stay visible as they were reviewed.
