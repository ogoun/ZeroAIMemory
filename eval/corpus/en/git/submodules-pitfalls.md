---
tags: [git, submodules, dependencies]
---
# Git submodules: how they work and where they hurt

We vendor three internal libraries and one third-party C library as submodules. Every new
team member trips over them in the first week. This is the long version of "how submodules
work" plus the incidents we had.

## Mental model

A submodule is **a pointer to one exact commit** of another repository, stored in the parent
repository's tree as a special entry (mode `160000`, a "gitlink"). Plus a `.gitmodules` file
that maps the path to a URL. That is all.

Consequences:

- The parent does not track a branch of the submodule; it tracks a commit. "Update the
  submodule" means "commit a new pointer in the parent".
- Cloning the parent does not fetch the submodule content unless asked.
- Inside the submodule directory you are in a separate repository, by default on a
  **detached HEAD**.

## Daily commands

```bash
# clone with everything
git clone --recurse-submodules git@host:team/app.git

# already cloned without it
git submodule update --init --recursive

# after a pull that moved the pointers
git submodule update --recursive

# make pull/checkout/switch do that automatically
git config --global submodule.recurse true
```

`submodule.recurse` is the single most useful setting. Without it, a `git pull` in the parent
updates the pointer but leaves the old checkout in the submodule directory, and `git status`
shows `modified: libs/crypto (new commits)` although you changed nothing.

### Bumping a submodule to a newer version

```bash
cd libs/crypto
git fetch
git switch --detach origin/main    # or a tag: git checkout v2.3.0
cd ../..
git add libs/crypto
git commit -m "crypto: bump to v2.3.0"
```

Or from the parent: `git submodule update --remote libs/crypto`, which uses the branch named
in `.gitmodules` (`branch = main`) or the remote HEAD.

## Pitfalls and incidents

### 1. Pushed a parent commit pointing to an unpushed submodule commit

Someone committed inside `libs/parser`, bumped the pointer in the parent, pushed the parent
but not the submodule. Every other clone failed with
`fatal: remote error: upload-pack: not our ref` / `Fetched in submodule path ..., but it did not
contain <hash>`. Prevention:

```bash
git config --global push.recurseSubmodules check     # refuse the push
# or: on-demand — push the submodules first automatically
```

### 2. Work lost on a detached HEAD

A developer edited code inside the submodule, committed on the detached HEAD, then ran
`git submodule update`, which checked out the recorded commit and left his commits dangling.
Recovered via the submodule's own reflog (`cd libs/x && git reflog`). Rule: before editing in a
submodule, `git switch -c my-change` or `git switch main`.

### 3. Changing a submodule URL

Editing `.gitmodules` is not enough; the URL is copied into `.git/config` at init time.

```bash
git config -f .gitmodules submodule.libs/crypto.url https://new.host/crypto.git
git submodule sync --recursive
git submodule update --init --recursive
```

### 4. Removing a submodule

The old dance with editing three places is no longer needed:

```bash
git rm libs/legacy
rm -rf .git/modules/libs/legacy      # optional: drop the stored repository
git commit -m "remove legacy submodule"
```

`git rm` removes the gitlink and the `.gitmodules` section. The cached repository in
`.git/modules` stays unless deleted, which matters if you later add a *different* repository at
the same path (git will complain or reuse the old one).

### 5. Merge conflicts on the pointer

When two branches bump the same submodule to different commits, the conflict shows up as
`CONFLICT (submodule)`. There is no text to edit. Decide which commit is right (often the newer
one that contains the other), check it out inside the submodule and `git add` the path in
the parent.

### 6. Relative URLs and CI

`.gitmodules` with `url = ../crypto.git` resolves against the parent's remote URL. Great when
the team uses both SSH and HTTPS, but CI systems that clone with a token in the URL need the
same host for the submodules, otherwise they fail with authentication errors. Our CI
rewrites SSH URLs to HTTPS with a token:

```bash
git config --global url."https://ci:${TOKEN}@git.example.com/".insteadOf "git@git.example.com:"
```

### 7. Shallow clones

`git clone --depth 1 --recurse-submodules --shallow-submodules` saves time in CI, but if the
pinned commit is not a branch tip on the remote, the shallow fetch can fail. Most servers allow
fetching any reachable commit by hash (`uploadpack.allowReachableSHA1InWant`), some do not.

## Status cheat sheet

`git submodule status` prefixes:

| Prefix | Meaning                                                |
|--------|--------------------------------------------------------|
| ` `    | checked out at the recorded commit                     |
| `-`    | not initialized                                        |
| `+`    | checked out commit differs from the recorded one       |
| `U`    | merge conflict                                         |

`git diff --submodule=log` (or `diff.submodule = log`) shows the list of commits between old and
new pointer instead of two opaque hashes. `status.submoduleSummary = true` adds a summary to
`git status`.

## Alternatives we considered

| Option                       | Pros                                   | Cons                                        |
|------------------------------|----------------------------------------|---------------------------------------------|
| submodules                   | exact pinning, separate history        | the pitfalls above                          |
| subtree (`git subtree`)      | plain files, clones just work          | messy history, upstreaming changes is manual |
| package manager (cargo, npm) | versioning, caching, tooling           | requires publishing the library             |
| monorepo                     | atomic changes across components       | bigger repo, needs build tooling            |

We moved two internal libraries to our private registry and kept submodules only for the C
library, which has no package.

## Checklist for newcomers

1. `git config --global submodule.recurse true`
2. `git config --global push.recurseSubmodules check`
3. `git config --global diff.submodule log`
4. Never commit inside a submodule on a detached HEAD.
5. A parent PR that bumps a submodule must link the submodule commit, and that commit must be on
   a branch of the submodule's remote.
