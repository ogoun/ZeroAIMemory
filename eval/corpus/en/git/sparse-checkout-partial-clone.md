# Working in a huge monorepo: partial clone and sparse-checkout

Our monorepo is 14 GB of history and 280k files. A plain clone takes 40 minutes on the office
link and the working tree alone is 9 GB. Most people touch two or three services. These two
features together bring a fresh setup down to a couple of minutes.

They solve different problems:

- **Partial clone** — fewer *objects* downloaded. History is fetched, but file contents (blobs)
  are downloaded lazily when needed.
- **Sparse-checkout** — fewer *files* in the working directory. The repository still knows
  about everything, only a subset is written to disk.

## Partial clone

```bash
# no blobs up front; they arrive on demand at checkout, diff, blame...
git clone --filter=blob:none git@host:corp/mono.git

# no trees either — even smaller, but log -- <path> and many commands become slow
git clone --filter=tree:0 git@host:corp/mono.git

# only skip big blobs
git clone --filter=blob:limit=1m git@host:corp/mono.git
```

`blob:none` is the sensible default for developers. `tree:0` suits CI jobs that build one
commit and throw the clone away. The server must support it (`uploadpack.allowFilter`); GitHub,
GitLab and recent Gerrit do.

Partial clone is not shallow clone. `--depth 1` cuts history and makes `log`, `blame` and
merges with older bases painful; partial clone keeps the full commit graph.

Downside: commands that need old file content (`git blame` on an old file, `git log -p`) go to
the network and can be slow the first time. Offline they fail.

## Sparse-checkout

Use cone mode; it is fast and matches whole directories.

```bash
git clone --filter=blob:none --sparse git@host:corp/mono.git
cd mono
git sparse-checkout set services/billing services/auth libs/common
git sparse-checkout add tools/codegen
git sparse-checkout list
git sparse-checkout disable      # back to a full working tree
```

In cone mode the files in the repository root are always present, plus every listed directory
recursively, plus the files directly in their parent directories.

Combined with `--filter=blob:none`, only the blobs for the sparse set at the current commit are
downloaded.

### Things to know

- `git status` shows `You are in a sparse checkout with 3% of tracked files present.`
- Files outside the cone carry the skip-worktree bit. Searching with `git grep` covers only
  the present files.
- A merge or rebase that conflicts outside the cone temporarily materialises those files.
- Build tools that glob across the whole repository will miss directories; our Bazel setup
  needed a list of dependent directories per service, which we generate with a script.
- `git sparse-checkout init --cone` followed by `set` is the older two-step spelling; `set`
  alone is enough on current versions.

## Our setup script

```bash
git clone --filter=blob:none --sparse --branch main git@host:corp/mono.git mono
cd mono
git sparse-checkout set $(./tools/deps-of.sh services/billing)
git maintenance start      # background prefetch and commit-graph updates
```

`git maintenance start` registers the repository for scheduled background tasks (prefetch,
loose-objects, incremental-repack). With it, `git fetch` is near-instant in the morning.
