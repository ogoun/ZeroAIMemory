# "No space left on device" while df says there is space

A checklist written after two separate incidents on build agents. Both times the
application reported `ENOSPC` / "No space left on device", and both times `df -h` showed
the filesystem far from full. There are a handful of reasons this happens; go through
them in order, cheapest first.

## 1. Out of inodes, not bytes

Every file, directory and symlink consumes an inode. Ext4 fixes the inode count when the
filesystem is created, so a directory with millions of tiny files can exhaust inodes
while gigabytes remain free.

```sh
df -i /var
```

If `IUse%` is at 100 %, find the directory with the most entries:

```sh
sudo du --inodes -x -d 3 /var | sort -n | tail -20
```

Our first incident: a PHP session directory with 4.1 million files, because the session
garbage collector was disabled in the container image. The second time it was a
`node_modules` cache per build that nobody cleaned.

Fixes:

- delete the files (with millions of entries `rm -rf dir` can be slow; `find dir -type f
  -delete` streams and does not build a huge argument list);
- fix whatever creates them;
- for the long term, recreate the filesystem with more inodes (`mkfs.ext4 -i 4096` or `-N`),
  or use XFS/btrfs, which allocate inodes dynamically.

## 2. Deleted files still held open

When a file is removed while a process keeps it open, the directory entry disappears but
the blocks are freed only when the last descriptor is closed. `du` no longer sees the file;
`df` still counts it. A classic case is a log file deleted by hand while the daemon keeps
writing to it.

```sh
sudo lsof +L1
```

`+L1` lists open files with a link count below one, i.e. unlinked. Output shows the
process, PID, descriptor number and size. Alternatively:

```sh
sudo find /proc/*/fd -ls 2>/dev/null | grep '(deleted)'
```

Options, from gentle to blunt:

1. Ask the process to reopen its logs (`systemctl reload`, `kill -HUP`, or whatever the
   daemon supports).
2. Truncate the file through the descriptor without restarting:
   `: > /proc/<pid>/fd/<fd>` — frees the space immediately.
3. Restart the process.

Prevention: rotate logs with `copytruncate` or a reopen signal instead of plain `rm`.

## 3. Reserved blocks

Ext4 reserves 5 % of blocks for root by default. A non-root process sees ENOSPC while
`df` shows a few percent free and root can still write. On a 2 TB data volume that is
100 GB doing nothing:

```sh
sudo tune2fs -l /dev/sdb1 | grep -i "reserved block count"
sudo tune2fs -m 1 /dev/sdb1
```

Keep the reserve on `/`; drop it on pure data volumes.

## 4. Something mounted over a full directory

If `/var/lib/app` was full before a new disk was mounted on top of it, the old files are
hidden under the mount point and still use space on the parent filesystem. `du` on the
parent cannot see them. Check with a bind mount of the root:

```sh
sudo mkdir /mnt/rootview
sudo mount --bind / /mnt/rootview
sudo du -sh /mnt/rootview/var/lib/app
```

## 5. Quotas and per-directory limits

User or project quotas (`quota -s`, `xfs_quota -x -c report`) produce the same error
string. Also overlay or tmpfs mounts with their own size: `/tmp` as tmpfs is limited to
half of RAM by default, and `/dev/shm` likewise.

## 6. Btrfs metadata

On btrfs, `df` can be misleading because data and metadata chunks are allocated
separately. Use `btrfs filesystem usage /` and, if metadata is full while data is not,
a filtered balance (`btrfs balance start -dusage=10 /`).

## Quick triage block

```sh
df -h /path; df -i /path
sudo lsof +L1 | sort -k7 -n | tail
sudo du -xh -d 2 /path | sort -h | tail -20
findmnt -T /path
```

| Symptom | Likely cause |
|---|---|
| `df -i` at 100 % | inode exhaustion |
| `df` full, `du` much smaller | deleted-but-open files or hidden mount |
| only non-root fails | reserved blocks |
| only one user fails | quota |
| `/tmp` or `/dev/shm` only | tmpfs size |

## Timeline of the second incident

For the record, because the order of steps mattered:

1. 10:40 — CI jobs on agent-7 fail at `npm ci` with ENOSPC. `df -h /` shows 38 % used.
2. 10:45 — `df -i /` shows 100 % inode use. The usual suspect, `/var/lib/php/sessions`,
   is small this time.
3. 10:55 — `du --inodes -x -d 3 /home` points at `/home/ci/.cache/builds`, 6.8 million
   entries: one `node_modules` copy per pipeline, never removed.
4. 11:05 — `find /home/ci/.cache/builds -mindepth 1 -maxdepth 1 -mtime +2 -exec rm -rf {} +`
   — takes 25 minutes on spinning disk; inode use drops to 31 %.
5. 11:40 — a builder still fails: one long-running process held a deleted 40 GB
   artifact tarball. `lsof +L1` found it; restarting the agent service released it.

Two separate causes in one incident is normal on shared build hosts, so do not stop at
the first finding — rerun the triage block after each fix until both bytes and inodes
look healthy and the failing command succeeds.

## Why du and df disagree in general

`df` asks the filesystem for its counters (statfs): allocated blocks, free blocks, free
inodes. `du` walks the directory tree and sums sizes of what it can reach. Anything the
walk cannot reach — unlinked but open files, files under a mount point, directories you
lack permission to read, other filesystems excluded by `-x` — creates a gap. Sparse files
and filesystem compression create a gap in the opposite direction: `du` reports actual
allocation while `ls -l` reports apparent size (`du --apparent-size` shows the latter).

## What we monitor now

Disk alerts on both bytes and inodes (`node_filesystem_files_free`), and a weekly job that
reports processes holding more than 1 GB of deleted files. The inode alert would have
caught the first incident a week earlier.
