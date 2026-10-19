# Container exited with code 137: memory limit or something else?

Exit code 137 = 128 + 9, i.e. the main process got SIGKILL. That is all the number
says. Two common sources:

1. **The container's own memory limit** (`--memory`, `deploy.resources.limits.memory` in
   compose, `resources.limits.memory` in Kubernetes). The kernel's cgroup OOM handling
   kills a process inside that cgroup even if the host has free RAM.
2. **Someone sent SIGKILL**: `docker kill`, or `docker stop` after the grace period
   (default 10 s) when the process ignored SIGTERM.

Distinguish them:

```sh
docker inspect --format '{{.State.OOMKilled}} {{.State.ExitCode}}' worker
docker stats --no-stream worker
```

`OOMKilled=true` means the memory limit. In Kubernetes the pod status shows
`OOMKilled` as the termination reason. On the host, `dmesg` shows
`oom-kill:constraint=CONSTRAINT_MEMCG` with the container's cgroup path — a cgroup-local
kill, unlike a machine-wide one.

Caveat: `OOMKilled` is reliable only when the killed process is PID 1 of the container.
If a child (say, a worker forked by gunicorn) is killed, the main process may survive or
exit with its own code, and you only notice from the kernel log.

If it is not OOM and the container keeps dying with 137 on `docker stop`: the app does
not handle SIGTERM. Often because it is started through `sh -c` in shell-form `CMD`, so
the shell is PID 1 and does not forward signals. Use exec form (`CMD ["node",
"server.js"]`) or `--init` (tini).

Related codes: 143 = SIGTERM handled by default action, 139 = SIGSEGV.

Fixes for real memory pressure: raise the limit with headroom, cap the runtime's heap
below it (JVM `-XX:MaxRAMPercentage=75`, Node `--max-old-space-size`), and look for
leaks with the memory graph over time rather than a single peak.
