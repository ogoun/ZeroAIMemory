---
tags: [github-actions, ci, matrix]
---
# GitHub Actions: matrix strategy notes

We test the library on three operating systems and four interpreter versions. Writing twelve
jobs by hand is silly, so `strategy.matrix` expands one job definition into the cartesian
product of the axes.

```yaml
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      max-parallel: 6
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
        python: ["3.10", "3.11", "3.12", "3.13"]
        exclude:
          - os: macos-latest
            python: "3.10"
        include:
          - os: ubuntu-latest
            python: "3.13"
            coverage: true
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: ${{ matrix.python }}
```

## Things that bit us

- **Quote version numbers.** YAML reads `3.10` as the float `3.1`. Our first matrix silently
  tested 3.1, which setup-python rejected with a confusing "version not found" message.
- **`fail-fast` defaults to `true`.** One red cell cancels every other cell still running. For
  a compatibility matrix you want the full picture, so we set `fail-fast: false`. On release
  branches we keep the default to save runner minutes.
- **`include` semantics are subtle.** An `include` entry that matches an existing combination
  only adds keys to it (here `coverage: true` lands on ubuntu/3.13). An entry that matches
  nothing becomes a brand new cell. `exclude` is applied before `include`.
- **Limits.** A single matrix may produce at most 256 jobs per workflow run. `max-parallel`
  caps concurrency, useful when the tests hit a shared staging database.
- **Required status checks.** Branch protection refers to check names such as
  `test (ubuntu-latest, 3.12)`. Renaming an axis value breaks the required check and the PR
  waits forever on "Expected — Waiting for status to be reported". We added a final
  aggregator job with `needs: test` and made only that one required.

## Dynamic matrices

When the list of packages lives in the repo, one job can emit JSON and the next consumes it:

```yaml
  plan:
    runs-on: ubuntu-latest
    outputs:
      pkgs: ${{ steps.list.outputs.pkgs }}
    steps:
      - uses: actions/checkout@v4
      - id: list
        run: echo "pkgs=$(ls packages | jq -R -s -c 'split("\n")[:-1]')" >> "$GITHUB_OUTPUT"
  test:
    needs: plan
    strategy:
      matrix:
        pkg: ${{ fromJSON(needs.plan.outputs.pkgs) }}
```

An empty array makes the dependent job fail with "Matrix vector does not contain any values",
so guard it with an `if:` on the output.
