---
tags: [node, npm, dependencies]
---
# npm install, semver ranges and the lockfile

Local notes on what `npm install` actually does to the lockfile, because reviewers keep
asking why a PR touching one line of code has a 3000-line diff in `package-lock.json`.

## Ranges in package.json

`npm install express` writes `"express": "^4.19.2"`:

- `^4.19.2` accepts `>=4.19.2 <5.0.0` (for `0.x` the caret is stricter: `^0.3.1` means
  `<0.4.0`);
- `~4.19.2` accepts `>=4.19.2 <4.20.0`;
- `4.19.2` pins exactly. Set `save-exact=true` in `.npmrc` to make that the default.

The range expresses what is allowed, the lockfile records what was chosen.

## What npm install does

- With no arguments: reads `package.json` and the lockfile. If a locked version still
  satisfies the range, it is kept. If the range was changed so the locked version no longer
  fits, npm resolves a new one and **rewrites the lockfile**.
- With a package name: adds or upgrades that package and its subtree, updates both files.
- Can also reshuffle the tree (deduplication, hoisting), which produces big lockfile diffs
  even when nothing meaningful changed, e.g. after switching npm major versions.

`npm update` deliberately moves locked versions to the newest within the ranges.
`npm outdated` shows what is behind.

## Team rules

1. The lockfile is always committed. Libraries too: it keeps contributors' installs
   reproducible, even though consumers of the library never see it.
2. Lockfile changes go in the same commit as the `package.json` change that caused them.
3. Merge conflicts in `package-lock.json`: do not hand-edit. Resolve `package.json`, then
   take either side of the lockfile and run `npm install` to regenerate the affected part.
4. Dependency bumps come from Renovate in separate PRs, so feature PRs have small lockfile
   diffs.
5. Everyone uses the same npm major version (see the engines notes), otherwise
   `lockfileVersion` flips back and forth.

## Checking for drift

`npm install --package-lock-only` updates the lockfile without touching `node_modules`. A CI
job runs it and fails if `git diff --exit-code package-lock.json` is non-empty, catching
`package.json` edits that were never installed.
