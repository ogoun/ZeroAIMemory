# npm ci in pipelines: what it does and why it fails

`npm ci` is the install command for automated environments. We switched every pipeline and
Dockerfile to it after a release shipped with a transitive dependency version nobody had
tested locally.

## Behaviour

- Requires an existing `package-lock.json` (or `npm-shrinkwrap.json`). No lockfile, no install.
- Installs **exactly** the versions in the lockfile. It never resolves ranges from
  `package.json` and never writes to the lockfile.
- If `package.json` and the lockfile disagree (a dependency added to `package.json` without
  re-running install), it aborts instead of fixing things up.
- Deletes `node_modules` first if it exists, then installs from scratch.
- Cannot add single packages: `npm ci lodash` is not a thing.

Typical failure after someone edited `package.json` by hand:

```
npm ERR! `npm ci` can only install packages when your package.json and
npm ERR! package-lock.json or npm-shrinkwrap.json are in sync.
npm ERR! Missing: left-pad@1.3.0 from lock file
```

The fix is on the developer side: run `npm install`, commit the updated lockfile. Never "fix"
CI by replacing `npm ci` with `npm install` there.

## Other errors we hit

- **Lockfile version mismatch.** A lockfile with `"lockfileVersion": 3` written by npm 9+ and
  a pipeline on an old npm 6 produced odd results. Pin the Node/npm version in CI to what the
  team uses.
- **Private registry auth.** `npm ci` respects `.npmrc`; in CI write the token into it from a
  secret via `NODE_AUTH_TOKEN` (actions/setup-node does that with `registry-url`).
- **Postinstall scripts.** Lifecycle scripts run as usual. For untrusted contexts use
  `npm ci --ignore-scripts`.

## Speed

Because `node_modules` is deleted every time, cache the npm download cache (`~/.npm`) keyed
on the lockfile hash, not `node_modules`. With a warm cache add `--prefer-offline` so npm
skips revalidation requests. Skip devDependencies in production images with
`npm ci --omit=dev`.
