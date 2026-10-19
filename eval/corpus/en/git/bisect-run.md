# Finding the commit that broke things: git bisect and bisect run

Post-incident notes. Release 4.12 started returning HTTP 500 on export of large reports; 4.11
was fine. Between the two tags there were 412 commits from nine people. Bisect found the
culprit in nine steps, about twenty minutes including builds. This note describes the method
and the automation we now use.

## The idea

Bisect is a binary search over history. You give it one commit where the behaviour is bad and
one where it is good. It checks out the commit in the middle, you test and answer, and it
halves the range each time. 412 commits need about log2(412) ≈ 9 answers.

## Manual session

```bash
git bisect start
git bisect bad                 # current checkout (v4.12) is broken
git bisect good v4.11          # this tag worked
# git checks out a commit in the middle:
# "Bisecting: 205 revisions left to test after this (roughly 8 steps)"
./build.sh && ./repro.sh       # test by hand
git bisect good                # or: git bisect bad
# ... repeat ...
# "a3f9e01 is the first bad commit"
git bisect reset               # return to where you started
```

Shortcut: `git bisect start v4.12 v4.11` sets bad and good in one command.

Useful commands during a session:

| Command                         | Purpose                                                    |
|---------------------------------|------------------------------------------------------------|
| `git bisect skip`               | this commit cannot be tested (does not build); pick another |
| `git bisect skip v1..v2`        | skip a whole range known to be broken for other reasons    |
| `git bisect log`                | print answers so far                                       |
| `git bisect replay file`        | redo a session from a saved log after a wrong answer       |
| `git bisect visualize`          | show the remaining candidates in gitk or `git log`         |
| `git bisect reset HEAD`         | finish but stay on the current commit                      |

If you answer wrong once, the result is garbage. Save the log (`git bisect log > bisect.log`),
edit out the wrong line and `git bisect replay bisect.log`.

### Custom terms

When you are looking for the commit that *fixed* something (or made it faster), "good" and
"bad" read backwards. Use your own words:

```bash
git bisect start --term-old=slow --term-new=fast
git bisect slow v4.0
git bisect fast HEAD
```

`--term-old` marks the older state, `--term-new` the newer one. The built-in synonyms
`old`/`new` also work.

## Automating with bisect run

When the check can be scripted, let git drive:

```bash
git bisect start v4.12 v4.11
git bisect run ./ci/bisect-check.sh
```

The exit code of the script is the answer:

| Exit code     | Meaning                                         |
|---------------|-------------------------------------------------|
| 0             | good                                            |
| 1–124, 126, 127 | bad                                           |
| 125           | skip: cannot test this commit                   |
| 128 and above | abort the whole bisect (something is very wrong) |

Our script for the export bug:

```bash
#!/usr/bin/env bash
# ci/bisect-check.sh — exit 0 = good, 1 = bad, 125 = untestable
set -u
cargo build --release --quiet 2>/dev/null || exit 125   # does not compile: skip
./target/release/server --port 18080 &
pid=$!
trap 'kill $pid 2>/dev/null' EXIT
sleep 2
code=$(curl -s -o /dev/null -w '%{http_code}' \
  "http://127.0.0.1:18080/export?report=big-fixture")
[ "$code" = "200" ]
```

Notes on the script:

- **Build failures must map to 125**, not 1. Otherwise bisect treats "does not compile" as
  "bad" and lands on an unrelated broken commit.
- The script must live **outside** the tracked tree or be untracked, because bisect checks out
  old commits where the script may not exist or may differ. Copy it to the repo's `.git`
  folder or to a scratch directory if in doubt.
- Use a dedicated fixture and port so that a test from a previous step cannot pass by accident.
- Keep it fast. Nine steps times a five-minute build is already 45 minutes; incremental builds
  help because consecutive checkouts are close in history.

A failing test can serve directly as the check, e.g. `git bisect run cargo test export_large`.
Be careful: if the test was added after the bug, older commits will not have it and the command
exits non-zero for "test not found", which looks like bad.

## Merges and first-parent

On a history with many merged feature branches, bisect may step into the middle of a branch
that was never in a release, which can be confusing and is often unbuildable. Since Git 2.29:

```bash
git bisect start --first-parent v4.12 v4.11
```

follows only the first parent of merges, i.e. the mainline. The result is the merge commit that
brought the problem in; then you can bisect inside that branch if needed.

## Limiting by path

If you know the bug is in the exporter, restrict candidates to commits touching it:

```bash
git bisect start v4.12 v4.11 -- src/export/ templates/report/
```

This cuts the number of steps but trusts your guess; if the cause is elsewhere (a dependency
bump in `Cargo.lock`, for example) you will get a wrong answer.

## What it found

The first bad commit replaced a streaming writer with an in-memory buffer "for simplicity". On
big reports the buffer hit the request body limit of the proxy and the handler returned 500.
The commit message mentioned only refactoring; review missed it because the diff was large.

## Lessons

1. Keep every commit on main buildable. Squash work-in-progress commits before merging,
   otherwise bisect spends steps on skips.
2. Write the reproduction as a script first. It doubles as a regression test afterwards.
3. Tag releases. "good v4.11" is much easier than digging up hashes.
4. Always finish with `git bisect reset`; leaving the repository in bisect state confuses the
   next person (the prompt shows `BISECTING`).
