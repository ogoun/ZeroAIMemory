# Pinning the Node.js version: engines, .nvmrc, nvm and fnm

Incident: a teammate on Node 16 ran the dev server, got `SyntaxError: Unexpected token '?'`
deep inside a dependency, and spent an hour debugging what was just an old runtime. Since
then the project pins Node in three places.

## 1. engines in package.json

```json
{
  "engines": { "node": ">=20.11 <23", "npm": ">=10" }
}
```

By default npm only **warns** (`EBADENGINE Unsupported engine`) and installs anyway. To make
it an error, add to the project `.npmrc`:

```
engine-strict=true
```

`engines` is also documentation for consumers of a library; package managers check it when
installing your package as a dependency.

## 2. .nvmrc for version managers

A one-line file at the repo root, e.g. `20.18.0` or `lts/iron`. Then:

```bash
nvm install      # reads .nvmrc
nvm use
```

nvm is a shell function for bash/zsh; on Windows there is a separate project, nvm-windows,
with different commands and no `.nvmrc` support in older versions. We moved the team to
**fnm** (Rust, single binary, works in PowerShell too):

```bash
fnm use --install-if-missing
# auto-switch on cd, in the shell profile:
eval "$(fnm env --use-on-cd)"
```

fnm reads both `.nvmrc` and `.node-version`.

## 3. CI uses the same file

```yaml
- uses: actions/setup-node@v4
  with:
    node-version-file: .nvmrc
    cache: npm
```

One source of truth; bumping Node is a one-line PR that CI validates.

## Corepack and the package manager version

For pnpm/yarn, `"packageManager": "pnpm@9.12.0"` in `package.json` plus `corepack enable`
gives everyone the same package manager build. Note that Corepack is no longer bundled with
Node starting from v25, so it may need a separate install.

## Checklist when someone reports a weird install

- `node -v` and `npm -v` against `.nvmrc` and `engines`;
- a global npm installed into an old Node prefix shadowing the managed one (`which -a node`);
- stale `node_modules` built for another Node ABI: native addons fail with
  `NODE_MODULE_VERSION` mismatch, fix with a clean `npm ci`.
