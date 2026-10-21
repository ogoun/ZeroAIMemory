# push --force-with-lease instead of --force

After a rebase or amend the remote branch is no longer an ancestor of the local one, so the push
is rejected as non-fast-forward. Bare `--force` overwrites whatever the server has, including a
commit a colleague pushed five minutes ago.

`git push --force-with-lease` only overwrites if the remote branch still points where your
remote-tracking ref (`origin/feature`) says it does. If someone pushed in between, the push
fails with `stale info` and you can fetch and look first.

Caveat: a background `git fetch` (IDEs do this) updates `origin/feature` without you seeing
the new commit, and the lease then passes. Two ways to close the hole:

```bash
# pin the expected value explicitly
git push --force-with-lease=feature:3f9c2ab origin feature

# also require that the remote tip was integrated locally (Git 2.30+)
git push --force-with-lease --force-if-includes
```

Alias we use: `git config --global alias.pushf "push --force-with-lease --force-if-includes"`.
Protected branches on the server reject any forced push regardless of the flag.
