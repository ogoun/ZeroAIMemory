# Git LFS for large binaries

Designers committed PSD files and test videos directly; the repository grew to 6 GB. We moved
binaries to Git LFS: git stores a small text pointer (`version`, `oid sha256:...`, `size`), and
the real content lives on the LFS server, downloaded only for the checked-out commit.

```bash
git lfs install                       # once per machine, sets up smudge/clean filters
git lfs track "*.psd" "*.mp4"         # writes patterns to .gitattributes
git add .gitattributes
git lfs ls-files                      # what is actually stored in LFS
```

Pitfalls:

- `track` only affects new commits. Old blobs stay in history; to move them, rewrite with
  `git lfs migrate import --include="*.psd" --everything` and force-push — coordinate with the
  team, everyone must re-clone.
- Forgetting `git lfs install` on a CI runner yields pointer files instead of images; tests then
  fail with odd "invalid PNG" errors. Check with `git lfs env`.
- Hosting quotas count bandwidth; cache LFS objects in CI (`.git/lfs`).
- `GIT_LFS_SKIP_SMUDGE=1 git clone ...` clones without downloading content; fetch selectively
  with `git lfs pull --include="assets/ui/**"`.
- File locking for unmergeable files: `git lfs lock design/home.psd`.
