# GitHub Actions concurrency groups

Problem: every push to a PR branch started a full workflow run, and the previous run kept
going. Five quick pushes meant five parallel runs fighting for the same runners, and the
deploy job could even finish out of order.

Fix at the workflow level:

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.head_ref || github.ref }}
  cancel-in-progress: true
```

- `group` is any string; runs sharing it are serialized. At most one is running and one is
  pending; a third run replaces the pending one.
- `cancel-in-progress: true` cancels the running one as well, so only the latest push matters.
- `head_ref` is set only for pull_request events, hence the fallback to `ref` for pushes.

For deployments we do **not** cancel: interrupting a half-applied migration is worse than
waiting. There the key is per environment, e.g. `deploy-production`, with
`cancel-in-progress: false`. The same block also works per job.
