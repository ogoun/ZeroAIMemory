---
index: false
---
# Conventional Commits in practice

We adopted the Conventional Commits format so that release tooling can derive the next
version and the changelog from history. These are the rules as we apply them.

## Format

```
<type>(<optional scope>)!: <description>

<optional body>

<optional footer(s)>
```

Examples:

```
feat(api): add pagination to /orders
fix(auth): refresh token before expiry instead of after
docs: explain local TLS setup
refactor(db)!: rename customer_id to account_id

BREAKING CHANGE: the column and the JSON field are renamed; clients must update.
```

## Types we allow

| Type | Meaning | Version effect |
|------|---------|----------------|
| `feat` | new user-visible capability | minor |
| `fix` | bug fix | patch |
| `perf` | performance improvement | patch |
| `refactor`, `test`, `docs`, `style`, `chore`, `ci`, `build` | no user-visible effect | none |

A `!` after the type/scope or a `BREAKING CHANGE:` footer means a major bump, whatever the
type. Only `feat` and `fix` are defined by the spec itself; the rest is the widely used
Angular-derived convention.

## Writing good descriptions

- Imperative mood, lower case, no trailing period: "add", not "added" or "adds".
- Describe the effect, not the file: `fix(cart): keep coupon after login`, not
  `fix: update CartService.ts`.
- The scope is a module name from a fixed list in our contributing guide; free-form scopes
  made the changelog unreadable.
- One logical change per commit. If the subject needs "and", split it.

## Enforcement

- `commitlint` with `@commitlint/config-conventional` runs in a `commit-msg` hook and in CI
  over the PR range (`npx commitlint --from origin/main --to HEAD`).
- With squash merges, the **PR title** becomes the commit, so we lint the PR title with a
  small workflow instead of every intermediate commit.
- Release tooling (release-please in our case) reads `feat`/`fix`/breaking markers, opens a
  release PR with the bumped version and generated changelog section.

## Pitfalls

- `chore` turned into a dumping ground. Dependency bumps that fix a vulnerability for users
  should be `fix(deps)`, otherwise they never appear in release notes.
- Reverts: `revert: feat(api): add pagination` with `Refs: <sha>` in the footer.
- The format is about history, not ceremony; a perfect type on a commit that mixes three
  changes is still a bad commit.
