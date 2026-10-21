# Host OOM killer: reading dmesg and deciding what to protect

Incident on db-replica-2: the Java indexer vanished overnight with no stack trace and no
application log entry. Nothing in our own logs because the process never got a chance to
write anything — the kernel killed it.

## Confirming it was the OOM killer

```sh
dmesg -T | grep -i -E "out of memory|oom-kill|killed process"
journalctl -k --since yesterday | grep -i oom
```

The relevant lines looked like:

```
Out of memory: Killed process 23817 (java) total-vm:9123456kB, anon-rss:7340032kB ...
oom-kill:constraint=CONSTRAINT_NONE,nodemask=(null),...,task=java,pid=23817,uid=1001
```

`constraint=CONSTRAINT_NONE` means the whole machine ran out of memory, not a cgroup.
If it says `CONSTRAINT_MEMCG`, the limit of a cgroup (a systemd slice or a container) was
hit instead, and the host itself may have plenty free — different problem, different fix.

Above the kill line the kernel dumps a table of tasks with their RSS and `oom_score_adj`.
That table shows who was actually eating memory; the victim is not always the culprit.

## How the victim is chosen

Each process gets a badness score, mostly proportional to its memory footprint, shifted
by `oom_score_adj` (range -1000..1000). Current values:

```sh
cat /proc/23817/oom_score
cat /proc/23817/oom_score_adj
```

`-1000` makes a process immune. For services, set it in the unit instead of echoing into
`/proc`: `OOMScoreAdjust=-500` for the database, `+300` for batch workers we would rather
lose first.

## What we changed

- Indexer heap was `-Xmx8g` on a 12 GB box with PostgreSQL next to it. Reduced to 5 GB.
- Put the indexer under `MemoryMax=6G` in its unit so a leak kills only it, inside its own
  cgroup, before the global killer starts choosing.
- Added swap (4 GB) with `vm.swappiness=10` — a little swap turns sudden kills into a slow
  degradation that alerts catch.
- Alert on `node_vmstat_oom_kill` increasing.

Overcommit (`vm.overcommit_memory=2`) was considered and rejected: the JVM reserves large
virtual ranges and starts failing allocations long before real memory is short.
